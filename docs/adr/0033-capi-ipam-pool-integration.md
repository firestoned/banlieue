# 0033 — CAPI IPAM pool integration (deferred)

## Status

Proposed — 2026-08-23. Not implemented. Extends [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)
(static IP via `networkOverrides`) and [ADR-0032](0032-per-zone-network-subnet-shape.md)
(per-zone subnet shape). Recorded now, ahead of an active virtrigaud
migration, to capture a real design conversation and its constraints
before the details are forgotten — implementation is intentionally
deferred.

## Context

`VMClass`/`VirtualMachine` IPAM already accepts a `pool` variant
(`PoolIpamConfig { pool_ref: TypedObjectReference }`, referencing a CAPI
`ipam.cluster.x-k8s.io` pool by apiGroup/kind/name), and the `Provider`'s
ClusterRole already grants full verbs on `ipaddressclaims`/`ipaddresses`.
Neither is wired up: no reconciler creates an `IPAddressClaim`, reads back
the resulting `IPAddress`, or garbage-collects one on delete. This was
confirmed by grep — the schema and RBAC exist; the mechanism does not.

A migration off `virtrigaud` onto banlieue is underway, and the immediate
question was whether CAPI IPAM pools could both (a) migrate already-issued
addresses onto banlieue-managed `VirtualMachine`s without a human
transcribing each one, and (b) hand out fresh addresses to new VMs going
forward, per drone cluster.

Two upstream CAPI IPAM providers are real, general-purpose options:

- **`cluster-api-ipam-provider-in-cluster`**'s `InClusterIPPool` — its
  `addresses` field accepts individual IPs, CIDRs, and `"start-end"` range
  strings, so a range like `10.0.0.10-10.0.0.20` per drone cluster is
  already a supported pool shape. It allocates **the next free address in
  the pool** per claim — no way to request a specific address.
- **`ipam.metal3.io`'s `IPPool`** — supports `preAllocations: map[string]string`
  keyed by claim name, i.e. an explicit, admin-curated name→address table.
  This is the shape that could actually preserve a known VM→IP mapping
  through a migration; the in-cluster provider's ranges cannot.

Neither addresses the actual constraint in this environment: **IP
assignment is already owned by an existing self-service system (the org's
own IPAM tooling, backed by a document database) — not by banlieue, and
not by whichever CAPI IPAM provider might be installed.** Wiring in either
upstream provider for addresses that system already manages creates two
systems that both believe they own the same range, with no coordination
between them unless that system is explicitly told to stop issuing from
whatever sub-range a banlieue-facing pool claims. `IPAddress.spec` also
only ever carries `address`/`prefix`/`gateway` — no DNS/domain — so
nameservers/domain would still resolve via ADR-0032's `perZoneSubnet`
regardless of which (if any) IPAM provider is chosen.

## Decision

### For the virtrigaud migration itself: no IPAM pool integration

Every VM being migrated already has a known, correct address (from
virtrigaud / the org's own IPAM tooling). `VirtualMachine.spec.networkOverrides[].static.address`
(ADR-0024) is exact by construction — there is no allocator to get the
mapping wrong, and combined with ADR-0032's `perZoneSubnet` for
gateway/DNS/domain, nothing about the migration itself needs IPAM pool
support to work correctly today. Building and testing a new IPAM
integration under migration pressure, for a problem static overrides
already solve exactly, is not worth doing now.

### For later: the shape banlieue-side integration must take, if built

Recorded here so the design isn't re-derived from scratch when this is
picked up:

1. When a `VirtualMachine`'s (or its `VMClass`'s) `ipam.pool` is set,
   `banlieue-controller` creates an `IPAddressClaim` referencing the named
   pool, owned by the `VirtualMachine` (cascade-deleted with it, same
   pattern as every other owned object in this codebase).
2. Requeue until `claim.status.addressRef` resolves — allocation is
   asynchronous, driven by whichever provider's controller owns the pool.
3. Read the resulting `IPAddress`; fold `address`/`prefix`/`gateway` into
   the same `StaticIpamConfig` shape every other code path already
   produces, so `merge_ipam_override` (ADR-0032) and the guestinfo builder
   need no awareness that the address came from a claim rather than a
   literal override — `nameservers`/`domain` still resolve from
   `perZoneSubnet` exactly as they do for a literal static override.
4. Deletion: owner-reference cascade removes the claim; whichever pool
   provider owns it is responsible for freeing the address.

### The provider choice is explicitly NOT decided by this ADR

Three real options exist when this is picked up, in descending order of
how well each avoids the two-sources-of-truth problem:

- **The org's own IPAM system implements the CAPI IPAM provider contract**
  (watches `IPAddressClaim`s referencing a pool kind it owns, creates the
  matching `IPAddress` from its own backing-store allocation, keeps
  freeing addresses on claim deletion in sync with its own state) — the
  only option with exactly one source of truth. Requires writing and
  operating a new controller against that system's own API/store, entirely
  outside banlieue's codebase.
- **Metal3's `IPPool` with `preAllocations`**, scoped to a sub-range the
  existing IPAM system is told to stop issuing from — usable off-the-shelf,
  but now two systems each hold half the bookkeeping (the existing system
  for its range, Metal3 IPAM for the banlieue-facing sub-range), which must
  never be allowed to overlap.
- **The in-cluster provider's range pools**, same sub-range caveat as
  above, plus no exact-address guarantee — only appropriate for genuinely
  new VMs where any free address in the sub-range is acceptable.

## Consequences

- Zero code changes now. The migration proceeds entirely on already-shipped
  mechanisms (ADR-0024, ADR-0032).
- When this is picked up: `crates/banlieue-controller` gains an
  `IPAddressClaim`-watching/creating code path; `PoolIpamConfig` and the
  `Provider` ClusterRole's existing `ipaddressclaims`/`ipaddresses` grants
  finally get used for something.
- The provider choice must be revisited with the existing IPAM system's
  actual owning team before implementation starts — this ADR intentionally
  stops short of choosing, since "which sub-range gets carved out, and
  does that team want to build a CAPI IPAM provider" are decisions outside
  banlieue's own codebase.
