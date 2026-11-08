#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# ch-storage-to-xfs.sh: reformat a dedicated partition holding Cloud
# Hypervisor guest storage (for example /srv) as XFS with reflinks, keeping
# its data.
#
# Why: on ext4 the Cloud Hypervisor provider cannot reflink, so every guest's
# os.raw is a full copy of its image, and the guest's first disk flush waits
# for that copy to reach the disk (7.4 s for a 2.5 GB image, 0.96 s as an XFS
# reflink; roadmap 18, 2026-11-07). The provider names such storage classes
# in its failure domain (`raw.storageWithoutReflink`).
#
# What it does, in order, and only with --apply (without it, it checks and
# prints the plan):
#
#   1. checks: root; the mountpoint is a real mount of a block device, not
#      `/`; not already XFS; `findmnt --verify` accepts fstab as it is; no Cloud Hypervisor guest is running; no libvirt
#      domain is running when a libvirt pool lives under the mountpoint; the
#      backup directory is on another filesystem with room for the data;
#   2. stops banlieue-provider-cloud-hypervisor if it is running;
#   3. copies the data aside (rsync -aHAXS), unmounts, `mkfs.xfs -m reflink=1`;
#   4. rewrites the mountpoint's fstab line by UUID (type xfs, options
#      `defaults`; the old line is kept in a dated backup of fstab), and
#      refuses to go on unless `findmnt --verify` accepts the result;
#   5. mounts the new filesystem, restores the data, checks that nothing
#      differs from the copy (owners, modes, ACLs, xattrs, hard links,
#      sparseness) and that reflinks are on;
#   6. restarts what it stopped, refreshes libvirt pools under the
#      mountpoint, and leaves the copy for you to delete.
#
# It refuses a partition that is not dedicated (a mountpoint it would have to
# share, like `/` or `/home`): moving those needs a new partition, which is a
# console job. On failure it stops, says which step failed and where the copy
# is; nothing is deleted.
#
# Usage (as root):
#   scripts/ch-storage-to-xfs.sh [--apply] [--backup-dir DIR] <mountpoint>
#
# Environment (the tests set all three, so a test run on a real CH host never
# touches its fstab, its provider or its guests):
#   CH_XFS_FSTAB           the fstab to edit (default /etc/fstab)
#   CH_XFS_PROVIDER_UNIT   the provider unit to stop and start
#                          (default banlieue-provider-cloud-hypervisor.service)
#   CH_XFS_GUEST_UNITS     the guest unit pattern that must not be running
#                          (default banlieue-ch@*)
#
# Needs: findmnt, blkid, mkfs.xfs, xfs_info, rsync, mount, umount, df, du.
set -euo pipefail

# Overridable so the tests never stop a real provider on the host they run on.
readonly PROVIDER_UNIT="${CH_XFS_PROVIDER_UNIT:-banlieue-provider-cloud-hypervisor.service}"
readonly GUEST_UNIT_PATTERN="${CH_XFS_GUEST_UNITS:-banlieue-ch@*}"
readonly DEFAULT_BACKUP_ROOT="/var/tmp"
readonly FSTAB_DEFAULT="/etc/fstab"
# Headroom over the data's size before the backup filesystem counts as big
# enough, percent.
readonly BACKUP_HEADROOM_PERCENT=10
readonly PERCENT=100
readonly BYTES_PER_KIB=1024

die() { echo "ch-storage-to-xfs: $*" >&2; exit 1; }
say() { echo "ch-storage-to-xfs: $*" >&2; }

