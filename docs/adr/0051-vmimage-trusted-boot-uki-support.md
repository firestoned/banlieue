# 0051 — `VMImage` Trusted Boot (UKI) support

- **Status:** Accepted
- **Date:** 2026-09-19
- **Proposed:** 2026-09-10
- **Amended:** 2026-09-10 (Decision #3, preflight key
  validation; Decision #4, route `trustedBoot` cloud-config through
  `overlayISOVolume` instead of `cloudConfigRef`, working around an upstream
  kairos-operator bug).
- **Related:** Extends [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md)
  (the `banlieue-imagebuilder` build pipeline), [ADR-0020](0020-vsphere-per-zone-iso-import.md)
  (vSphere ISO import) and [ADR-0022](0022-vsphere-iso-overlay-files.md) (the
  `OSArtifact` wiring pattern this ADR mirrors). Complements
  [ADR-0039](0039-vsphere-vtpm-support.md) / [ADR-0040](0040-deferred-install-for-vtpm-encryption.md)
  (vTPM device attachment + deferred install for `kcrypt`) — this ADR is the
  image-build-time half of the same end-to-end feature; ADR-0039/0040 are the
  VM-instance-time half.

## Context

The separate `vm-build` repo (which produces the base rootfs OCI image
`banlieue-imagebuilder`'s `OSArtifact` requests build from) changed its
Dockerfiles' `TRUSTED_BOOT` build-arg default from `false` to `true`. With
Trusted Boot on, `kairos-init` produces a Unified Kernel Image (UKI — kernel +
initrd + cmdline bundled into a single PE binary) instead of discrete
`/boot/vmlinuz*` + `/boot/initrd*`/`initramfs*` files.

`auroraboot build-iso` — the classic, non-UKI ISO assembly path kairos-operator
invokes today whenever `OSArtifact.spec.artifacts.iso == true` — scans the
rootfs for a file matching prefix `initrd`/`initramfs` to stage onto the ISO.
With a UKI rootfs that file no longer exists, so every vSphere `VMImage`
build now fails:

```
ERR [1] No initrd file found
ERR [1] Could not find kernel and/or initrd
ERR [1] Failed preparing ISO's root tree: No file found with prefixes: [initrd initramfs]
```

Nothing in `banlieue` caused this — `desired_os_artifact`
(`crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`) only ever requests
`spec.artifacts.iso: true`, never anything UKI-aware. The maintainer wants to
move forward with Trusted Boot (not revert `vm-build`), which requires
banlieue to request the correct artifact shape from kairos-operator.

**What kairos-operator's `OSArtifact` (`build.kairos.io/v1alpha2`) already
supports**, confirmed against its Go types
(`api/v1alpha2/osartifact_types.go`):

```go
type ArtifactSpec struct {
    ISO         bool     `json:"iso,omitempty"`
    CloudImage  bool     `json:"cloudImage,omitempty"`
    UKI         *UKISpec `json:"uki,omitempty"`
    // ... arch, cloudConfigRef, overlayISOVolume, etc.
}

type UKISpec struct {
    ISO        bool   `json:"iso,omitempty"`
    Container  bool   `json:"container,omitempty"`
    EFI        bool   `json:"efi,omitempty"`
    KeysVolume string `json:"keysVolume"` // required whenever iso||container||efi
}
```

`keysVolume` names an entry in `spec.volumes[]` (a plain `[]corev1.Volume`,
exactly like `overlayISOVolume`'s precedent) that must contain six specific
files: `PK.auth`, `KEK.auth`, `db.auth`, `db.key`, `db.pem`,
`tpm2-pcr-private.pem`. There is no way to build a `uki.iso` artifact without
supplying these — `auroraboot build-uki` needs at least a throwaway key set to
construct/sign the UKI's PE structure. Kairos ships `auroraboot genkey` to
generate exactly such a throwaway, self-signed set in one command; this is not
an enterprise PKI/HSM requirement, just six files an operator generates once
and stores as a Secret. UEFI firmware then trusts that self-signed set because
Kairos auto-enrolls it as the VM's `PK`/`KEK`/`db` on first boot when the VM's
firmware starts in UEFI Setup Mode (or the operator enrolls it once manually)
— the operator is both the CA and the enroller, deliberately, so there is no
external trust anchor to manage.

This is architecturally significant (new CRD field + a behavior change to
which kairos-operator artifact kind gets requested), so per
`rules/architecture-driven-development.md` it gets an ADR before the CALM
update and TDD implementation.

## Decision

### 1. New `VMImageSpec.trustedBoot: Option<TrustedBootSource>`

```rust
/// Requests a Trusted Boot (UKI) artifact instead of a classic
/// kernel+initrd one for `Url`-kind vSphere sources. Maps to
/// kairos-operator's `OSArtifact.spec.artifacts.uki` (ADR-0051). Only
/// meaningful for the `iso` artifact kind (vSphere); ignored for
/// `cloudImage`-kind builds and non-`Url` sources, mirroring
/// `isoOverlay`'s precedent (ADR-0022).
pub struct TrustedBootSource {
    /// Secret in the imagebuild namespace holding the six files
    /// `auroraboot build-uki` requires: `PK.auth`, `KEK.auth`, `db.auth`,
    /// `db.key`, `db.pem`, `tpm2-pcr-private.pem`. Generated out-of-band via
    /// `auroraboot genkey` — banlieue never generates or manages this key
    /// material, only references the Secret's name. Only the name is read
    /// by `banlieue-imagebuilder`; its content is mounted directly into the
    /// `OSArtifact` build pod by kairos-operator, never by banlieue.
    pub secret_ref: LocalObjectReference,
}
```

One field, no per-file key mapping (unlike `IsoOverlaySource.files`): the six
key filenames are fixed by kairos-operator's contract, not caller-declared,
so there is nothing to map — the Secret's keys must simply be named exactly
those six strings.

### 2. `desired_os_artifact` wiring

`crates/banlieue-imagebuilder/src/reconciler/vmimage.rs` gains a
`trusted_boot: Option<&TrustedBootSource>` parameter. When set (and only
meaningful when `kind == BuildArtifactKind::Iso`):

```json
{
  "spec": {
    "volumes": [
      {
        "name": "trusted-boot-keys",
        "secret": { "secretName": "<trustedBoot.secretRef.name>" }
      }
    ],
    "artifacts": {
      "uki": { "iso": true, "keysVolume": "trusted-boot-keys" },
      "arch": "amd64"
    }
  }
}
```

— **replacing**, not adding to, the plain `artifacts.iso: true` this same
build would otherwise request. `artifacts_flag`/the existing
`artifacts.insert(artifacts_flag(kind), true)` call is skipped whenever
`trusted_boot` is set; `BuildArtifactKind::Iso` still describes the *shape* of
output banlieue's status/provider-import code cares about (a bootable ISO),
it just now sometimes comes from `uki.iso` instead of the plain `iso` flag.
No `BuildArtifactKind` enum change — the distinction is purely in which
`OSArtifact.spec.artifacts` sub-field gets set, an implementation detail
`VMImageStatus.buildArtifact.kind` does not need to expose.

Unlike `IsoOverlaySource`'s Decision #3 workaround (ADR-0022), the Secret
volume is referenced **directly** as `keysVolume` — no `iso-overlay-materialize`-style
dereferencing init container. That workaround exists because
`auroraboot build-iso --overlay-iso` *merges* the overlay tree onto an
already-populated `/boot`, where kubelet's Secret-mount symlink layout
collides with real directories. `auroraboot build-uki`'s `--keysVolume`
consumption reads individual named key files directly; there is no
directory-merge step where that symlink layout could collide the same way.
If live-testing surfaces an analogous failure, it gets its own follow-up ADR
amendment, per ADR-0022's own precedent.

### 3. Preflight validation of the `trustedBoot` Secret (added 2026-09-10)

Without a `keysVolume` referencing the right six files, the failure surfaces
only as an opaque kairos-operator admission rejection or a failed build pod —
neither points back at "your Secret is missing `db.key`." Since
`banlieue-imagebuilder` does not generate this key material itself (see
Consequences below), a wrong/incomplete Secret is expected to happen in
practice, not just a hypothetical.

**Decision:** before `reconcile` touches any `OSArtifact`, when
`spec.trustedBoot` is set, fetch the referenced Secret and check that its
`data` map contains all six required key **names**:
`PK.auth`, `KEK.auth`, `db.auth`, `db.key`, `db.pem`, `tpm2-pcr-private.pem`.
Only the *names* are read — `missing_trusted_boot_keys`
(`crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`) takes a
`BTreeSet<String>` of key names and never sees a value, preserving the
"never touches Secret content" posture from Decision #1 (this crate already
had `Secret` read RBAC and *does* read cloud-config Secret values elsewhere,
via `merge_and_apply_cloud_configs` (ADR-0037) — checking key names here is
strictly less invasive than that existing precedent, not a new capability).

A missing or incomplete Secret publishes `VMImage.status.buildArtifact` with
`phase: Failed`, `reason: TrustedBootKeysMissing`, and a `message` naming
the Secret and exactly which keys are absent — then returns early
(`requeue_default`) without creating, deleting, or patching any
`OSArtifact` that pass. This mirrors the existing "stale or foreign
OSArtifact" early-return shape already in `reconcile`, just gated earlier,
before any `OSArtifact` exists to be stale.

### 4. Route `trustedBoot` cloud-config through `overlayISOVolume`, not `cloudConfigRef` (added 2026-09-10)

Live-testing this ADR against the real vCenter pipeline surfaced a second
upstream gap, beyond the initrd/UKI issue this ADR started from. A `VMImage`
with both `cloudConfigs` (ADR-0037) and `trustedBoot` set produces an
`OSArtifact` requesting both `cloudConfigRef` and `uki` — and kairos-operator
unconditionally translates `cloudConfigRef` into `auroraboot ... --cloud-config
/cloud-config.yaml` regardless of artifact kind. `auroraboot build-uki` has no
`--cloud-config` flag at all (confirmed via its own `--help`), so the build
pod fails immediately on flag parsing:

```
Incorrect Usage: flag provided but not defined: -cloud-config
```

Root-caused in `kairos-operator/internal/controller/job.go`'s
`buildUKICommand`: the code already has a comment correctly noting
`build-uki` doesn't support `--arch` and omits it — the same care wasn't
applied two lines later for `--cloud-config`. Filed upstream as
[kairos-io/kairos#4586](https://github.com/kairos-io/kairos/issues/4586)
(full report also kept at
`~/dev/issues/kairos-operator-uki-cloud-config-flag.md`); no fix exists yet.

**Decision:** never set `artifacts.cloudConfigRef` when `trustedBoot` is set.
Instead, reuse the *existing* `isoOverlay`/`overlayISOVolume` mechanism
(Decision #2 above, ADR-0022) to place the merged cloud-config content at the
ISO root as `config.yaml` — confirmed against AuroraBoot's own source
(`deployer.cloudConfigPath()` → `<dest>/config.yaml`, copied into the ISO's
root overlay) to be the *exact* file `--cloud-config` itself writes
internally, which kairos-agent's installer already scans for via
`/run/initramfs/live` (the ISO's own live-boot mount) among its
`GetUserConfigDirs()` search paths. `--overlay-iso` is supported by both
`build-iso` and `build-uki`, so this reproduces `--cloud-config`'s effect on
either without any upstream change.

Concretely: a second Secret-backed `spec.volumes[]` entry
(`trusted-boot-cloud-config-source`, item `path: config.yaml`) and a second
materializing importer (`trusted-boot-cloud-config-materialize`, reusing the
same dereference-symlinks script as `isoOverlay`'s own importer —
kairos-io/kairos#4324 applies here too, since this is still a raw
Secret-mount being merged onto the ISO root) both target the *same* shared
`ISO_OVERLAY_VOLUME_NAME` emptyDir `isoOverlay`'s importer already
materializes into — `overlayISOVolume` only ever names one volume, so the
emptyDir and the `overlayISOVolume` artifact key are added at most once,
after every mechanism that wants to write into it has had a chance to ask.
The two importers write non-colliding paths (`boot/grub2/grub.cfg` for a
user's declared `isoOverlay`, `config.yaml` for the injected cloud-config),
so running both against the same emptyDir is safe regardless of order.

This intentionally keeps `VMImageSpec.cloud_configs` as the single
user-facing API for cloud-config, unchanged — `trustedBoot` only changes
*how* `banlieue-imagebuilder` delivers it to kairos-operator, entirely inside
`desired_os_artifact`. No CRD change, no new field.

### 5. `cloudConfigs`-declared `install.encrypted_partitions`, unchanged

Whether the guest OS actually seals a partition to the TPM remains entirely a
`VMImageSpec.cloud_configs` concern (ADR-0037), exactly as ADR-0039 Decision 8
already established — no change needed here. This ADR only makes the
*artifact itself* buildable when the base image is UKI-shaped; ADR-0039/0040
remain what attaches the vTPM device and sequences install/power-on so
`kcrypt` has something to seal against.

## Consequences

- **Unblocks the `vm-build` TRUSTED_BOOT=true regression** without reverting
  `vm-build` — banlieue now requests the artifact shape that matches what
  `vm-build` actually produces.
- **New required operational step, out of band:** an operator must run
  `auroraboot genkey` once (per environment, or per `VMImage` if isolation is
  wanted) and store its output as a Secret before setting
  `spec.trustedBoot`. banlieue does not generate or rotate this key
  material — consistent with the "never touches Secret content" posture
  established for `cloudConfigRef`/`isoOverlay`. It does, per Decision #3,
  validate that the Secret carries the right key *names* before use.
- **One extra `Secret` `get` per reconcile pass when `trustedBoot` is set** —
  negligible next to the OCI build/ISO-generation work it gates, and no new
  RBAC (this crate already reads `Secret`s for cloud-config merging,
  ADR-0037).
- **No `BuildArtifactKind` schema change** — `VMImageStatus.buildArtifact.kind`
  stays `iso`/`cloudImage`; UKI vs. classic ISO is an internal
  `OSArtifact.spec.artifacts` request-shape detail.
- **Open risk, needs live-vCenter verification before this is end-to-end
  complete:** Kairos's documented auto-enrollment flow assumes the VM's UEFI
  firmware starts in **Setup Mode** (empty `PK`) on first boot. vSphere's own
  Secure Boot toggle (`VMImageTemplate.firmware = EfiSecure`,
  `govc vm.create -firmware`) is not confirmed to expose an equivalent empty/
  enrollable state — vSphere VMs may start with vendor-default keys already
  enrolled, in which case Kairos's self-signed `genkey` set would need manual
  enrollment (or a different mechanism) rather than automatic Setup Mode
  enrollment. This needs the same kind of live `govc`/vCenter investigation
  ADR-0039 did for the vTPM device gap, before declaring the full
  build-to-boot Trusted Boot path proven on vSphere. Tracked as a follow-up.
- **Does not change `banlieue-provider-proxmox` or `banlieue-provider-libvirt`**
  (neither exists yet) — `trustedBoot` is only wired for the `iso` artifact
  kind (vSphere) today; a `cloudImage`-kind UKI (`uki.container`/`uki.efi`)
  is out of scope until a concrete non-vSphere need arises.
- **Depends on an unfixed upstream kairos-operator bug staying a workaround,
  not a fix.** Decision #4's `overlayISOVolume`-based cloud-config injection
  is a consumer-side reproduction of `--cloud-config`'s effect, not the flag
  itself — if a future `auroraboot`/kairos-operator release changes where or
  how `--cloud-config` writes its file (or adds real UKI support for it),
  this workaround should be re-verified, not assumed to keep matching.
- **One additional `Secret`-materializing importer when `trustedBoot` +
  `cloudConfigs` are both set** — negligible cost, same shape as the existing
  `isoOverlay` importer (Decision #2/ADR-0022), and shares its emptyDir.

## Follow-ups

- ~~Live-verify vSphere UEFI Secure Boot key enrollment~~ — **done**, via live
  `govc`-driven testing against the maintainer's vCenter (2026-09-10/11,
  mirroring ADR-0039's approach). Mechanism: the VMX `extraConfig` keys
  `uefi.secureBoot.{pk,kek,db}Default.file0` (Broadcom KB 377306), pointing at
  DER-encoded certs uploaded to the VM's own datastore folder, plus
  `uefi.secureBoot.dbDefault.append=FALSE`. Only takes effect on a VM with no
  existing Secure Boot config in `.nvram`, so it must be set before first
  power-on. Documented in
  [Using banlieue-imagebuilder](../src/guides/using-banlieue-imagebuilder.md).
- **New finding, not yet root-caused:** the same live testing found the
  initial UEFI → OS handoff on vSphere to be extremely slow — multi-minute
  silent stalls at each Secure Boot stage transition (shim → systemd-boot →
  UKI). ESXi's `vmware.log` shows zero hypervisor-visible activity during the
  stalls (no vTPM command traffic, no disk I/O), which rules out vTPM
  emulation overhead and points to something inside guest space (kernel/
  systemd/dracut UKI-stub behavior specific to vSphere firmware). Reproduced
  on a Debian-based Trusted Boot image; a Hadron-based image also failed to
  boot cleanly on a follow-up attempt and was not further diagnosed (out of
  time). **`VMImage.spec.trustedBoot` on the vSphere provider should be
  treated as experimental** until this is root-caused. Next step when
  picked back up: capture the actual guest-side boot text via a file-backed
  serial console (`console=ttyS0` is already in Kairos's default UKI
  cmdline — `vsphere-add-tpm.py`/`create-vm.sh`'s workflow just needs a
  `govc device.serial.add`/`device.serial.connect` before first boot to
  capture it) instead of reasoning from a blank VGA console.
- Add a worked example under `examples/` showing `spec.trustedBoot` + a
  `VMClass.tpmEnabled: true` + an `install.encrypted_partitions` cloud-config
  together, end to end — blocked on the boot-stall finding above.
- If a genuine need for `uki.container`/`uki.efi` (non-ISO UKI outputs) shows
  up, extend `TrustedBootSource`/`desired_os_artifact` rather than modeling a
  new field — the same Secret and key material apply to all three sub-flags.
- Track the upstream fix for `kairos-operator`'s `buildUKICommand` —
  [kairos-io/kairos#4586](https://github.com/kairos-io/kairos/issues/4586)
  (filed 2026-09-10; local copy of the report at
  `~/dev/issues/kairos-operator-uki-cloud-config-flag.md`).
  If/when it lands, Decision #4's workaround can likely stay in place
  unconditionally (it's harmless even once `--cloud-config` also works) or be
  removed in favor of the fixed flag — revisit either way once a fix ships.
