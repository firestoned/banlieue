#!/usr/bin/env bash
# Bootstraps a 4-node k0s cluster (3 controller+worker, 1 worker) on Kairos
# Hadron VMs: creates VMs with virt-install/libvirt, lets Kairos install
# itself from its ISO, then installs k0s onto them with k0sctl.
#
# Run on the KVM/libvirt hypervisor host (or point LIBVIRT_URI at a remote
# one, e.g. qemu+ssh://user@host/system). Every step is idempotent: re-running
# skips VMs/disks that already exist and just reconciles the rest.
#
# Base OS: Kairos "Hadron" CORE (https://github.com/kairos-io/hadron) -- a
# minimal, immutable, from-scratch Linux with NO Kubernetes bundled. Hadron
# releases ship installer ISOs only (no qcow2 cloud image), so instead of the
# old "clone a backing-file qcow2 and boot" flow, each VM gets an EMPTY disk,
# boots the ISO, and Kairos's live environment installs itself onto the disk
# unattended (the `install:` stanza in the cloud-config seed) and powers off.
# The script then ejects the ISO and boots the installed system.
#
# k0s: not baked into any image. k0sctl installs exactly K0S_VERSION on every
# node, uploading the binary over SSH (uploadBinary: true) rather than having
# the hosts download it -- the whole point of the core flavor is choosing the
# k0s version ourselves, and the minimal OS isn't guaranteed to carry a
# download tool.
#
# Immutability note: /etc and /var are ephemeral on Kairos, but its DEFAULT
# persistent bind-mounts already cover everything k0s needs (/etc/k0s,
# /var/lib/k0s, /etc/systemd, /var/lib/kubelet, /etc/ssh, /var/lib/tailscale),
# so k0sctl-managed config survives reboots with no extra configuration.
# See https://kairos.io/docs/architecture/immutable/
#
# Topology/security note: the default NODE_ROLES is 3 controller+worker (a
# fault-tolerant etcd quorum) plus ONE pure worker. The worker is labelled
# banlieue.io/imagebuild=true and tainted dedicated=imagebuild:NoSchedule
# (see label_imagebuild_node), reserving it for kairos-operator's privileged
# imagebuilder (auroraboot) pods -- a compromised build never compromises a
# controller. The control-plane taint is lifted (NO_TAINTS=true) because the
# single worker is reserved: everything else (kairos-operator, local-path,
# banlieue itself) schedules on the controllers.
#
# Cloud-config is delivered via a NoCloud (cidata) ISO we build and attach
# ourselves (bus=sata cdrom) -- NOT virt-install's own --cloud-init flag: that
# convenience feature places its auto-generated ISO in a transient
# /var/lib/libvirt/boot/ location with automatic first-boot-only cleanup, and
# that cleanup was observed racing/failing (libvirtd: "Unable to get XATTR ...
# No such file or directory" / "Unable to remove disk metadata"), tearing the
# domain down ~2s after start.
#
# --boot uefi is required: Kairos's ISOs are built for UEFI (the project
# targets Trusted Boot / Secure Boot), matching the previous Debian
# genericcloud images which also only booted reliably under UEFI.
#
# SSH_USER defaults to your own username (added via cloud-config in Kairos's
# `admin` group, the sudoers group) rather than root; set SSH_USER=root to go
# back to key-based root login.
#
# Set TAILSCALE_AUTHKEY to join each VM to your tailnet on first boot (use a
# non-ephemeral, reusable key -- an ephemeral one gets the node dropped from
# the tailnet on disconnect, and re-auth as a "new" device can hand it a
# different IP, invalidating the SAN baked in below). On an immutable OS you
# can't `curl | sh` an installer, so Tailscale's official static binaries are
# installed into /usr/local (persistent) from a network stage, with tailscaled
# registered as a regular systemd unit. The k0s API server's TLS cert only
# covers addresses known at cluster-init time, so once each VM's Tailscale IP
# comes up it's added as an extra spec.api.san in the generated k0s config,
# and the kubeconfig's server address is rewritten to match -- otherwise
# `kubectl` from outside the hypervisor's libvirt network fails with an x509
# SAN mismatch. Set EXTRA_SANS (space-separated hostnames/IPs, e.g. a stable
# DNS name of your own pointed at one of the VMs) to bake in more SANs
# regardless of Tailscale.
#
# Tailscale is an *external entry point only*: the tailnet addresses go in
# the cert SANs and in the kubeconfig, but every in-cluster component still
# talks over the internal libvirt DHCP network via spec.api.externalAddress
# (see API_EXTERNAL_ADDRESS below). Keeping those two roles separate is what
# makes `kubectl logs/exec/port-forward` work at all -- see the long comment
# on API_EXTERNAL_ADDRESS for why.
#
# Usage:
#   ./bootstrap-k0s-cluster.sh [all|vms|config|apply|kubeconfig|label|destroy]
#
# All settings below can be overridden via environment variables (the
# Makefile in this directory forwards its own variables the same way).
set -euo pipefail

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
# config convention). Picked to match what Kairos v4.1.2 bundles in its
# standard k0s images (k0sv1.35.5+k0s.0), so the core flavor stays level with
# the supported matrix.
K0S_VERSION="${K0S_VERSION:-1.35.5+k0s.0}"