# check_mountpoint <path>: refuse what this script must never reformat.
check_mountpoint() {
  local mnt=$1
  [[ -n $mnt ]] || die "no mountpoint given"
  [[ $mnt == /* ]] || die "$mnt: give an absolute path"
  [[ $mnt != */ || $mnt == / ]] || die "$mnt: drop the trailing slash"
  [[ $mnt != / ]] || die "/ is the root filesystem: move it with a new partition, from a console"
  case $mnt in
    /boot | /boot/* | /usr | /var | /etc | /home)
      die "$mnt holds the system or users' data: move CH storage to a new partition instead" ;;
  esac
}

# under_mount <path> <mountpoint>: whether <path> is <mountpoint> or below it.
under_mount() {
  local path=$1 mnt=$2
  [[ $path == "$mnt" || $path == "$mnt"/* ]]
}

# fstab_rewrite <fstab> <mountpoint> <uuid>: print <fstab> with the one
# uncommented line whose mountpoint field is <mountpoint> replaced by an XFS
# line for UUID=<uuid>. Fails when there is no such line, or more than one.
fstab_rewrite() {
  local fstab=$1 mnt=$2 uuid=$3
  awk -v mnt="$mnt" -v uuid="$uuid" '
    /^[[:space:]]*#/ || NF < 2 { print; next }
    $2 == mnt { n++; print "UUID=" uuid "\t" mnt "\txfs\tdefaults\t0\t0"; next }
    { print }
    END {
      if (n == 0) { print "no fstab line for " mnt > "/dev/stderr"; exit 2 }
      if (n > 1)  { print n " fstab lines for " mnt > "/dev/stderr"; exit 3 }
    }' "$fstab"
}

# backup_fits <data_kib> <free_kib>: whether a copy of <data_kib> fits in
# <free_kib> with BACKUP_HEADROOM_PERCENT to spare.
backup_fits() {
  local data=$1 free=$2
  ((free * PERCENT >= data * (PERCENT + BACKUP_HEADROOM_PERCENT)))
}

# libvirt_pools_under <mountpoint>: the libvirt storage pools whose target
# path is under <mountpoint>, one per line. Nothing when virsh is absent.
libvirt_pools_under() {
  local mnt=$1 pool path
  command -v virsh >/dev/null || return 0
  for pool in $(virsh -q pool-list --all --name 2>/dev/null); do
    path=$(virsh pool-dumpxml "$pool" 2>/dev/null | sed -n 's|.*<path>\(.*\)</path>.*|\1|p' | head -n1)
    [[ -n $path ]] && under_mount "$path" "$mnt" && echo "$pool"
  done
  return 0
}

usage() {
  sed -n '/^# Usage/,/^# Needs/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

main() {
  local apply=false backup_root=$DEFAULT_BACKUP_ROOT mnt=""
  while (($#)); do
    case $1 in
      --apply) apply=true ;;
      --backup-dir) shift; backup_root=${1:-} ;;
      -h | --help) usage ;;
      -*) die "unknown option $1" ;;
      *) [[ -z $mnt ]] || die "one mountpoint only"; mnt=$1 ;;
    esac
    shift
  done
  check_mountpoint "$mnt"
  local fstab=${CH_XFS_FSTAB:-$FSTAB_DEFAULT}

  # ---- checks ---------------------------------------------------------------
  ((EUID == 0)) || die "run as root"
  local c
  for c in findmnt blkid mkfs.xfs xfs_info rsync mount umount df du; do
    command -v "$c" >/dev/null || die "$c is not on PATH"
  done
  local source fstype
  read -r source fstype < <(findmnt -no SOURCE,FSTYPE -M "$mnt") ||
    die "$mnt is not a mountpoint"
  [[ -b $source ]] || die "$mnt is mounted from $source, not a block device"
  if [[ $fstype == xfs ]]; then
    xfs_info "$mnt" | grep -q 'reflink=1' && { say "$mnt is already XFS with reflinks: nothing to do"; return 0; }
    die "$mnt is XFS without reflinks: reformat it by hand (mkfs.xfs -m reflink=1)"
  fi
  fstab_rewrite "$fstab" "$mnt" 00000000-0000-0000-0000-000000000000 >/dev/null ||
    die "$fstab must have exactly one line for $mnt"
  # Checked before anything changes, so a rejection after the edit is this
  # script's, not a line that was already broken.
  findmnt --verify --tab-file "$fstab" >/dev/null 2>&1 ||
    die "findmnt --verify already rejects $fstab: fix it first (findmnt --verify --tab-file $fstab)"
  if command -v systemctl >/dev/null &&
    [[ -n $(systemctl list-units "$GUEST_UNIT_PATTERN" --state=active --no-legend 2>/dev/null) ]]; then
    die "Cloud Hypervisor guests are running; delete their VMs, or move them off this host, first"
  fi
  local pools
  pools=$(libvirt_pools_under "$mnt")
  if [[ -n $pools ]] && [[ -n $(virsh -q list --name 2>/dev/null) ]]; then
    die "libvirt pools live under $mnt (${pools//$'\n'/ }) and libvirt domains are running; stop them first"
  fi
  [[ -d $backup_root ]] || die "backup directory $backup_root does not exist"
  local backup_dev mnt_dev
  backup_dev=$(stat -c %d "$backup_root")
  mnt_dev=$(stat -c %d "$mnt")
  [[ $backup_dev != "$mnt_dev" ]] || die "backup directory $backup_root is on $mnt itself"
  local data_kib free_kib
  data_kib=$(du -sxk "$mnt" | cut -f1)
  free_kib=$(df -Pk "$backup_root" | awk 'NR == 2 { print $4 }')
  backup_fits "$data_kib" "$free_kib" ||
    die "$backup_root has $((free_kib / BYTES_PER_KIB)) MiB free; the copy needs $((data_kib / BYTES_PER_KIB)) MiB plus ${BACKUP_HEADROOM_PERCENT}%"
  local provider_active=false
  command -v systemctl >/dev/null && systemctl is-active --quiet "$PROVIDER_UNIT" && provider_active=true

  local stamp backup
  stamp=$(date +%Y%m%dT%H%M%S)
  backup="$backup_root/ch-storage-to-xfs.$stamp"
  say "plan for $mnt ($source, $fstype, $((data_kib / BYTES_PER_KIB)) MiB):"
  $provider_active && say "  stop $PROVIDER_UNIT"
  say "  copy $mnt to $backup"
  say "  umount $mnt; mkfs.xfs -m reflink=1 $source"
  say "  rewrite $fstab (backup at $fstab.bak-ch-storage-to-xfs.$stamp); findmnt --verify"
  say "  mount, restore, compare with the copy, check reflink=1"
  $provider_active && say "  start $PROVIDER_UNIT"
  [[ -n $pools ]] && say "  refresh libvirt pools: ${pools//$'\n'/ }"
  if ! $apply; then
    say "dry run: nothing changed. Re-run with --apply to do it."
    return 0
  fi

  # ---- apply ----------------------------------------------------------------
  local step="start"
  trap 'say "FAILED at: $step. The copy of $mnt is in $backup; nothing was deleted."' ERR

  if $provider_active; then step="stop the provider"; systemctl stop "$PROVIDER_UNIT"; fi
  step="copy the data aside"
  mkdir -m 0700 "$backup"
  rsync -aHAXS "$mnt"/ "$backup"/
  step="unmount $mnt"
  umount "$mnt"
  step="format $source as XFS"
  mkfs.xfs -f -m reflink=1 "$source" >/dev/null
  local uuid
  uuid=$(blkid -s UUID -o value "$source")
  step="rewrite $fstab"
  cp -p "$fstab" "$fstab.bak-ch-storage-to-xfs.$stamp"
  fstab_rewrite "$fstab.bak-ch-storage-to-xfs.$stamp" "$mnt" "$uuid" >"$fstab"
  if ! findmnt --verify --tab-file "$fstab" >/dev/null 2>&1; then
    cp -p "$fstab.bak-ch-storage-to-xfs.$stamp" "$fstab"
    die "findmnt --verify rejected the new $fstab; restored the old one. $source is now an EMPTY XFS filesystem; the data is in $backup"
  fi
  [[ $fstab == "$FSTAB_DEFAULT" ]] && command -v systemctl >/dev/null && systemctl daemon-reload
  step="mount the new filesystem"
  mount -t xfs UUID="$uuid" "$mnt"
  step="restore the data"
  rsync -aHAXS "$backup"/ "$mnt"/
  step="compare with the copy"
  local diff
  diff=$(rsync -aHAXSn --itemize-changes "$backup"/ "$mnt"/)
  [[ -z $diff ]] || die "the restored data differs from the copy: $diff"
  xfs_info "$mnt" | grep -q 'reflink=1' || die "the new filesystem has no reflinks"
  step="restart"
  if $provider_active; then systemctl start "$PROVIDER_UNIT"; fi
  local pool
  for pool in $pools; do virsh -q pool-refresh "$pool" >/dev/null || say "pool-refresh $pool failed; refresh it by hand"; done
  trap - ERR
  say "done: $mnt is XFS with reflinks (UUID $uuid)."
  say "check the host, then remove the copy: rm -rf $backup"
}

if [[ -z ${CH_XFS_SOURCE_ONLY:-} ]]; then
  main "$@"
fi
