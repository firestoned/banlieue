# 0023 — Explicit failure-domain name override

## Status

Accepted — 2026-08-20. Extends [ADR-0019](0019-vsphere-capability-introspection-iter2.md)
(failure-domain discovery) and [ADR-0020](0020-vsphere-per-zone-iso-import.md)
Decision #5 (per-zone template folder isolation, which made failure-domain
names part of a vCenter folder path).

## Context

`FailureDomain.name` is 100% auto-computed today: `discover_inventory` walks
vCenter and calls `failure_domain_name(provider, dc, cluster)`, which
slugifies `<provider>-<dc>-<cluster>` and, when that exceeds Kubernetes'
63-char name cap, truncates and appends a stable FNV-1a hash
(`k8s_name::collision_safe_name`, ADR-0020-era fix). There is no way for an
admin to influence this name — it is derived, never declared.

Real vCenter naming schemes are long and enterprise-y (e.g. a cluster named
`compute-cluster-dedicated-nonreplicated-01`), so the auto-computed name
almost always hits the hash-suffix path:
`vcenter-example-dc-example-compute-cluster-dedicated-nonre-01883877`. This
name is not just internal plumbing — it is now:

- a Kubernetes label value and the failure domain's identity for
  scheduling/anti-affinity,
- embedded in every per-zone `image-import` Job's name and labels,
- (since ADR-0020 Decision #5) a **vCenter folder path segment** —
  `<rootFolder>/<failure-domain-name>` — so it is visible to anyone
  browsing vCenter's VM & Templates inventory.

Operators frequently already have a simpler internal convention for exactly
these zones (e.g. `cluster-01`, matching how templates were designated
before banlieue existed). Forcing the hashed name onto that operator with no
opt-out is a real usability regression, not just cosmetic — found live,
directly after landing Decision #5.

## Decision

### 1. `Provider.spec.failureDomainNameOverrides`

```rust
/// Explicit override for one discovered failure domain's generated `name`,
/// keyed by the (datacenter, cluster) pair `discover_inventory` resolves it
/// from. Named fields, not a "dc/cluster" string key, so a call site can't
/// accidentally swap them — same reasoning as `FailureDomainIdentity`
/// (ADR-0020-era `import_job_name` fix).
pub struct FailureDomainNameOverride {
    pub datacenter: String,
    pub cluster: String,
    pub name: String,
}
```

`Provider.spec.failureDomainNameOverrides: Vec<FailureDomainNameOverride>`,
`x-kubernetes-list-type: map` keyed on `[datacenter, cluster]` — the API
server rejects two overrides for the same zone at admission, the same
mechanism `VMImageSpec.sources[]` uses (keyed on `providerClass`) to reject
two sources for the same backend.

**Opt-in, not a replacement.** The auto-computed, collision-safe name is
always the fallback for any `(datacenter, cluster)` pair with no matching
override — nothing changes for an admin who never sets this field.

### 2. Lookup + a duplicate-name guard

`build_failure_domain` looks up an override by exact `(dc, cluster)` match;
if found, the override's `name` — slugified through the same
`k8s_name::collision_safe_name` path a single override name always goes
through (so a typo like `Cluster 01` still produces a valid Kubernetes
name, and a pathologically long override still gets the hash-suffix
safety net) — replaces the computed name entirely. No match: the existing
`failure_domain_name(provider, dc, cluster)` path, unchanged.

The schema's `x-kubernetes-list-type: map` prevents two overrides for the
*same* zone, but cannot express "two different zones must not resolve to
the *same* name" — an admin could type the same override `name` twice for
two different clusters. Given that name is now a vCenter folder segment and
a Job name (ADR-0020), silently allowing that would reintroduce exactly the
cross-zone collision Decision #5 fixed. `discover_inventory` therefore
checks the final `Vec<FailureDomain>` for duplicate `.name`s (regardless of
whether they came from an override, a hash collision, or anything else) and
fails the reconcile — surfaced as the existing `INVENTORY_FAILED` condition
— rather than publishing two failure domains with the same identity.

## Consequences

- **Simple, admin-chosen zone names are possible**, matching whatever
  convention already exists for a given vCenter, without losing the
  collision-safety net for zones that don't get an override.
- **One more thing for the scheduler/provider reconciler to look up per
  cluster** — negligible cost, a linear scan over what is normally a
  handful of overrides.
- **A bad override (duplicate name across zones) fails the whole Provider
  reconcile**, not just the affected zone — deliberate: the failure domain
  list is otherwise silently wrong for anyone relying on names for
  anti-affinity or folder placement, and failing loudly is cheaper than
  debugging a subtle collision later.

## Follow-ups

- If a future need arises for renaming *within* `raw` attributes (e.g. an
  override for the reported `cluster` label distinct from the name), revisit
  — out of scope here, which only overrides the generated `name`.
