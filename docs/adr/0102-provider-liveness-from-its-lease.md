<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0102: Provider liveness from its Lease

- **Status:** Accepted
- **Date:** 2026-10-15
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0012](0012-providerclass-crd-and-operator-role.md)
  (`status.workload`, the operator's own field) and
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md) Decision 3
  (External providers); reads the Lease of the SDK's leader election;
  adds a gauge under [ADR-0091](0091-prometheus-metrics-endpoint.md)'s rules; adds a
  scheduler filter beside [ADR-0098](0098-spread-placement-across-failure-domains.md)'s
  ordering

## Context

On 2026-10-13 the three Cloud Hypervisor providers of the core cluster lost
their API server address and crash-looped for about twelve hours, each
restarted by systemd roughly 8000 times. Nothing in the cluster noticed:

- Each `Provider` kept the `Ready=True` it had last written. A provider
  reports its own health, and a provider that is not running reports
  nothing, so its last word stands forever.
- `status.workload` says nothing for these providers. They are
  `External` (ADR-0060 Decision 3): the operator gave them an identity and
  runs no Deployment, and the field's own documentation says whether such a
  provider is running "is its own `Ready` condition and Lease to say". The
  first cannot change when the provider is down; nothing reads the second.
- The scheduler never consults Provider health. Its filters are selectors,
  image readiness, storage and network classes, features, firmware, vTPM and
  anti-affinity, so a dead provider keeps receiving new `VirtualMachine`s,
  which then wait for a reconciler that is not there.

A Managed provider is only partly better: the operator records the
Deployment's `readyReplicas`, which catches a pod that will not start, but
not a running pod that cannot reach the API server.

The evidence that was missing already exists. Every provider, in the
cluster or on a host, runs the SDK's leader election, which renews a
`coordination.k8s.io` Lease every 5 seconds (15-second lease). Its name is
the provider's workload name, `banlieue-provider-<ProviderClass>-<Provider>`
(`banlieue_provider_sdk::naming::workload_name`, which the operator passes
as `--leader-election-id` and the Cloud Hypervisor provider derives
itself), in the provider's workload namespace. During the outage those three
Leases simply stopped advancing. A Lease that stops being renewed is how
Kubernetes itself decides a node is gone; it is the right signal here too.

## Decision

### 1. The signal is the provider's own leader-election Lease

No new write path. A provider proves it is alive by renewing the Lease it
already renews. Nothing about the SDK's election changes.

### 2. The operator evaluates it and records it in `status.workload`

`ProviderWorkloadStatus` gains two fields, written only by the operator's
field manager, so ADR-0012's field split holds and no second writer touches
`status.conditions`:

| Field | Meaning |
| --- | --- |
| `heartbeat` | `Alive`, `Stale` or `Absent` |
| `lastHeartbeatTime` | the Lease's `renewTime`, when there is a Lease |

- **`Alive`**: the Lease was renewed within `staleAfter`.
- **`Stale`**: the Lease exists and its last renewal is older than
  `staleAfter`. The provider was running and has stopped.
- **`Absent`**: there is no Lease. The provider has never started, or runs
  with `--no-leader-elect` (local development and the e2e tiers).

`staleAfter` is an operator flag, default **60 seconds**: four lease
durations, so a slow renewal or a leader handover never reads as death.

It applies to Managed and External providers alike. For a Managed provider
it adds what `readyReplicas` cannot see: a pod that is Ready but cannot
renew.

### 3. Read on reconcile, re-checked at the deadline; no Lease watch

The operator already reconciles every Provider. It reads that Provider's
Lease with one `get` (a verb its ClusterRole already holds) and, while the
provider is `Alive`, requeues the Provider for the moment the heartbeat
would go stale (`lastHeartbeatTime + staleAfter`). While `Stale` or
`Absent` it re-checks every `staleAfter`. A Lease that stops renewing
produces no event, so a deadline is the only way to notice it; this is the
same shape as the node lifecycle controller, not a polling loop over the
cluster. No `list` or `watch` on Leases is granted, so the operator gains no
new view of other workloads' Leases.

Each transition is published as an Event on the Provider
(`ProviderHeartbeatStale`, `ProviderHeartbeatRestored`).

### 4. The scheduler skips a `Stale` provider

A new first filter: a Provider whose `status.workload.heartbeat` is
`Stale` is not a candidate. A VM left with no candidate for that reason
gets `Scheduled=False`, reason `ProviderNotAlive`. `Absent` is **not**
filtered, so a provider run with `--no-leader-elect` still schedules, and a
Provider written before this ADR (no `heartbeat` field) is treated as it was
before.

