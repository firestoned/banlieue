# 0030 — Per-zone concrete targets for storage/network class mappings

## Status

Accepted — 2026-08-23. Extends [ADR-0019](0019-vsphere-capability-introspection-iter2.md)
(capability reachability per failure domain).

## Context

`Provider.spec.capabilities.storageClasses[]` / `networkClasses[]` each map
one abstract class name to **one concrete backend target**, Provider-wide:

```rust
pub struct NetworkClassMapping {
    pub name: String,               // abstract, e.g. "network-01"
    pub target: BTreeMap<String, String>, // ONE target for the WHOLE Provider
}
```

A single `Provider` represents one backend instance (e.g. one vCenter) and
spans multiple failure domains — one per `(datacenter, cluster)`. ADR-0019
made *reachability* of a class properly per-failure-domain (a class is only
reported available in the zones where its target is actually reachable),
but the target checked against every zone is the same single value. In
practice, semantically-equivalent networks/datastores on different clusters
of the same vCenter are very rarely named identically — a distributed port
group called `sscssc90-01-d-3016` on `cluster-01` has a differently-named
counterpart on `cluster-02`/`cluster-03` — so today's single flat `target`
can only ever be reachable in the one cluster whose literal name happens to
match it.

Found live: a `VMClass` (`hadron-small`) requesting `networkClass:
network-01` scheduled correctly, but `network-01` was only ever reported
available on `cluster-01` — not because `cluster-02`/`03` lack an
equivalent network, but because the Provider's one `network_classes[]`
entry for `network-01` names `cluster-01`'s specific port group. The
abstraction the project intends — one `VMClass`, portable across every
cluster of a Provider, and across every Provider that advertises the same
abstract names (a `VMClass` has no binding to any specific `Provider`) —
is broken exactly one level below `VMClass`: at the Provider capability
mapping, not at `VMClass` or the scheduler.

Three call sites already resolve a class name against
`ProviderCapabilities`, and all three already have the specific
`(datacenter, cluster)` identity in scope at the point of resolution — they
just never pass it into the lookup:

- `banlieue-provider-vsphere/src/reconciler/provider.rs::compute_failure_domain_attributes`
  — reachability, called once per `(dc_name, cluster_name)` already.
- `banlieue-provider-vsphere/src/import.rs::resolve_storage_target` /
  `resolve_network_target` — the per-zone `image-import` Job already knows
  its own failure domain (`--failure-domain`, resolved via
  `failure_domain_of`, whose `attributes.raw` carries `datacenter`/`cluster`).
- `banlieue-controller/src/reconciler/scheduler.rs::build_decision` /
  `first_target_value` — already holds `fd: &FailureDomain` (the chosen
  zone) when resolving `VirtualMachine.status.scheduled.resolvedStorage[]` /
  `resolvedNetworks[]`.

## Decision

**`StorageClassMapping`/`NetworkClassMapping` gain an optional per-zone
override list, keyed by the same `(datacenter, cluster)` identity
[ADR-0023](0023-explicit-failure-domain-name-override.md)'s
`failureDomainNameOverrides` already uses** — the vCenter-reported names,
not a failure domain's own (possibly admin-renamed) display name, which
would make the mapping fragile to a rename.

```rust
pub struct StorageClassMapping {
    pub name: String,
    /// Default target, used in any zone not covered by `per_zone` below.
    /// `None` means this class resolves ONLY in the zones `per_zone`
    /// explicitly lists.
    pub target: Option<BTreeMap<String, String>>,
    /// Per-(datacenter, cluster) overrides of `target`.
    pub per_zone: Vec<ScopedTarget>,
}

pub struct ScopedTarget {
    pub datacenter: String,
    pub cluster: String,
    pub target: BTreeMap<String, String>,
}
```

`NetworkClassMapping` gains the identical two fields; `ScopedTarget` is
shared by both (same shape, same identity key).

**One method resolves the precedence, used by all three call sites instead
of each re-implementing it:**

