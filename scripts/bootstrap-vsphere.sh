#!/usr/bin/env bash
# Prepares a workstation and a vSphere estate for
# `BACKEND=vsphere ./scripts/bootstrap-k0s-cluster.sh`.
#
# vCenter and ESXi are assumed to exist already -- unlike the libvirt backend
# there is no hypervisor to install. What this does instead is:
#
#   * install the client tooling the vsphere backend needs (govc, jq, kubectl),
#   * verify the GOVC_* environment actually reaches vCenter,
#   * DISCOVER the estate and emit a populated env file, so the per-cluster
#     placement maps bootstrap-k0s-cluster.sh expects do not have to be written
#     out by hand from the vSphere UI,
#   * create the objects it safely can (the VM folder, optionally a resource
#     pool).
#
#   ./scripts/bootstrap-vsphere.sh all           # tools + check + discover
#   ./scripts/bootstrap-vsphere.sh discover > ~/.config/banlieue/hosts/prod.env
#   BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/prod.env \
#     ./scripts/bootstrap-vsphere.sh verify
#
# Credentials come from the ambient GOVC_* environment and are never printed,
# written to a file, or passed on a command line by this script.
#
# Templates are NOT built here -- see docs/src/guides/building-kairos-hadron-template.md
# and docs/src/guides/alpine-vsphere-template.md.
set -euo pipefail

BANLIEUE_ENV_FILE="${BANLIEUE_ENV_FILE:-}"
if [[ "${1:-}" != "--print-env-template" && -n "$BANLIEUE_ENV_FILE" ]]; then
  [[ -f "$BANLIEUE_ENV_FILE" ]] || { echo "BANLIEUE_ENV_FILE=$BANLIEUE_ENV_FILE not found" >&2; exit 1; }
  # shellcheck disable=SC1090  # path is operator-supplied by design
  source "$BANLIEUE_ENV_FILE"
fi

VSPHERE_FOLDER="${VSPHERE_FOLDER:-banlieue}"
# Optional: a resource pool to create under each discovered cluster. Empty
# means "use the cluster's root pool", which is what most estates want.
VSPHERE_CREATE_POOL="${VSPHERE_CREATE_POOL:-}"

K0S_VERSION="${K0S_VERSION:-1.35.5+k0s.0}"
KUBECTL_VERSION="${KUBECTL_VERSION:-}"
GOVC_VERSION="${GOVC_VERSION:-v0.52.0}"

log()  { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

# ------------------------------------------------------------------ tools ---
os_arch() {
  local os arch
  case "$(uname -s)" in Darwin) os=Darwin ;; Linux) os=Linux ;; *) warn "unsupported OS"; exit 1 ;; esac
  case "$(uname -m)" in x86_64|amd64) arch=x86_64 ;; arm64|aarch64) arch=arm64 ;; *) warn "unsupported arch"; exit 1 ;; esac
  echo "${os}_${arch}"
}

