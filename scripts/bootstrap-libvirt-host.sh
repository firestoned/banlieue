#!/usr/bin/env bash
# Turns a bare Debian/Ubuntu machine into a KVM/libvirt hypervisor ready for
# scripts/bootstrap-libvirt-tls.sh and scripts/bootstrap-k0s-cluster.sh.
#
# Those two scripts both start from an already-working host: bootstrap-k0s-cluster.sh
# fails in check_deps_libvirt without virt-install/virsh/qemu-img/k0sctl/kubectl/
# genisoimage, and bootstrap-libvirt-tls.sh needs a running libvirtd plus certtool.
# Nothing else in this repository gets you there from a fresh install. This does.
#
# Run this ON the target host, as root:
#
#   sudo ./scripts/bootstrap-libvirt-host.sh all
#
# The full path from nothing to a running cluster:
#
#   sudo ./scripts/bootstrap-libvirt-host.sh all   # this script
#   sudo ./scripts/bootstrap-libvirt-tls.sh all    # PKI + mutual TLS on 16514
#   ./scripts/bootstrap-k0s-cluster.sh all         # Kairos VMs + k0sctl
#
# Every step is idempotent; re-running is safe and is the intended way to change
# a setting. All configuration is environment variables -- see --print-env-template
# for a starting point. Per-host settings belong OUTSIDE this repository, in
# $HOME/.config/banlieue/hosts/<name>.env, sourced via BANLIEUE_ENV_FILE.
set -euo pipefail

BANLIEUE_ENV_FILE="${BANLIEUE_ENV_FILE:-}"
if [[ "${1:-}" != "--print-env-template" && -n "$BANLIEUE_ENV_FILE" ]]; then
  [[ -f "$BANLIEUE_ENV_FILE" ]] || { echo "BANLIEUE_ENV_FILE=$BANLIEUE_ENV_FILE not found" >&2; exit 1; }
  # shellcheck disable=SC1090  # path is operator-supplied by design
  source "$BANLIEUE_ENV_FILE"
fi

# The account that will drive libvirt (added to the libvirt and kvm groups).
# Defaults to whoever invoked sudo, which is almost always right.
LIBVIRT_USER="${LIBVIRT_USER:-${SUDO_USER:-root}}"

# Where disk images live. Empty means "pick the filesystem with the most free
# space" -- see pick_pool_root. A stock Debian install puts /var on a small
# partition, and /var/lib/libvirt/images fills after two or three VMs, so the
# default deliberately does NOT follow libvirt's own convention.
POOL_ROOT="${POOL_ROOT:-}"
POOL_NAME="${POOL_NAME:-default}"
ISO_POOL_NAME="${ISO_POOL_NAME:-iso}"
POOL_CANDIDATES="${POOL_CANDIDATES:-/srv /data /home /opt /var/lib}"

# Client tooling for bootstrap-k0s-cluster.sh.
INSTALL_K0S_TOOLS="${INSTALL_K0S_TOOLS:-true}"
K0SCTL_VERSION="${K0SCTL_VERSION:-v0.32.2}"
# kubectl is pinned to the k0s MINOR version rather than "stable". kubectl
# supports only +/-1 minor against the API server; "stable" runs ahead of the
# k0s release train, so following it eventually yields a client that refuses to
# talk to the cluster this repo builds. Empty = derive from K0S_VERSION.
K0S_VERSION="${K0S_VERSION:-1.35.5+k0s.0}"
KUBECTL_VERSION="${KUBECTL_VERSION:-}"

# Optional extras, both off by default.
INSTALL_COCKPIT="${INSTALL_COCKPIT:-false}"
# localhost | lan. Cockpit authenticates with PAM, so `lan` exposes a
# password-guessable administrative path to the whole network. Reach the
# default with: ssh -L 9090:localhost:9090 <host>
COCKPIT_BIND="${COCKPIT_BIND:-localhost}"

INSTALL_TAILSCALE="${INSTALL_TAILSCALE:-false}"
TAILSCALE_AUTHKEY="${TAILSCALE_AUTHKEY:-}"
TS_HOSTNAME="${TS_HOSTNAME:-}"
TS_SSH="${TS_SSH:-true}"
TS_ADVERTISE_ROUTES="${TS_ADVERTISE_ROUTES:-}"