```rust
impl StorageClassMapping /* and NetworkClassMapping */ {
    pub fn target_for(&self, datacenter: &str, cluster: &str) -> Option<&BTreeMap<String, String>> {
        self.per_zone
            .iter()
            .find(|z| z.datacenter == datacenter && z.cluster == cluster)
            .map(|z| &z.target)
            .or(self.target.as_ref())
    }
}
```

An exact `(datacenter, cluster)` match in `per_zone` wins; otherwise the
default `target` applies; if neither is present, the class simply does not
resolve in that zone (not an error — the same "not reported available" fail
path ADR-0019 already established for an unmatched `tagCategory`/`tag`
target).

### Why not always require the per-zone list (no bare `target`)

A class that genuinely is identical everywhere (a shared NFS datastore
mounted the same way on every cluster, for example) stays a one-line
mapping; only classes that actually differ per zone pay the verbosity of
listing `per_zone` entries. `target: Option<...>` rather than a mandatory
list also lets an admin declare a class available **only** on specific
zones (`target: None`, `per_zone` covering just those), which is a
legitimate configuration ADR-0019's original single-target shape could not
express either (a class was either global-and-checked-everywhere or absent
entirely).

### Why key on `(datacenter, cluster)`, not the failure domain's display name

`Provider.status.failureDomains[].name` can be an admin-chosen override
(ADR-0023) or a slugified auto-computed name — either way it is a *label*,
not the failure domain's identity. `attributes.raw["datacenter"]` /
`["cluster"]` (already populated by `compute_failure_domain_attributes`)
and ADR-0023's own override keying are the actual stable identity a mapping
should reference; matching a display name would silently break every
`per_zone` entry the moment someone renamed a failure domain.

### Call-site changes

- `compute_failure_domain_attributes` calls `mapping.target_for(dc, cluster)`
  and checks *that* target's reachability, not `mapping.target` unconditionally.
- `resolve_storage_target` / `resolve_network_target` in `import.rs` take
  the resolved failure domain's `datacenter`/`cluster` (already available
  from `failure_domain_of`'s `attributes.raw`) and call `target_for`.
- `first_target_value` in `scheduler.rs`'s `build_decision` takes `fd`'s
  `attributes.raw["datacenter"]` / `["cluster"]` and calls `target_for`
  instead of reading `mapping.target.values().next()` directly.

### Not covered by this ADR

- **libvirt / Proxmox.** libvirt's failure domains today are one host per
  Provider — no multi-cluster ambiguity exists there yet, so this ADR is
  vSphere-scoped for now; the `ScopedTarget` shape is generic enough to
  extend to another backend's own multi-zone case later without another
  breaking change.
- **Cross-Provider (cross-vCenter) portability of a `VMClass`.** Already
  true by construction — `VMClass` has no binding to a specific `Provider`;
  each Provider independently declares its own mapping (default and/or
  per-zone) for the same abstract class names. This ADR fixes the
  single-vCenter, multi-cluster case; nothing further is needed for the
  multi-vCenter case once each vCenter's own `Provider` correctly maps its
  own zones.
- **One `VirtualMachine` definition realized as N instances spread across
  clusters/vCenters.** A distinct, much larger feature (raised in the same
  discussion that led to this ADR) — replication/fan-out of one definition
  into multiple live VMs is a different problem from "one class,
  schedulable anywhere," and is deliberately left for its own future ADR
  rather than bundled here.

## Consequences

- A single `VMClass` (and a single `VMImage`) becomes truly usable across
  every cluster of a vCenter, and across multiple vCenters, once each
  `Provider` declares matching abstract class names — the actual blocker
  found live is fixed, with no changes to `VMClass`, the scheduler's
  matching logic, or `VirtualMachine`.
- `crates/banlieue-api` — a source-of-truth CRD schema change
  (`StorageClassMapping`/`NetworkClassMapping` gain `per_zone`; `target`
  becomes `Option`) — requires regenerating `deploy/crds/banlieue.io_providers.yaml`
  before this ships.
- Existing `Provider` CRs with only a flat `target` set keep working
  unchanged — `per_zone` defaults to empty, so `target_for` always falls
  through to today's behavior for any Provider that hasn't opted into the
  per-zone shape yet.
