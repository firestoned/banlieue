<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0098: Spread placement evenly across failure domains

- **Status:** Accepted
- **Date:** 2026-10-13
- **Deciders:** Erick Bourgeois
- **Related:** Implements the placement half of D-023 ("spread evenly for
  availability", `.github/community/01-decisions.md`) and
  [roadmap 03](../../.github/community/03-availability-zones-and-datastore-tiering.md);
  closes the spread item of [roadmap 17](../../.github/community/17-ephemeral-vm-pools.md)
  phase G; refines the scheduler's tie-break, which every earlier ADR left
  as "lexicographic, sufficient until a richer score is needed"

## Context

`schedule()` filters `(provider, failure domain)` candidates (selectors,
image readiness, storage and network classes, features, firmware, vTPM,
required anti-affinity) and then takes the lexicographically first
survivor. With more than one eligible failure domain, every VM the filters
do not separate lands on the same one.

Found live on roadmap 17 phase G: a `VirtualMachinePool` on Cloud
Hypervisor, with three hosts eligible, put every member on one host. A pool
is the case where this matters most. Its members exist to be handed out
quickly, and a pool whose warm set sits on one host loses all of it when
that host does, which is the opposite of what D-023 asks for: "even spread
across many small independent zones *is* the availability mechanism".

Two facts shape the fix:

1. The inputs are already there. The reconciler passes every
   `VirtualMachine` in the namespace to `schedule()` (for anti-affinity),
   and each scheduled one records its placement in `status.scheduled`
   (`providerName`, `failureDomain`).
2. A pool creates several members at once, and the controller reconciles
   them concurrently. A member scheduled a moment after another may not yet
   see that one's placement. Counting load alone therefore sees "both
   empty" for both, and any fixed order sends them to the same place.

## Decision

1. **Least loaded first.** Among the survivors, prefer the failure domain
   holding the fewest already-scheduled VMs of the VM's **spread group**,
   then the fewest scheduled VMs overall. The spread group of a pool member
   is the other members of the same pool (`banlieue.io/pool` label); for any
   other VM it is every VM in the namespace. A VM never counts against
   itself.
2. **Rendezvous hashing breaks ties.** Among equally loaded candidates, pick
   the one with the highest FNV-1a hash of `(vm name, provider, failure
   domain)`. Concurrent members that cannot see each other's placement still
   land on different domains with high probability, where a fixed order
   would send them all to one.
3. **Still a pure function.** Same inputs, same answer. The hash is FNV-1a,
   implemented in the scheduler, so the choice does not depend on the Rust
   version or on a randomly seeded hasher.
4. **Filters are unchanged.** Spread only orders what the filters let
   through; it never overrides a selector, a capability requirement or a
   required anti-affinity rule.

## Consequences

- A pool's members spread across every eligible host, and refills go to the
  least-loaded one.
- A VM's placement now depends on where other VMs in its namespace already
  are. That is the point, and it uses only data the scheduler already
  received; no new API field, no cache, no I/O.
- The race in Context 2 is mitigated, not eliminated: two concurrent members
  can still hash to the same domain. The next members, which do see them,
  correct the imbalance. Exact balance under concurrency would need a
  placement reservation, which this ADR does not add.
- Load is counted in VMs, not in CPU, memory or storage. Capacity-aware
  placement remains an open roadmap 03 sub-question.
- Placement is no longer predictable from names alone. Tests that relied on
  the lexicographic winner pin placement with selectors instead.