ARCH="${ARCH:-amd64}"

# Pin every virsh call to the system instance. Run by a non-root user, virsh
# otherwise defaults to qemu:///session -- a separate, empty, per-user libvirt
# that reports no pools and no networks, so `status` silently claims a
# correctly-provisioned host is unconfigured.
export LIBVIRT_DEFAULT_URI="${LIBVIRT_DEFAULT_URI:-qemu:///system}"

log()  { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

require_root() {
  [[ $EUID -eq 0 ]] || { warn "must run as root (installs packages, writes /etc)"; exit 1; }
}

require_apt() {
  command -v apt-get >/dev/null 2>&1 \
    || { warn "this script supports Debian/Ubuntu (apt-get) only"; exit 1; }
}

# ---------------------------------------------------------------- packages ---
install_base() {
  require_root; require_apt
  log "Enabling the en_US.UTF-8 locale"
  if [[ -f /etc/locale.gen ]]; then
    sed -i 's/^# *en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/' /etc/locale.gen
    locale-gen >/dev/null
  fi

  log "Installing base tooling"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends \
    git curl wget ca-certificates rsync less locales gnutls-bin
}

install_libvirt() {
  require_root; require_apt
  log "Installing the KVM/libvirt stack"
  export DEBIAN_FRONTEND=noninteractive
  apt-get install -y -qq \
    qemu-system-x86 qemu-utils \
    libvirt-daemon-system libvirt-clients libvirt-daemon-config-network \
    virtinst bridge-utils dnsmasq-base ovmf swtpm swtpm-tools \
    libnss-libvirt cpu-checker guestfs-tools

  log "Checking hardware virtualization"
  kvm-ok || warn "kvm-ok reported a problem -- check the BIOS VT-x/AMD-V setting"

  log "Adding $LIBVIRT_USER to the libvirt and kvm groups"
  if [[ "$LIBVIRT_USER" != "root" ]]; then
    usermod -aG libvirt,kvm "$LIBVIRT_USER"
    log "  $LIBVIRT_USER must log out and back in for this to take effect"
  fi

  log "Enabling libvirtd"
  systemctl enable --now libvirtd
  systemctl is-active --quiet libvirtd && log "  libvirtd active"
}

# ------------------------------------------------------------------ pools ---
# Rank the candidate mount points by free space. Disk images are the largest
# thing this host will ever store, and the stock location is on whichever
# partition the installer made smallest.
pick_pool_root() {
  local best="" best_avail=0 mp avail
  for mp in $POOL_CANDIDATES; do
    [[ -d "$mp" ]] || continue
    avail="$(df -P --output=avail "$mp" 2>/dev/null | tail -1 | tr -d ' ')" || continue
    [[ -n "$avail" ]] || continue
    if (( avail > best_avail )); then best_avail="$avail"; best="$mp"; fi
  done
  echo "${best:-/var/lib}/libvirt"
}

define_pool() {
  local name="$1" path="$2" current
  if virsh pool-info "$name" >/dev/null 2>&1; then
    current="$(virsh pool-dumpxml "$name" | sed -n 's:.*<path>\(.*\)</path>.*:\1:p' | head -1)"
    if [[ "$current" != "$path" ]]; then
      log "  repointing pool '$name': $current -> $path"
      virsh pool-destroy  "$name" >/dev/null 2>&1 || true
      virsh pool-undefine "$name" >/dev/null 2>&1 || true
    fi
  fi
  virsh pool-info "$name" >/dev/null 2>&1 || virsh pool-define-as "$name" dir --target "$path" >/dev/null
  virsh pool-autostart "$name" >/dev/null
  virsh pool-start "$name" >/dev/null 2>&1 || true
}

setup_pools() {
  require_root
  local root images isos
  root="${POOL_ROOT:-$(pick_pool_root)}"
  images="$root/images"; isos="$root/iso"

  log "Storage pools under $root ($(df -h --output=avail "$root" 2>/dev/null | tail -1 | tr -d ' ') free)"
  # install -d -m sets the mode explicitly. A plain mkdir under a restrictive
  # umask yields drwx------, and libvirt's qemu user then cannot traverse to
  # the images -- a failure that surfaces as an opaque permission error at
  # VM start, far from its cause.
  install -d -m 0771 "$images" "$isos"
  chown root:libvirt "$images" "$isos"

  define_pool "$POOL_NAME"     "$images"
  define_pool "$ISO_POOL_NAME" "$isos"

  log "Enabling the default NAT network"
  virsh net-autostart default >/dev/null 2>&1 || true
  virsh net-start default >/dev/null 2>&1 || log "  default network already running"
}

# ------------------------------------------------------------- k0s tooling ---
# Resolve the kubectl patch release matching the k0s minor version.
resolve_kubectl_version() {
  [[ -n "$KUBECTL_VERSION" ]] && { echo "$KUBECTL_VERSION"; return 0; }
  local minor ver
  minor="$(echo "$K0S_VERSION" | sed -n 's/^\([0-9]*\.[0-9]*\).*/\1/p')"
  if [[ -n "$minor" ]] && ver="$(curl -fsSL "https://dl.k8s.io/release/stable-${minor}.txt" 2>/dev/null)"; then
    echo "$ver"
  else
    warn "could not resolve kubectl for k0s minor '${minor:-unknown}'; set KUBECTL_VERSION"
    return 1
  fi
}

install_k0s_tools() {
  require_root; require_apt
  log "Installing genisoimage (cloud-init seed ISOs)"
  export DEBIAN_FRONTEND=noninteractive
  apt-get install -y -qq --no-install-recommends genisoimage

  log "Installing k0sctl $K0SCTL_VERSION"
  if command -v k0sctl >/dev/null 2>&1 && k0sctl version 2>/dev/null | grep -q "${K0SCTL_VERSION#v}"; then
    log "  already at $K0SCTL_VERSION"
  else
    local tmp; tmp="$(mktemp -d)"
    curl -fsSL -o "$tmp/k0sctl" \
      "https://github.com/k0sproject/k0sctl/releases/download/${K0SCTL_VERSION}/k0sctl-linux-${ARCH}"
    install -m 0755 "$tmp/k0sctl" /usr/local/bin/k0sctl
    rm -rf "$tmp"
  fi

  local kver; kver="$(resolve_kubectl_version)" || return 1
  log "Installing kubectl $kver (matched to k0s $K0S_VERSION)"
  # Guard on `command -v` first. Probing an ABSENT kubectl here exits 127, and
  # under `set -o pipefail` that propagates through the pipeline to the
  # assignment, where `set -e` kills the script -- silently, because 2>/dev/null
  # swallows the shell's "command not found". The install would then never run
  # on precisely the hosts that need it.
  local have=""
  if command -v kubectl >/dev/null 2>&1; then
    have="$(kubectl version --client -o json 2>/dev/null | sed -n 's/.*"gitVersion": *"\([^"]*\)".*/\1/p' | head -1 || true)"
  fi
  if [[ "$have" == "$kver" ]]; then
    log "  already at $kver"
  else
    local tmp; tmp="$(mktemp -d)"
    curl -fsSL -o "$tmp/kubectl"        "https://dl.k8s.io/release/${kver}/bin/linux/${ARCH}/kubectl"
    curl -fsSL -o "$tmp/kubectl.sha256" "https://dl.k8s.io/release/${kver}/bin/linux/${ARCH}/kubectl.sha256"
    echo "$(cat "$tmp/kubectl.sha256")  $tmp/kubectl" | sha256sum -c - >/dev/null \
      || { warn "kubectl checksum mismatch"; rm -rf "$tmp"; exit 1; }
    install -m 0755 "$tmp/kubectl" /usr/local/bin/kubectl
    rm -rf "$tmp"
  fi
}

# ----------------------------------------------------------------- extras ---
install_cockpit() {
  require_root; require_apt
  local dropin_dir=/etc/systemd/system/cockpit.socket.d
  log "Installing Cockpit (bind: $COCKPIT_BIND)"
  export DEBIAN_FRONTEND=noninteractive
  apt-get install -y -qq --no-install-recommends \
    cockpit cockpit-machines cockpit-storaged cockpit-networkmanager

  case "$COCKPIT_BIND" in
    localhost)
      mkdir -p "$dropin_dir"
      # The empty ListenStream= is required: it CLEARS the vendor unit's
      # 0.0.0.0:9090. Without it systemd appends and the socket stays exposed.
      printf '[Socket]\nListenStream=\nListenStream=127.0.0.1:9090\n' >"$dropin_dir/10-listen.conf"
      log "  127.0.0.1:9090 -- tunnel with: ssh -L 9090:localhost:9090 <host>"
      ;;
    lan)
      rm -f "$dropin_dir/10-listen.conf"
      warn "  listening on all interfaces; PAM login is exposed to the network"
      ;;
    *) warn "COCKPIT_BIND must be 'localhost' or 'lan', got '$COCKPIT_BIND'"; exit 1 ;;
  esac
  systemctl daemon-reload
  systemctl enable cockpit.socket >/dev/null 2>&1 || true
  systemctl restart cockpit.socket
}

