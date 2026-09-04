# 0040 — Deferred (per-clone) install for TPM-sealed Kairos encryption

## Status

Accepted — 2026-09-04. Amends [ADR-0039](0039-vsphere-vtpm-support.md) — its
claim that `VMImageSpec.cloudConfigs` alone was sufficient for encryption
(no `VMImage` schema change needed) is **wrong** and superseded here. Reuses
the deferred-install template shape ADR-0020 originally specified and
ADR-0021 preserved as the `autoManageInstall: false` escape hatch.

## Context

Live end-to-end testing of ADR-0039 (`VMClass.spec.tpmEnabled` attaching a
vTPM to the clone in `ensure_vm`, between `CloneVM_Task` and power-on) failed
with the freshly-cloned VM's Kairos install reporting:

```
Could not find TPM 2.0 device at /dev/tpmrm0
could not encrypt partitions error="... could not find TPM 2.0 device ..."
```

Deep research into `kairos-io/kcrypt`/`kairos-io/kairos` (agent
`ae455e6c0aa478ef7`, findings below) established why, and that this is not a
banlieue bug:

- `install.encrypted_partitions` is consumed **only** from a `PostInstall`
  hook wired into the install action itself
  (`agent/internal/agent/hooks/{encrypt,finish,hook}.go`,
  `agent/pkg/action/install.go:236`, `agent/pkg/uki/install.go:203` — the
  only two call sites in the `kairos-io/kairos` monorepo).
- The standalone `kcrypt encrypt --tpm LABEL` CLI no longer exists — `kcrypt`
  is archived (last push 2025-09-23) and folded into an internal SDK
  consumed by `kairos-agent`, whose current `kcrypt` subcommand tree is only
  `checknv | readnv | cleanupnv | unlock-all`. There is no command a
  cloud-config `stages:` hook could invoke to encrypt after the fact.
