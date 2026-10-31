<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0113: Pre-laid images for fast sealed clones

- **Status:** Proposed
- **Date:** 2026-10-31
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 18 phase C (the measurement this acts on); builds on
  [ADR-0088](0088-first-boot-sealing-for-immediate-images.md) and
  [ADR-0112](0112-first-boot-sealing-on-cloud-hypervisor.md) (first-boot
  sealing); keeps [ADR-0040](0040-deferred-install-for-vtpm-encryption.md)'s "never
  clone a sealed disk" and [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md)'s
  delivery by digest

## Context

First-boot sealing removed the per-clone OS install, but a sealed clone of a
Kairos raw image still takes **106 s** from creation to `GuestReady` on Cloud
Hypervisor (roadmap 18 phase C, three runs within 2 s of each other):

| Step | Takes |
| --- | --- |
| Provider: reflink the disk, manufacture the vTPM, start the VMM | 8.0 s |
| Firmware to GRUB | 6.7 s |
| GRUB menu countdown, then the recovery kernel | 12.2 s |
| Recovery boot to the start of the reset | 24.8 s |
| Reset: lay out `COS_STATE` and `COS_PERSISTENT` | 13.2 s |
| Reboot: shutdown, firmware to GRUB | 7.6 s |
| GRUB menu countdown, then the active kernel | 12.2 s |
| Active boot: sealing, `boot` stage, the vsock report | 22.0 s |

A raw image (AuroraBoot `disk.raw`) carries only EFI, `COS_OEM` and
`COS_RECOVERY`. Every clone's first boot runs recovery, whose reset lays out
the partitions, then reboots into the system it laid out. The recovery path
(GRUB, recovery boot, reset, reboot) is **57.8 s, 54 %** of the total, and it
does the same thing for every clone. GRUB's 10 s menu countdown on each boot
is another **19 %** (`/etc/cos/grub.cfg` has `set timeout=10`). The
provider advances a VMM start one step per reconcile pass, 3 s apart.

Two facts found while testing what to do about it:

- **The recovery boot already handles `COS_PERSISTENT`.** Its `rootfs`
  layout stage creates the partition, and with `kcrypt.encrypt_on_boot` the
  initramfs then tries to encrypt it. Booting the image at build time with
  its own configuration therefore either halts (no TPM) or seals the image to
  the *builder's* TPM, which every clone would then share. That is the one
  thing ADR-0040 forbids.
- **The build boot leaves traces.** It logs into `COS_PERSISTENT`
  (`.state/var-log.bind`: journal, `wtmp`, `lastlog`), and a journal
  directory is named after that boot's machine-id: identity, in what would
  become every clone's disk.

And one about GRUB: the reset writes a `grubcustom` onto `COS_STATE` that
sources Kairos's boot assessment, and GRUB sources the *first* `/grubcustom`
it finds. A `grubcustom` on `COS_OEM` (searched first) would silently
disable automatic fallback.

## Decision

1. **A sealed pool image can be pre-laid: the recovery reset is done once,
   at build time, and the result is the image.** The steps, each fixed:
   1. copy the raw image (sparse) and grow it to the clones' OS disk size;
   2. move the image's own cloud-configs off `COS_OEM`, keeping only
      AuroraBoot's `01_reset.yaml`, and add a throwaway admin user with a
      random password that is never stored (with no admin user, Kairos runs
      none of the configured stages, so the reset never starts). The build
      boot runs **no operator stage and no `kcrypt` section**: nothing an
      operator wrote executes at build time and gets baked into every clone,
      and nothing is sealed;
   3. boot it once with QEMU `-no-reboot`, KVM, **no TPM and no network
      device**; `01_reset.yaml` runs the reset in its `network` stage (after
      networkd's 120 s wait for a network that is not there) and QEMU exits
      at the reboot that follows;
   4. refuse the result unless `COS_STATE` holds `cOS/active.img` (the
      partitions alone prove nothing: `01_reset.yaml`'s layout stage creates
      them, empty, before the reset runs), `COS_PERSISTENT` exists and is not
      LUKS, and the console shows no boot failure;
   5. give `COS_PERSISTENT` a **fresh, empty filesystem** (the build boot's
      logs and machine-id go with the old one);
   6. put the image's own cloud-configs back on `COS_OEM` (`01_reset.yaml`
      stays gone, so a clone never resets);
   7. refuse the result unless `COS_PERSISTENT` is empty.

   A clone of the result boots once, into the active system, where
   `encrypt_on_boot` from the restored configuration seals `COS_PERSISTENT`
   to **its own** vTPM and the guest reports as today (ADR-0088 Decision 5).
2. **GRUB's menu timeout is 0 on a pre-laid image**, set as `timeout=0` in
   `COS_OEM`'s `grubenv`: that is the `/grubenv` GRUB already loads after
   its `set timeout=10`, so nothing Kairos ships is shadowed. Not a
   `grubcustom` on `COS_OEM`, for the reason above. Fallback after a failed
   boot still works (boot assessment); what is lost is choosing an entry on
   the console, which a pool member has no operator to use.
