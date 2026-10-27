<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0103: Isolated sandbox network, with egress through proxy VMs

- **Status:** Proposed
- **Date:** 2026-10-20
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0030](0030-per-zone-capability-targets.md) (per-zone
  network targets), [ADR-0032](0032-per-zone-network-subnet-shape.md)
  (per-zone subnet shape), [ADR-0046](0046-virtualmachinepool.md)
  (`VirtualMachinePool`), [ADR-0056](0056-vmpool-address-pool-entries.md) (pool
  address entries and draw order), [ADR-0082](0082-no-inbound-to-sandboxes.md) (no
  inbound to sandboxes; its *What this means for banlieue* section binds
  here), [ADR-0083](0083-block-duplicate-static-address.md) (duplicate static
  addresses are blocked), [ADR-0097](0097-api-ends-at-virtualmachineclaim.md)
  (Decision 5, the scope test),
  [ADR-0100](0100-sandbox-guest-software-is-consumer-image-content.md)
  (sandbox software is image content), roadmap 19 (`VMNetwork`, which will
  take over creating the port groups this ADR uses by hand).

## Context

Agent sandboxes are many, small and short-lived. Each one needs an address,
and on a routable corporate network that address comes out of an allocation
that a sandbox fleet will exhaust long before it exhausts CPU or memory.

The sandboxes do not need to be reachable from the corporate network, and
under ADR-0082 they must not be. They need the opposite: a path **out**, to
a small set of destinations, that does not give each sandbox a routable
address of its own.

Four facts shape the design. The first three were confirmed against a real
vSphere 8.0 cluster; the fourth is read from the tree.

- **vCenter allocates no addresses.** It attaches virtual NICs to layer 2
  port groups. An address range with no gateway on a port group of its own
  costs nothing to create and consumes nothing from the corporate
  allocation.
- **A port group belongs to one distributed switch.** A cluster, or a set
  of clusters, on separate switches needs one port group per switch, all on
  the same VLAN ID if the segment is to span them.
- **vCenter can create a VLAN but cannot carry it.** A new port group on a
  VLAN the physical switch ports do not trunk passes traffic between two VMs
  on the same host (the distributed switch forwards it in memory) and drops
  it between hosts. Nothing in vCenter reports the difference unless the
  switch's VLAN health check is on. Every host in a zone must be tested,
  not one pair.
