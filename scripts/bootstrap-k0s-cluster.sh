#!/usr/bin/env bash
# Bootstraps the k0s MANAGEMENT cluster that runs the banlieue controllers and
# kairos-operator, then installs k0s onto its nodes with k0sctl. This is the
# substrate `banlieue bootstrap` (ADR-0013) later installs onto -- it is NOT a
# workload provisioned through banlieue itself.
#
# Two provisioning backends, selected with BACKEND (ADR-0017):
#
#   BACKEND=libvirt  (default) -- create Kairos "Hadron" VMs on a KVM host,
#       let Kairos install itself from its ISO onto an empty disk, then
#       k0sctl. Run on the hypervisor host.
#
#       Each node runs under Cloud Hypervisor by default (ADR-0099): a plain
#       CH VM run by three systemd units on THIS host (no banlieue, no
#       libvirt domain), so it starts at boot like an autostarted domain. It
#       needs passwordless sudo, CH_FIRMWARE and a bridge with an address
#       (CH_BRIDGE, else LIBVIRT_BRIDGE, else LIBVIRT_NETWORK's bridge).
#       DEFAULT_VMM=libvirt makes every node a virt-install domain instead
#       (the earlier default; LIBVIRT_URI may then name a remote host), and
#       NODE_VMMS picks per node, so one cluster may mix both VMMs.
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
#   ./bootstrap-k0s-cluster.sh [all|vms|config|apply|kubeconfig|label|flux|join|destroy]
#   BANLIEUE_ENV_FILE=~/.k0s/banlieue.env BACKEND=vsphere ./bootstrap-k0s-cluster.sh all
#   ./bootstrap-k0s-cluster.sh --print-env-template   # scaffold for the vSphere env file
#
#   `join` adds the nodes named in JOIN_NODES to a cluster that is ALREADY
#   RUNNING, each in its NODE_ROLES role, with a 15-minute token from a
#   controller (ADR-0099). A new controller copies /etc/k0s and its install
#   flags from that running controller, not from this script's settings, so
#   it matches the cluster as it is now. `join` never writes to an existing
#   controller or uses the k0sctl config, so it is the safe way to grow (or
#   swap the nodes of) a cluster whose controllers have changed since
#   bootstrap; `config` + `apply` would push the generated config back onto
#   every controller. Typical: raise VM_COUNT, then `vms join`.
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
# A host bridge to attach the VMs to instead of a libvirt network, e.g. br0.
# The nodes then take addresses from the LAN's DHCP, so other machines can
# reach the API server without tailscale. libvirt has no lease for them, so
# their addresses are read from the guest agent (the Kairos-on-Debian image
# built by scripts/build-kairos-debian-iso.sh carries qemu-guest-agent), with
# the host's neighbour table as a fallback.
LIBVIRT_BRIDGE="${LIBVIRT_BRIDGE:-}"
# Space-separated MAC addresses, one per VM in order. Empty lets libvirt pick.
# Fixed MACs let a LAN DHCP server reserve each node's address, which a
# cluster needs: k0s does not survive its nodes changing address.
VM_MACS="${VM_MACS:-}"
# Space-separated names of nodes that were removed from the cluster but keep
# their position in NODE_ROLES, VM_MACS and NODE_VMMS, because nodes are
# named by position (ADR-0099). `vms` does not recreate them and no other
# step waits for them, so a cluster can swap its nodes one at a time.
RETIRED_NODES="${RETIRED_NODES:-}"
LIBVIRT_POOL="${LIBVIRT_POOL:-default}"

# --- per-node VMM (ADR-0099) ------------------------------------------------
# The VMM of every node NODE_VMMS does not name: `cloud-hypervisor` (the
# default) or `libvirt`. A cluster built when libvirt was the default sets
# DEFAULT_VMM=libvirt; every command refuses to run against one that does not
# (check_vmm_matches_host), so it is never doubled or half-destroyed.
DEFAULT_VMM="${DEFAULT_VMM:-cloud-hypervisor}"
# Space-separated, one per VM in order: `libvirt` or `cloud-hypervisor`.
# Missing entries (and an empty list) take DEFAULT_VMM. Example: a libvirt
# node beside two CH ones:
#   NODE_VMMS="cloud-hypervisor cloud-hypervisor libvirt"
NODE_VMMS="${NODE_VMMS:-}"
# Cloud Hypervisor nodes are created on THIS host through sudo (never through
# LIBVIRT_URI). Nothing below is guessed: a missing firmware or bridge is
# refused with a message.
CH_BINARY="${CH_BINARY:-/usr/local/bin/cloud-hypervisor}"
CH_REMOTE="${CH_REMOTE:-/usr/local/bin/ch-remote}"
# CLOUDHV.fd (rust-hypervisor-firmware's EDK2 build). `banlieue host
# cloud-hypervisor install` puts one under /opt/banlieue/firmware/<version>/.
CH_FIRMWARE="${CH_FIRMWARE:-}"
# The bridge CH nodes' taps join. It must carry a host address: the guest
# pings it on every boot so the host's neighbour table resolves the node.
# Empty with no LIBVIRT_BRIDGE: the bridge of LIBVIRT_NETWORK (virbr0 for
# libvirt's `default` network), whose dnsmasq hands out the addresses.
CH_BRIDGE="${CH_BRIDGE:-$LIBVIRT_BRIDGE}"
# Unprivileged system account (kvm group, no login) that runs every CH node's
# VMM and vTPM. Created on first use.
CH_VM_USER="${CH_VM_USER:-k0s-vm}"
# Give each CH node a vTPM, as virt-install does for a UEFI libvirt domain.
CH_TPM="${CH_TPM:-true}"
SWTPM="${SWTPM:-/usr/bin/swtpm}"
# The PCI slot every CH node's NIC is pinned to, so the guest always names it
# ens2. Left to CH, the slot follows the disks and moves when the installer
# disk is attached. Not a setting: every CH node gets the same name.
readonly CH_NIC_PCI_SLOT=2
CH_UNIT_DIR="${CH_UNIT_DIR:-/etc/systemd/system}"

# --- join (ADR-0099) ----------------------------------------------------------
# Space-separated node names to add to a running cluster, each in its
# NODE_ROLES role (worker, controller or controller+worker).
JOIN_NODES="${JOIN_NODES:-}"
# Controller address to mint the join token on, and that a joining
# controller copies its configuration from. Empty: the first controller in
# the node table.
JOIN_CONTROLLER="${JOIN_CONTROLLER:-}"
JOIN_TOKEN_EXPIRY="${JOIN_TOKEN_EXPIRY:-15m}"
# No osinfo entry exists for Kairos, so `generic` is the right fallback there.
# A distro cloud image DOES have one, and naming it gets the correct virtio
# defaults and clock/CPU tuning instead of virt-install's "VM performance may
# suffer" fallback. Resolved after LIBVIRT_IMAGE_KIND is known, further down.
OS_VARIANT="${OS_VARIANT:-}"
CLOUD_OS_VARIANT="${CLOUD_OS_VARIANT:-debian13}"
# Kairos Hadron CORE installer ISO -- deliberately NOT the `standard` flavor,
# which would bundle its own k0s/k3s at a version we don't control.
IMAGE_URL="${IMAGE_URL:-https://github.com/kairos-io/kairos/releases/download/v4.1.2/kairos-hadron-v0.4.0-core-amd64-generic-v4.1.2.iso}"
IP_WAIT_ATTEMPTS="${IP_WAIT_ATTEMPTS:-60}"   # 60 * 5s = 5min
SSH_WAIT_ATTEMPTS="${SSH_WAIT_ATTEMPTS:-60}" # 60 * 5s = 5min
# 5min. The Kairos path drops in a prebuilt tarball, but a distro cloud image
# installs tailscale through apt -- repo add, apt update, apt install -- which
# routinely outlasts a 2 minute budget on first boot.
TAILSCALE_WAIT_ATTEMPTS="${TAILSCALE_WAIT_ATTEMPTS:-60}" # 60 * 5s = 5min
INSTALL_WAIT_ATTEMPTS="${INSTALL_WAIT_ATTEMPTS:-180}" # 180 * 5s = 15min

# Set BASE_IMAGE_PATH to use a locally-downloaded Kairos installer ISO
# instead of fetching IMAGE_URL.
BASE_IMAGE_PATH="${BASE_IMAGE_PATH:-}"

# How the libvirt backend gets an OS onto a node (ADR-0017 covers the backend
# split; this is a choice WITHIN the libvirt backend):
#
#   kairos  Boot a Kairos installer ISO against an EMPTY disk and let Kairos
#           install itself, then power off, eject and reboot. Immutable, but
#           every node pays a full install -- minutes each, serialised behind
#           INSTALL_WAIT_ATTEMPTS.
#
#   cloud   Boot a prebuilt cloud image directly. cloud-init does first-boot
#           config from a NoCloud seed ISO. No install phase at all: nodes are
#           SSH-reachable in well under a minute. A mutable, familiar Debian.
#
# `cloud` is the faster path and the one to reach for when you want an
# ordinary distro; `kairos` remains the default so existing clusters and the
# imagebuilder's expectations are unchanged.
LIBVIRT_IMAGE_KIND="${LIBVIRT_IMAGE_KIND:-kairos}"

# Debian 13 "trixie" genericcloud: the slim cloud variant (~323MB). It carries
# cloud-init and virtio drivers but none of the bare-metal firmware the
# `generic` build ships, which is dead weight inside KVM. `nocloud` is
# deliberately NOT the default -- it has no cloud-init datasource, so the seed
# ISO below would be ignored and the node would come up with no user and no key.
CLOUD_IMAGE_URL="${CLOUD_IMAGE_URL:-https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2}"

# --- image integrity -------------------------------------------------------
# Downloaded images are executable content that becomes every node in the
# cluster, so they are verified before use -- on EVERY run, not just on the run
# that downloaded them. A cached file is exactly where a tampered image would
# hide, and re-hashing 300MB costs about a second.
#
#   auto (default)  fetch the checksum upstream publishes beside the image
#                   (Kairos: <asset>.sha256; Debian: SHA512SUMS) and compare
#   off             skip verification entirely -- for air-gapped mirrors that
#                   publish no checksum, and nothing else
#
# Setting IMAGE_SHA256 / CLOUD_IMAGE_SHA512 explicitly PINS the expected digest
# and takes precedence over `auto`. That is the strongest option: `auto` trusts
# whatever the same server serves for the checksum, so it catches corruption
# and a compromised mirror, but not a compromised origin. A pin in your own env
# file catches that too. Pin in production.
IMAGE_VERIFY="${IMAGE_VERIFY:-auto}"
IMAGE_SHA256="${IMAGE_SHA256:-}"
CLOUD_IMAGE_SHA512="${CLOUD_IMAGE_SHA512:-}"

# Kairos additionally publishes cosign (sigstore) artifacts: .sig/.pem/.bundle.
# Opt-in because it needs the cosign binary, and because verifying keylessly
# means asserting WHICH workflow identity is allowed to sign -- a decision that
# belongs to the operator, not to a default.
IMAGE_COSIGN_VERIFY="${IMAGE_COSIGN_VERIFY:-false}"
IMAGE_COSIGN_IDENTITY="${IMAGE_COSIGN_IDENTITY:-https://github.com/kairos-io/kairos/.*}"
IMAGE_COSIGN_ISSUER="${IMAGE_COSIGN_ISSUER:-https://token.actions.githubusercontent.com}"

