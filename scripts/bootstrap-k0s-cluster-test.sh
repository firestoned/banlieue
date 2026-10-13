#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Tests for scripts/bootstrap-k0s-cluster.sh: the pure parts of the Cloud
# Hypervisor node support and of the `join` step (ADR-0099), workers and
# controllers. The script is
# sourced, not run, so nothing here touches libvirt, systemd, sudo or SSH.
#
# Usage: scripts/bootstrap-k0s-cluster-test.sh

# Variables set here are read by the sourced script's functions, which the
# linter cannot follow through a computed path; the expect_* helpers are
# deliberate A && B || C one-liners; stubs that replace the script's
# functions are called by those functions. Kept before the first command,
# so these apply to the whole file.
# shellcheck disable=SC2034,SC2015,SC2016,SC1091,SC2329
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bootstrap-k0s-cluster.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Keep sourcing side-effect free: no virsh lookup, no osinfo probe, scratch dirs.
export WORKDIR="$TMP/work" POOL_DIR="$TMP/pool" OS_VARIANT=generic
export SSH_PUBKEY="$TMP/id.pub"
echo "ssh-ed25519 AAAATEST test@bar.foo.io" >"$SSH_PUBKEY"

# shellcheck source=bootstrap-k0s-cluster.sh
source "$SCRIPT"
set +e

failures=0
ok()   { echo "ok   $1"; }
fail() { echo "FAIL $1${2:+: $2}"; failures=$((failures + 1)); }

# expect_eq <case> <actual> <expected>
expect_eq() { [[ "$2" == "$3" ]] && ok "$1" || fail "$1" "got '$2', want '$3'"; }
# expect_has <case> <haystack> <needle>
expect_has() { [[ "$2" == *"$3"* ]] && ok "$1" || fail "$1" "missing '$3'"; }
# expect_lacks <case> <haystack> <needle>
expect_lacks() { [[ "$2" != *"$3"* ]] && ok "$1" || fail "$1" "unexpected '$3'"; }
# expect_refused <case> <command...>: the command must exit non-zero.
expect_refused() {
  local name="$1"; shift
  if ( "$@" ) >/dev/null 2>&1; then fail "$name" "was accepted"; else ok "$name"; fi
}
# expect_accepted <case> <command...>
expect_accepted() {
  local name="$1"; shift
  if ( "$@" ) >/dev/null 2>&1; then ok "$name"; else fail "$name" "was refused"; fi
}

# --- per-node VMM ------------------------------------------------------------
VM_COUNT=4 NODE_VMMS=""
expect_eq "VMM defaults to libvirt" "$(node_vmm 3)" "libvirt"
NODE_VMMS="libvirt libvirt libvirt cloud-hypervisor"
expect_eq "VMM read per node" "$(node_vmm 3)" "cloud-hypervisor"
expect_eq "VMM of an earlier node" "$(node_vmm 0)" "libvirt"
expect_accepted "a node uses cloud-hypervisor" any_node_vmm cloud-hypervisor
NODE_VMMS="libvirt"
expect_eq "missing entries default to libvirt" "$(node_vmm 3)" "libvirt"
expect_refused "no node uses cloud-hypervisor" any_node_vmm cloud-hypervisor
NODE_VMMS="libvirt qemu"
expect_refused "unknown VMM refused" validate_node_vmms
NODE_VMMS="libvirt libvirt libvirt libvirt cloud-hypervisor"
expect_refused "more VMMs than nodes refused" validate_node_vmms
NODE_VMMS="libvirt libvirt libvirt cloud-hypervisor"
expect_accepted "valid VMM list accepted" validate_node_vmms

