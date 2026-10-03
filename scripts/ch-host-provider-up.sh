#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Bring a Cloud Hypervisor host up as a banlieue Provider in one root step:
# bootstrap the host, install the provider's kubeconfig, seed the image cache,
# and start the provider. Used for the live end-to-end run against a
# throwaway kind cluster (docs/src/guides/cloud-hypervisor-host.md).
#
# Everything host-specific comes from the environment; nothing is baked in:
#
#   PROVIDER_CREDENTIALS directory written by
#                        `banlieue bootstrap cloud-hypervisor-host`
#                        (a kubeconfig reading a token file, and the token);
#                        required
#   BANLIEUE_BINARY_SRC  banlieue binary to install; default: target/release
#   BASE_IMAGE           raw image to put in the first storage class's cache;
#                        optional
#   STORAGE_CLASSES      name=dir ...    (bootstrap-cloud-hypervisor-host.sh
#                        translates these to `banlieue host cloud-hypervisor` flags)
#   NETWORK_CLASSES      name=bridge ...
#   PROVIDER_NAME        the Provider object this host is; default: hostname
#
# Example:
#   banlieue bootstrap cloud-hypervisor-host --provider <name> --output-dir /tmp/creds
#   sudo PROVIDER_CREDENTIALS=/tmp/creds \
#        BASE_IMAGE=/var/lib/libvirt/images/kairos-ubuntu-2404.raw \
#        STORAGE_CLASSES="default=/srv/banlieue/ch" \
#        NETWORK_CLASSES="default=virbr0" \
#        scripts/ch-host-provider-up.sh
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
: "${PROVIDER_CREDENTIALS:?set PROVIDER_CREDENTIALS to the directory banlieue bootstrap cloud-hypervisor-host wrote}"
for f in kubeconfig token; do
  [[ -f "$PROVIDER_CREDENTIALS/$f" ]] || { echo "$PROVIDER_CREDENTIALS/$f missing" >&2; exit 1; }
done
export BANLIEUE_BINARY_SRC="${BANLIEUE_BINARY_SRC:-$REPO/target/release/banlieue}"
export STORAGE_CLASSES="${STORAGE_CLASSES:-}"
export NETWORK_CLASSES="${NETWORK_CLASSES:-}"
export PROVIDER_NAME="${PROVIDER_NAME:-$(hostname -s)}"
BANLIEUE_USER="${BANLIEUE_USER:-banlieue}"
CONF_DIR="${CONF_DIR:-/etc/banlieue}"
UNIT=banlieue-provider-cloud-hypervisor.service

[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 1; }
[[ -x "$BANLIEUE_BINARY_SRC" ]] || { echo "no binary at $BANLIEUE_BINARY_SRC (cargo build --release -p banlieue)" >&2; exit 1; }

"$REPO/scripts/bootstrap-cloud-hypervisor-host.sh" all

# The provider's identity, 0600 and its own: a kubeconfig that reads the token
# file beside it, which the provider renews there itself (ADR-0060 D5).
CREDENTIALS_DIR="${CREDENTIALS_DIR:-$CONF_DIR/credentials}"
install -d -m 0700 -o "$BANLIEUE_USER" -g "$BANLIEUE_USER" "$CREDENTIALS_DIR"
for f in kubeconfig token; do
  install -m 0600 -o "$BANLIEUE_USER" -g "$BANLIEUE_USER" "$PROVIDER_CREDENTIALS/$f" "$CREDENTIALS_DIR/$f"
done

if [[ -n "${BASE_IMAGE:-}" ]]; then
  # First storage class's image cache (ADR-0064 Decision 5: BackingFile).
  cache="$(awk -F' = ' '/^\[storage_classes\]/{f=1;next} /^\[/{f=0} f&&NF==2{gsub(/"/,"",$2); print $2; exit}' "$CONF_DIR/cloud-hypervisor.toml")/images"
  dest="$cache/$(basename "$BASE_IMAGE")"
  echo "Seeding $dest"
  cp --sparse=always "$BASE_IMAGE" "$dest"
  chown "$BANLIEUE_USER:$BANLIEUE_USER" "$dest"
  chmod 0640 "$dest"
fi

systemctl enable "$UNIT"
systemctl restart "$UNIT"
systemctl --no-pager status "$UNIT" | head -15
