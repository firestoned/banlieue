#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Tests for scripts/ch-storage-swap-to-xfs.sh.
#
# Without root: the pure functions (fstab swap retirement, the resume rewrite,
# the kernel command line check, the memory rule, sizes) and the refusals
# main makes before it touches anything.
#
# With CH_XFS_LIVE=1, as root, on a Linux host with mkfs.xfs: the whole move
# against a loop device used as the swap partition, with copies of fstab, the
# resume file and the kernel command line, and provider and guest unit names
# that cannot exist, so nothing of the host's own is involved.
#
# Usage: scripts/ch-storage-swap-to-xfs-test.sh
#        sudo CH_XFS_LIVE=1 scripts/ch-storage-swap-to-xfs-test.sh
#
# The `bash -c '...' _ "$arg"` checks below expand "$1" in the child on
# purpose: single quotes are the point.
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ch-storage-swap-to-xfs.sh"
WORK="$(mktemp -d)"
LOOP=""
cleanup() {
  swapoff "$WORK/swapfile" 2>/dev/null || true
  umount "$WORK/store/ch" 2>/dev/null || true
  if [[ -n $LOOP ]]; then
    swapoff "$LOOP" 2>/dev/null || true
    losetup -d "$LOOP" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

failures=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }
check() { local name=$1; shift; if "$@"; then ok "$name"; else fail "$name"; fi; }
refuses() { local name=$1; shift; if ("$@") >/dev/null 2>&1; then fail "$name"; else ok "$name"; fi; }

# shellcheck source-path=SCRIPTDIR source=ch-storage-swap-to-xfs.sh
CH_XFS_SWAP_SOURCE_ONLY=1 source "$SCRIPT"
set +e

# ---- check_storage_dir ------------------------------------------------------

check "accepts a storage directory" check_storage_dir /srv/banlieue/ch
refuses "refuses an empty path" check_storage_dir ""
refuses "refuses a relative path" check_storage_dir srv/banlieue/ch
refuses "refuses a trailing slash" check_storage_dir /srv/banlieue/ch/
refuses "refuses /home" check_storage_dir /home
refuses "refuses /srv itself" check_storage_dir /srv
refuses "refuses under /etc" check_storage_dir /etc/banlieue

# ---- fstab_comment_swap -----------------------------------------------------

fstab="$WORK/fstab"
cat >"$fstab" <<'EOF'
UUID=aaaa / ext4 errors=remount-ro 0 1
# swap was on /dev/sda4 during installation
UUID=596154be-9397 none            swap    sw              0       0
/dev/sdb2 none swap sw 0 0
/home/swapfile none swap sw 0 0
EOF
out=$(fstab_comment_swap "$fstab" 596154be-9397 /dev/sda4)
check "the partition's swap line is commented out" grep -q '^# UUID=596154be-9397 .*retired by ch-storage-swap-to-xfs' <<<"$out"
check "another partition's swap line is kept" grep -qx '/dev/sdb2 none swap sw 0 0' <<<"$out"
check "a swap file's line is kept" grep -qx '/home/swapfile none swap sw 0 0' <<<"$out"
check "the root line is kept" grep -q '^UUID=aaaa / ' <<<"$out"
check "the line count is unchanged" test "$(wc -l <<<"$out")" -eq "$(wc -l <"$fstab")"
check "matches by device path too" bash -c 'grep -q "^# /dev/sdb2" <<<"$1"' _ "$(fstab_comment_swap "$fstab" none-such /dev/sdb2)"
refuses "refuses a partition with no swap line" fstab_comment_swap "$fstab" bbbb /dev/sdc1
printf '/dev/sda4 none swap sw 0 0\n' >>"$fstab"
refuses "refuses two swap lines for one partition" fstab_comment_swap "$fstab" 596154be-9397 /dev/sda4

# ---- resume -----------------------------------------------------------------

printf 'RESUME=UUID=596154be-9397\n' >"$WORK/resume"
check "a resume file naming the partition refers to it" resume_refers "$WORK/resume" 596154be-9397 /dev/sda4
refuses "one naming another does not" resume_refers "$WORK/resume" cccc /dev/sdc1
refuses "a missing resume file does not" resume_refers "$WORK/absent" 596154be-9397 /dev/sda4
check "the rewrite says RESUME=none" test "$(resume_rewrite "$WORK/resume" 596154be-9397 /dev/sda4)" = "RESUME=none"
printf 'RESUME=/dev/sda4\n' >"$WORK/resume2"
check "a device-path resume is rewritten too" test "$(resume_rewrite "$WORK/resume2" 596154be-9397 /dev/sda4)" = "RESUME=none"

