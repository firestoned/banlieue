# IPAM Pool Integration (CAPI `ipam.cluster.x-k8s.io`)

> **Goal.** Let a `VirtualMachine` (or its `VMClass`) request a static
> address from a CAPI IPAM pool instead of a literal
> `networkOverrides[].static.address`, without banlieue becoming a second
> source of truth for addresses an external IPAM system already owns.
>
> **Stop condition.** A `VirtualMachine` with `ipam.pool` set gets a real
> `IPAddressClaim` created, waits for it to resolve, and boots with the
> claimed address/prefix/gateway — merged with `perZoneSubnet` (ADR-0032)
> for nameservers/domain exactly as a literal static override is today.
> Deleting the VM frees the address.
>
> **Status: not started.** Deferred by explicit decision — see
> [ADR-0033](../../banlieue/docs/adr/0033-capi-ipam-pool-integration.md)
> for the full design conversation, the two upstream provider options
> considered, and why this was deferred rather than built immediately.
> **Read that ADR before touching this roadmap** — it is the source of
> truth for the reasoning; this file is only the execution plan once
> someone picks the work up.

## Why this exists as its own roadmap entry

Raised during the `virtrigaud` → banlieue migration: every VM being
migrated already has a known-correct address from the existing
self-service IPAM system, so the migration itself needs zero new code
(literal `networkOverrides[].static` + ADR-0032's `perZoneSubnet` already
cover it exactly). This roadmap is for the *next* need — new VMs, created
after the migration, that should get a fresh address from a managed pool
instead of a human hand-picking one per VM.

## Preconditions

- [ ] **A decision from the existing IPAM system's owning team**, made
      *before* any code here starts (this is the actual blocker, not
      banlieue engineering time):
  - Will that team build a CAPI IPAM provider against their own system
    (the only option with exactly one source of truth for addresses)?
  - Or does banlieue get a carved-out sub-range that the existing system
    is told to permanently stop issuing from, backed by an off-the-shelf
    provider (`cluster-api-ipam-provider-in-cluster` or
    `ipam.metal3.io`)?
  - This decision changes which CRD kind `pool_ref` in the tasks below
    actually points at — do not start implementation until it's made.
- [ ] `ADR-0032` (per-zone subnet shape) merged and deployed — this work
      builds directly on `merge_ipam_override`'s existing precedence
      logic (`crates/banlieue-controller/src/reconciler/infra.rs`).
- [ ] Whichever IPAM provider was chosen above is actually installed in
      the management cluster and has at least one pool object created
      and reachable, confirmed independently of banlieue (e.g. a manual
      `IPAddressClaim` against it resolves to an `IPAddress`) — don't
      debug banlieue's own reconcile loop against a pool that was never
      going to answer in the first place.

## Current state (confirmed in code, as of this writing)

- `banlieue-api`'s `PoolIpamConfig { pool_ref: TypedObjectReference }`
  already exists on both `IpamSpec` (per-VM) and is reachable via
  `VMClass.network.interfaces[].ipam` → nothing reads it downstream yet.
- The `Provider`'s `ClusterRole` already grants full verbs on
  `ipam.cluster.x-k8s.io`'s `ipaddressclaims`/`ipaddresses` — RBAC is
  ahead of the reconciler.
- Nothing in `banlieue-controller` or any provider creates an
  `IPAddressClaim`, watches one, reads back an `IPAddress`, or
  garbage-collects a claim on VM deletion. This is a from-scratch
  reconcile path, not a gap in an existing one.

## Design (recap from ADR-0033 — read the ADR for full reasoning)

1. `banlieue-controller` creates one `IPAddressClaim` per NIC with
   `ipam.pool` set, owned by the `VirtualMachine` (cascade-deleted with
   it — same pattern as every other owned object in this codebase, e.g.
   the `VSphereMachine` itself).
