#!/usr/bin/env bash
# Bootstraps a 3-node k0s cluster: creates VMs with virt-install/libvirt,
# then installs k0s onto them with k0sctl.
#
# Run on the KVM/libvirt hypervisor host (or point LIBVIRT_URI at a remote
# one, e.g. qemu+ssh://user@host/system). Every step is idempotent: re-running
# skips VMs/disks that already exist and just reconciles the rest.
#
# Uses a plain Debian 13 cloud-init image (or BASE_IMAGE_PATH pointing at a
# customized derivative, e.g. build-debian-k0s-image.sh's output with k0s
# pre-baked). Cloud-config is delivered via a NoCloud (cidata) ISO we build
# and attach ourselves (bus=sata cdrom) -- NOT virt-install's own
# --cloud-init flag: that convenience feature places its auto-generated ISO
# in a transient /var/lib/libvirt/boot/ location with automatic
# first-boot-only cleanup, and that cleanup was observed racing/failing
# (libvirtd: "Unable to get XATTR ... No such file or directory" / "Unable
# to remove disk metadata"), tearing the domain down ~2s after start.
#
# --boot uefi is required: Debian's "genericcloud" image variant (unlike
# plain "generic") boots reliably under UEFI but resets in an instant,
# silent loop under legacy BIOS -- confirmed by booting the pristine,
# unmodified source image both ways (BIOS: repeating "Booting `Debian
# GNU/Linux'" on serial, forever, no kernel output ever; UEFI: boots
# clean, cloud-init runs). Ruled out along the way: virt-customize
# corruption (pristine image fails identically), CPU passthrough
# (fails identically with a plain qemu64 CPU too), and the itco/Q35
# watchdog device (present on this host's other, working VMs too).
#
# SSH_USER defaults to your own username (adds it as a sudoer via
# cloud-init, same as the Ubuntu/Debian cloud-image convention) rather than
# root; set SSH_USER=root to go back to the root/disable_root:false path.
#
# Set TAILSCALE_AUTHKEY to join each VM to your tailnet on first boot (use a
# non-ephemeral, reusable key -- an ephemeral one gets the node dropped from
# the tailnet on disconnect, and re-auth as a "new" device can hand it a
# different IP, invalidating the SAN baked in below). The k0s API server's
# TLS cert only covers addresses known at cluster-init time, so once each
# VM's Tailscale IP comes up it's added as an extra spec.api.san in the
# generated k0s config, and the kubeconfig's server address is rewritten to
# match -- otherwise `kubectl` from outside grill's libvirt network fails
# with an x509 SAN mismatch. Set EXTRA_SANS (space-separated hostnames/IPs,
# e.g. a stable DNS name of your own pointed at one of the VMs) to bake in
# more SANs regardless of Tailscale.
#
# Usage:
#   ./bootstrap-k0s-cluster.sh [all|vms|config|apply|kubeconfig|destroy]
#
# All settings below can be overridden via environment variables (the
# Makefile in this directory forwards its own variables the same way).
set -euo pipefail

VM_COUNT="${VM_COUNT:-3}"
VCPUS="${VCPUS:-2}"
MEM_MB="${MEM_MB:-6144}"
DISK_GB="${DISK_GB:-25}"
VM_PREFIX="${VM_PREFIX:-k0s}"

LIBVIRT_URI="${LIBVIRT_URI:-qemu:///system}"
LIBVIRT_NETWORK="${LIBVIRT_NETWORK:-default}"
LIBVIRT_POOL="${LIBVIRT_POOL:-default}"
OS_VARIANT="${OS_VARIANT:-debian13}"
IMAGE_URL="${IMAGE_URL:-https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2}"
IP_WAIT_ATTEMPTS="${IP_WAIT_ATTEMPTS:-60}"   # 60 * 5s = 5min
SSH_WAIT_ATTEMPTS="${SSH_WAIT_ATTEMPTS:-60}" # 60 * 5s = 5min
TAILSCALE_WAIT_ATTEMPTS="${TAILSCALE_WAIT_ATTEMPTS:-24}" # 24 * 5s = 2min

