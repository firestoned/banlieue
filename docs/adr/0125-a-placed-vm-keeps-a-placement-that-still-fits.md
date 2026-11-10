<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0125: A placed VM keeps its placement while it still fits

- **Status:** Accepted
- **Date:** 2026-11-10
- **Proposed:** 2026-11-09
- **Accepted:** 2026-11-10
- **Deciders:** Erick Bourgeois
- **Related:** Restores the intent of D-009 (`.github/community/01-decisions.md`, "if the
  current placement no longer satisfies the spec"); [ADR-0098](0098-spread-placement-across-failure-domains.md)
  (the spread ordering it narrows to new placements); [ADR-0124](0124-unschedulable-providers.md)
  and [ADR-0102](0102-provider-liveness-from-its-lease.md) (which found the same gap for two
  filters); [ADR-0036](0036-live-migration-phased-approach.md) (recreate is the only migration
  today).

## Context

D-009 made scheduling non-sticky: the scheduler runs on every reconcile, and "if the current
placement no longer satisfies the spec", the VM gets `PlacementValid=False` and its
`migrationPolicy` decides what happens. The default, `Automatic`, recreates the VM on the new
placement, since live migration is not built (ADR-0036).

The code does something stronger than D-009 says. `migration.rs` compares the recorded placement
with the scheduler's *best* placement now, not with whether the recorded one still fits. And the
spread ordering (ADR-0098) ranks every survivor by how many VMs of the group are on each failure
domain, where "a VM never counts against itself". So a placed VM sees its own domain with one
VM fewer than it really holds, and any emptier domain outranks it:

- a new, empty host joins: every VM on a busier host is a candidate to move to it;
- a VM is deleted from one host: its neighbours elsewhere can move into the gap.

Each move is drift, and under `Automatic` drift deletes the infrastructure machine and creates
a new one. On a provider with local disks (Cloud Hypervisor's storage classes, libvirt's
pools) that is a new, empty VM. Rebalancing by recreating is the last thing an operator adding
capacity expects.

ADR-0124 and the ADR-0102 amendment closed this gap for two filters (a cordoned or stale own
provider), not for the ranking.

## Decision

1. **Drift means the current placement no longer fits, not that a better one exists.** For a VM
   with a recorded placement (`status.scheduled`), the scheduler first checks that placement
   against the filters that describe the VM's spec, and returns it unchanged when it passes:
   - the provider still exists and matches `providerSelector` (a cordoned or stale own provider
     still counts, ADR-0124 and ADR-0102);
   - its failure domain still exists and matches `failureDomainSelector`;
   - it still offers the VM's storage classes, network classes and features, and the firmware
     and vTPM the class asks for;
   - required anti-affinity still holds, counting only its siblings: the VM never repels
     itself from its own domain when its labels match its own rule (until now it was counted,
     so such a VM always drifted off its domain);
   - capacity still fits, counting the other VMs there (the VM itself is already in it).
2. **Image readiness is not re-checked for a placed VM.** It decides where a new disk can be
   made; a placed VM already has its disk, and an image revision going not-ready on its host
   must not recreate it.
3. **Spread and the rendezvous hash order new placements only**, and a placed VM whose current
   placement fails decision 1. There, the scheduler places the VM as it would a new one, and
   `migrationPolicy` acts on the result as today.
4. **Rebalancing is not automatic.** Moving VMs to use new capacity is an explicit act: recreate
   or redeploy them, or a later ADR for a rebalancer that works within `migrationPolicy` and
   live migration.

## Alternatives considered

- **Make the default `migrationPolicy` `Manual`.** Stops the recreate, but every VM would sit
  at `PlacementValid=False` whenever the fleet is unbalanced, which is noise, and the scheduler
  would still be computing a placement it should not want.
- **Count the VM against its own domain in the ranking.** Removes the bias for an equal
  count, not the move when another domain is emptier by two or more.
- **Keep D-009's wording and the current code.** That is the bug: the code and D-009 disagree,
  and the code is the one that loses disks.

## Consequences

- A host joining or a VM leaving moves nothing. New VMs fill new capacity; old ones stay.
- Drift becomes rare and meaningful: a selector, class, capability or capacity change that the
  current placement no longer meets. `PlacementValid=False` then means something.
- The scheduler gains a "does the current placement still fit" path, unit-tested beside the
  spread tests: a new empty domain does not move a placed VM, a deleted neighbour does not, a
  selector or class change does, and a rule matching the VM's own labels does not block its
  own domain.
- Implemented in `crates/banlieue-controller/src/reconciler/scheduler.rs` (`place`, after
  anti-affinity; `blocked_topology_values` skips the VM itself), tested in `scheduler_tests.rs`.
- D-009 needs no change: this is what it said. `migration.rs`'s comparison stays as it is; the
  decision it compares against changes.
