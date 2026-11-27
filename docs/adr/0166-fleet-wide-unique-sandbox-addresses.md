<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0166: Sandbox addresses are private and unique across the fleet, carved from one recorded supernet and checked against it

- **Status:** Accepted
- **Date:** 2026-11-27
- **Proposed:** 2026-11-19
- **Notes:** Not implemented.
- **Renumbered:** 2026-11-25. Written as 0145 on 2026-11-19; `main` took 0145 for "controllers audit their own progress and fail liveness when objects go unreconciled" first, so this is 0166 (the next free number; 0165 was the highest)
- **Deciders:** Erick Bourgeois
- **Related:** Enforces the amended Decision 2 of
  [ADR-0144](0144-sandbox-segment-and-egress-gateways-on-a-cloud-hypervisor-host.md)
  (sandbox addresses unique across the fleet); extends
  [ADR-0083](0083-block-duplicate-static-address.md) (its key is a
  `networkClass`, which is the wrong key for a fleet);
  uses [ADR-0056](0056-vmpool-address-pool-entries.md) (inline pool entries)
  and, later, [ADR-0053](0053-ipam-claims-for-pool-members.md) (claims from an
  IPAM pool, blocked by roadmap 13); respects
  [ADR-0123](0123-provider-side-duplicate-address-check.md) (Rejected: banlieue
  does not probe the network); [ADR-0114](0114-static-addressing-in-the-nocloud-seed.md)
  (static addressing reaches the guest); [ADR-0137](0137-firecracker-provider-class.md)
  (Firecracker hosts); [ADR-0007](0007-admission-policies.md)-style
  admission (CEL, no webhook).

## Context

ADR-0144 Decision 2 now says: no two sandboxes anywhere hold the same address,
no host reuses another host's sandbox range, a host's range is carved from one
fleet-wide private supernet and recorded (never chosen per host, never
defaulted), the pool draws from that carve-out, and the gateways' sandbox-side
addresses come from the same host range. Only sandbox addresses must be
unique; the uplink segment and a host's own bridge addresses may repeat.
That is a rule. This ADR decides how it is recorded and enforced.

What exists today, read from the tree:

- **A pool's addresses are whatever the operator typed.**
  `VirtualMachinePool.spec.addressing.pool` is a list of addresses, ranges and
  CIDRs (ADR-0056). Nothing relates it to a host, a Provider or another pool.
  The vSphere sandbox pools document "pools do not share an allocator; never
  overlap another pool on the same segment" as an operator convention. Their
  segments are one /19 per availability zone inside one
  private range, each on its own port group, so they are
  unique today because a person kept them so.
- **ADR-0083 cannot see the fleet.** It blocks a duplicate `(networkClass,
  address)` pair, and rightly: the same address on two isolated networks is
  normal. Two Cloud Hypervisor hosts each name their sandbox class `sandbox`
  today, but nothing forces the names to match, and a class on one host is a
  different network from a class on another. A sandbox range that repeats
  across hosts under different class names is invisible to it.
- **`Provider.spec.capabilities.networkClasses[]` carries a name, a backend
  target and a `subnet` (`gateway`, `nameservers`, `domain`), and no range.**
  ADR-0062 Decision 4 keeps host paths and bridges out of the cluster; the
  carve-out is not a host path, it is an address plan, and it must be visible
  to the controller.
- **IPAM claims (ADR-0053) are not available.** Roadmap 13 is not started: the
  choice of IPAM provider is open. Whatever this ADR decides must work on the
  inline path (ADR-0056) and move to claims later without changing the rule.
- **A cross-object rule cannot be a ValidatingAdmissionPolicy alone.** CEL sees
  one object (ADR-0083 makes the same point). Admission can check one
  Provider against a parameter; only the controller sees every Provider.

### The hard case

The two existing lab Cloud Hypervisor hosts each number their lab bridge from
the same private range (placeholder: `192.168.0.0/24` on both). The vSphere
sandbox pools use a second range (placeholder: `198.18.0.0/19`, one /19 per
zone). The supernet must overlap neither, and the lab range must never be
mistaken for sandbox space just because a host also has a bridge numbered from
it. "Unique across the fleet" has to mean unique against everything banlieue
knows is routed or bridged anywhere, not only against other sandbox ranges.

## Decision

Placeholders below (`10.0.0.0/16` supernet, `192.0.2.0/24`, `198.51.100.0/24`,
`203.0.113.0/24` carve-outs) stand for real values that live only in the
operator's cluster.

### 1. One cluster-scoped object records the supernet: `SandboxAddressSpace`

