# 0056 — VirtualMachinePool inline addressing: a list of IPs/ranges, not one range

- **Status:** Accepted
- **Date:** 2026-09-24
- **Related:** Amends `PoolAddressing` from
  [ADR-0046](0046-virtualmachinepool.md) Decision 9. Distinct from
  [ADR-0053](0053-ipam-claims-for-pool-members.md)'s `poolRef` (an external
  CAPI IPAM pool) — this ADR only changes the zero-dependency inline path.

## Context

`VirtualMachinePool.spec.addressing` (ADR-0046 Decision 9) hands each member
an address out of exactly one contiguous `rangeStart`..`rangeEnd` pair. That
is too rigid for the addresses an operator actually has on hand: real
allocations are rarely one clean block. An operator may have a handful of
leftover /32s, two disjoint ranges either side of a block already used by
something else, or want to hand-pin specific addresses (e.g. a jump host)
alongside a range for the rest. A single contiguous range cannot express
any of that; the operator's only recourse today is to over-provision a
bigger contiguous block than they actually have, or run two pools.

MetalLB's `IPAddressPool.spec.addresses` solves the identical problem for
LoadBalancer IPs with one field: a list of entries, each either a single IP,
an inclusive range (`a.b.c.d-a.b.c.e`), or a CIDR. That shape is well known
to anyone who has operated MetalLB and is a strict generalization of what
`rangeStart`/`rangeEnd` already does — a list of one range *is* the old
shape.

## Decision

1. **`PoolAddressing.rangeStart`/`rangeEnd` are replaced by `pool: Vec<String>`.**
   banlieue has no release and no external consumers yet (unreleased, so no
   back-compat shim is owed), so this is a straight rename-and-reshape
   rather than an additive field living alongside the old one.

   ```yaml
   addressing:
     interface: eth0
     pool:
       - 192.0.2.10-192.0.2.29   # inclusive range
       - 192.0.2.40              # a single address
       - 192.0.2.50/31           # CIDR
     prefix: 24
     gateway: 192.0.2.1
   ```

   Each entry is one of:
   - a single IPv4 address (`192.0.2.40`)
   - an inclusive range, low-high (`192.0.2.10-192.0.2.29`)
   - a CIDR block (`192.0.2.50/31`), expanded to every address in the block
     including the network and broadcast addresses — banlieue does not
     assume the block is being used for anything but individual member
     addresses, so there is nothing to reserve

   `prefix`, `gateway`, `nameservers` and `domain` are unchanged: they
   describe the network every address in the pool sits on, which the list
   form does not change.

2. **Allocation walks entries in the order they're written, each low to
   high.** A member gets the first free address found; the planner does not
   sort or merge entries. This keeps the plan deterministic (same inputs,
   same output — the property `pool_plan::plan` already guarantees) and
   lets an operator control which pocket of addresses gets used first by
   ordering the list, e.g. exhausting a small range of spares before
   drawing from a larger one.

3. **`pool_plan::PoolInputs.address_range: Option<AddressRange>` becomes
   `address_ranges: Vec<AddressRange>`.** `AddressRange` itself (a `start`,
   `end` pair) is unchanged — the reconciler parses each `pool` string into
   one `AddressRange` before handing the list to the planner, so `plan()`
   still only ever deals in resolved integer ranges and stays free of string
   parsing, per its existing "pure planning logic, no I/O" charter. An empty
   `Vec` means what `None` used to: DHCP or class-level IPAM, no addressing.

4. **An unparseable entry fails the whole pool**, the same way an
   unparseable `rangeStart`/`rangeEnd` already did: `pool_inputs()` returns
   `Err`, the reconciler reports it and requeues, and no members are
   created or deleted until it is fixed. Partial application of a
   partially-valid list would silently under-provision.

5. **Exhaustion across the whole list is still one condition.** `Capacity`
   `AddressRangeExhausted` — unchanged name and meaning, now computed over
   every entry's remaining capacity together rather than one range's. An
   operator does not need to know which entry ran dry, only that the pool
   did.

## Consequences

- **Strictly more expressive, same mental model.** A pool with one range in
  the list behaves exactly as before; `rangeStart: X, rangeEnd: Y` becomes
  `pool: ["X-Y"]`. There is no new addressing *concept*, only a list instead
  of a pair.
- **CRD change.** `PoolAddressing`'s shape changes, so this needs
  `regen-crds`, an updated example, and the API reference regenerated.
  Every reader of `range_start`/`range_end` in Rust becomes a reader of
  `pool: Vec<String>` — mechanical, compile-checked.
- **String parsing moves to the reconciler, not the CRD schema.** The
  apiserver cannot validate that `"192.0.2.10-192.0.2.29"` is well-formed at
  admission time (no CEL rule is written for it here); a malformed entry is
  caught at reconcile time and surfaced as a stuck pool with a message
  naming the bad entry, same as today's parse failures.
- **No change to who allocates.** This ADR only reshapes the *set* of
  candidate addresses the existing planner draws from. Ownership, the
  "held until the backend VM is really gone" invariant (ADR-0046 pool_plan
  invariant 6), and the poolRef/claim-based alternative (ADR-0053) are all
  untouched.
