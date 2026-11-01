# 18: Fast sealed clones: isolation proof, boot speed, verified base

> **Goal.** A `tpmEnabled` pool member is cloned, sealed to its own fresh
> vTPM and `GuestReady` in **seconds**, on every backend, and the isolation
> that sealing is supposed to buy is **proven live**, not assumed: no
> member's sealed disk opens under any vTPM but its own. Memory forking stays
> rejected (ADR-0052).
>
> **Stop condition.** On libvirt, vSphere and Cloud Hypervisor: a
> `GuestReady` pool of `tpmEnabled` members goes clone → `GuestReady=True`
> in **≤ 30 s** per member; every backend runs the cross-member negative test
> green; and the base image is integrity-verified and scanned for secrets at
> build time.

**Rewritten 2026-10-31.** This roadmap was written (2026-09-24, baseline
`677be59`) as "split-image fast clone": a dm-verity base plus an empty
per-VM LUKS volume, because Kairos could then only encrypt at install time
(ADR-0040), so the only per-VM sealed disk was a per-VM install. Kairos
v4.4.0 changed that. **ADR-0088** (libvirt, vSphere) and **ADR-0112** (Cloud
Hypervisor) now clone an `Immediate` image and let Kairos seal
`COS_PERSISTENT` to the clone's own vTPM on its first boot. That removed the
per-clone install, which was the bulk of this roadmap's cost, by a different
route than the one planned here. What is left is below; what was planned and
is no longer needed is recorded under *Superseded*.

## Where it stands

| Measure | Before (2026-09) | Now | Target |
|---|---|---|---|
| Per-clone OS install | Every member (`Deferred`) | **None** (first-boot sealing, ADR-0088/0112) | None |
| Pool warm-up, 2 members | 130.3 s vSphere, 158 s libvirt, 341 s one CH install | **125 s** libvirt, **125–130 s** Cloud Hypervisor | ≤ 30 s per member |
| One sealed member, creation → `GuestReady` | n/a | **39.3 s** Cloud Hypervisor with a pre-laid image (106.3 s raw; phase C) | ≤ 30 s |
| Cross-member negative test | Not run | **Green on Cloud Hypervisor** (2026-10-31) | Green on every backend |
| Base integrity (dm-verity / UKI) | None | None | Verified base |
| Build-time secret scanner | None | None | Fails the build |

Where the time goes now: a raw clone has no `COS_STATE` or `COS_PERSISTENT`
yet, so its first boot runs recovery, whose reset lays out the partitions,
then reboots into the active system, which seals `COS_PERSISTENT` and
reports (ADR-0088 *Amendment 2026-10-26*). The install is gone; a recovery
boot, a reset and a second boot remain, 54 % of a lone member's 106 s, and
GRUB's 10 s menu wait on each boot is another 19 % (phase C).

## Security invariants (every phase must preserve all five)

| # | Invariant | Status |
|---|---|---|
| 1 | **The base image contains no secrets and no identity.** No machine-id, SSH host keys, tokens, enrolled TPM material or pre-created keyslots. Enforced at build time with a scanner, not a convention. | ⛔ Convention only. Phase D. |
| 2 | **One volume key per member, sealed to that member's vTPM only.** No fleet key, no escrow copy. The negative test is a live deliverable, not an assumption. | 🔶 Proven on Cloud Hypervisor (phase B); libvirt and vSphere open. |
| 3 | **The base is integrity-bound to the image artifact.** | ⛔ Phase D. |
| 4 | **`GuestReady` implies sealed.** A member that cannot seal never announces. | ✅ ADR-0088 Decision 5 readback on all three backends; `FirstBootSealingTimeout` for a clone that halts. Caveat below. |
| 5 | **Fresh vTPM per clone, never migrated, never duplicated.** | ✅ All three backends (ADR-0039, ADR-0050, ADR-0065). |