A new cluster-scoped, admin-owned, singleton-by-convention CRD (name it in the
ADR's implementation; shape below):

```yaml
apiVersion: banlieue.io/v1alpha1
kind: SandboxAddressSpace
metadata: { name: fleet }
spec:
  supernets: ["10.0.0.0/16"]          # sandbox addresses live only here
  reserved:                            # ranges that must never be carved
    - { cidr: "192.168.0.0/24", reason: lab bridges }
    - { cidr: "198.18.0.0/19",  reason: vSphere sandbox zones }
  hostPrefix: 24                       # size of every carve-out
```

The supernet, the reserved ranges (the lab bridge range and the vSphere
sandbox ranges of the hard case) and the carve-out size live in one object so
that "what is the fleet's sandbox space" has one answer, readable with
`kubectl get`, versioned in GitOps with the rest of the platform. A ConfigMap
would hold the same data with no schema, no `status` and nothing for admission
to bind to; an annotation per Provider would scatter the supernet across the
hosts it is meant to constrain. `reserved` is explicit (Non-Negotiable 4): the
controller never discovers ranges to avoid; the operator lists them.

Rejected: a Provider field alone (no place for the supernet, so each host
could pick its own "supernet"); the IPAM pool object (ADR-0053's `poolRef`
kinds are not chosen and roadmap 13 is open).

### 2. Each host's carve-out is a field on its Provider's sandbox network class

`NetworkClassMapping` gains an optional `sandbox` block, set on the class the
sandboxes use and on the gateways' `sandbox-gw` class (same bridge, ADR-0144
Decision 3):

```yaml
networkClasses:
  - name: sandbox
    sandbox: { range: "203.0.113.0/24" }   # from SandboxAddressSpace, recorded
    subnet: { gateway: "203.0.113.1" }
```

A class without `sandbox` is not a sandbox class and is outside this ADR; a
class with it is subject to every rule below. No default, no inference from the
class name (ADR-0144 Decision 3 makes the same choice for isolation). The
Provider is the right owner: one CH or FC host is one Provider, so "a host's
range" is one field on one object, and retiring the host retires the range with
it (Decision 7).

### 3. The pool draws from the carve-out; the class pins it

On the inline path (now), a pool's `addressing.pool` entries for a sandbox
interface must each lie inside the carve-out of the Provider its members land
on. The pool targets one host through `providerSelector` on a label unique to
the host (ADR-0144 Decision 4), so the pair is knowable. The controller, which
already resolves a member's `VMClass` interface to a `networkClass` and the
Provider's mapping, checks the stamped address against `range` before it applies
the infra CR. Outside the range: the VM is blocked with `Ready=False`, reason
`AddressOutsideHostRange`, naming the range, as ADR-0083 blocks a duplicate.
A sandbox on host A therefore cannot be given an address from host B's range,
by a mistyped pool, a hand-written override or a pool whose selector later
moves.

The gateways' sandbox-side addresses are ordinary `networkOverrides` on the
gateway VM and are checked by the same rule: inside the same host range, and
(Decision 4) unique against every sandbox.

When roadmap 13 lands, the same carve-out becomes one IPAM pool per host
(`poolRef`, ADR-0053), created from `range`, and the pool-membership check
stays as the second layer. Nothing in this ADR changes the rule then, only who
hands out the address.

### 4. Uniqueness is enforced at three layers, with a fleet-wide key

The key for a sandbox class is **the address alone**, not `(networkClass,
address)`: a sandbox address is unique across every class that carries `sandbox`.
ADR-0083's function (`find_duplicate_address`) takes a key function; the
sandbox case supplies a constant class. Held addresses (`status.heldAddresses`)
already survive reboots and need no change.

| # | Layer | Where | What it decides |
| - | ----- | ----- | --------------- |
| 1 | Admission, one object | `ValidatingAdmissionPolicy` on `Provider`, parameter `SandboxAddressSpace/fleet` | `range` is a CIDR of size `hostPrefix`, inside a supernet, overlapping no `reserved` entry; `range` is immutable once set (Decision 6) |
| 2 | Controller, many objects | Provider reconcile | no two Providers' `range` overlap; sets `Ready=False`, reason `SandboxRangeOverlap`, on the later one; a Provider in that state is not scheduled onto |
| 3 | Controller, per VM | VM reconcile, extending ADR-0083 | stamped address inside the host's `range` (Decision 3); no other VM, on any sandbox class, holds the same address |

Layer 1 alone cannot see another Provider; layer 2 alone cannot stop a bad
write at the door; layer 3 catches every address that reaches a guest whatever
produced it (pool, override, claim, gateway). Each blocks with a reason an
operator can read. The ARP probe of ADR-0123 stays rejected: this guarantee is
about banlieue's own records, stated as such.

### 5. The hard case is handled by `reserved`, and the supernet is not dual-use

The lab bridge range and the vSphere sandbox ranges are listed in `reserved`,
so layer 1 refuses any carve-out that touches them and layer 3 treats an
address in a reserved range as a sandbox address on no host. The supernet must
be chosen outside both: that is the operator's one-time decision and is the
first open question below. Existing vSphere sandbox ranges are not moved by
this ADR; they are adopted as reserved. Whether they later become carve-outs
of the supernet is a migration, not a prerequisite.

