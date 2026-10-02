<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0083: Block a `VirtualMachine` whose static address another VM already claims

- **Status:** Accepted
- **Date:** 2026-10-01
- **Proposed:** 2026-09-30
- **Deciders:** Daniel Guns
- **Related:** [#49](https://github.com/firestoned/banlieue/issues/49);
  constrains [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)
  (`networkOverrides`) and [ADR-0056](0056-vmpool-address-pool-entries.md)
  (pool-stamped addresses); same reconcile-time shape as
  [ADR-0048](0048-tpm-enabled-requires-deferred-install.md)

## Context

A `VirtualMachine` gets a static address through
`spec.networkOverrides[].static.address` (ADR-0024), either written by a user
or stamped by a `VirtualMachinePool` from its `addressing.pool` (ADR-0056).
Nothing checked that the address was free. Two VMs naming `192.0.2.10` on the
same network both scheduled, both got an infra CR, and both booted with the
same address: ARP flapping, connections landing on whichever guest answered
last, and both VMs `Ready`.

Issue #49 asks for creation to fail with a clear status instead, and names two
independent checks:

1. **Another `VirtualMachine` claims the address.** Answerable from the
   Kubernetes API alone.
2. **The backend already has something at that address**, which catches
   writers that are not banlieue. This needs a per-provider lookup.

This ADR decides the first check. The second is deferred (see
[Not decided here](#not-decided-here)).

**The scope is banlieue's own `VirtualMachine`s.** The check runs entirely in
`banlieue-controller` against the API server. It never asks a hypervisor, an
IPAM or the network. A VM created directly in vCenter, a machine managed by
another tool, or a physical host on the same subnet is invisible to it.

### Why not an admission policy

A `ValidatingAdmissionPolicy` (ADR-0007) sees one object. It cannot list other
`VirtualMachine`s, and it cannot resolve a `VMClass` to learn which
`networkClass` an override's interface sits on. As with ADR-0048, the
controller is the only component that holds the whole graph.

### Why "same `networkClass`", not "same address anywhere"

The same address on two isolated networks is not a conflict. Two labs that
both use `10.0.0.0/24` on different bridges or port groups are a normal
deployment. `networkClass` is the finest network identity the controller has
without asking a provider, so it is the key.

### Why incumbency is recorded, not observed

A first implementation decided "who already has the address" from
`status.addresses`. Review found three problems with that, all from the same
root:

- it is empty while a libvirt guest boots, so a reboot briefly made the
  holder look free and let an older claimant take the address;
- vSphere publishes no addresses at all, so incumbency never applied there;
- it is reported by the guest, so a hostile guest could claim an address it
  was never given.

Incumbency has to come from something the controller itself wrote and that
does not change when a guest reboots.

## Decision

**1. The check runs in `banlieue-controller`'s `VirtualMachine` reconcile**,
after `VMClass` and `VMImage` resolve and after the ADR-0048 pairing check,
*before* scheduling. A VM with no static override is never checked and never
blocked: the address is not its choice.

**2. The decision is a pure function**,
`reconciler::address_conflict::find_duplicate_address`, unit-tested without a
cluster. The reconcile gathers the inputs and acts on the answer.

**3. A claim is a `(networkClass, address)` pair, of two kinds.**

- **Declared:** a `spec.networkOverrides[]` entry whose `name` matches an
  interface in the VM's `VMClass`, scoped by that interface's `networkClass`.
  An override naming no class interface is ignored, as every infra builder
  ignores it. A claimant whose `VMClass` cannot be resolved claims its
  addresses on every `networkClass` (fail closed).
- **Held:** an entry of the new `status.heldAddresses`
  (`interface`, `networkClass`, `address`). The controller writes it **only**
  when it applies the VM's infra CR, from the claims it applied, and carries
  it forward unchanged on every other status write. It therefore records what
  the guest's infra CR is configured with, survives reboots, is the same on
  every backend, and is never written by a guest.

Addresses are compared as parsed IPs where they parse, and as trimmed strings
where they don't. `status.addresses` (observed, guest-reported) plays no part.

**4. One global order, decided greedily.** Over the VMs that share an address
with this one (transitively), the controller computes:

1. **Held addresses are reserved first.** A held claim is never given to
   another VM. If two VMs hold the same claim (possible only for duplicates
   that predate this ADR, or a lost race), the older keeps it and the other
   is blocked.
2. **Then every VM, oldest first** (`creationTimestamp`, then
   `namespace/name`), is admitted if none of its declared claims matches a
   claim already reserved by another VM, and its declared claims are then
   reserved. Otherwise it is blocked, naming the VM that reserved the claim.

A blocked VM reserves nothing beyond what it already holds, so it cannot block
a third VM on its other addresses. A single order across all VMs means two VMs
with several shared addresses cannot block each other in a cycle. Every VM
computes the same result, so of two VMs racing for one address exactly one is
admitted.

**5. Scope is every `VirtualMachine` the controller watches**: cluster-wide
for a cluster-scoped install, the watched namespace for a namespaced one. The
check reads the controller's existing `VirtualMachine` reflector store, not
the API, and GETs only the `VMClass`es of the few VMs that share an address.
The ClusterRole already grants `get` on `vmclasses`.

**6. A blocked VM that has never been provisioned** gets `Ready=False`, reason
`DuplicateAddress`, and no infra CR. `Scheduled` is not touched, because the
VM never reached the scheduler (as in ADR-0048).

**7. A blocked VM that is already provisioned is not frozen.** Its infra CR is
not re-applied (that would push the contested address), but its status is
still mirrored from the infra CR and `spec.desiredPowerState` is still applied
to it with a merge patch of that one field, so an operator can power it off.
It reports `Ready=False reason=DuplicateAddress`, and the message says that
spec changes other than power are withheld. This is what happens to the newer
of two VMs that already shared an address before upgrade, and to a running VM
edited onto an address another VM holds.

**8. The message names the holder as `namespace/name` only within the blocked
VM's own namespace.** Otherwise it says "a VirtualMachine in another
namespace".

**9. Recovery is by requeue, at the default interval (30s).** An event-driven
wake-up would need a second, unfiltered `VirtualMachine` watch, because
kube-runtime's shared-stream subscribers receive `Apply` events but never
`Delete` (4.2, `reflector/store.rs::dispatch_event`), and the holder's
deletion is the event that matters. A requeue against an in-memory store
costs no API calls, so the blocked VM rechecks cheaply and proceeds within one
interval of the holder going away.

## Not decided here

- **The provider-side lookup** (issue #49's second check). It is the only way
  to catch a writer that is not banlieue, and it belongs in each provider,
  reporting through the infra CR. It is deferred because no backend was
  available to verify it against, and a lookup built against an unverified
  model of a vSphere or Proxmox API would only look like a control.
- **DHCP-assigned addresses.** A DHCP VM does not declare its address, and
  the only record of it is guest-reported. Keep static addresses outside DHCP
  scopes.
- **CAPI IPAM (`pool`) addresses** (ADR-0033/0053). The IPAM provider
  allocates those and is responsible for their uniqueness within its pool.

## Consequences

**A silent network outage becomes a visible `Ready=False`.** The later claimant
never provisions and says why. This is a behaviour change. If two VMs already
share an address at upgrade, the older keeps it and the newer is flagged; its
guest keeps running on the address until an operator acts, and its power
state can still be changed.

**`VirtualMachine.status` gains `heldAddresses`.** A CRD change, additive and
controller-written. Like every status field it can be forged by a principal
with `update` on `virtualmachines/status`, which is already
infrastructure-admin-equivalent.

**First come, first served.** Any principal who can create a `VirtualMachine`
on a `networkClass` can reserve an address on it before someone else does.
That is the network's own behaviour made visible, recorded as an accepted risk
in the threat model.

**A pool whose range overlaps a standalone VM churns.** The pool allocator
(ADR-0056) only avoids addresses its *own* members hold. A member stamped with
an address a standalone VM holds is blocked, reaped at
`provisioningTimeoutSeconds`, and the address can be drawn again. Visible and
bounded. The follow-up is to feed foreign claims into the pool planner.

**`networkClass` is the limit of what is detected.** Two `networkClass`es that
map to the same physical segment can still collide undetected. Only the
deferred provider-side lookup can close that.

**Cost.** No API LISTs. One in-memory pass over the VM store per reconcile of
a VM with a static override, plus one `VMClass` GET per distinct class among
the VMs sharing its addresses (usually none beyond its own).
