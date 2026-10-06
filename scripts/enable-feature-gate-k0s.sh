#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Set kube-apiserver extraArgs on an EXISTING k0s cluster -- feature gates
# and/or runtime-config -- one controller at a time, with a backup and
# automatic rollback if the apiserver does not come back healthy with every
# requested setting in effect.
#
# WHY THIS EXISTS
#
#   ADR-0089's banlieue-virtualmachine-created-by MutatingAdmissionPolicy
#   needs admissionregistration.k8s.io MutatingAdmissionPolicy, whose API
#   group VERSION tracks its stability stage like any other Kubernetes API:
#     v1.32 alpha  -> admissionregistration.k8s.io/v1alpha1
#     v1.34 beta   -> admissionregistration.k8s.io/v1beta1  (off by default)
#     v1.36 GA     -> admissionregistration.k8s.io/v1       (on by default)
#   Pre-GA, BOTH the feature gate AND the API group/version need to be
#   enabled explicitly -- the feature gate alone does not make the apiserver
#   serve the group. scripts/bootstrap-k0s-cluster.sh defaults K0S_VERSION to
#   1.35.5+k0s.0 (beta stage), so a cluster built with it needs:
#     FEATURE_GATES=MutatingAdmissionPolicy=true
#     RUNTIME_CONFIG=admissionregistration.k8s.io/v1beta1=true
#   and the matching v1beta1 policy manifest, not the v1 one
#   (deploy/admission/virtualmachine-created-by.yaml targets v1 / GA
#   deliberately -- ADR-0089 chose not to write code against a pre-GA API).
#   CONFIRM THE EXACT VERSION before setting RUNTIME_CONFIG: an apiserver
#   that does not recognise a --feature-gates key or a --runtime-config
#   group/version refuses to start at all, which is exactly the failure this
#   script's rollback exists to catch, but it still costs one restart cycle
#   to find out. `k0s version` / `kubectl version` tells you which stage.
#
# HOW IT DIFFERS FROM dev-oidc-k0s.sh
#
#   dev-oidc-k0s.sh's attach_controller refuses to touch a node that already
#   has `spec.api.extraArgs` and tells the operator to edit it by hand --
#   fine for a one-time setup script, wrong here, because a cluster that
#   already has OIDC flags under extraArgs (dev-oidc-k0s.sh's own
#   authentication-config, or plain --oidc-* flags set some other way) needs
#   this script to add SIBLING keys, not require a manual edit every time.
#   Each setting is merged independently: a key already present keeps every
#   other key/value pair it already carries (e.g. an existing feature-gates
#   list is extended, never replaced) -- a blind overwrite would silently
#   turn off whatever was already enabled there.
#
#   Multiple settings are applied in ONE backup + ONE edit pass + ONE
#   restart, not one per setting -- restarting kube-apiserver once per
#   requested key would both be slower and, worse, make every edit after the
#   first read from a backup that no longer reflects the prior edit if that
#   backup were retaken each time. Backup is taken exactly once per
#   controller, before any edit; every edit step reads and rewrites the
#   LIVE config file, so edits chain correctly within one run.
#
# Nothing here names a real host. Controllers are discovered from the
# cluster; the cluster comes from KUBECONFIG. awk, not `sed -i`, because
# Kairos ships a non-GNU sed -- same reasoning as dev-oidc-k0s.sh.
set -euo pipefail

# Comma-separated key=value pairs, same syntax --feature-gates itself takes.
# Empty: do not touch feature-gates at all.
FEATURE_GATES="${FEATURE_GATES:-MutatingAdmissionPolicy=true}"
# Comma-separated group/version=bool pairs, same syntax --runtime-config
# itself takes. Empty (the default): do not touch runtime-config -- only set
# this once you have confirmed the cluster's actual stability stage (see the
# header comment); the wrong value is a fatal apiserver startup error, not a
# warning.
RUNTIME_CONFIG="${RUNTIME_CONFIG:-}"
# root on bare metal; any other user must have passwordless sudo.
SSH_USER="${SSH_USER:-root}"
# Path to a private key for `ssh -i`. Empty: ssh's own default identity
# resolution (agent, ~/.ssh/id_*) is used, same as before this existed.
SSH_IDENTITY="${SSH_IDENTITY:-}"
# Space-separated controller addresses. Empty: every control-plane node's
# InternalIP, from the cluster.
CONTROLLERS="${CONTROLLERS:-}"
K0S_CONFIG="${K0S_CONFIG:-/etc/k0s/k0s.yaml}"
K0S_BACKUP="${K0S_CONFIG}.pre-feature-gates"
APISERVER_PORT="${APISERVER_PORT:-6443}"
RESTART_TIMEOUT_SECS="${RESTART_TIMEOUT_SECS:-300}"
LOG_TAIL_LINES="${LOG_TAIL_LINES:-80}"

