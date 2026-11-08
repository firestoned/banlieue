#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Tests for scripts/ch-storage-to-xfs.sh.
#
# Without root: the pure functions (the fstab rewrite, the mountpoint
# refusals, the backup-size rule) and the refusals main makes before it
# touches anything.
#
# With CH_XFS_LIVE=1, as root, on a Linux host with mkfs.xfs: the whole move,
# end to end, against an ext4 filesystem on a loop device, with a copy of
# fstab (CH_XFS_FSTAB), so no real disk and no real fstab is involved. It
# checks that the data, owners, modes, a hard link and a sparse file survive
# and that the result reflinks.
#
# Usage: scripts/ch-storage-to-xfs-test.sh
#        sudo CH_XFS_LIVE=1 scripts/ch-storage-to-xfs-test.sh
#
# The `bash -c '...' _ "$arg"` checks below expand "$1" in the child on
# purpose: single quotes are the point.
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ch-storage-to-xfs.sh"
WORK="$(mktemp -d)"
LOOP=""
cleanup() {
  if [[ -n $LOOP ]]; then
    umount "$WORK/mnt" 2>/dev/null || true
    losetup -d "$LOOP" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

failures=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }
check() { # case name, then a command that must succeed
  local name=$1; shift
  if "$@"; then ok "$name"; else fail "$name"; fi
}
refuses() { # case name, then a command that must fail
  local name=$1; shift
  if ("$@") >/dev/null 2>&1; then fail "$name"; else ok "$name"; fi
}

# shellcheck source-path=SCRIPTDIR source=ch-storage-to-xfs.sh
CH_XFS_SOURCE_ONLY=1 source "$SCRIPT"
set +e

# ---- check_mountpoint -------------------------------------------------------

refuses "refuses an empty mountpoint" check_mountpoint ""
refuses "refuses a relative path" check_mountpoint srv
refuses "refuses the root filesystem" check_mountpoint /
refuses "refuses a trailing slash" check_mountpoint /srv/
refuses "refuses /home" check_mountpoint /home
refuses "refuses /var" check_mountpoint /var
refuses "refuses /boot/efi" check_mountpoint /boot/efi
check "accepts /srv" check_mountpoint /srv
check "accepts a nested data mount" check_mountpoint /var/lib/banlieue-ch

# ---- under_mount ------------------------------------------------------------

check "a pool path under the mount is under it" under_mount /srv/libvirt/images /srv
check "the mount itself is under it" under_mount /srv /srv
refuses "a sibling with the same prefix is not" under_mount /srvx/images /srv
refuses "the parent is not" under_mount / /srv

# ---- fstab_rewrite ----------------------------------------------------------

fstab="$WORK/fstab"
cat >"$fstab" <<'EOF'
# /etc/fstab: static file system information.
UUID=1111	/	ext4	errors=remount-ro	0	1
# /srv was on /dev/nvme0n1p5 during installation
UUID=2222 /srv ext4 defaults 0 2
UUID=3333	/srv/banlieue	none	bind	0	0
UUID=4444 none swap sw 0 0
EOF
out=$(fstab_rewrite "$fstab" /srv abcd-ef)
check "the /srv line becomes XFS by the new UUID" grep -qxP 'UUID=abcd-ef\t/srv\txfs\tdefaults\t0\t0' <<<"$out"
check "the old /srv line is gone" bash -c '! grep -q "UUID=2222" <<<"$1"' _ "$out"
check "a line whose mountpoint only starts with /srv is kept" grep -q 'UUID=3333' <<<"$out"
check "the root line is kept" grep -qP '^UUID=1111\t/\t' <<<"$out"
check "comments are kept, including one naming /srv" grep -q '^# /srv was on' <<<"$out"
check "the line count is unchanged" test "$(wc -l <<<"$out")" -eq "$(wc -l <"$fstab")"
refuses "refuses a mountpoint with no line" fstab_rewrite "$fstab" /data abcd
printf 'UUID=5555 /srv xfs defaults 0 0\n' >>"$fstab"
refuses "refuses a mountpoint with two lines" fstab_rewrite "$fstab" /srv abcd
printf '#UUID=6666 /data ext4 defaults 0 2\n' >"$WORK/fstab2"
refuses "a commented-out line does not count" fstab_rewrite "$WORK/fstab2" /data abcd

# ---- backup_fits ------------------------------------------------------------

check "a copy fits with room to spare" backup_fits 1000 2000
check "exactly the headroom fits" backup_fits 1000 1100
refuses "less than the headroom does not" backup_fits 1000 1099
refuses "less than the data does not" backup_fits 1000 500

# ---- main's refusals, before anything changes --------------------------------

refuses "main refuses no mountpoint" bash "$SCRIPT"
refuses "main refuses two mountpoints" bash "$SCRIPT" /srv /data
refuses "main refuses an unknown option" bash "$SCRIPT" --force /srv
refuses "main refuses the root filesystem" bash "$SCRIPT" --apply /
if ((EUID != 0)); then
  refuses "main refuses to run without root" bash "$SCRIPT" /srv
fi

# ---- live: the whole move on a loop device ----------------------------------