# k0s version k0sctl installs on every node (no leading `v`, matching k0sctl's
# config convention).
K0S_VERSION="${K0S_VERSION:-1.35.5+k0s.0}"

# The controllers' datastore. `etcd` is k0s's default: an etcd member on each
# controller. `kine` keeps cluster state in an external SQL database instead,
# named by KINE_DATASOURCE, so the controllers hold no state of their own and
# the database's backups are the cluster's.
K0S_STORAGE_TYPE="${K0S_STORAGE_TYPE:-etcd}"
# kine only, e.g. postgres://k0s:<password>@db.example.com:5432/k0s?sslmode=require.
# It holds a password: set it in BANLIEUE_ENV_FILE, never on a command line.
# The generated k0sctl config is written 0600 because it carries it.
KINE_DATASOURCE="${KINE_DATASOURCE:-}"

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

# Disable the konnectivity server (default true, on both backends: ADR-0017 for
# vSphere, ADR-0086 for libvirt). On a flat, routable network the API server
# reaches kubelets (:10250) and pods directly for logs/exec/port-forward,
# webhooks and aggregated APIs such as metrics.k8s.io, so the tunnel is
# unnecessary. Kept on, its agents all dial externalAddress:8132 and so hold a
# connection to ONE controller's konnectivity server (a CPLB VIP does not
# change that: it balances only the API port). Any request that lands on
# another API server then fails with "No agent available" (k0s #600/#5503).
# Set false only if the controllers cannot reach the nodes directly.
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
# Scratch directory for downloaded ISOs and cloud-init seeds. Defaults to a
# subdirectory of whatever the `default` storage pool actually points at, NOT
# to libvirt's stock /var/lib/libvirt/images: bootstrap-libvirt-host.sh places
# pools on the filesystem with the most free space, because a stock Debian
# install leaves /var far too small for disk images. Hardcoding the stock path
# fails with a bare "Permission denied" on any host whose pool lives elsewhere.
_default_pool_dir() {
  local path
  path="$(LIBVIRT_DEFAULT_URI="${LIBVIRT_URI:-qemu:///system}" \
          virsh pool-dumpxml default 2>/dev/null \
          | sed -n 's:.*<path>\(.*\)</path>.*:\1:p' | head -1)"
  echo "${path:-/var/lib/libvirt/images}/k0s-bootstrap"
}
POOL_DIR="${POOL_DIR:-$(_default_pool_dir)}"
# Cloud Hypervisor nodes' disks, seeds and vTPM state, one directory per node,
# beside the libvirt pool so they land on the same large filesystem.
CH_VM_DIR="${CH_VM_DIR:-$(dirname "$POOL_DIR")/k0s-ch}"
INSTALL_ISO="$POOL_DIR/$(basename "${BASE_IMAGE_PATH:-$IMAGE_URL}")"
CLOUD_IMAGE="$POOL_DIR/$(basename "${BASE_IMAGE_PATH:-$CLOUD_IMAGE_URL}")"

# Fall back to `generic` if this libvirt's osinfo-db predates the named
# variant -- an unknown --os-variant is a hard error from virt-install, and a
# stale osinfo-db should not be fatal.
if [[ -z "$OS_VARIANT" ]]; then
  OS_VARIANT="generic"
  if [[ "$LIBVIRT_IMAGE_KIND" == "cloud" ]]; then
    # Capture first, then match. Piping straight into `grep -q` makes grep exit
    # at the first hit, which SIGPIPEs virt-install into exit 120 -- and under
    # `set -o pipefail` that is the pipeline's status, so the test ALWAYS failed
    # and silently fell back to `generic`.
    #
    # The list prints aliases comma-separated on one line ("debian13,
    # debiantrixie"), so an exact-line match never hits either.
    _osinfo="$(virt-install --osinfo list 2>/dev/null || true)"
    if grep -qE "(^|, )${CLOUD_OS_VARIANT}(,|\$)" <<<"$_osinfo"; then
      OS_VARIANT="$CLOUD_OS_VARIANT"
    fi
    unset _osinfo
  fi
fi
K0SCTL_CONFIG="${K0SCTL_CONFIG:-$WORKDIR/k0sctl.yaml}"
# Name the kubeconfig after the cluster rather than calling every one of them
# "kubeconfig". Bootstrapping a second cluster otherwise overwrites the first,
# and a directory of identically-named files tells you nothing about which is
# which. KUBECONFIG_NAME also becomes the cluster/context/user name inside the
# file, so several can be merged into ~/.kube/config without colliding.
KUBECONFIG_NAME="${KUBECONFIG_NAME:-$CLUSTER_NAME}"
KUBECONFIG_OUT="${KUBECONFIG_OUT:-$WORKDIR/$KUBECONFIG_NAME.kubeconfig}"
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

# ----------------------------------------------------------------------------
# Control plane VIP (ADR-0086)
# ----------------------------------------------------------------------------
# A virtual IP in CIDR form, e.g. 192.0.2.10/24, that k0s's control plane load
# balancing (keepalived VRRP + k0s's userspace reverse proxy) floats between
# the controllers. It becomes spec.api.externalAddress, a certificate SAN and
# the default kubeconfig server, so losing any one controller no longer takes
# the API away. It must be on the controllers' network, outside every DHCP
# range and MetalLB pool: k0s does no address management, and there is no
# default because any default would be a guess at somebody's LAN.
API_VIP="${API_VIP:-}"
# VRRP virtual router ID; must be unique on the broadcast domain.
API_VIP_ROUTER_ID="${API_VIP_ROUTER_ID:-51}"
# VRRP password, identical on every controller. Empty generates one on first
# use and keeps it in $WORKDIR so reruns don't change it. keepalived reads at
# most 8 characters. It prevents accidental collisions between clusters; it is
# not authentication.
API_VIP_AUTH_PASS="${API_VIP_AUTH_PASS:-}"
API_VIP_AUTH_PASS_FILE="${API_VIP_AUTH_PASS_FILE:-$WORKDIR/api-vip-authpass}"
readonly API_VIP_AUTH_PASS_MAX_LEN=8
# A multi-controller cluster without API_VIP is refused: its whole control
# plane would sit behind one controller's address. Set true only when that
# single entry point is intended (e.g. a disposable test cluster).
NO_API_VIP="${NO_API_VIP:-false}"

# api_vip_addr: API_VIP without its prefix length.
api_vip_addr() { printf '%s' "${API_VIP%/*}"; }

# api_vip_host_route: the VIP as a single-host route (/32 or /128), the form a
# Tailscale subnet route takes.
api_vip_host_route() {
  local addr; addr="$(api_vip_addr)"
  case "$addr" in
    *:*) printf '%s/128' "$addr" ;;
    *)   printf '%s/32' "$addr" ;;
  esac
}

# validate_api_vip: refuse a malformed API_VIP / API_VIP_AUTH_PASS before they
# reach a YAML file or a cloud-init seed.
validate_api_vip() {
  [[ -n "$API_VIP" ]] || return 0
  if [[ ! "$API_VIP" =~ ^[0-9A-Fa-f.:]+/[0-9]{1,3}$ ]]; then
    log "API_VIP must be an address with a prefix length, e.g. 192.0.2.10/24 (got '$API_VIP')"
    exit 1
  fi
  if [[ -n "$API_EXTERNAL_ADDRESS" && "$API_EXTERNAL_ADDRESS" != "$(api_vip_addr)" ]]; then
    log "API_VIP and API_EXTERNAL_ADDRESS disagree; with a VIP the external address IS the VIP, so unset API_EXTERNAL_ADDRESS"
    exit 1
  fi
  if [[ ! "$API_VIP_ROUTER_ID" =~ ^[0-9]+$ ]]; then
    log "API_VIP_ROUTER_ID must be a number (got '$API_VIP_ROUTER_ID')"
    exit 1
  fi
  if [[ -n "$API_VIP_AUTH_PASS" && ! "$API_VIP_AUTH_PASS" =~ ^[A-Za-z0-9]{1,8}$ ]]; then
    log "API_VIP_AUTH_PASS must be 1-$API_VIP_AUTH_PASS_MAX_LEN letters or digits"
    exit 1
  fi
}

# api_vip_auth_pass: prints the VRRP password, generating and persisting one
# (0600) on first use so every rerun renders the same value.
api_vip_auth_pass() {
  if [[ -n "$API_VIP_AUTH_PASS" ]]; then
    printf '%s' "$API_VIP_AUTH_PASS"
    return
  fi
  if [[ ! -s "$API_VIP_AUTH_PASS_FILE" ]]; then
    mkdir -p "$(dirname "$API_VIP_AUTH_PASS_FILE")"
    ( umask 077
      LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c "$API_VIP_AUTH_PASS_MAX_LEN" >"$API_VIP_AUTH_PASS_FILE" )
  fi
  cat "$API_VIP_AUTH_PASS_FILE"
}

# require_api_vip <controller-count>: the guard behind NO_API_VIP.
require_api_vip() {
  local controllers="$1"
  [[ -n "$API_VIP" || "$controllers" -le 1 || "$NO_API_VIP" == "true" ]] && return 0
  log "This cluster has $controllers controllers but no API_VIP (ADR-0086)."
  log "  Without one, every client and in-cluster component reaches the API through"
  log "  a single controller, and losing it loses the API."
  log "  Set API_VIP=<free LAN address>/<prefix>, outside DHCP and any MetalLB pool,"
  log "  or NO_API_VIP=true if one entry point is really what you want."
  exit 1
}

# render_cplb <indent>: the spec.network.controlPlaneLoadBalancing block, with
# every line prefixed by <indent>. Prints nothing without API_VIP.
render_cplb() {
  local pad="$1" pass
  [[ -n "$API_VIP" ]] || return 0
  pass="$(api_vip_auth_pass)"
  printf '%s\n' \
    "${pad}controlPlaneLoadBalancing:" \
    "${pad}  enabled: true" \
    "${pad}  type: Keepalived" \
    "${pad}  keepalived:" \
    "${pad}    vrrpInstances:" \
    "${pad}    - virtualIPs: [\"$API_VIP\"]" \
    "${pad}      authPass: \"$pass\"" \
    "${pad}      virtualRouterID: $API_VIP_ROUTER_ID"
}

# Tailscale flag that advertises the VIP as a subnet route from every node, so
# a client off the LAN reaches the VIP through whichever node is up. Empty
# without API_VIP.
tailscale_route_flag() {
  [[ -n "$API_VIP" ]] || return 0
  printf ' --advertise-routes=%s' "$(api_vip_host_route)"
}

# log_api_vip_followups: the steps a VIP needs that the script cannot take.
log_api_vip_followups() {
  [[ -n "$API_VIP" ]] || return 0
  log "Control plane VIP: $(api_vip_addr) (VRRP router id $API_VIP_ROUTER_ID; password in $API_VIP_AUTH_PASS_FILE unless API_VIP_AUTH_PASS is set)"
  log "  Reserve $(api_vip_addr) on the LAN: outside DHCP and every MetalLB pool."
  if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
    log "  Each node advertises $(api_vip_host_route) on the tailnet. Approve it once per node in"
    log "  the admin console (Machines -> <node> -> Edit route settings), or with an autoApprovers policy."
  fi
}

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
    node_retired "$idx" && continue
    echo "$(vm_name "$idx") ${roles[$idx]:-worker}"
  done
}

