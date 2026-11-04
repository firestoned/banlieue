<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0113: Pre-laid images for fast sealed clones

- **Status:** Accepted
- **Date:** 2026-11-02
- **Proposed:** 2026-10-31
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-10-31 (Open question 1 answered: `COS_PERSISTENT`
  does not grow; Decision 4 is the rule; see *Amendment: one disk size*);
  2026-11-01 (Decision 3's imagebuilder step and its API; see *Amendment:
  the imagebuilder pre-lays*); 2026-11-02 (accepted: every criterion met;
  see *Amendment: accepted*); 2026-11-04 (banlieue publishes the pre-lay
  image and the imagebuilder defaults to it; see *Amendment: a published
  pre-lay image*); 2026-11-04 (the pre-lay image gets a Grype gate of its
  own; see *Amendment: a Grype gate for the pre-lay image*)
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
4. **A pre-laid image is laid out for one disk size; pre-lay at the
   clones' disk size.** `COS_PERSISTENT` fills the disk it was laid out on
   and does not grow on a larger clone disk (*Amendment: one disk size*). A
   clone's disk must be at least the image's size; the Cloud Hypervisor
   provider already refuses an image larger than the requested disk
   (`crates/banlieue-provider-cloud-hypervisor/src/hostfs.rs`,
   `ensure_os_disk`). A larger disk works but its extra space is unused, so
   a class with a different disk size needs its own pre-laid image.
5. **The Cloud Hypervisor provider requeues a VMM start step after 1 s, not
   3 s.** Each step (manufacture, swtpm, VMM unit, API socket) is well under
   a second of work.

**Accept when** (all met 2026-11-02, *Amendment: accepted*): the imagebuilder
pre-lays a `Url` image end to end (implemented 2026-11-01, run in a cluster
2026-11-02); a
`GuestReady` pool of a pre-laid image warms on Cloud Hypervisor and libvirt
(met: Cloud Hypervisor 2026-10-31, libvirt 2026-11-01, 65 s, below);
`make ch-sealing-isolation-e2e` is green against a pre-laid image (met
2026-10-31, below); and Open question 1 is answered (met 2026-10-31: no).

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
networkd wait).

On libvirt (2026-11-01), a pre-laid image with a guest agent, the
file-based phase marker and the guest-side EK export, uploaded as a
`BackingFile` volume to one host's pool: a two-member `GuestReady` pool of a
`tpmEnabled` class (2 vCPU, 4 GiB, 20 GiB) warmed in **65 s**, both members
with `sealedPartitions: [COS_PERSISTENT]` and an EK certificate (34 s and
64 s to `GuestReady`). The raw image's pool on libvirt took 125 s (ADR-0088). `make ch-sealing-isolation-e2e` passed against the
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

1. **Does `COS_PERSISTENT` grow on a larger clone disk?** Answered: **no**
   (*Amendment: one disk size*).
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

## Amendment: one disk size (2026-10-31)

**Open question 1 is answered: `COS_PERSISTENT` does not grow.** An image
pre-laid at 20 GiB was cloned on Cloud Hypervisor (v0.4.8) with a 40 GiB and
a 20 GiB class. Both sealed and unlocked; the provider grew the 40 GiB
clone's `os.raw` to 40 GiB, and inside the guest the partition table was
identical on both: `persistent` 13.1 GB, its filesystem 12.8 GB, the extra
20 GiB unused. The GPT still describes the 20 GiB disk, its backup header at
the old end.

Making the guest grow the partition on its first boot was tried and **is not
adopted**:

- a Kairos layout stage (`rootfs.before`, `expand_partition: size: 0` on
  `COS_PERSISTENT`) **rewrote partition 5 without growing it and dropped its
  `persistent` partition label**: the layout tool expands to the end the GPT
  describes;
- relocating the backup GPT header first needs `sfdisk` (or `sgdisk`), and
  the installed system's initramfs has neither (exit 127);
- the guard meant to keep the stage away from a sealed partition (`blkid`
  reporting `crypto_LUKS`) **did not hold in that stage**: the stage ran
  again on the clone's second boot, against the sealed partition. It changed
  nothing there, but a partition-table rewrite on every boot of a sealed disk
  is the opposite of what this ADR is for.

So Decision 4 is the rule: pre-lay at the clones' disk size. Two ways to lift
it remain, neither decided: the builder pre-lays one image per class disk
size (cheap, a build per size); or the provider relocates the backup GPT
header when it grows a fresh clone's `os.raw`, before first boot (the disk is
still a pristine copy then, but the provider would be parsing an
image-controlled partition table on the host, a new trust boundary that
needs its own ADR).

## Amendment: the imagebuilder pre-lays (2026-11-01)

Decision 3 placed pre-laying in the builder. This is how:

1. **`VMImage.spec.prelay: { diskSizeGiB }` asks for it.** It applies to a
   `Url` source whose build is a raw `cloudImage`; for an `iso` build the
   imagebuilder refuses (`buildArtifact.phase: Failed`, reason
   `PrelayNeedsCloudImage`). `diskSizeGiB` is the clones' OS disk size
   (Decision 4), at least 10 (the reset lays out about 8 GiB of `COS_STATE`
   and `COS_RECOVERY` before `COS_PERSISTENT`). Pre-laying is not tied to
   `firstBootSealing`: any raw Kairos image skips its recovery path this way.