# k0sctl's OS registry doesn't know Hadron yet -- auto-detection reads
# /etc/os-release (ID=hadron) and aborts with "unsupported OS: hadron". The
# `os:` host field overrides detection; it only selects k0sctl's configurer
# (file/service helpers), and in current k0sctl every Linux configurer is the
# same generic systemd implementation with a different ID matcher -- so
# `debian` is the most vanilla choice, not a claim about the actual OS.
K0SCTL_OS_OVERRIDE="${K0SCTL_OS_OVERRIDE:-debian}"

SSH_PUBKEY="${SSH_PUBKEY:-$HOME/.ssh/id_ed25519.pub}"
SSH_PRIVKEY="${SSH_PRIVKEY:-${SSH_PUBKEY%.pub}}"
SSH_USER="${SSH_USER:-${USERNAME:-${USER:-root}}}"

# Set to join each VM to your tailnet on first boot. Installed from the
# official static binaries into /usr/local (persistent on Kairos), see the
# "Install and join Tailscale" network stage. Left empty by default: no
# tailscale at all.
TAILSCALE_AUTHKEY="${TAILSCALE_AUTHKEY:-}"
# Tailscale static-binary release to install when TAILSCALE_AUTHKEY is set
# (pkgs.tailscale.com/stable/tailscale_<version>_amd64.tgz).
TAILSCALE_VERSION="${TAILSCALE_VERSION:-1.98.10}"
# Destroy-time tailnet cleanup: the VMs register as devices named k0s-01..N
# and destroying the VMs does NOT remove them (a non-ephemeral authkey means
# they linger in the admin console forever). With TAILSCALE_API_KEY set
# (admin console -> Settings -> Keys -> API access tokens), `destroy` deletes
# them via the API. TAILSCALE_TAILNET defaults to "-" (the default tailnet);
# set it explicitly (e.g. my-tailnet.ts.net) if your token rejects "-".
TAILSCALE_API_KEY="${TAILSCALE_API_KEY:-}"
TAILSCALE_TAILNET="${TAILSCALE_TAILNET:--}"

# k0s taints controller+worker nodes `node-role.kubernetes.io/control-plane:
# NoSchedule` by default. The default topology reserves the ONLY worker for
# image builds (see IMAGEBUILD_NODE below), so the default here LIFTS the
# taint: everything else -- kairos-operator, local-path, banlieue itself --
# schedules on the controllers. Set NO_TAINTS=false to keep the stock taint,
# e.g. if you run additional general-purpose workers.
NO_TAINTS="${NO_TAINTS:-true}"

CLUSTER_NAME="${CLUSTER_NAME:-${VM_PREFIX}-cluster}"
# Space-separated role per node, one entry per VM (index 0..VM_COUNT-1).
# Default: three controller+worker (a fault-tolerant etcd quorum) plus one
# pure worker, which label_imagebuild_node reserves for image builds.
NODE_ROLES="${NODE_ROLES:-controller+worker controller+worker controller+worker worker}"

# The pure worker is dedicated to image builds: labelled
# banlieue.io/imagebuild=true and tainted dedicated=imagebuild:NoSchedule, so
# only pods that explicitly tolerate the taint (the kairos-operator
# imagebuilder pods, once configured to) can be scheduled there -- everything
# else stays on the controllers. Empty means the LAST node whose
# NODE_ROLES entry is exactly "worker"; set to a node name (e.g. k0s-03) to
# pin a different one.
IMAGEBUILD_NODE="${IMAGEBUILD_NODE:-}"

