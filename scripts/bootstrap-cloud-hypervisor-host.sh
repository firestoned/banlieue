#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Prepare a Cloud Hypervisor host for banlieue's host-resident provider.
#
# The work is `banlieue host cloud-hypervisor` (ADR-0067, ADR-0084): the same
# binary the host then runs as its provider. This script is a thin wrapper
# around it, kept for two things the binary deliberately does not do:
#
#   --remote user@host   copy the binary to a host over SSH and run it there
#                        under sudo (the binary has no SSH client, ADR-0011);
#   BANLIEUE_ENV_FILE    the environment file earlier versions of this script
#                        read, translated to `banlieue host cloud-hypervisor` flags.
#
# On the host itself, run the binary directly:
#
#   sudo banlieue host cloud-hypervisor install
#
# From a workstation:
#
#   BANLIEUE_BINARY=target/release/banlieue \
#   BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
#     ./scripts/bootstrap-cloud-hypervisor-host.sh --remote admin@bar.foo.io all
#
# Steps: all (= install), preflight, vmm, host, tpm, polkit, provider,
# selftest, status. There is no packages step: the host's own package manager
# supplies systemd, swtpm and swtpm-tools (ADR-0084). Keep per-host values
# OUTSIDE this repository (they name real hosts); see --print-env-template.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd || true)"

usage() {
  cat >&2 <<'USAGE'
Usage: bootstrap-cloud-hypervisor-host.sh [step]
       bootstrap-cloud-hypervisor-host.sh --remote <user@host> [step]
       bootstrap-cloud-hypervisor-host.sh --print-env-template

  all        banlieue host cloud-hypervisor install
  preflight  banlieue host cloud-hypervisor preflight  (changes nothing)
  vmm|host|tpm|polkit|provider
             banlieue host cloud-hypervisor install --only <step>
  selftest   banlieue host cloud-hypervisor selftest   (boots nothing)
  status     banlieue host cloud-hypervisor status     (changes nothing)

BANLIEUE_BINARY is the banlieue binary to run (default: banlieue on PATH,
then target/release/banlieue). BANLIEUE_ENV_FILE is a file of settings; see
--print-env-template. Any other `banlieue host cloud-hypervisor` flag: run
the binary.
USAGE
  exit 1
}

print_env_template() {
  cat <<'TEMPLATE'
# banlieue Cloud Hypervisor host settings, for
# scripts/bootstrap-cloud-hypervisor-host.sh (BANLIEUE_ENV_FILE). Each maps to
# a `banlieue host cloud-hypervisor` flag. Keep this file OUTSIDE the
# repository: it names real hosts. Convention:
# $HOME/.config/banlieue/hosts/<name>.env.

# The Provider this host is (one Provider, one host -- ADR-0060).  --provider-name
#PROVIDER_NAME=bar
#PROVIDER_NAMESPACE=banlieue-system

# name=path, space-separated. Unset = `default` on the roomiest candidate mount.
#STORAGE_CLASSES="default=/srv/banlieue/ch fast=/nvme/banlieue/ch"

# name=bridge, space-separated. The bridge must exist; nothing creates one.
#NETWORK_CLASSES="default=br0"

# Guest uid range (one uid per guest). Must not overlap accounts or subuids.
#GUEST_UID_BASE=2000000
#GUEST_UID_COUNT=1024

# Registry for Url images, pulled by digest from this repository only.
#REGISTRY_REPOSITORY=registry.internal:5000/banlieue/disks
#REGISTRY_PLAIN_HTTP=false
#REGISTRY_KEEP_UNREFERENCED=1

# Lab hosts only: allow running inside a VM (nested virtualization).
#ALLOW_VIRTUALIZED_HOST=false

# Regenerate the host config and ROTATE THE EK CA.
#FORCE=false

# The VMM and firmware (ADR-0084). Unset = the release pinned in banlieue,
# from github.com. A version older than the pinned one is refused.
#VMM_VERSION=v54.0
#FIRMWARE_TAG=ch-97eeb7b09

# An air-gapped host: one HTTPS URL per file, e.g. an Artifactory remote.
#VMM_URL=https://internal.example.com/artifactory/vcs-github/cloud-hypervisor/cloud-hypervisor/v54.0/cloud-hypervisor-static
#CH_REMOTE_URL=https://internal.example.com/artifactory/vcs-github/cloud-hypervisor/cloud-hypervisor/v54.0/ch-remote-static
#FIRMWARE_URL=https://internal.example.com/artifactory/vcs-github/cloud-hypervisor/edk2/ch-97eeb7b09/CLOUDHV.fd

# sha256 of each file, for a version other than the pinned one, when the
# host cannot read GitHub's published digests (refused for a pinned file).
#VMM_SHA256=
#CH_REMOTE_SHA256=
#FIRMWARE_SHA256=

# Or no network at all: local copies of the three release assets.
#ARTIFACTS_DIR=/srv/banlieue/artifacts
TEMPLATE
}