install_tailscale() {
  require_root; require_apt
  local codename
  # shellcheck disable=SC1091  # os-release is present on every supported target
  . /etc/os-release
  codename="${VERSION_CODENAME:-stable}"

  log "Adding the Tailscale apt repository ($codename)"
  export DEBIAN_FRONTEND=noninteractive
  apt-get install -y -qq --no-install-recommends curl ca-certificates
  if [[ ! -f /usr/share/keyrings/tailscale-archive-keyring.gpg ]]; then
    curl -fsSL "https://pkgs.tailscale.com/stable/debian/${codename}.noarmor.gpg" \
      -o /usr/share/keyrings/tailscale-archive-keyring.gpg
    chmod 0644 /usr/share/keyrings/tailscale-archive-keyring.gpg
  fi
  curl -fsSL "https://pkgs.tailscale.com/stable/debian/${codename}.tailscale-keyring.list" \
    -o /etc/apt/sources.list.d/tailscale.list
  chmod 0644 /etc/apt/sources.list.d/tailscale.list

  log "Installing tailscale"
  apt-get update -qq
  apt-get install -y -qq tailscale
  systemctl enable --now tailscaled

  local state
  state="$(tailscale status --json 2>/dev/null | sed -n 's/.*"BackendState": *"\([A-Za-z]*\)".*/\1/p' | head -1 || true)"
  if [[ "$state" == "Running" ]]; then
    log "  already Running -- leaving the existing session alone"
    return 0
  fi

  local args=(--hostname "${TS_HOSTNAME:-$(hostname -s)}")
  [[ "$TS_SSH" == "true" ]]       && args+=(--ssh)
  [[ -n "$TS_ADVERTISE_ROUTES" ]] && args+=(--advertise-routes "$TS_ADVERTISE_ROUTES")

  if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
    log "  using TAILSCALE_AUTHKEY (${#TAILSCALE_AUTHKEY} chars, not logged)"
    # Keys expire, get spent, or are refused by tailnet ACLs. None of those
    # should abort the whole run when a browser login would still work.
    tailscale up --authkey "$TAILSCALE_AUTHKEY" "${args[@]}" || {
      warn "  auth key rejected; falling back to interactive authentication"
      tailscale up "${args[@]}"
    }
  else
    log "  no TAILSCALE_AUTHKEY -- authorise this host in a browser:"
    tailscale up "${args[@]}"
  fi
}