# --- tap name and MAC ----------------------------------------------------------
expect_eq "tap name for a short node name" "$(ch_tap_name core-04)" "kv-core-04"
long="$(ch_tap_name a-very-long-node-name-01)"
[[ ${#long} -le 15 ]] && ok "long tap name fits IFNAMSIZ" || fail "long tap name fits IFNAMSIZ" "$long"
expect_eq "long tap name is stable" "$(ch_tap_name a-very-long-node-name-01)" "$long"
[[ "$(ch_tap_name a-very-long-node-name-01)" != "$(ch_tap_name a-very-long-node-name-02)" ]] \
  && ok "long tap names differ" || fail "long tap names differ"

VM_MACS="52:54:00:00:00:01 52:54:00:00:00:02 52:54:00:00:00:03 52:54:00:00:00:04"
expect_eq "MAC taken from VM_MACS" "$(ch_node_mac 3 core-04)" "52:54:00:00:00:04"
VM_MACS="" CLUSTER_NAME=core
derived="$(ch_node_mac 3 core-04)"
[[ "$derived" =~ ^52:54:00(:[0-9a-f]{2}){3}$ ]] && ok "derived MAC is well formed" || fail "derived MAC is well formed" "$derived"
expect_eq "derived MAC is stable" "$(ch_node_mac 3 core-04)" "$derived"
[[ "$(ch_node_mac 4 core-05)" != "$derived" ]] && ok "derived MACs differ per node" || fail "derived MACs differ per node"

# --- neighbour table -----------------------------------------------------------
neigh='192.0.2.20 lladdr 52:54:00:aa:bb:01 REACHABLE
192.0.2.21 lladdr 52:54:00:AA:BB:04 STALE
192.0.2.22 FAILED
192.0.2.23 lladdr 52:54:00:aa:bb:05 FAILED'
expect_eq "neighbour IP found by MAC, any case" "$(parse_neigh_ip 52:54:00:aa:bb:04 <<<"$neigh")" "192.0.2.21"
expect_eq "failed neighbour entry ignored" "$(parse_neigh_ip 52:54:00:aa:bb:05 <<<"$neigh")" ""
expect_eq "unknown MAC yields nothing" "$(parse_neigh_ip 52:54:00:aa:bb:99 <<<"$neigh")" ""
# A reinstalled node keeps its MAC, so the installer's old lease can linger as
# a STALE entry beside the live one: the live one wins, wherever it sits.
neigh='192.0.2.30 lladdr 52:54:00:aa:bb:06 STALE
192.0.2.31 lladdr 52:54:00:aa:bb:06 REACHABLE
192.0.2.32 lladdr 52:54:00:aa:bb:07 STALE
192.0.2.33 lladdr 52:54:00:aa:bb:07 DELAY'
expect_eq "live entry beats a stale one listed first" "$(parse_neigh_ip 52:54:00:aa:bb:06 <<<"$neigh")" "192.0.2.31"
expect_eq "DELAY counts as live" "$(parse_neigh_ip 52:54:00:aa:bb:07 <<<"$neigh")" "192.0.2.33"

# --- unit rendering ------------------------------------------------------------
CH_VM_USER=k0s-vm CH_BRIDGE=br0 CH_VM_DIR=/srv/k0s-ch CH_TPM=true VCPUS=8 MEM_MB=10240
CH_BINARY=/usr/local/bin/cloud-hypervisor CH_REMOTE=/usr/local/bin/ch-remote
CH_FIRMWARE=/opt/fw/CLOUDHV.fd SWTPM=/usr/bin/swtpm
VM_MACS="52:54:00:00:00:01 52:54:00:00:00:02 52:54:00:00:00:03 52:54:00:00:00:04"

vm_unit="$(render_ch_vm_unit core-04 3)"
expect_has "VMM runs as the VM user" "$vm_unit" "User=k0s-vm"
expect_has "VMM in the kvm group" "$vm_unit" "SupplementaryGroups=kvm"
expect_has "vCPUs from VCPUS" "$vm_unit" "--cpus boot=8"
expect_has "memory from MEM_MB" "$vm_unit" "--memory size=10240M"
expect_has "firmware from CH_FIRMWARE" "$vm_unit" "--firmware /opt/fw/CLOUDHV.fd"
expect_has "OS disk typed qcow2" "$vm_unit" "path=/srv/k0s-ch/core-04/disk.qcow2,image_type=qcow2"
expect_has "seed attached read-only raw" "$vm_unit" "path=/srv/k0s-ch/core-04/seed.iso,readonly=on,image_type=raw"
expect_has "installer disk is an unbraced env word" "$vm_unit" ' $K0S_VM_INSTALL_DISK '
expect_has "tap and fixed MAC" "$vm_unit" "--net tap=kv-core-04,mac=52:54:00:00:00:04"
expect_has "vTPM socket" "$vm_unit" "--tpm socket=/run/k0s-vm-core-04/swtpm.sock"
expect_has "needs the swtpm unit" "$vm_unit" "Requires=k0s-vm-core-04-swtpm.service"
expect_has "bound to its tap unit" "$vm_unit" "BindsTo=k0s-vm-core-04-net.service"
expect_has "starts at boot" "$vm_unit" "WantedBy=multi-user.target"
expect_has "no new privileges" "$vm_unit" "NoNewPrivileges=yes"
expect_has "devices closed" "$vm_unit" "DevicePolicy=closed"
expect_has "kvm device allowed" "$vm_unit" "DeviceAllow=/dev/kvm rw"
expect_has "writes only its own dirs" "$vm_unit" "ReadWritePaths=/srv/k0s-ch/core-04 /run/k0s-vm-core-04"
expect_has "Landlock on" "$vm_unit" "--landlock"
expect_has "Landlock reads only its own tap in sysfs" "$vm_unit" "--landlock-rules path=/sys/class/net/kv-core-04,access=r"
expect_has "restarts are paced" "$vm_unit" "RestartSec=5s"
expect_has "runtime dir private" "$vm_unit" "RuntimeDirectoryMode=0700"
expect_has "sockets private" "$vm_unit" "UMask=0077"
expect_has "stop presses the power button" "$vm_unit" "power-button"
expect_has "stop waits for the guest" "$vm_unit" 'while kill -0 $MAINPID'
CH_TPM=false
vm_unit="$(render_ch_vm_unit core-04 3)"
expect_lacks "no vTPM when CH_TPM=false" "$vm_unit" "--tpm"
expect_lacks "no swtpm dependency when CH_TPM=false" "$vm_unit" "swtpm.service"
CH_TPM=true

net_unit="$(render_ch_net_unit core-04)"
expect_has "tap owned by the VM user" "$net_unit" "tuntap add dev kv-core-04 mode tap user k0s-vm group k0s-vm"
expect_has "tap enslaved to the bridge" "$net_unit" "link set kv-core-04 master br0 up"
expect_has "tap removed on stop" "$net_unit" "link del kv-core-04"
expect_has "waits for the bridge device" "$net_unit" "BindsTo=sys-subsystem-net-devices-br0.device"
expect_has "ordered after the network is online" "$net_unit" "After=network-online.target"

swtpm_unit="$(render_ch_swtpm_unit core-04)"
expect_has "swtpm runs as the VM user" "$swtpm_unit" "User=k0s-vm"
expect_has "swtpm state in the node dir" "$swtpm_unit" "--tpmstate dir=/srv/k0s-ch/core-04/tpm"
expect_has "swtpm stops with the VM" "$swtpm_unit" "PartOf=k0s-vm-core-04.service"
expect_has "swtpm runtime dir private" "$swtpm_unit" "RuntimeDirectoryMode=0700"
expect_has "swtpm socket private" "$swtpm_unit" "UMask=0077"

# --- Cloud Hypervisor settings guard -------------------------------------------
LIBVIRT_URI=qemu:///system
expect_accepted "complete CH settings accepted" validate_ch_node core-04
CH_FIRMWARE=""
expect_refused "no firmware refused" validate_ch_node core-04
CH_FIRMWARE=/opt/fw/CLOUDHV.fd
CH_BRIDGE=""
expect_refused "no bridge refused" validate_ch_node core-04
CH_BRIDGE=br0
LIBVIRT_URI=qemu+tls://bar.foo.io/system
expect_refused "remote libvirt URI refused for CH nodes" validate_ch_node core-04
LIBVIRT_URI=qemu:///system
expect_refused "unsafe node name refused" validate_ch_node 'core 04;rm'

# --- join targets ----------------------------------------------------------------
NODE_TABLE=("core-01|controller+worker|192.0.2.11" "core-02|controller+worker|192.0.2.12" "core-04|worker|192.0.2.14")
JOIN_NODES="core-04"
expect_eq "join picks the named worker" "$(join_targets)" "core-04|worker|192.0.2.14"
JOIN_NODES="core-01"
expect_eq "join picks a controller" "$(join_targets)" "core-01|controller+worker|192.0.2.11"
JOIN_NODES="core-04 core-01"
expect_eq "join keeps JOIN_NODES order" "$(join_targets | cut -d'|' -f1 | tr '\n' ' ')" "core-04 core-01 "
JOIN_NODES="core-09"
expect_refused "join refuses an unknown node" join_targets
JOIN_NODES=""
expect_refused "join needs JOIN_NODES" join_targets
JOIN_CONTROLLER=""
expect_eq "token from the first controller" "$(join_controller_ip)" "192.0.2.11"
JOIN_CONTROLLER=192.0.2.12
expect_eq "JOIN_CONTROLLER overrides" "$(join_controller_ip)" "192.0.2.12"

# --- controller join: flags cloned from the running controller ---------------
src="/usr/local/bin/k0s controller --config=/etc/k0s/k0s.yaml --data-dir=/var/lib/k0s --enable-worker=true --no-taints=true --disable-components=konnectivity-server"
NO_TAINTS=true
expect_eq "controller+worker keeps the source's flags" "$(controller_install_flags "$src" controller+worker)" \
  "--config=/etc/k0s/k0s.yaml --data-dir=/var/lib/k0s --disable-components=konnectivity-server --enable-worker --no-taints"
expect_eq "a plain controller runs no kubelet" "$(controller_install_flags "$src" controller)" \
  "--config=/etc/k0s/k0s.yaml --data-dir=/var/lib/k0s --disable-components=konnectivity-server"
NO_TAINTS=false
expect_eq "NO_TAINTS=false keeps the taint" "$(controller_install_flags "$src" controller+worker)" \
  "--config=/etc/k0s/k0s.yaml --data-dir=/var/lib/k0s --disable-components=konnectivity-server --enable-worker"
NO_TAINTS=true
expect_eq "the source's own token file is dropped, both spellings" \
  "$(controller_install_flags "/opt/k0s/k0s-v1-amd64 controller --token-file /etc/k0s/t --token-file=/etc/k0s/t -c /etc/k0s/k0s.yaml" controller)" \
  "-c /etc/k0s/k0s.yaml"
expect_eq "worker flags of another spelling are dropped" \
  "$(controller_install_flags "k0s controller --enable-worker --no-taints" controller+worker)" "--enable-worker --no-taints"
expect_refused "a --single controller cannot be joined" controller_install_flags "k0s controller --single" controller
expect_refused "a unit that is not a controller is refused" controller_install_flags "k0s worker --token-file=/x" controller
expect_refused "quoted flags are refused" controller_install_flags "k0s controller --labels='a=b c'" controller
# A converted worker's Node outlives its reset, and kubelet sets a Node's
# labels only when it registers it: the controller would come up unlabelled.
fake_cluster() {
  ssh_run() {
    case "$2" in
      *is-active*) return 1 ;;
      *"systemctl cat k0scontroller"*) echo "ExecStart=/usr/local/bin/k0s controller --config=/etc/k0s/k0s.yaml" ;;
      *"get node core-04"*) [[ -n "$NODE_EXISTS" ]] ;;
      *) return 1 ;;
    esac
  }
  stage_k0s_binary() { echo STAGED; exit 0; }
}
staged="$( NODE_EXISTS=1; fake_cluster; join_controller_node 192.0.2.11 core-04 controller+worker 192.0.2.14 /opt/k0s/k0s 2>/dev/null )"
expect_lacks "a controller is not joined over an existing Node" "$staged" "STAGED"
staged="$( NODE_EXISTS=""; fake_cluster; join_controller_node 192.0.2.11 core-04 controller+worker 192.0.2.14 /opt/k0s/k0s 2>/dev/null )"
expect_has "a controller with no Node yet goes ahead" "$staged" "STAGED"
expect_has "the clone skips token files" "$K0S_CONFIG_CLONE_EXCLUDES" "--exclude=*token-file"
expect_has "the clone skips the worker's generated containerd config" "$K0S_CONFIG_CLONE_EXCLUDES" "--exclude=containerd.toml"