# node_retired <idx>: succeeds when VM <idx> is named in RETIRED_NODES.
node_retired() {
  local name retired
  name="$(vm_name "$1")"
  for retired in $RETIRED_NODES; do
    [[ "$retired" == "$name" ]] && return 0
  done
  return 1
}

# node_vmm <idx>: the VMM that runs VM <idx> (libvirt backend), from NODE_VMMS,
# else DEFAULT_VMM.
node_vmm() {
  local vmms
  read -r -a vmms <<<"$NODE_VMMS"
  printf '%s' "${vmms[$1]:-$DEFAULT_VMM}"
}

# any_node_vmm <vmm>: succeeds when at least one of the VM_COUNT nodes uses it.
any_node_vmm() {
  local idx
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    node_retired "$idx" && continue
    [[ "$(node_vmm "$idx")" == "$1" ]] && return 0
  done
  return 1
}

# validate_node_vmms: refuse an unknown VMM, or more entries than nodes (a
# list that does not line up with VM_COUNT is a typo, not a request).
validate_node_vmms() {
  local vmms v
  read -r -a vmms <<<"$NODE_VMMS"
  if ((${#vmms[@]} > VM_COUNT)); then
    log "NODE_VMMS has ${#vmms[@]} entries but VM_COUNT is $VM_COUNT"
    exit 1
  fi
  case "$DEFAULT_VMM" in
    libvirt|cloud-hypervisor) ;;
    *) log "DEFAULT_VMM must be 'libvirt' or 'cloud-hypervisor', not '$DEFAULT_VMM'"; exit 1 ;;
  esac
  for v in "${vmms[@]}"; do
    case "$v" in
      libvirt|cloud-hypervisor) ;;
      *) log "NODE_VMMS entries must be 'libvirt' or 'cloud-hypervisor', not '$v'"; exit 1 ;;
    esac
  done
}

# libvirt_domain_defined <name>: a libvirt domain of that name exists. False
# where virsh is not installed (a CH-only host).
libvirt_domain_defined() {
  command -v virsh >/dev/null 2>&1 || return 1
  virsh --connect "$LIBVIRT_URI" dominfo "$1" >/dev/null 2>&1
}

# ch_node_defined <name>: this host has that CH node's VMM unit.
ch_node_defined() {
  sudo -n test -f "$CH_UNIT_DIR/$(ch_unit "$1").service" 2>/dev/null
}

# check_vmm_matches_host: refuse when a node already exists on this host under
# the other VMM. Without it, a cluster built when libvirt was the default
# would get a CH node beside each libvirt domain (same name, same MAC) from
# `vms`, and `destroy` would report it removed while every domain still runs.
check_vmm_matches_host() {
  local idx name vmm
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    node_retired "$idx" && continue
    name="$(vm_name "$idx")"; vmm="$(node_vmm "$idx")"
    if [[ "$vmm" == "cloud-hypervisor" ]] && libvirt_domain_defined "$name"; then
      log "$name is a libvirt domain, but its VMM resolves to cloud-hypervisor."
      log "  A cluster built on libvirt sets DEFAULT_VMM=libvirt (or names it in NODE_VMMS)."
      exit 1
    fi
    if [[ "$vmm" == "libvirt" ]] && ch_node_defined "$name"; then
      log "$name is a Cloud Hypervisor node, but its VMM resolves to libvirt."
      log "  Name it cloud-hypervisor in NODE_VMMS, or unset DEFAULT_VMM=libvirt."
      exit 1
    fi
  done
}

# ============================================================================
# libvirt backend
# ============================================================================
check_deps_libvirt() {
  local missing=() tools=(qemu-img k0sctl kubectl curl ssh genisoimage)
  validate_node_vmms
  check_vmm_matches_host
  any_node_vmm libvirt && tools+=(virt-install virsh)
  for cmd in "${tools[@]}"; do
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
  if any_node_vmm cloud-hypervisor; then
    check_deps_ch
  fi
}

vm_name() { printf '%s-%02d' "$VM_PREFIX" "$(($1 + 1))"; }

# virt-install's --network value for VM index $1: a host bridge or a libvirt
# network, with that VM's fixed MAC when VM_MACS names one.
vm_network() {
  local macs net
  read -r -a macs <<<"$VM_MACS"
  if [[ -n "$LIBVIRT_BRIDGE" ]]; then
    net="bridge=$LIBVIRT_BRIDGE,model=virtio"
  else
    net="network=$LIBVIRT_NETWORK,model=virtio"
  fi
  [[ -n "${macs[$1]:-}" ]] && net+=",mac=${macs[$1]}"
  printf '%s' "$net"
}

# Compare a file against an expected digest. On mismatch the file is REMOVED:
# leaving it in place means the next run finds it cached, skips the download,
# and reuses the very artifact that just failed verification.
verify_digest() {
  local file="$1" algo="$2" expected="$3" actual
  [[ -n "$expected" ]] || { log "no $algo digest available for $(basename "$file")"; return 1; }
  case "$algo" in
    sha256) actual="$(sha256sum "$file" | awk '{print $1}')" ;;
    sha512) actual="$(sha512sum "$file" | awk '{print $1}')" ;;
    *)      log "unknown digest algorithm '$algo'"; return 1 ;;
  esac
  if [[ "$actual" != "$expected" ]]; then
    log "INTEGRITY FAILURE: $(basename "$file")"
    log "  expected $algo: $expected"
    log "  actual   $algo: $actual"
    log "  removing the file so a retry cannot silently reuse it"
    rm -f "$file"
    exit 1
  fi
  log "  $algo verified: ${actual:0:16}..."
}

# Kairos: <asset-url>.sha256, contents "<digest>  <filename>".
verify_installer_iso() {
  [[ "$IMAGE_VERIFY" == "off" ]] && { log "IMAGE_VERIFY=off -- skipping ISO verification"; return 0; }
  local expected="$IMAGE_SHA256"
  if [[ -z "$expected" && -z "$BASE_IMAGE_PATH" ]]; then
    log "Fetching published checksum for $(basename "$INSTALL_ISO")"
    expected="$(curl -fsSL "${IMAGE_URL}.sha256" 2>/dev/null | awk 'NR==1{print $1}' || true)"
  fi
  [[ -n "$expected" ]] || { log "no checksum available -- set IMAGE_SHA256 or IMAGE_VERIFY=off"; exit 1; }
  verify_digest "$INSTALL_ISO" sha256 "$expected"

  if [[ "$IMAGE_COSIGN_VERIFY" == "true" ]]; then
    command -v cosign >/dev/null 2>&1 || { log "IMAGE_COSIGN_VERIFY=true but cosign is not installed"; exit 1; }
    log "Verifying the cosign bundle on the checksum file"
    local tmp; tmp="$(mktemp -d)"
    curl -fsSL -o "$tmp/sum"    "${IMAGE_URL}.sha256"        || { log "cannot fetch .sha256"; exit 1; }
    curl -fsSL -o "$tmp/bundle" "${IMAGE_URL}.sha256.bundle" || { log "cannot fetch .sha256.bundle"; exit 1; }
    cosign verify-blob "$tmp/sum" --bundle "$tmp/bundle" \
      --certificate-identity-regexp "$IMAGE_COSIGN_IDENTITY" \
      --certificate-oidc-issuer "$IMAGE_COSIGN_ISSUER" \
      || { log "cosign verification FAILED"; rm -rf "$tmp"; exit 1; }
    rm -rf "$tmp"
    log "  cosign signature verified"
  fi
}

# Debian: SHA512SUMS beside the image, "<digest>  <filename>" per line.
verify_cloud_image() {
  [[ "$IMAGE_VERIFY" == "off" ]] && { log "IMAGE_VERIFY=off -- skipping cloud image verification"; return 0; }
  local expected="$CLOUD_IMAGE_SHA512" base
  base="$(basename "$CLOUD_IMAGE_URL")"
  if [[ -z "$expected" && -z "$BASE_IMAGE_PATH" ]]; then
    log "Fetching SHA512SUMS for $base"
    expected="$(curl -fsSL "$(dirname "$CLOUD_IMAGE_URL")/SHA512SUMS" 2>/dev/null \
                | awk -v f="$base" '$2 == f {print $1; exit}' || true)"
  fi
  [[ -n "$expected" ]] || { log "no checksum available -- set CLOUD_IMAGE_SHA512 or IMAGE_VERIFY=off"; exit 1; }
  verify_digest "$CLOUD_IMAGE" sha512 "$expected"
}

fetch_cloud_image() {
  if [[ -f "$CLOUD_IMAGE" ]]; then
    log "Cloud image already present at $CLOUD_IMAGE, skipping"
  elif [[ -n "$BASE_IMAGE_PATH" ]]; then
    [[ -f "$BASE_IMAGE_PATH" ]] || { log "BASE_IMAGE_PATH=$BASE_IMAGE_PATH not found"; exit 1; }
    log "Linking local cloud image $BASE_IMAGE_PATH -> $CLOUD_IMAGE"
    ln -f "$BASE_IMAGE_PATH" "$CLOUD_IMAGE" 2>/dev/null || cp "$BASE_IMAGE_PATH" "$CLOUD_IMAGE"
  else
    log "Downloading cloud image from $CLOUD_IMAGE_URL"
    curl -fL --output "$CLOUD_IMAGE.tmp" "$CLOUD_IMAGE_URL"
    mv "$CLOUD_IMAGE.tmp" "$CLOUD_IMAGE"
  fi
}

fetch_base_image() {
  if [[ "$LIBVIRT_IMAGE_KIND" == "cloud" ]]; then
    fetch_cloud_image; verify_cloud_image
  else
    fetch_installer_iso; verify_installer_iso
  fi
}

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