# ----------------------------------------------------------------- status ---
status() {
  echo "--- host ---"
  echo "  $(. /etc/os-release && echo "$PRETTY_NAME")  kernel $(uname -r)  $(nproc) vCPU"
  free -h | awk '/Mem:/ {printf "  memory %s total, %s available\n", $2, $7}'
  echo "--- tooling ---"
  local c
  for c in virt-install virsh qemu-img certtool k0sctl kubectl genisoimage curl ssh; do
    printf '  %-14s %s\n' "$c" "$(command -v "$c" || echo MISSING)"
  done
  echo "--- libvirt ---"
  systemctl is-active libvirtd >/dev/null 2>&1 \
    && echo "  libvirtd active" || echo "  libvirtd NOT active"
  virsh pool-list --all 2>/dev/null | sed 's/^/  /'
  virsh net-list --all 2>/dev/null | sed 's/^/  /'
  echo "--- extras ---"
  printf '  %-14s %s\n' cockpit   "$(systemctl is-active cockpit.socket 2>/dev/null || echo absent)"
  printf '  %-14s %s\n' tailscale "$(tailscale ip -4 2>/dev/null || echo absent)"
}

print_env_template() {
  cat <<'TEMPLATE'
# banlieue libvirt host settings.
#
# Keep this file OUTSIDE the repository -- it names real hosts. The convention
# is $HOME/.config/banlieue/hosts/<name>.env, used as:
#
#   BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
#     sudo -E ./scripts/bootstrap-libvirt-host.sh all

# Account that drives libvirt. Defaults to $SUDO_USER.
#LIBVIRT_USER=admin

# Where disk images live. Unset = the candidate mount point with the most free
# space, which is usually right and is rarely /var/lib.
#POOL_ROOT=/srv/libvirt
#POOL_CANDIDATES="/srv /data /home /opt /var/lib"

# Client tooling for bootstrap-k0s-cluster.sh.
INSTALL_K0S_TOOLS=true
#K0S_VERSION=1.35.5+k0s.0
#K0SCTL_VERSION=v0.32.2
# Unset = the kubectl patch matching K0S_VERSION's minor. Pin only to override.
#KUBECTL_VERSION=

# Cockpit web console. `lan` exposes a PAM login to the whole network; the
# default binds localhost and expects an SSH tunnel.
INSTALL_COCKPIT=false
#COCKPIT_BIND=localhost

# Tailscale. Put the auth key in a separate 0600 file if this one is shared;
# an unset key falls back to interactive browser authentication.
INSTALL_TAILSCALE=false
#TAILSCALE_AUTHKEY=
#TS_HOSTNAME=bar
#TS_ADVERTISE_ROUTES=192.0.2.0/24
TEMPLATE
}

