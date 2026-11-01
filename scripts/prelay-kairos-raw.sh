#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# prelay-kairos-raw.sh: turn a Kairos raw image (AuroraBoot `disk.raw`) into a
# PRE-LAID raw image, so a clone boots straight into the installed system
# instead of a recovery boot, a reset and a reboot (ADR-0113).
#
# A raw image has only EFI, COS_OEM and COS_RECOVERY. Every clone's first
# boot runs recovery, whose reset lays out COS_STATE and COS_PERSISTENT, then
# reboots. That is over half of a sealed clone's time to GuestReady (roadmap
# 18 phase C: 57.8 s of 106 s). This script does that reset ONCE, at build
# time, and keeps the result:
#
#   1. copy the image (sparse) and grow it to the clones' disk size;
#   2. move the image's own cloud-configs off COS_OEM, keeping only
#      AuroraBoot's 01_reset.yaml, plus a throwaway admin user with a random
#      password that is never stored (with no admin user Kairos runs none of
#      the configured stages, so the reset never starts). The build-time boot
#      runs no operator stage and has no kcrypt section: first-boot sealing
#      must happen in each clone, with its own vTPM, never here;
#   3. boot it once in QEMU with -no-reboot, no TPM and no network:
#      01_reset.yaml runs the reset in its `network` stage (after networkd's
#      120 s wait for a network that is not there), and QEMU exits at the
#      reboot that follows. Starting the reset from the `boot` stage instead
#      was tried and hangs;
#   4. give COS_PERSISTENT a fresh, empty filesystem (the build boot logged
#      into it, including a journal directory named after its machine-id);
#   5. put the image's own cloud-configs back on COS_OEM (01_reset.yaml
#      stays gone, so a clone never resets), and set GRUB's menu timeout to 0
#      in COS_OEM's grubenv (GRUB waits 10 s per boot otherwise);
#   6. check the result: COS_STATE and an empty, plaintext COS_PERSISTENT
#      exist, nothing is LUKS, no build boot failure.
#
# Every clone of the result boots once, seals COS_PERSISTENT to its OWN vTPM
# (kcrypt encrypt_on_boot from the restored config) and reports. The image
# itself is never sealed: never clone a sealed disk (ADR-0040).
#
# Usage (as root, on a host with KVM):
#   scripts/prelay-kairos-raw.sh <input.raw> <output.raw> [disk-size-GiB]
#
#   disk-size-GiB  the clones' OS disk size (default 20). COS_PERSISTENT
#                  fills it and does not grow on a larger disk; give the
#                  clones' class exactly this size (ADR-0113 Decision 4).
#
# With no arguments, the three come from PRELAY_INPUT, PRELAY_OUTPUT and
# PRELAY_DISK_SIZE_GIB: that is how banlieue-imagebuilder's pre-lay Job runs
# it (ADR-0113, Amendment 2026-11-01).
#
# Environment:
#   OVMF_CODE      UEFI firmware code (default: first of the usual paths)
#   OVMF_VARS      UEFI variable template (default: next to OVMF_CODE)
#   PRELAY_TIMEOUT seconds the build boot may take (default 900)
#   PRELAY_VERIFY_CHECKSUM  <alg>:<hex> (sha256 or sha512) the input must
#                  match before anything is done to it (SEC-004)
#   PRELAY_RESULT_FILE  where to write the output's sha256:<hex> when done
#                  (the Job's /dev/termination-log)
#
# Needs: qemu-system-x86_64, sgdisk, losetup, blkid, mke2fs, mount, truncate.
set -euo pipefail

readonly DEFAULT_DISK_SIZE_GIB=20
readonly DEFAULT_TIMEOUT_SECS=900
readonly GRUBENV_BLOCK_BYTES=1024
readonly GRUBENV_HEADER="# GRUB Environment Block"
readonly RESET_CONFIG="01_reset.yaml"
readonly BUILD_CONFIG="00_banlieue_prelay.yaml"
readonly BUILD_USER="banlieue-prelay"
readonly BUILD_PASSWORD_BYTES=24
readonly ACTIVE_IMAGE="cOS/active.img"
# QEMU's seccomp sandbox: the guest is the image being built, run once with
# no network and no TPM; it has no business spawning, escalating or
# reconfiguring anything on the build node.
readonly QEMU_SANDBOX="on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny"
readonly BUILD_VCPUS=2
readonly BUILD_MEMORY_MIB=4096
readonly OVMF_CODE_CANDIDATES=(
  /usr/share/OVMF/OVMF_CODE_4M.fd
  /usr/share/OVMF/OVMF_CODE.fd
  /usr/share/edk2/ovmf/OVMF_CODE.fd
  /usr/share/qemu/ovmf-x86_64-code.bin
)

die() { echo "prelay: $*" >&2; exit 1; }
say() { echo "prelay: $*" >&2; }

