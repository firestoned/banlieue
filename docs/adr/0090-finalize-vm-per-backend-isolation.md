<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0090 — `VirtualMachine` finalize cascade: isolate each backend kind

- **Status:** Accepted
- **Date:** 2026-10-06
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0026](0026-vspheremachine-deletion-lifecycle.md)
  (the original two-level finalizer cascade this amends);
  [ADR-0074](0074-banlieue-proxmox-rest-client.md),
  [ADR-0075](0075-proxmoxmachine-inframachine-contract.md) (Proxmox, the
  backend whose RBAC gap exposed this)

## Context

`VirtualMachine`'s deletion path (`finalize_vm` in
`crates/banlieue-controller/src/reconciler/virtualmachine.rs`) does not know,
at delete time, which backend a VM was ever scheduled to — `status` may be
unset (deleted mid-first-reconcile), the `Provider` may already be gone, or
the VM may have been migrated. ADR-0026 resolved this by checking **every**
infra kind (`VSphereMachine`, `LibvirtMachine`, `CloudHypervisorMachine`,
`ProxmoxMachine`) unconditionally and only dropping the `banlieue.io/
virtualmachine` finalizer once none of the four exist.

That check was written as four sequential `apis.<kind>.get_opt(name).await?`
calls. `?` means the **first** kind whose lookup errors aborts the whole
function — including kinds already resolved earlier in the sequence, and
before the "issue a delete" code further down ever runs for any of them.

This was found live: the deployed `ClusterRole` for `banlieue-controller`
predated the Proxmox provider and had no `proxmoxmachines` grant, so
`apis.proxmox.get_opt(name)` returned `403 Forbidden` instead of `404 Not
Found` on every `VirtualMachine` deletion — including ones backed by vSphere,
which have no `ProxmoxMachine` and never will. The `proxmox` check runs last
in the sequence, so `vsphere`'s `get_opt` had already succeeded by the time
the function aborted, but `destroy_vm` was never called: the "issue delete"
branch sits below all four lookups. The finalizer stayed on indefinitely,
`VSphereMachine` was never asked to delete, and the backend VM in vCenter
never got destroyed — stuck exactly where ADR-0026 says "deletion blocks
here," except blocked by a permission gap on an unrelated backend rather
than a real pending teardown.

The structural problem: a permission or connectivity fault scoped to **one**
backend blocks deletion for **every** `VirtualMachine`, regardless of which
backend it actually uses. A single RBAC gap or a Proxmox API outage halts
vSphere, libvirt, and cloud-hypervisor teardown cluster-wide.

## Decision

Resolve each backend's state independently, and never let one kind's lookup
or delete failure block another's.

1. Introduce `InfraState` (`Absent` / `Present` / `Terminating` / `Unknown`)
   and a pure `plan_finalize([(InfraKind, InfraState); 4]) -> FinalizePlan`
   function — same `reconciler.rs` / `*_plan.rs` split already used by
   `pool.rs`/`pool_plan.rs` and `scheduler.rs`, so the decision is
   unit-testable without a fake kube client.
2. `Unknown` (a lookup error other than 404) is **not** treated as `Absent`.
   `FinalizePlan.remove_finalizer` is `true` only when **all four** kinds
   resolve to `Absent` — an `Unknown` must never let the finalizer come off,
   since that is precisely the dangling-VM risk ADR-0026 exists to prevent.
3. `Unknown` is also **not** treated as `Present`: a kind this reconciler
   cannot currently see is not asked to delete anything, because there is
   nothing to safely act on. It simply contributes nothing to the plan.
4. Every kind whose state resolves to `Present` gets its delete issued,
   independent of whether any other kind's lookup or delete failed. A kind's
   own `delete_ignoring_404` error is caught, logged, and does not stop the
   remaining kinds' deletes from being attempted in the same pass.
5. The reconciler returns `requeue_on_error()` (the SDK's standard
   transient-failure backoff) whenever the finalizer did not come off this
   pass, whether that is because something is still `Terminating` or because
   something is `Unknown` — both recheck on the same short interval, and the
   log line at `warn` names which backend and which error, so the two cases
   are distinguishable in a `kubectl logs` grep without being distinguished
   in control flow.

## Consequences

- A permission or connectivity fault on one backend now degrades to "VMs
  that use the broken backend don't finalize," not "no VM of any backend
  finalizes." This is a strict improvement with no new failure mode: the
  previously-unreachable kind is exactly as stuck as before, and every other
  kind is now unblocked.
- `remove_finalizer` staying conservative on `Unknown` means a VM whose
  backend lookup is persistently broken (e.g. CRD never installed, RBAC
  permanently missing for a decommissioned backend) stays `Terminating`
  forever rather than silently leaking. That is the correct default for a
  destructive, irreversible operation — the alternative (treating `Unknown`
  as `Absent` to make progress) is exactly the bug this ADR fixes, moved to
  a different trigger.
- `plan_finalize` is now testable as a pure function over four `InfraState`
  values, covering: all-absent removes the finalizer; any-unknown does not;
  a mix of present/absent/unknown issues deletes only for the present ones
  and still withholds the finalizer.
