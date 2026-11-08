#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# ch-storage-swap-to-xfs.sh: give Cloud Hypervisor guest storage an XFS
# filesystem with reflinks on a host that has no spare partition, by turning
# its swap partition into one. A swap file takes the partition's place first,
# so swap never drops to zero.
#
# Use it when the storage directory shares a system partition (for example
# /srv/banlieue/ch inside /home) and that partition cannot be shrunk without
# taking the host down. For storage that is already on a partition of its
# own, use scripts/ch-storage-to-xfs.sh instead. Why XFS: on ext4 every
# guest's os.raw is a full copy of its image and the guest's first disk flush
# waits for it (7.4 s for a 2.5 GB image, 0.96 s as an XFS reflink; roadmap
# 18).
#
# What it does, in order, and only with --apply (without it, it checks and
# prints the plan):
#
#   1. checks: root; <swap-device> is a swap partition with exactly one fstab
#      line; `findmnt --verify` accepts fstab as it is; the kernel command line does not resume from it; <storage-dir>
#      exists and is not a mountpoint; no Cloud Hypervisor guest is running;
#      the swap file's filesystem is ext4 or XFS with room for it; the
#      partition holds the storage directory's data; the swap in use on the
#      partition fits in free memory plus the new swap file;
#   2. creates, enables and adds to fstab the swap file;
#   3. disables the swap partition and comments its fstab line out;
#   4. points hibernation resume (initramfs-tools) away from it, and
#      rebuilds the initramfs when that file is the system's;
#   5. formats the partition `mkfs.xfs -m reflink=1`;
#   6. stops banlieue-provider-cloud-hypervisor if it is running, copies the
#      storage directory onto the new filesystem (rsync -aHAXS) and compares;
#   7. adds the new filesystem to fstab at <storage-dir> (refusing unless
#      `findmnt --verify` accepts it), moves the old directory aside to
#      <storage-dir>.pre-xfs.<stamp>, mounts, compares again;
#   8. restarts the provider, and leaves the old directory for you to delete.
#
# Every fstab change is made on a dated backup's copy (fstab.bak-ch-storage-
# swap-to-xfs.<stamp>). On failure it stops, says which step failed and what
# state the host is in; nothing is deleted.
#
# Usage (as root):
#   scripts/ch-storage-swap-to-xfs.sh [--apply] --swapfile PATH
#       [--swapfile-size SIZE] <swap-device> <storage-dir>
#   SIZE is fallocate's (16G, 512M); default 16G.
#
# Environment (the tests set all of them, so a test run on a real CH host
# never touches its fstab, initramfs, provider or guests):
#   CH_XFS_FSTAB           the fstab to edit (default /etc/fstab)
#   CH_XFS_RESUME_CONF     initramfs-tools' resume file
#                          (default /etc/initramfs-tools/conf.d/resume)
#   CH_XFS_CMDLINE         the kernel command line (default /proc/cmdline)
#   CH_XFS_BACKUP_DIR      where the resume file's backup goes, outside
#                          conf.d (default /var/backups/ch-storage-swap-to-xfs)
#   CH_XFS_PROVIDER_UNIT   the provider unit to stop and start
#   CH_XFS_GUEST_UNITS     the guest unit pattern that must not be running
#
# Needs: blkid, blockdev, findmnt, swapon, swapoff, mkswap, fallocate, mkfs.xfs,
# xfs_info, rsync, mount, umount, du, df.
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The shared checks and constants (PROVIDER_UNIT, GUEST_UNIT_PATTERN,
# BACKUP_HEADROOM_PERCENT, backup_fits, under_mount).
# shellcheck source-path=SCRIPTDIR source=ch-storage-to-xfs.sh
CH_XFS_SOURCE_ONLY=1 source "$SELF_DIR/ch-storage-to-xfs.sh"

readonly RESUME_CONF_DEFAULT="/etc/initramfs-tools/conf.d/resume"
# Where the resume file's backup goes: never beside it, because
# initramfs-tools reads every file in conf.d, and a backup there would put the
# old RESUME= back into the initramfs (found on a host, 2026-11-08).
readonly BACKUP_DIR_DEFAULT="/var/backups/ch-storage-swap-to-xfs"
readonly CMDLINE_DEFAULT="/proc/cmdline"
readonly DEFAULT_SWAPFILE_SIZE="16G"
readonly SWAPFILE_MODE="0600"

die() { echo "ch-storage-swap-to-xfs: $*" >&2; exit 1; }
say() { echo "ch-storage-swap-to-xfs: $*" >&2; }

# check_storage_dir <path>: refuse what must never become a mountpoint here.
check_storage_dir() {
  local dir=$1
  [[ -n $dir ]] || die "no storage directory given"
  [[ $dir == /* ]] || die "$dir: give an absolute path"
  [[ $dir != */ ]] || die "$dir: drop the trailing slash"
  case $dir in
    / | /boot | /boot/* | /usr | /usr/* | /var | /etc | /etc/* | /home | /srv | /root | /proc/* | /sys/* | /dev/*)
      die "$dir is not a storage directory this script will mount over" ;;
  esac
}

# fstab_comment_swap <fstab> <uuid> <device>: print <fstab> with the one
# uncommented swap line for the partition (by UUID=, /dev/disk/by-uuid/ or
# its device path) commented out. Fails when there is no such line, or more
# than one.
fstab_comment_swap() {
  local fstab=$1 uuid=$2 dev=$3
  awk -v uuid="$uuid" -v dev="$dev" '
    /^[[:space:]]*#/ || NF < 3 { print; next }
    $3 == "swap" && ($1 == "UUID=" uuid || $1 == "/dev/disk/by-uuid/" uuid || $1 == dev) {
      n++; print "# " $0 "  # retired by ch-storage-swap-to-xfs"; next
    }
    { print }
    END {
      if (n == 0) { print "no fstab swap line for " dev > "/dev/stderr"; exit 2 }
      if (n > 1)  { print n " fstab swap lines for " dev > "/dev/stderr"; exit 3 }
    }' "$fstab"
}

# resume_refers <file> <uuid> <device>: whether an initramfs-tools resume
# file resumes from the partition.
resume_refers() {
  local file=$1 uuid=$2 dev=$3
  [[ -f $file ]] || return 1
  grep -Eq "^[[:space:]]*RESUME=(UUID=${uuid}|/dev/disk/by-uuid/${uuid}|${dev})[[:space:]]*$" "$file"
}

# resume_rewrite <file> <uuid> <device>: print the resume file with the
# partition's RESUME= line replaced by RESUME=none.
resume_rewrite() {
  local file=$1 uuid=$2 dev=$3
  sed -E "s#^[[:space:]]*RESUME=(UUID=${uuid}|/dev/disk/by-uuid/${uuid}|${dev})[[:space:]]*\$#RESUME=none#" "$file"
}

# cmdline_resumes_from <cmdline-file> <uuid> <device>: whether the running
# kernel was told to resume from the partition (a bootloader edit, which this
# script does not make).
cmdline_resumes_from() {
  local file=$1 uuid=$2 dev=$3 word
  for word in $(<"$file"); do
    case $word in
      "resume=UUID=$uuid" | "resume=/dev/disk/by-uuid/$uuid" | "resume=$dev") return 0 ;;
    esac
  done
  return 1
}

# swapoff_fits <used_kib> <mem_available_kib> <new_swap_kib>: whether the swap
# in use on the partition can move into free memory plus the new swap file,
# with BACKUP_HEADROOM_PERCENT to spare.
swapoff_fits() {
  local used=$1 avail=$2 new=$3
  (((avail + new) * PERCENT >= used * (PERCENT + BACKUP_HEADROOM_PERCENT)))
}

# size_kib <SIZE>: fallocate-style size (16G, 512M, 1024K, 4096) in KiB.
size_kib() {
  local size=$1 n unit
  [[ $size =~ ^([0-9]+)([KMGT]?)$ ]] || return 1
  n=${BASH_REMATCH[1]} unit=${BASH_REMATCH[2]}
  case $unit in
    "") echo $((n / BYTES_PER_KIB)) ;;
    K) echo "$n" ;;
    M) echo $((n * BYTES_PER_KIB)) ;;
    G) echo $((n * BYTES_PER_KIB * BYTES_PER_KIB)) ;;
    T) echo $((n * BYTES_PER_KIB * BYTES_PER_KIB * BYTES_PER_KIB)) ;;
  esac
}

swap_usage() {
  sed -n '/^# Usage/,/^# Environment/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

swap_main() {
  local apply=false swapfile="" swapfile_size=$DEFAULT_SWAPFILE_SIZE dev="" dir=""
  while (($#)); do
    case $1 in
      --apply) apply=true ;;
      --swapfile) shift; swapfile=${1:-} ;;
      --swapfile-size) shift; swapfile_size=${1:-} ;;
      -h | --help) swap_usage ;;
      -*) die "unknown option $1" ;;
      *)
        if [[ -z $dev ]]; then dev=$1
        elif [[ -z $dir ]]; then dir=$1
        else die "too many arguments"; fi ;;
    esac
    shift
  done
  [[ -n $dev ]] || die "no swap device given"
  check_storage_dir "$dir"
  [[ $swapfile == /* ]] || die "--swapfile needs an absolute path"
  local swap_kib
  swap_kib=$(size_kib "$swapfile_size") || die "--swapfile-size $swapfile_size: use fallocate's form (16G, 512M)"
  local fstab=${CH_XFS_FSTAB:-$FSTAB_DEFAULT}
  local resume_conf=${CH_XFS_RESUME_CONF:-$RESUME_CONF_DEFAULT}
  local cmdline=${CH_XFS_CMDLINE:-$CMDLINE_DEFAULT}
  local backup_dir=${CH_XFS_BACKUP_DIR:-$BACKUP_DIR_DEFAULT}

  # ---- checks ---------------------------------------------------------------
  ((EUID == 0)) || die "run as root"
  local c
  for c in blkid blockdev findmnt swapon swapoff mkswap fallocate mkfs.xfs xfs_info rsync mount umount du df; do
    command -v "$c" >/dev/null || die "$c is not on PATH"
  done
  [[ -b $dev ]] || die "$dev is not a block device"
  [[ $(blkid -s TYPE -o value "$dev") == swap ]] || die "$dev is not a swap partition"
  local uuid
  uuid=$(blkid -s UUID -o value "$dev")
  [[ -n $uuid ]] || die "$dev has no UUID"
  fstab_comment_swap "$fstab" "$uuid" "$dev" >/dev/null || die "$fstab must have exactly one swap line for $dev"
  # Checked before anything changes, so a rejection after the edits is this
  # script's, not a line that was already broken.
  findmnt --verify --tab-file "$fstab" >/dev/null 2>&1 ||
    die "findmnt --verify already rejects $fstab: fix it first (findmnt --verify --tab-file $fstab)"
  ! cmdline_resumes_from "$cmdline" "$uuid" "$dev" ||
    die "the kernel command line resumes from $dev: remove resume= from the bootloader config first"
  [[ -d $dir ]] || die "$dir does not exist"
  ! findmnt -M "$dir" >/dev/null || die "$dir is already a mountpoint"
  if command -v systemctl >/dev/null &&
    [[ -n $(systemctl list-units "$GUEST_UNIT_PATTERN" --state=active --no-legend 2>/dev/null) ]]; then
    die "Cloud Hypervisor guests are running; delete their VMs, or move them off this host, first"
  fi
  [[ ! -e $swapfile ]] || die "$swapfile already exists"
  local swapdir swapfs
  swapdir=$(dirname "$swapfile")
  [[ -d $swapdir ]] || die "$swapdir does not exist"
  swapfs=$(findmnt -no FSTYPE -T "$swapdir")
  case $swapfs in ext4 | xfs) ;; *) die "$swapdir is on $swapfs; a swap file here needs ext4 or XFS" ;; esac
  local free_kib data_kib part_kib used_kib avail_kib
  free_kib=$(df -Pk "$swapdir" | awk 'NR == 2 { print $4 }')
  backup_fits "$swap_kib" "$free_kib" || die "$swapdir has $((free_kib / BYTES_PER_KIB)) MiB free; the swap file needs $((swap_kib / BYTES_PER_KIB)) MiB plus ${BACKUP_HEADROOM_PERCENT}%"
  data_kib=$(du -sxk "$dir" | cut -f1)
  part_kib=$(($(blockdev --getsize64 "$dev") / BYTES_PER_KIB))
  backup_fits "$data_kib" "$part_kib" || die "$dev ($((part_kib / BYTES_PER_KIB)) MiB) cannot hold $dir ($((data_kib / BYTES_PER_KIB)) MiB) plus ${BACKUP_HEADROOM_PERCENT}%"
  used_kib=$(awk -v d="$(readlink -f "$dev")" '$1 == d { print $4 }' /proc/swaps)
  used_kib=${used_kib:-0}
  avail_kib=$(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo)
  swapoff_fits "$used_kib" "$avail_kib" "$swap_kib" ||
    die "$dev has $((used_kib / BYTES_PER_KIB)) MiB of swap in use; free memory plus the new swap file is not enough to take it"
  local resume_change=false
  resume_refers "$resume_conf" "$uuid" "$dev" && resume_change=true
  local provider_active=false
  command -v systemctl >/dev/null && systemctl is-active --quiet "$PROVIDER_UNIT" && provider_active=true

  local stamp
  stamp=$(date +%Y%m%dT%H%M%S)
  local aside="$dir.pre-xfs.$stamp"
  say "plan: $dev (swap, $((part_kib / BYTES_PER_KIB)) MiB, $((used_kib / BYTES_PER_KIB)) MiB in use) becomes XFS at $dir ($((data_kib / BYTES_PER_KIB)) MiB of data):"
  say "  swap file $swapfile ($swapfile_size): create, enable, add to $fstab"
  say "  swapoff $dev; comment out its $fstab line (backup $fstab.bak-ch-storage-swap-to-xfs.$stamp)"
  $resume_change && say "  $resume_conf: RESUME=none (old file kept in $backup_dir/$stamp)$([[ $resume_conf == "$RESUME_CONF_DEFAULT" ]] && echo '; update-initramfs -u')"
  say "  mkfs.xfs -m reflink=1 $dev"
  $provider_active && say "  stop $PROVIDER_UNIT"
  say "  copy $dir onto it and compare; add it to $fstab at $dir; findmnt --verify"
  say "  move $dir to $aside; mount; compare"
  $provider_active && say "  start $PROVIDER_UNIT"
  if ! $apply; then
    say "dry run: nothing changed. Re-run with --apply to do it."
    return 0
  fi

  # ---- apply ----------------------------------------------------------------
  local step="start"
  trap 'say "FAILED at: $step. fstab backup: $fstab.bak-ch-storage-swap-to-xfs.$stamp; $dir is untouched unless the step was after the move (then it is at $aside)."' ERR
  cp -p "$fstab" "$fstab.bak-ch-storage-swap-to-xfs.$stamp"

  step="create the swap file"
  fallocate -l "$swapfile_size" "$swapfile"
  chmod "$SWAPFILE_MODE" "$swapfile"
  mkswap -q "$swapfile" >/dev/null
  swapon "$swapfile"
  printf '%s\tnone\tswap\tsw\t0\t0\n' "$swapfile" >>"$fstab"

  step="retire the swap partition"
  if [[ -n $(awk -v d="$(readlink -f "$dev")" '$1 == d' /proc/swaps) ]]; then swapoff "$dev"; fi
  local tmp
  tmp=$(mktemp)
  fstab_comment_swap "$fstab" "$uuid" "$dev" >"$tmp"
  cat "$tmp" >"$fstab"
  rm -f "$tmp"

  if $resume_change; then
    step="point resume away from $dev"
    mkdir -p "$backup_dir"
    mkdir -m 0700 "$backup_dir/$stamp"
    cp -p "$resume_conf" "$backup_dir/$stamp/$(basename "$resume_conf")"
    tmp=$(mktemp)
    resume_rewrite "$backup_dir/$stamp/$(basename "$resume_conf")" "$uuid" "$dev" >"$tmp"
    cat "$tmp" >"$resume_conf"
    rm -f "$tmp"
    if [[ $resume_conf == "$RESUME_CONF_DEFAULT" ]] && command -v update-initramfs >/dev/null; then
      update-initramfs -u >/dev/null
    fi
  fi

  step="format $dev as XFS"
  mkfs.xfs -f -m reflink=1 "$dev" >/dev/null
  local xuuid
  xuuid=$(blkid -s UUID -o value "$dev")

  if $provider_active; then step="stop the provider"; systemctl stop "$PROVIDER_UNIT"; fi
  step="copy $dir onto the new filesystem"
  local stage
  stage=$(mktemp -d)
  mount -t xfs UUID="$xuuid" "$stage"
  rsync -aHAXS "$dir"/ "$stage"/
  local diff
  diff=$(rsync -aHAXSn --itemize-changes "$dir"/ "$stage"/)
  [[ -z $diff ]] || die "the copy differs from $dir: $diff"
  xfs_info "$stage" | grep -q 'reflink=1' || die "the new filesystem has no reflinks"
  umount "$stage"
  rmdir "$stage"

  step="add the new filesystem to $fstab"
  printf 'UUID=%s\t%s\txfs\tdefaults\t0\t0\n' "$xuuid" "$dir" >>"$fstab"
  if ! findmnt --verify --tab-file "$fstab" >/dev/null 2>&1; then
    die "findmnt --verify rejected $fstab. The swap file is on, $dev is an XFS copy of $dir, and $dir is untouched; fix $fstab (backup: $fstab.bak-ch-storage-swap-to-xfs.$stamp)"
  fi
  [[ $fstab == "$FSTAB_DEFAULT" ]] && command -v systemctl >/dev/null && systemctl daemon-reload

  step="move $dir aside and mount the new filesystem there"
  local owner mode
  owner=$(stat -c %u:%g "$dir")
  mode=$(stat -c %a "$dir")
  mv "$dir" "$aside"
  mkdir "$dir"
  chown "$owner" "$dir"
  chmod "$mode" "$dir"
  mount -t xfs UUID="$xuuid" "$dir"
  diff=$(rsync -aHAXSn --itemize-changes "$aside"/ "$dir"/)
  [[ -z $diff ]] || die "the mounted copy differs from $aside: $diff"

  step="restart"
  if $provider_active; then systemctl start "$PROVIDER_UNIT"; fi
  trap - ERR
  say "done: $dir is XFS with reflinks on $dev (UUID $xuuid); swap is $swapfile."
  say "check the host (a reboot is the real test of fstab$($resume_change && echo ' and the initramfs')), then remove the old copy: rm -rf $aside"
}

if [[ -z ${CH_XFS_SWAP_SOURCE_ONLY:-} ]]; then
  swap_main "$@"
fi
