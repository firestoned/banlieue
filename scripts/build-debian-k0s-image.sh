#!/usr/bin/env bash
# Builds a Debian 13 cloud image with a specific k0s version baked in
# (binary only -- no systemd unit, nothing enabled; k0sctl decides the
# role and runs `k0s install <role>` itself at cluster-apply time).
#
# Works on a COPY of the base image via virt-customize (libguestfs) --
# no VM boot required, and the base image (which may be a shared backing
# file for other VMs on this host) is never touched. Every step is
# idempotent: re-running skips the libguestfs-tools install / customize
# step if already done, unless FORCE=1.
#
# Usage:
#   K0S_VERSION=v1.31.1+k0s.0 ./build-debian-k0s-image.sh [build|clean]
set -euo pipefail

K0S_VERSION="${K0S_VERSION:?set K0S_VERSION, e.g. v1.31.1+k0s.0}"

# A plain Debian 13 cloud-init image, e.g. from
# https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2
# -- reused as-is if already present (checked before downloading).
SOURCE_IMAGE="${SOURCE_IMAGE:-/var/lib/libvirt/images/debian-13-genericcloud-amd64.qcow2}"
SOURCE_IMAGE_URL="${SOURCE_IMAGE_URL:-https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2}"

WORKDIR="${WORKDIR:-$HOME/.local/share/k0s-bootstrap}"
OUT_DIR="${OUT_DIR:-$WORKDIR/debian-images}"

FORCE="${FORCE:-0}"

log() { echo "==> $*" >&2; }

K0S_VERSION_SAFE="$(echo "$K0S_VERSION" | tr '+' '-')"
FINAL_QCOW2="$OUT_DIR/debian-13-k0s-${K0S_VERSION_SAFE}.qcow2"

mkdir -p "$OUT_DIR"

ensure_source_image() {
  if [[ -f "$SOURCE_IMAGE" ]]; then
    log "Source image already present at $SOURCE_IMAGE, skipping download"
    return
  fi
  log "Downloading Debian 13 cloud image from $SOURCE_IMAGE_URL"
  curl -fL --output "$SOURCE_IMAGE.tmp" "$SOURCE_IMAGE_URL"
  mv "$SOURCE_IMAGE.tmp" "$SOURCE_IMAGE"
}

ensure_libguestfs() {
  command -v virt-customize >/dev/null 2>&1 && return
  log "Installing libguestfs-tools"
  apt-get update -qq
  apt-get install -y -qq libguestfs-tools
}

build_image() {
  if [[ "$FORCE" != "1" && -f "$FINAL_QCOW2" ]]; then
    log "$FINAL_QCOW2 already exists, skipping (FORCE=1 to rebuild)"
    return
  fi

  log "Copying $SOURCE_IMAGE -> $FINAL_QCOW2 (never modifying the shared source in place)"
  cp --reflink=auto "$SOURCE_IMAGE" "$FINAL_QCOW2" 2>/dev/null || cp "$SOURCE_IMAGE" "$FINAL_QCOW2"

  log "Baking k0s $K0S_VERSION into $FINAL_QCOW2 via virt-customize"
  # Debian's dpkg arch names (amd64/arm64) already match k0s's release
  # asset naming, no translation needed.
  virt-customize -a "$FINAL_QCOW2" \
    --run-command "curl -sSLf -o /usr/local/bin/k0s https://github.com/k0sproject/k0s/releases/download/${K0S_VERSION}/k0s-${K0S_VERSION}-\$(dpkg --print-architecture)" \
    --run-command "chmod +x /usr/local/bin/k0s" \
    --run-command "/usr/local/bin/k0s version"

  log "Sysprepping $FINAL_QCOW2 for cloning (machine-id, ssh host keys)"
  # virt-customize's own finalization ("Setting the machine ID in
  # /etc/machine-id") bakes a FIXED machine-id into the image -- every VM
  # cloned from it (via qemu-img backing-file, as create_vm() does) would
  # then share that same id, and systemd-networkd derives a DHCP client
  # identifier (DUID) from it, so cloned VMs collide/shadow each other's
  # leases. virt-sysprep's machine-id/ssh-hostkeys operations clear both
  # (empty machine-id -> systemd-machine-id-setup regenerates a fresh one
  # per boot; likewise cloud-init regenerates host keys), restoring the
  # standard "safe to clone" cloud-image convention.
  virt-sysprep -a "$FINAL_QCOW2" --operations machine-id,ssh-hostkeys

  log "Done: $FINAL_QCOW2"
  log "Use it with bootstrap-k0s-cluster.sh via: BASE_IMAGE_PATH=$FINAL_QCOW2"
}

clean() {
  log "Removing $FINAL_QCOW2"
  rm -f "$FINAL_QCOW2"
}

main() {
  local cmd="${1:-build}"
  case "$cmd" in
    build)
      ensure_source_image
      ensure_libguestfs
      build_image
      ;;
    clean)
      clean
      ;;
    *)
      echo "Usage: $0 [build|clean]" >&2
      exit 1
      ;;
  esac
}

main "$@"