usage() {
  cat >&2 <<'USAGE'
Usage: bootstrap-libvirt-host.sh [all|base|libvirt|pools|tools|cockpit|tailscale|status]
       bootstrap-libvirt-host.sh --print-env-template

  all        base + libvirt + pools + tools, then cockpit/tailscale if enabled
  base       locale and base packages
  libvirt    qemu/libvirt packages, groups, daemon
  pools      storage pools and the default NAT network
  tools      k0sctl, kubectl, genisoimage
  cockpit    Cockpit web console (INSTALL_COCKPIT/COCKPIT_BIND)
  tailscale  join a tailnet (INSTALL_TAILSCALE/TAILSCALE_AUTHKEY)
  status     report what is installed; changes nothing

Configuration is entirely environment variables; BANLIEUE_ENV_FILE points at a
file of them. See --print-env-template.
USAGE
  exit 1
}

main() {
  case "${1:-all}" in
    base)      install_base ;;
    libvirt)   install_libvirt ;;
    pools)     setup_pools ;;
    tools)     install_k0s_tools ;;
    cockpit)   install_cockpit ;;
    tailscale) install_tailscale ;;
    status)    status ;;
    --print-env-template) print_env_template ;;
    all)
      install_base
      install_libvirt
      setup_pools
      [[ "$INSTALL_K0S_TOOLS" == "true" ]] && install_k0s_tools
      [[ "$INSTALL_COCKPIT"   == "true" ]] && install_cockpit
      [[ "$INSTALL_TAILSCALE" == "true" ]] && install_tailscale
      echo
      status
      echo
      log "Next: sudo ./scripts/bootstrap-libvirt-tls.sh all"
      log "Then: ./scripts/bootstrap-k0s-cluster.sh all"
      ;;
    *) usage ;;
  esac
}

main "$@"
