<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0144: A Cloud Hypervisor host serves sandboxes from an isolated segment, behind two egress gateway VMs that NAT out through the host's own address

- **Status:** Accepted
- **Date:** 2026-11-19
- **Proposed:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Related:** Builds on [ADR-0063](0063-cloud-hypervisor-host-supervision.md)
  (the provider creates and owns taps), [ADR-0067](0067-banlieue-host-install-subcommand.md)
  Decision 7 (the binary never creates a bridge, and installs no packages),
  [ADR-0114](0114-static-addressing-in-the-nocloud-seed.md) (static addressing
  through the seed) and ADR-0062 Decision 4 (host paths and bridges are
  resolved on the host, never in the cluster). Consumes mediatore ADR-0027,
  ADR-0028 and ADR-0029 (the egress gateway and the routeless sandbox segment)
  as the guest-side contract; this ADR does not change them.

## Context

A Cloud Hypervisor host is about to run agent sandboxes: many small guests (2
CPUs, 4 GiB) that run code nobody reviewed. Today a CH host has one internal
bridge per network class, with no uplink (ADR-0067 Decision 7: the bridge is
the operator's, the binary never makes one). That is enough for a lab guest and
wrong for a sandbox, for three reasons:

1. **Sandboxes can reach each other.** Guests on one bridge share a broadcast
   domain. One compromised sandbox could probe, ARP-spoof or attack its
   neighbours.
2. **Sandboxes can reach the host.** The host has an address on the bridge, and
   on the host measured below the kernel forwards (`ip_forward = 1`, FORWARD
   policy ACCEPT), so a guest that names the host as its gateway is routed.
3. **There is no governed way out.** The sandbox model (mediatore ADR-0029) is a
   routeless segment whose only exit is the egress gateway, which enforces what
   a sandbox may reach. The gateways that exist are VMs on another platform
   with no leg on a CH host's bridge.

Constraints given by the operator, taken as fixed:

- **No additional routable address can be put on the host.** The only way out
  is NAT through the host's existing address.
- **Every sandbox is isolated from every other sandbox.**
- **The gateway is not a single point of failure per host: two gateways.**

### What was measured

On the first host that will do this (RHEL 8.10, kernel 4.18.0-553, iproute2 6.2,
cgroup v1). Read-only checks; nothing was changed to take them.

| Question | Observed |
| --- | --- |
| Kernel port isolation | `/sys/class/net/<port>/brport/isolated` exists, so a bridge port can be set `isolated on` (`bridge link set dev <tap> isolated on`) with no extra package. An isolated port reaches only non-isolated ports. |
| Packet filter tools | `iptables` present; `nft`, `ebtables` and `keepalived` are **not** installed. The kernel has `nf_tables` with the bridge family. |
| `br_netfilter` | Loaded, `net.bridge.bridge-nf-call-iptables = 1`: bridged frames already traverse the host's iptables FORWARD chain, whose policy is ACCEPT. Another tenant of the host (the container runtime and the kubelet's networking) set this, so it is not ours to change. |
| Forwarding | `net.ipv4.ip_forward = 1`, set by another tenant. |
| API | `CloudHypervisorMachine.spec.nics` is a list (one tap per NIC, ADR-0062), so a two-NIC guest needs no API change. |

## Decision

### 1. Two internal bridges per host, neither with a physical port

A host that serves sandboxes has a **sandbox bridge** and an **uplink bridge**.
Neither has a physical port; neither is created by the binary (ADR-0067
Decision 7 stands). Each is a network class in the host config, so a machine
names `sandbox` or `uplink` and the host resolves the bridge. The sandbox
bridge has no uplink at all.

### 2. Sandboxes: one NIC, a static address, the gateways as the only route

A sandbox has one NIC on the sandbox class, a static address from a pool
(ADR-0114), and a default route to the **gateway address** (Decision 4), never
to the host. The host's own address on the sandbox bridge, if it has one, is
answered by nothing (Decision 6).

### 3. Each sandbox port is isolated

The provider already creates each guest's tap and joins it to its bridge
(ADR-0063, with `CAP_NET_ADMIN`). For a network class declared `isolate_ports =
true` in the host config it also sets `isolated on` on that port, so a sandbox
reaches the gateways and nothing else on the segment. Gateway ports are not
isolated. Explicit over implicit: nothing infers isolation from a class name.

`preflight` refuses a class declared `isolate_ports = true` on a kernel
without `brport/isolated`: isolation that is configured and not enforced is
worse than none.

### 4. Two gateway VMs per host, each on both bridges

