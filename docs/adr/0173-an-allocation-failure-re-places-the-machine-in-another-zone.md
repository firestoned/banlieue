<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0173: An allocation failure re-places the machine in another zone

- **Status:** Accepted
- **Date:** 2026-11-26
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-11-27 (Decision 7: Azure creates the OS disk with the VM's tags; the Tags API merge, `status.osDiskTagged` and `Microsoft.Resources/tags/write` are removed)
- **Related:** Implements [ADR-0161](0161-capacity-on-a-cloud-is-quota.md)
  Decision 2 (allocation failure is a zone problem, retried in another zone),
  which said what happens but not who does it. Builds on
  [ADR-0158](0158-shape-of-a-cloud-provider-class.md) Decision 3 (zones as
  failure domains), [ADR-0164](0164-azure-inframachine-and-a-first-party-arm-client.md)
  (the Azure InfraMachine), [ADR-0125](0125-a-placed-vm-keeps-a-placement-that-still-fits.md)
  (a placed VM keeps a placement that still fits) and
  [ADR-0098](0098-spread-placement-across-failure-domains.md) (spread). Roadmap 23
  phase C.

## Context

A cloud can refuse a VM in one zone that it would take in another: the size
is restricted in that zone for the subscription (`SkuNotAvailable`,
`NotAvailableForSubscription`) or the zone has no capacity for it right now
(`ZonalAllocationFailed`, `AllocationFailed`, the two `Overconstrained...`
codes). Quota cannot predict either (ADR-0161). ADR-0161 Decision 2 says such
a failure skips the zone with exponential back-off, starting at 5 minutes, and
retries the machine in another zone if its placement allows.

Until now the Azure provider recorded `VMCreated=False` with reason
`ZoneUnavailable` and retried **the same zone** every 5 minutes, forever. Its
module docs said that moving the machine was the controller's decision, but
nothing in the controller acted on it.

The zone is part of the infra CR's spec (`AzureVirtualMachine.spec.zone`,
from the failure domain), and the spec is the controller's: it schedules the
`VirtualMachine` and server-side-applies the infra CR. A provider that
rewrote the zone would be a second scheduler, would fight the controller's
field manager, and would make placement decisions without the inputs (spread,
anti-affinity, capacity, the VM's own selectors) the scheduler has. The
non-negotiables settle the shape: the provider reports through the infra CR's
status (no RPC, status mirrors infra) and the controller decides.

Three facts make the decision less obvious than "the scheduler skips the
zone":

1. Placement is re-run every pass, and a placed VM keeps a placement that
   still fits (ADR-0125). Without a memory of the failure, the scheduler
   chooses the failed zone again.
2. Re-placing means deleting the infra CR and applying a new one. The memory
   cannot live on the infra CR, which is the thing being deleted.
3. A different scheduler answer is ordinarily **drift**, answered by
   `migrationPolicy` (`Never` keeps the old placement). A machine whose create
   failed has no VM to migrate.

## Decision

### 1. The provider reports; it never moves a machine

A create that ARM refuses or fails for a zone-specific reason sets the infra
CR's `VMCreated` condition to `False` with reason `ZoneUnavailable`, and its
`lastTransitionTime` to the time of that failure. `status.failureDomain`
names the zone it failed in. This is a contract any cloud provider can meet:
the condition type and reason are constants in `banlieue-api`
(`common::allocation`), not Azure strings in the controller.

The provider never edits `spec.zone`.

### 2. The controller re-places a machine that never ran

When the infra CR of a `VirtualMachine`'s **recorded** placement reports
`VMCreated=False`/`ZoneUnavailable` for that failure domain, and
`status.initialization.provisioned` is not `true`, the controller:

1. records the failure on `VirtualMachine.status.placementFailures`
   (Decision 3);
2. schedules again with the failed failure domain excluded (Decision 4);
3. if that yields a different placement, writes it to `status.scheduled`,
   sets `PlacementValid=False` with reason `ZoneUnavailable`, and deletes
   the infra CR in the same pass. The provider's finalizer removes the NIC
   and anything else the failed create left, by owner tag (ADR-0158
   Decision 6). The next pass applies a fresh infra CR for the new
   placement, with a new UID and so new resource names.

This is **not a migration** and ignores `migrationPolicy`: there is no VM,
so there is nothing a policy could protect. The guard is the infra CR's own
status: a machine whose infra reports `provisioned=true` is never re-placed
this way, whatever its conditions say.

If no other placement fits (one zone, a selector pinning the VM to the
failed zone, or every other zone excluded too), the infra CR stays where it
is and the provider retries it in place, with its own back-off (Decision 5).
An exclusion is a preference; it never makes a VM unschedulable.

The new placement may be another zone of the same `Provider` or, when the VM's
placement selectors allow, another `Provider`: the scheduler chooses with all
its usual inputs, which is the point of asking it.

### 3. The memory lives on the `VirtualMachine`'s status

`status.placementFailures` is a list of `{providerName, failureDomain,
reason, failures, lastFailureTime}`, at most one entry per `(provider,
failure domain)` and at most `MAX_PLACEMENT_FAILURES` (8) entries, the oldest
dropped first. An infra CR's failure is counted once: a failure is new only
when its `lastTransitionTime` is later than the entry's `lastFailureTime`.

The list is cleared when the infra CR reports `provisioned=true`: a running
VM keeps its placement (ADR-0125), and its creation history no longer
decides anything.

The controller owns `VirtualMachine.status`, so no new writer is introduced,
and the status survives the infra CR's deletion and a controller restart
(controllers are stateless).

### 4. Exclusion is exponential and bounded

A failure domain with an entry is excluded for
`min(5 min * 2^(failures - 1), 1 h)` after `lastFailureTime`
(`ZONE_EXCLUSION_BASE`, `ZONE_EXCLUSION_MAX`): 5, 10, 20, 40, then 60
minutes. The 5-minute start is ADR-0161's. The cap keeps a zone that recovers
from being shunned for longer than an operator would wait.

The scheduler takes the active exclusions as an input
(`schedule_excluding`), keeping it a pure function, and falls back to the
unexcluded answer when the exclusions leave nothing.

### 5. The provider's in-place retry backs off exponentially too

When the machine stays where it is, the provider waits
`min(5 min * 2^(n - 1), 1 h)` before creating again, where `n` is
`status.createFailures`, the consecutive failed creates of this generation.
A new generation starts the count again and retries at once (the spec
changed, so the old failure says nothing about the new request). `VMCreated=True`
resets it. This replaces the fixed 5-minute retry, and applies to every reason
that waits (`ZoneUnavailable`, `QuotaExceeded`, `CreateRefused`,
`ForeignResource`, `OperationFailed`). Transient errors (throttling, 5xx,
transport) keep their short requeue and ARM's `Retry-After`.

### 6. Per machine, not per fleet

ADR-0161 says the zone "is skipped for that size". This ADR skips it for the
machine that saw the failure. A `Provider`-wide, per-size skip (published on
`Provider.status` so every new placement avoids the zone) needs the evidence
aggregated across machines and a writer for it, and is left open.

### 7. The OS disk carries the machine's ownership tags

Re-placement deletes and recreates machines more often, so what a failed or
deleted machine leaves behind must be findable by tag. ARM takes no `tags`
on `storageProfile.osDisk`, but it creates the implicit OS disk with the
VM's tags in the same write, so the disk carries the same ownership tags as
the VM and NIC from the create. The provider names the OS disk from the
owner's UID (`banlieue-<uid>-osdisk`), so it is found by its owner and not
by a name ARM made up. Never by a name prefix: a name is not proof of
ownership, and a tagged disk is one the finalizer and the orphan sweep will
delete.

*Amended 2026-11-27.* As first decided, this said ARM copies none of the
VM's tags onto the disk, and had the provider merge them on through the Tags
API (`PATCH {disk}/providers/Microsoft.Resources/tags/default`, `operation:
Merge`) once the VM had provisioned, recording `status.osDiskTagged`, with
`Microsoft.Resources/tags/write` added to its role. Observed on 2026-11-27
(compute API 2024-07-01): every OS disk of five banlieue VMs, all created
before the disk was named from the UID, carried exactly its VM's tags, and
the activity log shows a single `Microsoft.Compute/disks/write` at create
and no tag write. The merge was redundant, and on those older disks it
missed (it looked them up by the new name) and retried on every pass. It is
removed, with the role's `Microsoft.Resources/tags/write`;
`status.osDiskTagged` is deprecated and no longer written.

## Consequences

- A VM whose zone is full is running elsewhere within one create attempt
  instead of waiting, at best, 5 minutes per retry in a zone that may stay
  full for hours.
- The provider stays a reporter, the scheduler stays the only placer, and
  nothing new crosses the CRD-only bus: one condition reason the provider
  already set, one status list the controller already owns.
- `VirtualMachine.status.placementFailures` and
  `AzureVirtualMachine.status.createFailures` are new status fields; CRDs are
  regenerated, and a live cluster must re-apply them before the new binary
  writes them. `AzureVirtualMachine.status.osDiskTagged` shipped in v0.6.1
  and is deprecated since the 2026-11-27 amendment: kept so stored objects
  stay valid, no longer written. A VM created before this change keeps its
  ARM-named disk, tagged by Azure the same way.
- The provider's role holds no `Microsoft.Resources/tags/write` (removed by
  the amendment). A role created from v0.6.1's
  `deploy/provider-azure/role-definition.json` still has it and should be
  updated; the extra grant is unused.
