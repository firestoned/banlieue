# 0042 — vSphere Instant Clone ("vmFork") is not a banlieue provisioning strategy

## Status

Accepted — 2026-09-11.

## Context

While validating the Trusted Boot/UKI boot-stall investigation (ADR-0041) via
live `govc` commands against the maintainer's real vCenter, the question came
up of whether this environment supports vSphere **Instant Clone** — the
productized descendant of VMware's "vmFork" research project — as a faster
alternative to the `CloneVM_Task`-based provisioning `banlieue-provider-vsphere`
already implements (ADR-0024 and everything built on top of it).

Investigation (live, via `govc`):

- The environment (`vCenter Server 8.0.3 build-25600417`) supports it.
  `govc vm.instantclone` wraps the `InstantClone_Task` API directly and is
  usable today.
- Instant Clone forks a **running, already-booted** source VM's live memory
  and disk state into a new VM — a fundamentally different lifecycle from
  `CloneVM_Task`, which cold-clones a powered-off template and boots the
  clone fresh. `ensure_vm`'s entire sequencing model (clone while
  `power_on: false` → attach vTPM per ADR-0039 → set boot options → power on
  → first-boot cloud-init) assumes the cold-clone lifecycle throughout.
- **Instant Clone does not support a source VM with a vTPM device attached.**
  ADR-0039 (vTPM support) and ADR-0040/ADR-0041 (deferred install, Trusted
  Boot/UKI) are the current, active investment for `VSphereMachine` — Kairos's
  `kcrypt` requires a vTPM to seal LUKS keys. Any Instant Clone support could
  therefore only ever apply to a non-TPM subset of the fleet, in parallel
  with — not in place of — the cold-clone path.
- **New IP/hostname per fork is not automatic.** `guestinfo.userdata`
  (ADR-0024's static-IP cloud-config mechanism) is a first-boot-only
  cloud-init datasource; a forked child resumes mid-runtime with the
  parent's network stack already configured in memory and does not re-read
  it. VMware's supported mechanism for this — the **Guest Customization
  Engine for Instant Clone** — is Linux-only and requires a *pre-freeze*
  script on the parent (flush/disable the primary NIC, drop unique identity
  before it's frozen as a fork source) and a *post-thaw* script on the child
  (read new `guestinfo.*`, reconfigure networking) baked into the golden
  image. This duplicates what cloud-init already does for cold clones, with
  no code reuse between the two paths.

Taken together, Instant Clone would not be an extension of the existing
`CloneVmRequest`/`ReconfigVM_Task` pipeline — it would be a second, parallel
VM-provisioning architecture inside the vSphere provider, with its own
image-preparation requirements (pre-freeze/post-thaw scripts, no vTPM), for a
benefit (near-instant boot via memory-sharing) that matters most for
stateless/ephemeral workloads. `VirtualMachine` today targets general-purpose,
persistent VMs, and the provider's active investment is Trusted Boot/vTPM —
which Instant Clone is incompatible with.

## Decision

**Do not implement Instant Clone support in `banlieue-provider-vsphere`.**
`VSphereMachine` provisioning remains exclusively the cold `CloneVM_Task`
path (ADR-0024 onward). No `CloneVmRequest` field, `VSphereClient` trait
method, or CRD field is added for Instant Clone.

This is not a rejection of the capability's usefulness in the abstract — it is
scoped out because it does not fit banlieue's current target workload
(persistent, optionally Trusted-Boot/vTPM-backed VMs) without building an
entirely separate provisioning subsystem for a use case
(stateless/ephemeral, non-TPM workers) banlieue does not yet serve.

## Consequences

- No code changes; this ADR exists purely to record the investigation and
  close the question so it is not re-researched from scratch later.
- If banlieue ever takes on a stateless/ephemeral-worker use case that
  explicitly does not need vTPM/Trusted Boot (e.g. a fast-scaling CI runner
  pool), this decision should be revisited — Instant Clone would need its own
  ADR at that point, covering: a parallel provisioning path distinct from
  `ensure_vm`'s cold-clone sequencing, a golden-image contract for
  pre-freeze/post-thaw guest customization scripts, and an explicit
  incompatibility with `VMClassSpec.tpm_enabled`.
- Does not affect `banlieue-provider-proxmox` or `banlieue-provider-libvirt`
  (neither exists yet); this decision is vSphere-specific since Instant Clone
  is a vSphere/ESXi feature with no equivalent assumed for other backends.