2. **A pre-lay Job runs between the build and the artifact being `Ready`.**
   When the `OSArtifact` is `Ready`, the imagebuilder creates
   `<osartifact>-prelay` in the build namespace, owned by the `OSArtifact`
   (so a rebuild garbage-collects it, as the push Job), mounting the
   artifacts PVC. Until it succeeds `buildArtifact` stays `Building`, reason
   `Prelaying`, so no import Job or registry push starts on the raw file.
3. **The Job keeps SEC-004's chain intact.** It verifies the built raw file
   against the source's `checksum` (when set) **before** touching it, writes
   the pre-laid image to `<osartifact>-prelaid.raw` beside it (the raw file
   is never modified), and reports the result's `sha256:<hex>` as its
   termination message. The imagebuilder then publishes `buildArtifact.file`
   as the pre-laid file, `buildArtifact.checksum` as that digest (consumers
   verify what they receive against a digest the builder computed from a
   verified input) and `buildArtifact.prelaidDiskSizeGiB`. A failed Job
   publishes `Failed`, reason `PrelayFailed`, with its message.
4. **The Job's image is the operator's** (`--prelay-image`): it must carry
   `scripts/prelay-kairos-raw.sh`, QEMU, OVMF, e2fsprogs, gdisk and
   util-linux; `Dockerfile.prelay` builds one. With the flag unset, a
   `VMImage` asking for `prelay` fails with `PrelayImageUnset` rather than
   silently shipping the raw file.