- A pool member reaped by its pool's `provisioningTimeoutSeconds` takes its
  failure history with it; its replacement starts with none. Re-placement
  is immediate, so it happens long before any reap. The in-place back-off
  can exceed a pool's timeout only when there is no other zone, and then the
  pool's recreate is the retry. The fleet-wide skip of Decision 6 is what
  would remember across members.
- `PlacementValid=False`/`ZoneUnavailable` is visible on the
  `VirtualMachine` for the pass that re-places it, and the failure history
  stays on its status until it runs.
- Any cloud provider (AWS `InsufficientInstanceCapacity`) gets the same
  behaviour by reporting the same condition.

## Alternatives considered

- **The provider rewrites `spec.zone`.** Rejected: two writers of one field,
  a second scheduler without the scheduler's inputs, and a provider that
  could move a machine into a zone its network or storage class does not
  reach.
- **Treat it as drift and let `migrationPolicy` decide.** Rejected: `Never`
  (and `Manual` without the annotation) would leave a machine that never ran
  in a zone that refuses it, forever, to protect a VM that does not exist.
- **Keep the exclusions on the infra CR.** Rejected: it is deleted by the
  re-placement, and a new one starts with an empty status.
- **A fixed exclusion period.** Rejected for the same reason ADR-0161 asked
  for back-off: a zone out of capacity for hours would be retried every few
  minutes by every machine that tried it.