# Set BASE_IMAGE_PATH to use a locally-built image (e.g. build-debian-k0s-image.sh's
# output, with k0s pre-baked) instead of downloading IMAGE_URL.
BASE_IMAGE_PATH="${BASE_IMAGE_PATH:-}"

SSH_PUBKEY="${SSH_PUBKEY:-$HOME/.ssh/id_ed25519.pub}"
SSH_PRIVKEY="${SSH_PRIVKEY:-${SSH_PUBKEY%.pub}}"
SSH_USER="${SSH_USER:-${USERNAME:-${USER:-root}}}"

# Set to join each VM to your tailnet on first boot (tailscale installed +
# `tailscale up --authkey=... --ssh` run via cloud-init runcmd, so you can
# also SSH in via Tailscale's own identity-based SSH, not just the regular
# key). Left empty by default: no tailscale at all.
TAILSCALE_AUTHKEY="${TAILSCALE_AUTHKEY:-}"

CLUSTER_NAME="${CLUSTER_NAME:-${VM_PREFIX}-cluster}"
# Space-separated role per node, one entry per VM (index 0..VM_COUNT-1).
# Default: all three nodes run controller+worker, giving a 3-node etcd quorum.
NODE_ROLES="${NODE_ROLES:-controller+worker controller+worker controller+worker}"

WORKDIR="${WORKDIR:-$HOME/.local/share/k0s-bootstrap}"
# virt-install's qemu process runs as the unprivileged libvirt-qemu user,
# which can't traverse into $HOME (e.g. /root is 700) -- disks default under
# libvirt's own images pool (world-traversable, 711) rather than under
# WORKDIR for exactly that reason. Override only if your libvirt storage
# pool lives elsewhere.
POOL_DIR="${POOL_DIR:-/var/lib/libvirt/images/k0s-bootstrap}"
BASE_IMAGE="$POOL_DIR/$(basename "${BASE_IMAGE_PATH:-$IMAGE_URL}")"
K0SCTL_CONFIG="${K0SCTL_CONFIG:-$WORKDIR/k0sctl.yaml}"
KUBECONFIG_OUT="${KUBECONFIG_OUT:-$WORKDIR/kubeconfig}"

mkdir -p "$WORKDIR" "$POOL_DIR"

log() { echo "==> $*" >&2; }