install_tools() {
  log "Installing client tooling"
  if command -v govc >/dev/null 2>&1; then
    log "  govc present: $(govc version 2>/dev/null | head -1)"
  else
    local oa tmp
    oa="$(os_arch)"; tmp="$(mktemp -d)"
    log "  fetching govc $GOVC_VERSION ($oa)"
    curl -fsSL -o "$tmp/govc.tar.gz" \
      "https://github.com/vmware/govmomi/releases/download/${GOVC_VERSION}/govc_${oa}.tar.gz"
    tar -C "$tmp" -xzf "$tmp/govc.tar.gz" govc
    install -m 0755 "$tmp/govc" /usr/local/bin/govc 2>/dev/null \
      || sudo install -m 0755 "$tmp/govc" /usr/local/bin/govc
    rm -rf "$tmp"
  fi

  if command -v jq >/dev/null 2>&1; then
    log "  jq present"
  elif command -v apt-get >/dev/null 2>&1; then
    sudo apt-get install -y -qq jq
  elif command -v brew >/dev/null 2>&1; then
    brew install jq
  else
    warn "  install jq manually (no apt-get or brew found)"
  fi

  if command -v kubectl >/dev/null 2>&1; then
    log "  kubectl present: $(kubectl version --client 2>/dev/null | head -1)"
  else
    # Matched to the k0s minor, not "stable": kubectl supports only +/-1 minor
    # against the API server and "stable" runs ahead of the k0s release train.
    local minor kver os arch tmp
    minor="$(echo "$K0S_VERSION" | sed -n 's/^\([0-9]*\.[0-9]*\).*/\1/p')"
    # `|| true` so a failed fetch falls through to the explicit emptiness check
    # below rather than aborting under set -e with no explanation.
    kver="${KUBECTL_VERSION:-$(curl -fsSL "https://dl.k8s.io/release/stable-${minor}.txt" 2>/dev/null || true)}"
    [[ -n "$kver" ]] || { warn "  cannot resolve kubectl for k0s minor $minor; set KUBECTL_VERSION"; return 1; }
    case "$(uname -s)" in Darwin) os=darwin ;; *) os=linux ;; esac
    case "$(uname -m)" in arm64|aarch64) arch=arm64 ;; *) arch=amd64 ;; esac
    tmp="$(mktemp -d)"
    log "  fetching kubectl $kver ($os/$arch)"
    curl -fsSL -o "$tmp/kubectl" "https://dl.k8s.io/release/${kver}/bin/${os}/${arch}/kubectl"
    install -m 0755 "$tmp/kubectl" /usr/local/bin/kubectl 2>/dev/null \
      || sudo install -m 0755 "$tmp/kubectl" /usr/local/bin/kubectl
    rm -rf "$tmp"
  fi

  local c missing=()
  for c in govc jq kubectl ssh base64; do
    command -v "$c" >/dev/null 2>&1 || missing+=("$c")
  done
  ((${#missing[@]})) && { warn "still missing: ${missing[*]}"; return 1; }
  log "  all of govc jq kubectl ssh base64 present"
}

# ------------------------------------------------------------------ check ---
check_env() {
  command -v govc >/dev/null 2>&1 || { warn "govc not installed (run: $0 tools)"; exit 1; }
  [[ -n "${GOVC_URL:-}" ]] || { warn "GOVC_URL is not set"; exit 1; }
  # GOVC_PASSWORD may legitimately be embedded in GOVC_URL, so only warn.
  [[ -n "${GOVC_USERNAME:-}" ]] || warn "GOVC_USERNAME is not set (may be embedded in GOVC_URL)"

  log "Contacting vCenter"
  if ! govc about >/dev/null 2>&1; then
    warn "cannot reach vCenter with the current GOVC_* environment"
    warn "  GOVC_URL=${GOVC_URL}"
    warn "  set GOVC_INSECURE=true for a self-signed certificate"
    exit 1
  fi
  govc about 2>/dev/null | sed 's/^/  /' >&2
  log "  connected"
}

# --------------------------------------------------------------- discover ---
# vSphere object names routinely contain spaces, dots and hyphens. The k0s
# script reads placement as flat shell variables (VSPHERE_RP_<id>) because
# macOS ships bash 3.2 with no associative arrays -- so <id> has to be a legal
# shell identifier fragment.
sanitize_id() { echo "$1" | sed 's#.*/##; s/[^A-Za-z0-9]/_/g'; }

discover() {
  check_env

  local dcs clusters cl id rp dsc net tpls
  dcs="$(govc find / -type d 2>/dev/null || true)"
  [[ -n "$dcs" ]] || { warn "no datacenters visible to this account"; exit 1; }

  echo "# banlieue vSphere settings -- generated by bootstrap-vsphere.sh discover"
  echo "# $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "#"
  echo "# Keep this file OUTSIDE the repository: it names real infrastructure."
  echo "# Convention: \$HOME/.config/banlieue/hosts/<name>.env"
  echo "#"
  echo "# Review every value below. Discovery lists what EXISTS; it cannot know"
  echo "# which cluster, datastore or network you intend to use. Delete the"
  echo "# alternatives, then fill in the NODES table at the bottom."
  echo
  echo "BACKEND=vsphere"
  echo "VSPHERE_FOLDER=${VSPHERE_FOLDER}"
  echo

  for dc in $dcs; do
    echo "# ===== datacenter: $dc ====="
    clusters="$(govc find "$dc" -type c 2>/dev/null || true)"
    [[ -n "$clusters" ]] || { echo "#   (no compute clusters)"; continue; }

    for cl in $clusters; do
      id="$(sanitize_id "$cl")"
      echo "# --- cluster: $cl  ->  id '$id' ---"

      # Resource pools under this cluster. The root pool is called Resources
      # and is the right default; named children are listed as alternatives.
      rp="$(govc find "$cl" -type p 2>/dev/null | head -5 || true)"
      if [[ -n "$rp" ]]; then
        echo "$rp" | head -1 | sed "s#^#VSPHERE_RP_${id}=#"
        echo "$rp" | tail -n +2 | sed "s#^#  # ; s#^#\# alt: VSPHERE_RP_${id}=#"
      else
        echo "# VSPHERE_RP_${id}=   # none found"
      fi

      # Datastore clusters (StoragePod). Note: StoragePod has NO single-letter
      # alias in `govc find -type`, unlike Datastore (s) -- the full managed
      # object type name is required. Fall back to plain datastores.
      dsc="$(govc find "$dc" -type StoragePod 2>/dev/null | head -3 || true)"
      if [[ -n "$dsc" ]]; then
        echo "$dsc" | head -1 | sed "s#^#VSPHERE_DSC_${id}=#"
        echo "$dsc" | tail -n +2 | sed "s#^#\# alt: VSPHERE_DSC_${id}=#"
      else
        echo "# no datastore cluster found; these are plain datastores:"
        govc find "$dc" -type s 2>/dev/null | head -3 | sed "s#^#\# VSPHERE_DSC_${id}=#"
      fi

      # Distributed port groups first: the k0s script passes this to `govc -net`.
      net="$(govc find "$dc" -type g 2>/dev/null | head -3 || true)"
      [[ -n "$net" ]] || net="$(govc find "$dc" -type n 2>/dev/null | head -3 || true)"
      if [[ -n "$net" ]]; then
        echo "$net" | head -1 | sed "s#^#VSPHERE_NET_${id}=#"
        echo "$net" | tail -n +2 | sed "s#^#\# alt: VSPHERE_NET_${id}=#"
      else
        echo "# VSPHERE_NET_${id}=   # none found"
      fi

      # Templates the k0s script can clone.
      tpls="$(govc find "$dc" -type m -config.template true 2>/dev/null | head -5 || true)"
      if [[ -n "$tpls" ]]; then
        echo "$tpls" | head -1 | sed "s#^#VSPHERE_TPL_${id}=#"
        echo "$tpls" | tail -n +2 | sed "s#^#\# alt: VSPHERE_TPL_${id}=#"
      else
        echo "# VSPHERE_TPL_${id}=   # no templates found -- see"
        echo "#   docs/src/guides/building-kairos-hadron-template.md"
      fi

      echo "# VSPHERE_GW_${id}=      # optional; default is <ip first three octets>.1"
      echo
    done
  done

  cat <<'TAIL'
# ===== static networking (shared by all nodes) =====
NET_PREFIX=24
#DNS_SERVERS=192.0.2.53,198.51.100.53
#DNS_DOMAIN=foo.io
# Stable API server name. Must resolve to a controller, or be in /etc/hosts;
# it is baked into the serving certificate SANs.
#API_SAN=k0s-api.foo.io

# ===== node table =====
# One entry per node: "<name> <cluster_id> <ip> <role>"
#   name       vSphere VM name AND k0s node hostname
#   cluster_id one of the ids above -- spread nodes across clusters so each is
#              an etcd failure domain (losing a cluster costs one control-plane
#              node, not the cluster)
#   ip         STATIC IPv4
#   role       controller+worker | worker
#NODES=(
#  "k0s-01 CLUSTER_A 192.0.2.11 controller+worker"
#  "k0s-02 CLUSTER_B 192.0.2.12 controller+worker"
#  "k0s-03 CLUSTER_C 192.0.2.13 controller+worker"
#  "k0s-04 CLUSTER_A 192.0.2.14 worker"
#)
TAIL
}

# ----------------------------------------------------------------- create ---
create_objects() {
  check_env
  log "Ensuring folder '$VSPHERE_FOLDER'"
  if govc folder.info "$VSPHERE_FOLDER" >/dev/null 2>&1; then
    log "  exists"
  else
    govc folder.create "$VSPHERE_FOLDER" >/dev/null 2>&1 \
      && log "  created" || warn "  could not create (check permissions / path)"
  fi

  if [[ -n "$VSPHERE_CREATE_POOL" ]]; then
    local cl
    for cl in $(govc find / -type c 2>/dev/null); do
      if govc pool.info "$cl/Resources/$VSPHERE_CREATE_POOL" >/dev/null 2>&1; then
        log "  pool '$VSPHERE_CREATE_POOL' exists under $cl"
      else
        govc pool.create "$cl/Resources/$VSPHERE_CREATE_POOL" >/dev/null 2>&1 \
          && log "  created pool under $cl" || warn "  could not create pool under $cl"
      fi
    done
  fi
}

# ----------------------------------------------------------------- verify ---
# Validate that everything the env file names actually exists. Every one of
# these failures otherwise surfaces mid-run, after VMs have been cloned.
verify() {
  check_env
  local rc=0 v id path
  [[ -n "${NODES:-}" ]] || warn "NODES is not set (is BANLIEUE_ENV_FILE pointing at your env file?)"

  govc folder.info "$VSPHERE_FOLDER" >/dev/null 2>&1 \
    && log "  folder $VSPHERE_FOLDER: ok" || { warn "  folder $VSPHERE_FOLDER: MISSING"; rc=1; }

  for v in $(set | sed -n 's/^\(VSPHERE_\(RP\|DSC\|NET\|TPL\)_[A-Za-z0-9_]*\)=.*/\1/p'); do
    id="${v}"; eval "path=\${$v}"
    [[ -n "$path" ]] || continue
    if govc ls -- "$path" >/dev/null 2>&1 || govc find -- "$path" >/dev/null 2>&1; then
      log "  $id: ok"
    else
      warn "  $id -> $path: NOT FOUND"; rc=1
    fi
  done
  [[ $rc -eq 0 ]] && log "all referenced objects exist" || warn "some objects are missing"
  return $rc
}

print_env_template() {
  cat <<'TEMPLATE'
# banlieue vSphere settings.
#
# Prefer generating this with real values discovered from your estate:
#   ./scripts/bootstrap-vsphere.sh discover > ~/.config/banlieue/hosts/prod.env
#
# Credentials come from the ambient GOVC_* environment, NOT from this file:
#   export GOVC_URL=https://vcenter.example.com/sdk
#   export GOVC_USERNAME=admin
#   export GOVC_PASSWORD=...
#   export GOVC_INSECURE=true      # self-signed vCenter certificate

BACKEND=vsphere
VSPHERE_FOLDER=banlieue
#VSPHERE_CREATE_POOL=banlieue

# Per-cluster placement. <id> is any shell-safe token; it links a NODES entry
# to the four maps below. `discover` fills these in from the live estate.
#VSPHERE_RP_CLUSTER_A=/dc1/host/cluster-a/Resources
#VSPHERE_DSC_CLUSTER_A=/dc1/datastore/sdrs-a
#VSPHERE_NET_CLUSTER_A=/dc1/network/pg-servers
#VSPHERE_TPL_CLUSTER_A=/dc1/vm/templates/kairos-hadron
#VSPHERE_GW_CLUSTER_A=192.0.2.1

NET_PREFIX=24
#DNS_SERVERS=192.0.2.53,198.51.100.53
#DNS_DOMAIN=foo.io
#API_SAN=k0s-api.foo.io

#NODES=(
#  "k0s-01 CLUSTER_A 192.0.2.11 controller+worker"
#  "k0s-02 CLUSTER_B 192.0.2.12 controller+worker"
#  "k0s-03 CLUSTER_C 192.0.2.13 controller+worker"
#)
TEMPLATE
}

usage() {
  cat >&2 <<'USAGE'
Usage: bootstrap-vsphere.sh [all|tools|check|discover|create|verify]
       bootstrap-vsphere.sh --print-env-template

  tools     install govc, jq and kubectl locally
  check     verify the GOVC_* environment reaches vCenter
  discover  enumerate the estate and print a populated env file to stdout
  create    create the VM folder (and VSPHERE_CREATE_POOL, if set)
  verify    check every object named in BANLIEUE_ENV_FILE exists
  all       tools + check + discover

Credentials come from the ambient GOVC_* environment and are never printed.
Next step: BACKEND=vsphere ./scripts/bootstrap-k0s-cluster.sh all
USAGE
  exit 1
}

main() {
  case "${1:-all}" in
    tools)    install_tools ;;
    check)    check_env ;;
    discover) discover ;;
    create)   create_objects ;;
    verify)   verify ;;
    --print-env-template) print_env_template ;;
    all)      install_tools; check_env; discover ;;
    *)        usage ;;
  esac
}

main "$@"
