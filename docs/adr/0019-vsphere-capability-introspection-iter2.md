<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0019 — vSphere capability introspection (iteration 2)

- **Status:** Accepted
- **Date:** 2026-08-09
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0002 (InfraCluster failure-domain aggregation); ADR-0008
  (BYOC vSphere HTTP client). Extends the Phase 1B iteration-1 `Provider`
  reconciler, which discovers failure domains but leaves their capability
  attributes empty.

## Context

The `Provider` reconciler (iteration 1) walks the vCenter inventory and emits
one `FailureDomain` per `(datacenter, cluster)`, but hardcodes
`FailureDomainAttributes { available_storage_classes: [], available_network_classes: [], features: [] }`.
`Provider.spec.capabilities` (the admin's storage/network-class → concrete-target
mappings, plus asserted feature flags) is accepted by the CRD but never read.

Confirmed on-prem against a real vCenter: an enriched `Provider` (three
`storageClasses` → datastore clusters, three `networkClasses` → DVS port groups,
`features: [hotAddCPU, hotAddMemory, efiSecureBoot]`) reconciles to
`observedGeneration` matching spec and `Ready=True`, yet every failure domain's
`attributes` stay empty. Without those fields the scheduler (a later phase)
cannot filter failure domains by the storage/network classes a `VMClass` /
`VMImage` requests — which is the entire point of declaring capabilities.

## Decision

Implement capability **reachability** per failure domain: for each
`(datacenter, cluster)`, discover the datastores and networks reachable from
that cluster, match them against `spec.capabilities`, and populate the failure
domain's `attributes`.

### Client surface (two new `VSphereClient` methods)

```rust
async fn list_datastores(&self, cluster: &Cluster) -> Result<Vec<Datastore>>;
async fn list_networks(&self, cluster: &Cluster) -> Result<Vec<Network>>;
```

with slim projections:

```rust
pub struct Datastore { name, moref, datastore_cluster: Option<String> }
pub struct Network   { name, moref, distributed: bool }
```

Both are derived from the cluster's own associations, not the folder tree:
`ClusterComputeResource.datastore` / `.network` give the reachable MORefs
directly. `Datastore.datastore_cluster` is the name of the containing
`StoragePod` when the datastore's parent is one (i.e. an SDRS datastore
cluster), else `None`. `Network.distributed` is true when the MORef type is
`DistributedVirtualPortgroup`.

### Matching rules (pure, unit-tested)

A `storageClasses[]` entry is available in a failure domain when its `target`:

- `{ datastore: X }` — some reachable datastore is named `X`; or
- `{ datastoreCluster: X }` — some reachable datastore's `datastore_cluster` is `X`.

A `networkClasses[]` entry is available when its `target`:

- `{ portGroup: X }` — some reachable **non-distributed** network is named `X`; or
- `{ distributedPortGroup: X }` — some reachable **distributed** network is named `X`.

`attributes.availableStorageClasses` / `availableNetworkClasses` are the `name`s
(not targets) of the matching entries, so the scheduler matches on the abstract
class name.

### Deliberately out of scope for this iteration

- **`tagCategory` / `tag` storage targets.** These require the vCenter CIS REST
  (vAPI) tagging endpoint, a different transport from the VI/JSON one the BYOC
  client speaks (ADR-0008). Such a target is left unmatched (the class is simply
  not reported available) and logged, not errored — a follow-up ADR.
- **Feature-flag downgrade.** `attributes.features` is populated by passing
  through `spec.capabilities.features` verbatim (the admin's assertion). Probing
  actual cluster capability flags (`hotAdd*`, `efiSecureBoot`,
  `nestedVirtualization`) to *downgrade* an over-asserted feature needs the
  cluster `EnvironmentBrowser` / host capability surface and is a follow-up. The
  status field's contract already allows a later downgrade.

### Reconciler shape

`discover_inventory` gains the `&ProviderCapabilities` argument and, per cluster,
calls `list_datastores` + `list_networks`, then a pure
`compute_failure_domain_attributes(capabilities, &datastores, &networks)` builds
the `FailureDomainAttributes`. The extra two calls per cluster keep the
5-minute reconcile cadence (they are not on the hot path).

## Consequences

**Positive**

- The scheduler can finally filter failure domains by storage/network class —
  capabilities stop being write-only.
- Reachability is computed from the cluster's real associations, so a class
  targeting a datastore/PG not reachable from a given cluster correctly does
  *not* appear available there (per-failure-domain precision, ADR-0002).
- The matching logic is pure and `FakeClient`-testable; no vСenter needed for
  unit coverage.

**Negative / trade-offs**

- Two extra vCenter round-trips per cluster per reconcile. Acceptable at the
  5-minute cadence; if inventories grow, a PropertyCollector batch fetch is the
  optimisation (noted, not done).
- `tagCategory`/`tag` and feature-downgrade remain unimplemented; a Provider
  relying on tag-based storage targets will see those classes reported
  unavailable until the CIS REST follow-up lands. Documented, not silent.

**Follow-ups**

- CIS REST tagging client for `tagCategory`/`tag` storage targets.
- Feature-flag probing to downgrade over-asserted `features`.
- PropertyCollector batching if per-cluster round-trips become costly.