2. Requeue (short interval, same shape as any other "waiting on an async
   external actor" reconcile in this codebase) until
   `claim.status.addressRef` resolves.
3. Read the resulting `IPAddress`; fold `address`/`prefix`/`gateway` into
   the same `StaticIpamConfig` shape `merge_ipam_override` already
   produces for a literal override — `nameservers`/`domain` still resolve
   from `perZoneSubnet` exactly as they do today. Everything downstream
   (`guestinfo` builder, the vSphere clone request, …) needs zero
   awareness that the address came from a claim.
4. Deletion: owner-reference cascade removes the claim; whichever
   provider owns the pool is responsible for freeing the address on its
   own side.

## Tasks

### Schema / plumbing

- [ ] Confirm `PoolIpamConfig.pool_ref`'s shape (`apiGroup`/`kind`/`name`)
      matches whatever pool CRD was actually chosen in Preconditions —
      adjust if the chosen provider's `poolRef` contract differs.
- [ ] Add a `DynamicObject`-based client for `IPAddressClaim`/`IPAddress`
      (both are simple, well-known upstream types; no vendored Go types
      needed — `kube-rs` can address them generically the same way
      `banlieue-imagebuilder` already talks to kairos-operator's
      `OSArtifact` via a `DynamicObject` `Api` for an external CRD it
      doesn't own).

### Reconciler

- [ ] New reconcile step in `banlieue-controller` (likely
      `crates/banlieue-controller/src/reconciler/ipam.rs`, mirroring the
      existing `infra.rs`/`scheduler.rs` split — pure decision logic
      separate from the K8s-API-touching apply step, same testing
      pattern as the rest of this codebase): given a `VirtualMachine`
      whose class/override requests `ipam.pool`, compute the desired
      `IPAddressClaim`.
- [ ] Server-side-apply the claim; own it by the `VirtualMachine`.
- [ ] Watch `IPAddressClaim` (`.owns()` on the `VirtualMachine`
      `Controller`, same event-driven pattern as everywhere else in this
      codebase — no polling) so a claim resolving re-triggers the VM's
      reconcile immediately.
- [ ] Resolve the claimed `IPAddress` and thread its `address`/`prefix`/
      `gateway` into `merge_ipam_override` (extend its signature the same
      way ADR-0032 added `zone_subnet` — a third optional input, same
      "explicit per-VM value always wins" precedence already established
      there).
- [ ] Handle the not-yet-resolved case: requeue, do not fail the VM's own
      reconcile or block unrelated fields from progressing.

### Deletion / GC

- [ ] Confirm owner-reference cascade actually frees the address on the
      chosen provider (verify live against whichever pool provider was
      installed in Preconditions — do not assume from documentation
      alone, this project's own convention per `rules/testing.md` and
      the several ADRs this session that were only trusted after a live
      check).

### Tests (TDD, per this project's mandatory workflow)

- [ ] Pure unit tests for the claim-building logic (given a `VMClass` NIC
      + `pool_ref`, what `IPAddressClaim` gets built) — no cluster needed,
      same style as `build_vsphere_machine`'s existing tests.
- [ ] `merge_ipam_override` extended-signature tests: address from a
      resolved `IPAddress`, `zone_subnet` still supplies nameservers/
      domain, an explicit per-VM override field still wins.
- [ ] Integration test against a real (or `kind`-hosted) instance of
      whichever pool provider was chosen, per this project's existing
      "live-verify before trusting" convention.

### Docs (mandatory per this project's process — ADR → CALM → TDD → implement → docs)

- [ ] `docs/architecture/calm/architecture.json`: new relationship/flow
      for the claim→address resolution, alongside the existing
      `flow-provision-a-vm`-style entries.
- [ ] `docs/src/guides/` — likely a new guide or a section added to
      `environment-provider-isolation.md`, once the actual chosen
      provider is known (the write-up is meaningfully different for "the
      existing IPAM system implements the contract" vs. "we're pointed
      at an off-the-shelf pool with a carved-out sub-range").
- [ ] `.claude/CHANGELOG.md` entry, `**Author:**` line, per this
      project's mandatory changelog convention.
- [ ] Flip [ADR-0033](../../banlieue/docs/adr/0033-capi-ipam-pool-integration.md)'s
      status from Proposed to Accepted once the provider decision is
      final and implementation lands — record the actual provider choice
      in the ADR itself (its "Decision" section currently, correctly,
      leaves this open).

## Open questions (answer before or during implementation)

- Does the chosen pool provider's `IPAddressClaim` support any kind of
  "give me this specific address" request, or is it strictly "next free
  in the pool"? This determines whether pools can ever be used to
  *reclaim* a specific known-good address (e.g. re-provisioning a VM that
  should keep its old IP) or only ever hand out fresh ones.
- Should `VMClass.network.interfaces[].ipam.pool` (class-level) and a
  per-VM override both be able to request a pool, or only one level? The
  existing static-IP precedent (ADR-0024) makes the per-VM override the
  only place an *address* can ever live (a class is shared); the same
  reasoning likely applies here — the class can declare "this NIC uses
  pool-based IPAM" as a mode, but the actual claim is inherently a
  per-VM object regardless of which level requested it.
- What happens to an `IPAddressClaim` if its `VirtualMachine` is deleted
  *before* the claim ever resolved (pool exhausted, provider down)? Owner-
  reference cascade still deletes the claim correctly, but confirm the
  pool provider doesn't leak a half-allocated state on its own side.

## Gotchas (anticipated, cross-reference ADR-0033's own Context section)

- **Two sources of truth.** The single biggest risk this whole roadmap
  entry exists to name explicitly: if the sub-range fed to any
  off-the-shelf pool provider is not *exclusively* reserved from the
  existing IPAM system's perspective, you will eventually get a real
  address collision between the two systems. This must be verified with
  the existing system's owning team, not assumed.
- **No DNS/domain in the CAPI IPAM contract.** `IPAddress.spec` only ever
  carries `address`/`prefix`/`gateway` — don't expect a pool provider to
  ever supply nameservers or a domain; `perZoneSubnet` (ADR-0032) is
  permanently the right place for those regardless of how the address
  itself was obtained.
- **Async allocation latency.** Unlike a literal static override
  (instant), a claim resolving is asynchronous and depends on the pool
  provider's own reconcile loop. A `VirtualMachine` requesting a pool
  address will visibly sit in a pending state for some (hopefully short)
  window — make sure `VirtualMachine.status.conditions` says something
  meaningful during that window (e.g. a distinct `reason` like
  `AwaitingIPAddressClaim`), not just a generic "not ready."