# grubenv_set <file> <key> <value>: set one variable in a GRUB environment
# block, keeping the others, padded with '#' to exactly 1024 bytes as GRUB's
# load_env requires. Pure file edit; no grub-editenv needed.
grubenv_set() {
  local file=$1 key=$2 value=$3 body
  body=$(
    if [[ -f $file ]]; then
      grep -v '^#' "$file" | tr -d '\0' | grep -v "^${key}=" | grep '=' || true
    fi
    printf '%s=%s\n' "$key" "$value"
  )
  # Command substitution strips trailing newlines; put the last one back,
  # or the '#' padding would run on into the last value.
  local content
  content="$(printf '%s\n%s' "$GRUBENV_HEADER" "$body")"$'\n'
  local len=${#content}
  ((len < GRUBENV_BLOCK_BYTES)) || die "grubenv content exceeds ${GRUBENV_BLOCK_BYTES} bytes"
  {
    printf '%s' "$content"
    printf '%*s' $((GRUBENV_BLOCK_BYTES - len)) '' | tr ' ' '#'
  } >"$file"
}

# part_by_label <loopdev> <label>: the partition device carrying <label>.
part_by_label() {
  local dev=$1 label=$2 p
  for p in "$dev"p*; do
    [[ $(blkid -s LABEL -o value "$p" 2>/dev/null) == "$label" ]] && { echo "$p"; return 0; }
  done
  return 1
}

# verify_checksum <file> <alg>:<hex>: fail unless <file> matches.
verify_checksum() {
  local file=$1 expected=$2 alg hex actual
  alg=${expected%%:*}
  hex=${expected#*:}
  case $alg in
    sha256 | sha512) ;;
    *) die "unsupported checksum algorithm '$alg' (sha256 or sha512)" ;;
  esac
  [[ $hex =~ ^[0-9a-f]+$ ]] || die "checksum '$expected' is not <alg>:<lowercase hex>"
  actual=$("${alg}sum" "$file" | cut -d' ' -f1)
  [[ $actual == "$hex" ]] || die "checksum mismatch for $file: expected $expected, computed $alg:$actual"
}

find_ovmf() {
  if [[ -n ${OVMF_CODE:-} ]]; then echo "$OVMF_CODE"; return; fi
  local c
  for c in "${OVMF_CODE_CANDIDATES[@]}"; do [[ -f $c ]] && { echo "$c"; return; }; done
  die "no UEFI firmware found; set OVMF_CODE"
}

