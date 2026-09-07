# 0039 — vSphere virtual TPM (vTPM) support for Kairos disk encryption

## Status

Accepted — 2026-09-04.

## Context

The maintainer's vCenter now has a KMS (Key Management Server) registered
under Configure → Key Providers (confirmed live: `govc kms.ls` shows a
default provider, backed by a healthy KMIP server). This unlocks attaching a
virtual TPM (vTPM) device to a VM, which Kairos's `kcrypt` uses to seal LUKS
keys for disk encryption without a remote unlock server — the TPM is local to
each VM, so encryption works standalone per-VM once the device is present at
install time.

Investigation (manual `govc` exploration against the real environment) found:

- `govc` (checked 0.52.0 and 0.56.0, and the upstream `USAGE.md` at
  `v0.56.0`) has **no subcommand for attaching a vTPM to a VM**. The only
  TPM-related commands are `host.tpm.info`/`host.tpm.report` (physical ESXi
  host attestation, unrelated) and a `-tpm` flag on `kms.add` (marks a
  *native* key provider as host-TPM-backed, also unrelated to a VM's vTPM
  device). `vm.create`, `vm.clone`, `vm.change`, and `library.deploy`'s OVF
  deploy-options file have no TPM knob either — vTPM is not an OVF concept,
  so there is no create/clone-time flag for it in principle, not just in
  govc.
- The underlying vSphere API call is a `ReconfigVM_Task` with a
  `VirtualDeviceConfigSpec{ Operation: add, Device: &types.VirtualTPM{} }` —
  the same call the vCenter UI and PowerCLI's `New-VTpm` make under the hood.
  govc simply never wrapped it in a typed subcommand the way it did
  `device.cdrom.add` / `device.usb.add` / etc.