**The caveat on invariant 4.** The sealing report says each listed partition
*is* a LUKS container, not that the guest *unlocked* it. A guest that
booted without opening its data volume would still report it sealed if it
got far enough to report. Today that cannot happen with Kairos, which halts
in the initramfs when an unlock fails (the negative test observes exactly
that: no report, `FirstBootSealingTimeout`), but the guarantee comes from
Kairos's behaviour, not from what banlieue reads. Phase B's test asserts the
copy is never `GuestReady`, so a Kairos change that degraded instead of
halting would turn the suite red.

## Fixed constraints (not up for re-litigation here)

| Constraint | Source |
|---|---|
| No memory forking, on any backend. Instant Clone, snapshot-restore fan-out and fork-from-golden all copy the parent's RAM, including its unsealed volume key, so no vTPM swap restores isolation afterwards. This roadmap is **disk-only** fast cloning. | ADR-0052 |
| Never clone TPM state. Fresh vTPM per member. | ADR-0040, roadmap 17 §D/§E |
| Never clone a disk that has been *sealed*. Clones are taken from the unsealed image; sealing happens inside each clone, on its own first boot. | ADR-0040, ADR-0088 |
| The EK certificate gate stands: a `tpmEnabled` member is unbindable until it publishes `status.tpmEndorsementCertificates`. | ADR-0045 |
| CR-to-CR only; pools and claims never talk to a provider. | 01-DECISIONS |

## A. Per-clone sealing without an installer ✅

Delivered by ADR-0088 (Accepted 2026-10-26) and ADR-0112 (Accepted
2026-10-28), not by this roadmap's original phases 0–C:

- [x] An `Immediate` `VMImage` declares `firstBootSealing`; the controller
      copies it onto the infra CR; `tpmEnabled` + `Immediate` is valid when
      it does (ADR-0048 superseded).