- The one boot-time (initrd/immucore) encryption path that exists ("RAM
  mode") only encrypts partitions it creates from scratch when they're
  entirely absent, and explicitly no-ops once `COS_OEM`/`COS_PERSISTENT`
  already exist — exactly the state any clone of a pre-installed disk is in.

**Kairos disk encryption is install-phase-only, with no supported mechanism
to trigger it later against an already-installed disk.** This collides
directly with banlieue's vSphere pipeline (ADR-0020/0021): the golden
template is installed **once**, then every production VM is a
`CloneVM_Task` copy of that already-installed disk — the clone never re-runs
Kairos's installer, so `encrypted_partitions` never has anything to act on
for a production VM, no matter when a vTPM is attached to it.

A second, independent finding from researching vSphere's own clone
semantics closes off the obvious workaround (encrypt the golden template
once, since it's the only thing that ever actually installs): vSphere's
**default** clone behavior duplicates a source VM's vTPM **and its
secrets** onto the clone (an advanced 8.0+ setting,
`vpxd.clone.tpmProvisionPolicy`, can change the default to "replace" —
banlieue does not set it). Encrypting the golden template would therefore
only remain unlockable on its clones if every clone shares the *identical*
TPM identity and encryption key as the template and every sibling clone —
the exact anti-pattern VMware's own docs warn against, and unacceptable
isolation for a fleet of VMs.

**The only combination that gives every VM a genuinely unique, install-time-
sealed key is: don't install the template at all — defer the install to
each clone's own first boot**, at which point ADR-0039's already-attached,
already-unique vTPM is present *during* that VM's own install. This is,
notably, exactly what ADR-0020 originally specified before ADR-0021 added
the install-and-generalize default: "`import_iso_template` creates an empty
EFI VM, attaches the built Kairos ISO as a CD-ROM (`startConnected: true`),
and immediately calls `MarkAsTemplate`... every future clone of that
template must boot from the still-attached ISO and run Kairos's unattended
installer itself." ADR-0021 kept this exact behavior alive as
`VMImageTemplate.autoManageInstall: false`, just reframed as an opt-out "for
a build that isn't Kairos-driven" rather than a deliberate per-VM-install
strategy.

Checking the actual `import_iso_template` implementation
(`crates/banlieue-provider-vsphere/src/client/vim.rs`) confirms the
mechanism already fully exists and needs no new vSphere-side code:

- The CD-ROM is built with `start_connected: true` **unconditionally**, in
  `build_template_config_spec`, common to both the `true` and `false`
  branches — not something added only for the auto-install path.
- `auto_manage_install: false` already skips straight from VM creation to
  `mark_as_template()` — no power-on, no wait, no boot-order reconfigure,
  and (crucially) **the CD-ROM is never stripped**, unlike the `true`
  branch's post-install cleanup.
- `clone_vm` builds a plain `VirtualMachineCloneSpec` with no
  `device_change` touching the CD-ROM — a full clone carries the source's
  virtual hardware over by default, so the clone inherits the
  still-attached, still-connected install ISO.
- `ensure_vm`'s existing ADR-0039 sequencing — `clone_vm` (always powered
  off) → `add_tpm_device` (if `spec.tpmEnabled`) → power-on — already lands
  the vTPM before the clone's first boot, which is now also its *install*
  boot. **No change needed here at all.**

So the vSphere-provider mechanics for "defer install to the clone" already
work today, untested, under the `autoManageInstall: false` name. What's
missing is: (1) the field's name and docs actively mislead someone into
thinking it's for non-Kairos builds, not encrypted ones; (2) there is no
guidance on the *different* cloud-config contract a deferred-install image
needs (ADR-0021's `poweroff: true` / `reboot: false` / identity-wipe
contract is for building a disposable template — actively wrong for a
production VM that must reboot into itself and keep running); (3) the
consequence of "provisioned" meaning "install just started" rather than
"VM is ready" is not documented anywhere the new use case would surface it.

## Decision

1. **Rename/clarify, don't rebuild.** `VMImageTemplate.autoManageInstall:
   Option<bool>` becomes `VMImageTemplate.installMode: InstallMode`, an enum
   with three variants, in `crates/banlieue-api/src/banlieue/vmimage.rs`:
   - `Immediate` (default) — today's `true`: install now, wait for
     poweroff, strip the CD-ROM, template is pre-installed. Unaffected by
     this ADR; unsuitable for `tpmEnabled: true` VMClasses (see Consequences).
   - `Deferred` — same vSphere mechanics as today's `false`, renamed and
     documented as the sanctioned path for `tpmEnabled: true` VMClasses:
     create VM, attach ISO (already `startConnected: true`), `MarkAsTemplate`
     immediately, no power-on. The install runs once per clone, at that
     clone's own first boot, with that clone's own already-attached vTPM
     present.
   - `Manual` — today's `false`'s other original meaning: a build that
     isn't Kairos-driven at all, or whose install/generalize is managed
     some other way. Identical vSphere behavior to `Deferred`; kept as a
     separate name because it's a different *intent* a reader should not
     confuse with "deliberately deferred for encryption."

   `Deferred` and `Manual` are mechanically identical on the vSphere-provider
   side today (both map to the existing "skip to `MarkAsTemplate`" branch in
   `import_iso_template`) — the split is for schema clarity and future
   room to diverge (e.g. `Deferred` could later gain its own boot-order
   reconfigure or validation that `Manual` shouldn't), not because
   `vim.rs` needs two branches right now. This is pre-1.0 with no external
   consumers, so renaming the field outright (not adding a parallel one) is
   the correct move — no deprecation path needed.

2. **No `VSphereMachineSpec` / `ensure_vm` change.** ADR-0039's clone → add
   vTPM → power-on sequencing already does the right thing for a
   `Deferred`-mode clone. Confirmed by reading the current implementation,
   not assumed.

3. **Document the cloud-config contract split.** A `Deferred`-mode image's
   baked-in cloud-config (`VMImageSpec.cloudConfigs`) MUST set
   `install.reboot: true` / `install.poweroff: false` (the *opposite* of
   ADR-0021's template-building contract) and must NOT carry an
   `after-install-chroot` identity-wipe stage — each clone installs fresh
   and generates its own machine-id/SSH host keys naturally, and the VM is
   meant to keep running as the production workload after install completes,
   not power itself off for templating. Add a new example
   (`examples/13-vmimage-kairos-deferred-install-tpm.yaml`) showing this
   contract alongside `tpmEnabled: true`, distinct from example 07's
   `Immediate`-mode contract.

4. **Document the status/timing consequence.** `VSphereMachineStatus`
   already reports `provisioned=true` the instant `CloneVM_Task` +
   power-on succeed, without tracking guest-OS boot completion (ADR-0034's
   already-accepted gap). For a `Deferred`-mode clone this now means
   "provisioned=true" fires the moment an 8-12 minute unattended install
   *starts*, not when the VM is actually usable — a materially bigger gap
   than ADR-0034 was written against. This ADR documents the consequence
   rather than solving it; tracking real guest-install/boot readiness is
   out of scope here and left as a future, separate ADR if it becomes a
   real operational problem.

5. **No validation coupling `tpmEnabled` to `installMode`.** A `VMClass`
   with `tpmEnabled: true` referencing an `Immediate`-mode `VMImage` is not
   rejected at admission — it will schedule and clone successfully, attach a
   real vTPM, and simply never encrypt anything (since the disk is already
   installed unencrypted), silently. Adding that cross-CRD validation
   (`VMClass` doesn't know which `VMImage` a `VirtualMachine` will pair it
   with; only the `VirtualMachine` does) is a real gap but a separate,
   non-trivial scheduler change — left as a documented known-gap, not solved
   here.

## Consequences

- **Confirmed working end-to-end, live, 2026-09-04.** A `VirtualMachine`
  using a `tpmEnabled: true` `VMClass` paired with a `Deferred`-mode
  `VMImage` was created against the real vCenter and validated over SSH on
  first boot (`uptime` ~1 minute): `/dev/tpm0`/`/dev/tpmrm0` present, `sda5`
  (`COS_PERSISTENT`) is `crypto_LUKS` and mounted read-write across every
  `/var/lib/*`/`/etc/*` bind target, and `dmsetup ls` shows the LUKS mapping
  open — the encrypted partition was created during that VM's own install
  and auto-unlocked via its own vTPM with zero manual intervention. This is
  the first live confirmation that ADR-0039 + ADR-0040 together produce a
  genuinely unique, install-time-sealed encryption key per VM, not just a
  unit-tested code path.
- Every `tpmEnabled: true` `VMClass` MUST be paired with a `Deferred`-mode
  `VMImage` carrying an install-not-template cloud-config, or encryption
  silently does not happen (see Decision #5). Operators must get this right
  by convention/docs; nothing currently enforces it.
- Per-VM provisioning time for `Deferred`-mode VMs jumps from seconds
  (cloning an already-installed disk) to the full unattended-install window
  (typically 8-12 minutes, per ADR-0021), every time, for every VM — not
  just once at template-build time. This is a real, ongoing cost of this
  design, not a one-time template-build cost.
- `installTimeoutSeconds` (`VMImageTemplate`) has no effect for `Deferred`/
  `Manual` images — the import Job never powers on or waits at build time.
  Already true today; this ADR just documents it against the new intended
  use case.
- `Immediate`-mode images remain fully supported and unaffected — this is
  purely an additive, opt-in path for the specific `tpmEnabled: true` case.
- Existing example `07-vmimage-kairos-url-source.yaml` and any other
  `VMImage` using the old `autoManageInstall` field name need a mechanical
  rename to `installMode: Immediate` (schema rename, not a behavior change).