if [[ ${CH_XFS_LIVE:-} == 1 ]]; then
  if ((EUID != 0)); then
    fail "CH_XFS_LIVE=1 needs root"
  elif ! command -v mkfs.xfs >/dev/null; then
    fail "CH_XFS_LIVE=1 needs mkfs.xfs (xfsprogs)"
  else
    img="$WORK/disk.img"
    mkdir -p "$WORK/mnt" "$WORK/backups"
    truncate -s 1G "$img"
    LOOP=$(losetup --find --show "$img")
    mkfs.ext4 -q -F "$LOOP"
    mount "$LOOP" "$WORK/mnt"
    # What a CH host's storage looks like: a provider-owned tree, private
    # dirs, an image, a hard link and a sparse disk.
    mkdir -p "$WORK/mnt/banlieue/ch/images"
    chown -R nobody:nogroup "$WORK/mnt/banlieue"
    chmod 0711 "$WORK/mnt/banlieue/ch"
    chmod 0750 "$WORK/mnt/banlieue/ch/images"
    head -c 4096 /dev/urandom >"$WORK/mnt/banlieue/ch/images/base.raw"
    ln "$WORK/mnt/banlieue/ch/images/base.raw" "$WORK/mnt/banlieue/ch/images/base.link"
    truncate -s 512M "$WORK/mnt/banlieue/ch/images/sparse.raw"
    chown nobody:nogroup "$WORK/mnt/banlieue/ch/images/"*
    sum_before=$(sha256sum "$WORK/mnt/banlieue/ch/images/base.raw" | cut -d' ' -f1)
    printf 'UUID=%s %s ext4 defaults 0 2\n' "$(blkid -s UUID -o value "$LOOP")" "$WORK/mnt" >"$WORK/fstab-live"
    # Never the host's own fstab, provider or guests: this may run on a live
    # CH host (it once restarted one's provider before these were set).
    LIVE_ENV=(env CH_XFS_FSTAB="$WORK/fstab-live"
      CH_XFS_PROVIDER_UNIT="banlieue-test-no-such-provider-$$.service"
      CH_XFS_GUEST_UNITS="banlieue-test-no-such-guest-$$@*")

    "${LIVE_ENV[@]}" bash "$SCRIPT" --backup-dir "$WORK/backups" "$WORK/mnt" >"$WORK/dry.out" 2>&1
    check "live: a dry run succeeds" test $? -eq 0
    check "live: a dry run changes nothing" test "$(findmnt -no FSTYPE -M "$WORK/mnt")" = ext4
    check "live: a dry run says so" grep -q 'dry run' "$WORK/dry.out"

    { printf 'UUID=00000000-dead-beef-0000-000000000000 /nowhere ext4 defaults 0 2\n'; cat "$WORK/fstab-live"; } >"$WORK/fstab-broken"
    env CH_XFS_FSTAB="$WORK/fstab-broken" CH_XFS_PROVIDER_UNIT="banlieue-test-no-such-provider-$$.service" \
      CH_XFS_GUEST_UNITS="banlieue-test-no-such-guest-$$@*" \
      bash "$SCRIPT" --apply --backup-dir "$WORK/backups" "$WORK/mnt" >"$WORK/broken.out" 2>&1
    check "live: an fstab that is already broken is refused up front" grep -q 'already rejects' "$WORK/broken.out"
    check "live: and nothing changed" test "$(findmnt -no FSTYPE -M "$WORK/mnt")" = ext4

    "${LIVE_ENV[@]}" bash "$SCRIPT" --apply --backup-dir "$WORK/backups" "$WORK/mnt" >"$WORK/apply.out" 2>&1
    rc=$?
    check "live: --apply succeeds" test $rc -eq 0
    [[ $rc -eq 0 ]] || sed 's/^/     /' "$WORK/apply.out"
    check "live: the mount is now XFS" test "$(findmnt -no FSTYPE -M "$WORK/mnt")" = xfs
    check "live: with reflinks" bash -c 'xfs_info "$1" | grep -q reflink=1' _ "$WORK/mnt"
    check "live: fstab names the new filesystem" grep -q "^UUID=$(blkid -s UUID -o value "$LOOP")[[:space:]]$WORK/mnt[[:space:]]xfs" "$WORK/fstab-live"
    check "live: the old fstab is kept" bash -c 'ls "$1".bak-ch-storage-to-xfs.* >/dev/null' _ "$WORK/fstab-live"
    check "live: the data survived" test "$(sha256sum "$WORK/mnt/banlieue/ch/images/base.raw" | cut -d' ' -f1)" = "$sum_before"
    check "live: owners survived" test "$(stat -c %U:%G "$WORK/mnt/banlieue/ch/images/base.raw")" = nobody:nogroup
    check "live: modes survived" test "$(stat -c %a "$WORK/mnt/banlieue/ch")" = 711
    check "live: the hard link survived" test "$(stat -c %h "$WORK/mnt/banlieue/ch/images/base.raw")" -eq 2
    check "live: the sparse file stayed sparse" test "$(du -k "$WORK/mnt/banlieue/ch/images/sparse.raw" | cut -f1)" -lt 1024
    check "live: the copy is left for the operator" bash -c 'ls -d "$1"/ch-storage-to-xfs.* >/dev/null' _ "$WORK/backups"
    check "live: a reflink copy now works there" cp --reflink=always "$WORK/mnt/banlieue/ch/images/base.raw" "$WORK/mnt/banlieue/ch/images/clone.raw"

    "${LIVE_ENV[@]}" bash "$SCRIPT" --apply --backup-dir "$WORK/backups" "$WORK/mnt" >"$WORK/again.out" 2>&1
    check "live: a second run is a no-op" grep -q 'already XFS with reflinks' "$WORK/again.out"
  fi
else
  echo "skip live move (set CH_XFS_LIVE=1, as root, to run it on a loop device)"
fi

if ((failures)); then
  echo "$failures failure(s)"
  exit 1
fi
echo "all passed"
