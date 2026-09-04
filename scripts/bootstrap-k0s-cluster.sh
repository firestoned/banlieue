#!/usr/bin/env bash
# Bootstraps the k0s MANAGEMENT cluster that runs the banlieue controllers and
# kairos-operator, then installs k0s onto its nodes with k0sctl. This is the
# substrate `banlieue bootstrap` (ADR-0013) later installs onto -- it is NOT a
# workload provisioned through banlieue itself.
#
# Two provisioning backends, selected with BACKEND (ADR-0017):
#
#   BACKEND=libvirt  (default) -- create Kairos "Hadron" VMs with
#       virt-install on a KVM/libvirt host, let Kairos install itself from its
#       ISO onto an empty disk, then k0sctl. Run on the hypervisor host (or
#       point LIBVIRT_URI at a remote one). Unchanged from the original script.
#
#   BACKEND=vsphere  -- clone cluster-specific Kairos VM *templates* in a
#       VMware vSphere estate with govc, reconfigure networking/CPU/memory/disk,
#       let the template's baked-in installer run unattended, then k0sctl. No
#       ISO fetch, no install-media dance -- the template already carries it.
#       Nodes are spread evenly across several vSphere compute clusters so each
#       is an etcd FAILURE DOMAIN (losing one cluster costs one control-plane
#       node, not the cluster). See ADR-0002 for the same reasoning applied to
#       InfraClusters.
#
# ---------------------------------------------------------------------------
# NO REAL INFRASTRUCTURE IN THIS FILE (rules/no-real-infrastructure.md)
# ---------------------------------------------------------------------------
# banlieue is a public repo. This script therefore contains NO real vCenter
# hostname, datacenter, resource-pool name, subnet, DNS server, or node IP.
# Every environment-specific value for the vSphere backend comes from:
#   1. the ambient GOVC_* environment (vCenter URL / creds / datacenter / CA),
#   2. `govc` discovery at runtime, and
#   3. an operator-supplied, UNTRACKED env file (BANLIEUE_ENV_FILE=...), sourced
#      before anything else, which declares the node table and per-cluster
#      placement. A template for that file is printed by `--print-env-template`.
#
# Corporate HTTP proxies are unset before every on-prem govc/kubectl call.
#
# ---------------------------------------------------------------------------
# k0s: not baked into any image. k0sctl installs exactly K0S_VERSION on every
# node, uploading the binary over SSH (uploadBinary: true).
#
# Usage:
#   ./bootstrap-k0s-cluster.sh [all|vms|config|apply|kubeconfig|label|flux|destroy]
#   BANLIEUE_ENV_FILE=~/.k0s/banlieue.env BACKEND=vsphere ./bootstrap-k0s-cluster.sh all
#   ./bootstrap-k0s-cluster.sh --print-env-template   # scaffold for the vSphere env file
#
#   FLUX_ENABLED=true (in the env file) adds an opt-in `flux` step -- fetches a
#   registry credential from Vault (token auth via the `vault` CLI; VAULT_ADDR/
#   VAULT_TOKEN come from your shell, not the env file) and pushes flux-operator
#   + flux-core manifests onto a controller (ADR-0018).
#
# All settings below can be overridden via environment variables (the Makefile
# forwards its own variables the same way).
set -euo pipefail

# --- vSphere env file (UNTRACKED) -----------------------------------------
# Sourced FIRST so it can declare the node table (NODES) and the per-cluster
# placement maps (VSPHERE_RP / VSPHERE_DSC / VSPHERE_NET / VSPHERE_TPL), plus
# override any default below. Keep this file OUTSIDE the repo -- it holds real
# hostnames and IPs.
BANLIEUE_ENV_FILE="${BANLIEUE_ENV_FILE:-}"
if [[ "${1:-}" != "--print-env-template" && -n "$BANLIEUE_ENV_FILE" ]]; then
  [[ -f "$BANLIEUE_ENV_FILE" ]] || { echo "BANLIEUE_ENV_FILE=$BANLIEUE_ENV_FILE not found" >&2; exit 1; }
  # shellcheck disable=SC1090  # path is operator-supplied by design
  source "$BANLIEUE_ENV_FILE"
fi

# Which backend provisions the VMs.
BACKEND="${BACKEND:-libvirt}"

VM_COUNT="${VM_COUNT:-4}"
VCPUS="${VCPUS:-2}"
MEM_MB="${MEM_MB:-8192}"
DISK_GB="${DISK_GB:-25}"
VM_PREFIX="${VM_PREFIX:-k0s}"

LIBVIRT_URI="${LIBVIRT_URI:-qemu:///system}"
LIBVIRT_NETWORK="${LIBVIRT_NETWORK:-default}"
LIBVIRT_POOL="${LIBVIRT_POOL:-default}"
# No osinfo entry exists for Kairos; `generic` is the right fallback.
OS_VARIANT="${OS_VARIANT:-generic}"
# Kairos Hadron CORE installer ISO -- deliberately NOT the `standard` flavor,
# which would bundle its own k0s/k3s at a version we don't control.
IMAGE_URL="${IMAGE_URL:-https://github.com/kairos-io/kairos/releases/download/v4.1.2/kairos-hadron-v0.4.0-core-amd64-generic-v4.1.2.iso}"
IP_WAIT_ATTEMPTS="${IP_WAIT_ATTEMPTS:-60}"   # 60 * 5s = 5min
SSH_WAIT_ATTEMPTS="${SSH_WAIT_ATTEMPTS:-60}" # 60 * 5s = 5min
TAILSCALE_WAIT_ATTEMPTS="${TAILSCALE_WAIT_ATTEMPTS:-24}" # 24 * 5s = 2min
INSTALL_WAIT_ATTEMPTS="${INSTALL_WAIT_ATTEMPTS:-180}" # 180 * 5s = 15min

# Set BASE_IMAGE_PATH to use a locally-downloaded Kairos installer ISO
# instead of fetching IMAGE_URL.
BASE_IMAGE_PATH="${BASE_IMAGE_PATH:-}"

# k0s version k0sctl installs on every node (no leading `v`, matching k0sctl's
# config convention).
K0S_VERSION="${K0S_VERSION:-1.35.5+k0s.0}"

# k0sctl's OS registry doesn't know Kairos (ID=kairos / ID=hadron) -- the `os:`
# host field overrides detection. Every Linux configurer in current k0sctl is
# the same generic systemd implementation with a different ID matcher, so
# `debian` is the most vanilla choice, not a claim about the actual OS.
K0SCTL_OS_OVERRIDE="${K0SCTL_OS_OVERRIDE:-debian}"

SSH_PUBKEY="${SSH_PUBKEY:-$HOME/.ssh/id_ed25519.pub}"
SSH_PRIVKEY="${SSH_PRIVKEY:-${SSH_PUBKEY%.pub}}"
SSH_USER="${SSH_USER:-${USERNAME:-${USER:-root}}}"

TAILSCALE_AUTHKEY="${TAILSCALE_AUTHKEY:-}"
TAILSCALE_VERSION="${TAILSCALE_VERSION:-1.98.10}"
TAILSCALE_API_KEY="${TAILSCALE_API_KEY:-}"
TAILSCALE_TAILNET="${TAILSCALE_TAILNET:--}"

# k0s taints controller+worker nodes control-plane:NoSchedule by default. The
# default topology reserves the ONLY worker for image builds, so the default
# here LIFTS the taint: everything else schedules on the controllers.
NO_TAINTS="${NO_TAINTS:-true}"

CLUSTER_NAME="${CLUSTER_NAME:-${VM_PREFIX}-cluster}"
# Space-separated role per node (libvirt backend), one entry per VM.
NODE_ROLES="${NODE_ROLES:-controller+worker controller+worker controller+worker worker}"

# The pure worker dedicated to image builds: labelled banlieue.io/imagebuild=true
# and tainted dedicated=imagebuild:NoSchedule. Empty means the LAST node whose
# role is exactly "worker"; set to a node name to pin a different one.
IMAGEBUILD_NODE="${IMAGEBUILD_NODE:-}"

# ============================================================================
# vSphere backend configuration (BACKEND=vsphere)
# ============================================================================
# NODES: the node table, one entry per node -- "<name> <cluster_id> <ip> <role>"
#   name       : vSphere VM name AND k0s node hostname (an FQDN works)
#   cluster_id : key into the per-cluster placement maps below
#   ip         : the node's STATIC IPv4 address
#   role       : controller+worker | worker
# Declared in BANLIEUE_ENV_FILE (a plain indexed array).
#
# Per-cluster placement is declared in BANLIEUE_ENV_FILE as FLAT variables named
# <PREFIX>_<cluster_id> and read via _cfg (macOS ships bash 3.2, which has no
# associative arrays -- so no `declare -A`):
#   VSPHERE_RP_<id>  = resource pool path         (govc -pool)
#   VSPHERE_DSC_<id> = SDRS datastore-cluster PATH (a member DS is auto-picked by free space)
#   VSPHERE_NET_<id> = DVS port group             (govc -net)
#   VSPHERE_TPL_<id> = cluster-specific template   (govc -vm, source of the clone)
#   VSPHERE_GW_<id>  = gateway (optional; default = first three octets of ip + .1)
# _cfg PREFIX ID -> value of ${PREFIX}_${ID} (empty if unset). ID is trusted
# (comes from NODES in the operator's own env file).
_cfg() { eval "printf '%s' \"\${${1}_${2}-}\""; }