log()  { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

check_deps() {
  local missing=()
  local tools=(kubectl ssh python3)
  for c in "${tools[@]}"; do
    command -v "$c" >/dev/null 2>&1 || missing+=("$c")
  done
  [[ ${#missing[@]} -eq 0 ]] || { warn "missing: ${missing[*]}"; exit 1; }
  kubectl get --raw /readyz >/dev/null \
    || { warn "kubectl cannot reach the cluster (KUBECONFIG=${KUBECONFIG:-~/.kube/config})"; exit 1; }
}

controllers() {
  if [[ -n "$CONTROLLERS" ]]; then echo "$CONTROLLERS"; return; fi
  kubectl get nodes -l node-role.kubernetes.io/control-plane \
    -o jsonpath='{range .items[*]}{.status.addresses[?(@.type=="InternalIP")].address}{" "}{end}'
}

# Run a script as root on a controller. The script travels base64-encoded so
# no remote shell's quoting rules apply, and stdin stays free for data.
on() {
  local host="$1"; shift
  local sudo="" script
  [[ "$SSH_USER" == "root" ]] || sudo="sudo -n"
  script="$(printf '%s' "$*" | base64 -w0)"
  local -a ssh_opts=(-o BatchMode=yes -o ConnectTimeout=10)
  [[ -z "$SSH_IDENTITY" ]] || ssh_opts+=(-i "$SSH_IDENTITY" -o IdentitiesOnly=yes)
  ssh "${ssh_opts[@]}" "${SSH_USER}@${host}" \
    "echo $script | base64 -d > /tmp/.banlieue-fg.\$\$ && $sudo sh /tmp/.banlieue-fg.\$\$; rc=\$?; rm -f /tmp/.banlieue-fg.\$\$; exit \$rc"
}

apiserver_ready() {
  kubectl --server "https://$1:${APISERVER_PORT}" get --raw /readyz >/dev/null 2>&1
}

# The RUNNING kube-apiserver's value for --$1, or empty if the flag is
# absent. Read from /proc by comm, so the probe never matches itself.
running_flag_value() {
  local host="$1" flag="$2"
  on "$host" "for p in /proc/[0-9]*; do
      [ \"\$(cat \$p/comm 2>/dev/null)\" = kube-apiserver ] || continue
      tr '\\0' '\\n' <\$p/cmdline; done | sed -n 's/^--${flag}=//p' | head -n1"
}

# True if every "key=value" in $2 (comma-separated) appears verbatim in $1
# (the running flag's value) -- order-independent, so a re-run that finds
# everything already satisfied is a no-op.
commalist_satisfied() {
  local running="$1" want_list="$2" want
  [[ -n "$want_list" ]] || return 0
  for want in ${want_list//,/ }; do
    case ",${running}," in
      *",${want},"*) ;;
      *) return 1 ;;
    esac
  done
  return 0
}

# Merge comma-separated "key=value" list $2 into $1, overriding any key $2
# names and preserving every other key untouched -- never a blind overwrite.
merge_commalist() {
  python3 - "$1" "$2" <<'PY'
import sys
def parse(s):
    out = {}
    for pair in s.split(","):
        pair = pair.strip()
        if not pair:
            continue
        k, _, v = pair.partition("=")
        out[k] = v
    return out
existing = parse(sys.argv[1])
existing.update(parse(sys.argv[2]))
print(",".join(f"{k}={v}" for k, v in sorted(existing.items())))
PY
}

# Edit $K0S_CONFIG on $host (already backed up by the caller) so that
# spec.api.extraArgs.$2 contains every "key=value" in $3, merged with
# whatever that yaml key already held. Reads and rewrites the LIVE config
# file (never the backup), so repeated calls in the same run chain
# correctly. No-op if $3 is empty.
ensure_extra_arg() {
  local host="$1" yaml_key="$2" desired="$3"
  [[ -n "$desired" ]] || return 0

  if on "$host" "grep -q '^      ${yaml_key}:' $K0S_CONFIG"; then
    local current merged
    current="$(on "$host" "sed -n 's/^      ${yaml_key}: *//p' $K0S_CONFIG | head -n1")"
    merged="$(merge_commalist "$current" "$desired")"
    log "  merging into existing ${yaml_key}: $current -> $merged"
    on "$host" "awk -v v='$merged' '{ if (\$0 ~ /^      ${yaml_key}:/) print \"      ${yaml_key}: \" v; else print }' \
        $K0S_CONFIG >$K0S_CONFIG.tmp && cat $K0S_CONFIG.tmp >$K0S_CONFIG && rm -f $K0S_CONFIG.tmp"
  elif on "$host" "grep -q '^    extraArgs:' $K0S_CONFIG"; then
    log "  extraArgs exists with no ${yaml_key} key yet — adding it"
    on "$host" "awk -v v='$desired' '{print} /^    extraArgs:\$/ {print \"      ${yaml_key}: \" v}' \
        $K0S_CONFIG >$K0S_CONFIG.tmp && cat $K0S_CONFIG.tmp >$K0S_CONFIG && rm -f $K0S_CONFIG.tmp"
  else
    log "  no extraArgs block yet — creating one"
    on "$host" "awk -v v='$desired' '{print} /^  api:\$/ {print \"    extraArgs:\"; print \"      ${yaml_key}: \" v}' \
        $K0S_CONFIG >$K0S_CONFIG.tmp && cat $K0S_CONFIG.tmp >$K0S_CONFIG && rm -f $K0S_CONFIG.tmp"
  fi
  on "$host" "grep -q '${yaml_key}:' $K0S_CONFIG"
}

all_satisfied() {
  local host="$1"
  commalist_satisfied "$(running_flag_value "$host" feature-gates)" "$FEATURE_GATES" \
    && commalist_satisfied "$(running_flag_value "$host" runtime-config)" "$RUNTIME_CONFIG"
}

attach_controller() {
  local host="$1"
  log "controller $host"

  if all_satisfied "$host"; then
    log "  already in effect — nothing to do"
    return 0
  fi

  on "$host" "cp -p $K0S_CONFIG $K0S_BACKUP"
  ensure_extra_arg "$host" feature-gates "$FEATURE_GATES"
  ensure_extra_arg "$host" runtime-config "$RUNTIME_CONFIG"

  log "  restarting k0scontroller"
  on "$host" "systemctl restart k0scontroller"

  local deadline=$((SECONDS + RESTART_TIMEOUT_SECS))
  # /readyz alone passes for an API server that came back WITHOUT the
  # settings, so the running process's actual flags are checked too.
  until apiserver_ready "$host" && all_satisfied "$host"; do
    if (( SECONDS > deadline )); then
      warn "  $host did not come back with every setting in ${RESTART_TIMEOUT_SECS}s; restoring"
      warn "  --- last ${LOG_TAIL_LINES} lines of 'journalctl -u k0scontroller' on $host ---"
      on "$host" "journalctl -u k0scontroller --no-pager -n $LOG_TAIL_LINES" >&2 || true
      warn "  --- end log ---"
      on "$host" "[ -f $K0S_BACKUP ] && cp -p $K0S_BACKUP $K0S_CONFIG && systemctl restart k0scontroller" || true
      exit 1
    fi
    sleep 5
  done
  log "  ✓ $host ready, feature-gates=$FEATURE_GATES runtime-config=$RUNTIME_CONFIG in effect"
}

attach_all() {
  local host
  for host in $(controllers); do attach_controller "$host"; done
}

status() {
  local host
  for host in $(controllers); do
    log "controller $host: --feature-gates=$(running_flag_value "$host" feature-gates) --runtime-config=$(running_flag_value "$host" runtime-config)"
  done
}

main() {
  case "${1:-}" in
    enable) check_deps; attach_all ;;
    status) check_deps; status ;;
    *)
      cat >&2 <<EOF
Usage: $0 [enable|status]

  enable  merge FEATURE_GATES and/or RUNTIME_CONFIG into spec.api.extraArgs
          on every controller (one backup + one edit pass + one restart per
          controller, rolled back on failure)
  status  print the running --feature-gates / --runtime-config value per
          controller

Env: KUBECONFIG, SSH_USER (default root; else passwordless sudo),
     SSH_IDENTITY (path to a private key for ssh -i; default: ssh's own
     identity resolution), CONTROLLERS (default: discovered),
     FEATURE_GATES (default: MutatingAdmissionPolicy=true),
     RUNTIME_CONFIG (default: empty — set only after confirming the
     cluster's exact pre-GA stage; see this script's header comment)
EOF
      exit 1
      ;;
  esac
}

main "$@"
