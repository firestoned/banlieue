# 0026 — VSphereMachine deletion lifecycle: finalizer + Destroy_Task

## Status

Proposed — 2026-08-22. Amends [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md),
whose own *Follow-ups* section named this explicitly: "`VSphereMachine`
reconciler's remaining lifecycle: status mirroring, power-state
reconciliation, update semantics, **deletion** — separate implementation
work under this same reconciler, likely its own ADR amendment once the
create path (this ADR) is proven." The create path is proven (live-tested,
several rounds of bugfixing); this ADR covers deletion.

## Context

`banlieue-controller`'s `VirtualMachine` reconciler already implements its
half of a cascade-delete contract
(`crates/banlieue-controller/src/reconciler/virtualmachine.rs::finalize_vm`):
on delete, it requests deletion of the owned `VSphereMachine` and only drops
its own `banlieue.io/virtualmachine` finalizer once that infra CR is
confirmed gone. Its doc comment states the intended guarantee plainly: *"we
never leave the backend with a dangling VM: deletion of the parent
VirtualMachine blocks at the K8s API until the provider has confirmed the
backend resource is gone."*

That guarantee depends entirely on the **provider** holding `VSphereMachine`
open with its own finalizer until the vSphere VM is actually destroyed.
`crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs::reconcile`
has no finalizer and no `deletion_timestamp` check at all — confirmed by
reading the function start to end. The result, found live: deleting a
`VirtualMachine` CR removes it and its `VSphereMachine` from Kubernetes
immediately (nothing blocks the delete), but the underlying vSphere VM
(created by `CloneVM_Task`, ADR-0024) is never touched and keeps running,
orphaned, with nothing left in the cluster pointing at it.

This also means banlieue does not yet satisfy the CAPI v1beta2 InfraMachine
contract's deletion requirement (Non-Negotiable #2) — an InfraMachine
implementation is expected to block its own deletion on successfully
tearing down the backend resource, exactly the guarantee the controller side
already assumes exists.

## Decision

### 1. `VSphereMachine` gets its own finalizer

Add `banlieue.io/vspheremachine` (mirroring the parent's
`banlieue.io/virtualmachine`, ADR pattern already used there and reusing the
same `banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer}`
helpers already depended on by `banlieue-provider-sdk`).

`reconcile` gains a `deletion_timestamp` branch, checked first (mirroring
the parent controller's own `finalize_vm` structure):

- **Set** → finalize path: resolve the `Provider`/client exactly as the
  create path does, then attempt `destroy_vm(vm_ref)` (new
  `VSphereClient` trait method, §2). On success (including "already gone" —
  idempotent), remove the finalizer. On failure, requeue with backoff and
  leave the finalizer in place — the parent's cascade-wait means the whole
  delete blocks here, which is the correct, conservative behavior for a
  destructive, irreversible operation.
- **Unset** → existing create-path logic, unchanged, except it now also
  calls `ensure_finalizer` before doing anything else (matching the parent
  controller's own ordering: finalizer first, then the real work).

A `VSphereMachine` with no `status.vmRef` yet (create never got far enough
to clone a VM) finalizes as a no-op success — nothing to destroy.

### 2. `VSphereClient::destroy_vm(vm_moref: &str) -> Result<()>`

A new trait method, moref-based (unlike the existing `destroy_if_present`,
which is name+folder based and belongs to the *template* import path,
ADR-0020/0021). `VSphereMachine.status.vmRef` already holds the exact moref
`clone_vm` returned, so no lookup-by-name is needed or wanted — a name-based
lookup would reintroduce exactly the cross-zone same-display-name collision
risk already fixed twice this project (template lookup, VM lookup).

Behavior, mirroring `destroy_if_present`'s existing power-off-then-destroy
sequence: resolve the moref to a `VirtualMachine` managed object; if
`RuntimeInfo` reports it already gone (`ManagedObjectNotFound` from the
first read) treat as success; else power off if not already
`poweredOff` (`Destroy_Task` rejects a running VM, same fault
`destroy_if_present` already works around), then `Destroy_Task`, then wait.

### 3. Not covered by this ADR

- **Already-orphaned VMs** created before this fix ships have no
  `VSphereMachine`/`VirtualMachine` left pointing at them (the CRs are
  already gone) — this ADR cannot retroactively find or clean those up.
  They need one-time manual identification and cleanup directly in vCenter.
- **Graceful in-guest shutdown** before power-off — `destroy_if_present`
  already hard-powers-off, and this ADR reuses that behavior rather than
  inventing a new one. Worth revisiting once a real guest-shutdown use case
  exists.

## Consequences

- Deleting a `VirtualMachine` now actually blocks (with visible `Terminating`
  status and requeue-driven retries) until the backend VM is destroyed —
  observable, not silent, and consistent with the parent controller's
  existing doc comment/intent, which this ADR finally makes true end to end.
- `VSphereMachine` becomes CAPI v1beta2 InfraMachine deletion-contract
  compliant.
- Destroying a VM is irreversible; a stuck finalizer (e.g. vCenter
  unreachable) blocks the whole delete chain by design — this is the safe
  failure mode for a destructive operation, not a bug to work around with a
  force-remove path.
