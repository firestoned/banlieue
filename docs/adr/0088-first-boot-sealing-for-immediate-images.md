<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0088: `tpmEnabled` with an `Immediate` image, sealed on first boot

- **Status:** Proposed
- **Date:** 2026-10-05
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-10-22 (implemented behind the Decision 7 flag; Decision 2
  for ISO builds, Decision 3's flag-off handling, Decision 5's timeout
  origin; see *Amendment 2026-10-22*)
- **Supersedes:** [ADR-0048](0048-tpm-enabled-requires-deferred-install.md),
  on acceptance. ADR-0048 named its own end date: the day
  [kairos-io/kairos#4556](https://github.com/kairos-io/kairos/issues/4556)
  ships. Its Decision inverts rather than narrows, so it is superseded, not
  amended, exactly as its Expiry section asks.
- **Related:** Builds on [ADR-0040](0040-deferred-install-for-vtpm-encryption.md)
  (Deferred stays valid and unchanged),
  [ADR-0043](0043-guestready-installed-guest-signal.md) (redefines what
  `GuestReady` means for these images),
  [ADR-0045](0045-vtpm-endorsement-key-certificate.md),
  [ADR-0039](0039-vsphere-vtpm-support.md),
  [ADR-0050](0050-libvirtmachine-domain-lifecycle.md),
  [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md);
  [roadmap 17](../../.github/community/17-ephemeral-vm-pools.md) (warm pool
  sizing), [roadmap 18](../../.github/community/18-split-image-fast-clone.md)
  (the banlieue-side alternative, see Context)
- **Accept when:** all of these hold, and not before:
  1. a released Kairos contains first-boot sealing
     ([kairos-io/kairos#4924](https://github.com/kairos-io/kairos/pull/4924),
     merged 2026-09-25; the latest release, v4.3.0 of 2026-09-08, predates
     it);
  2. the upstream defects that break first-boot sealing in production are
     fixed in that release: kairos-io/kairos#5149 (one cryptsetup warning on
     stderr fails the boot), #5193 (a configured `c_index` seals with one
     index and unseals with another, so the node cannot unlock after its
     first reboot), #5158 (the post-unlock check cannot detect a failed
     unlock);
  3. Open question 1 below is answered.

## Context

### The fact ADR-0048 rested on has changed upstream

ADR-0048 rejected `VMClass.tpmEnabled: true` paired with an `Immediate`
image because Kairos partition encryption was install-phase-only: `kcrypt`
sealed LUKS keys during `kairos-agent install`, an `Immediate` template is
installed once before any clone's vTPM exists, so every clone booted
plaintext while looking sealed. Its Expiry section recorded that this was the
sole reason, "not anything about vTPMs, vSphere, libvirt or banlieue", and
that #4556 proposed exactly the missing capability.

That capability is now merged on Kairos `master`:

- **First-boot sealing (#4924, merged 2026-09-25).** A new immucore step,
  `encrypt-pending`, runs on the normal boot path when `COS_OEM` is
  plaintext, after OEM is mounted and before partitions are unlocked or
  mounted. It reads a policy from `COS_OEM` (or the kernel command line):

  ```yaml
  kcrypt:
    encrypt_on_boot: true
  install:
    encrypted_partitions:
      - COS_PERSISTENT
  ```

  For each listed partition that is still plaintext it runs the same
  `kcrypt` encryptor the install hook uses, against the TPM (or the
  challenger KMS) the machine has *now*. A partition that is already a LUKS
  container is skipped, so every boot after the first is a no-op. Partitions
  the running system depends on (OEM, state, recovery, EFI) are refused.
- **It fails closed.** If the policy asks for encryption and it cannot be
  performed (no TPM, KMS unreachable, the configuration cannot be read), the
  boot halts on a failure screen and reboots into the same halt; the step is
  fatal to immucore's boot graph, so the system never reaches userspace on
  plaintext. There is no plaintext fallback.
- **Reset parity (#4973, merged 2026-10-02).** A reset that reformats a
  partition the policy lists re-encrypts it, and refuses before formatting
  anything when it could not.
- **UKI is out of scope upstream.** UKI installs are always encrypted at
  install time and have no install opt-out, so there is nothing pending on a
  UKI image.

### Evidence from a live run

The full template flow was run against a Kairos `master` build on a
libvirt/KVM host with swtpm, outside banlieue (recorded 2026-09-26):

| Step | Observed |
|---|---|
| Install once, no TPM, no encryption config, power off | plaintext install |
| Write the policy into `COS_OEM` offline; never boot the template | template carries policy, no key material, no TPM state |
| Clone, attach a fresh vTPM to the clone only, boot | `encrypt-pending` sealed `COS_PERSISTENT` to the clone's TPM before any mount, 6.2 s; unlock 0.9 s; `/usr/local` on the mapper |
| Reboot the same clone | `already LUKS; encrypt-pending is a no-op`, 329 ms |
| Second clone of the same template | sealed on its own first boot; LUKS volume-key digests differ between the clones |
| Clone with no vTPM attached | halted on the failure screen, rebooting every 90 s; never reached userspace |

The LUKS header **UUID** was identical across clones: it derives from the
image's deterministic GPT GUIDs, not from the node. It is public metadata
with no functional impact, but nothing in banlieue may treat it as a machine
identity.

### The two questions ADR-0048 left for its successor

1. How does banlieue **know** an image was built with the first-boot policy
   in `COS_OEM`? Its answer had to be "an assertion on the `VMImage`, or
   something readable back from the artifact, not an assumption".
2. What does `GuestReady` mean when first boot includes an encryption step
   that can fail closed?

### Why upstream sealing rather than roadmap 18

Roadmap 18 (split-image fast clone, not started) would get template-speed
clones with per-VM sealing on the banlieue side: a qcow2 overlay over a
verified base plus an empty per-VM LUKS volume with fresh swtpm state. It is
backend work per provider, and it seals a *new* volume rather than the
Kairos persistent partition. First-boot sealing does the sealing where
Kairos already does it, through the encryptor and unlock path every Kairos
node uses, and needs no provider to understand LUKS. The two are not
exclusive; this ADR does not close roadmap 18.

## Decision

**1. `VMImage` gains a first-boot sealing declaration.**

```yaml
spec:
  firstBootSealing:
    partitions: [COS_PERSISTENT]   # default when the block is present
```

Its presence means "a clone of this image seals the listed partitions to its
own vTPM on its first boot". The partition list is validated at admission
with a `ValidatingAdmissionPolicy` (ADR-0007), since it is a single-object
check: non-empty, and none of `COS_OEM`, `COS_STATE`, `COS_RECOVERY`,
`COS_GRUB`, mirroring the refusal list Kairos enforces itself.

`firstBootSealing` together with `trustedBoot` is rejected at admission:
first-boot sealing does not exist for UKI upstream.

**2. How banlieue knows, by source kind.**

- **`Url` sources: by construction.** `banlieue-imagebuilder` generates the
  policy layer itself (`kcrypt.encrypt_on_boot: true` and
  `install.encrypted_partitions` from Decision 1) and appends it as the last
  entry of the merged cloud-config (ADR-0037), so no operator layer can
  override it. banlieue wrote the policy, so it does not have to trust that
  it is there. The build records `status.firstBootSealing: Built`.
  The builder refuses (`Ready=False`, reason `FirstBootSealingUnsupported`)
  when the source image's Kairos version predates the first release carrying
  #4924; that version is a constant set when the release ships.
- **`BackingFile` and `Template` sources: an operator assertion.** banlieue
  did not build the disk and cannot inspect it before boot, so the block is
  recorded as `status.firstBootSealing: Asserted`. This is the same class of
  claim as ADR-0040's `Manual`, and Decision 5's readback is what keeps it
  from being silent.

**3. The pairing rules, replacing ADR-0048's predicate.** The pure function
gains the image's sealing declaration:

```rust
#[must_use]
pub fn image_class_mismatch(
    tpm_enabled: bool,
    mode: InstallMode,
    first_boot_sealing: bool,
) -> Option<&'static str>
```

| `tpmEnabled` | install mode | `firstBootSealing` | Result |
|---|---|---|---|
| true | `Immediate` (or no `template`) | absent | **rejected**, as ADR-0048: silently plaintext |
| true | `Immediate` (or no `template`) | present | **allowed**: sealed per clone on first boot |
| true | `Deferred` / `Manual` | any | allowed, unchanged (ADR-0040) |
| false | any | present | **rejected**: every boot of every clone would halt fail closed |
| false | any | absent | allowed, unchanged |

The new rejection in the fourth row is not hypothetical: it is the
"forgot the vTPM" clone from the live run, which halts forever. Remote KMS
(challenger) sealing does not lift it, because the challenger attests the
node through its TPM.

Mismatches keep ADR-0048 Decision 4's handling unchanged: `Ready=False` with
reason `ImageClassMismatch`, no infrastructure CR, long requeue backed by the
`VMImage` and `VMClass` watches.

**4. Clone mechanics stay per backend and need no new provider behaviour.**

- **libvirt:** `Immediate` is a qcow2 overlay over the image volume
  (ADR-0050); `tpmEnabled` renders a fresh swtpm instance keyed by domain
  UUID. First-boot sealing writes the LUKS header and the new filesystem into
  the overlay; the base volume is never written. The libvirt guide's rule
  "never clone a domain that has TPM state" is unaffected: the base carries
  none.
- **vSphere:** clone the template, then attach the vTPM before power-on,
  which is ADR-0039's original mechanics. That path becomes correct again for
  a `firstBootSealing` image.
- **Cloud Hypervisor:** whether ADR-0065's vTPM path accepts an `Immediate`
  source is Open question 2; until answered, the scheduler does not place a
  `firstBootSealing` + `tpmEnabled` VM on a Cloud Hypervisor provider.

**5. `GuestReady` for a `firstBootSealing` image means "sealed and booted",
read back from the guest.**

- **The existing marker already implies success.** ADR-0043's announcement
  is a cloud-config `boot` stage, which runs in the booted system after the
  initramfs hands over. `encrypt-pending` is fatal to immucore's boot graph
  in the initramfs, so a boot whose sealing failed halts before that handover
  and the stage never runs. (The `/run/cos/active_mode` guard on the stage is
  not what provides this: immucore writes that sentinel early in its graph,
  before `encrypt-pending`. The guarantee comes from never reaching the
  booted system.) So `phase=installed` on a `firstBootSealing` image cannot
  be written by a boot whose sealing failed.
- **Plus an explicit sealing report.** The guest also writes which listed
  partitions are LUKS containers: `/run/banlieue/sealed` on libvirt and
  `guestinfo.banlieue.sealed` on vSphere, through the same transports and
  the same active/passive guard as ADR-0043. The report is produced by a
  cloud-config stage that `banlieue-imagebuilder` adds to `Url` builds
  alongside the policy, and that an asserted image must carry itself.
  `GuestReady` requires every partition in `firstBootSealing.partitions` to
  be reported. A missing or short report is `GuestReady=False` with reason
  `FirstBootSealingUnverified`, which is what turns an `Asserted` image that
  lies into a loud failure instead of a silent one.
- **A halted first boot is detected by deadline.** A guest halted fail
  closed never reaches userspace and never announces anything, which is
  indistinguishable in-band from a slow boot. After
  `firstBootSealing.timeoutSeconds` (default 600) from first power-on without
  `GuestReady`, the condition becomes `GuestReady=False` with reason
  `FirstBootSealingTimeout`. The serial console carries the failure screen
  with the actual cause.
- The ADR-0045 gate (`TpmEndorsementPending`) applies unchanged, since the
  class is `tpmEnabled`.
- **`Ready` does not change** (ADR-0043 Decision 4, ADR-0048 Decision 5).

**6. Deferred is unchanged and stays the right answer when an image cannot
carry the policy**, for example a non-Kairos image, or a UKI image. For
Kairos images that can, `Immediate` + `firstBootSealing` becomes the
preferred shape for pools (roadmap 17): one install amortised across every
clone.

**7. The feature is gated until acceptance.** `firstBootSealing` is behind a
controller feature flag, default off, so ADR-0048's rejection keeps holding
for every cluster until the Accept-when conditions are met. With the flag
off the field is rejected at admission, not silently ignored.

## Consequences

**Clones get per-node sealing at template speed.** The template carries
policy and no key material, so it stays safe to copy, export and store; every
clone's TPM identity and LUKS key are born on that clone. The per-clone cost
is one `encrypt-pending` pass on first boot (6.2 s for an empty persistent
partition in the live run) instead of a full install per VM.

**First boot destroys what the template left on the sealed partitions.**
Sealing reformats them. An image author who bakes data into
`COS_PERSISTENT` loses it on every clone's first boot. This is inherent and
is documented on the field.

**`COS_OEM` is never sealed, and per-VM user-data is persisted on it.**
First-boot sealing reads its policy from `COS_OEM` before anything is
unlocked, so OEM itself stays plaintext, and Kairos refuses to seal it on
first boot. That covers more than the image's baked cloud-config: on the
first boot that finds a datasource, Kairos's datasource stage pulls the
per-VM user-data (the NoCloud seed on libvirt and Cloud Hypervisor,
guestinfo on vSphere) and writes it to `/oem/95_userdata` on `COS_OEM`. The
pull is guarded on that file not existing, so it happens once and the file
is never refreshed or removed. Anything delivered through
`VirtualMachine.spec.userData` therefore ends up in plaintext on the guest's
disk, next to the policy, for the life of the VM. For a Kubernetes node that
is typically the cluster **join token**, plus any registry or KMS
credentials. Sealing does not change this, and the documentation for
`firstBootSealing` must say so, so that no one reads "sealed" as "secrets at
rest are protected". The mitigations are outside this ADR: short-lived join
tokens (k0s and k3s both support an expiry) or fetching secrets at boot from
an authenticated source rather than shipping them in user-data. On libvirt
and Cloud Hypervisor the same user-data also sits in the seed ISO on the
storage pool, which the threat model already records (asset A-2); first-boot
sealing does not change that either.

**An `Asserted` image can still lie at boot time, and is caught at readback.**
A `BackingFile` image that claims the policy but does not carry it boots
plaintext; Decision 5's sealing report makes it `GuestReady=False` instead of
healthy. An image that carries a forged report stage defeats the readback
too; that residual is recorded in the threat model next to ADR-0040's
`Manual`, and does not apply to `Url` builds, where banlieue wrote both the
policy and the report stage.

**A misconfigured clone is unavailable, never insecure.** Every way first
boot can fail (no TPM, KMS unreachable, upstream defect) halts the guest
rather than booting plaintext. The cost lands on availability: a pool member
that times out has to be replaced, not repaired in place.

**Behaviour changes on upgrade are confined to opted-in VMs.** With the flag
off nothing changes. With it on, the only newly allowed pairing requires the
new field, and the only newly rejected pairing (row four) could not boot
before either.

**On acceptance this ADR requires:** ADR-0048 marked *Superseded by 0088*;
the CALM model updated for the builder's generated layers and the new
readback; the threat model rows citing ADR-0048 (asset A-12, the
"looks sealed but plaintext" information-disclosure row, the `Manual` row,
and the Cloud Hypervisor vTPM row) rewritten in a full pass; an example
`VMImage` and `VMClass` pair; and the Kairos-on-libvirt guide updated.

## Amendment 2026-10-22: implemented behind the flag

Implemented ahead of acceptance, as Decision 7 anticipates: every new
behaviour sits behind the controller flag `--enable-first-boot-sealing`
(`BANLIEUE_ENABLE_FIRST_BOOT_SEALING`, default off). The status stays
*Proposed*, and the Accept-when list is unchanged: kairos-io/kairos#5193
closed on 2026-10-22, after `v4.4.0-rc2`, so no Kairos build yet carries all
three fixes. Four points of the Decision change in the implementing.

**A. Decision 2, for ISO builds: the policy is written into `COS_OEM` after
the install, never put in the install configuration.** Upstream #4924 is "the
immucore DAG step and config gating only": the install hook does not read
`kcrypt.encrypt_on_boot`, so `install.encrypted_partitions` in an ISO's
install configuration makes the installer encrypt at install time. An
`Immediate` template installs once with no vTPM, so that install fails.
banlieue's merge concatenates lists (ADR-0037), so an appended layer cannot
take the key back out either. For a `Url` build whose artifact is an ISO
(vSphere's Immediate path, ADR-0021), the generated layer therefore:

- writes the policy (`kcrypt.encrypt_on_boot: true` and
  `install.encrypted_partitions` from `spec.firstBootSealing.partitions`) as
  its own file, `/oem/91_banlieue_first_boot_sealing.yaml`, from an
  `after-install-chroot` stage. That is the installed disk's `COS_OEM`, where
  `encrypt-pending` reads it, and it is the same hook images already use to
  write `/oem` (for example a GRUB environment);
- refuses the build (`Failed`, reason `FirstBootSealingConflict`) when an
  operator layer of an `Immediate` image sets a non-empty
  `install.encrypted_partitions`. Removing it silently would be implicit; the
  install would fail anyway.

For a `CloudImage` build (no installer runs; libvirt and Cloud Hypervisor),
the layer carries both keys directly, since the image's cloud-config is what
lands in `COS_OEM`. Whether it does is still Open question 1; the guest
readback below is what fails loudly if it does not.

**B. Decision 2, provenance.** The builder records
`status.buildArtifact.firstBootSealingLayer: true` when it appended the
layer. The controller's `VMImage` reconciler, the only writer of
`status.firstBootSealing`, derives `Built` from that and `Asserted` for an
image with no `Url` source. The Kairos-version gate
(`FirstBootSealingUnsupported`) is **not** implemented: the builder never
learns the source image's Kairos version, and no stable release carries #4924
to name as the constant. The readback is the only protection against an
image too old to seal, and it fails loudly (`FirstBootSealingTimeout`).

**C. Decision 7, flag off: the `VirtualMachine` is refused, not the
`VMImage`.** An admission policy cannot read a controller flag. With the flag
off, a `VirtualMachine` whose image declares `firstBootSealing` gets
`Ready=False`, reason `FirstBootSealingDisabled`, and no infrastructure CR,
the same handling as `ImageClassMismatch`. Nothing boots expecting a policy
that was never honoured, and nothing is silently ignored. The admission policy
validates only what is independent of the flag: a non-empty partition list,
no `COS_OEM`, `COS_STATE`, `COS_RECOVERY` or `COS_GRUB`, and not together with
`trustedBoot`.

**D. Decision 5, the timeout origin.** `timeoutSeconds` counts from the
infrastructure machine's `Ready=True` transition, the end of provisioning,
which on every backend is the power-on. No new timestamp is stored. The
controller copies `firstBootSealing` into the infrastructure machine's spec
(`VSphereMachine`, `LibvirtMachine`), and the provider records the reported
partitions in a sticky `status.sealedPartitions`, alongside the sticky
`guestInstalled`. The guest report is `guestinfo.banlieue.sealed` (vSphere) or
`/run/banlieue/sealed` (libvirt): a comma-separated list of the listed
partitions found as LUKS containers (label `<P>` or `<P>_LUKS`, both shapes
from kairos-io/kairos#4403), written from a `boot` stage the builder appends.

**Live run, 2026-10-22 (vSphere, Kairos `v4.4.0-rc1`).** An `Immediate`
Debian 13 image with `firstBootSealing: {}` built with
`status.firstBootSealing: Built`, and its clone got its own vTPM. The run found
two defects, both fixed:

1. **The policy file had no `#cloud-config` header.** Kairos's collector
   silently skips a config file without one (`HasValidHeader`, and immucore
   scans with logging off), so `encrypt-pending` saw no
   `kcrypt.encrypt_on_boot` and was a no-op. The gate held, as designed:
   `GuestReady=False`, `FirstBootSealingUnverified`. With the header added,
   the next boot encrypted `COS_PERSISTENT` (LUKS2, `COS_PERSISTENT_LUKS`) in
   about 4 s and unlocked it from the clone's own vTPM ("Local TPM NV
   passphrase"), and the report step published `COS_PERSISTENT`.
2. **vSphere never recorded the report.** The provider writes a provisioned
   machine's status only when something changed. The pass on which the
   report arrives closes the gate, so "gate open" no longer forced the write
   and `sealedPartitions` / `GuestReady=True` were never written. A changed
   sticky `sealedPartitions` is now a trigger. libvirt re-patches every pass
   and was not affected.

With both fixes in the image, a rerun from a fresh build passed with no
manual step. The image took about 2 m 52 s to build and import into three
zones, once. A clone went from `kubectl apply` to `GuestReady=True` in 70 s:
`encrypt-pending` took 4.5 s and the unlock 0.33 s. After a reboot,
`encrypt-pending` was a no-op and the unlock took 0.35 s. The per-step
timings are in
[the vTPM/kcrypt design doc, §4b](../design/vtpm-kairos-k0s-encryption.md#4b-proof-an-immediate-template-clone-sealed-on-its-own-first-boot-adr-0088).
This answers the vSphere half of the design, not the Accept-when list:
`v4.4.0-rc1` is not a release, and Open question 1 is about `CloudImage`
builds, which vSphere does not use.

## Open questions

1. **Does AuroraBoot place a raw image's cloud-config into `COS_OEM`?**
   Decision 2 assumes that `cloudConfigRef` on a `cloudImage` build ends up
   as a file in the built disk's OEM partition, where `encrypt-pending`
   reads it. ADR-0051 already found that `cloudConfigRef` behaves differently
   across AuroraBoot build commands. This must be checked against a real raw
   build before acceptance; if it does not hold, the builder writes the layer
   into OEM itself.
2. **Can Cloud Hypervisor clone an `Immediate` image with a vTPM?** ADR-0065
   added vTPM for Deferred installs; whether its `os.raw` handling supports a
   shared base is unverified.
3. **Should the kernel command line route be supported?** Kairos also reads
   the policy from the command line, which matters for raw cloud images whose
   OEM cannot be pre-populated (#4556's phase 4). banlieue builds its own
   images, so OEM is always writable here; the question is only whether an
   `Asserted` source should be allowed to declare the command line route.