check_deps() {
  local missing=()
  for cmd in virt-install virsh qemu-img k0sctl curl ssh genisoimage; do
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

fetch_base_image() {
  if [[ -f "$BASE_IMAGE" ]]; then
    log "Base image already present at $BASE_IMAGE, skipping"
  elif [[ -n "$BASE_IMAGE_PATH" ]]; then
    [[ -f "$BASE_IMAGE_PATH" ]] || { log "BASE_IMAGE_PATH=$BASE_IMAGE_PATH not found"; exit 1; }
    log "Linking local base image $BASE_IMAGE_PATH -> $BASE_IMAGE"
    ln -f "$BASE_IMAGE_PATH" "$BASE_IMAGE" 2>/dev/null || cp "$BASE_IMAGE_PATH" "$BASE_IMAGE"
  else
    log "Downloading base cloud image from $IMAGE_URL"
    curl -fL --output "$BASE_IMAGE.tmp" "$IMAGE_URL"
    mv "$BASE_IMAGE.tmp" "$BASE_IMAGE"
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
  echo "#cloud-config" >>"$seed_dir/user-data"
  cat >>"$seed_dir/user-data" <<EOF
hostname: $name
manage_etc_hosts: true
ssh_pwauth: false
growpart:
  mode: auto
  devices: ["/"]
resize_rootfs: true
EOF

  if [[ "$SSH_USER" == "root" ]]; then
    # cloud-init wraps any key on the *existing* root user with a forced
    # "please don't log in as root" command unless disable_root is off.
    cat >>"$seed_dir/user-data" <<EOF
disable_root: false
ssh_authorized_keys:
  - $(cat "$SSH_PUBKEY")
EOF
  else
    cat >>"$seed_dir/user-data" <<EOF
users:
  - name: $SSH_USER
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - $(cat "$SSH_PUBKEY")
EOF
  fi

  if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
runcmd:
  - curl -fsSL https://tailscale.com/install.sh | sh
  - tailscale up --authkey=$TAILSCALE_AUTHKEY --hostname=$name --ssh
EOF
  fi

  # virt-install's own --cloud-init flag builds this same ISO but places it
  # in a transient /var/lib/libvirt/boot/ location with automatic
  # first-boot-only cleanup -- that cleanup raced/failed here (libvirtd:
  # "Unable to get XATTR ... No such file or directory" / "Unable to remove
  # disk metadata"), tearing the domain down ~2s after start. Building our
  # own persistent seed ISO and attaching it as a normal cdrom sidesteps
  # that entirely; it's the same mechanism that ran reliably all session
  # before this detour.
  genisoimage -output "$seed_dir.iso" -volid cidata -joliet -rock \
    "$seed_dir/user-data" "$seed_dir/meta-data" >/dev/null
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

  log "Creating disk for $name (${DISK_GB}G, backed by $(basename "$BASE_IMAGE"))"
  qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMAGE" "$disk" "${DISK_GB}G" >/dev/null

  log "Building cloud-init seed for $name"
  make_cloud_init_files "$name" "$seed_dir"

  log "Defining and starting VM $name (${VCPUS} vCPU, ${MEM_MB}MB RAM)"
  virt-install \
    --connect "$LIBVIRT_URI" \
    --name "$name" \
    --memory "$MEM_MB" \
    --vcpus "$VCPUS" \
    --disk "path=$disk,format=qcow2,bus=virtio" \
    --disk "path=$seed_iso,device=cdrom,bus=sata" \
    --os-variant "$OS_VARIANT" \
    --network "network=$LIBVIRT_NETWORK,model=virtio" \
    --boot uefi \
    --graphics none \
    --console pty,target_type=serial \
    --import \
    --noautoconsole
}

create_vms() {
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    create_vm "$idx"
  done
}

vm_ip() {
  local name="$1"
  virsh --connect "$LIBVIRT_URI" domifaddr "$name" --source lease 2>/dev/null \
    | awk '/ipv4/ {print $4}' | cut -d/ -f1 | head -n1
}

wait_for_ip() {
  local name="$1" ip=""
  log "Waiting for $name to get a DHCP lease..."
  for _ in $(seq 1 "$IP_WAIT_ATTEMPTS"); do
    ip="$(vm_ip "$name")"
    [[ -n "$ip" ]] && { echo "$ip"; return 0; }
    sleep 5
  done
  log "Timed out waiting for an IP address for $name"
  exit 1
}

ssh_run() {
  local ip="$1"; shift
  # These VMs are ephemeral and libvirt's DHCP pool gets reused across
  # runs/recreations, so a *different* VM can show up later at the same IP
  # with a different host key. accept-new rejects that as "changed" and
  # fails every single retry deterministically (looks exactly like a VM
  # that never comes up) -- host identity isn't meaningful for throwaway
  # dev VMs anyway, so skip checking it entirely rather than accumulate
  # stale known_hosts entries.
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
    ts_ip="$(ssh_run "$ip" "tailscale ip -4 2>/dev/null" 2>/dev/null | tr -d '\r')"
    [[ -n "$ts_ip" ]] && { echo "$ts_ip"; return 0; }
    sleep 5
  done
  log "Timed out waiting for a Tailscale IP on $ip -- continuing without it (its" \
      "API server cert won't cover the tailnet address)"
  return 1
}

# The k0s API server's TLS cert only covers addresses known at cluster-init
# time (local interfaces + service IPs) -- the Tailscale IP isn't one of them
# unless we tell k0s about it upfront. So each Tailscale IP (once up) gets
# added as an extra spec.api.san in the generated k0s config, and the
# internal<->tailscale IP mapping gets persisted to TAILSCALE_IP_MAP for
# fetch_kubeconfig() to rewrite the kubeconfig's server address with later
# (a separate invocation, so it can't just reuse a local variable here).
TAILSCALE_IP_MAP="${TAILSCALE_IP_MAP:-$WORKDIR/tailscale-ips.map}"
# Space-separated extra hostnames/IPs to bake into the API server cert's
# SANs regardless of Tailscale, e.g. a stable DNS name you point at
# whichever address you actually use to connect: EXTRA_SANS=k0s.example.com
EXTRA_SANS="${EXTRA_SANS:-}"

generate_k0sctl_config() {
  local roles=($NODE_ROLES)
  local hosts="" sans=""
  for extra_san in $EXTRA_SANS; do
    sans+="            - $extra_san"$'\n'
  done
  : >"$TAILSCALE_IP_MAP"
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    local name ip role ts_ip=""
    name="$(vm_name "$idx")"
    ip="$(wait_for_ip "$name")"
    wait_for_ssh "$ip"
    if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
      ts_ip="$(wait_for_tailscale_ip "$ip" || true)"
      if [[ -n "$ts_ip" ]]; then
        echo "$ip $ts_ip" >>"$TAILSCALE_IP_MAP"
        sans+="            - $ts_ip"$'\n'
      fi
    fi
    role="${roles[$idx]:-worker}"
    hosts+=$(cat <<EOF
  - role: $role
    ssh:
      address: $ip
      user: $SSH_USER
      port: 22
      keyPath: $SSH_PRIVKEY
EOF
)
    hosts+=$'\n'
  done

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
    version: null
EOF
    if [[ -n "$sans" ]]; then
      cat <<EOF
    config:
      apiVersion: k0s.k0sproject.io/v1beta1
      kind: ClusterConfig
      spec:
        api:
          sans:
$sans
EOF
    fi
  } >"$K0SCTL_CONFIG"
}