- `banlieue-provider-vsphere`'s `ensure_vm` (`crates/banlieue-provider-vsphere/
  src/reconciler/vspheremachine.rs`) already clones with `power_on: false`
  (ADR-0024's clone spec) and only calls `set_power_state` afterward as a
  separate step when `desired_power_state != PoweredOff`. This is exactly the
  gap a vTPM attach needs — Kairos's `kcrypt` seals against the TPM during
  install, so the device must exist **before first boot**, but it does not
  need to be part of the same API call as the clone.

Separately, `VMClassSpec` already has a precedent for exactly this shape of
capability: `firmware: Firmware` is a class-level (non-overridable) hardware
decision that the scheduler uses to filter candidate Providers/failure
domains, and that flows verbatim into `VSphereMachineSpec.firmware` for the
provider controller to act on. `VirtualMachineSpec.hardware_override`
deliberately does *not* expose `firmware` as a per-VM delta — only
`cpus`/`memoryMiB`/disk sizes are overridable, because firmware (like TPM)
is a scheduling-relevant capability, not a per-instance sizing knob.

`VMImageSpec` was considered and rejected as a home for this: whether a vTPM
device exists is a property of the VM instance, orthogonal to which OS image
runs. What *does* belong at the image layer — whether the guest actually uses
the TPM (Kairos's `install.encrypted_partitions` cloud-config stanza) — is
already covered by the existing `VMImageSpec.cloud_configs` layered-Secret
mechanism (ADR-0037); no schema change is needed there.

## Decision

1. **`VMClassSpec.tpm_enabled: bool`** (`crates/banlieue-api/src/banlieue/
   vmclass.rs`), default `false`, sibling to `firmware` — not nested in
   `HardwareSpec`, matching `firmware`'s precedent of living at the class's
   top level as a capability/scheduling concern rather than a sizing knob.

2. **New well-known feature string, `FEATURE_VTPM = "vtpm"`**, documented
   next to `ProviderCapabilities` (`crates/banlieue-api/src/banlieue/
   provider.rs`). Reuses the existing generic `capabilities.features:
   Vec<String>` gate rather than adding a dedicated capabilities field —
   `features` exists precisely for this shape of boolean capability flag,
   and TPM (unlike `firmware`, which has three variants) is a plain yes/no.
   The scheduler rejects a Provider/failure domain whose
   `capabilities.features` does not contain `FEATURE_VTPM` when the
   candidate `VMClass.spec.tpm_enabled == true`.

3. **No per-VM override.** `VirtualMachineSpec.HardwareOverride` gains no
   `tpm_enabled` field, mirroring `firmware`'s exclusion from that struct for
   the same reason: this is a class-level shape decision, not a per-instance
   delta.

4. **`VSphereMachineSpec.tpm_enabled: bool`** (`crates/banlieue-api/src/
   infrastructure/vsphere_machine.rs`), sibling to `pub firmware: Firmware`,
   resolved from the VM's `VMClass` by `banlieue-controller` the same way
   `firmware` is today.

5. **New `VSphereClient` trait method**, `add_tpm_device(&self, vm_ref: &str)
   -> Result<()>`, implemented for both `VimClientImpl` (real
   `ReconfigVM_Task` + `VirtualDeviceConfigSpec`/`VirtualTPM` call) and
   `FakeClient` (records the device against the fake's `Inventory` fixture,
   for reconciler tests). Mirrors the existing pattern of one trait method
   per discrete vCenter mutation (`clone_vm`, `set_power_state`, etc.).

6. **`ensure_vm` sequencing**: insert the `add_tpm_device` call immediately
   after `clone_vm` completes and before the existing power-state branch —
   i.e. clone (already `power_on: false`) → add vTPM if
   `spec.tpm_enabled` → power on. This lands the device before Kairos's
   first boot, which is the hard requirement for `kcrypt` to seal against it
   during unattended install.

7. **`VSphereMachineStatus.tpm_attached: Option<bool>`**, set once
   `add_tpm_device` succeeds (or observed absent), mirroring the
   `observed_power_state` precedent (ADR-0034) of a status field the
   provider owns and that isn't part of the CAPI contract proper.
   `VirtualMachineStatus` gets no separate copy — a failed attach surfaces
   through the existing `InfrastructureReady`/`Ready` conditions, which is
   the same reasoning ADR-0034 used for not duplicating every diagnostic
   field onto the parent `VirtualMachine`.

8. **No `VMImage` schema change.** Encryption behavior (whether Kairos
   actually seals partitions to the TPM) is left entirely to a
   `VMImageSpec.cloud_configs` entry carrying Kairos's
   `install.encrypted_partitions` stanza — an example will be added under
   `examples/` once this ADR is implemented, but it requires no CRD change.

## Consequences

- A `VMClass` with `tpmEnabled: true` can only schedule onto a Provider
  whose failure domain(s) advertise `FEATURE_VTPM` — this must be set by
  hand on the relevant `Provider.spec.capabilities.features` once the
  operator confirms KMS + vTPM actually work end-to-end in that vCenter (no
  auto-discovery of KMS presence; matches the "explicit over implicit"
  non-negotiable).
- `create-kairos-template.sh` / `create-vm.sh` (in the separate `vm-build`
  repo) are unaffected — the vTPM device is attached by
  `banlieue-provider-vsphere` at clone time, not baked into the golden
  template. (vSphere generates a fresh vTPM device/key per clone the same
  way it regenerates MAC addresses, so a template does not need to carry
  one itself.)
- One extra `ReconfigVM_Task` round-trip per VM creation when
  `tpm_enabled == true`; zero overhead otherwise (skipped entirely when
  `false`, matching the existing `desired_power_state == PoweredOff`
  early-skip pattern already in `ensure_vm`).
- Does not address full-VM encryption (encrypting every disk / VM home via
  a storage policy) — only the vTPM device needed for `kcrypt`'s local
  key-sealing use case. Full-VM encryption, if ever needed, is a separate,
  larger decision (storage-policy resolution, at minimum) and explicitly
  out of scope here.
- Does not change `banlieue-provider-proxmox` or `banlieue-provider-libvirt`
  (neither exists yet) — `FEATURE_VTPM` is a generic capability string any
  future provider can advertise, but only the vSphere provider implements
  the attach mechanics in this ADR.