printf 'BOOT_IMAGE=/vmlinuz root=UUID=aaaa ro quiet\n' >"$WORK/cmdline"
refuses "a command line without resume= does not resume" cmdline_resumes_from "$WORK/cmdline" 596154be-9397 /dev/sda4
printf 'BOOT_IMAGE=/vmlinuz root=UUID=aaaa resume=UUID=596154be-9397 ro\n' >"$WORK/cmdline"
check "a command line with the partition's resume= does" cmdline_resumes_from "$WORK/cmdline" 596154be-9397 /dev/sda4

# ---- swapoff_fits, size_kib --------------------------------------------------

check "swap in use fits in memory plus the new file" swapoff_fits 2561128 16739308 16777216
check "it fits in the new file alone" swapoff_fits 1000 0 1100
refuses "not when neither has room" swapoff_fits 10000 5000 4000
check "16G is 16 GiB in KiB" test "$(size_kib 16G)" -eq 16777216
check "512M is 512 MiB in KiB" test "$(size_kib 512M)" -eq 524288
refuses "a size with a decimal is refused" size_kib 1.5G

# ---- main's refusals, before anything changes --------------------------------

refuses "main refuses no arguments" bash "$SCRIPT"
refuses "main refuses no storage directory" bash "$SCRIPT" --swapfile /home/swapfile /dev/sda4
refuses "main refuses a relative swap file" bash "$SCRIPT" --swapfile swapfile /dev/sda4 /srv/banlieue/ch
refuses "main refuses a bad size" bash "$SCRIPT" --swapfile /home/swapfile --swapfile-size lots /dev/sda4 /srv/banlieue/ch
refuses "main refuses three positionals" bash "$SCRIPT" --swapfile /home/swapfile /dev/sda4 /srv/banlieue/ch extra
if ((EUID != 0)); then
  refuses "main refuses to run without root" bash "$SCRIPT" --swapfile /home/swapfile /dev/sda4 /srv/banlieue/ch
fi

# ---- live: the whole move with a loop device as the swap partition ----------

