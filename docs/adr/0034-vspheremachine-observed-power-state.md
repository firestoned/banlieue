# 0034 — VSphereMachine/VirtualMachine mirror observed VM power state

## Status

Accepted — 2026-08-24. Amends [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)'s
create-path-only scoping decision. Extends `banlieue.io/v1alpha1`
`VirtualMachine` (`crates/banlieue-api/src/banlieue/virtualmachine.rs`) and
`infrastructure.banlieue.io/v1alpha1` `VSphereMachine`
(`crates/banlieue-api/src/infrastructure/vsphere_machine.rs`).

## Context

`VirtualMachine` already ships a `Power` printcolumn
(`.status.observedPowerState`) and a matching
`VirtualMachineStatus.observed_power_state: Option<PowerState>` field — but
nothing ever writes to it. Found live: two freshly created `VirtualMachine`s,
confirmed up and running in vCenter, both showed an empty `Power` column.

The underlying `VSphereMachine` infra CR has no equivalent field at all, and
its reconciler (ADR-0024) is deliberately create-path-only: once
`status.initialization.provisioned == true`, `reconcile()` returns
immediately without any further vCenter round-trip —

> "Create-path-only scope (ADR-0024): once a VM exists, this reconciler does
> nothing further — no vCenter round-trip needed to find that out."

That scoping was correct for its original purpose (avoid re-resolving
template/datastore/network on every poll once a VM is cloned and powered on),
but it also means the actual, current power state of the backend VM is never
observed again after the initial `set_power_state` call — a VM manually
powered off in vCenter, or one that failed to power on, looks identical to a
healthy running one from banlieue's point of view.

`VSphereClient` already reads `runtime.power_state` internally (twice: inside
`power_off_and_destroy`'s pre-destroy check, and implicitly via
`set_power_state`'s own task), but has no read-only accessor a reconciler can
call without also trying to *change* the power state.

## Decision

1. **New read-only trait method.** Add
   `VSphereClient::power_state(&self, vm_moref: &str) -> Result<PowerState>`,
   reading `VirtualMachine.runtime.power_state` and mapping it onto
   `banlieue_api::common::PowerState` (`PoweredOn` / `PoweredOff` /
   `Suspended`) — the same three-way mapping `set_power_state` already
   uses in the other direction. Implemented for both `VimClientImpl` and
   `FakeClient` (reading the fake's `Inventory` fixture).

2. **New status field.** Add `VSphereMachineStatus.observed_power_state:
   Option<PowerState>`, mirroring `VirtualMachineStatus`'s own field of the
   same name and the same optionality (absent until first observed).

3. **Narrow the create-path-only early return, not remove it.** Once
   provisioned, `reconcile()` still skips every other step `ensure_vm` does
   (datacenter/cluster/template/datastore/network resolution, clone) — those
   remain genuinely unnecessary after the VM exists, and ADR-0024's
   reasoning for skipping *them* stands. It now additionally does exactly
   one cheap read (`power_state`) before returning, patches
   `observed_power_state` if it changed, and requeues at the normal long
   interval. This is a narrow amendment to ADR-0024, not a reversal of it —
   the "no round-trip" rule now applies to everything except this one
   read.

4. **Status mirror.** `InfraMachineRead` (banlieue-controller's
   `status_mirror.rs`) gains an `observed_power_state()` accessor;
   `mirror_status_from_infra` copies it onto the parent `VirtualMachine`'s
   own `status.observed_power_state`, which the existing `Power`
   printcolumn already renders — no CRD/printcolumn change needed on the
   `VirtualMachine` side, only on `VSphereMachine`.

5. **More info logging across `ensure_vm`.** Bundled with this change (the
   original ask that led here): add `info!` logs at each existing step in
   `ensure_vm` — datacenter/cluster/template/datastore/network resolved,
   clone submitted, clone complete (`vm_ref`), power-on requested/observed
   — closing the gap where the entire clone-to-power-on sequence produced
   zero log lines between "reconciling" and "provisioned".

6. **Every status patch applies the full object, never a hand-picked
   subset.** Found live, twice, immediately after (1)-(5) shipped: both
   `refresh_power_state`'s narrow `{observedPowerState, observedGeneration}`
   patch and `banlieue-controller`'s pre-existing `patch_status`/
   `patch_status_conditions_only` (which had *never* forwarded
   `initialization`/`addresses`, and then also never forwarded
   `observedPowerState` once this ADR added it) hit the same failure mode:
   under server-side-apply, the same field manager re-applying a *narrower*
   field set than a previous apply makes the apiserver retract — and then
   wipe — every field the narrower payload omits, since nothing else owns
   them. Once schema/timing lined up for `refresh_power_state`'s narrow
   patch to actually succeed, it silently erased `status.vmRef` on a live
   `VSphereMachine`; `finalize()` then read `vm_ref` as `None` and skipped
   `destroy_vm`, orphaning the backend VM in vCenter on delete. Fixed by
   making every status-patching function in both reconcilers build and
   send the complete `VSphereMachineStatus`/`VirtualMachineStatus` (cloned
   from current, only the relevant fields overridden) rather than a
   constructed subset.

7. **Self-heal by detect-and-report only, never by rediscovery or
   recreation.** If `status.initialization.provisioned == true` but
   `status.vmRef` is unset (the exact inconsistency (6) could produce, or
   any future one like it), or if the stored `vmRef` no longer resolves in
   vCenter (`ManagedObjectNotFound`), `refresh_power_state` now reports it
   clearly — `Ready=False` with reason `BackendRefMissing` or
   `BackendMissing` and a descriptive message — instead of either silently
   doing nothing (the prior behavior for a missing `vmRef`) or retrying
   forever on an opaque error (the prior behavior for a missing backend
   VM). It deliberately does **not** attempt to rediscover the VM by name
   (that needs the same datacenter/folder resolution round-trip ADR-0024
   exists to avoid, and risks adopting the wrong VM if a same-named one
   exists elsewhere) or recreate it (a human should decide whether a
   missing VM was deleted on purpose). `Ready` is restored to
   `True`/`Reconciled` automatically once a `power_state` read succeeds
   again, so a transient blip doesn't stay reported as broken forever.

## Consequences

- `VSphereMachine`/`VirtualMachine` now reflect a VM manually powered off or
  suspended out-of-band in vCenter, on the normal long poll interval — not
  instantly (this is not event-driven; a vCenter-side power change isn't
  something banlieue can subscribe to without a much larger vCenter
  event-listener investment, out of scope here).
- Still does **not** track guest-OS boot completion (VMware Tools running
  status, guest IP, etc.) — `power_state` reports the *hypervisor's* view
  (poweredOn/poweredOff/suspended), which is available immediately on
  power-on, before the guest OS has finished booting. Tracking actual guest
  boot is a separate, larger follow-up (would need to poll guest-info/Tools
  status and decide what "Ready" means while waiting on it) and is
  explicitly not addressed by this ADR.
- One extra `VirtualMachine.runtime` read per `VSphereMachine` reconcile,
  even once steady-state — bounded, cheap, and only on the existing
  `requeue_long` cadence (no new polling frequency introduced).
