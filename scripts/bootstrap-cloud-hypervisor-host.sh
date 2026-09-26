#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Turns a bare-metal Debian/Ubuntu machine into a Cloud Hypervisor host for
# banlieue's host-resident provider (roadmap 09, ADR-0060 to ADR-0065).
# The Cloud Hypervisor counterpart of bootstrap-libvirt-host.sh.
#
# Run it ON the host, as root:
#
#   sudo ./scripts/bootstrap-cloud-hypervisor-host.sh all
#
# or from a workstation against a REMOTE host over SSH. The script copies
# itself (and BANLIEUE_ENV_FILE, if set) to the host, runs there under sudo,
# and removes the copies:
#
#   BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
#     ./scripts/bootstrap-cloud-hypervisor-host.sh --remote admin@bar.foo.io all
#
# What it sets up, and which decision each piece comes from:
#
#   vmm       cloud-hypervisor + ch-remote (static) and CLOUDHV.fd, pinned
#             and sha256-verified; a mismatch fails closed     (ADR-0061)
#   host      the `banlieue` system user, storage directories, the run root,
#             the host-local config file that maps storage and network
#             classes to paths and bridges                     (ADR-0062 D4)
#   tpm       swtpm, and a per-host swtpm_localca that signs EK
#             certificates, its key readable by `banlieue` only (ADR-0065)
#   polkit    a rule letting `banlieue` manage only banlieue-ch-* and
#             banlieue-swtpm-* units                            (ADR-0063 D6)
#   provider  the provider's own systemd unit. Installed, NOT enabled until
#             the binary and its kubeconfig exist           (ADR-0060 D5/D7)
#   selftest  proves the pieces work together without booting a guest
#
# It never creates or modifies a network bridge. On a remote host, bridging
# the uplink over the SSH session you are using is how you lose the host. Make
# the bridge first (or reuse libvirt's virbr0), then name it in
# NETWORK_CLASSES; `preflight` refuses a bridge that does not exist.
#
# Every step is idempotent. All configuration is environment variables; keep
# per-host values OUTSIDE this repository (they name real hosts), e.g.
# $HOME/.config/banlieue/hosts/<name>.env via BANLIEUE_ENV_FILE. See
# --print-env-template.
set -euo pipefail

# ------------------------------------------------------------ remote mode ---
# Handled before anything else: on the workstation nothing below runs, and the
# workstation need not be Linux.
if [[ "${1:-}" == "--remote" ]]; then
  target="${2:-}"
  [[ -n "$target" ]] || { echo "usage: $0 --remote <user@host> [step]" >&2; exit 1; }
  shift 2
  step="${1:-all}"
  remote_dir="/tmp/banlieue-ch-bootstrap.$$"
  env_arg=""
  echo "==> copying bootstrap to $target:$remote_dir" >&2
  # shellcheck disable=SC2029  # the path is chosen here, on purpose
  ssh "$target" "mkdir -m 0700 -p $remote_dir"
  scp -q "$0" "$target:$remote_dir/bootstrap.sh"
  if [[ -n "${BANLIEUE_ENV_FILE:-}" ]]; then
    [[ -f "$BANLIEUE_ENV_FILE" ]] || { echo "BANLIEUE_ENV_FILE=$BANLIEUE_ENV_FILE not found" >&2; exit 1; }
    scp -q "$BANLIEUE_ENV_FILE" "$target:$remote_dir/host.env"
    env_arg="BANLIEUE_ENV_FILE=$remote_dir/host.env"
  fi
  echo "==> running '$step' on $target (sudo may prompt)" >&2
  rc=0
  # -t: sudo needs a terminal to ask for a password.
  ssh -t "$target" "sudo $env_arg bash $remote_dir/bootstrap.sh $step" || rc=$?
  # shellcheck disable=SC2029  # the path is chosen here, on purpose
  ssh "$target" "rm -rf $remote_dir" || true
  exit "$rc"
fi

BANLIEUE_ENV_FILE="${BANLIEUE_ENV_FILE:-}"
if [[ "${1:-}" != "--print-env-template" && -n "$BANLIEUE_ENV_FILE" ]]; then
  [[ -f "$BANLIEUE_ENV_FILE" ]] || { echo "BANLIEUE_ENV_FILE=$BANLIEUE_ENV_FILE not found" >&2; exit 1; }
  # shellcheck disable=SC1090  # path is operator-supplied by design
  source "$BANLIEUE_ENV_FILE"
