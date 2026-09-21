# 0053 — CAPI IPAM for pool members: one claim path, not two

- **Status:** Proposed
- **Date:** 2026-09-19
- **Related:** Extends
  [ADR-0033](0033-capi-ipam-pool-integration.md) (CAPI IPAM pool integration)
  to cover `VirtualMachinePool` ([ADR-0046](0046-virtualmachinepool.md)),
  which did not exist when 0033 was written.

## Context

ADR-0033 records how a `VirtualMachine` should obtain an address from a CAPI
IPAM pool: create an `IPAddressClaim`, requeue until `status.addressRef`
resolves, fold the resulting `IPAddress` into the same `StaticIpamConfig`
every other path already produces, and let owner-reference cascade free it.
That design stands and this ADR does not revisit it.

What 0033 could not cover is the pool. `VirtualMachinePool.spec.addressing`
(ADR-0046 Decision 9) hands each member an address from an **inline IPv4
range**, explicitly as the zero-dependency option "until ADR-0033 lands", at
which point it "gains a `poolRef` alternative". This ADR decides what that
alternative actually is.

### What already works, and what does not

`VMClass.spec.network.interfaces[].ipam` is an `IpamShape`, which already
has `pool: Option<PoolIpamConfig>` carrying a `poolRef`. So pool-based IPAM
is *already expressible* — at the class level.

`VirtualMachine.spec.networkOverrides[]` is not. `NetworkInterfaceOverride`
carries `static_: StaticIpamConfig` and nothing else: a per-VM override can
name an address, but cannot name a pool to draw one from.

That asymmetry is the whole problem. A `VirtualMachinePool` addresses its
members by writing `networkOverrides` entries — that is how
`spec.addressing` works today. With the override type unable to express "use
this pool", a pool has no way to say it.

### Why this is not already solved by the class-level field

A pool member is an ordinary `VirtualMachine` using a `VMClass`. So a
`VMClass` whose NIC declares `ipam.pool` would give every member a claim for
free, with no pool-specific code at all, the moment ADR-0033 is implemented.

That is true, and it is genuinely the right answer for some deployments. It
is not sufficient, because a `VMClass` is **shared**. Using it to carry a
pool's address source couples the two: every other VM of that class draws
from the same IPAM pool, and a class cannot be reused across two pools that
must not share a range. The existing `spec.addressing` exists precisely so a
pool can address its members without editing a shared class, and the
`poolRef` alternative has to preserve that property or it is not an
alternative.

## Decision

1. **`NetworkInterfaceOverride` gains `pool`, mirroring `IpamSpec`.**
   `static_` becomes optional alongside it:

   ```rust
   pub struct NetworkInterfaceOverride {
       pub name: String,
       #[serde(rename = "static", default, skip_serializing_if = "Option::is_none")]
       pub static_: Option<StaticIpamConfig>,
       #[serde(default, skip_serializing_if = "Option::is_none")]
       pub pool: Option<PoolIpamConfig>,
   }
   ```

   Additive and backward compatible: relaxing a required field to optional
   accepts every object that validated before. Precedence matches
   `IpamSpec::source()` exactly — `static` > `pool` > DHCP — so there is one
   rule in the codebase, not two.

   This is worth doing on its own merits, pool or no pool: "this VM draws
   from that pool" is a legitimate per-VM statement that the API currently
   cannot make.

2. **`VirtualMachinePool.spec.addressing` gains `poolRef`, exclusive with
   the inline range.** Exactly one of the two must be set:

   ```yaml
   addressing:
     interface: eth0
     poolRef:
       apiGroup: ipam.cluster.x-k8s.io
       kind: InClusterIPPool
       name: sandbox-pool-range
   ```

   The pool stamps `networkOverrides[].pool` on each member instead of
   `networkOverrides[].static`. That is the *only* change to the pool: same
   field, same stamping step, different half of the override.

3. **The pool never creates an `IPAddressClaim`.** Claims are created by the
   `VirtualMachine` reconciler, per ADR-0033, and owned by the member.

   This is the load-bearing decision. The alternative — the pool allocating
   addresses itself and stamping resolved literals, which is what it does
   for the inline range — would put claim creation in two controllers. Two
   things creating claims against the same pool is the two-sources-of-truth
   problem ADR-0033 spends half its Decision section avoiding, and it would
   reintroduce it inside banlieue rather than between banlieue and someone
   else's IPAM.

4. **The pool's address bookkeeping does not apply to the `poolRef` path.**
   With an inline range the planner tracks which addresses are in use and
   holds one until the deleted member's backend VM is really gone (ADR-0046
   Decision 9, and `pool_plan`'s invariant 6). With `poolRef` none of that
   is the pool's business: the claim's owner-reference cascade frees the
   address when the member goes, and the IPAM provider owns the ledger.

   So `PoolInputs` carries no `AddressRange` on this path and the planner's
   `blocked_on_addresses` is always zero. Exhaustion surfaces as members
   stuck waiting on an unresolved claim, reported by the member, not as a
   pool-level capacity condition.

5. **A member waiting on a claim is `Provisioning`, not failed.** Allocation
   is asynchronous. The member sits in `Provisioning` until its address
   resolves, which means `provisioningTimeoutSeconds` is what reaps a member
   whose claim never resolves — the existing poisoned-member path, with no
   new timeout to configure.

   Consequence worth stating: a pool pointed at an exhausted IPAM pool will
   churn — create, wait, reap at timeout, create again. That is visible
   (members appearing and disappearing) but it is not self-explanatory, so
   the member's `Ready=False` message must name the unresolved claim.

6. **`spec.addressing.poolRef` is a `TypedObjectReference`**, the same shape
   `PoolIpamConfig` already uses. banlieue does not care which IPAM provider
   implements the pool kind, and this ADR does not choose one — see below.

## What this ADR does *not* decide

**Which IPAM provider.** ADR-0033 lays out three options (the org's own
system implementing the CAPI provider contract; Metal3 `IPPool` with
`preAllocations`; the in-cluster provider's range pools) and deliberately
stops short of choosing, because "which sub-range gets carved out, and does
that team want to build a CAPI IPAM provider" are decisions outside this
codebase. That is still true and still blocking: **this ADR is implementable
only after that choice is made**, exactly as roadmap 13's first precondition
says.

What this ADR removes is the *banlieue-side* design gap. When the provider
question is answered, there is no longer a second design conversation about
pools.

## Consequences

- **The pool half is small.** One new field on `PoolAddressing`, one
  either/or validation, and a branch in the stamping step choosing which
  half of the override to write. The claim machinery is entirely ADR-0033's,
  written once for `VirtualMachine` and inherited.
- **`NetworkInterfaceOverride` changes shape.** Additive and backward
  compatible, but it is a `VirtualMachine` CRD change, so it needs
  `regen-crds`, examples updated, and the API reference regenerated. Every
  reader of `static_` becomes an `Option` match — a small, mechanical, and
  entirely compile-checked change.
- **Two addressing modes on a pool, permanently.** The inline range does not
  go away: it is the zero-dependency option, and it is what makes a pool
  usable with no IPAM provider installed at all. The either/or validation is
  what keeps "which one is in effect" from ever being ambiguous.
- **A pool can no longer report address exhaustion on the `poolRef` path.**
  `Capacity=False reason=AddressRangeExhausted` is inline-range-only. This is
  correct — the pool genuinely does not know — but it moves a diagnosis from
  one object to another, and the docs must say where it went.
- **Still blocked on the provider decision.** Nothing here can be
  implemented until roadmap 13's first precondition is met. This ADR is
  recorded now so that when it is, the pool question is already answered.