- [x] The imagebuilder appends a sealing layer (Kairos `encrypt_on_boot` on
      the listed partitions, plus a report on every backend's channel).
- [x] Each provider reads the report back and gates `GuestReady` on it, with
      a deadline: libvirt (`qemu-guest-agent`), vSphere (`guestinfo`), Cloud
      Hypervisor (vsock `sealed=` line).
- [x] Live: vSphere end to end (2026-10-22), libvirt pool 125 s
      (2026-10-26), Cloud Hypervisor pool 125–130 s (2026-10-28).

## B. Cross-member isolation proof (invariant 2) 🔶

Every clone of one image gets the **same LUKS header UUID**, because every
clone formats the same partition of the same image (ADR-0088, *Evidence from
a live run*). That is harmless only if each clone's volume key is its own
and is sealed to its own vTPM. The test that shows it:

1. seal member A through banlieue, and confirm from its console that
   `COS_PERSISTENT` is mounted from a device-mapper device (unlocked);
2. power A off and copy its sealed disk;
3. boot the copy as a new machine with a freshly manufactured vTPM: it must
   never unlock, and never be `GuestReady`;
4. power A on again with its own vTPM: it unlocks, so step 3 failed because
   of the vTPM, not because the copy was damaged.

The console probe is a test-only cloud-config stage
(`banlieue-e2e-persistent=$(findmnt -no SOURCE /usr/local)` to `ttyS0`),
because the sealing report cannot tell unlocked from merely LUKS (the
invariant 4 caveat).

- [x] **Cloud Hypervisor**: `make ch-sealing-isolation-e2e`
      (`crates/banlieue-provider-cloud-hypervisor/tests/e2e_sealing_isolation.rs`).
      **Green 2026-10-31**: A sealed and unlocked (`/dev/mapper/vda5`) in
      110 s; the copy under a fresh vTPM never unlocked and timed out
      (`FirstBootSealingTimeout`, 300 s budget); A unlocked again 30 s after
      power-on.
- [ ] **libvirt**: the same four steps against `LibvirtMachine` (copy the
      domain's overlay flattened onto its base, or the volume with
      `qemu-img convert`). The probe can read back through the guest agent
      instead of a console.
- [ ] **vSphere**: the same, copying the VMDK into a new VM with a fresh
      vTPM (ADR-0039).
- [ ] **Capture the failure, not only its absence.** The installed system's
      kernel does not write to the serial console, so step 3's evidence is
      that the copy never unlocked while the same disk does under its own
      vTPM: a differential proof. Add `console=ttyS0` to the installed
      boot's command line in the test image so the run also records
      Kairos's own unlock failure.

## C. Boot path to ≤ 30 s 🔶 (106 s → 39 s)

What remains is boot, not install: recovery boot, reset, reboot, seal,
report. Any path that changes a `VMImage` contract or a provider's disk
layout is an ADR.

- [x] **Measure the split** (2026-10-31). Three sealed VMs, one at a time,
      on one Cloud Hypervisor host (32 cores, the patched VMM), banlieue
      v0.4.8, `ch-tpm` class (2 vCPU, 4 GiB), the ADR-0112 test image (Debian
      13 + kairos-init v4.4.0, raw) plus a test-only layer printing a marker
      to `ttyS0` at every Kairos stage. A host-side sampler stamped each
      serial line with host time; cluster times are the conditions'
      `lastTransitionTime`. Creation to `GuestReady=True`: **106, 108 and
      105 s** (a lone member; a pool of two on one host took 125 s). The
      three runs agree within 2 s at every step:

      | Seconds after creation (mean of 3) | Step | Takes |
      | --- | --- | --- |
      | 0 → 8.0 | provider: reflink disk, manufacture swtpm, start the VMM (`Ready=True`) | 8.0 s |
      | 8.0 → 14.7 | firmware to GRUB | 6.7 s |
      | 14.7 → 26.9 | **GRUB menu countdown** (10.3 s), then recovery kernel | 12.2 s |
      | 26.9 → 51.7 | recovery boot: kernel, `rootfs`, `initramfs`, `boot`, `before-reset` | 24.8 s |
      | 51.7 → 64.9 | **reset** (lays out `COS_STATE` and `COS_PERSISTENT`) | 13.2 s |
      | 64.9 → 72.5 | reboot: shutdown, firmware to GRUB | 7.6 s |
      | 72.5 → 84.7 | **GRUB menu countdown** (10.3 s), then the active kernel | 12.2 s |
      | 84.7 → 93.9 | active kernel to the `rootfs` stage | 9.2 s |
      | 93.9 → 101.8 | `rootfs` to `initramfs` (first-boot sealing runs here) | 7.9 s |
      | 101.8 → 106.7 | `boot` stage: the vsock `sealed=` and `phase=installed` reports, `GuestReady=True` | 4.9 s |

      What it says:
      - **The recovery path is over half of it**: GRUB, recovery boot, reset
        and reboot, 14.7 → 72.5, is **57.8 s (54 %)**.
      - **GRUB waits 10.3 s on each boot**, 20.6 s (19 %) of doing nothing.
      - **Sealing itself is cheap.** The active `rootfs` → `initramfs` window
        that contains it takes 7.9 s, *less* than the same window in the
        recovery boot (9.7 s), which seals nothing.
      - **The provider's 8 s is mostly pacing**: one step per reconcile pass,
        about 3 s apart (as `docs/src/reference/cloud-hypervisor-pool-timings.md`
        found for `Deferred`).
- [x] **The three candidates, done and measured (2026-10-31, ADR-0113,
      Proposed).** Same host, class and image, three clones per row,
      creation to `GuestReady=True` with `sealedPartitions: [COS_PERSISTENT]`:

      | Image and provider | Mean | Runs |
      | --- | --- | --- |
      | Raw, v0.4.8 | 106.3 s | 106, 108, 105 |
      | Pre-laid, GRUB timeout 0 (candidates 1 and 2), v0.4.8 | 41.3 s | 41, 39, 44 |
      | The same with 1 s provider pacing (candidate 3) | **39.3 s** | 39, 40, 39 |
      | Built by `scripts/prelay-kairos-raw.sh`, 1 s pacing | **39.3 s** | 39, 39, 40 |

      1. **GRUB timeout 0**: `timeout=0` in `COS_OEM`'s `grubenv`, the
         `/grubenv` GRUB already loads after its `set timeout=10`. Not a
         `grubcustom` on `COS_OEM`: GRUB sources the first one it finds, and
         the reset writes Kairos's boot-assessment hook as `COS_STATE`'s.
      2. **Pre-laid image**: `scripts/prelay-kairos-raw.sh` (with
         `scripts/prelay-kairos-raw-test.sh`) does the recovery reset once at
         build time. The design changed twice on contact: the recovery boot
         itself tries to encrypt `COS_PERSISTENT` (so the build boot must
         run with no `kcrypt` section, or it halts without a TPM, or seals to
         the builder's), and it logs a machine-id into `COS_PERSISTENT` (so
         the partition is re-made empty). `make ch-sealing-isolation-e2e`
         passed against the result.
      3. **Provider pacing**: `REQUEUE_TRANSITION` 3 s → 1 s; the machine
         reaches `Ready` in 5 s instead of 7 s. Smaller than the 5 s
         estimate.

      In the pre-laid boot the sealing window is 12.1 s (7.9 s in the raw
      flow's second boot, where the recovery boot had done part of it).
- [x] **The imagebuilder pre-lays `Url` images** (ADR-0113, *Amendment: the
      imagebuilder pre-lays*, 2026-11-01): `VMImage.spec.prelay.diskSizeGiB`;
      a privileged pre-lay Job after the build verifies the raw file against
      the source checksum, pre-lays it into a new file and reports its
      digest, which becomes the artifact's checksum. `Dockerfile.prelay`
      builds the Job's image (`--prelay-image`); the script's Job mode
      (environment arguments, checksum, result file, QEMU seccomp sandbox)
      ran live on a build host. Not yet run in a cluster: that needs the
      image built.
- [x] **A pre-laid pool on libvirt** (2026-11-01): two members `Warm` in
      65 s, against 125 s for the raw image (ADR-0088), both sealed with an
      EK certificate.
- [x] **Does `COS_PERSISTENT` grow on a larger clone disk?** No
      (2026-10-31, ADR-0113 *Amendment: one disk size*): a 20 GiB pre-laid
      image cloned to a 40 GiB disk keeps a 13.1 GB `persistent`, the rest
      unused. A first-boot grow stage was tried and rejected (no `sfdisk` in
      the initramfs, the layout tool dropped the partition label without
      growing it, and its plaintext guard did not hold). Pre-lay at the
      class's disk size.
- [x] **Where the firmware's 9 s goes** (2026-11-01). Twelve VMs on three
      hosts (prototype and tpmfix VMMs, with and without a vTPM), console
      and unit start times on one clock:

      | VMM unit started → first console byte | first console byte → firmware hands off to GRUB |
      | --- | --- |
      | 3.4 s on most runs (0.4 s on some) | 3.5 to 8.5 s, any host, any VMM, with or without a vTPM |

      About 3.4 s is **banlieue's pacing**: the provider starts the VMM unit
      and boots the VM on the next pass (3 s in v0.4.8; the 1 s
      `REQUEUE_TRANSITION` change cuts it). The swtpm → VMM gap is the same
      pass (3.05 s every time). The rest, **4 to 8 s, is the firmware
      itself**, silent on the console, and the same on the prototype and the
      tpmfix VMMs and with no vTPM at all: **not the vTPM polling**, and
      nothing for the upstream vTPM fix.
- [ ] **Find what the firmware waits on** for 4 to 8 s before choosing the
      boot disk: a debug build of the firmware (`CLOUDHV.fd`) with its
      log on the console. The largest step left between 39 s and 30 s.
- [ ] Re-measure the pool with whichever lands, on all three backends.

## D. Verified base and build-time scanner (invariants 1, 3) ⛔

- [ ] Build-time scanner over the image: fails the build on a machine-id,
      SSH host keys, LUKS keyslots, TPM state or other identity material.
      Note the image's `COS_OEM` carries the operator's cloud-config (users,
      password hashes): what is allowed there is part of the decision.
- [ ] Base integrity (ADR-0070, reserved): dm-verity root hash pinned by the
      image artifact, reaching the guest through a channel it cannot be lied
      to about cheaply; a signed UKI when ADR-0051's stall resolves.
- [ ] Threat-model pass: asset A-6's per-volume distinction, a row for the
      verity root hash (integrity, not confidentiality), §8 risk for any
      unsigned-cmdline interim with *Revisit when: UKI lands*.

## E. Proxmox ⛔ (with roadmap 06)

The Proxmox provider exists (roadmap 06) but has no create-from-ISO,
`GuestReady`, sealing readback or EK path. When it gets them it should start
from phase A's shape (fresh `tpmstate0` at create, never copied; first-boot
sealing readback), then phase B's test.

## ADR reservations

Reserved 2026-09-24: ADR-0068 to ADR-0073. ADR-0068 (the split `VMImage`
contract) and ADR-0069 (the first-boot sealing mechanism) were decided by
ADR-0088 instead and will not be written. Still reserved: ADR-0070 (base
integrity, phase D) and ADR-0071 to ADR-0073 (per-backend split provisioning
for vSphere, Proxmox and Cloud Hypervisor), which phase C writes only if it
picks the split model.

## Superseded

From the 2026-09-24 plan, no longer needed:

- **Phase 0's decision gate** (a fourth `installMode`, whether the stack
  stays Kairos, what happens to `Deferred`): answered by ADR-0088. The mode
  is `Immediate` + `firstBootSealing`; the stack stays Kairos; `Deferred`
  remains for full-disk persistent VMs.