# Standard cloud-init for a distro cloud image. Shares nothing with the Kairos
# path below: `install:` and `stages:` are Kairos/yip extensions that ordinary
# cloud-init ignores outright, so a Kairos user-data on Debian yields a node
# with no user, no key and no way in.
make_cloud_init_cloud() {
  local name="$1" seed_dir="$2" announce_host="${3:-}"
  mkdir -p "$seed_dir"

  cat >"$seed_dir/meta-data" <<EOF
instance-id: $name
local-hostname: $name
EOF

  : >"$seed_dir/user-data"
  chmod 600 "$seed_dir/user-data"   # may hold TAILSCALE_AUTHKEY below
  cat >>"$seed_dir/user-data" <<EOF
#cloud-config
hostname: $name
fqdn: $name
preserve_hostname: false
ssh_pwauth: false
EOF

  if [[ "$SSH_USER" == "root" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
disable_root: false
users:
  - name: root
    ssh_authorized_keys:
      - $(cat "$SSH_PUBKEY")
EOF
  else
    # k0sctl connects as this user and escalates for every privileged step, so
    # NOPASSWD is required -- without it k0sctl hangs on an invisible password
    # prompt and eventually times out. Kairos granted this via its `admin`
    # group; on Debian it has to be spelled out.
    cat >>"$seed_dir/user-data" <<EOF
users:
  - name: $SSH_USER
    groups: [sudo]
    shell: /bin/bash
    sudo: ['ALL=(ALL) NOPASSWD:ALL']
    ssh_authorized_keys:
      - $(cat "$SSH_PUBKEY")
EOF
  fi

  # Loop devices for the imagebuilder's disk-image work, matching what the
  # Kairos path arranges in its `boot` stage.
  cat >>"$seed_dir/user-data" <<EOF
write_files:
  - path: /etc/modules-load.d/banlieue-loop.conf
    permissions: '0644'
    content: |
      loop
  - path: /etc/modprobe.d/banlieue-loop.conf
    permissions: '0644'
    content: |
      options loop max_loop=8
EOF

  # Cloud Hypervisor nodes (ADR-0099): a unit, not a runcmd, because runcmd
  # runs once per instance and the host's neighbour entry must be renewed on
  # every boot. See announce_command.
  if [[ -n "$announce_host" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
  - path: /etc/systemd/system/k0s-vm-announce.service
    permissions: '0644'
    content: |
      [Unit]
      Description=Make the hypervisor host learn this node's address
      Wants=network-online.target
      After=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/bin/sh -c '$(announce_command "$announce_host")'
      [Install]
      WantedBy=multi-user.target
EOF
  fi

  cat >>"$seed_dir/user-data" <<EOF
package_update: true
packages:
  - curl
  - ca-certificates
runcmd:
  - modprobe loop max_loop=8 || modprobe loop
EOF
  if [[ -n "$announce_host" ]]; then
    echo "  - systemctl enable --now k0s-vm-announce.service" >>"$seed_dir/user-data"
  fi

  if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
  - curl -fsSL https://tailscale.com/install.sh | sh
  - tailscale up --authkey=$TAILSCALE_AUTHKEY --hostname=$name --ssh$(tailscale_route_flag)
EOF
  fi
}

# announce_command <host-ip>: what a Cloud Hypervisor node runs at boot so
# the host resolves its MAC to its address. Any packet the host must answer
# will do; ping first, and a TCP connection attempt for an image without it.
# Never fails the boot: an unreachable host only delays address discovery.
announce_command() {
  printf 'ping -c 3 -W 1 %s >/dev/null 2>&1 || curl -s -m 2 -o /dev/null http://%s:1/ || true' "$1" "$1"
}

# make_cloud_init_files <name> <seed-dir> [announce-host]
make_cloud_init_files() {
  local seed_dir="$2"
  if [[ "$LIBVIRT_IMAGE_KIND" == "cloud" ]]; then
    make_cloud_init_cloud "$@"
  else
    make_cloud_init_kairos "$@"
  fi

  # Build the NoCloud seed ISO here, once, for EVERY image kind. Leaving this
  # to each generator means a new kind can write its seed files and silently
  # forget the ISO -- which does not fail here, but much later and obscurely,
  # as virt-install's "Size must be specified for non existent volume".
  genisoimage -output "$seed_dir.iso" -volid cidata -joliet -rock \
    "$seed_dir/user-data" "$seed_dir/meta-data" >/dev/null
}

make_cloud_init_kairos() {
  local name="$1" seed_dir="$2" announce_host="${3:-}"
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

  # One `network:` stage key, whichever entries it carries: a duplicate key
  # is not an error in YAML parsers that keep the last one, it is a silently
  # dropped stage.
  if [[ -n "$announce_host" || -n "$TAILSCALE_AUTHKEY" ]]; then
    echo "  network:" >>"$seed_dir/user-data"
  fi
  # Cloud Hypervisor nodes (ADR-0099): yip stages run on every boot, so the
  # host's neighbour entry for this node is renewed each time it starts.
  if [[ -n "$announce_host" ]]; then
    cat >>"$seed_dir/user-data" <<EOF
    - name: "Make the hypervisor host learn this node's address"
      commands:
        - "$(announce_command "$announce_host")"
EOF
  fi

  if [[ -n "$TAILSCALE_AUTHKEY" ]]; then
    # Not from the installer: Kairos runs this seed's stages in the live
    # boot too, and a join there leaves an offline device on the tailnet
    # that pushes the installed node's name to <name>-1. Same markers as
    # Kairos's own stages in /system/oem.
    cat >>"$seed_dir/user-data" <<EOF
    - name: "Install and join Tailscale"
      if: '[ ! -f /run/cos/live_mode ] && [ ! -f /run/cos/uki_install_mode ]'
      commands:
        - "[ -x /usr/local/bin/tailscaled ] || ( curl -fsSL -o /tmp/ts.tgz https://pkgs.tailscale.com/stable/tailscale_${TAILSCALE_VERSION}_amd64.tgz && tar xzf /tmp/ts.tgz -C /tmp && cp /tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscale /tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscaled /usr/local/bin/ && rm -rf /tmp/ts.tgz /tmp/tailscale_${TAILSCALE_VERSION}_amd64 )"
        - systemctl daemon-reload
        - systemctl enable --now tailscaled
        - "tailscale status >/dev/null 2>&1 || tailscale up --authkey=$TAILSCALE_AUTHKEY --hostname=$name --ssh$(tailscale_route_flag)"
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
  if [[ "$(node_vmm "$idx")" == "cloud-hypervisor" ]]; then
    create_vm_ch "$idx"
    return
  fi
  name="$(vm_name "$idx")"

  if virsh --connect "$LIBVIRT_URI" dominfo "$name" >/dev/null 2>&1; then
    log "VM $name already defined, ensuring it is running"
    virsh --connect "$LIBVIRT_URI" start "$name" >/dev/null 2>&1 || true
    return
  fi

  local disk="$POOL_DIR/$name.qcow2"
  local seed_dir="$POOL_DIR/$name-seed"
  local seed_iso="$POOL_DIR/$name-seed.iso"

  if [[ "$LIBVIRT_IMAGE_KIND" == "cloud" ]]; then
    # Copy the base image rather than using it as a qcow2 BACKING FILE. A
    # backing chain would be faster and thinner, but it leaves every node's
    # disk depending on a file outside its own storage pool -- which libvirt's
    # AppArmor profile on Debian does not automatically grant, and which makes
    # deleting the base image silently destroy every VM. --reflink=auto still
    # gets copy-on-write for free on btrfs/xfs.
    log "Cloning cloud image for $name (${DISK_GB}G)"
    cp --reflink=auto "$CLOUD_IMAGE" "$disk"
    qemu-img resize "$disk" "${DISK_GB}G" >/dev/null

    log "Building cloud-init seed for $name"
    make_cloud_init_files "$name" "$seed_dir"

    log "Defining and starting VM $name (${VCPUS} vCPU, ${MEM_MB}MB RAM)"
    # --import: boot the disk as-is. There is no installer to run and nothing
    # to eject afterwards; cloud-init does first-boot config from the seed.
    #
    # --boot uefi is NOT optional here. Debian's cloud images carry both a BIOS
    # boot partition and an ESP, so SeaBIOS looks like it should work -- GRUB
    # even prints "Booting Debian GNU/Linux" -- but the guest then spins at
    # 100% CPU with a ~64MB working set, never reaching the kernel. No console
    # output, no ARP, but a DHCP lease from the brief moment it was alive, which
    # makes it look like a network fault rather than a boot failure. Booting via
    # OVMF avoids it, and matches the Kairos path on the same host.
    virt-install \
      --connect "$LIBVIRT_URI" \
      --name "$name" \
      --memory "$MEM_MB" \
      --vcpus "$VCPUS" \
      --import \
      --disk "path=$disk,format=qcow2,bus=virtio" \
      --disk "path=$seed_iso,device=cdrom,bus=sata" \
      --os-variant "$OS_VARIANT" \
      --network "$(vm_network "$idx")" \
      --channel unix,target_type=virtio,name=org.qemu.guest_agent.0 \
      --boot uefi \
      --graphics none \
      --console pty,target_type=serial \
      --noautoconsole

    virsh --connect "$LIBVIRT_URI" autostart "$name"
    return
  fi

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
    --network "$(vm_network "$idx")" \
    --channel unix,target_type=virtio,name=org.qemu.guest_agent.0 \
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

# virt-install implicitly DEFINES a libvirt storage pool for whatever directory
# `--disk path=` points into. Because create_vms_libvirt fans out every VM in
# parallel, all VM_COUNT of them race to define the SAME pool, and all but the
# winner die with:
#
#   Could not define storage pool: operation failed: pool 'k0s-bootstrap'
#   already exists with uuid ...
#
# The first VM therefore succeeds and the rest fail, which looks like a random
# per-VM error rather than a race. Defining the pool once, before the fan-out,
# removes it: the parallel virt-installs then find an existing pool and use it.
ensure_pool() {
  local name="${POOL_NAME_OVERRIDE:-$(basename "$POOL_DIR")}"
  virsh --connect "$LIBVIRT_URI" pool-info "$name" >/dev/null 2>&1 && return 0
  log "Defining storage pool '$name' at $POOL_DIR"
  virsh --connect "$LIBVIRT_URI" pool-define-as "$name" dir --target "$POOL_DIR" >/dev/null
  virsh --connect "$LIBVIRT_URI" pool-build     "$name" >/dev/null 2>&1 || true
  virsh --connect "$LIBVIRT_URI" pool-start     "$name" >/dev/null 2>&1 || true
  virsh --connect "$LIBVIRT_URI" pool-autostart "$name" >/dev/null 2>&1 || true
}

create_vms_libvirt() {
  local idx pids=()
  if any_node_vmm libvirt; then
    ensure_pool
  fi
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    if node_retired "$idx"; then
      log "$(vm_name "$idx") is retired (RETIRED_NODES), not creating it"
      continue
    fi
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
  local name="$1" source ip
  if [[ -z "$LIBVIRT_BRIDGE" ]]; then
    virsh --connect "$LIBVIRT_URI" domifaddr "$name" --source lease 2>/dev/null \
      | awk '/ipv4/ {print $4}' | cut -d/ -f1 | head -n1
    return
  fi
  # On a bridge the LAN's DHCP server hands out the address, so libvirt has
  # no lease: ask the guest agent, then the host's neighbour table.
  for source in agent arp; do
    ip="$(virsh --connect "$LIBVIRT_URI" domifaddr "$name" --source "$source" 2>/dev/null \
      | awk '$1 != "lo" && /ipv4/ {print $4}' | cut -d/ -f1 \
      | grep -v -E '^(127\.|169\.254\.)' | head -n1)"
    [[ -n "$ip" ]] && { echo "$ip"; return 0; }
  done
}

wait_for_ip_libvirt() {
  local name="$1" ip=""
  log "Waiting for $name's address..."
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
  validate_node_vmms
  check_vmm_matches_host
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    local name
    name="$(vm_name "$idx")"
    if [[ "$(node_vmm "$idx")" == "cloud-hypervisor" ]]; then
      destroy_ch_node "$idx"
      continue
    fi
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
  any_node_vmm libvirt || { log "All $VM_PREFIX-* nodes removed"; return 0; }
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
# Cloud Hypervisor nodes (libvirt backend, NODE_VMMS=cloud-hypervisor; ADR-0099)
# ============================================================================
# A node is three units in CH_UNIT_DIR, written as root through sudo:
#
#   k0s-vm-<name>-net.service    root oneshot: tap kv-<name> on CH_BRIDGE,
#                                owned by CH_VM_USER (so the VMM needs no
#                                CAP_NET_ADMIN), deleted on stop
#   k0s-vm-<name>-swtpm.service  the vTPM, as CH_VM_USER (CH_TPM=true)
#   k0s-vm-<name>.service        cloud-hypervisor, as CH_VM_USER; enabled, so
#                                it starts at boot without the cluster
#
# The VMM and vTPM units run in banlieue-nodes.slice (ADR-0106), written once
# beside them.
#
# Files live in CH_VM_DIR/<name>/ (disk.qcow2, seed.iso, tpm/), runtime
# sockets in /run/k0s-vm-<name>/ (api.sock, swtpm.sock, serial.sock). No
# banlieue component is involved: a management node must come up before the
# cluster banlieue runs on.
readonly CH_TAP_PREFIX="kv-"
readonly IFNAME_MAX_LEN=15          # IFNAMSIZ (16) less the terminating NUL
readonly CH_TAP_HASH_LEN=12
readonly CH_STOP_TIMEOUT_S=120      # ACPI shutdown budget before SIGTERM
readonly CH_TASKS_MAX=1024
readonly CH_RESTART_DELAY_S=5
readonly CH_SWTPM_WAIT_TRIES=50     # x 0.1 s for the vTPM socket to appear
readonly CH_RUNTIME_DROPIN_ROOT=/run/systemd/system
readonly CH_INSTALLED_MARKER=".installed"
# Every node's VMM and vTPM run here (ADR-0106): a child of banlieue.slice,
# beside banlieue's own guests in banlieue-guests.slice, so one cgroup totals
# the host's VMs and one `systemctl set-property banlieue.slice ...` bounds them.
readonly CH_SLICE="banlieue-nodes.slice"

ch_unit()     { printf 'k0s-vm-%s' "$1"; }
ch_run_dir()  { printf '/run/%s' "$(ch_unit "$1")"; }
ch_node_dir() { printf '%s/%s' "$CH_VM_DIR" "$1"; }

_sha256_hex() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi | awk '{print $1}'
}

# ch_tap_name <name>: kv-<name>, or kv-<hash> when that exceeds IFNAMSIZ.
ch_tap_name() {
  local tap="${CH_TAP_PREFIX}$1"
  if ((${#tap} > IFNAME_MAX_LEN)); then
    tap="${CH_TAP_PREFIX}$(printf '%s' "$1" | _sha256_hex | cut -c1-"$CH_TAP_HASH_LEN")"
  fi
  printf '%s' "$tap"
}

# ch_node_mac <idx> <name>: VM_MACS[idx], else one derived from the cluster
# and node name. Fixed either way: the node's DHCP address must not change,
# and the host finds the node by its MAC.
ch_node_mac() {
  local macs h
  read -r -a macs <<<"$VM_MACS"
  if [[ -n "${macs[$1]:-}" ]]; then
    printf '%s' "${macs[$1]}"
    return
  fi
  h="$(printf '%s/%s' "$CLUSTER_NAME" "$2" | _sha256_hex)"
  printf '52:54:00:%s:%s:%s' "${h:0:2}" "${h:2:2}" "${h:4:2}"
}

# parse_neigh_ip <mac>: reads `ip -4 neigh show` output on stdin and prints
# the address resolved to <mac>, ignoring entries that never resolved. A live
# entry wins over a STALE one: a reinstalled node keeps its MAC, and the
# installer's earlier lease can linger in the table beside the real address.
parse_neigh_ip() {
  awk -v mac="$(tr '[:upper:]' '[:lower:]' <<<"$1")" '
    $2 != "lladdr" || tolower($3) != mac || $4 == "FAILED" || $4 == "INCOMPLETE" { next }
    $4 != "STALE" { print $1; found = 1; exit }
    stale == "" { stale = $1 }
    END { if (!found && stale != "") print stale }'
}

# ch_bridge_addr: the host's IPv4 address on CH_BRIDGE (empty if none).
ch_bridge_addr() {
  ip -4 -o addr show dev "$CH_BRIDGE" 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1
}

# systemd's escaping of a device name inside a unit name (`-` is the path
# separator there).
_systemd_escape_dev() { printf '%s' "${1//-/\\x2d}"; }

# validate_ch_node <name>: settings a CH node cannot be created without.
# Pure: checks values, not the host (see check_deps_ch).
validate_ch_node() {
  local name="$1"
  if [[ ! "$name" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]]; then
    log "'$name' is not usable as a Cloud Hypervisor node name (lowercase letters, digits and '-')"
    exit 1
  fi
  if [[ -z "$CH_FIRMWARE" ]]; then
    log "Cloud Hypervisor node $name needs CH_FIRMWARE=<path to CLOUDHV.fd>"
    log "  (\`banlieue host cloud-hypervisor install\` puts one under /opt/banlieue/firmware/)"
    exit 1
  fi
  if [[ -z "$CH_BRIDGE" ]]; then
    log "Cloud Hypervisor node $name needs a bridge: set CH_BRIDGE (or LIBVIRT_BRIDGE)"
    exit 1
  fi
  if [[ "$LIBVIRT_URI" =~ ^[a-z+]+://[^/] ]]; then
    log "Cloud Hypervisor node $name is created on THIS host, but LIBVIRT_URI ($LIBVIRT_URI) is remote."
    log "  Run the script on the hypervisor host (the k0s-remote-* make targets do)."
    exit 1
  fi
  if [[ ! "$CH_VM_USER" =~ ^[a-z_][a-z0-9_-]*$ ]]; then
    log "CH_VM_USER '$CH_VM_USER' is not a valid user name"
    exit 1
  fi
}

# check_deps_ch: what the host must provide for any CH node.
# resolve_ch_bridge: with no CH_BRIDGE (and no LIBVIRT_BRIDGE behind it),
# use LIBVIRT_NETWORK's bridge, read from libvirt rather than guessed.
resolve_ch_bridge() {
  [[ -n "$CH_BRIDGE" ]] && return 0
  CH_BRIDGE="$(virsh --connect "$LIBVIRT_URI" net-dumpxml "$LIBVIRT_NETWORK" 2>/dev/null \
    | sed -n "s/.*<bridge name=['\"]\([^'\"]*\)['\"].*/\1/p" | head -n1)"
  if [[ -z "$CH_BRIDGE" ]]; then
    log "No bridge for Cloud Hypervisor nodes: set CH_BRIDGE (or LIBVIRT_BRIDGE),"
    log "  or define libvirt network '$LIBVIRT_NETWORK' so its bridge can be used."
    exit 1
  fi
  log "Cloud Hypervisor nodes join $CH_BRIDGE (libvirt network $LIBVIRT_NETWORK)"
}

check_deps_ch() {
  local missing=() idx cmd
  for idx in $(seq 0 $((VM_COUNT - 1))); do
    node_retired "$idx" && continue
    [[ "$(node_vmm "$idx")" == "cloud-hypervisor" ]] && validate_ch_node "$(vm_name "$idx")"
  done
  for cmd in "$CH_BINARY" "$CH_REMOTE" qemu-img genisoimage ip systemctl sudo; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  [[ "$CH_TPM" == "true" ]] && { command -v "$SWTPM" >/dev/null 2>&1 || missing+=("$SWTPM (or CH_TPM=false)"); }
  [[ -r "$CH_FIRMWARE" ]] || missing+=("readable CH_FIRMWARE ($CH_FIRMWARE)")
  if ((${#missing[@]})); then
    log "Missing for Cloud Hypervisor nodes: ${missing[*]}"
    exit 1
  fi
  if ! ch_pins_nic_slot; then
    log "$CH_BINARY cannot pin a NIC's PCI slot (no pci_device_id in --net); upgrade Cloud Hypervisor."
    exit 1
  fi
  if ! sudo -n true 2>/dev/null; then
    log "Cloud Hypervisor nodes are created through sudo, which needs a password here."
    log "  Run 'sudo -v' first, or configure passwordless sudo for this user."
    exit 1
  fi
  resolve_ch_bridge
  if [[ -z "$(ch_bridge_addr)" ]]; then
    log "Bridge $CH_BRIDGE has no IPv4 address on this host (or does not exist)."
    log "  Cloud Hypervisor nodes announce themselves to that address so the host can find them."
    exit 1
  fi
}

# ch_pins_nic_slot: does CH_BINARY's --net take pci_device_id (CH_NIC_PCI_SLOT)?
ch_pins_nic_slot() {
  # Captured, not piped to grep -q: under pipefail an early grep exit would
  # SIGPIPE the help output and read as "no".
  [[ "$("$CH_BINARY" --help 2>/dev/null)" == *pci_device_id=* ]]
}

render_ch_net_unit() {
  local name="$1" tap ip_bin br_dev
  tap="$(ch_tap_name "$name")"
  ip_bin="$(command -v ip || echo /usr/sbin/ip)"
  br_dev="sys-subsystem-net-devices-$(_systemd_escape_dev "$CH_BRIDGE").device"
  cat <<EOF
# Written by scripts/bootstrap-k0s-cluster.sh (ADR-0099). Do not edit.
[Unit]
Description=k0s node $name: tap $tap on $CH_BRIDGE
Wants=network-online.target
After=network-online.target
BindsTo=$br_dev
After=$br_dev

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStartPre=-$ip_bin link del $tap
ExecStart=$ip_bin tuntap add dev $tap mode tap user $CH_VM_USER group $CH_VM_USER
ExecStart=$ip_bin link set $tap master $CH_BRIDGE up
ExecStop=$ip_bin link del $tap
EOF
}

# render_ch_slice_unit: no limits. The operator bounds every VM on the host
# with `systemctl set-property banlieue.slice ...`, a drop-in this script
# never overwrites.
render_ch_slice_unit() {
  cat <<EOF
# Written by scripts/bootstrap-k0s-cluster.sh (ADR-0106). Do not edit.
# Management-cluster nodes on Cloud Hypervisor; a child of banlieue.slice.
# Bound all VMs on this host with:
#   systemctl set-property banlieue.slice CPUWeight=50 MemoryMax=48G
[Unit]
Description=banlieue management-cluster nodes (Cloud Hypervisor)

[Slice]
EOF
}

# render_ch_slice_dropin: moves a node whose units predate ADR-0106 into the
# slice without rewriting them (they carry the sizing they were built with).
render_ch_slice_dropin() {
  cat <<EOF
# Written by scripts/bootstrap-k0s-cluster.sh (ADR-0106). Do not edit.
[Service]
Slice=$CH_SLICE
EOF
}

# ensure_ch_slice <name>: the slice unit, and a drop-in on each of the node's
# VM units that does not already name it. Takes effect when the unit next
# restarts; a running node is not restarted for it.
ensure_ch_slice() {
  local unit u dropin
  unit="$(ch_unit "$1")"
  render_ch_slice_unit | sudo tee "$CH_UNIT_DIR/$CH_SLICE" >/dev/null
  for u in "$unit" "$unit-swtpm"; do
    sudo test -f "$CH_UNIT_DIR/$u.service" || continue
    sudo grep -qx "Slice=$CH_SLICE" "$CH_UNIT_DIR/$u.service" && continue
    dropin="$CH_UNIT_DIR/$u.service.d"
    sudo install -d "$dropin"
    render_ch_slice_dropin | sudo tee "$dropin/50-slice.conf" >/dev/null
    log "$u moves into $CH_SLICE at its next restart"
  done
  sudo systemctl daemon-reload
}

render_ch_swtpm_unit() {
  local name="$1" unit run dir
  unit="$(ch_unit "$name")"; run="$(ch_run_dir "$name")"; dir="$(ch_node_dir "$name")"
  cat <<EOF
# Written by scripts/bootstrap-k0s-cluster.sh (ADR-0099). Do not edit.
[Unit]
Description=k0s node $name: vTPM
PartOf=$unit.service

[Service]
Slice=$CH_SLICE
User=$CH_VM_USER
Group=$CH_VM_USER
RuntimeDirectory=$unit
RuntimeDirectoryMode=0700
RuntimeDirectoryPreserve=yes
UMask=0077
ExecStart=$SWTPM socket --tpm2 --tpmstate dir=$dir/tpm --ctrl type=unixio,path=$run/swtpm.sock
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes
ReadWritePaths=$dir/tpm $run
DevicePolicy=closed
EOF
}

# render_ch_vm_unit <name> <idx>. $K0S_VM_INSTALL_DISK is deliberately an
# unbraced word of its own: systemd splits it at whitespace, so it adds the
# installer disk while the install drop-in sets it and nothing otherwise.
# Landlock is on, as for banlieue's own guests. The VMM derives rules for the
# files and sockets it is given, but not for the tap's sysfs entry it reads at
# boot (tun_flags), so that one directory is granted read-only.
render_ch_vm_unit() {
  local name="$1" idx="$2" unit run dir tap mac tpm_deps="" tpm_wait="" tpm_arg=""
  unit="$(ch_unit "$name")"; run="$(ch_run_dir "$name")"; dir="$(ch_node_dir "$name")"
  tap="$(ch_tap_name "$name")"; mac="$(ch_node_mac "$idx" "$name")"
  if [[ "$CH_TPM" == "true" ]]; then
    tpm_deps="Requires=$unit-swtpm.service"$'\n'"After=$unit-swtpm.service"
    # swtpm is Type=simple: wait for its socket, or the VMM races it.
    tpm_wait="ExecStartPre=/bin/sh -c 'i=0; while [ ! -S $run/swtpm.sock ]; do i=\$\$((i+1)); [ \$\$i -gt $CH_SWTPM_WAIT_TRIES ] && exit 1; sleep 0.1; done'"
    tpm_arg="  --tpm socket=$run/swtpm.sock \\"$'\n'
  fi
  cat <<EOF
# Written by scripts/bootstrap-k0s-cluster.sh (ADR-0099). Do not edit.
# A management-cluster node on Cloud Hypervisor. Serial console:
#   sudo socat -,raw,echo=0 UNIX-CONNECT:$run/serial.sock
[Unit]
Description=k0s node $name (Cloud Hypervisor)
BindsTo=$unit-net.service
After=$unit-net.service
$tpm_deps

[Service]
Slice=$CH_SLICE
User=$CH_VM_USER
Group=$CH_VM_USER
SupplementaryGroups=kvm
RuntimeDirectory=$unit
RuntimeDirectoryMode=0700
RuntimeDirectoryPreserve=yes
# The API, serial and vTPM sockets: the VM user's and root's only.
UMask=0077
# Set only during install, by a drop-in under $CH_RUNTIME_DROPIN_ROOT.
Environment=K0S_VM_INSTALL_DISK=
ExecStartPre=/bin/rm -f $run/api.sock
$tpm_wait
ExecStart=$CH_BINARY \\
  --api-socket path=$run/api.sock \\
  --landlock \\
  --landlock-rules path=/sys/class/net/$tap,access=r \\
  --firmware $CH_FIRMWARE \\
  --cpus boot=$VCPUS \\
  --memory size=${MEM_MB}M \\
  --disk path=$dir/disk.qcow2,image_type=qcow2 path=$dir/seed.iso,readonly=on,image_type=raw \$K0S_VM_INSTALL_DISK \\
  --net tap=$tap,mac=$mac,pci_device_id=$CH_NIC_PCI_SLOT \\
${tpm_arg}  --rng src=/dev/urandom \\
  --serial socket=$run/serial.sock \\
  --console off
# The guest gets an ACPI power button and time to shut down; SIGTERM only
# after TimeoutStopSec. A host shutdown is then a guest shutdown.
ExecStop=/bin/sh -c '$CH_REMOTE --api-socket $run/api.sock power-button; while kill -0 \$MAINPID 2>/dev/null; do sleep 1; done'
TimeoutStopSec=$CH_STOP_TIMEOUT_S
Restart=on-failure
# Paced, so a VMM that cannot start does not drag its vTPM unit into
# systemd's start limit.
RestartSec=${CH_RESTART_DELAY_S}s
TasksMax=$CH_TASKS_MAX
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes
ReadWritePaths=$dir $run
DevicePolicy=closed
DeviceAllow=/dev/kvm rw
DeviceAllow=/dev/net/tun rw

[Install]
WantedBy=multi-user.target
EOF
}

# The install-time drop-in: attaches the installer and never restarts, so a
# guest that powers off after installing leaves the unit inactive.
render_ch_install_dropin() {
  local dir; dir="$(ch_node_dir "$1")"
  cat <<EOF
[Service]
Environment=K0S_VM_INSTALL_DISK=path=$dir/install.iso,readonly=on,image_type=raw
Restart=no
EOF
}

ensure_ch_vm_user() {
  id -u "$CH_VM_USER" >/dev/null 2>&1 && return 0
  log "Creating system user $CH_VM_USER for Cloud Hypervisor nodes"
  sudo useradd --system --no-create-home --home-dir /nonexistent \
    --shell /usr/sbin/nologin "$CH_VM_USER"
}

write_ch_units() {
  local name="$1" idx="$2" unit
  unit="$(ch_unit "$name")"
  render_ch_slice_unit | sudo tee "$CH_UNIT_DIR/$CH_SLICE" >/dev/null
  render_ch_net_unit "$name" | sudo tee "$CH_UNIT_DIR/$unit-net.service" >/dev/null
  if [[ "$CH_TPM" == "true" ]]; then
    render_ch_swtpm_unit "$name" | sudo tee "$CH_UNIT_DIR/$unit-swtpm.service" >/dev/null
  fi
  render_ch_vm_unit "$name" "$idx" | sudo tee "$CH_UNIT_DIR/$unit.service" >/dev/null
  sudo systemctl daemon-reload
}

# wait_for_install_ch <name>: the install drop-in has Restart=no, so the unit
# goes inactive when Kairos powers the guest off, and failed if the VMM did.
wait_for_install_ch() {
  local name="$1" unit state
  unit="$(ch_unit "$name").service"
  log "Waiting for $name to install Kairos and power off..."
  for _ in $(seq 1 "$INSTALL_WAIT_ATTEMPTS"); do
    state="$(systemctl is-active "$unit" 2>/dev/null || true)"
    case "$state" in
      inactive) return 0 ;;
      failed)
        log "$unit failed during install:"
        sudo journalctl -u "$unit" -n 20 --no-pager >&2 || true
        exit 1 ;;
    esac
    sleep 5
  done
  log "Timed out waiting for $name to power off after install"
  exit 1
}

create_vm_ch() {
  local idx="$1" name unit dir host_ip seed_dir dropin
  name="$(vm_name "$idx")"; unit="$(ch_unit "$name")"; dir="$(ch_node_dir "$name")"

  if sudo test -f "$CH_UNIT_DIR/$unit.service"; then
    if sudo test -f "$dir/$CH_INSTALLED_MARKER"; then
      log "Cloud Hypervisor node $name already defined, ensuring it is running"
      ensure_ch_slice "$name"
      sudo systemctl start "$unit.service"
      return
    fi
    # An install that was interrupted (the script killed, the host rebooted)
    # leaves units and a partial disk but no marker. Nothing on that disk is
    # worth keeping, so this node alone is removed and installed again.
    log "$name never finished installing; removing it and starting over"
    destroy_ch_node "$idx"
  fi

  host_ip="$(ch_bridge_addr)"
  ensure_ch_vm_user
  sudo install -d -m 0750 -o "$CH_VM_USER" -g "$CH_VM_USER" "$dir"
  if ! sudo -u "$CH_VM_USER" test -w "$dir"; then
    log "$CH_VM_USER cannot reach $dir: every parent directory must be searchable by it"
    exit 1
  fi
  sudo install -d -m 0700 -o "$CH_VM_USER" -g "$CH_VM_USER" "$dir/tpm"

  # The seed is built as the operator, then installed into the node's
  # directory; the working copy may hold TAILSCALE_AUTHKEY, so it goes.
  seed_dir="$WORKDIR/ch-seed/$name"
  log "Building cloud-init seed for $name (announces itself to $host_ip)"
  make_cloud_init_files "$name" "$seed_dir" "$host_ip"
  sudo install -m 0400 -o "$CH_VM_USER" -g "$CH_VM_USER" "$seed_dir.iso" "$dir/seed.iso"
  rm -rf "$seed_dir" "$seed_dir.iso"

  if [[ "$LIBVIRT_IMAGE_KIND" == "cloud" ]]; then
    log "Cloning cloud image for $name (${DISK_GB}G)"
    sudo cp --reflink=auto "$CLOUD_IMAGE" "$dir/disk.qcow2"
    sudo qemu-img resize "$dir/disk.qcow2" "${DISK_GB}G" >/dev/null
  else
    log "Creating empty disk for $name (${DISK_GB}G -- Kairos installs onto it from the ISO)"
    sudo qemu-img create -f qcow2 "$dir/disk.qcow2" "${DISK_GB}G" >/dev/null
    sudo cp --reflink=auto "$INSTALL_ISO" "$dir/install.iso"
    sudo chown "$CH_VM_USER:$CH_VM_USER" "$dir/install.iso"
  fi
  sudo chown "$CH_VM_USER:$CH_VM_USER" "$dir/disk.qcow2"
  sudo chmod 0600 "$dir/disk.qcow2"

  log "Writing systemd units for $name (${VCPUS} vCPU, ${MEM_MB}MB RAM, Cloud Hypervisor)"
  write_ch_units "$name" "$idx"

  if [[ "$LIBVIRT_IMAGE_KIND" != "cloud" ]]; then
    dropin="$CH_RUNTIME_DROPIN_ROOT/$unit.service.d"
    sudo install -d "$dropin"
    render_ch_install_dropin "$name" | sudo tee "$dropin/install.conf" >/dev/null
    sudo systemctl daemon-reload
    sudo systemctl start "$unit.service"
    wait_for_install_ch "$name"
    log "Detaching the installer from $name and booting the installed system"
    sudo rm -rf "$dropin"
    sudo rm -f "$dir/install.iso"
    sudo systemctl daemon-reload
  fi
  sudo touch "$dir/$CH_INSTALLED_MARKER"
  sudo systemctl enable --now "$unit.service"
}

wait_for_ip_ch() {
  local idx="$1" name="$2" mac ip=""
  mac="$(ch_node_mac "$idx" "$name")"
  log "Waiting for $name's address (MAC $mac in the neighbour table on $CH_BRIDGE)..."
  for _ in $(seq 1 "$IP_WAIT_ATTEMPTS"); do
    ip="$(ip -4 neigh show dev "$CH_BRIDGE" 2>/dev/null | parse_neigh_ip "$mac")"
    [[ -n "$ip" ]] && { echo "$ip"; return 0; }
    sleep 5
  done
  log "Timed out waiting for an IP address for $name"
  exit 1
}

destroy_ch_node() {
  local idx="$1" name unit dir dropin
  name="$(vm_name "$idx")"; unit="$(ch_unit "$name")"; dir="$(ch_node_dir "$name")"
  validate_ch_node "$name"
  if sudo test -f "$CH_UNIT_DIR/$unit.service"; then
    log "Destroying Cloud Hypervisor node $name"
    sudo systemctl disable "$unit.service" >/dev/null 2>&1 || true
    # destroy is a power cut, like `virsh destroy`.
    sudo systemctl kill --signal=SIGKILL "$unit.service" >/dev/null 2>&1 || true
    sudo systemctl stop "$unit.service" "$unit-swtpm.service" "$unit-net.service" >/dev/null 2>&1 || true
  fi
  dropin="$CH_RUNTIME_DROPIN_ROOT/$unit.service.d"
  sudo rm -rf "$dropin"
  sudo rm -f "$CH_UNIT_DIR/$unit.service" "$CH_UNIT_DIR/$unit-swtpm.service" "$CH_UNIT_DIR/$unit-net.service"
  sudo rm -rf "$CH_UNIT_DIR/$unit.service.d" "$CH_UNIT_DIR/$unit-swtpm.service.d"
  sudo systemctl daemon-reload
  sudo systemctl reset-failed "$unit.service" "$unit-swtpm.service" "$unit-net.service" >/dev/null 2>&1 || true
  # validate_ch_node above guarantees a non-empty, slash-free name.
  sudo rm -rf "${CH_VM_DIR:?}/${name:?}"
  rm -rf "$WORKDIR/ch-seed/$name" "$WORKDIR/ch-seed/$name.iso"
}

# ============================================================================
# join: add workers to a RUNNING cluster (any backend; ADR-0099)
# ============================================================================
# Remote snippet that finds the k0s binary however it was installed: k0sctl
# puts it on PATH, the native path under /opt/k0s. Single-quoted on purpose:
# it is expanded on the node, never here.
# shellcheck disable=SC2016
readonly K0S_FIND_REMOTE='k=$(command -v k0s 2>/dev/null || ls -1 /opt/k0s/k0s-v*-amd64 2>/dev/null | tail -n1); [ -n "$k" ] || { echo "no k0s binary on this node" >&2; exit 1; }'
readonly JOIN_READY_ATTEMPTS=60     # x 5 s for the new node to report Ready
# What a joining controller does NOT copy from the running one's /etc/k0s:
# join tokens (each node gets its own) and the containerd config k0s
# generates for each worker. GNU tar exclusions match any path component.
readonly K0S_CONFIG_CLONE_EXCLUDES="--exclude=*token-file --exclude=containerd.toml"

# join_targets: the NODE_TABLE rows named by JOIN_NODES, in that order.
# Refuses names that are not nodes of this cluster.
join_targets() {
  [[ -n "$JOIN_NODES" ]] || { log "join needs JOIN_NODES=<node names>"; exit 1; }
  local want row name role ip found
  for want in $JOIN_NODES; do
    found=""
    for row in "${NODE_TABLE[@]}"; do
      IFS='|' read -r name role ip <<<"$row"
      [[ "$name" == "$want" ]] || continue
      found="$row"
      break
    done
    [[ -n "$found" ]] || { log "JOIN_NODES names '$want', which is not one of this cluster's nodes"; exit 1; }
    echo "$found"
  done
}

# join_controller_ip: JOIN_CONTROLLER, else the first controller's address.
join_controller_ip() {
  if [[ -n "$JOIN_CONTROLLER" ]]; then
    printf '%s' "$JOIN_CONTROLLER"
    return
  fi
  local row name role ip
  for row in "${NODE_TABLE[@]}"; do
    IFS='|' read -r name role ip <<<"$row"
    [[ "$role" == controller* ]] && { printf '%s' "$ip"; return; }
  done
  log "no controller in the node table; set JOIN_CONTROLLER=<address>"
  exit 1
}

# controller_install_flags <ExecStart> <role>: the `k0s install controller`
# flags for a node joining in <role>, from the running controller's
# ExecStart line. Every flag after `controller` is kept (config path, data
# dir, disabled components) except the worker ones, which follow <role> and
# NO_TAINTS as at bootstrap, and a token file, which the join supplies.
# Refuses a `--single` controller (it cannot have peers), a line that is
# not a controller's, and quoting this word split cannot carry.
controller_install_flags() {
  local exec_start="$1" role="$2" word seen="" skip="" out=()
  case "$exec_start" in
    *\'*|*\"*|*\\*) log "the controller's ExecStart has quoting join cannot copy: $exec_start"; return 1 ;;
  esac
  for word in $exec_start; do
    if [[ -z "$seen" ]]; then
      [[ "$word" == controller ]] && seen=1
      continue
    fi
    if [[ -n "$skip" ]]; then skip=""; continue; fi
    case "$word" in
      --single|--single=true)
        log "the running controller is a --single one, which cannot take peers"; return 1 ;;
      --token-file) skip=1 ;;
      --token-file=*|--enable-worker|--enable-worker=*|--no-taints|--no-taints=*) ;;
      *) out+=("$word") ;;
    esac
  done
  [[ -n "$seen" ]] || { log "not a k0s controller's ExecStart: $exec_start"; return 1; }
  if [[ "$role" == "controller+worker" ]]; then
    out+=(--enable-worker)
    [[ "$NO_TAINTS" == "true" ]] && out+=(--no-taints)
  fi
  printf '%s' "${out[*]}"
}

# join_worker_node <controller-ip> <name> <ip> <k0s-bin>
join_worker_node() {
  local cp_ip="$1" name="$2" ip="$3" k0s_bin="$4"
  if ssh_run "$ip" 'systemctl is-active --quiet k0sworker' 2>/dev/null; then
    log "[$name] k0sworker already active, skipping"
    return 0
  fi
  stage_k0s_binary "$ip"
  log "[$name] minting a worker token on $cp_ip (expires in $JOIN_TOKEN_EXPIRY)"
  # Straight from the controller into a root-only file on the node: the
  # token is never written on this machine.
  ssh_run "$cp_ip" "sudo sh -c '$K0S_FIND_REMOTE; \"\$k\" token create --role=worker --expiry=$JOIN_TOKEN_EXPIRY'" \
    | tr -d '\r' \
    | ssh_run "$ip" 'sudo install -d -m 0700 /etc/k0s && sudo sh -c "umask 077; cat >/etc/k0s/worker-token-file"'
  log "[$name/$ip] installing k0s worker"
  ssh_run "$ip" "sudo $k0s_bin install worker --force --token-file /etc/k0s/worker-token-file && sudo $k0s_bin start"
  wait_node_ready "$cp_ip" "$name"
}

# join_controller_node <controller-ip> <name> <role> <ip> <k0s-bin>: the new
# controller takes the RUNNING controller's /etc/k0s (k0s.yaml with its
# storage, VIP and API server settings, and the files it names, such as a
# datastore CA or an authentication config) and its install flags, so it
# matches the cluster as it is, not as this script would generate it. The
# token carries the cluster CA and service-account keys over k0s's join API.
join_controller_node() {
  local cp_ip="$1" name="$2" role="$3" ip="$4" k0s_bin="$5" exec_start flags
  if ssh_run "$ip" 'systemctl is-active --quiet k0scontroller' 2>/dev/null; then
    log "[$name] k0scontroller already active, skipping"
    return 0
  fi
  if ssh_run "$ip" 'systemctl is-active --quiet k0sworker' 2>/dev/null; then
    log "[$name] runs k0s as a worker; reset it before joining it as a $role:"
    log "  on $name: sudo k0s stop && sudo k0s reset && sudo reboot   (k0s may be under /opt/k0s)"
    log "  then: kubectl delete node $name"
    exit 1
  fi
  exec_start="$(ssh_run "$cp_ip" 'systemctl cat k0scontroller' | tr -d '\r' | sed -n 's/^ExecStart=//p' | tail -n1)"
  flags="$(controller_install_flags "$exec_start" "$role")" || exit 1
  # A Node left from an earlier role (a reset worker) would be reused as is:
  # kubelet sets a Node's labels only when it registers it, so the new
  # controller would never carry node-role.kubernetes.io/control-plane.
  if ssh_run "$cp_ip" "sudo sh -c '$K0S_FIND_REMOTE; \"\$k\" kubectl get node $name --no-headers'" >/dev/null 2>&1; then
    log "[$name] a Node named $name already exists, from its earlier role."
    log "  kubelet labels a Node only when it registers it, so delete it first: kubectl delete node $name"
    exit 1
  fi
  stage_k0s_binary "$ip"
  log "[$name] copying /etc/k0s from $cp_ip (no tokens)"
  # Node to node through this machine's pipe, never written here: k0s.yaml
  # carries the datastore password and the VRRP secret.
  ssh_run "$cp_ip" "sudo tar -C /etc/k0s --numeric-owner $K0S_CONFIG_CLONE_EXCLUDES -cf - ." \
    | ssh_run "$ip" 'sudo install -d -m 0755 /etc/k0s && sudo tar -C /etc/k0s --numeric-owner -xpf -'
  log "[$name] minting a controller token on $cp_ip (expires in $JOIN_TOKEN_EXPIRY)"
  ssh_run "$cp_ip" "sudo sh -c '$K0S_FIND_REMOTE; \"\$k\" token create --role=controller --expiry=$JOIN_TOKEN_EXPIRY'" \
    | tr -d '\r' \
    | ssh_run "$ip" 'sudo sh -c "umask 077; cat >/etc/k0s/controller-token-file"'
  log "[$name/$ip] installing k0s $role: $flags"
  ssh_run "$ip" "sudo $k0s_bin install controller --force $flags --token-file /etc/k0s/controller-token-file && sudo $k0s_bin start"
  wait_controlnode "$cp_ip" "$name"
  [[ "$role" == "controller+worker" ]] && wait_node_ready "$cp_ip" "$name"
  return 0
}

join_nodes() {
  populate_node_table
  local targets cp_ip cp_version tag k0s_bin name role ip
  targets="$(join_targets)"
  cp_ip="$(join_controller_ip)"
  tag="$(_k0s_ver_tag)"
  k0s_bin="/opt/k0s/k0s-${tag}-amd64"

  cp_version="$(ssh_run "$cp_ip" "sudo sh -c '$K0S_FIND_REMOTE; \"\$k\" version'" | tr -d '\r')"
  if [[ "$cp_version" != "$tag" ]]; then
    log "The controller at $cp_ip runs k0s $cp_version but K0S_VERSION is $tag."
    log "  Set K0S_VERSION to the cluster's version: a worker joins at the version it is installed with."
    exit 1
  fi

  while IFS='|' read -r name role ip; do
    case "$role" in
      worker) join_worker_node "$cp_ip" "$name" "$ip" "$k0s_bin" ;;
      controller|controller+worker) join_controller_node "$cp_ip" "$name" "$role" "$ip" "$k0s_bin" ;;
      *) log "[$name] unknown role '$role'"; exit 1 ;;
    esac
  done <<<"$targets"
  log "join complete: $JOIN_NODES"
}

# wait_controlnode <controller-ip> <node>: k0s's autopilot records every
# controller as a ControlNode once it has started against the datastore.
wait_controlnode() {
  local cp_ip="$1" name="$2"
  log "[$name] waiting for the controller to register..."
  for _ in $(seq 1 "$JOIN_READY_ATTEMPTS"); do
    if ssh_run "$cp_ip" "sudo sh -c '$K0S_FIND_REMOTE; \"\$k\" kubectl get controlnode $name --no-headers'" >/dev/null 2>&1; then
      log "[$name] controller registered"
      return 0
    fi
    sleep 5
  done
  log "[$name] never registered as a controller; check 'sudo journalctl -u k0scontroller' on the node."
  log "  With kine, the datastore must accept this node's address (for PostgreSQL, a pg_hba.conf line)."
  exit 1
}

# wait_node_ready <controller-ip> <node>: asks the controller, so it works
# whatever kubeconfig the operator has.
wait_node_ready() {
  local cp_ip="$1" name="$2" status
  log "[$name] waiting for the node to report Ready..."
  for _ in $(seq 1 "$JOIN_READY_ATTEMPTS"); do
    status="$(ssh_run "$cp_ip" "sudo sh -c '$K0S_FIND_REMOTE; \"\$k\" kubectl get node $name --no-headers'" 2>/dev/null | awk '{print $2}' || true)"
    [[ "$status" == Ready* ]] && { log "[$name] Ready"; return 0; }
    sleep 5
  done
  log "[$name] did not report Ready; check 'sudo journalctl -u k0sworker' on the node"
  exit 1
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
  [[ -n "$API_VIP" ]] && echo "    externalAddress: $(api_vip_addr)"
  echo "    sans:"
  [[ -n "$API_SAN" ]] && echo "      - $API_SAN"
  [[ -n "$API_VIP" ]] && echo "      - $(api_vip_addr)"
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
  render_cplb "    "
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
  validate_api_vip
  local entry controllers=0
  for entry in "${NODES[@]}"; do
    [[ -n "$entry" ]] || continue
    _parse_node "$entry"
    [[ "$_N_ROLE" == controller* ]] && controllers=$((controllers + 1))
  done
  require_api_vip "$controllers"
  populate_node_table
  local cp cp_ip row name role ip
  cp="$(first_controller)" || { log "no controller in NODES"; exit 1; }
  cp_ip="${cp#* }"
  for row in "${NODE_TABLE[@]}"; do
    IFS='|' read -r name role ip <<<"$row"
    stage_k0s_binary "$ip"
    [[ "$role" == controller* ]] && { push_k0s_yaml "$ip" "$cp_ip" || true; }
  done
  local vip=""
  [[ -n "$API_VIP" ]] && vip="$(api_vip_addr)"
  echo "${KUBECONFIG_SERVER:-${API_SAN:-${vip:-$cp_ip}}}" >"$KUBECONFIG_SERVER_FILE"
  log "vSphere prepare complete (binaries staged, controller configs written)"
  log_api_vip_followups
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
  # k0sctl names the cluster/context/user after the k0sctl config's cluster
  # name. Force them to KUBECONFIG_NAME so the file is self-describing and can
  # be merged with other clusters' configs without a name clash.
  if [[ "$KUBECONFIG_NAME" != "$CLUSTER_NAME" ]]; then
    sed -i.bak -E "s/\b${CLUSTER_NAME}\b/${KUBECONFIG_NAME}/g" "$KUBECONFIG_OUT"
    rm -f "$KUBECONFIG_OUT.bak"
  fi

  grep -E '^\s*server:' "$KUBECONFIG_OUT" >&2 || true
  log "export KUBECONFIG=$KUBECONFIG_OUT"
  log "context: $(kubectl --kubeconfig "$KUBECONFIG_OUT" config current-context 2>/dev/null || echo "$KUBECONFIG_NAME")"
  log ""
  log "To use it from your workstation:"
  log "  scp $(whoami)@$(hostname -f 2>/dev/null || hostname):$KUBECONFIG_OUT ~/.kube/$KUBECONFIG_NAME.yaml"
  log "  export KUBECONFIG=~/.kube/$KUBECONFIG_NAME.yaml"
  log "Note: the server address above is on the libvirt NAT network and is"
  log "reachable only from this host unless you route or tunnel to it."
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
#   libvirt: name from VM_COUNT, role from NODE_ROLES, ip from DHCP lease
#            (or, for a cloud-hypervisor node, the host's neighbour table).
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
    node_retired "$idx" && continue
    name="$(vm_name "$idx")"
    if [[ "$(node_vmm "$idx")" == "cloud-hypervisor" ]]; then
      ip="$(wait_for_ip_ch "$idx" "$name")"
    else
      ip="$(wait_for_ip_libvirt "$name")"
    fi
    wait_for_ssh "$ip"
    NODE_TABLE+=("$name|${roles[$idx]:-worker}|$ip")
  done
}

generate_k0sctl_config() {
  validate_api_vip
  populate_node_table

  local hosts="" sans="" cp_ip="" cp_ts_ip="" controllers=0
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
      else
        # Do not let this pass quietly. An auth key was supplied, so the
        # operator wants the API reachable over the tailnet -- and the SANs are
        # baked into the serving certificate at install time, so a miss here
        # cannot be repaired later without reinstalling k0s. Falling back to the
        # NAT address silently produces a cluster that is unreachable from
        # anywhere but the hypervisor: the exact problem the key was meant to solve.
        log "WARNING: $name never reported a tailscale IP after $((TAILSCALE_WAIT_ATTEMPTS * 5))s."
        log "  Its tailnet address will NOT be in the API server certificate."
        log "  Check on the node: cloud-init status; systemctl status tailscaled; tailscale status"
        log "  Then destroy and re-run -- the SANs cannot be added afterwards."
      fi
    fi
    # On vSphere, and on a libvirt host bridge, every node has a routable
    # address; add each as a SAN so kubectl can land on any controller.
    if [[ "$BACKEND" == "vsphere" || -n "$LIBVIRT_BRIDGE" ]]; then
      sans+="            - $ip"$'\n'
    fi

    if [[ "$role" == controller* ]]; then
      controllers=$((controllers + 1))
      if [[ -z "$cp_ip" ]]; then
        cp_ip="$ip"; cp_ts_ip="$ts_ip"
      fi
    fi
    hosts+="  - role: $role"$'\n'
    hosts+="    uploadBinary: true"$'\n'
    hosts+="    os: $K0SCTL_OS_OVERRIDE"$'\n'
    if [[ "$role" == "controller+worker" && "$NO_TAINTS" == "true" ]]; then
      hosts+="    noTaints: true"$'\n'
    fi
    # See K0S_DISABLE_KONNECTIVITY and ADR-0086. k0sctl's reinstall phase
    # applies a changed flag to an existing controller on the next apply.
    if [[ "$role" == controller* && "$K0S_DISABLE_KONNECTIVITY" == "true" ]]; then
      hosts+="    installFlags:"$'\n'
      hosts+="    - --disable-components=konnectivity-server"$'\n'
    fi
    hosts+="    ssh:"$'\n'
    hosts+="      address: $ip"$'\n'
    hosts+="      user: $SSH_USER"$'\n'
    hosts+="      port: 22"$'\n'
    hosts+="      keyPath: $SSH_PRIVKEY"$'\n'
  done

  require_api_vip "$controllers"
  local vip=""
  [[ -n "$API_VIP" ]] && vip="$(api_vip_addr)"
  local api_external="${vip:-${API_EXTERNAL_ADDRESS:-$cp_ip}}"
  if [[ -z "$api_external" ]]; then
    log "No controller found in the node table and no API_EXTERNAL_ADDRESS set"
    exit 1
  fi
  case $'\n'"$sans" in
    *$'\n'"            - $api_external"$'\n'*) ;;
    *) sans+="            - $api_external"$'\n' ;;
  esac

  # kubeconfig server preference: explicit override -> API_SAN -> the VIP ->
  # control-plane node's Tailscale IP -> its internal/static address. Off the
  # LAN the VIP is reached through the nodes' Tailscale subnet route.
  echo "${KUBECONFIG_SERVER:-${API_SAN:-${vip:-${cp_ts_ip:-$api_external}}}}" >"$KUBECONFIG_SERVER_FILE"

  local network=""
  if [[ -n "$API_VIP" ]]; then
    network="        network:"$'\n'
    network+="$(render_cplb "          ")"$'\n'
  fi

  local storage=""
  case "$K0S_STORAGE_TYPE" in
    etcd) ;;
    kine)
      [[ -n "$KINE_DATASOURCE" ]] || { log "K0S_STORAGE_TYPE=kine needs KINE_DATASOURCE"; exit 1; }
      case "$KINE_DATASOURCE" in
        *'"'*|*\\*) log "KINE_DATASOURCE must not contain a double quote or backslash"; exit 1 ;;
      esac
      storage="        storage:"$'\n'
      storage+="          type: kine"$'\n'
      storage+="          kine:"$'\n'
      storage+="            dataSource: \"$KINE_DATASOURCE\""$'\n'
      ;;
    *) log "K0S_STORAGE_TYPE must be etcd or kine, not '$K0S_STORAGE_TYPE'"; exit 1 ;;
  esac

  log "Control-plane entry: in-cluster=$api_external kubeconfig=$(cat "$KUBECONFIG_SERVER_FILE")"
  log "Writing k0sctl config to $K0SCTL_CONFIG (datastore: $K0S_STORAGE_TYPE)"
  ( umask 077; : >"$K0SCTL_CONFIG" )
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
$sans$network$storage
EOF
  } >"$K0SCTL_CONFIG"
  log_api_vip_followups
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
      [[ "$BACKEND" == "libvirt" ]] && fetch_base_image
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
    join)
      check_deps
      join_nodes
      ;;
    destroy)
      destroy_all
      ;;
    all)
      check_deps
      [[ "$BACKEND" == "libvirt" ]] && fetch_base_image
      create_vms
      k0s_config
      k0s_apply
      k0s_kubeconfig
      label_imagebuild_node
      deploy_flux
      ;;
    *)
      echo "Usage: $0 [all|vms|config|apply|kubeconfig|label|flux|join|destroy|--print-env-template]" >&2
      echo "  BACKEND=libvirt (default) | vsphere   (vsphere also needs BANLIEUE_ENV_FILE)" >&2
      echo "  DEFAULT_VMM=cloud-hypervisor (default) | libvirt   the nodes' VMM on the libvirt backend (ADR-0099)" >&2
      echo "  NODE_VMMS=\"cloud-hypervisor ... libvirt\"  per-node VMM, overriding DEFAULT_VMM" >&2
      echo "  JOIN_NODES=\"<name> ...\"  with 'join': add those workers or controllers to a RUNNING cluster" >&2
      echo "  FLUX_ENABLED=true         to enable the flux/all flux-operator deployment step" >&2
      exit 1
      ;;
  esac
}

# Sourced by scripts/bootstrap-k0s-cluster-test.sh for its functions only.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