main() {
  if [[ $# -eq 0 && -n ${PRELAY_INPUT:-} ]]; then
    set -- "$PRELAY_INPUT" "${PRELAY_OUTPUT:-}" "${PRELAY_DISK_SIZE_GIB:-}"
  fi
  [[ $# -ge 2 && $# -le 3 && -n $2 ]] || die "usage: $0 <input.raw> <output.raw> [disk-size-GiB]"
  # Globals, not locals: the EXIT trap runs after main returns.
  input=$1 out=$2
  local size_gib=${3:-$DEFAULT_DISK_SIZE_GIB}
  [[ $size_gib =~ ^[0-9]+$ ]] && ((size_gib > 0)) || die "disk size must be a positive number of GiB"
  [[ -f $input ]] || die "input $input not found"
  [[ ! -e $out ]] || die "output $out exists; remove it first"
  ((EUID == 0)) || die "run as root (loop devices, mounts)"
  local t
  for t in qemu-system-x86_64 sgdisk losetup blkid mke2fs mount truncate; do
    command -v "$t" >/dev/null || die "$t not found"
  done
  [[ -e /dev/kvm ]] || die "/dev/kvm missing: the build boot needs KVM"
  local code vars timeout=${PRELAY_TIMEOUT:-$DEFAULT_TIMEOUT_SECS}
  code=$(find_ovmf)
  vars=${OVMF_VARS:-${code/CODE/VARS}}
  [[ -f $vars ]] || die "no UEFI variable template at $vars; set OVMF_VARS"

  loop="" done=0
  work=$(mktemp -d)
  mnt="$work/mnt"; mkdir "$mnt"
  # On any failure, leave nothing that could be mistaken for a good image.
  cleanup() {
    mountpoint -q "$mnt" && umount "$mnt"
    [[ -n $loop ]] && losetup -d "$loop" 2>/dev/null
    if ((done == 0)); then
      [[ -f $work/serial.log ]] && cp "$work/serial.log" "$out.failed-console.log" 2>/dev/null
      rm -f "$out"
    fi
    rm -rf "$work"
  }
  trap cleanup EXIT

  if [[ -n ${PRELAY_VERIFY_CHECKSUM:-} ]]; then
    say "verifying $input against $PRELAY_VERIFY_CHECKSUM"
    verify_checksum "$input" "$PRELAY_VERIFY_CHECKSUM"
  fi
  say "copying $input to $out, grown to ${size_gib} GiB"
  cp --sparse=always "$input" "$out"
  truncate -s "${size_gib}G" "$out"
  sgdisk -e "$out" >/dev/null

  loop=$(losetup -fP --show "$out")
  local oem
  oem=$(part_by_label "$loop" COS_OEM) || die "no COS_OEM partition: not a Kairos raw image"
  [[ -z $(part_by_label "$loop" COS_STATE || true) ]] || die "input already has COS_STATE: already laid out"
  mount "$oem" "$mnt"
  [[ -f $mnt/$RESET_CONFIG ]] || die "no $RESET_CONFIG on COS_OEM: not an AuroraBoot raw image"
  mkdir "$work/oem"
  local f
  for f in "$mnt"/*.yaml "$mnt"/*.yml; do
    [[ -e $f && $(basename "$f") != "$RESET_CONFIG" ]] || continue
    cp -p "$f" "$work/oem/" && rm -f "$f"
  done
  # Kairos runs no configured stage without an admin user, and the reset is
  # one. This one lives only in the recovery boot's in-memory /etc, and its
  # config is deleted below.
  printf '%s\n' '#cloud-config' 'users:' "  - name: $BUILD_USER" \
    "    passwd: \"$(head -c "$BUILD_PASSWORD_BYTES" /dev/urandom | base64 | tr -d '/+=\n')\"" \
    '    groups: [admin]' >"$mnt/$BUILD_CONFIG"
  chmod 0600 "$mnt/$BUILD_CONFIG"
  say "build boot runs with only $RESET_CONFIG and $BUILD_CONFIG; kept aside: $(ls "$work/oem" | tr '\n' ' ')"
  umount "$mnt"; losetup -d "$loop"; loop=""

  cp "$vars" "$work/vars.fd"
  say "build boot (recovery, reset; QEMU exits at the reboot)"
  local started=$SECONDS rc=0
  timeout "$timeout" qemu-system-x86_64 -name prelay -enable-kvm -machine q35 -cpu host \
    -smp "$BUILD_VCPUS" -m "$BUILD_MEMORY_MIB" -no-reboot \
    -drive if=pflash,format=raw,readonly=on,file="$code" \
    -drive if=pflash,format=raw,file="$work/vars.fd" \
    -drive file="$out",if=virtio,format=raw \
    -nic none -sandbox "$QEMU_SANDBOX" \
    -display none -serial file:"$work/serial.log" -monitor none || rc=$?
  ((rc == 0)) || die "build boot failed or timed out (rc=$rc); console kept as $out.failed-console.log"
  if grep -aq 'KAIROS BOOT FAILED' "$work/serial.log"; then
    grep -a -A12 'KAIROS BOOT FAILED' "$work/serial.log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' >&2
    die "the build boot failed"
  fi
  say "build boot done in $((SECONDS - started)) s"

  loop=$(losetup -fP --show "$out")
  local state persistent fstype
  state=$(part_by_label "$loop" COS_STATE) || die "no COS_STATE after the build boot: the reset did not run"
  persistent=$(part_by_label "$loop" COS_PERSISTENT) || die "no COS_PERSISTENT after the build boot"
  # The partitions alone prove nothing: 01_reset.yaml's layout stage creates
  # them, empty, before the reset runs. Only the reset writes the system.
  mount -o ro "$state" "$mnt"
  [[ -s $mnt/$ACTIVE_IMAGE ]] || die "COS_STATE has no $ACTIVE_IMAGE: the reset did not run (console kept as $out.failed-console.log)"
  umount "$mnt"
  fstype=$(blkid -s TYPE -o value "$persistent")
  [[ $fstype != crypto_LUKS ]] || die "COS_PERSISTENT is LUKS after the build boot: it must never be sealed here"
  say "COS_STATE is $state; giving COS_PERSISTENT ($persistent) a fresh, empty $fstype filesystem"
  mke2fs -q -F -t "$fstype" -L COS_PERSISTENT "$persistent"

  oem=$(part_by_label "$loop" COS_OEM)
  mount "$oem" "$mnt"
  rm -f "$mnt"/*.yaml "$mnt"/*.yml "$mnt/grubcustom"
  cp -p "$work/oem"/* "$mnt/" 2>/dev/null || true
  grubenv_set "$mnt/grubenv" timeout 0
  say "COS_OEM now: $(ls "$mnt" | tr '\n' ' ')"
  umount "$mnt"

  local p
  for p in "$loop"p*; do
    [[ $(blkid -s TYPE -o value "$p" 2>/dev/null) != crypto_LUKS ]] || die "$p is LUKS: refusing a sealed image"
  done
  mount -o ro "$persistent" "$mnt"
  [[ -z $(find "$mnt" -mindepth 1 -not -name lost+found -print -quit) ]] || die "COS_PERSISTENT is not empty"
  umount "$mnt"
  losetup -d "$loop"; loop=""
  if [[ -n ${PRELAY_RESULT_FILE:-} ]]; then
    printf 'sha256:%s\n' "$(sha256sum "$out" | cut -d' ' -f1)" >"$PRELAY_RESULT_FILE"
  fi
  done=1
  say "pre-laid image written: $out"
}

# Sourced by the test with PRELAY_SOURCE_ONLY=1, for its pure functions.
if [[ ${PRELAY_SOURCE_ONLY:-} != 1 ]]; then
  main "$@"
fi