WORKDIR="${WORKDIR:-$HOME/.local/share/k0s-bootstrap}"
# virt-install's qemu process runs as the unprivileged libvirt-qemu user,
# which can't traverse into $HOME (e.g. /root is 700) -- disks default under
# libvirt's own images pool (world-traversable, 711) rather than under
# WORKDIR for exactly that reason. Override only if your libvirt storage
# pool lives elsewhere.
POOL_DIR="${POOL_DIR:-/var/lib/libvirt/images/k0s-bootstrap}"
INSTALL_ISO="$POOL_DIR/$(basename "${BASE_IMAGE_PATH:-$IMAGE_URL}")"
K0SCTL_CONFIG="${K0SCTL_CONFIG:-$WORKDIR/k0sctl.yaml}"
KUBECONFIG_OUT="${KUBECONFIG_OUT:-$WORKDIR/kubeconfig}"
# Written by generate_k0sctl_config, read by fetch_kubeconfig (separate
# invocations) -- holds the address the kubeconfig's `server:` should use.
KUBECONFIG_SERVER_FILE="${KUBECONFIG_SERVER_FILE:-$WORKDIR/kubeconfig-server}"

mkdir -p "$WORKDIR" "$POOL_DIR"

log() { echo "==> $*" >&2; }

check_deps() {
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
# Unattended install: Kairos's live environment reads this datasource and
# installs itself onto the empty virtio disk (device "auto" picks it).
# install.reboot would boot the ISO again and reinstall in a loop, so the
# script instead waits for a powerOFF as its "install finished" signal,
# ejects the ISO, and starts the VM back up on the installed system.
install:
  auto: true
  device: "auto"
  # poweroff deliberately NOT set here: kairos-agent implements it as a
  # systemd-SCHEDULED shutdown (~60s delay, the "The system will power off
  # at ..." broadcast), which is dead time for a script that only cares the
  # VM went down. The after-install stage below powers off immediately
  # instead (kairos-install.after runs dead last in the install action,
  # after state recording and cleanup, so a forced poweroff there is safe).
EOF

  if [[ "$SSH_USER" == "root" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
users:
  - name: root
    ssh_authorized_keys:
      - $(cat "$SSH_PUBKEY")
EOF
  else
    # Kairos's `admin` group is the sudoers group -- Debian cloud-init's
    # `sudo:`/`lock_passwd:` user keys don't exist in Kairos's dialect.
    cat >>"$seed_dir/user-data" <<EOF
users:
  - name: $SSH_USER
    groups: [admin]
    ssh_authorized_keys:
      - $(cat "$SSH_PUBKEY")
EOF
  fi

  # Kairos runs its own yip-based stages engine on the cloud-config rather
  # than Debian cloud-init's modules, so boot-time commands live in a `boot`
  # stage (which runs on every boot, in the live environment AND the installed
  # system -- keep every command idempotent).
  cat >>"$seed_dir/user-data" <<EOF
stages:
  after-install:
    - name: "Power off immediately"
      commands:
        - poweroff -f
  boot:
    - name: "Provide loop devices for disk-image builds"
      # `files` is a property of a stage STEP, not a stage of its own. Written
      # here so the module and its parameter survive reboots; the commands
      # below cover the running system, which the files alone cannot because
      # module parameters only take effect at load time.
      files:
        - path: /etc/modules-load.d/banlieue-loop.conf
          permissions: 0644
          content: |
            loop
        - path: /etc/modprobe.d/banlieue-loop.conf
          permissions: 0644
          content: |
            # Create loop0..loop7 at module load. Without this the kernel
            # creates loop devices on demand, and a privileged build container
            # cannot see one that appears after it started (ADR-0010/bug-098).
            options loop max_loop=8
      commands:
        # Disk-image builders -- kairos-operator's auroraboot, which is what
        # banlieue-imagebuilder drives an OSArtifact through (ADR-0010) --
        # create a filesystem image and loop-mount it. A privileged container
        # only sees host device NODES that existed when the container was
        # CREATED, so any device appearing later is invisible to it and the
        # build fails with
        #
        #   gen-raw-efi-disk (error: open /dev/loop1: no such file or directory)
        #
        # even though the node has loop devices by the time you go looking.
        #
        # Loading the module is NOT sufficient. Modern kernels default to
        # max_loop=0, which means loop devices are created ON DEMAND through
        # /dev/loop-control rather than up front -- so `modprobe loop` alone
        # yields loop-control and nothing else, and the first build still
        # races. max_loop=8 makes the module create loop0..loop7 at load time.
        - modprobe loop max_loop=8 || modprobe loop
        # Belt and braces: if the module was already loaded (initrd, or an
        # earlier on-demand autoload) the parameter above is ignored, because
        # module parameters only apply at load time. Creating the nodes
        # directly is idempotent and works either way.
        - for i in 0 1 2 3 4 5 6 7; do [ -e /dev/loop$i ] || mknod -m 660 /dev/loop$i b 7 $i; done
EOF

  if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
    # Packages can't be installed on an immutable OS, and there is NO published
    # Tailscale sysext image (`kairos-agent sysext install` needs an
    # oci:/file:/http source -- a bare "tailscale" fails with "source does not
    # match any of oci:, file: or http(s)"). So install the official STATIC
    # binaries into /usr/local (Kairos-persistent) and register tailscaled as a
    # unit under /etc/systemd (also persistent). State goes to the default
    # /var/lib/tailscale, which Kairos bind-mounts persistent as well.
    # Everything is idempotent: the download is skipped once the binaries
    # exist, and `tailscale up` only runs when not already on the tailnet.
    # Runs in the `network` stage (cos-setup-network.service) because the
    # download needs connectivity -- `boot` fires too early.
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

# The Kairos after-install stage powers the VM OFF the moment the install
# finishes (see make_cloud_init_files) -- the shut-off is the "install
# finished" signal. create_vm then ejects the ISO and boots the installed
# system.
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
  # Boot the installer ISO first (boot_order=1), the empty disk second -- the
  # install powers the VM off before the disk is ever booted, and by the time
  # the disk becomes the boot device the ISO has been ejected below.
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

  # Come back on their own after the HOST reboots. libvirt defaults domains to
  # autostart=disable, so an unplanned host power loss leaves a cluster that is
  # defined, healthy, and entirely down until someone notices and starts each
  # VM by hand -- which is exactly what happened here on 2026-08-02.
  #
  # Set BEFORE the first start so a host that dies during bootstrap still
  # recovers.
  virsh --connect "$LIBVIRT_URI" autostart "$name"

  virsh --connect "$LIBVIRT_URI" start "$name"
}

create_vms() {
  # Create + install all VMs in PARALLEL: the Kairos install (partitioning +
  # image copy + poweroff) is the slowest part of the whole bootstrap and the
  # VMs don't depend on each other until k0sctl runs, so serializing it would
  # multiply the wait by VM_COUNT for no benefit. Logs from the N jobs
  # interleave -- every line is still prefixed with the VM name it concerns.
  local idx pids=()
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    create_vm "$idx" &
    pids+=($!)
  done
  local rc=0 pid
  for pid in "${pids[@]}"; do
    # A failed job must not abort the wait loop for the others (set -e is
    # suspended inside `wait || ...`), but it must fail the step overall.
    wait "$pid" || rc=1
  done
  return "$rc"
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
  # `tailscale ip` talks to tailscaled's local API socket, which is root-only
  # by default -- SSH_USER is unprivileged, so go through sudo (NOPASSWD via
  # Kairos's admin group). Without this the probe returns empty forever and
  # every node "times out" despite being on the tailnet.
  for _ in $(seq 1 "$TAILSCALE_WAIT_ATTEMPTS"); do
    ts_ip="$(ssh_run "$ip" "sudo tailscale ip -4 2>/dev/null" 2>/dev/null | tr -d '\r')"
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

# The single control-plane address every in-cluster component dials.
#
# This MUST be set on a multi-controller cluster, and it must be an address
# on the internal network. k0s's konnectivity agents (the tunnel the API
# server uses to reach kubelets for logs/exec/port-forward) all connect to
# exactly ONE address: spec.api.externalAddress when set, otherwise whichever
# controller's own spec.api.address won the race to write the DaemonSet.
# Leaving it unset on a 3-controller cluster therefore pins all agents to an
# arbitrary controller -- and if kubectl then talks to a *different* one, that
# API server's konnectivity server has zero agents registered and every
# proxied call fails with "No agent available" while `kubectl get` (which
# never leaves etcd) keeps working and hides the problem. Upstream tracks the
# single-address limitation in k0sproject/k0s#600 and #5503.
#
# Defaults to the first controller's internal DHCP address, which keeps all
# node-to-node traffic on the libvirt network. The Tailscale addresses are
# deliberately NOT used here -- they are an external kubectl entry point only
# (added as SANs, and used for the kubeconfig's server address), so the
# tailnet never carries in-cluster traffic and konnectivity's port never needs
# to be exposed on it.
#
# Set explicitly to point at a real load balancer or a k0s CPLB/keepalived VIP
# once one exists; a VIP is the HA upgrade path here, since pinning to one
# controller means losing kubectl (though not etcd quorum) if that node dies.
API_EXTERNAL_ADDRESS="${API_EXTERNAL_ADDRESS:-}"

# Address the generated kubeconfig points `server:` at. Defaults to the
# Tailscale IP of the same node API_EXTERNAL_ADDRESS resolves to -- the same
# node on purpose, so kubectl lands on the one API server whose konnectivity
# server actually holds the agent connections. Override to use a DNS name you
# manage instead (make sure it is also in EXTRA_SANS).
KUBECONFIG_SERVER="${KUBECONFIG_SERVER:-}"

generate_k0sctl_config() {
  local roles=($NODE_ROLES)
  local hosts="" sans=""
  # Internal + Tailscale address of the first *controller*, which becomes the
  # cluster's single control-plane entry point (see API_EXTERNAL_ADDRESS).
  local cp_ip="" cp_ts_ip=""
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
    # First controller wins: everything in-cluster dials its internal address,
    # and the kubeconfig points at its tailnet address, so kubectl and the
    # konnectivity agents converge on the same API server.
    if [[ -z "$cp_ip" && "$role" == controller* ]]; then
      cp_ip="$ip"
      cp_ts_ip="$ts_ip"
    fi
    hosts+="  - role: $role"$'\n'
    # Hadron core ships no k0s, and its minimal rootfs isn't guaranteed to
    # carry a download tool -- so don't let k0sctl make each host fetch the
    # binary itself. With uploadBinary, k0sctl downloads K0S_VERSION on the
    # machine running it and pushes the binary over the same SSH connection.
    hosts+="    uploadBinary: true"$'\n'
    # See K0SCTL_OS_OVERRIDE above: without this k0sctl aborts at its
    # "Detect host operating systems" phase with "unsupported OS: hadron".
    hosts+="    os: $K0SCTL_OS_OVERRIDE"$'\n'
    # k0s taints controller+worker nodes `node-role.kubernetes.io/control-plane:
    # NoSchedule` by default. NO_TAINTS defaults to true because the default
    # topology's only worker is reserved for image builds -- lifting the taint
    # is what lets everything else (kairos-operator, local-path, banlieue)
    # schedule on the controllers. Set NO_TAINTS=false when you have
    # general-purpose workers that can carry those workloads instead.
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
    log "No controller found in NODE_ROLES=$NODE_ROLES and no API_EXTERNAL_ADDRESS set"
    exit 1
  fi
  # externalAddress has to be in the cert too -- k0s adds it automatically, but
  # spell it out so a hand-set API_EXTERNAL_ADDRESS (VIP, LB, DNS name) is
  # covered without the caller also having to remember to add it to EXTRA_SANS.
  case $'\n'"$sans" in
    *$'\n'"            - $api_external"$'\n'*) ;;
    *) sans+="            - $api_external"$'\n' ;;
  esac

  # Persist the address the kubeconfig should use, for fetch_kubeconfig (a
  # separate invocation, so it can't just read a local variable here).
  # Preference: explicit override -> the control-plane node's Tailscale IP ->
  # its internal address (no tailnet in play at all).
  echo "${KUBECONFIG_SERVER:-${cp_ts_ip:-$api_external}}" >"$KUBECONFIG_SERVER_FILE"

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
    # Pinned here because the Hadron core image carries no k0s -- k0sctl
    # installs exactly this version on every host (see uploadBinary above).
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

# k0sctl brings its own SSH stack (the `rig` library) and consults
# ~/.ssh/known_hosts directly. It does NOT honour the StrictHostKeyChecking=no
# / UserKnownHostsFile=/dev/null that ssh_run sets for our own SSH, and it
# exposes no flag to relax the check (verified against k0sctl v0.32.1's
# `apply --help`). libvirt recycles its DHCP pool across destroy/create
# cycles, so a freshly built VM routinely lands on an address that a
# *previous* VM's host key is still pinned to, and k0sctl then aborts the
# entire run after a single attempt:
#
#   ssh: handshake failed: host key mismatch: knownhosts: key mismatch
#
# Drop the pins for exactly the addresses we are about to touch. Targeted, and
# much better than turning host-key verification off globally. `ssh-keygen -R`
# handles hashed known_hosts entries correctly, which a grep-based purge would
# not.
purge_known_hosts() {
  local ip
  for ip in "$@"; do
    [[ -n "$ip" ]] || continue
    ssh-keygen -R "$ip" >/dev/null 2>&1 || true
  done
}

apply_k0sctl() {
  local ips
  # Read the addresses back out of the generated config so this works even
  # when `apply` is invoked on its own, without a `config` step in the same run.
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

  # k0sctl emits the in-cluster address (spec.api.externalAddress), which is on
  # the libvirt network and unreachable from outside the hypervisor. Rewrite
  # only the `server:` line -- a blanket search/replace of the address would
  # also rewrite it anywhere else it legitimately appears.
  local target=""
  [[ -s "$KUBECONFIG_SERVER_FILE" ]] && target="$(<"$KUBECONFIG_SERVER_FILE")"
  if [[ -n "$target" ]]; then
    log "Pointing kubeconfig server at $target"
    sed -i.bak -E \
      "s#^([[:space:]]*server: https://).*:([0-9]+)[[:space:]]*\$#\1${target}:\2#" \
      "$KUBECONFIG_OUT"
    rm -f "$KUBECONFIG_OUT.bak"
  else
    log "No $KUBECONFIG_SERVER_FILE (run '$0 config' first) -- leaving the" \
        "in-cluster server address as-is; kubectl will only work from a host" \
        "that can route to the libvirt network"
  fi

  grep -E '^\s*server:' "$KUBECONFIG_OUT" >&2 || true
  log "export KUBECONFIG=$KUBECONFIG_OUT"
}

# Dedicate one worker to image builds: label + NoSchedule taint, so only pods
# that explicitly tolerate the taint can be scheduled there. Runs against the
# fetched kubeconfig, so the node name is the VM hostname as registered by the
# kubelet. --overwrite on both makes re-runs idempotent.
#
# NOTE: the label/taint alone only RESERVES the node -- kairos-operator's
# builder pods must also be given the matching toleration (and ideally a
# nodeSelector on banlieue.io/imagebuild=true) or they will never land here.
label_imagebuild_node() {
  local node="$IMAGEBUILD_NODE"
  if [[ -z "$node" ]]; then
    # Default: the LAST NODE_ROLES entry that is exactly "worker" (with the
    # default roles that is k0s-04, the cluster's only worker).
    local roles=($NODE_ROLES) idx
    for idx in $(seq 0 $((VM_COUNT - 1))); do
      [[ "${roles[$idx]:-worker}" == "worker" ]] && node="$(vm_name "$idx")"
    done
  fi
  if [[ -z "$node" ]]; then
    log "No pure worker in NODE_ROLES='$NODE_ROLES' and IMAGEBUILD_NODE unset -- skipping"
    return 0
  fi

  # Right after `apply`, the API servers may still be restarting from the
  # config change k0sctl just installed -- the first kubectl call can hit
  # "connection refused". Wait for the API to answer before touching it.
  log "Waiting for the API server to answer..."
  for _ in $(seq 1 24); do
    kubectl --kubeconfig "$KUBECONFIG_OUT" get --raw=/readyz >/dev/null 2>&1 && break
    sleep 5
  done

  log "Dedicating worker $node to image builds"
  kubectl --kubeconfig "$KUBECONFIG_OUT" label node "$node" \
    banlieue.io/imagebuild=true --overwrite
  kubectl --kubeconfig "$KUBECONFIG_OUT" taint node "$node" \
    dedicated=imagebuild:NoSchedule --overwrite
}

# Delete the VMs' tailnet devices via the Tailscale admin API (node keys can't
# delete devices -- only an API access token can). Failures here are warnings,
# not fatal: the VMs are already gone, which is the point of `destroy`.
# Uses python3 (always present on the Debian-ish hosts this script targets)
# instead of jq for the JSON parsing.
tailscale_remove_devices() {
  if [[ -z "$TAILSCALE_API_KEY" ]]; then
    log "TAILSCALE_API_KEY not set -- leaving any $VM_PREFIX-* tailnet devices behind;"
    log "  delete them in the admin console or set TAILSCALE_API_KEY"
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
        print(d["id"])
        break
PY
)"
    if [[ -z "$id" ]]; then
      log "No tailnet device named $name -- nothing to delete"
      continue
    fi
    log "Deleting tailnet device $name ($id)"
    curl -fsSL -X DELETE -u "$TAILSCALE_API_KEY:" \
      "https://api.tailscale.com/api/v2/device/$id" >/dev/null \
      || log "FAILED to delete tailnet device $name -- remove it in the admin console"
  done
}

destroy_all() {
  local failed=0
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    local name
    name="$(vm_name "$idx")"
    if virsh --connect "$LIBVIRT_URI" dominfo "$name" >/dev/null 2>&1; then
      log "Destroying VM $name"
      # `destroy` is a forced power-off. A domain that is already shut off
      # errors here, and that one really is uninteresting -- but note it only
      # powers the domain OFF; `undefine` below is what actually removes it.
      virsh --connect "$LIBVIRT_URI" destroy "$name" >/dev/null 2>&1 || true

      # --nvram: create_vm builds these with `--boot uefi`, and libvirt refuses
      #   to undefine a domain carrying an <nvram> varstore unless told what to
      #   do with it ("cannot undefine domain with nvram").
      # --managed-save: same class of guard for a saved-state file.
      # NOT --remove-all-storage, deliberately: the disks live in a
      #   subdirectory of the libvirt pool and are not pool-managed volumes
      #   (`vol-list default` lists only the k0s-bootstrap directory itself),
      #   so libvirt cannot resolve them and the entire undefine fails. The
      #   qcow2 and seed ISO are removed directly below instead.
      # Errors are NOT swallowed: the previous version sent both of these to
      # /dev/null with `|| true`, so a failed undefine looked like a clean
      # teardown -- and since create_vm reuses any still-defined domain, the
      # next `apply` silently came back up on the old VMs.
      if ! virsh --connect "$LIBVIRT_URI" undefine "$name" --nvram --managed-save; then
        log "FAILED to undefine $name -- it will still appear in virsh/Cockpit"
        failed=1
      fi
    fi
    rm -f "$POOL_DIR/$name.qcow2" "$POOL_DIR/$name-seed.iso"
    rm -rf "$POOL_DIR/$name-seed"
  done
  rm -f "$K0SCTL_CONFIG" "$KUBECONFIG_OUT" "$TAILSCALE_IP_MAP" "$KUBECONFIG_SERVER_FILE"

  # The VMs are gone; their tailnet device entries don't disappear with them.
  tailscale_remove_devices

  # Verify instead of assuming. Teardown that quietly half-succeeds is worse
  # than one that fails loudly, because the next bootstrap inherits the
  # leftovers.
  local leftover
  leftover="$(virsh --connect "$LIBVIRT_URI" list --all --name 2>/dev/null \
    | grep -E "^${VM_PREFIX}-[0-9]+\$" || true)"
  if [[ -n "$leftover" ]]; then
    log "Domains still defined after destroy: $(echo "$leftover" | tr '\n' ' ')"
    exit 1
  fi
  if [[ "$failed" -ne 0 ]]; then
    exit 1
  fi
  log "All $VM_PREFIX-* domains removed"
}

main() {
  local cmd="${1:-all}"
  case "$cmd" in
    vms)
      check_deps
      fetch_installer_iso
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
    label)
      check_deps
      label_imagebuild_node
      ;;
    destroy)
      destroy_all
      ;;
    all)
      check_deps
      fetch_installer_iso
      create_vms
      generate_k0sctl_config
      apply_k0sctl
      fetch_kubeconfig
      label_imagebuild_node
      ;;
    *)
      echo "Usage: $0 [all|vms|config|apply|kubeconfig|label|destroy]" >&2
      exit 1
      ;;
  esac
}

main "$@"
