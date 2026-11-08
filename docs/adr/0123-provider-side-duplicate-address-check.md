<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0123: Providers check a static address against their backend before creating a machine

- **Status:** Proposed
- **Date:** 2026-11-08
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0083](0083-block-duplicate-static-address.md)
  (its "Not decided here": the provider-side lookup);
  [#157](https://github.com/firestoned/banlieue/issues/157). Constrained by
  [ADR-0050](0050-libvirtmachine-domain-lifecycle.md) Decision 8 (what
  libvirt's address sources can see) and
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md) (the Cloud
  Hypervisor provider runs on the host and owns its bridges' taps).
  ADR-0116 to ADR-0122 are reserved by roadmap 20.

## Context

ADR-0083 blocks a `VirtualMachine` whose static address another banlieue VM
on the same `networkClass` already claims. It sees only banlieue's own
objects. A static address that a machine banlieue did not create is already
using (a router, a hand-made VM, a forgotten appliance) still reaches the
guest, and two hosts then answer for one address: the same silent outage
ADR-0083 exists to prevent, from the half it left open.

ADR-0083 deferred the provider-side lookup because no backend was available
to verify it against. That has changed for libvirt and Cloud Hypervisor, and
verifying them showed that "ask the backend" means something different on
each:

| Backend | What it can see of other machines' addresses | Limits found |
| --- | --- | --- |
| Cloud Hypervisor | everything on the bridge it serves: it is on the host, and can probe the segment itself | none beyond the segment's own reach |
| libvirt | its network's own addresses and DHCP range, the network's DHCP leases, and the guest agent of each domain on the host | the ARP source misses most guests on a host with a large neighbour table, and every guest in `DELAY` (ADR-0050 Decision 8); a static guest with no agent is seen by nothing |
| vSphere | every VM's VMware Tools-reported addresses (`guest.net`), banlieue's or not | a VM without Tools reports nothing |
| Proxmox | each VM's guest agent (`network-get-interfaces`) | a VM without the agent reports nothing |

Only an active probe answers "is anything using this address" rather than
"did something tell us it is". The others are a lookup in somebody's
records.

## Decision

**1. Each provider checks every static address of a new machine before it
creates the backend machine**, in the infra CR's reconcile, on the create
path only. A machine that already exists is never re-checked: its address is
its own by then, and ADR-0083's `heldAddresses` protects it.

**2. What "check" means is the provider's, by backend:**

- **Cloud Hypervisor: an RFC 5227 probe.** ARP requests for the address,
  sender address `0.0.0.0`, on the network class's bridge, over the probe
  window RFC 5227 specifies (Open question 2); any ARP reply or conflicting request for that address means
  it is in use. Plus the host's own addresses on that bridge.
- **libvirt: the records it has.** The libvirt network's own addresses, its
  DHCP leases, and the addresses the guest agents of the host's other
  domains report. Its ARP source is not used (ADR-0050 Decision 8). A static
  address inside the network's DHCP range is reported as a conflict waiting
  to happen, not as in use.
- **vSphere: Tools-reported addresses** of every VM the provider's account
  can read, from one property-collector query.
- **Proxmox: guest-agent addresses** of the cluster's running VMs.

**3. In use blocks the machine.** The infra CR gets `Ready=False`, reason
`AddressInUse`, naming the address and what observed it (`arpProbe`,
`dhcpLease`, `guestAgent`, `vmwareTools`, `hostAddress`), and the backend
machine is not created. The controller mirrors it onto the
`VirtualMachine`, as it does every infra condition. The reconcile requeues
and checks again, so the machine proceeds once the address is freed.

**4. A check that could not look says so.** A new infra condition
`AddressChecked`: `True` when every source the backend has answered and
none saw the address; `Unknown` with the sources that could not answer (no
agent on a domain, a probe that could not open its socket); never `False`
(that is `Ready`'s `AddressInUse`). `Unknown` does not block: the operator
sees that the check was partial instead of reading silence as "free".

**5. ADR-0083 stays first.** The controller's check runs before scheduling
and costs nothing; this one runs after placement, on one backend, and only
catches what the controller cannot see.

## Consequences

**A static address already in use stops a machine before it exists**, on
every backend, with the evidence named. On Cloud Hypervisor the probe sees
anything on the segment that answers ARP, banlieue's or not.

**The Cloud Hypervisor provider needs `CAP_NET_RAW`** to send ARP on the
bridge. Its unit grants `CAP_NET_ADMIN`, `CAP_CHOWN` and `CAP_FOWNER`
(`deploy/provider-cloud-hypervisor/host/banlieue-provider-cloud-hypervisor.service`);
this is a new capability, and the threat-model pass must record it (TB-4, §6).

**A probe is traffic.** Up to a few ARP requests per static address per new
machine, on the segment the machine joins. Nothing else changes on the wire.

**Anyone on the segment can answer every probe** and block every static
machine there: a denial of service by a host already on that layer-2
segment. It cannot redirect traffic (the machine is never created), and it
is the same position from which that host could already ARP-spoof a running
guest (§8, no anti-spoofing). Record it in §8 with *Revisit when*: a network
class serves untrusted tenants on a shared segment.

**libvirt, vSphere and Proxmox see records, not the wire.** A static
machine with no agent and no lease is invisible to them, which
`AddressChecked=Unknown` cannot detect either (nothing reports what nothing
saw). Their check catches the common case (another VM with an agent, a lease
handed out by the same network), not every case.

**Create-time only.** A machine that takes the address after the check is
not caught; neither is a duplicate that predates the check. ADR-0083 has the
same window for banlieue's own objects.

## Open questions

1. **Opt-out.** A network where probing is unwelcome (a production VLAN
   with ARP inspection) needs a way to skip the probe: a per network class
   setting in the Provider's `networkClasses`, or provider configuration?
2. **The probe window.** RFC 5227's full probe (a random wait of up to
   `PROBE_WAIT` = 1 s, then `PROBE_NUM` = 3 requests 1 to 2 s apart, then
   `ANNOUNCE_WAIT` = 2 s) takes roughly 4 to 7 s per address, paid on every
   new static machine and in parallel across its addresses. Is a shorter,
   documented window acceptable for pools?
3. **libvirt's DHCP-range rule:** warn (an event) or block?

## Not decided here

- **DHCP machines.** Their address is the DHCP server's to keep unique.
- **IPv6.** The same shape with Duplicate Address Detection (RFC 4862)
  instead of RFC 5227, when static IPv6 lands.
- **Continuous detection** of a conflict that appears after creation.
