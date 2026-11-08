<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Guide: an isolated sandbox segment

Sandboxes that run untrusted code (an AI agent, a build from a stranger's
repository) should reach exactly two things: each other, and a small set of
gateways that decide what leaves. This guide sets that up with fields
banlieue already has: no new kind, no controller, no Rust
([ADR-0109](https://github.com/firestoned/banlieue/blob/main/docs/adr/0109-isolated-sandbox-network-with-proxy-egress.md)).

The complete manifests are in
[`examples/25-isolated-sandbox-segment.yaml`](https://github.com/firestoned/banlieue/blob/main/examples/25-isolated-sandbox-segment.yaml).

## The shape

```text
                 routable network (egress-nat)
   ──────────────┬─────────────────┬─────────────────┬────────────
                 │ egress (static, │                 │
                 │ default route)  │                 │
           ┌─────┴─────┐     ┌─────┴─────┐     ┌─────┴─────┐
           │ gateway-1 │     │ gateway-2 │     │ gateway-3 │   forwarding OFF:
           └─────┬─────┘     └─────┬─────┘     └─────┬─────┘   each connection is
                 │ sandbox         │                 │         terminated and
   ──────────────┴─────────────────┴─────────────────┴──────   re-opened
                 sandbox segment: layer 2 only, no router,
                 no DHCP, no resolver
   ───────┬──────────┬──────────┬──────────┬──────────────────
       ┌──┴──┐    ┌──┴──┐    ┌──┴──┐    ┌──┴──┐
       │ sbx │    │ sbx │    │ sbx │    │ sbx │   one NIC each, a static address
       └─────┘    └─────┘    └─────┘    └─────┘   from the pool, no default route
```

A sandbox cannot route anywhere because nothing on its segment routes. The
only way out is an application-layer connection to a gateway, which
authorizes it and opens its own connection from its routable address.
Sandbox addresses never appear on the routable network.

## The pieces

### 1. The segment (host configuration)

banlieue names the segment; it does not create it (roadmap 19's `VMNetwork`
will). Create it once per host:

- **libvirt:** an isolated network, with no `<forward>` (libvirt then
  rejects anything forwarded off its bridge) and no `<dhcp>` or `<dns>`.
  The definition is in the example's header.
- **vSphere:** a port group on a VLAN with no SVI and no gateway, trunked
  on every host a member can land on. List the zone in `perZone` only after
  a two-host test passes: banlieue sees that a port group of that name
  exists, not that its VLAN is trunked (ADR-0109 Decision 3).

Then offer it as a network class on the Provider:

```yaml
networkClasses:
  - name: egress-nat          # routable, for the gateways' egress only
    target: { network: default }
  - name: sandbox-segment
    target: { network: sandbox-segment }
```

### 2. The gateways (two NICs, `egress` first)

A gateway is an ordinary `VirtualMachine`. Its class lists `egress` first,
static with a gateway (the default route), and `sandbox` second, on the
segment. Order matters: the first static interface is the one the
user-data placeholders and vSphere's `guestinfo.network.*` describe
(ADR-0109 Decision 6).

- The `egress` address comes from the `VirtualMachine`'s
  `networkOverrides`.
- The `sandbox` address is fixed in the gateway image, three gateways
  being three per-image constants. On vSphere banlieue can set it too
  (ADR-0111 publishes every static NIC).
- The software (Envoy, its SPIFFE identity, the allowlist, IP forwarding
  off, a listener on the `sandbox` interface only) is image content
  (ADR-0108, ADR-0109 Decision 5). mediatore builds one
  ([its ADR-0028](https://github.com/firestoned/mediatore/blob/main/docs/adr/0028-egress-gateway-envoy-with-spiffe-identity.md)).

Run three per environment (Decision 4). A sandbox gets the ordered list of
gateway addresses from its broker and fails over per connection.

### 3. The sandboxes (one NIC, no gateway)

A pool on the segment, each member drawing a static address with **no
`gateway`**, so it has no default route (Decision 7):

```yaml
addressing:
  interface: eth0
  pool:
    - 10.200.0.32-10.200.0.95   # keep clear of the gateways' sandbox addresses
  prefix: 24
```

## Addresses on Kairos guests

Kairos applies no cloud-init `network-config` from the seed, so a Kairos
gateway or sandbox writes its own networkd file from the placeholders
banlieue renders into its user-data. The example carries both stages, as
ConfigMaps referenced by `userData.configMapRef`. Two things found on the
first live run:

- **Match the right NIC on a gateway.** `Name=en*` matches both
  interfaces; match the egress one by name (on banlieue's libvirt domains
  the first interface is `ens2`).
- **Kairos ships its own `20-dhcp.network`.** Name your file `10-…` so it
  sorts first. Kairos's initramfs may still try DHCP once during early
  boot; on a segment without DHCP that request goes unanswered.

A cloud-init image needs neither stage: it gets its addresses from the
seed's `network-config` ([ADR-0114](https://github.com/firestoned/banlieue/blob/main/docs/adr/0114-static-addressing-in-the-nocloud-seed.md)).

## Checking it is isolated

| Check | How | Expect |
| --- | --- | --- |
| No route out of the segment | on the host: `virsh net-dumpxml sandbox-segment` | no `<forward>`, no `<dhcp>`, no `<dns>` |
| Forwarded traffic is refused | the host's libvirt firewall rules for the bridge | a reject rule for traffic forwarded off it |
| Members have no default route | a member's `networkOverrides` (stamped by the pool) | an address and prefix, no `gateway` |
| Gateways do not route | in the gateway image | `net.ipv4.ip_forward = 0` |
| Each member has its address | the member's `status.addresses` | the drawn address; `addressSource: static` until something observes it (declared, not proven) |

## What has run live

On a libvirt host (2026-11-05, and end to end on 2026-11-08), mediatore ran
this topology: an isolated libvirt network as the segment, three two-NIC
gateways, and a pool drawing static addresses through the Kairos stage
above.

- **Proven:** a member attested through a gateway's relay, loaded a host
  table that drops everything but traffic to the gateways, and reached its
  egress proxy through the gateways. A request outside the claim's rules
  was refused (`403 outside_rules`). libvirt's isolated network refused a
  connection forwarded off the segment.
- **Proven 2026-11-08:** an allowed request completing through a gateway's
  Envoy. A request inside the claim's rules reached the upstream, which
  answered 200 with the broker-minted bearer present, while a request outside
  them was refused (403). Releasing the claim retracted its entry and the
  pool recycled the member. (The 2026-11-05 run had stopped at an expired
  upstream credential, an environment problem.)

## What banlieue does not do

- Create, change or delete the segment (roadmap 19).
- Run, configure or know about the gateways' software.
- Inspect images to check that forwarding is off or that only the
  `sandbox` interface listens: that is the image author's test.
- Offer any inbound path to a sandbox. There is no SSH and no jump host;
  the hypervisor console, behind RBAC, is the only way in
  ([ADR-0082](https://github.com/firestoned/banlieue/blob/main/docs/adr/0082-no-inbound-to-sandboxes.md)).