- **Phase A's first-boot unit and phase B's API and controller work**:
  ADR-0088's sealing layer, readback and `image_class_mismatch` change.
- **"`GuestReady` written only after the volume is sealed"**: the readback
  gate does this on every backend (invariant 4).

## Testing (the four tiers, per `rules/testing.md`)

| Tier | What it proves here |
|---|---|
| Unit | sealing gate and deadline, report parsing, controller pairing rules (all landed with ADR-0088/0112) |
| Live protocol | phase C's disk layout accepted by each backend |
| Live API server | nothing new: claims and pools unchanged |
| E2E | clone → `GuestReady` timing; **cross-member unseal fails** on every backend; teardown leaves no disk, volume or TPM state |

The negative test is the point of the whole roadmap: a suite that only shows
members *can* seal, and never that siblings *can't* unseal each other, would
pass with a fleet-shared key.

## Explicitly out of scope

- **Memory forking in any form**: ADR-0052 is reaffirmed, not weakened.
- **Re-encrypting a shared base per member** (`cryptsetup reencrypt`): it
  rewrites every block, destroying the CoW sharing.
- **Network-bound or broker-released keys** (Tang/Clevis-style): a boot-time
  network dependency and a crown-jewel broker; its own ADR on top of
  ADR-0049 if ever wanted.
- **Sharing a vTPM across members**, ever.

## Relationship to other roadmaps

| Roadmap | Relationship |
|---|---|
| 17 (pools/sandboxes) | First consumer. Phase A already shrank its warm-pool math; phase C would shrink it again. |
| 15 (vSphere disk import) | Needed only if phase C picks the split model on vSphere (a pre-laid base disk). |
| 09 (Cloud Hypervisor) | Phase B's first green run is there; phase C's reflink path lands there. |
| 06 (Proxmox) | Phase E. |
| ADR-0051 (UKI) | Phase D's integrity story upgrades from "pinned hash" to "signed, measured chain" when it resolves. |