if [[ ${CH_XFS_LIVE:-} == 1 ]]; then
  if ((EUID != 0)); then
    fail "CH_XFS_LIVE=1 needs root"
  elif ! command -v mkfs.xfs >/dev/null; then
    fail "CH_XFS_LIVE=1 needs mkfs.xfs (xfsprogs)"
  else
    truncate -s 600M "$WORK/swap.img"
    LOOP=$(losetup --find --show "$WORK/swap.img")
    mkswap -q "$LOOP" >/dev/null
    swapon "$LOOP"
    suuid=$(blkid -s UUID -o value "$LOOP")
    # Only lines that resolve: findmnt --verify must accept it before the run.
    printf 'UUID=%s none swap sw 0 0\n' "$suuid" >"$WORK/fstab-live"
    printf 'RESUME=UUID=%s\n' "$suuid" >"$WORK/resume-live"
    printf 'BOOT_IMAGE=/vmlinuz root=UUID=aaaa ro quiet\n' >"$WORK/cmdline-live"
    # The storage directory, as on a CH host: provider-owned, private, an
    # image, a hard link and a sparse disk.
    mkdir -p "$WORK/store/ch/images"
    chown -R nobody:nogroup "$WORK/store/ch"
    chmod 0711 "$WORK/store/ch"
    chmod 0750 "$WORK/store/ch/images"
    head -c 4096 /dev/urandom >"$WORK/store/ch/images/base.raw"
    ln "$WORK/store/ch/images/base.raw" "$WORK/store/ch/images/base.link"
    truncate -s 256M "$WORK/store/ch/images/sparse.raw"
    chown nobody:nogroup "$WORK/store/ch/images/"*
    sum_before=$(sha256sum "$WORK/store/ch/images/base.raw" | cut -d' ' -f1)
    LIVE_ENV=(env CH_XFS_FSTAB="$WORK/fstab-live" CH_XFS_RESUME_CONF="$WORK/resume-live"
      CH_XFS_CMDLINE="$WORK/cmdline-live"
      CH_XFS_PROVIDER_UNIT="banlieue-test-no-such-provider-$$.service"
      CH_XFS_GUEST_UNITS="banlieue-test-no-such-guest-$$@*")
    args=(--swapfile "$WORK/swapfile" --swapfile-size 64M "$LOOP" "$WORK/store/ch")

    "${LIVE_ENV[@]}" bash "$SCRIPT" "${args[@]}" >"$WORK/dry.out" 2>&1
    check "live: a dry run succeeds" test $? -eq 0
    check "live: a dry run changes nothing" bash -c 'grep -q "^$1 " /proc/swaps && [[ ! -e $2 ]]' _ "$LOOP" "$WORK/swapfile"
    check "live: the plan includes the resume change" grep -q 'RESUME=none' "$WORK/dry.out"

    printf 'UUID=00000000-dead-beef-0000-000000000000 /nowhere ext4 defaults 0 2\n' >"$WORK/fstab-broken"
    cat "$WORK/fstab-live" >>"$WORK/fstab-broken"
    env CH_XFS_FSTAB="$WORK/fstab-broken" CH_XFS_RESUME_CONF="$WORK/resume-live" CH_XFS_CMDLINE="$WORK/cmdline-live" \
      CH_XFS_PROVIDER_UNIT="banlieue-test-no-such-provider-$$.service" CH_XFS_GUEST_UNITS="banlieue-test-no-such-guest-$$@*" \
      bash "$SCRIPT" "${args[@]}" >"$WORK/broken.out" 2>&1
    check "live: an fstab that is already broken is refused up front" grep -q 'already rejects' "$WORK/broken.out"

    "${LIVE_ENV[@]}" bash "$SCRIPT" --apply "${args[@]}" >"$WORK/apply.out" 2>&1
    rc=$?
    check "live: --apply succeeds" test $rc -eq 0
    [[ $rc -eq 0 ]] || sed 's/^/     /' "$WORK/apply.out"
    check "live: the swap file is on" grep -q "^$WORK/swapfile " /proc/swaps
    check "live: the partition is no longer swap" bash -c '! grep -q "^$1 " /proc/swaps' _ "$LOOP"
    check "live: the partition is XFS" test "$(blkid -s TYPE -o value "$LOOP")" = xfs
    check "live: mounted at the storage directory" test "$(findmnt -no SOURCE -M "$WORK/store/ch")" = "$LOOP"
    check "live: with reflinks" bash -c 'xfs_info "$1" | grep -q reflink=1' _ "$WORK/store/ch"
    check "live: fstab retires the old swap line" grep -q "^# UUID=$suuid none swap" "$WORK/fstab-live"
    check "live: fstab has the swap file" grep -qP "^\Q$WORK/swapfile\E\tnone\tswap" "$WORK/fstab-live"
    check "live: fstab mounts the new filesystem" grep -qP "^UUID=$(blkid -s UUID -o value "$LOOP")\t\Q$WORK/store/ch\E\txfs" "$WORK/fstab-live"
    check "live: findmnt accepts the fstab" findmnt --verify --tab-file "$WORK/fstab-live"
    check "live: resume points nowhere" grep -qx 'RESUME=none' "$WORK/resume-live"
    check "live: the data survived" test "$(sha256sum "$WORK/store/ch/images/base.raw" | cut -d' ' -f1)" = "$sum_before"
    check "live: owners survived" test "$(stat -c %U:%G "$WORK/store/ch/images/base.raw")" = nobody:nogroup
    check "live: the mountpoint's mode is the old directory's" test "$(stat -c %a "$WORK/store/ch")" = 711
    check "live: the hard link survived" test "$(stat -c %h "$WORK/store/ch/images/base.raw")" -eq 2
    check "live: the sparse file stayed sparse" test "$(du -k "$WORK/store/ch/images/sparse.raw" | cut -f1)" -lt 1024
    check "live: the old directory is kept aside" bash -c 'ls -d "$1".pre-xfs.* >/dev/null' _ "$WORK/store/ch"
    check "live: a reflink copy works there" cp --reflink=always "$WORK/store/ch/images/base.raw" "$WORK/store/ch/images/clone.raw"


    "${LIVE_ENV[@]}" bash "$SCRIPT" --apply "${args[@]}" >"$WORK/again.out" 2>&1
    check "live: a second run refuses (the partition is no longer swap)" grep -q 'is not a swap partition' "$WORK/again.out"
  fi
else
  echo "skip live move (set CH_XFS_LIVE=1, as root, to run it with a loop device)"
fi

if ((failures)); then
  echo "$failures failure(s)"
  exit 1
fi
echo "all passed"