# vCenter folder the new VMs are placed in (created if missing).
VSPHERE_FOLDER="${VSPHERE_FOLDER:-banlieue}"
# Static networking parameters shared by all nodes.
NET_PREFIX="${NET_PREFIX:-24}"
DNS_SERVERS="${DNS_SERVERS:-}"          # comma-separated, e.g. 192.0.2.53,198.51.100.53
DNS_DOMAIN="${DNS_DOMAIN:-}"            # primary search domain
DNS_SEARCH="${DNS_SEARCH:-}"            # extra space-separated search domains
# Stable name for the API server, baked into the cert SANs and used as the
# kubeconfig server address. Must resolve to a controller (or be in /etc/hosts).
API_SAN="${API_SAN:-}"

# --- vSphere k0s install (NATIVE, not k0sctl -- ADR-0017) ---
# On-prem the k0s binary is installed to /opt/k0s/<ver>-amd64 (a persistent
# bind mount in the estate's Kairos image) and symlinked to /usr/local/bin/k0s, then
# `k0s install controller|worker` + token joins bring the cluster up -- k0sctl
# has no way to place the binary under /opt/k0s. Every node downloads the binary
# itself from K0S_BINARY_BASEURL (set to an internal mirror in BANLIEUE_ENV_FILE
# for air-gapped estates; defaults to the public GitHub releases).
K0S_BINARY_BASEURL="${K0S_BINARY_BASEURL:-https://github.com/k0sproject/k0s/releases/download}"
# If set, becomes spec.images.repository (internal registry mirror) so nodes
# don't pull k0s system images from the public internet. NOTE: this alone does
# NOT repoint the CRI sandbox (pause) image -- containerd's sandbox_image is a
# separate setting, only rewritten via spec.images.pause below. Without that,
# nodes with no route to quay.io stay NotReady even with repository set.
K0S_IMAGE_REPOSITORY="${K0S_IMAGE_REPOSITORY:-}"
# Full override for the CRI sandbox (pause) image, e.g.
# quay-mirror.example.com/k0sproject/pause -- required in air-gapped estates
# even when K0S_IMAGE_REPOSITORY is set (see note above).
K0S_PAUSE_IMAGE="${K0S_PAUSE_IMAGE:-}"
K0S_PAUSE_VERSION="${K0S_PAUSE_VERSION:-}"
# CNI: kuberouter (k0s default) or calico.
K0S_NETWORK_PROVIDER="${K0S_NETWORK_PROVIDER:-kuberouter}"
# calico only: ipAutodetectionMethod can-reach=<addr> (defaults to first controller IP).
CALICO_REACH="${CALICO_REACH:-}"

# Disable the konnectivity server (default true on vSphere). On a flat, routable
# on-prem network the API server reaches kubelets directly (:10250) for
# logs/exec/port-forward, so the konnectivity tunnel is unnecessary -- and on a
# multi-controller cluster with no single externalAddress/VIP its agents pin to
# ONE controller, so kubectl hitting any other returns "No agent available"
# (k0s #600/#5503). Disabling it removes that failure mode entirely and matches
# the reference on-prem clusters. Set false only if the network is NOT flat.
K0S_DISABLE_KONNECTIVITY="${K0S_DISABLE_KONNECTIVITY:-true}"

# ----------------------------------------------------------------------------
# Vault + flux-operator (opt-in; ADR-0018)
# ----------------------------------------------------------------------------
# When true, the `flux` step (and `all`) fetches a registry credential from
# Vault and pushes flux-operator + flux-core manifests onto the cluster.
FLUX_ENABLED="${FLUX_ENABLED:-false}"

# The `vault` CLI reads VAULT_ADDR/VAULT_TOKEN from the ambient environment
# itself -- token auth only, no login/AppRole flow (matches the reference
# Ansible role this was modeled on: a mandatory pre-existing VAULT_TOKEN, no
# auth negotiation; see ADR-0018). Mount + secret path are entirely
# operator-supplied -- no default, since any value here would be a guess at an
# estate's naming convention.
VAULT_KV_MOUNT="${VAULT_KV_MOUNT:-}"
VAULT_SECRET_PATH="${VAULT_SECRET_PATH:-}"
VAULT_FLUX_USER_KEY="${VAULT_FLUX_USER_KEY:-flux}"
VAULT_FLUX_PASS_KEY="${VAULT_FLUX_PASS_KEY:-flux-pass}"

# Registry the flux-artifactory imagePullSecret authenticates against, and the
# OCIRepository flux-core pulls from. No default -- both name a real estate
# registry.
FLUX_REGISTRY="${FLUX_REGISTRY:-}"
FLUX_CORE_OCI_URL="${FLUX_CORE_OCI_URL:-}"

# flux-operator itself: public upstream by default, overridable to an internal
# mirror the same way K0S_BINARY_BASEURL above is.
FLUX_OPERATOR_VERSION="${FLUX_OPERATOR_VERSION:-v0.20.0}"
FLUX_OPERATOR_INSTALL_URL="${FLUX_OPERATOR_INSTALL_URL:-https://github.com/controlplaneio-fluxcd/flux-operator/releases/download}"
FLUX_OPERATOR_REGISTRY="${FLUX_OPERATOR_REGISTRY:-ghcr.io/controlplaneio-fluxcd}"
FLUX_DISTRIBUTION_VERSION="${FLUX_DISTRIBUTION_VERSION:-2.x}"

# Optional local file whose contents become the flux-tls CA secret; if unset,
# no flux-tls secret is created and OCIRepository.certSecretRef is omitted --
# there is no automated CA-bundle fetch (the reference's equivalent pulls from
# an org-internal HTTP API out of scope for a public tool; see ADR-0018).
FLUX_CA_BUNDLE_FILE="${FLUX_CA_BUNDLE_FILE:-}"

# Space-separated KEY=VALUE pairs merged into the flux-core Kustomization's
# postBuild.substitute, alongside the always-injected CLUSTER_DNS. Substitution
# keys are entirely operator-defined -- banlieue has no appcode/env convention
# of its own to default them from.
FLUX_SUBSTITUTIONS="${FLUX_SUBSTITUTIONS:-}"

WORKDIR="${WORKDIR:-$HOME/.local/share/k0s-bootstrap}"
POOL_DIR="${POOL_DIR:-/var/lib/libvirt/images/k0s-bootstrap}"
INSTALL_ISO="$POOL_DIR/$(basename "${BASE_IMAGE_PATH:-$IMAGE_URL}")"
K0SCTL_CONFIG="${K0SCTL_CONFIG:-$WORKDIR/k0sctl.yaml}"
KUBECONFIG_OUT="${KUBECONFIG_OUT:-$WORKDIR/kubeconfig}"
KUBECONFIG_SERVER_FILE="${KUBECONFIG_SERVER_FILE:-$WORKDIR/kubeconfig-server}"

if [[ "${1:-}" != "--print-env-template" ]]; then
  mkdir -p "$WORKDIR"
  [[ "$BACKEND" == "libvirt" ]] && mkdir -p "$POOL_DIR"
fi

log() { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

# On-prem vCenter/API/SSH endpoints are reached directly -- a corporate HTTP
# proxy in the environment would black-hole govc/kubectl/ssh. Unset every
# proxy variable before ANY on-prem call (idempotent; a no-op if none are
# set). Called unconditionally, regardless of BACKEND: both backends shell out
# to kubectl/ssh, and a remote libvirt host (LIBVIRT_URI=qemu+ssh://...) is
# just as on-prem as vCenter.
unset_proxy() {
  unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY 2>/dev/null || true
}
unset_proxy

# The k0s API server's TLS cert only covers addresses known at cluster-init
# time. Extra SANs (Tailscale IPs, a stable DNS name, every static node IP on
# the vSphere backend) are added to spec.api.sans below.
TAILSCALE_IP_MAP="${TAILSCALE_IP_MAP:-$WORKDIR/tailscale-ips.map}"
EXTRA_SANS="${EXTRA_SANS:-}"
API_EXTERNAL_ADDRESS="${API_EXTERNAL_ADDRESS:-}"
KUBECONFIG_SERVER="${KUBECONFIG_SERVER:-}"

# ============================================================================
# Node model (backend-agnostic)
# ============================================================================
# node_name_roles: prints "<name> <role>" per node, WITHOUT resolving IPs.
# Used by label_imagebuild_node (which needs names/roles but no addresses).
node_name_roles() {
  if [[ "$BACKEND" == "vsphere" ]]; then
    local entry
    for entry in "${NODES[@]}"; do
      [[ -n "$entry" ]] || continue
      # shellcheck disable=SC2086  # deliberate word split of "name id ip role"
      set -- $entry
      echo "$1 $4"
    done
    return
  fi
  local roles=($NODE_ROLES) idx
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    echo "$(vm_name "$idx") ${roles[$idx]:-worker}"
  done
}

# ============================================================================
# libvirt backend
# ============================================================================
check_deps_libvirt() {
  local missing=()
  for cmd in virt-install virsh qemu-img k0sctl kubectl curl ssh genisoimage; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  if [[ ! -f "$SSH_PUBKEY" ]]; then
    log "SSH public key not found at $SSH_PUBKEY (set SSH_PUBKEY=...)"
    missing+=("ssh keypair")
  fi
  if ((${#missing[@]})); then
    log "Missing dependencies: ${missing[*]}"
    exit 1
  fi
}

vm_name() { printf '%s-%02d' "$VM_PREFIX" "$(($1 + 1))"; }

fetch_installer_iso() {
  if [[ -f "$INSTALL_ISO" ]]; then
    log "Installer ISO already present at $INSTALL_ISO, skipping"
  elif [[ -n "$BASE_IMAGE_PATH" ]]; then
    [[ -f "$BASE_IMAGE_PATH" ]] || { log "BASE_IMAGE_PATH=$BASE_IMAGE_PATH not found"; exit 1; }
    log "Linking local installer ISO $BASE_IMAGE_PATH -> $INSTALL_ISO"
    ln -f "$BASE_IMAGE_PATH" "$INSTALL_ISO" 2>/dev/null || cp "$BASE_IMAGE_PATH" "$INSTALL_ISO"
  else
    log "Downloading Kairos installer ISO from $IMAGE_URL"
    curl -fL --output "$INSTALL_ISO.tmp" "$IMAGE_URL"
    mv "$INSTALL_ISO.tmp" "$INSTALL_ISO"
  fi
}

make_cloud_init_files() {
  local name="$1" seed_dir="$2"
  mkdir -p "$seed_dir"

  cat >"$seed_dir/meta-data" <<EOF
instance-id: $name
local-hostname: $name
EOF

  : >"$seed_dir/user-data"
  chmod 600 "$seed_dir/user-data" # may end up holding TAILSCALE_AUTHKEY below
  cat >>"$seed_dir/user-data" <<EOF
#cloud-config
hostname: $name
install:
  auto: true
  device: "auto"
EOF

  if [[ "$SSH_USER" == "root" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
users:
  - name: root
    ssh_authorized_keys:
      - $(cat "$SSH_PUBKEY")
EOF
  else
    cat >>"$seed_dir/user-data" <<EOF
users:
  - name: $SSH_USER
    groups: [admin]
    ssh_authorized_keys:
      - $(cat "$SSH_PUBKEY")
EOF
  fi

  cat >>"$seed_dir/user-data" <<EOF
stages:
  after-install:
    - name: "Power off immediately"
      commands:
        - poweroff -f
  boot:
    - name: "Provide loop devices for disk-image builds"
      files:
        - path: /etc/modules-load.d/banlieue-loop.conf
          permissions: 0644
          content: |
            loop
        - path: /etc/modprobe.d/banlieue-loop.conf
          permissions: 0644
          content: |
            options loop max_loop=8
      commands:
        - modprobe loop max_loop=8 || modprobe loop
        - for i in 0 1 2 3 4 5 6 7; do [ -e /dev/loop\$i ] || mknod -m 660 /dev/loop\$i b 7 \$i; done
EOF

  if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
  network:
    - name: "Install and join Tailscale"
      commands:
        - "[ -x /usr/local/bin/tailscaled ] || ( curl -fsSL -o /tmp/ts.tgz https://pkgs.tailscale.com/stable/tailscale_${TAILSCALE_VERSION}_amd64.tgz && tar xzf /tmp/ts.tgz -C /tmp && cp /tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscale /tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscaled /usr/local/bin/ && rm -rf /tmp/ts.tgz /tmp/tailscale_${TAILSCALE_VERSION}_amd64 )"
        - systemctl daemon-reload
        - systemctl enable --now tailscaled
        - "tailscale status >/dev/null 2>&1 || tailscale up --authkey=$TAILSCALE_AUTHKEY --hostname=$name --ssh"
write_files:
  - path: /etc/systemd/system/tailscaled.service
    permissions: "0644"
    content: |
      [Unit]
      Description=Tailscale node agent
      Wants=network-online.target
      After=network-online.target
      [Service]
      ExecStart=/usr/local/bin/tailscaled --state=/var/lib/tailscale/tailscaled.state --socket=/run/tailscale/tailscaled.sock --port=41641
      ExecStopPost=/usr/local/bin/tailscaled --cleanup
      Restart=on-failure
      [Install]
      WantedBy=multi-user.target
EOF
  fi

  genisoimage -output "$seed_dir.iso" -volid cidata -joliet -rock \
    "$seed_dir/user-data" "$seed_dir/meta-data" >/dev/null
}

wait_for_install() {
  local name="$1"
  log "Waiting for $name to install Kairos and power off..."
  for _ in $(seq 1 "$INSTALL_WAIT_ATTEMPTS"); do
    if [[ "$(virsh --connect "$LIBVIRT_URI" domstate "$name" 2>/dev/null)" == "shut off" ]]; then
      return 0
    fi
    sleep 5
  done
  log "Timed out waiting for $name to power off after install"
  exit 1
}

create_vm() {
  local idx="$1" name
  name="$(vm_name "$idx")"

  if virsh --connect "$LIBVIRT_URI" dominfo "$name" >/dev/null 2>&1; then
    log "VM $name already defined, ensuring it is running"
    virsh --connect "$LIBVIRT_URI" start "$name" >/dev/null 2>&1 || true
    return
  fi

  local disk="$POOL_DIR/$name.qcow2"
  local seed_dir="$POOL_DIR/$name-seed"
  local seed_iso="$POOL_DIR/$name-seed.iso"

  log "Creating empty disk for $name (${DISK_GB}G -- Kairos installs onto it from the ISO)"
  qemu-img create -f qcow2 "$disk" "${DISK_GB}G" >/dev/null

  log "Building cloud-init seed for $name"
  make_cloud_init_files "$name" "$seed_dir"

  log "Defining and starting VM $name (${VCPUS} vCPU, ${MEM_MB}MB RAM)"
  virt-install \
    --connect "$LIBVIRT_URI" \
    --name "$name" \
    --memory "$MEM_MB" \
    --vcpus "$VCPUS" \
    --disk "path=$disk,format=qcow2,bus=virtio,boot_order=2" \
    --disk "path=$INSTALL_ISO,device=cdrom,bus=sata,boot_order=1" \
    --disk "path=$seed_iso,device=cdrom,bus=sata" \
    --os-variant "$OS_VARIANT" \
    --network "network=$LIBVIRT_NETWORK,model=virtio" \
    --boot uefi \
    --graphics none \
    --console pty,target_type=serial \
    --noautoconsole

  wait_for_install "$name"

  log "Ejecting the installer ISO from $name and booting the installed system"
  virsh --connect "$LIBVIRT_URI" change-media "$name" "$INSTALL_ISO" --eject --config --force
  virsh --connect "$LIBVIRT_URI" autostart "$name"
  virsh --connect "$LIBVIRT_URI" start "$name"
}

create_vms_libvirt() {
  local idx pids=()
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    create_vm "$idx" &
    pids+=($!)
  done
  local rc=0 pid
  for pid in "${pids[@]}"; do
    wait "$pid" || rc=1
  done
  return "$rc"
}

vm_ip_libvirt() {
  local name="$1"
  virsh --connect "$LIBVIRT_URI" domifaddr "$name" --source lease 2>/dev/null \
    | awk '/ipv4/ {print $4}' | cut -d/ -f1 | head -n1
}

wait_for_ip_libvirt() {
  local name="$1" ip=""
  log "Waiting for $name to get a DHCP lease..."
  for _ in $(seq 1 "$IP_WAIT_ATTEMPTS"); do
    ip="$(vm_ip_libvirt "$name")"
    [[ -n "$ip" ]] && { echo "$ip"; return 0; }
    sleep 5
  done
  log "Timed out waiting for an IP address for $name"
  exit 1
}

destroy_libvirt() {
  local failed=0
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    local name
    name="$(vm_name "$idx")"
    if virsh --connect "$LIBVIRT_URI" dominfo "$name" >/dev/null 2>&1; then
      log "Destroying VM $name"
      virsh --connect "$LIBVIRT_URI" destroy "$name" >/dev/null 2>&1 || true
      if ! virsh --connect "$LIBVIRT_URI" undefine "$name" --nvram --managed-save; then
        log "FAILED to undefine $name -- it will still appear in virsh/Cockpit"
        failed=1
      fi
    fi
    rm -f "$POOL_DIR/$name.qcow2" "$POOL_DIR/$name-seed.iso"
    rm -rf "$POOL_DIR/$name-seed"
  done
  rm -f "$K0SCTL_CONFIG" "$KUBECONFIG_OUT" "$TAILSCALE_IP_MAP" "$KUBECONFIG_SERVER_FILE"
  tailscale_remove_devices
  local leftover
  leftover="$(virsh --connect "$LIBVIRT_URI" list --all --name 2>/dev/null \
    | grep -E "^${VM_PREFIX}-[0-9]+\$" || true)"
  if [[ -n "$leftover" ]]; then
    log "Domains still defined after destroy: $(echo "$leftover" | tr '\n' ' ')"
    exit 1
  fi
  [[ "$failed" -ne 0 ]] && exit 1
  log "All $VM_PREFIX-* domains removed"
}

# ============================================================================
# vSphere backend
# ============================================================================
check_deps_vsphere() {
  local missing=()
  for cmd in govc jq kubectl ssh base64; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  if [[ ! -f "$SSH_PUBKEY" ]]; then
    log "SSH public key not found at $SSH_PUBKEY (set SSH_PUBKEY=...)"
    missing+=("ssh keypair")
  fi
  if ((${#missing[@]})); then
    log "Missing dependencies: ${missing[*]}"
    exit 1
  fi
  [[ -n "${GOVC_URL:-}" ]] || { log "GOVC_URL is not set (source your vCenter env)"; exit 1; }
  [[ -n "${NODES:-}" ]] || { log "NODES is empty -- set it in BANLIEUE_ENV_FILE"; exit 1; }
  [[ -n "$DNS_SERVERS" ]] || { log "DNS_SERVERS is empty -- set it in BANLIEUE_ENV_FILE"; exit 1; }
  log "vCenter: $GOVC_URL  datacenter: ${GOVC_DATACENTER:-<default>}"
}

# Split a NODES entry "name id ip role" into the globals _N_NAME/_N_ID/_N_IP/_N_ROLE.
_parse_node() {
  # shellcheck disable=SC2086  # deliberate word split
  set -- $1
  _N_NAME="$1"; _N_ID="$2"; _N_IP="$3"; _N_ROLE="${4:-worker}"
}

# Gateway for a node: explicit per-cluster override, else first three octets + .1.
node_gateway() {
  local id="$1" ip="$2" gw
  gw="$(_cfg VSPHERE_GW "$id")"
  [[ -n "$gw" ]] && { echo "$gw"; return; }
  echo "${ip%.*}.1"
}

# Pick the member datastore with the most free space inside an SDRS datastore
# cluster. VSPHERE_DSC holds the datastore-cluster PATH; its members are
# enumerated with `govc find` and ranked by summary.freeSpace. Prints the leaf
# datastore name (accepted by `govc ... -ds`).
pick_datastore() {
  local dsc="$1" best="" bestfree=-1 ds free
  while read -r ds; do
    [[ -n "$ds" ]] || continue
    free="$(govc datastore.info -json "$ds" 2>/dev/null | jq -r '.datastores[0].summary.freeSpace // 0')"
    [[ "$free" =~ ^[0-9]+$ ]] || free=0
    if (( free > bestfree )); then bestfree="$free"; best="$ds"; fi
  done < <(govc find "$dsc" -type s 2>/dev/null)
  [[ -n "$best" ]] && basename "$best"
}

# Resolve and print the placement for a cluster id: "<template>|<datastore>|<rp>|<net>".
# Every value is taken from the per-cluster maps in BANLIEUE_ENV_FILE; the only
# runtime discovery is choosing a concrete datastore inside the SDRS cluster.
resolve_placement() {
  local id="$1" tpl ds rp net dsc
  tpl="$(_cfg VSPHERE_TPL "$id")"; rp="$(_cfg VSPHERE_RP "$id")"
  net="$(_cfg VSPHERE_NET "$id")"; dsc="$(_cfg VSPHERE_DSC "$id")"
  [[ -n "$tpl" && -n "$rp" && -n "$net" && -n "$dsc" ]] || {
    warn "cluster '$id': VSPHERE_TPL/RP/NET/DSC_$id not all set in BANLIEUE_ENV_FILE"; return 1; }
  ds="$(pick_datastore "$dsc")"
  [[ -n "$ds" ]] || { warn "cluster '$id': could not pick a datastore from '$dsc'"; return 1; }
  echo "$tpl|$ds|$rp|$net"
}

# Render a Kairos cloud-config for one node (static networking via
# systemd-networkd; the template's baked installer consumes `install:`).
make_vsphere_cloud_config() {
  local fqdn="$1" ip="$2" gw="$3" prefix="$4" dns_csv="$5" domain="$6"
  local shortname="${fqdn%%.*}" pubkey dns_lines search
  pubkey="$(cat "$SSH_PUBKEY")"
  dns_lines=""
  local d; local IFS=','
  for d in $dns_csv; do dns_lines+="            DNS=$d"$'\n'; done
  unset IFS
  search="$domain $DNS_SEARCH"
  cat <<EOF
#cloud-config
install:
  auto: true
  device: /dev/sda
  reboot: true
hostname: $fqdn
fqdn: $fqdn
users:
  - name: root
    ssh_authorized_keys:
      - $pubkey
  - name: kairos
    groups: [admin]
    ssh_authorized_keys:
      - $pubkey
stages:
  after-install-chroot:
    - name: "Set root SSH authorized keys"
      files:
        - path: /root/.ssh/authorized_keys
          permissions: 0600
          owner: 0
          group: 0
          content: |
            $pubkey
  initramfs:
    - name: "Static IP via systemd-networkd"
      commands:
        - systemctl mask NetworkManager || true
        - systemctl mask systemd-networkd-wait-online.service || true
      files:
        - path: /etc/systemd/network/10-static.network
          permissions: 0644
          owner: 0
          content: |
            [Match]
            Name=ens* en*

            [Network]
            DHCP=no
            Address=$ip/$prefix
            Gateway=$gw
$dns_lines            Domains=$search
        - path: /etc/hostname
          permissions: 0644
          content: |
            $fqdn
        - path: /etc/hosts
          permissions: 0644
          content: |
            127.0.0.1 localhost $fqdn $shortname
            ::1       localhost
            $ip $fqdn $shortname
    - name: "Enable networkd + resolved"
      commands:
        - systemctl enable systemd-networkd || true
        - systemctl enable systemd-resolved || true
        - hostnamectl set-hostname $fqdn || true
  boot:
    - name: "Eject install media"
      commands:
        - eject /dev/sr0 || true
    - name: "Provide loop devices for disk-image builds"
      files:
        - path: /etc/modules-load.d/banlieue-loop.conf
          permissions: 0644
          content: |
            loop
        - path: /etc/modprobe.d/banlieue-loop.conf
          permissions: 0644
          content: |
            options loop max_loop=8
      commands:
        - modprobe loop max_loop=8 || modprobe loop
        - for i in 0 1 2 3 4 5 6 7; do [ -e /dev/loop\$i ] || mknod -m 660 /dev/loop\$i b 7 \$i; done
EOF
}

# Ensure the target VM folder exists (idempotent).
ensure_vsphere_folder() {
  govc folder.info "$VSPHERE_FOLDER" >/dev/null 2>&1 && return 0
  log "Creating vCenter folder $VSPHERE_FOLDER"
  govc folder.create "$VSPHERE_FOLDER" >/dev/null 2>&1 || true
}

create_vm_vsphere() {
  local entry="$1"
  _parse_node "$entry"
  local name="$_N_NAME" id="$_N_ID" ip="$_N_IP"

  if govc vm.info "$name" >/dev/null 2>&1 && [[ -n "$(govc vm.info -json "$name" 2>/dev/null | jq -r '.virtualMachines[0].name // empty')" ]]; then
    log "VM $name already exists, ensuring it is powered on"
    govc vm.power -on "$name" >/dev/null 2>&1 || true
    return 0
  fi

  local placement tpl ds rp net gw prefix
  placement="$(resolve_placement "$id")" || return 1
  IFS='|' read -r tpl ds rp net <<<"$placement"
  gw="$(node_gateway "$id" "$ip")"; prefix="$NET_PREFIX"

  log "[$name] cloning from $(basename "$tpl") (cluster $id) ds=$ds"
  govc vm.clone -vm="$tpl" -ds="$ds" -folder="$VSPHERE_FOLDER" -pool="$rp" -net="$net" -on=false "$name" >/dev/null

  govc vm.change -vm="$name" -annotation="$tpl" >/dev/null 2>&1 || true

  # Connect the CD-ROM (installer ISO baked into the template) and set boot order.
  local cdrom
  cdrom="$(govc device.info -vm="$name" -json 'cdrom-*' 2>/dev/null | jq -r '.devices[0].name // empty')"
  [[ -n "$cdrom" ]] && govc device.connect -vm="$name" "$cdrom" >/dev/null 2>&1 || true
  govc vm.change -vm="$name" -e "bios.bootOrder=cdrom,hdd" >/dev/null 2>&1 || true

  # Pin the NIC to PCI slot 192 so the guest names it ens192 (matches the
  # systemd-networkd [Match] above).
  local nic
  nic="$(govc device.info -vm="$name" -json 'ethernet-*' 2>/dev/null | jq -r '.devices[0].name // empty')"
  [[ -n "$nic" ]] && govc device.remove -vm="$name" "$nic" >/dev/null 2>&1 || true
  govc vm.network.add -vm="$name" -net="$net" -net.adapter=vmxnet3 >/dev/null
  govc vm.change -vm="$name" -e "ethernet0.pciSlotNumber=192" >/dev/null

  # Static networking hints for the Kairos vmware datasource (belt and braces
  # with the cloud-config above).
  govc vm.change -vm="$name" \
    -e "guestinfo.network.ip=$ip" \
    -e "guestinfo.network.prefix=$prefix" \
    -e "guestinfo.network.gateway=$gw" \
    -e "guestinfo.network.dns=$DNS_SERVERS" \
    -e "guestinfo.network.domain=${DNS_DOMAIN:-local}" >/dev/null

  # cloud-init via guestinfo.userdata (base64, single line -- macOS base64 wraps).
  local cc b64
  cc="$(make_vsphere_cloud_config "$name" "$ip" "$gw" "$prefix" "$DNS_SERVERS" "${DNS_DOMAIN:-local}")"
  b64="$(printf '%s' "$cc" | base64 | tr -d '\n')"
  govc vm.change -vm="$name" -e "guestinfo.userdata=$b64" -e "guestinfo.userdata.encoding=base64" >/dev/null

  # CPU / memory.
  govc vm.change -vm="$name" -c "$VCPUS" -m "$MEM_MB" >/dev/null

  # Grow the root disk to DISK_GB if the template's disk is smaller.
  local disk cur_kb target_mb
  disk="$(govc device.info -vm="$name" -json 'disk-*' 2>/dev/null | jq -r '.devices[0].name // empty')"
  cur_kb="$(govc device.info -vm="$name" -json 'disk-*' 2>/dev/null | jq -r '.devices[0].capacityInKB // 0')"
  target_mb=$((DISK_GB * 1024))
  if [[ -n "$disk" ]] && (( target_mb * 1024 > cur_kb )); then
    log "[$name] resizing root disk to ${DISK_GB}G"
    govc vm.disk.change -vm="$name" -disk.name "$disk" -size "${target_mb}M" >/dev/null
  fi

  log "[$name] powering on"
  govc vm.power -on "$name" >/dev/null
}

create_vms_vsphere() {
  ensure_vsphere_folder
  local entry pids=()
  for entry in "${NODES[@]}"; do
    [[ -n "$entry" ]] || continue
    create_vm_vsphere "$entry" &
    pids+=($!)
  done
  local rc=0 pid
  for pid in "${pids[@]}"; do
    wait "$pid" || rc=1
  done
  return "$rc"
}

# Kairos boots the template, installs to /dev/sda, reboots into the immutable
# image (root on /dev/loop0), ~8-12 min. Readiness = SSH answers AND root is
# loop0 (still on LiveOS_rootfs means the install is still running/failed).
wait_for_install_vsphere() {
  local ip="$1" src=""
  log "Waiting for Kairos install to finish on $ip (root fs -> /dev/loop0)..."
  for _ in $(seq 1 "$INSTALL_WAIT_ATTEMPTS"); do
    src="$(ssh_run "$ip" "findmnt -n -o SOURCE / 2>/dev/null" 2>/dev/null | tr -d '\r' || true)"
    [[ "$src" == "/dev/loop0" ]] && return 0
    sleep 5
  done
  log "Timed out waiting for Kairos install on $ip (last root source: '${src:-none}')"
  return 1
}

destroy_vsphere() {
  local entry name
  for entry in "${NODES[@]}"; do
    [[ -n "$entry" ]] || continue
    _parse_node "$entry"; name="$_N_NAME"
    if govc vm.info "$name" >/dev/null 2>&1; then
      log "Destroying VM $name"
      govc vm.power -off "$name" >/dev/null 2>&1 || true
      govc vm.destroy "$name" >/dev/null 2>&1 || warn "failed to destroy $name"
    fi
  done
  rm -f "$K0SCTL_CONFIG" "$KUBECONFIG_OUT" "$KUBECONFIG_SERVER_FILE"
  log "vSphere teardown complete"
}

# ----------------------------------------------------------------------------
# vSphere: NATIVE k0s install (mirrors the on-prem forge layout; ADR-0017)
# ----------------------------------------------------------------------------
# Normalise K0S_VERSION to a leading-v tag (URL + on-disk binary name use it).
_k0s_ver_tag() { local v="${K0S_VERSION#v}"; echo "v${v}"; }

# First controller: prints "<name> <ip>" (first NODES entry whose role is controller*).
first_controller() {
  local entry
  for entry in "${NODES[@]}"; do
    [[ -n "$entry" ]] || continue
    _parse_node "$entry"
    [[ "$_N_ROLE" == controller* ]] && { echo "$_N_NAME $_N_IP"; return 0; }
  done
  return 1
}

# Build /etc/k0s/k0s.yaml. SANs cover API_SAN + every node FQDN + every node IP.
render_k0s_yaml() {
  local cp_ip="$1" entry
  echo "apiVersion: k0s.k0sproject.io/v1beta1"
  echo "kind: ClusterConfig"
  echo "metadata:"
  echo "  name: k0s"
  echo "spec:"
  echo "  api:"
  echo "    sans:"
  [[ -n "$API_SAN" ]] && echo "      - $API_SAN"
  for entry in "${NODES[@]}"; do
    [[ -n "$entry" ]] || continue
    _parse_node "$entry"
    echo "      - $_N_NAME"
    echo "      - $_N_IP"
  done
  if [[ -n "$K0S_IMAGE_REPOSITORY" || -n "$K0S_PAUSE_IMAGE" ]]; then
    echo "  images:"
    [[ -n "$K0S_IMAGE_REPOSITORY" ]] && echo "    repository: $K0S_IMAGE_REPOSITORY"
    if [[ -n "$K0S_PAUSE_IMAGE" ]]; then
      echo "    pause:"
      echo "      image: $K0S_PAUSE_IMAGE"
      [[ -n "$K0S_PAUSE_VERSION" ]] && echo "      version: $K0S_PAUSE_VERSION"
    fi
  fi
  echo "  network:"
  echo "    provider: $K0S_NETWORK_PROVIDER"
  if [[ "$K0S_NETWORK_PROVIDER" == "calico" ]]; then
    echo "    calico:"
    echo "      ipAutodetectionMethod: \"can-reach=${CALICO_REACH:-$cp_ip}\""
    echo "      envVars:"
    echo "        FELIX_IGNORELOOSERPF: \"true\""
  fi
  echo "  telemetry:"
  echo "    enabled: false"
}

# Download + verify + symlink the k0s binary on a node (idempotent).
stage_k0s_binary() {
  local ip="$1" tag bn
  tag="$(_k0s_ver_tag)"; bn="k0s-${tag}-amd64"
  log "[$ip] staging k0s $tag into /opt/k0s (symlink /usr/local/bin/k0s)"
  ssh_run "$ip" 'sudo bash -s' <<EOF
set -e
mkdir -p /opt/k0s
bn="$bn"; base="$K0S_BINARY_BASEURL"; tag="$tag"
if [ ! -x "/opt/k0s/\$bn" ]; then
  curl -fsSL -o "/opt/k0s/\$bn" "\$base/\$tag/\$bn"
  if curl -fsSL -o /opt/k0s/sha256sums.txt "\$base/\$tag/sha256sums.txt"; then
    want=\$(grep " \$bn\$" /opt/k0s/sha256sums.txt | awk '{print \$1}')
    got=\$(sha256sum "/opt/k0s/\$bn" | awk '{print \$1}')
    if [ -n "\$want" ] && [ "\$want" != "\$got" ]; then echo "checksum mismatch for \$bn"; exit 1; fi
  fi
  chmod 0755 "/opt/k0s/\$bn"
fi
# Kairos' root filesystem is a read-only squashfs overlay — /usr/local/bin is
# not guaranteed writable even as root, so the symlink is best-effort only.
# /opt/k0s/\$bn (used directly by every later step) is the source of truth.
ln -sf "/opt/k0s/\$bn" /usr/local/bin/k0s 2>/dev/null || echo "note: /usr/local/bin not writable, using /opt/k0s/\$bn directly"
"/opt/k0s/\$bn" version
EOF
}

# Write the shared cluster config onto a controller node. Idempotent: only
# rewrites the file and returns 0 ("changed") when content actually differs
# from what's already there; returns 1 ("unchanged") otherwise, so callers
# can decide whether a running k0scontroller needs restarting to pick it up.
push_k0s_yaml() {
  local ip="$1" cp_ip="$2" content new_hash old_hash
  content="$(render_k0s_yaml "$cp_ip")"
  new_hash="$(printf '%s\n' "$content" | sha256sum | awk '{print $1}')"
  old_hash="$(ssh_run "$ip" 'sudo sha256sum /etc/k0s/k0s.yaml 2>/dev/null' | awk '{print $1}')"
  printf '%s\n' "$content" | ssh_run "$ip" 'sudo mkdir -p /etc/k0s && sudo tee /etc/k0s/k0s.yaml >/dev/null'
  [[ "$new_hash" != "$old_hash" ]]
}

# Wait until a controller's kube-api answers and reports the node Ready.
wait_k0s_api() {
  local ip="$1" k0s_bin
  k0s_bin="/opt/k0s/k0s-$(_k0s_ver_tag)-amd64"
  log "[$ip] waiting for kube-api..."
  for _ in $(seq 1 60); do
    ssh_run "$ip" "sudo $k0s_bin kubectl get --raw=/readyz" >/dev/null 2>&1 && return 0
    sleep 5
  done
  log "[$ip] kube-api never became ready"; return 1
}

# config step (vsphere): ensure nodes are up, stage the binary on all, write
# the cluster config on every controller.
vsphere_config() {
  populate_node_table
  local cp cp_ip row name role ip
  cp="$(first_controller)" || { log "no controller in NODES"; exit 1; }
  cp_ip="${cp#* }"
  for row in "${NODE_TABLE[@]}"; do
    IFS='|' read -r name role ip <<<"$row"
    stage_k0s_binary "$ip"
    [[ "$role" == controller* ]] && { push_k0s_yaml "$ip" "$cp_ip" || true; }
  done
  echo "${KUBECONFIG_SERVER:-${API_SAN:-$cp_ip}}" >"$KUBECONFIG_SERVER_FILE"
  log "vSphere prepare complete (binaries staged, controller configs written)"
}

# apply step (vsphere): init the first controller, then join the rest.
vsphere_apply() {
  populate_node_table
  local cp cp_name cp_ip row name role ip k0s_bin
  cp="$(first_controller)" || { log "no controller in NODES"; exit 1; }
  cp_name="${cp% *}"; cp_ip="${cp#* }"
  k0s_bin="/opt/k0s/k0s-$(_k0s_ver_tag)-amd64"

  # Disable konnectivity on flat routable networks (default) -- see
  # K0S_DISABLE_KONNECTIVITY. Avoids the multi-controller "No agent available"
  # trap; the API server reaches kubelets directly.
  local konny=""
  [[ "$K0S_DISABLE_KONNECTIVITY" == "true" ]] && konny="--disable-components=konnectivity-server"

  # First controller.
  if ssh_run "$cp_ip" 'systemctl is-active --quiet k0scontroller' 2>/dev/null; then
    if push_k0s_yaml "$cp_ip" "$cp_ip"; then
      log "[$cp_name] config changed, restarting k0scontroller"
      ssh_run "$cp_ip" 'sudo systemctl restart k0scontroller'
    else
      log "[$cp_name] k0scontroller already active, config unchanged, skipping"
    fi
  else
    log "[$cp_name/$cp_ip] installing FIRST controller"
    ssh_run "$cp_ip" "sudo $k0s_bin install controller --force --enable-worker --no-taints -c /etc/k0s/k0s.yaml $konny && sudo $k0s_bin start"
  fi
  wait_k0s_api "$cp_ip"

  # Remaining nodes.
  for row in "${NODE_TABLE[@]}"; do
    IFS='|' read -r name role ip <<<"$row"
    [[ "$ip" == "$cp_ip" ]] && continue
    if [[ "$role" == controller* ]]; then
      if ssh_run "$ip" 'systemctl is-active --quiet k0scontroller' 2>/dev/null; then
        if push_k0s_yaml "$ip" "$cp_ip"; then
          log "[$name] config changed, restarting k0scontroller"
          ssh_run "$ip" 'sudo systemctl restart k0scontroller'
          wait_k0s_api "$ip"
        else
          log "[$name] controller already active, config unchanged, skipping join"
        fi
        continue
      fi
      log "[$name/$ip] joining as controller"
      push_k0s_yaml "$ip" "$cp_ip" || true
      ssh_run "$cp_ip" "sudo $k0s_bin token create --role=controller --expiry=15m" | tr -d '\r' \
        | ssh_run "$ip" 'sudo tee /etc/k0s/token-file >/dev/null && sudo chmod 600 /etc/k0s/token-file'
      ssh_run "$ip" "sudo $k0s_bin install controller --force --enable-worker --no-taints --token-file /etc/k0s/token-file -c /etc/k0s/k0s.yaml $konny && sudo $k0s_bin start"
      wait_k0s_api "$ip"
    else
      if ssh_run "$ip" 'systemctl is-active --quiet k0sworker' 2>/dev/null; then
        log "[$name] worker already active, skipping join"; continue
      fi
      log "[$name/$ip] joining as worker"
      ssh_run "$cp_ip" "sudo $k0s_bin token create --role=worker --expiry=15m" | tr -d '\r' \
        | ssh_run "$ip" 'sudo tee /etc/k0s/worker-token-file >/dev/null && sudo chmod 600 /etc/k0s/worker-token-file'
      ssh_run "$ip" "sudo $k0s_bin install worker --token-file /etc/k0s/worker-token-file && sudo $k0s_bin start"
    fi
  done
  log "vSphere k0s install complete"
}

# kubeconfig step (vsphere): pull admin kubeconfig from the first controller,
# repoint server: at API_SAN (or the controller IP).
vsphere_kubeconfig() {
  local cp cp_ip target k0s_bin
  cp="$(first_controller)" || { log "no controller in NODES"; exit 1; }
  cp_ip="${cp#* }"
  k0s_bin="/opt/k0s/k0s-$(_k0s_ver_tag)-amd64"
  log "Fetching admin kubeconfig from $cp_ip"
  ssh_run "$cp_ip" "sudo $k0s_bin kubeconfig admin" >"$KUBECONFIG_OUT"
  target="${API_SAN:-$cp_ip}"
  [[ -s "$KUBECONFIG_SERVER_FILE" ]] && target="$(<"$KUBECONFIG_SERVER_FILE")"
  log "Pointing kubeconfig server at $target"
  sed -i.bak -E "s#^([[:space:]]*server: https://).*:([0-9]+)[[:space:]]*\$#\1${target}:\2#" "$KUBECONFIG_OUT"
  rm -f "$KUBECONFIG_OUT.bak"
  grep -E '^\s*server:' "$KUBECONFIG_OUT" >&2 || true
  log "export KUBECONFIG=$KUBECONFIG_OUT"
}

# ============================================================================
# Backend dispatch
# ============================================================================
check_deps()     { if [[ "$BACKEND" == "vsphere" ]]; then check_deps_vsphere;  else check_deps_libvirt;  fi; check_deps_flux; }
create_vms()     { if [[ "$BACKEND" == "vsphere" ]]; then create_vms_vsphere;  else create_vms_libvirt;  fi; }
destroy_all()    { if [[ "$BACKEND" == "vsphere" ]]; then destroy_vsphere;     else destroy_libvirt;     fi; }
# k0s install half: vSphere installs natively; libvirt keeps k0sctl.
k0s_config()     { if [[ "$BACKEND" == "vsphere" ]]; then vsphere_config;      else generate_k0sctl_config; fi; }
k0s_apply()      { if [[ "$BACKEND" == "vsphere" ]]; then vsphere_apply;       else apply_k0sctl;           fi; }
k0s_kubeconfig() { if [[ "$BACKEND" == "vsphere" ]]; then vsphere_kubeconfig;  else fetch_kubeconfig;        fi; }

# ============================================================================
# Common: SSH, k0sctl, kubeconfig, labelling (backend-agnostic)
# ============================================================================
ssh_run() {
  local ip="$1"; shift
  ssh -i "$SSH_PRIVKEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o LogLevel=ERROR \
    -o BatchMode=yes "$SSH_USER@$ip" "$@"
}

wait_for_ssh() {
  local ip="$1"
  log "Waiting for SSH on $ip..."
  for _ in $(seq 1 "$SSH_WAIT_ATTEMPTS"); do
    ssh_run "$ip" true 2>/dev/null && return 0
    sleep 5
  done
  log "Timed out waiting for SSH on $ip"
  exit 1
}

wait_for_tailscale_ip() {
  local ip="$1" ts_ip=""
  log "Waiting for Tailscale IP on $ip..."
  for _ in $(seq 1 "$TAILSCALE_WAIT_ATTEMPTS"); do
    ts_ip="$(ssh_run "$ip" "sudo tailscale ip -4 2>/dev/null" 2>/dev/null | tr -d '\r')"
    [[ -n "$ts_ip" ]] && { echo "$ts_ip"; return 0; }
    sleep 5
  done
  log "Timed out waiting for a Tailscale IP on $ip -- continuing without it"
  return 1
}

# Build the node table the k0sctl config is generated from. Each row is
# "name|role|ip". This is where a backend resolves node addresses and waits for
# the nodes to become reachable.
#   libvirt: name from VM_COUNT, role from NODE_ROLES, ip from DHCP lease.
#   vsphere: name/role/ip straight from NODES; wait for the install to finish.
NODE_TABLE=()
populate_node_table() {
  NODE_TABLE=()
  if [[ "$BACKEND" == "vsphere" ]]; then
    local entry
    for entry in "${NODES[@]}"; do
      [[ -n "$entry" ]] || continue
      _parse_node "$entry"
      wait_for_ssh "$_N_IP"
      wait_for_install_vsphere "$_N_IP" || { log "node $_N_NAME never reached /dev/loop0"; exit 1; }
      NODE_TABLE+=("$_N_NAME|$_N_ROLE|$_N_IP")
    done
    return
  fi
  local roles=($NODE_ROLES) idx name ip
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    name="$(vm_name "$idx")"
    ip="$(wait_for_ip_libvirt "$name")"
    wait_for_ssh "$ip"
    NODE_TABLE+=("$name|${roles[$idx]:-worker}|$ip")
  done
}

generate_k0sctl_config() {
  populate_node_table

  local hosts="" sans="" cp_ip="" cp_ts_ip=""
  for extra_san in $EXTRA_SANS; do
    sans+="            - $extra_san"$'\n'
  done
  [[ -n "$API_SAN" ]] && sans+="            - $API_SAN"$'\n'
  : >"$TAILSCALE_IP_MAP"

  local row name role ip ts_ip
  for row in "${NODE_TABLE[@]}"; do
    IFS='|' read -r name role ip <<<"$row"
    ts_ip=""
    if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
      ts_ip="$(wait_for_tailscale_ip "$ip" || true)"
      if [[ -n "$ts_ip" ]]; then
        echo "$ip $ts_ip" >>"$TAILSCALE_IP_MAP"
        sans+="            - $ts_ip"$'\n'
      fi
    fi
    # On vSphere every node has a routable static IP; add each as a SAN so
    # kubectl can land on any controller.
    [[ "$BACKEND" == "vsphere" ]] && sans+="            - $ip"$'\n'

    if [[ -z "$cp_ip" && "$role" == controller* ]]; then
      cp_ip="$ip"; cp_ts_ip="$ts_ip"
    fi
    hosts+="  - role: $role"$'\n'
    hosts+="    uploadBinary: true"$'\n'
    hosts+="    os: $K0SCTL_OS_OVERRIDE"$'\n'
    if [[ "$role" == "controller+worker" && "$NO_TAINTS" == "true" ]]; then
      hosts+="    noTaints: true"$'\n'
    fi
    hosts+="    ssh:"$'\n'
    hosts+="      address: $ip"$'\n'
    hosts+="      user: $SSH_USER"$'\n'
    hosts+="      port: 22"$'\n'
    hosts+="      keyPath: $SSH_PRIVKEY"$'\n'
  done

  local api_external="${API_EXTERNAL_ADDRESS:-$cp_ip}"
  if [[ -z "$api_external" ]]; then
    log "No controller found in the node table and no API_EXTERNAL_ADDRESS set"
    exit 1
  fi
  case $'\n'"$sans" in
    *$'\n'"            - $api_external"$'\n'*) ;;
    *) sans+="            - $api_external"$'\n' ;;
  esac

  # kubeconfig server preference: explicit override -> API_SAN -> control-plane
  # node's Tailscale IP -> its internal/static address.
  echo "${KUBECONFIG_SERVER:-${API_SAN:-${cp_ts_ip:-$api_external}}}" >"$KUBECONFIG_SERVER_FILE"

  log "Control-plane entry: in-cluster=$api_external kubeconfig=$(cat "$KUBECONFIG_SERVER_FILE")"
  log "Writing k0sctl config to $K0SCTL_CONFIG"
  {
    cat <<EOF
apiVersion: k0sctl.k0sproject.io/v1beta1
kind: Cluster
metadata:
  name: $CLUSTER_NAME
spec:
  hosts:
$hosts  k0s:
    version: $K0S_VERSION
    config:
      apiVersion: k0s.k0sproject.io/v1beta1
      kind: ClusterConfig
      metadata:
        name: k0s
      spec:
        api:
          externalAddress: $api_external
          sans:
$sans
EOF
  } >"$K0SCTL_CONFIG"
}

purge_known_hosts() {
  local ip
  for ip in "$@"; do
    [[ -n "$ip" ]] || continue
    ssh-keygen -R "$ip" >/dev/null 2>&1 || true
  done
}

apply_k0sctl() {
  local ips
  ips="$(awk '$1 == "address:" {print $2}' "$K0SCTL_CONFIG" 2>/dev/null || true)"
  if [[ -n "$ips" ]]; then
    log "Clearing stale known_hosts entries for: $(echo "$ips" | tr '\n' ' ')"
    # shellcheck disable=SC2086  # word splitting is intended: one arg per IP
    purge_known_hosts $ips
  fi
  log "Running k0sctl apply"
  k0sctl apply --config "$K0SCTL_CONFIG"
}

fetch_kubeconfig() {
  log "Fetching kubeconfig to $KUBECONFIG_OUT"
  k0sctl kubeconfig --config "$K0SCTL_CONFIG" >"$KUBECONFIG_OUT"

  local target=""
  [[ -s "$KUBECONFIG_SERVER_FILE" ]] && target="$(<"$KUBECONFIG_SERVER_FILE")"
  if [[ -n "$target" ]]; then
    log "Pointing kubeconfig server at $target"
    sed -i.bak -E \
      "s#^([[:space:]]*server: https://).*:([0-9]+)[[:space:]]*\$#\1${target}:\2#" \
      "$KUBECONFIG_OUT"
    rm -f "$KUBECONFIG_OUT.bak"
  else
    log "No $KUBECONFIG_SERVER_FILE (run 'config' first) -- leaving server as-is"
  fi
  grep -E '^\s*server:' "$KUBECONFIG_OUT" >&2 || true
  log "export KUBECONFIG=$KUBECONFIG_OUT"
}

label_imagebuild_node() {
  # By default EVERY pure worker is an imagebuild node (one per failure domain
  # with the default topology). Set IMAGEBUILD_NODE to a space-separated list of
  # node names to pin a specific subset instead.
  local nodes="$IMAGEBUILD_NODE"
  if [[ -z "$nodes" ]]; then
    local n r
    while read -r n r; do
      [[ "$r" == "worker" ]] && nodes+="${nodes:+ }$n"
    done < <(node_name_roles)
  fi
  if [[ -z "$nodes" ]]; then
    log "No pure workers and IMAGEBUILD_NODE unset -- skipping imagebuild labelling"
    return 0
  fi

  log "Waiting for the API server to answer..."
  for _ in $(seq 1 24); do
    kubectl --kubeconfig "$KUBECONFIG_OUT" get --raw=/readyz >/dev/null 2>&1 && break
    sleep 5
  done

  local node
  for node in $nodes; do
    log "Dedicating worker $node to image builds"
    kubectl --kubeconfig "$KUBECONFIG_OUT" label node "$node" \
      banlieue.io/imagebuild=true --overwrite
    kubectl --kubeconfig "$KUBECONFIG_OUT" taint node "$node" \
      dedicated=imagebuild:NoSchedule --overwrite
  done
}

# ============================================================================
# Vault + flux-operator (backend-agnostic; opt-in via FLUX_ENABLED, ADR-0018)
# ============================================================================
# Fetch ONE field from a Vault KV secret. Uses the `vault kv` subcommand (not
# `vault read`) so this doesn't need to know whether the mount is KV v1 or v2
# -- unlike a raw API/lookup-plugin path, `vault kv get` handles the /data/
# nesting itself. VAULT_ADDR/VAULT_TOKEN are read by the vault binary from the
# ambient environment; no login flow is implemented here.
vault_kv_get() {
  local field="$1" attempt out
  for attempt in 1 2 3; do
    if out="$(vault kv get -mount="$VAULT_KV_MOUNT" -field="$field" "$VAULT_SECRET_PATH" 2>/dev/null)"; then
      printf '%s' "$out"
      return 0
    fi
    [[ "$attempt" -lt 3 ]] && sleep 5
  done
  log "vault kv get failed for field '$field' at $VAULT_KV_MOUNT/$VAULT_SECRET_PATH after 3 attempts"
  return 1
}

check_deps_flux() {
  [[ "$FLUX_ENABLED" == "true" ]] || return 0
  command -v vault >/dev/null 2>&1 || { log "FLUX_ENABLED=true requires the vault CLI"; exit 1; }
  [[ -n "${VAULT_ADDR:-}" ]] || { log "VAULT_ADDR is not set (required when FLUX_ENABLED=true)"; exit 1; }
  [[ -n "${VAULT_TOKEN:-}" ]] || { log "VAULT_TOKEN is not set (required when FLUX_ENABLED=true)"; exit 1; }
  [[ -n "$VAULT_KV_MOUNT" ]] || { log "VAULT_KV_MOUNT is empty -- set it in BANLIEUE_ENV_FILE"; exit 1; }
  [[ -n "$VAULT_SECRET_PATH" ]] || { log "VAULT_SECRET_PATH is empty -- set it in BANLIEUE_ENV_FILE"; exit 1; }
  [[ -n "$FLUX_REGISTRY" ]] || { log "FLUX_REGISTRY is empty -- set it in BANLIEUE_ENV_FILE"; exit 1; }
  [[ -n "$FLUX_CORE_OCI_URL" ]] || { log "FLUX_CORE_OCI_URL is empty -- set it in BANLIEUE_ENV_FILE"; exit 1; }
  if [[ -n "$FLUX_CA_BUNDLE_FILE" && ! -f "$FLUX_CA_BUNDLE_FILE" ]]; then
    log "FLUX_CA_BUNDLE_FILE=$FLUX_CA_BUNDLE_FILE not found"; exit 1
  fi
}

# dockerconfigjson auths entry for one registry.
_flux_dockerconfigjson() {
  local user="$1" pass="$2" registry="$3" auth
  auth="$(printf '%s:%s' "$user" "$pass" | base64 | tr -d '\n')"
  printf '{"auths":{"%s":{"username":"%s","password":"%s","auth":"%s"}}}' \
    "$registry" "$user" "$pass" "$auth"
}

# flux-operator's own install manifest, optionally rewritten to an internal
# image mirror (generalizes the reference's hardcoded Artifactory rewrite).
render_flux_operator_install() {
  local url="$FLUX_OPERATOR_INSTALL_URL/$FLUX_OPERATOR_VERSION/install.yaml"
  if [[ "$FLUX_OPERATOR_REGISTRY" != "ghcr.io/controlplaneio-fluxcd" ]]; then
    curl -fsSL "$url" | sed -e "s#ghcr\.io/controlplaneio-fluxcd#${FLUX_OPERATOR_REGISTRY}#"
  else
    curl -fsSL "$url"
  fi
}

# flux-system namespace, optional flux-tls CA secret, and the flux-artifactory
# dockerconfigjson image-pull secret built from the Vault-fetched credential.
render_flux_prereqs() {
  local flux_user="$1" flux_pass="$2" dockerconfig b64dc ca_b64
  echo "---"
  echo "apiVersion: v1"
  echo "kind: Namespace"
  echo "metadata:"
  echo "  name: flux-system"
  if [[ -n "$FLUX_CA_BUNDLE_FILE" ]]; then
    ca_b64="$(base64 <"$FLUX_CA_BUNDLE_FILE" | tr -d '\n')"
    echo "---"
    echo "apiVersion: v1"
    echo "kind: Secret"
    echo "type: Opaque"
    echo "metadata:"
    echo "  name: flux-tls"
    echo "  namespace: flux-system"
    echo "data:"
    echo "  ca.crt: $ca_b64"
  fi
  dockerconfig="$(_flux_dockerconfigjson "$flux_user" "$flux_pass" "$FLUX_REGISTRY")"
  b64dc="$(printf '%s' "$dockerconfig" | base64 | tr -d '\n')"
  echo "---"
  echo "apiVersion: v1"
  echo "kind: Secret"
  echo "type: kubernetes.io/dockerconfigjson"
  echo "metadata:"
  echo "  name: flux-artifactory"
  echo "  namespace: flux-system"
  echo "data:"
  echo "  .dockerconfigjson: $b64dc"
}

# FluxInstance (installs the operator's components) + the flux-core
# OCIRepository it reconciles from. certSecretRef is only emitted when a CA
# bundle secret was created.
render_flux_instance() {
  local has_ca="$1"
  echo "---"
  echo "apiVersion: fluxcd.controlplane.io/v1"
  echo "kind: FluxInstance"
  echo "metadata:"
  echo "  name: flux"
  echo "  namespace: flux-system"
  echo "spec:"
  echo "  distribution:"
  echo "    version: \"$FLUX_DISTRIBUTION_VERSION\""
  echo "    registry: \"$FLUX_OPERATOR_REGISTRY\""
  echo "    imagePullSecret: \"flux-artifactory\""
  echo "  components:"
  echo "    - source-controller"
  echo "    - kustomize-controller"
  echo "    - helm-controller"
  echo "    - notification-controller"
  echo "  cluster:"
  echo "    type: kubernetes"
  echo "    multitenant: false"
  echo "    networkPolicy: false"
  echo "---"
  echo "apiVersion: source.toolkit.fluxcd.io/v1"
  echo "kind: OCIRepository"
  echo "metadata:"
  echo "  name: flux-core"
  echo "  namespace: flux-system"
  echo "spec:"
  echo "  interval: 5m0s"
  echo "  provider: generic"
  echo "  ref:"
  echo "    semver: '>= 0.0.0-0'"
  echo "  secretRef:"
  echo "    name: flux-artifactory"
  if [[ "$has_ca" == "true" ]]; then
    echo "  certSecretRef:"
    echo "    name: flux-tls"
  fi
  echo "  timeout: 60s"
  echo "  url: $FLUX_CORE_OCI_URL"
}

# The flux-core Kustomization: reconciles whatever the OCIRepository above
# pulls, substituting CLUSTER_DNS plus any operator-supplied FLUX_SUBSTITUTIONS.
render_flux_bootstrap() {
  echo "---"
  echo "apiVersion: kustomize.toolkit.fluxcd.io/v1"
  echo "kind: Kustomization"
  echo "metadata:"
  echo "  name: flux-core"
  echo "  namespace: flux-system"
  echo "spec:"
  echo "  sourceRef:"
  echo "    kind: OCIRepository"
  echo "    name: flux-core"
  echo "  path: \"./\""
  echo "  interval: 10m"
  echo "  timeout: 5m"
  echo "  prune: false"
  echo "  wait: true"
  echo "  serviceAccountName: kustomize-controller"
  echo "  targetNamespace: flux-system"
  echo "  postBuild:"
  echo "    substitute:"
  echo "      CLUSTER_DNS: \"${API_SAN:-$CLUSTER_NAME}\""
  local kv k v
  for kv in $FLUX_SUBSTITUTIONS; do
    k="${kv%%=*}"; v="${kv#*=}"
    echo "      $k: \"$v\""
  done
}

# Deploy flux-operator + flux-core onto the cluster. Backend-agnostic: reuses
# populate_node_table/NODE_TABLE (already resolves a uniform table for both
# vsphere and libvirt) and drops manifests onto
# /var/lib/k0s/manifests/flux-operator/ on the first controller -- k0s's
# manifest deployer auto-applies them regardless of install method (native or
# k0sctl).
deploy_flux() {
  if [[ "$FLUX_ENABLED" != "true" ]]; then
    log "FLUX_ENABLED is not 'true' -- skipping flux-operator deployment"
    return 0
  fi
  populate_node_table
  local row name role ip cp_ip=""
  for row in "${NODE_TABLE[@]}"; do
    IFS='|' read -r name role ip <<<"$row"
    if [[ "$role" == controller* ]]; then cp_ip="$ip"; break; fi
  done
  [[ -n "$cp_ip" ]] || { log "no controller found in NODE_TABLE"; exit 1; }

  log "[$cp_ip] fetching flux registry credential from Vault ($VAULT_KV_MOUNT/$VAULT_SECRET_PATH)"
  local flux_user flux_pass
  flux_user="$(vault_kv_get "$VAULT_FLUX_USER_KEY")" || exit 1
  flux_pass="$(vault_kv_get "$VAULT_FLUX_PASS_KEY")" || exit 1

  local has_ca="false"
  [[ -n "$FLUX_CA_BUNDLE_FILE" ]] && has_ca="true"

  log "[$cp_ip] writing flux-operator manifests to /var/lib/k0s/manifests/flux-operator/"
  ssh_run "$cp_ip" 'sudo mkdir -p /var/lib/k0s/manifests/flux-operator'
  render_flux_operator_install | ssh_run "$cp_ip" 'sudo tee /var/lib/k0s/manifests/flux-operator/00-install.yaml >/dev/null'
  render_flux_prereqs "$flux_user" "$flux_pass" | ssh_run "$cp_ip" 'sudo tee /var/lib/k0s/manifests/flux-operator/10-pre-reqs.yaml >/dev/null'
  render_flux_instance "$has_ca" | ssh_run "$cp_ip" 'sudo tee /var/lib/k0s/manifests/flux-operator/30-flux-instance.yaml >/dev/null'
  render_flux_bootstrap | ssh_run "$cp_ip" 'sudo tee /var/lib/k0s/manifests/flux-operator/40-flux-bootstrap.yaml >/dev/null'
  log "flux-operator manifests staged on $cp_ip -- k0s will apply them automatically"
}

tailscale_remove_devices() {
  if [[ -z "$TAILSCALE_API_KEY" ]]; then
    return 0
  fi
  local devices
  if ! devices="$(curl -fsSL -u "$TAILSCALE_API_KEY:" \
      "https://api.tailscale.com/api/v2/tailnet/$TAILSCALE_TAILNET/devices" 2>/dev/null)"; then
    log "Failed to list tailnet devices (check TAILSCALE_API_KEY / TAILSCALE_TAILNET)"
    return 0
  fi
  local idx name id
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    name="$(vm_name "$idx")"
    id="$(TSDEVICES="$devices" python3 - "$name" <<'PY'
import json, os, sys
name = sys.argv[1]
for d in json.loads(os.environ["TSDEVICES"]).get("devices", []):
    if d.get("hostname") == name:
        print(d["id"]); break
PY
)"
    [[ -z "$id" ]] && continue
    log "Deleting tailnet device $name ($id)"
    curl -fsSL -X DELETE -u "$TAILSCALE_API_KEY:" \
      "https://api.tailscale.com/api/v2/device/$id" >/dev/null \
      || log "FAILED to delete tailnet device $name"
  done
}

print_env_template() {
  cat <<'TEMPLATE'
# banlieue k0s bootstrap -- vSphere env file (UNTRACKED; keep outside the repo).
# Source it via BANLIEUE_ENV_FILE=/path/to/this ./bootstrap-k0s-cluster.sh all
#
# vCenter creds/URL/datacenter/CA come from your ambient GOVC_* environment.
BACKEND=vsphere
VCPUS=4
MEM_MB=10240
DISK_GB=100
SSH_USER=root
SSH_PUBKEY=$HOME/.ssh/id_ed25519.pub          # key injected into the VMs
SSH_PRIVKEY=$HOME/.ssh/id_ed25519
CLUSTER_NAME=banlieue
NET_PREFIX=24
DNS_SERVERS=192.0.2.53,198.51.100.53          # your real resolvers
DNS_DOMAIN=example.com                          # your real search domain
API_SAN=banlieue.example.com                    # stable API name (must resolve)
VSPHERE_FOLDER=/DC-EXAMPLE/vm/banlieue          # created if missing

# k0s native install (binary -> /opt/k0s + symlink; not k0sctl)
K0S_VERSION=v1.35.1+k0s.1
K0S_BINARY_BASEURL=https://github.com/k0sproject/k0s/releases/download   # internal mirror for air-gapped
K0S_IMAGE_REPOSITORY=                            # e.g. an internal registry mirror
# K0S_IMAGE_REPOSITORY alone doesn't repoint the CRI sandbox (pause) image --
# set this too in air-gapped estates or nodes stay NotReady pulling quay.io:
K0S_PAUSE_IMAGE=                                 # e.g. quay-mirror.example.com/k0sproject/pause
K0S_PAUSE_VERSION=                               # e.g. 3.9 (match the tag your mirror carries)
K0S_NETWORK_PROVIDER=calico                      # or kuberouter (k0s default)
# CALICO_REACH=10.0.0.90                          # can-reach addr (defaults to first controller IP)

# Per-cluster placement as FLAT vars <PREFIX>_<cluster_id> (macOS bash 3.2 has
# no associative arrays). One set per cluster id you use in NODES; fill with
# paths from `govc find` / `govc ls`.
VSPHERE_RP_01="/DC-EXAMPLE/host/Compute/Cluster-01/Resources"
VSPHERE_DSC_01="/DC-EXAMPLE/datastore/Cluster-01-DSC"   # SDRS datastore-cluster PATH; a member DS is auto-picked
VSPHERE_NET_01="port-group-cluster-01"
VSPHERE_TPL_01="/DC-EXAMPLE/vm/templates/cluster-01/rhelXX-kairos-vX.Y.Z"
# Optional per-cluster gateway override (default = first three octets of ip + .1)
# VSPHERE_GW_01="10.0.0.1"

# Node table: "<name/fqdn> <cluster_id> <static_ip> <role>"
NODES=(
  "node01.example.com 01 10.0.0.90 controller+worker"
  "node02.example.com 01 10.0.0.91 worker"
)

# Optional: flux-operator + flux-core (ADR-0018). Leave FLUX_ENABLED=false to
# skip this entirely. VAULT_ADDR/VAULT_TOKEN are exported in your shell, NOT
# written here. VAULT_KV_MOUNT/VAULT_SECRET_PATH point at YOUR estate's KV
# secret (no leading/trailing slash, no /data/ segment -- `vault kv get`
# handles that itself); the example values below are placeholders, not a
# convention to imitate.
# FLUX_ENABLED=true
# VAULT_KV_MOUNT=kv
# VAULT_SECRET_PATH=myteam/dev/service-ids
# VAULT_FLUX_USER_KEY=flux                        # field name in that secret
# VAULT_FLUX_PASS_KEY=flux-pass
# FLUX_REGISTRY=registry.example.com               # imagePullSecret target
# FLUX_CORE_OCI_URL=oci://registry.example.com/flux-core/main
# FLUX_OPERATOR_VERSION=v0.20.0
# FLUX_OPERATOR_REGISTRY=ghcr.io/controlplaneio-fluxcd   # or your mirror
# FLUX_CA_BUNDLE_FILE=                             # optional local CA file
# FLUX_SUBSTITUTIONS="CLUSTER_ENV=dev TEAM=myteam" # operator-defined keys
TEMPLATE
}

main() {
  local cmd="${1:-all}"
  case "$cmd" in
    --print-env-template) print_env_template ;;
    vms)
      check_deps
      [[ "$BACKEND" == "libvirt" ]] && fetch_installer_iso
      create_vms
      ;;
    config)
      check_deps
      k0s_config
      ;;
    apply)
      check_deps
      k0s_apply
      ;;
    kubeconfig)
      check_deps
      k0s_kubeconfig
      ;;
    label)
      check_deps
      label_imagebuild_node
      ;;
    flux)
      check_deps
      deploy_flux
      ;;
    destroy)
      destroy_all
      ;;
    all)
      check_deps
      [[ "$BACKEND" == "libvirt" ]] && fetch_installer_iso
      create_vms
      k0s_config
      k0s_apply
      k0s_kubeconfig
      label_imagebuild_node
      deploy_flux
      ;;
    *)
      echo "Usage: $0 [all|vms|config|apply|kubeconfig|label|flux|destroy|--print-env-template]" >&2
      echo "  BACKEND=libvirt (default) | vsphere   (vsphere also needs BANLIEUE_ENV_FILE)" >&2
      echo "  FLUX_ENABLED=true         to enable the flux/all flux-operator deployment step" >&2
      exit 1
      ;;
  esac
}

main "$@"