apply_k0sctl() {
  log "Running k0sctl apply"
  k0sctl apply --config "$K0SCTL_CONFIG"
}

fetch_kubeconfig() {
  log "Fetching kubeconfig to $KUBECONFIG_OUT"
  k0sctl kubeconfig --config "$K0SCTL_CONFIG" >"$KUBECONFIG_OUT"

  if [[ -s "$TAILSCALE_IP_MAP" ]]; then
    log "Rewriting kubeconfig server address to its Tailscale IP"
    while read -r internal_ip ts_ip; do
      sed -i.bak "s/$internal_ip/$ts_ip/g" "$KUBECONFIG_OUT"
    done <"$TAILSCALE_IP_MAP"
    rm -f "$KUBECONFIG_OUT.bak"
  fi

  log "export KUBECONFIG=$KUBECONFIG_OUT"
}

destroy_all() {
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    local name
    name="$(vm_name "$idx")"
    if virsh --connect "$LIBVIRT_URI" dominfo "$name" >/dev/null 2>&1; then
      log "Destroying VM $name"
      virsh --connect "$LIBVIRT_URI" destroy "$name" >/dev/null 2>&1 || true
      virsh --connect "$LIBVIRT_URI" undefine "$name" --remove-all-storage >/dev/null 2>&1 || true
    fi
    rm -f "$POOL_DIR/$name.qcow2" "$POOL_DIR/$name-seed.iso"
    rm -rf "$POOL_DIR/$name-seed"
  done
  rm -f "$K0SCTL_CONFIG" "$KUBECONFIG_OUT" "$TAILSCALE_IP_MAP"
}

main() {
  local cmd="${1:-all}"
  case "$cmd" in
    vms)
      check_deps
      fetch_base_image
      create_vms
      ;;
    config)
      check_deps
      generate_k0sctl_config
      ;;
    apply)
      apply_k0sctl
      ;;
    kubeconfig)
      fetch_kubeconfig
      ;;
    destroy)
      destroy_all
      ;;
    all)
      check_deps
      fetch_base_image
      create_vms
      generate_k0sctl_config
      apply_k0sctl
      fetch_kubeconfig
      ;;
    *)
      echo "Usage: $0 [all|vms|config|apply|kubeconfig|destroy]" >&2
      exit 1
      ;;
  esac
}

main "$@"