3. **The builder pre-lays, never the host.** A host runs exactly the bytes
   that were built and pinned by digest (ADR-0064); a host-side pre-lay
   would run something other than the published artifact. The first-party
   tool is `scripts/prelay-kairos-raw.sh` (root, KVM), usable now for
   `BackingFile` images on any KVM host; the imagebuilder runs the same
   steps as a build step after AuroraBoot, in a follow-up that needs a
   builder image carrying QEMU, OVMF, e2fsprogs and gdisk.
4. **A pre-laid image is laid out for one disk size.** `COS_PERSISTENT`
   fills the disk it was laid out on. A clone's disk must be at least that
   size.
5. **The Cloud Hypervisor provider requeues a VMM start step after 1 s, not
   3 s.** Each step (manufacture, swtpm, VMM unit, API socket) is well under
   a second of work.

**Accept when:** the imagebuilder pre-lays a `Url` image end to end; a
`GuestReady` pool of a pre-laid image warms on Cloud Hypervisor and libvirt;
`make ch-sealing-isolation-e2e` is green against a pre-laid image (met
2026-10-31, below); and Open question 1 is answered.

## Measured (2026-10-31, before acceptance)

Three clones per row, one Cloud Hypervisor host, `tpmEnabled` class (2
vCPU, 4 GiB), Debian 13 + kairos-init v4.4.0, creation to `GuestReady=True`
with `sealedPartitions: [COS_PERSISTENT]`:

| Image and provider | Mean | Runs |
| --- | --- | --- |
| Raw, v0.4.8 provider | 106.3 s | 106, 108, 105 |
| Pre-laid with GRUB timeout 0 (Decisions 1, 2), v0.4.8 provider | 41.3 s | 41, 39, 44 |
| The same, with 1 s pacing (Decision 5) | **39.3 s** | 39, 40, 39 |
| Built by `scripts/prelay-kairos-raw.sh`, with 1 s pacing | **39.3 s** | 39, 39, 40 |

The script's build boot takes about three minutes (177 s, two of them the
networkd wait). `make ch-sealing-isolation-e2e` passed against the
script-built image (393 s): member A sealed and announced in 40 s, its
disk copied to a machine with a fresh vTPM never unlocked
(`FirstBootSealingTimeout`), and A unlocked again under its own vTPM. Each
clone seals its own copy of the plaintext partition, as Decision 1 says.

In the pre-laid boot the sealing window (`rootfs` to `initramfs`) takes
12.1 s, against 7.9 s in the raw image's active boot: the encryption now
visibly happens there, where in the raw flow part of it ran in the recovery
boot. What remains: provider 5 s, **9 s from the VMM starting to the firmware
handing over to GRUB** (Cloud Hypervisor's firmware, worth its own look),
kernel to `rootfs` 8 s, sealing 12 s, report 4 s.

Found on the way, and what the steps above now guard against: an early
version of the tool reported success on a run whose reset never happened,
because the layout stage had already created the partitions and QEMU exits
0 when it is sent SIGTERM. Step 4's `active.img` check is that lesson.
Starting the reset from the recovery boot's `boot` stage, to skip the 120 s
networkd wait, hangs after the reset mounts the system images; it is not
used.

## Open questions

1. **Does `COS_PERSISTENT` grow on a larger clone disk?** Kairos's layout
   stage expands the last partition in recovery; whether the active boot of
   a pre-laid clone does the same for a disk larger than the image is
   unverified. Until it is, Decision 4 is the rule.
2. **Stages bound to the reset** (`before-reset`, `after-reset`,
   `after-reset-chroot`) no longer run on a pre-laid clone, and Decision 1
   keeps them from running at build time. An image that relies on them must
   move that work to a boot stage. Is a builder-side warning enough, or
   should the builder refuse such an image?

## Consequences

- A sealed clone reaches `GuestReady` in about 39 s instead of 106 s, with
  the same per-clone sealing and the same isolation argument: the image is
  plaintext and empty where it matters, and each clone seals its own copy to
  its own vTPM (roadmap 18 invariants 2 and 5).
- The image is 20 GiB apparent (sparse; about 3.3 GiB of data, 1.2 GiB
  compressed), against 2.1 GiB for the raw image: hosts store it sparse
  (reflink or `cp --sparse`), the registry stores it compressed.
- Building needs KVM (or a much slower TCG) and root for loop devices and
  mounts; it is a build-time cost paid once per image revision, about two to
  three minutes.
- The console GRUB menu is gone on pool images; boot-assessment fallback is
  not.
- Reset-time stages stop running on clones (Open question 2).
- The ≤ 30 s target of roadmap 18 is not met by this ADR alone; the next
  levers are the firmware's 9 s and the active boot.