# --- an interrupted CH install is cleared and redone ---------------------------
# Stubs: units exist; the installed marker exists only when INSTALLED is set.
# The run stops at the VM user (ensure_ch_vm_user), after the decision under
# test and before anything is built.
fake_ch_host() {
  sudo() { case "$*" in *"$CH_INSTALLED_MARKER"*) [[ -n "$INSTALLED" ]] ;; "test -f "*) return 0 ;; *) echo "SUDO $*" ;; esac; }
  destroy_ch_node() { echo "DESTROYED $1"; }
  ch_bridge_addr() { echo 192.0.2.1; }
  ensure_ch_vm_user() { echo "REBUILT"; exit 0; }
}
VM_PREFIX=core
out="$( INSTALLED=""; fake_ch_host; create_vm_ch 0 2>/dev/null )"
expect_has "a half-installed node is destroyed" "$out" "DESTROYED 0"
expect_has "and built again" "$out" "REBUILT"
out="$( INSTALLED=1; fake_ch_host; create_vm_ch 0 2>/dev/null )"
expect_lacks "an installed node is not destroyed" "$out" "DESTROYED"
expect_has "an installed node is started" "$out" "systemctl start"

# --- retired nodes keep their position but are skipped ----------------------
VM_COUNT=4 VM_PREFIX=core NODE_VMMS="" NODE_ROLES="controller+worker controller+worker controller+worker controller+worker"
RETIRED_NODES="core-01"
expect_accepted "a retired node is retired" node_retired 0
expect_refused "other nodes are not" node_retired 1
created="$( ensure_pool() { :; }; create_vm() { vm_name "$1"; echo; }; create_vms_libvirt | tr '\n' ' ' )"
expect_eq "vms does not recreate a retired node" "$created" "core-02 core-03 core-04 "
table="$( wait_for_ip_libvirt() { echo "192.0.2.$((10 + ${1##*-}))"; }; wait_for_ssh() { :; }
  NODE_TABLE=(); populate_node_table; printf '%s ' "${NODE_TABLE[@]}" )"
expect_lacks "the node table skips a retired node" "$table" "core-01"
expect_has "the node table keeps the others" "$table" "core-02|controller+worker|192.0.2.12"
expect_eq "label sees no retired node" "$(node_name_roles | cut -d' ' -f1 | tr '\n' ' ')" "core-02 core-03 core-04 "
RETIRED_NODES=""

# --- seed: announce to the host ------------------------------------------------
SSH_USER=erick TAILSCALE_AUTHKEY="" LIBVIRT_IMAGE_KIND=kairos
make_cloud_init_kairos core-04 "$TMP/seed-k" 192.0.2.1
ud="$(cat "$TMP/seed-k/user-data")"
expect_has "kairos seed pings the host" "$ud" "ping -c 3 -W 1 192.0.2.1"
TAILSCALE_AUTHKEY=tskey-test
make_cloud_init_kairos core-04 "$TMP/seed-kt" 192.0.2.1
ud="$(cat "$TMP/seed-kt/user-data")"
[[ "$(grep -c '^  network:' <<<"$ud")" == 1 ]] && ok "one network stage with tailscale too" || fail "one network stage with tailscale too"
expect_has "tailscale still joins" "$ud" "tailscale up"
# The live installer boot runs the same seed; joining there leaves an
# offline device on the tailnet and pushes the node's name to <name>-1.
ts_stage="$(sed -n '/Install and join Tailscale/,/^write_files:/p' <<<"$ud")"
expect_has "tailscale skips the live installer" "$ts_stage" "[ ! -f /run/cos/live_mode ]"
expect_has "and the UKI installer" "$ts_stage" "[ ! -f /run/cos/uki_install_mode ]"
TAILSCALE_AUTHKEY=""
make_cloud_init_kairos core-01 "$TMP/seed-kl"
expect_lacks "libvirt seed has no announce" "$(cat "$TMP/seed-kl/user-data")" "ping -c 3"
make_cloud_init_cloud core-04 "$TMP/seed-c" 192.0.2.1
expect_has "cloud seed announces every boot" "$(cat "$TMP/seed-c/user-data")" "k0s-vm-announce.service"

# --- usage -------------------------------------------------------------------------
expect_has "usage lists join" "$(main bogus 2>&1)" "join"
expect_has "usage says join takes controllers" "$(main bogus 2>&1)" "controllers"

if ((failures)); then
  echo "$failures failure(s)"
  exit 1
fi
echo "all passed"