Each gateway is an ordinary VM with two NICs (sandbox bridge, uplink bridge)
of an `egress` class. Two exist per host and are placed on that host
(`providerSelector` on a label unique to the host). The two share one address
on the sandbox segment, which is what a sandbox's default route names; how that
address fails over between them is the gateway image's concern (mediatore) and
is open below.

### 5. The host NATs the uplink bridge out through its own address

The host masquerades traffic from the uplink bridge's range out of every
interface except the uplink bridge itself. The source seen beyond the host is
the host's existing address; no routable address is added to the host. The
rules are `iptables` (already present; the binary installs no package).

The sandbox bridge's range is **not** masqueraded and **not** forwarded
(Decision 6). Only the gateways' uplink leg is.

### 6. The host drops everything the sandboxes could aim at it

Root-owned host rules, installed as a unit and a rules file by the operator
(a documented recipe, like the bridge; the binary does not install them):

- INPUT: drop from the sandbox bridge entirely, and from the uplink bridge
  except what NAT needs;
- FORWARD: drop from the sandbox bridge to anything outside it, and drop
  between the two bridges; allow the uplink bridge outward, and replies back;
- IPv6 is disabled on both bridges.

`banlieue host cloud-hypervisor preflight` reports whether these are in place
for a class marked as a sandbox class, and refuses `install` for one when they
are not.

### 7. Layers, and what each one is worth

| Layer | Stops | State | Cost |
| --- | --- | --- | --- |
| Port isolation (Decision 3) | sandbox to sandbox, ARP spoofing between sandboxes | measured available; the provider change is new | one netlink call per tap |
| Host INPUT and FORWARD drops (Decision 6) | a sandbox using the host as a router or attacking its services | new rules file and unit | operator step; verified by preflight |
| No host address leaks upstream (Decision 5) | the segment being reachable from outside | NAT only on the uplink bridge | none |
| Gateway enforcement (mediatore ADR-0028, 0030) | what a sandbox may reach beyond | exists on the other platform | the two gateways' capacity |
| Static neighbour entries for sandbox addresses on the gateways | a sandbox claiming another's address toward the gateway | not built | gateway image change |

No layer is trusted alone. The provider holds `CAP_NET_ADMIN` and could clear
a port's isolation: a compromised provider is a host-level compromise already
(ADR-0063), so this is recorded and not defended here.

### 8. A bring-up exception, tracked

A test guest that has to download software before the gateways exist may use a
temporary masquerade of the sandbox range. It is added by hand, recorded in the
host's log, and removed before the host serves sandboxes. It is not a mode of
the design.

## Consequences

- **Capacity:** two gateways of the `egress` class (4 CPUs and 8 GiB each)
  take 8 CPUs and 16 GiB of the host's guest budget (ADR-0105), before any
  sandbox. A 44 CPU budget leaves room for 18 sandboxes of 2 CPUs, not 22.
- **Failure domain:** the gateways and the sandboxes share the host. A host
  failure takes all of them; two gateways protect against one gateway VM
  failing, not against the host.
- **Isolation is a kernel feature we measured on one kernel.** It exists on
  RHEL 8's 4.18 through a backport. `preflight` checks for it rather than
  assuming it.
- **Operator work stays operator work.** The bridges, the host rules and the
  NAT unit are documented recipes (ADR-0067 Decision 7, ADR-0084). The binary
  gains: a `isolate_ports` class attribute, the tap flag, and the preflight
  checks.
- Threat model: two new trust boundaries (the sandbox segment, and the uplink
  segment with NAT), a new host rules file, a new class attribute. Pass due when
  implemented.
- Accepted, not implemented. CALM follows now; the threat-model pass follows
  the implementation.

## Open questions

- **Gateway failover.** A shared address with VRRP in the gateway image
  (`keepalived` is not on the host and need not be: it belongs in the guest), or
  a gateway assigned per pool with no shared address. The gateway image
  decides, in mediatore. This ADR needs only that the sandbox's route names one
  address.
- **Per-sandbox pinning.** Static neighbour entries on the gateways and
  `learning off` on the isolated ports would stop address claiming. Who
  programs them per sandbox (the provider through netlink, or the gateway
  image from the claim) is not decided.
- **Does the sandbox bridge need a host address?** Without one the host cannot
  be reached at all on that segment; with one, Decision 6 must be verified
  to answer nothing. Preferred: none.
- **Rule persistence.** Another tenant of the host manages iptables and
  forwarding; the recipe must order itself after theirs and survive their
  reloads.
- **A gateway on a host that serves no sandboxes** is out of scope.