fi

# ------------------------------------------------------------ pinned VMM ---
# The versions banlieue's client is written against (ADR-0061 Decision 2).
# Changing a version without its checksums fails closed: an unverified
# hypervisor binary is not something to install on the way to running guests.
CH_VERSION="${CH_VERSION:-v53.0}"
CH_SHA256="${CH_SHA256:-448af3d4e59b22c2987f7df94c213ad40fb53a10d437e42b5ee6c4fce7c29ecc}"
CH_REMOTE_SHA256="${CH_REMOTE_SHA256:-13f32ba952e6791fd901f2279be2055fbacc64005f96c42a8e90d58860df84a7}"
FIRMWARE_TAG="${FIRMWARE_TAG:-ch-97eeb7b09}"
FIRMWARE_SHA256="${FIRMWARE_SHA256:-dc2fc8f0e43b96712d9fccc52a3a590769606412b3e1bc911d217addd3bef624}"
CH_RELEASES="${CH_RELEASES:-https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download}"
FIRMWARE_RELEASES="${FIRMWARE_RELEASES:-https://github.com/cloud-hypervisor/edk2/releases/download}"

# ------------------------------------------------------------------ paths ---
OPT_ROOT="${OPT_ROOT:-/opt/banlieue}"
BIN_DIR="${BIN_DIR:-/usr/local/bin}"
CONF_DIR="${CONF_DIR:-/etc/banlieue}"
STATE_ROOT="${STATE_ROOT:-/var/lib/banlieue}"
RUN_ROOT="${RUN_ROOT:-/run/banlieue/ch}"
HOST_CONFIG="${HOST_CONFIG:-$CONF_DIR/cloud-hypervisor.toml}"
KUBECONFIG_PATH="${KUBECONFIG_PATH:-$CONF_DIR/kubeconfig}"
PROVIDER_BINARY="${PROVIDER_BINARY:-$BIN_DIR/banlieue}"
# A banlieue binary to install as PROVIDER_BINARY. Empty = leave it alone.
BANLIEUE_BINARY_SRC="${BANLIEUE_BINARY_SRC:-}"

# The unprivileged account the provider runs as (ADR-0063 Decision 6).
BANLIEUE_USER="${BANLIEUE_USER:-banlieue}"

# One uid per guest, from this range (ADR-0063 Decision 3). Must not overlap
# real accounts or container subuid ranges; `preflight` checks both.
GUEST_UID_BASE="${GUEST_UID_BASE:-2000000}"
GUEST_UID_COUNT="${GUEST_UID_COUNT:-10000}"

# Storage and network classes: name=path and name=bridge, space-separated.
# Machines name a class; only this host knows what it means (ADR-0062 D4).
# Empty STORAGE_CLASSES = one `default` class on the mount point with the most
# free space. Empty NETWORK_CLASSES = `default` on virbr0 if it exists.
STORAGE_CLASSES="${STORAGE_CLASSES:-}"
STORAGE_CANDIDATES="${STORAGE_CANDIDATES:-/srv /data /home /opt /var/lib}"
NETWORK_CLASSES="${NETWORK_CLASSES:-}"

# The Kubernetes Provider this host is (ADR-0060: one Provider, one host).
PROVIDER_NAME="${PROVIDER_NAME:-$(hostname -s 2>/dev/null || hostname)}"
PROVIDER_NAMESPACE="${PROVIDER_NAMESPACE:-banlieue-system}"

# The environment constraint is "bare-metal KVM only". Set true for a lab VM
# with nested virtualization, knowing it is unsupported.
ALLOW_VIRTUALIZED_HOST="${ALLOW_VIRTUALIZED_HOST:-false}"

# Regenerate the host config and the swtpm CA. The CA is the per-host EK trust
# anchor (ADR-0065 Decision 6): rotating it invalidates every EK certificate
# already issued on this host.
FORCE="${FORCE:-false}"

UNIT_PREFIX_VMM="banlieue-ch-"
UNIT_PREFIX_TPM="banlieue-swtpm-"
PROVIDER_UNIT="banlieue-provider-cloud-hypervisor.service"