# The step's `banlieue host cloud-hypervisor` verb and flags.
step_args() {
  case "$1" in
    all)       echo "install" ;;
    vmm|host|tpm|polkit|provider) echo "install --only $1" ;;
    preflight|selftest|status) echo "$1" ;;
    *) usage ;;
  esac
}

# Flags from the environment file's variables. Space-separated lists become
# repeated flags; nothing is passed that was not set.
settings_args() {
  local out=() pair
  [[ -n "${PROVIDER_NAME:-}" ]] && out+=(--provider-name "$PROVIDER_NAME")
  [[ -n "${PROVIDER_NAMESPACE:-}" ]] && out+=(--provider-namespace "$PROVIDER_NAMESPACE")
  for pair in ${STORAGE_CLASSES:-}; do out+=(--storage-class "$pair"); done
  for pair in ${NETWORK_CLASSES:-}; do out+=(--network-class "$pair"); done
  [[ -n "${GUEST_UID_BASE:-}" ]] && out+=(--guest-uid-base "$GUEST_UID_BASE")
  [[ -n "${GUEST_UID_COUNT:-}" ]] && out+=(--guest-uid-count "$GUEST_UID_COUNT")
  [[ -n "${REGISTRY_REPOSITORY:-}" ]] && out+=(--registry-repository "$REGISTRY_REPOSITORY")
  [[ "${REGISTRY_PLAIN_HTTP:-false}" == "true" ]] && out+=(--registry-plain-http)
  [[ -n "${REGISTRY_KEEP_UNREFERENCED:-}" ]] && out+=(--registry-keep-unreferenced "$REGISTRY_KEEP_UNREFERENCED")
  [[ "${ALLOW_VIRTUALIZED_HOST:-false}" == "true" ]] && out+=(--allow-virtualized-host)
  (( ${#out[@]} )) && printf '%q ' "${out[@]}"
  return 0
}

# The release flags `install` and `selftest` take.
release_args() {
  local out=()
  [[ -n "${VMM_VERSION:-}" ]] && out+=(--vmm-version "$VMM_VERSION")
  [[ -n "${FIRMWARE_TAG:-}" ]] && out+=(--firmware-tag "$FIRMWARE_TAG")
  [[ -n "${VMM_URL:-}" ]] && out+=(--vmm-url "$VMM_URL")
  [[ -n "${CH_REMOTE_URL:-}" ]] && out+=(--ch-remote-url "$CH_REMOTE_URL")
  [[ -n "${FIRMWARE_URL:-}" ]] && out+=(--firmware-url "$FIRMWARE_URL")
  [[ -n "${VMM_SHA256:-}" ]] && out+=(--vmm-sha256 "$VMM_SHA256")
  [[ -n "${CH_REMOTE_SHA256:-}" ]] && out+=(--ch-remote-sha256 "$CH_REMOTE_SHA256")
  [[ -n "${FIRMWARE_SHA256:-}" ]] && out+=(--firmware-sha256 "$FIRMWARE_SHA256")
  (( ${#out[@]} )) && printf '%q ' "${out[@]}"
  return 0
}

# Flags only `install` takes.
install_args() {
  local out=()
  [[ "${FORCE:-false}" == "true" ]] && out+=(--force)
  [[ -n "${ARTIFACTS_DIR:-}" ]] && out+=(--artifacts-dir "$ARTIFACTS_DIR")
  [[ -n "${1:-}" ]] && out+=(--provider-binary "$1")
  (( ${#out[@]} )) && printf '%q ' "${out[@]}"
  return 0
}

load_env_file() {
  [[ -n "${BANLIEUE_ENV_FILE:-}" ]] || return 0
  [[ -f "$BANLIEUE_ENV_FILE" ]] || { echo "BANLIEUE_ENV_FILE=$BANLIEUE_ENV_FILE not found" >&2; exit 1; }
  # shellcheck disable=SC1090  # path is operator-supplied by design
  source "$BANLIEUE_ENV_FILE"
}

find_binary() {
  local b="${BANLIEUE_BINARY:-${BANLIEUE_BINARY_SRC:-}}"
  [[ -z "$b" ]] && b="$(command -v banlieue || true)"
  [[ -z "$b" && -n "$REPO" ]] && b="$REPO/target/release/banlieue"
  [[ -x "$b" ]] || { echo "no banlieue binary (set BANLIEUE_BINARY; cargo build --release -p banlieue)" >&2; exit 1; }
  echo "$b"
}

# The command line to run on the host, with `binary` as the banlieue binary.
command_line() {
  local binary="$1" step="$2" verb
  verb="$(step_args "$step")"
  local line
  line="$(printf '%q' "$binary") host cloud-hypervisor $verb $(settings_args)"
  if [[ "$verb" == install* || "$verb" == selftest ]]; then
    line+=" $(release_args)"
  fi
  if [[ "$verb" == install* ]]; then
    # The binary being run is the one to install as the provider when asked.
    local install_bin=""
    [[ -n "${BANLIEUE_BINARY_SRC:-}" || -n "${INSTALL_PROVIDER_BINARY:-}" ]] && install_bin="$binary"
    line+=" $(install_args "$install_bin")"
  fi
  echo "$line"
}

case "${1:-all}" in
  --print-env-template) print_env_template; exit 0 ;;
  -h|--help) usage ;;
esac

if [[ "${1:-}" == "--remote" ]]; then
  target="${2:-}"
  [[ -n "$target" ]] || usage
  step="${3:-all}"
  step_args "$step" >/dev/null
  load_env_file
  binary="$(find_binary)"
  remote_dir="/tmp/banlieue-host.$$"
  echo "==> copying $binary to $target:$remote_dir" >&2
  # shellcheck disable=SC2029  # the path is chosen here, on purpose
  ssh "$target" "mkdir -m 0700 -p $remote_dir"
  scp -q "$binary" "$target:$remote_dir/banlieue"
  # Installed as the provider too, since it is the binary the host runs.
  INSTALL_PROVIDER_BINARY=1
  line="$(command_line "$remote_dir/banlieue" "$step")"
  echo "==> running on $target: sudo $line" >&2
  rc=0
  # -t: sudo needs a terminal to ask for a password.
  # shellcheck disable=SC2029
  ssh -t "$target" "sudo $line" || rc=$?
  # shellcheck disable=SC2029
  ssh "$target" "rm -rf $remote_dir" || true
  exit "$rc"
fi

step="${1:-all}"
step_args "$step" >/dev/null
load_env_file
binary="$(find_binary)"
line="$(command_line "$binary" "$step")"
echo "==> $line" >&2
eval "exec $line"