### 6. A carve-out is immutable and allocated in order; never reused while live

- `range` on a sandbox class is immutable once the Provider has Ready sandboxes
  (admission, layer 1). Re-carving a host is a new Provider or a drained one.
- Carve-outs are cut from the supernet in address order; the controller
  publishes the next free `hostPrefix` block in `SandboxAddressSpace.status`
  (`nextFree`, `allocated[]` by Provider name) as information. It never
  assigns one: the operator copies it into the Provider (ADR-0144: recorded,
  never chosen by the host).

### 7. Retirement does not free a range at once

Deleting a Provider moves its `range` to `status.retired[]` with the date, and
layer 1 refuses a carve-out that overlaps `retired` until the entry is removed
by a person. Reuse is therefore an explicit act, after the operator has
confirmed that no guest, gateway config, allow-list or log pipeline still names
the old addresses. Banlieue cannot know that, so it does not guess.

## Consequences

- **The rule becomes a refusal.** A repeated or out-of-range address stops at a
  Provider write (layer 1), at the Provider reconcile (layer 2) or on the VM
  (layer 3), each with a named condition, instead of surviving until two
  sandboxes answer for one address.
- **One new CRD and one new field.** `SandboxAddressSpace` and
  `NetworkClassMapping.sandbox`: `regen-crds`, examples, one admission policy,
  a controller extension of ADR-0083's key. CALM gains the object and its
  relationship to Provider; the threat model gains it as an admin-owned address
  plan (a writer who widens `reserved` or `supernets` changes who can be
  reached from a sandbox segment, so it is cluster-admin only, like
  `ProviderClass`).
- **The operator still allocates.** Nothing hands out carve-outs; `status`
  only suggests the next free one. That keeps ADR-0144's "never chosen per
  host" true at the cost of one manual copy per host.
- **Exhaustion is per host and visible.** A `hostPrefix` of `/24` gives about
  250 sandboxes and two gateways per host; a pool larger than the carve-out
  reports `AddressRangeExhausted` as it does today (ADR-0056 Decision 5).
- **Forward path to IPAM.** The inline check is the interim; claims per host
  (ADR-0053) replace the hand-listed pool entries without changing layers 1 to
  3.
- **Not covered:** a machine outside banlieue using a sandbox address
  (ADR-0123's boundary stands); a sandbox that spoofs another's address on the
  segment (ADR-0144 open question, per-sandbox pinning); IPv6 (disabled on
  both bridges, ADR-0144 Decision 6).

### Layers, and what each one is worth

| layer | stops | state | cost |
| ----- | ----- | ----- | ---- |
| Operator convention (today) | overlap, if the person remembers | exists; the vSphere pool docs say it | none; fails silently on the first typo |
| Per-host carve-out on `Provider` (this ADR) | a host choosing its own range | missing | one field; one manual copy per host |
| `SandboxAddressSpace` + admission, layer 1 | carve-out outside supernet, in `reserved`, wrong size, edited live | missing | one CRD, one CEL policy (K8s 1.30+, already required) |
| Provider-overlap check, layer 2 | two hosts with one range, whatever the class names | missing | one reconcile pass over Providers; cheap, list from the reflector |
| Address-in-range and fleet-wide duplicate, layer 3 | a bad stamp or hand override; a duplicate across hosts | partly (ADR-0083, per `networkClass`) | key change in `address_conflict.rs`; one range lookup per VM |
| Retired-range hold | reuse of a range still named somewhere | missing | one status list; reuse needs a human |
| IPAM claims per host (ADR-0053) | a pool listing addresses by hand | blocked (roadmap 13) | an IPAM provider and a decision on which |
| Network probe (ADR-0123) | an address in use outside banlieue | Rejected | stays rejected |
| Per-sandbox pinning (ADR-0144 open) | address spoofing on the segment | not decided | provider or gateway work |

## Open questions

- **The supernet itself.** Which private range, sized for how many hosts
  (`hostPrefix` times hosts). It must avoid the lab bridge range and the
  vSphere sandbox ranges (Decision 5). Operator decision; the ADR cannot make
  it, and it must never be written into this repository.
- **Adopt or migrate the vSphere sandbox ranges?** Reserve them (this ADR) or
  later renumber them into carve-outs of the supernet. Reserving costs
  nothing; renumbering touches every gateway's allow-list.
- **Is `/24` the right `hostPrefix`**, or does a host need a larger block for
  its gateways and sandboxes together?
- **A new CRD, or a Provider-only design?** The alternative is no
  `SandboxAddressSpace`: the supernet in an admission-policy parameter
  ConfigMap and layers 1 and 2 only. Smaller, but nothing observable and no
  `retired` ledger. Recommended: the CRD.
- **Should layer 3 also cover non-sandbox classes** (fleet-wide uniqueness for
  everything)? No: ADR-0083's per-class key is correct for them, and ADR-0144
  allows repeats on the uplink and host bridges.