log()  { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

require_root() {
  [[ $EUID -eq 0 ]] || { warn "must run as root (installs packages, writes /etc, $OPT_ROOT)"; exit 1; }
}

require_apt() {
  command -v apt-get >/dev/null 2>&1 \
    || { warn "this script supports Debian/Ubuntu (apt-get) only"; exit 1; }
}

keep_existing() {
  [[ -e "$1" && "$FORCE" != "true" ]] && { log "$1 exists, keeping (FORCE=true to regenerate)"; return 0; }
  return 1
}

# Largest-free-space candidate, as bootstrap-libvirt-host.sh does: disk images
# are the biggest thing this host stores, and the stock /var is often small.
pick_storage_root() {
  local best="" best_avail=0 mp avail
  for mp in $STORAGE_CANDIDATES; do
    [[ -d "$mp" ]] || continue
    avail="$(df -P --output=avail "$mp" 2>/dev/null | tail -1 | tr -d ' ')" || continue
    [[ -n "$avail" ]] || continue
    if (( avail > best_avail )); then best_avail="$avail"; best="$mp"; fi
  done
  echo "${best:-/var/lib}/banlieue/ch"
}

resolve_classes() {
  [[ -n "$STORAGE_CLASSES" ]] || STORAGE_CLASSES="default=$(pick_storage_root)"
  if [[ -z "$NETWORK_CLASSES" ]] && ip link show virbr0 >/dev/null 2>&1; then
    NETWORK_CLASSES="default=virbr0"
  fi
}

# -------------------------------------------------------------- preflight ---
preflight() {
  local ok=true
  log "Preflight"

  local virt; virt="$(systemd-detect-virt --vm 2>/dev/null || true)"
  if [[ -n "$virt" && "$virt" != "none" ]]; then
    if [[ "$ALLOW_VIRTUALIZED_HOST" == "true" ]]; then
      warn "  running inside a VM ($virt): nested virtualization is unsupported (ALLOW_VIRTUALIZED_HOST=true)"
    else
      warn "  running inside a VM ($virt). This provider targets bare-metal KVM only."
      warn "  Set ALLOW_VIRTUALIZED_HOST=true for a lab host, knowing it is unsupported."
      ok=false
    fi
  fi

  if [[ -c /dev/kvm ]]; then log "  /dev/kvm present"; else warn "  /dev/kvm missing (BIOS VT-x/AMD-V, kvm module?)"; ok=false; fi
  getent group kvm >/dev/null || { warn "  no kvm group"; ok=false; }
  [[ "$(uname -m)" == "x86_64" ]] || { warn "  $(uname -m): only x86_64 is pinned here"; ok=false; }
  command -v systemctl >/dev/null || { warn "  systemd is required (ADR-0063)"; ok=false; }

  resolve_classes
  if [[ -z "$NETWORK_CLASSES" ]]; then
    warn "  NETWORK_CLASSES is empty and there is no virbr0. Create a bridge, then set"
    warn "  e.g. NETWORK_CLASSES=\"default=br0\". This script never creates one."
    ok=false
  fi
  local pair name br
  for pair in $NETWORK_CLASSES; do
    name="${pair%%=*}"; br="${pair#*=}"
    if [[ -d "/sys/class/net/$br/bridge" ]]; then
      log "  network class $name -> bridge $br"
    else
      warn "  network class $name -> $br: not a bridge on this host"; ok=false
    fi
  done

  # The guest uid range must be free of real accounts and container ranges.
  local end=$(( GUEST_UID_BASE + GUEST_UID_COUNT - 1 ))
  if getent passwd | awk -F: -v a="$GUEST_UID_BASE" -v b="$end" '$3>=a && $3<=b {f=1} END {exit !f}'; then
    warn "  an account already uses a uid in $GUEST_UID_BASE-$end"; ok=false
  fi
  local f start count
  for f in /etc/subuid /etc/subgid; do
    [[ -f "$f" ]] || continue
    while IFS=: read -r _ start count; do
      [[ -n "$start" && -n "$count" ]] || continue
      if (( start <= end && start + count - 1 >= GUEST_UID_BASE )); then
        warn "  $f range $start+$count overlaps guest uids $GUEST_UID_BASE-$end"; ok=false
      fi
    done < "$f"
  done

  [[ "$ok" == "true" ]] || { warn "preflight failed"; exit 1; }
  log "  preflight ok"
}

# --------------------------------------------------------------- packages ---
install_packages() {
  require_root; require_apt
  log "Installing host packages"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  # swtpm-tools brings swtpm_setup and swtpm_localca. iproute2 for taps and
  # neighbour lookups; policykit for the unit rule. No qemu, no libvirt.
  apt-get install -y -qq --no-install-recommends \
    ca-certificates curl openssl iproute2 swtpm swtpm-tools polkitd dbus
}

# -------------------------------------------------------------------- vmm ---
fetch_verified() {
  local url="$1" want="$2" out="$3" got
  curl -fsSL --proto '=https' --tlsv1.2 -o "$out.part" "$url"
  got="$(sha256sum "$out.part" | awk '{print $1}')"
  if [[ "$got" != "$want" ]]; then
    rm -f "$out.part"
    warn "checksum mismatch for $url"
    warn "  want $want"
    warn "  got  $got"
    exit 1
  fi
  mv -f "$out.part" "$out"
}

install_vmm() {
  require_root
  local vdir="$OPT_ROOT/cloud-hypervisor/$CH_VERSION" fdir="$OPT_ROOT/firmware/$FIRMWARE_TAG"
  install -d -m 0755 "$vdir" "$fdir"

  if [[ -x "$vdir/cloud-hypervisor" && "$FORCE" != "true" ]]; then
    log "cloud-hypervisor $CH_VERSION already installed"
  else
    log "Downloading cloud-hypervisor $CH_VERSION (sha256-pinned)"
    fetch_verified "$CH_RELEASES/$CH_VERSION/cloud-hypervisor-static" "$CH_SHA256" "$vdir/cloud-hypervisor"
    fetch_verified "$CH_RELEASES/$CH_VERSION/ch-remote-static" "$CH_REMOTE_SHA256" "$vdir/ch-remote"
    chmod 0755 "$vdir/cloud-hypervisor" "$vdir/ch-remote"
  fi
  if [[ -f "$fdir/CLOUDHV.fd" && "$FORCE" != "true" ]]; then
    log "CLOUDHV.fd $FIRMWARE_TAG already installed"
  else
    log "Downloading CLOUDHV.fd $FIRMWARE_TAG (sha256-pinned)"
    fetch_verified "$FIRMWARE_RELEASES/$FIRMWARE_TAG/CLOUDHV.fd" "$FIRMWARE_SHA256" "$fdir/CLOUDHV.fd"
    chmod 0644 "$fdir/CLOUDHV.fd"
  fi

  ln -sfn "$vdir/cloud-hypervisor" "$BIN_DIR/cloud-hypervisor"
  ln -sfn "$vdir/ch-remote" "$BIN_DIR/ch-remote"
  log "  $("$BIN_DIR/cloud-hypervisor" --version | head -1)"
}

# ------------------------------------------------------------------- host ---
setup_host() {
  require_root
  resolve_classes

  if ! id "$BANLIEUE_USER" >/dev/null 2>&1; then
    log "Creating system user $BANLIEUE_USER"
    useradd --system --home-dir "$STATE_ROOT" --no-create-home \
      --shell /usr/sbin/nologin --user-group "$BANLIEUE_USER"
  fi
  # The provider hands taps and files to guest uids, and joins kvm only to
  # check /dev/kvm; guests get kvm themselves as a supplementary group.
  if getent group kvm >/dev/null; then
    usermod -aG kvm "$BANLIEUE_USER"
  else
    warn "no kvm group on this host; $BANLIEUE_USER not added (preflight would have failed)"
  fi

  install -d -m 0755 -o root -g root "$CONF_DIR"
  install -d -m 0750 -o "$BANLIEUE_USER" -g "$BANLIEUE_USER" "$STATE_ROOT"

  # /run is tmpfs; tmpfiles.d recreates the run root at every boot.
  cat >/etc/tmpfiles.d/banlieue-cloud-hypervisor.conf <<EOF
# Written by bootstrap-cloud-hypervisor-host.sh (ADR-0063 Decision 5).
d $(dirname "$RUN_ROOT") 0755 root root -
d $RUN_ROOT 0750 $BANLIEUE_USER $BANLIEUE_USER -
EOF
  systemd-tmpfiles --create /etc/tmpfiles.d/banlieue-cloud-hypervisor.conf

  local pair name path
  for pair in $STORAGE_CLASSES; do
    name="${pair%%=*}"; path="${pair#*=}"
    # 0750: guests' per-machine directories live under here, each 0700 and
    # owned by its own uid; nobody else lists them.
    install -d -m 0750 -o "$BANLIEUE_USER" -g "$BANLIEUE_USER" "$path" "$path/images"
    log "  storage class $name -> $path ($(df -h --output=avail "$path" | tail -1 | tr -d ' ') free)"
  done

  write_host_config
}

write_host_config() {
  keep_existing "$HOST_CONFIG" && return 0
  log "Writing $HOST_CONFIG"
  local tmp; tmp="$(mktemp)"
  {
    cat <<EOF
# banlieue Cloud Hypervisor host configuration.
# Written by scripts/bootstrap-cloud-hypervisor-host.sh.
#
# Host-local BY DESIGN (ADR-0062 Decision 4): paths and bridges are resolved
# here and never taken from the cluster, so a stolen cluster credential can
# choose among what this file declares and nothing else. Machines name a
# class; the provider publishes the class names on its failure domain.
#
# Proposed schema; the provider crate owns it once it exists.

[provider]
name = "$PROVIDER_NAME"
namespace = "$PROVIDER_NAMESPACE"
kubeconfig = "$KUBECONFIG_PATH"

[vmm]
binary = "$BIN_DIR/cloud-hypervisor"
version = "$CH_VERSION"
firmware = "$OPT_ROOT/firmware/$FIRMWARE_TAG/CLOUDHV.fd"

[paths]
run_root = "$RUN_ROOT"
state_root = "$STATE_ROOT"

[guests]
uid_base = $GUEST_UID_BASE
uid_count = $GUEST_UID_COUNT

[tpm]
swtpm = "$(command -v swtpm || echo /usr/bin/swtpm)"
swtpm_setup = "$(command -v swtpm_setup || echo /usr/bin/swtpm_setup)"
setup_config = "$CONF_DIR/swtpm/swtpm_setup.conf"
ek_ca_certificate = "$STATE_ROOT/swtpm-localca/issuercert.pem"

[storage_classes]
EOF
    local pair
    for pair in $STORAGE_CLASSES; do echo "${pair%%=*} = \"${pair#*=}\""; done
    echo
    echo "[network_classes]"
    for pair in $NETWORK_CLASSES; do echo "${pair%%=*} = \"${pair#*=}\""; done
  } >"$tmp"
  install -m 0640 -o root -g "$BANLIEUE_USER" "$tmp" "$HOST_CONFIG"
  rm -f "$tmp"
}

# -------------------------------------------------------------------- tpm ---
# The per-host EK CA (ADR-0065 Decision 6). Its private key is readable by the
# provider's user ONLY. TPM state must therefore be manufactured as that user,
# never as a guest's uid: a guest uid that could read this key could mint EK
# certificates this host's CA vouches for.
setup_tpm_ca() {
  require_root
  local ca="$STATE_ROOT/swtpm-localca" sdir="$CONF_DIR/swtpm"
  install -d -m 0700 -o "$BANLIEUE_USER" -g "$BANLIEUE_USER" "$ca"
  install -d -m 0755 "$sdir"

  if ! keep_existing "$sdir/swtpm_setup.conf"; then
    log "Writing swtpm_setup / swtpm_localca configuration"
    cat >"$sdir/swtpm-localca.conf" <<EOF
statedir = $ca
signingkey = $ca/signkey.pem
issuercert = $ca/issuercert.pem
certserial = $ca/certserial
EOF
    cat >"$sdir/swtpm-localca.options" <<EOF
--platform-manufacturer banlieue
--platform-version 2.1
--platform-model cloud-hypervisor
EOF
    cat >"$sdir/swtpm_setup.conf" <<EOF
create_certs_tool = $(command -v swtpm_localca || echo /usr/bin/swtpm_localca)
create_certs_tool_config = $sdir/swtpm-localca.conf
create_certs_tool_options = $sdir/swtpm-localca.options
active_pcr_banks = sha256
EOF
    chmod 0644 "$sdir"/swtpm-localca.conf "$sdir"/swtpm-localca.options "$sdir"/swtpm_setup.conf
  fi

  if [[ "$FORCE" == "true" ]]; then
    warn "FORCE=true: rotating the EK CA. Every EK certificate issued here stops verifying."
    rm -f "$ca"/*.pem "$ca"/certserial
  fi
  # swtpm_localca creates the CA on first use; do that now, as the provider's
  # user, so the key is born with the right owner and the CA certificate
  # exists to publish (ADR-0065 Decision 6).
  if [[ ! -f "$ca/issuercert.pem" ]]; then
    log "Creating the per-host EK CA"
    local tmp; tmp="$(mktemp -d)"; chown "$BANLIEUE_USER:" "$tmp"
    runuser -u "$BANLIEUE_USER" -- swtpm_setup --tpm2 --tpmstate "$tmp" \
      --create-ek-cert --config "$sdir/swtpm_setup.conf" >/dev/null
    rm -rf "$tmp"
  fi
  chmod 0600 "$ca"/*.pem 2>/dev/null || true
  chmod 0644 "$ca/issuercert.pem"
  log "  EK CA: $(openssl x509 -in "$ca/issuercert.pem" -noout -subject 2>/dev/null || echo "$ca/issuercert.pem")"
}

# ----------------------------------------------------------------- polkit ---
setup_polkit() {
  require_root
  local rule=/etc/polkit-1/rules.d/60-banlieue-cloud-hypervisor.rules
  log "Writing $rule"
  install -d -m 0755 /etc/polkit-1/rules.d
  cat >"$rule" <<EOF
// Written by bootstrap-cloud-hypervisor-host.sh (ADR-0063 Decision 6).
// The provider user may manage ITS OWN transient units and nothing else:
// per machine a VMM, an swtpm and a one-shot TPM-setup unit, and per image an
// import unit, each named by Kubernetes UID (ADR-0063, ADR-0064, ADR-0065).
polkit.addRule(function(action, subject) {
    if (action.id != "org.freedesktop.systemd1.manage-units" ||
        subject.user != "$BANLIEUE_USER") {
        return polkit.Result.NOT_HANDLED;
    }
    var unit = action.lookup("unit") || "";
    var verb = action.lookup("verb") || "";
    var ours = /^banlieue-(ch|swtpm|swtpm-setup|ch-import)-[0-9a-f-]{36}\\.service\$/;
    if (ours.test(unit) &&
        ["start", "stop", "restart", "kill", "reset-failed"].indexOf(verb) >= 0) {
        return polkit.Result.YES;
    }
    return polkit.Result.NOT_HANDLED;
});
EOF
  chmod 0644 "$rule"
}

# --------------------------------------------------------------- provider ---
setup_provider_unit() {
  require_root
  resolve_classes
  if [[ -n "$BANLIEUE_BINARY_SRC" ]]; then
    log "Installing $BANLIEUE_BINARY_SRC as $PROVIDER_BINARY"
    install -m 0755 "$BANLIEUE_BINARY_SRC" "$PROVIDER_BINARY"
  fi

  local unit="/etc/systemd/system/$PROVIDER_UNIT"
  log "Writing $unit"
  cat >"$unit" <<EOF
# Written by bootstrap-cloud-hypervisor-host.sh.
# banlieue's host-resident Cloud Hypervisor provider (ADR-0060).
[Unit]
Description=banlieue Cloud Hypervisor provider
Documentation=https://github.com/firestoned/banlieue
Wants=network-online.target
After=network-online.target dbus.service systemd-tmpfiles-setup.service
ConditionPathExists=$PROVIDER_BINARY
ConditionPathExists=$KUBECONFIG_PATH

[Service]
User=$BANLIEUE_USER
Group=$BANLIEUE_USER
ExecStart=$PROVIDER_BINARY provider cloud-hypervisor --config $HOST_CONFIG
# Taps over netlink, and handing files to guest uids (ADR-0063 Decision 6).
AmbientCapabilities=CAP_NET_ADMIN CAP_CHOWN CAP_FOWNER
CapabilityBoundingSet=CAP_NET_ADMIN CAP_CHOWN CAP_FOWNER
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=$RUN_ROOT $STATE_ROOT $(for p in $STORAGE_CLASSES; do printf '%s ' "${p#*=}"; done)$KUBECONFIG_PATH
# Guests are transient units owned by systemd, not children of this process,
# so stopping or upgrading the provider must not touch them (ADR-0063 D1).
KillMode=process
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  if [[ -x "$PROVIDER_BINARY" && -f "$KUBECONFIG_PATH" ]]; then
    systemctl enable --now "$PROVIDER_UNIT"
    log "  $PROVIDER_UNIT enabled"
  else
    log "  unit installed, NOT enabled: needs $PROVIDER_BINARY and $KUBECONFIG_PATH"
    log "  (the kubeconfig comes from \`banlieue bootstrap\`, ADR-0060 Decision 5)"
  fi
}

# --------------------------------------------------------------- selftest ---
# Proves the pieces fit without booting a guest: the pinned VMM runs, the
# firmware matches its pin, the provider user can open /dev/kvm, and a vTPM
# can be manufactured with an EK certificate carrying a <name>:<uid> CN, the
# check ADR-0045/ADR-0065 rely on.
selftest() {
  require_root
  local ok=true tmp cn want
  log "Self-test"
  if "$BIN_DIR/cloud-hypervisor" --version >/dev/null; then
    log "  cloud-hypervisor runs"
  else
    warn "  cloud-hypervisor does not run"; ok=false
  fi
  if [[ "$(sha256sum "$OPT_ROOT/firmware/$FIRMWARE_TAG/CLOUDHV.fd" | awk '{print $1}')" == "$FIRMWARE_SHA256" ]]; then
    log "  CLOUDHV.fd matches its pin"
  else
    warn "  CLOUDHV.fd checksum drifted"; ok=false
  fi
  if runuser -u "$BANLIEUE_USER" -- test -r /dev/kvm -a -w /dev/kvm; then
    log "  $BANLIEUE_USER can open /dev/kvm"
  else
    warn "  $BANLIEUE_USER cannot open /dev/kvm"; ok=false
  fi

  tmp="$(mktemp -d)"; install -d -o "$BANLIEUE_USER" "$tmp/state"; chown "$BANLIEUE_USER:" "$tmp"
  want="banlieue-selftest:00000000-0000-4000-8000-000000000000"
  if runuser -u "$BANLIEUE_USER" -- swtpm_setup --tpm2 --tpmstate "$tmp/state" \
       --create-ek-cert --vmid "$want" --config "$CONF_DIR/swtpm/swtpm_setup.conf" \
       --write-ek-cert-files "$tmp" >/dev/null 2>&1; then
    cn="$(openssl x509 -inform DER -in "$tmp"/ek-rsa2048.crt -noout -subject 2>/dev/null | sed 's/^subject=\s*CN\s*=\s*//')"
    if [[ "$cn" == "$want" ]]; then
      log "  vTPM manufactured, EK CN = $cn"
    else
      warn "  EK CN is '$cn', want '$want'"; ok=false
    fi
  else
    warn "  swtpm_setup failed as $BANLIEUE_USER"; ok=false
  fi
  rm -rf "$tmp"

  [[ "$ok" == "true" ]] || { warn "self-test failed"; exit 1; }
  log "  self-test ok"
}

# ----------------------------------------------------------------- status ---
status() {
  echo "--- host ---"
  echo "  $(. /etc/os-release && echo "$PRETTY_NAME")  kernel $(uname -r)  $(nproc) vCPU  virt=$(systemd-detect-virt --vm 2>/dev/null || echo none)"
  free -h | awk '/Mem:/ {printf "  memory %s total, %s available\n", $2, $7}'
  echo "--- vmm ---"
  printf '  %-18s %s\n' cloud-hypervisor "$("$BIN_DIR/cloud-hypervisor" --version 2>/dev/null | head -1 || echo MISSING)"
  printf '  %-18s %s\n' firmware "$(find "$OPT_ROOT/firmware" -name CLOUDHV.fd 2>/dev/null | head -1 | grep . || echo MISSING)"
  printf '  %-18s %s\n' swtpm "$(swtpm --version 2>/dev/null | head -1 || echo MISSING)"
  echo "--- config ---"
  printf '  %-18s %s\n' host-config "$([[ -f "$HOST_CONFIG" ]] && echo "$HOST_CONFIG" || echo MISSING)"
  printf '  %-18s %s\n' ek-ca "$([[ -f "$STATE_ROOT/swtpm-localca/issuercert.pem" ]] && echo present || echo MISSING)"
  printf '  %-18s %s\n' polkit-rule "$([[ -f /etc/polkit-1/rules.d/60-banlieue-cloud-hypervisor.rules ]] && echo present || echo MISSING)"
  printf '  %-18s %s\n' kubeconfig "$([[ -f "$KUBECONFIG_PATH" ]] && echo present || echo "absent (banlieue bootstrap)")"
  echo "--- provider ---"
  printf '  %-18s %s\n' unit "$(systemctl is-active "$PROVIDER_UNIT" 2>/dev/null || true)"
  echo "--- guests ---"
  systemctl list-units --no-legend --plain "${UNIT_PREFIX_VMM}*" "${UNIT_PREFIX_TPM}*" 2>/dev/null \
    | awk '{print "  " $1 "  " $3 "/" $4}' | head -20
}

print_env_template() {
  cat <<'TEMPLATE'
# banlieue Cloud Hypervisor host settings.
#
# Keep this file OUTSIDE the repository -- it names real hosts. Convention:
# $HOME/.config/banlieue/hosts/<name>.env, used as
#
#   BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
#     ./scripts/bootstrap-cloud-hypervisor-host.sh --remote admin@bar.foo.io all

# The Provider this host is (one Provider, one host -- ADR-0060).
#PROVIDER_NAME=bar
#PROVIDER_NAMESPACE=banlieue-system

# name=path. Unset = one `default` class on the largest candidate mount.
#STORAGE_CLASSES="default=/srv/banlieue/ch fast=/nvme/banlieue/ch"

# name=bridge. The bridge must already exist; this script never creates one.
# Unset = `default` on virbr0, if present.
#NETWORK_CLASSES="default=br0"

# Guest uid range (one uid per guest). Must not overlap accounts or subuids.
#GUEST_UID_BASE=2000000
#GUEST_UID_COUNT=10000

# Pinned VMM. Change the version only together with its checksums.
#CH_VERSION=v53.0
#CH_SHA256=
#CH_REMOTE_SHA256=
#FIRMWARE_TAG=ch-97eeb7b09
#FIRMWARE_SHA256=

# A banlieue binary to install as the provider. Unset = leave it alone.
#BANLIEUE_BINARY_SRC=/tmp/banlieue

# Lab hosts only: allow running inside a VM (nested virtualization).
#ALLOW_VIRTUALIZED_HOST=false
TEMPLATE
}

usage() {
  cat >&2 <<'USAGE'
Usage: bootstrap-cloud-hypervisor-host.sh [step]
       bootstrap-cloud-hypervisor-host.sh --remote <user@host> [step]
       bootstrap-cloud-hypervisor-host.sh --print-env-template

  all        preflight + packages + vmm + host + tpm + polkit + provider + selftest
  preflight  bare metal, /dev/kvm, bridges, guest uid range; changes nothing
  packages   swtpm, polkit, iproute2 (no qemu, no libvirt)
  vmm        cloud-hypervisor, ch-remote, CLOUDHV.fd -- pinned and verified
  host       banlieue user, storage and run directories, host config file
  tpm        per-host EK CA for swtpm_localca
  polkit     rule scoping the provider to its own units
  provider   the provider's systemd unit (enabled only once it can run)
  selftest   VMM, firmware pin, /dev/kvm, vTPM + EK CN; boots nothing
  status     report what is installed; changes nothing

Configuration is environment variables; BANLIEUE_ENV_FILE points at a file of
them. See --print-env-template.
USAGE
  exit 1
}

main() {
  case "${1:-all}" in
    preflight) preflight ;;
    packages)  install_packages ;;
    vmm)       install_vmm ;;
    host)      setup_host ;;
    tpm)       setup_tpm_ca ;;
    polkit)    setup_polkit ;;
    provider)  setup_provider_unit ;;
    selftest)  selftest ;;
    status)    status ;;
    --print-env-template) print_env_template ;;
    all)
      require_root
      preflight
      install_packages
      install_vmm
      setup_host
      setup_tpm_ca
      setup_polkit
      setup_provider_unit
      selftest
      echo
      status
      ;;
    *) usage ;;
  esac
}

main "$@"
