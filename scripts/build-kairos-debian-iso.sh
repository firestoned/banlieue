#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Build a Kairos installer ISO on a Debian base, for
# scripts/bootstrap-k0s-cluster.sh (LIBVIRT_IMAGE_KIND=kairos, BASE_IMAGE_PATH).
#
# Kairos v4 publishes installer ISOs only for Hadron, its own distribution.
# Any other base is built the way Kairos documents ("bring your own image"):
# kairos-init turns a stock distro container image into a Kairos image, and
# AuroraBoot turns that image into an installer ISO.
#
#   scripts/build-kairos-debian-iso.sh            # -> $OUT_DIR/*.iso + .sha256
#   DEBIAN_VERSION=13 OUT_DIR=/tmp/iso scripts/build-kairos-debian-iso.sh
#
# The image carries no k0s: k0sctl installs exactly K0S_VERSION on every node
# (bootstrap-k0s-cluster.sh). It does carry qemu-guest-agent, so a bridged VM's
# address can be read from the hypervisor without libvirt's DHCP leases.
#
# Needs podman (rootless is fine) or docker. With podman, AuroraBoot reads the
# image through podman's docker-compatible API socket.
set -euo pipefail

DEBIAN_VERSION="${DEBIAN_VERSION:-13}"
KAIROS_INIT_VERSION="${KAIROS_INIT_VERSION:-v0.17.3}"
AURORABOOT_VERSION="${AURORABOOT_VERSION:-v0.27.1}"
# Recorded in the image's /etc/kairos-release.
IMAGE_VERSION="${IMAGE_VERSION:-v0.1.0}"
IMAGE_TAG="${IMAGE_TAG:-localhost/kairos-debian:${DEBIAN_VERSION}-${IMAGE_VERSION}}"
OUT_DIR="${OUT_DIR:-$PWD/build/kairos-debian}"
RUNTIME="${CONTAINER_RUNTIME:-$(command -v podman >/dev/null && echo podman || echo docker)}"

log() { echo "==> $*" >&2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$OUT_DIR"

cat >"$work/Containerfile" <<EOF
FROM docker.io/library/debian:${DEBIAN_VERSION}
# The guest agent lets the hypervisor read the VM's addresses on a bridge.
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends qemu-guest-agent \\
 && rm -rf /var/lib/apt/lists/*
RUN --mount=type=bind,from=quay.io/kairos/kairos-init:${KAIROS_INIT_VERSION},src=/kairos-init,dst=/kairos-init \\
    /kairos-init --version "${IMAGE_VERSION}"
RUN systemctl enable qemu-guest-agent || true
EOF

log "Building $IMAGE_TAG (debian:$DEBIAN_VERSION + kairos-init $KAIROS_INIT_VERSION) with $RUNTIME"
"$RUNTIME" build -t "$IMAGE_TAG" -f "$work/Containerfile" "$work"

sock=/var/run/docker.sock
if [[ "$RUNTIME" == podman ]]; then
  sock="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/podman/podman.sock"
  [[ -S "$sock" ]] || { log "no podman socket at $sock (systemctl --user enable --now podman.socket)"; exit 1; }
fi

log "Building the ISO with AuroraBoot $AURORABOOT_VERSION"
"$RUNTIME" run --rm --privileged \
  -v "$sock:/var/run/docker.sock" \
  -v "$OUT_DIR:/output" \
  "quay.io/kairos/auroraboot:${AURORABOOT_VERSION}" \
  build-iso --output /output/ "oci:${IMAGE_TAG}"

iso="$(find "$OUT_DIR" -maxdepth 1 -name '*.iso' -newer "$work/Containerfile" -print -quit)"
[[ -n "$iso" ]] || { log "AuroraBoot produced no ISO in $OUT_DIR"; exit 1; }
sha256sum "$iso" | awk '{print $1}' >"$iso.sha256"
log "ISO: $iso"
log "sha256: $(cat "$iso.sha256")  (pin it as IMAGE_SHA256 with BASE_IMAGE_PATH=$iso)"