5. **It needs KVM on the build node.** The Job is privileged (the build
   namespace already is, for kairos-operator's builds, ADR-0016) and uses
   the node's `/dev/kvm`; it inherits the imagebuilder's build scheduling
   (`--build-node-selector`, `--build-toleration`), which is how an
   operator puts builds on KVM-capable, dedicated nodes.
6. **QEMU runs sandboxed.** The script starts QEMU with
   `-sandbox on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny`
   (seccomp), no network device and no TPM: the guest is the image being
   built, run once, with nothing to reach.

## Amendment: accepted (2026-11-02)

**The imagebuilder pre-laid a `Url` image end to end, in a cluster**, the
last criterion. A kind cluster with privileged nodes (so pods see the host's
`/dev/kvm` and loop devices), kairos-operator and `banlieue imagebuilder
--prelay-image` built from `Dockerfile.prelay`. The `VMImage`: a `Url` source
`quay.io/kairos/hadron:v0.6.3-core-amd64-generic-v4.4.0` pinned by digest, a
cloud-config layer (a user and the Cloud Hypervisor phase report),
`firstBootSealing: {}` and `prelay: { diskSizeGiB: 20 }`.

- kairos-operator built the raw image; the artifact stayed `Building`,
  reason `Prelaying`, while the pre-lay Job ran; nothing consumed the raw
  file.
- The Job booted the build once under QEMU inside its pod (168 s), re-made
  `COS_PERSISTENT` empty and reported `sha256:a3b85589…`. The artifact became
  `Ready` with `file: <osartifact>-prelaid.raw`, that digest as `checksum`
  and `prelaidDiskSizeGiB: 20`; the digest recomputed from the PVC's file
  matched byte for byte, and the built raw file sat untouched beside it.
- The image held what Decision 1 requires: `COS_STATE` with `active.img`
  and `passive.img`, `COS_PERSISTENT` empty and plaintext, `COS_OEM` with only
  the image's own configuration (its sealing policy included) and `grubenv`
  with `timeout=0`, nothing LUKS.
- A clone of that exact artifact, through banlieue on a Cloud Hypervisor
  host (`tpmEnabled` class, `BackingFile` copy of the artifact, digest
  checked after the copy), sealed (`sealedPartitions: [COS_PERSISTENT]`, two
  EK certificates) and reached `GuestReady` **30 s** after creation: roadmap
  18's target, on Hadron, against 40 s for the Debian test image.
- The failure path was seen too: before the fix below, the Job failed and
  the artifact went `Failed`, reason `PrelayFailed`, with no `file`.

**What the run found, both fixed before acceptance:**

1. `Dockerfile.prelay` could not build: the repository's `.dockerignore`
   excludes `scripts/`, so its `COPY` found nothing. `.dockerignore` now
   lets that one script through.
2. Inside a container the script found no partitions ("no COS_OEM
   partition"). `losetup -P` makes the kernel create `loopNpM`, but a
   container's `/dev` is a snapshot from container start and no udev adds
   the new nodes. The script now makes any missing partition node from
   `/sys/block/<loop>/<partition>/dev` (`ensure_part_nodes`); on a host udev
   has already made them and it does nothing.

**Retrying a failed pre-lay** takes deleting the Job (it allows no retries)
and touching the `VMImage` (any annotation change), because a `Failed`
artifact is otherwise only re-examined on the long requeue.

## Amendment: a published pre-lay image (2026-11-04)

Decision 4 of *the imagebuilder pre-lays* left the pre-lay Job's image to
the operator: build `Dockerfile.prelay`, push it somewhere and set
`--prelay-image`. Nobody can use `spec.prelay` without doing that first, and
an image each operator builds is an image nobody signs. banlieue now
publishes it.

1. **CI publishes `ghcr.io/firestoned/banlieue-prelay`.** The build
   workflow builds `Dockerfile.prelay` on every event and pushes it where it
   pushes the banlieue image, with the same tags (`pr-<N>`, `sha-<short>`,
   `latest` and `main-<date>` on main, `v<version>` and `v<major>.<minor>`
   on a release). Every pushed image is signed with Cosign, keyless, by
   digest; on main and on a release it also gets a build-provenance
   attestation and an SBOM, as the banlieue image does (ADR-0006).
2. **linux/amd64 only.** The script boots an x86_64 Kairos image under KVM;
   an arm64 node has no x86 KVM to run it with. Pre-laying an arm64 image is
   out of scope until someone needs it.
3. **The imagebuilder defaults to it.** `--prelay-image` defaults to
   `ghcr.io/firestoned/banlieue-prelay:v<its own version>`, the way
   `--push-image` defaults to the public banlieue image. Pre-laying still
   happens only for a `VMImage` that sets `spec.prelay`; the default only
   removes the setup step. An empty value (`--prelay-image ""`) turns it off,
   and such a `VMImage` still fails with `PrelayImageUnset`.
4. **`banlieue bootstrap imagebuilder` writes the reference explicitly**:
   `<registry>/banlieue-prelay:<version>` under `--registry` (a mirrored
   cluster cannot pull ghcr.io), and pinned with the new
   `--prelay-image-digest`. It is its own flag because it is its own image
   with its own digest; `--image-digest` pins the banlieue image, and a
   privileged Job deserves a digest for the same reason the Deployments do
   (ADR-0103: a tag can move under a running cluster).
5. **No Grype gate yet.** The banlieue image's gates (Grype with VEX from
   `auto-vex-presence` and `auto-vex-reachability`) are built around one
   static Rust binary. The pre-lay image is a Debian 13 userland (QEMU,
   OVMF, e2fsprogs, gdisk, util-linux) whose findings those tools cannot
   triage, and a gate nobody can pass would only be switched off. Its base
   is pinned by digest and re-pinned by Dependabot, and its SBOM is
   published so a consumer can scan it. Recorded as an accepted risk in the
   threat model, to revisit when the image gets a scan gate of its own.
   *Superseded by Amendment: a Grype gate for the pre-lay image.*

## Amendment: a Grype gate for the pre-lay image (2026-11-04)

Decision 5 of *a published pre-lay image* shipped the image with no scan
gate, because the banlieue image's gate cannot judge it. That reasoning
holds for the banlieue gate; it does not stop the pre-lay image having one
built for what it is.

1. **The pre-lay image has its own Grype gate.** `make grype-prelay`
   (`grype-prelay` job in `.github/workflows/build.yaml`) scans the pushed
   image by digest twice. The first scan reports every finding as SARIF to
   GitHub Code Scanning (category `grype-<event>-Prelay`) and never fails.
   The second is the gate: it fails on a finding at or above **high** that
   Debian has **already fixed** (`--only-fixed --fail-on high`).
2. **Only a fixed finding fails it.** Debian marks most of a distribution
   userland's findings `wont-fix` or `not-fixed`; a gate on those is the gate
   nobody can pass that Decision 5 warned about. Built on 2026-11-04 from
   `Dockerfile.prelay`, the image has 333 findings, 82 of them high or
   critical, and **none with a fix**: every high is `wont-fix` (80) or
   `not-fixed` (1). The gate passes, and still says something: when Debian
   fixes a high, an image built without the fix fails until it is rebuilt
   or re-pinned. The unfixed findings stay visible in Code Scanning.
3. **Its VEX is its own: `.vex/prelay/`.** Statements there name
   `pkg:oci/banlieue-prelay` and are passed to both scans as `--vex`. They
   are not merged into the banlieue image's VEX (`build-vex` globs
   `.vex/*.json` only), and none of the auto-VEX tools run on this image:
   their arguments (an SBOM without the package, a binary without the
   symbol) are about one static Rust binary. The directory starts empty; a
   statement is needed only for a fixed finding that cannot be taken or that
   does not apply to how the Job runs the package.
4. **It scans every pushed image and gates the release.** It runs wherever
   `docker-prelay` pushed (in-repo pull requests, main, releases; a fork's
   pull request pushes nothing to scan), and the release's asset upload
   waits for it. It is **not** one of the pull request's required checks: a
   Debian fix published the day a pull request runs would otherwise block an
   unrelated change.

A fix that lands in Debian is not picked up by a cached build: the
`apt-get install` layer is reused until `Dockerfile.prelay` changes. A
Dependabot re-pin of the base changes the `FROM` line and rebuilds that
layer; until then, a failing gate is cleared by a rebuild without the cache.