VMs already placed on a provider that goes stale are not moved: banlieue
has no live migration, and a provider that comes back resumes them.

### 5. A gauge and a printed column

- `banlieue_provider_heartbeat{provider, kind, state}`: `1` for the
  Provider's recorded state, `0` for the other two, and no series until the
  operator has recorded one. It is computed by **banlieue-controller**,
  from the same Provider reflector that already serves
  `banlieue_provider_failure_domains`, the existing per-Provider gauge
  (ADR-0091), and reads only `status.workload.heartbeat`; the operator has
  no per-Provider gauges of its own. Labels follow that gauge: Providers are
  admin-created and few, and no namespace, error text or credential is ever
  a label.
- `kubectl get providers` gains a `Heartbeat` column.
- `docs/src/guides/observability.md` gains an example alert:
  `banlieue_provider_heartbeat{state="Stale"} == 1` for five minutes.

### 6. The Cloud Hypervisor provider names itself in its Lease

On a host, the Lease's holder reads `unknown`, because the default identity
comes from `HOSTNAME`, which systemd does not set. It falls back to the
host's name, so a stale Lease says which host stopped.

## Consequences

- The 2026-10-13 outage would have shown as `Heartbeat=Stale` on three
  Providers within a minute, an Event on each, a gauge an alert can fire on,
  and no new VM scheduled onto them.
- A provider that is alive but failing (bad credentials, an unreachable
  vCenter) is still `Alive`: the Lease proves the process runs and reaches
  the API server, not that its backend works. That remains the provider's
  own `Ready` and `ProviderReachable` to say.
- A paused Provider (or a paused ProviderClass) is not reconciled, so its
  heartbeat is not re-evaluated either, exactly as its workload is not;
  it keeps the last recorded value until unpaused.
- Liveness depends on the operator running. If the operator is down,
  `heartbeat` freezes at its last value, the same weakness this ADR fixes
  for providers, one level up. The operator's own `banlieue_leader` gauge is
  the place to alert on that.
- A provider partitioned from the API server but still able to reach its
  hypervisor, and so still running guests, reads `Stale`. Correct for
  scheduling (it cannot take new work); its existing guests keep running.
- `status.workload` gains two fields, a CRD change: regenerate
  `deploy/crds/`, the API reference, and re-apply the CRDs on upgrade.
- No identity gains a permission: the operator's `get` on Leases already
  exists, and providers write nothing new. The threat-model pass found the
  opposite problem: every shared ClusterRole but the operator's granted Lease
  writes cluster-wide, so any provider could forge another's heartbeat (or a
  node's in `kube-node-lease`). Lease rights are now a namespaced Role
  narrowed to each role's own Lease, built by `banlieue bootstrap` (which now
  passes `--leader-election-namespace`), shipped in
  `deploy/*/rbac/role-leader-election.yaml`, and already the shape of the
  operator's per-Provider Role. The operator keeps its cluster-wide grant, to
  delegate it.
- ADD order for the implementation: CALM (the operator gains a read of
  each provider's Lease; the controller's scheduler reads one more status
  field), TDD, docs, then a full threat-model pass: a forged or stalled
  Lease, and who may write one, belong in it.

## Alternatives considered

- **The provider writes a heartbeat into its own Provider status.** It
  is already the field manager of `status.conditions`, but a provider that is
  down cannot write, so someone else must still notice the silence. The
  Lease is that write, already happening.
- **The controller evaluates liveness.** It schedules, so it is the reader,
  but it does not own any field of Provider status, and adding a third field
  manager to Provider status is what ADR-0012 split the fields to avoid. The
  operator already owns `status.workload` and already describes the
  workload there.
- **Watch every Lease.** Reacts instantly to a renewal, but needs
  `list`/`watch` on Leases, which reveals every Lease in its scope, and a
  stopped renewal still emits nothing, so a deadline timer is needed anyway.
- **Rely on Prometheus `up` for each provider's metrics port.** Works only
  where a scraper reaches every host and every provider pod, and an
  unscraped host fails silently, as it did. The gauge in Decision 5 comes
  from inside the cluster, and the scheduler must not depend on telemetry
  anyway (TB-14).
- **Make systemd give up (`StartLimitBurst`) so the unit shows `failed`.**
  Louder on the host, invisible to the cluster, and a provider that gave up
  needs a human to restart it after the cause is fixed. Not a substitute.