- **banlieue already covers the VM side.** `VMClass` takes several network
  interfaces (ADR-0031's multi-NIC template, `NetworkInterfaceSpec`),
  `VirtualMachinePool.spec.addressing` draws one static address per member
  from an inline range (ADR-0056), and the vSphere provider resolves a class
  to a literal port group per zone (ADR-0030). One gap is real: the vSphere
  provider writes only the **first** static NIC into `guestinfo.network.*`
  (`build_guestinfo`), so a VM cannot receive two static addresses that way.

### Alternatives considered

- **A routed RFC 1918 range for sandboxes.** Reintroduces the allocation
  problem under a different name, and a router makes ADR-0082's "no
  inbound" a firewall rule instead of a topological fact.
- **A public-space /19 such as `100.0.0.0/19` or `100.1.0.0/19`.** Valid
  while the segment is isolated, and a collision the day anything routes it:
  the sandboxes could never reach whoever really holds those addresses.
  `100.64.0.0/10` is RFC 6598 shared address space, reserved so it is never
  globally assigned.
- **NSX or another overlay.** Not present in the target environment, and
  banlieue would gain a backend-specific dependency for a problem plain VLANs
  solve.
- **One shared segment for every environment.** Rejected: a dev sandbox and
  a regulated one would share a broadcast domain and a proxy set.

## Decision

1. **The sandbox segment is layer 2 only.** One `/19` from `100.64.0.0/10`
   per environment (for example `100.64.0.0/19` for one and `100.65.0.0/19`
   for the next), each with its own VLAN ID. No SVI, no gateway and no route
   to or from the corporate network is ever configured. A sandbox can reach
   exactly two things: other members of its own segment, and the proxy VMs.
   A `/19` is 8,190 usable addresses; the proxies take a handful and a pool
   draws the rest.
2. **The segment is a literal network class target, for now.** The port
   groups are created once, by hand, one per distributed switch, and named
   in `Provider.spec.capabilities.networkClasses[].target` (and in
   `perZone` where the names differ, ADR-0030). banlieue creates, changes and
   deletes no port group under this ADR. Roadmap 19's `VMNetwork` replaces
   the hand-made object later and the literal target form stays supported
   for brownfield, so nothing here is thrown away.
3. **A zone is listed only after its VLAN is verified across every host.**
   A `perZone` entry for the sandbox class is added once a two-host test
   passes for the hosts the zone's VMs can land on, because ADR-0019's
   reachability reports that a port group of that *name* exists, not that
   its VLAN is trunked (roadmap 19 records the same gap).
4. **Egress is application-layer, through three proxy VMs per environment.**
   Each proxy is an ordinary `VirtualMachine` with two interfaces:
   - `egress` on an existing routable network class, static address, the
     default route;
   - `sandbox` on the segment, a fixed address at the low end of the range.

   IP forwarding is **off**. A proxy terminates the sandbox's connection and
   opens a new one from its own routable address, so sandbox addresses never
   appear on the corporate network and no NAT or route exists. Two services
   run on the `sandbox` interface only, never on `egress`: a TCP passthrough
   keyed on SNI (haproxy) and an HTTP forward proxy for tools that need one
   (`apt`, `pip`). Destinations are a default-deny allowlist held in the
   proxy image. Protocols that are neither (raw UDP, ICMP) are not carried.
5. **The proxies are image content, not a banlieue kind (ADR-0100,
   ADR-0097 Decision 5).** The proxy software and its allowlist live in a
   `VMImage` and its cloud-config layers, owned by whoever runs the
   environment. banlieue gains no CRD and no controller for them. The scope
   test is met by *not* adding a kind: nothing about a proxy needs banlieue
   to know it is one.
6. **The proxy's second address is set by the image, not by banlieue.**
   Because only the first static NIC rides `guestinfo.network.*`, the
   `egress` NIC is first in the class (so it carries the static address and
   the default route) and the `sandbox` NIC's fixed address is written by a
   cloud-config layer. Three proxies with fixed addresses make this a
   per-image constant, not a per-VM value. Carrying several static NICs in
   `guestinfo` is a separate change and is not made here.
7. **Sandbox VMs have one NIC, on the segment, and no default route.**
   Addresses come from `VirtualMachinePool.spec.addressing` over the range
   minus the proxies' addresses, sized at `maxReplicas` plus spares
   (ADR-0056). `gateway` is omitted. Clients reach the proxies by address.
   Name resolution happens at the proxy, so the segment needs no resolver.
8. **No inbound path, per ADR-0082.** Sandbox images ship without sshd. The
   proxies accept no connection on `egress` and are not a jump host. The
   only way into a sandbox is the hypervisor console, protected by RBAC.
   Enforcement that holds against guest root sits outside the guest: the
   segment has no router, so there is no path to filter, and the proxy's
   listen address is on the segment only.
9. **One environment, one segment, one proxy set.** Proxies are never
   shared across environments.

## Consequences

- **No Rust change is required.** Everything above uses fields that exist
  today. The deliverables are the CALM model, an `examples/` set (two
  network classes, a two-NIC proxy class, a sandbox class and pool), a
  guide under `docs/src/`, the threat-model pass, and a live test.
- **Sandboxes consume no corporate addresses.** An environment costs three
  routable addresses, one per proxy, however many sandboxes run.
- **The proxies are a chokepoint.** Three give redundancy, but a sandbox's
  proxy choice (a list in its image, a name resolving to all three, or a
  VRRP address) is undecided below. If all three are down, every sandbox in
  the environment loses egress.
- **The proxies are the one component on both sides of a trust boundary.**
  They hold a routable address and an interface in the most hostile segment
  in the system. That is a new boundary for the threat model, and the proxy
  image's hardening (no forwarding, listen address, allowlist, no other
  services) is now a security control that must exist before this counts as
  implemented.
- **An operational dependency exists outside banlieue.** The VLAN must be
  trunked on the physical switch ports of every host. A network team owns
  that, and without it the segment silently works on one host only.
- **This rules out** SSH access through a proxy (a jump host contradicts
  ADR-0082), a routed sandbox network, and generic outbound protocols.
- **DHCP on the segment is not proposed.** Static addressing from the pool
  is what ADR-0056 already provides.

### Still to decide

- How a sandbox picks among three proxies: an address list in the image, one
  name resolving to all three (which needs a resolver after all), or a
  floating address between them.
- Who authors and reviews the proxy allowlist, and how a change reaches
  running proxies without rebuilding the image.
- Whether the `guestinfo` convention should carry several static NICs, which
  would let banlieue, rather than the image, address a proxy's second NIC.
- When `VMNetwork` (roadmap 19) replaces the hand-made port groups, and how
  an existing literal target migrates.
- Whether `VMClass` interface order, which this ADR now leans on for the
  first-static-NIC rule, should be made an explicit contract.
