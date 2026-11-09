<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0124: A Provider labelled unschedulable takes no new VM

- **Status:** Accepted
- **Date:** 2026-11-08
- **Proposed:** 2026-11-08
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0102](0102-provider-liveness-from-its-lease.md) (the liveness filter
  this sits beside); faubourg ADR-0003 decision 5 and its accepted risk R-4 (cordon is
  advisory until banlieue honours it). ADR-0116 to ADR-0122 are reserved by roadmap 20.

## Context

An operator taking a host out for maintenance wants banlieue to stop placing new VMs on it,
without stopping the VMs already there. Kubernetes calls this cordoning a Node. banlieue has no
such state: `Provider.spec.paused` suspends *reconciliation* (Cluster API's meaning), which
would also stop the provider looking after the VMs it already runs, and the scheduler's only
Provider-level filter besides the selector is liveness (ADR-0102).

faubourg, which groups hosts into availability zones and bootstraps their Cloud Hypervisor
providers, already marks a cordoned host's `Provider` with the label
`faubourg.firestoned.io/cordoned: "true"`. A `VirtualMachine` can exclude it today with
`matchExpressions: [{key: faubourg.firestoned.io/cordoned, operator: DoesNotExist}]` in its
`providerSelector`, which the scheduler honours. But that is opt-in per VM: one that leaves the
expression out still lands on the cordoned host, so faubourg records the cordon as advisory
(its R-4). And a label named after faubourg is the wrong thing for banlieue's scheduler to know
about: any operator, with or without faubourg, needs the same switch.

## Decision

1. **A well-known label, `banlieue.io/unschedulable: "true"`.** On a `Provider`, it means:
   place no new VM here. Any other value, or no label, means schedulable. Whoever may label
   Providers may set it: an operator with `kubectl label`, or a controller such as faubourg.
2. **The scheduler drops such Providers right after the selector**, before the liveness filter
   (ADR-0102), in the pure placement function. If the selector matched Providers and every one
   is unschedulable, the VM is not scheduled, with the new reason `ProviderUnschedulable` on
   its `Scheduled` condition, and is retried like any unscheduled VM.
3. **It only affects new placements.** A VM already placed on the Provider keeps its placement
   and keeps running; banlieue moves nothing and stops nothing. This needs care, because the
   reconciler re-runs the scheduler on every pass and answers drift by the VM's
   `migrationPolicy`, whose default recreates it elsewhere (losing a local disk). So a VM's own
   Provider, the one in its `status.scheduled`, stays a candidate through this filter, and
   through the liveness filter of ADR-0102, which had the same gap: its decision cannot change
   because of either. A pool replacing a member goes through the scheduler as a new VM, so it
   avoids the Provider.
4. **No per-VM override in v1.** A cordon that a VM can opt out of is advisory again. If
   maintenance VMs need one, a later ADR adds an explicit toleration.
5. **It is documented beside `providerSelector`** as the way to cordon a host, and faubourg's
   cordon sets it (a faubourg change once this ships).

## Alternatives considered

- **A spec field, `Provider.spec.unschedulable`,** as on a Node. Typed, but a Provider's spec
  is its owner's: faubourg, for one, never writes an adopted Provider's spec, only its labels.
  A label lets every party that can already label a Provider cordon it, and is what the
  selector machinery already reads.
- **Honour `faubourg.firestoned.io/cordoned`.** Couples banlieue's scheduler to one consumer's
  naming. faubourg can set banlieue's label instead.
- **A taint and toleration model.** More general, and more than the one question asked: may
  new VMs land here. Decision 4 leaves room for it.
- **`spec.paused`.** Stops reconciliation of the Provider's existing machines too.

## Consequences

- The scheduler gains one filter, one error variant and one reason, `ProviderUnschedulable`,
  unit-tested like the liveness filter (all unschedulable, some, none, combined with stale).
- Cordon becomes enforced for every VM, not only those whose selector excludes it. faubourg's
  R-4 closes once faubourg sets the label.
- Anyone who can label Providers can stop placement on them. That is no new power: they can
  already change the labels every selector matches on.
