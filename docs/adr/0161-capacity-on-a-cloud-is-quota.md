<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0161: Capacity on a cloud is quota, not measured hardware

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 22, roadmap 23. [ADR-0107](0107-capacity-aware-scheduling.md)
  (capacity-aware scheduling), [ADR-0128](0128-every-provider-publishes-measured-capacity.md)
  (every provider publishes measured capacity), [ADR-0124](0124-unschedulable-providers.md)
  (unschedulable providers), [ADR-0158](0158-shape-of-a-cloud-provider-class.md),
  [ADR-0160](0160-a-vmclass-becomes-an-instance-size-by-explicit-mapping.md).

## Context

ADR-0128 has every provider publish measured capacity (free CPU, memory and
disk on the hardware), and ADR-0107 schedules against it. A cloud has no
hardware banlieue can measure. What limits how many VMs fit is the
**subscription's quota**: on the roadmap 23 spike, a new subscription had 4
vCPUs per region and 4 per size family, so a two-member pool of 2-vCPU VMs
plus a router used every core, and a refill would have failed.

Quota cannot predict everything: a zone can be out of capacity for a size
right now (`ZonalAllocationFailed`, `InsufficientInstanceCapacity`) though
quota allows it.

## Decision

### 1. The provider publishes quota headroom as capacity

Per `Provider`, the provider reads vCPU usage and limit for the region and
for each mapped size's family (Azure Compute usages; AWS Service Quotas) on a
slow cadence and publishes **headroom = limit - usage**, in vCPUs, as the
failure domain's capacity (the same `FailureDomainCapacity.cpus` ADR-0128
uses), the smaller of regional and family headroom for the mapped size.
Memory and disk are not quota-limited and are left unset. The scheduler
treats it as ADR-0107 treats measured capacity; the cloud overcommit ratio is
1.

Regional quota is shared by all zones, so each zone publishes the same
regional headroom; the scheduler's per-`Provider` sum must not double count
(it uses the regional figure once).

### 2. Allocation failure is a zone problem, retried in another zone

A create that fails with a capacity error in one zone is a placement failure
on that zone: the provider records the reason, the zone is skipped for that
size with exponential back-off (starting at 5 minutes), and the machine is
retried in another zone of the same `Provider` if its placement allows.
It is surfaced as a condition, never a silent spin.

### 3. Quota exhaustion is not an error state of the `Provider`

Zero headroom makes a `Provider` unschedulable for that size, with a reason
naming the quota, exactly as a full host would (ADR-0124); it is not
`Ready=False`.

## Consequences

- Pools and the scheduler see clouds and hosts through the same capacity
  field, so a pool can prefer a host and spill to a cloud.
- Quota increases are an operator action outside banlieue; the provider makes
  the ceiling visible, which is what the spike needed and lacked.
- Capacity errors at create time remain possible and are handled per zone.

## Alternatives considered

- **No capacity on clouds ("infinite").** Rejected: the spike hit the quota
  with three VMs.
- **Probe by creating.** Rejected: billable, slow, and throttled.
