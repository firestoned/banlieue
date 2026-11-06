<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0114: Static addressing reaches libvirt and Cloud Hypervisor guests through the NoCloud seed

- **Status:** Accepted
- **Date:** 2026-11-04
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-11-06 (a static machine reports its declared address
  when nothing observes one; see *Amendment: the declared address*)
- **Related:** Extends [ADR-0054](0054-nocloud-seed-iso-first-party.md) (the
  first-party NoCloud seed) and the per-interface `ipam` of
  [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md); generalizes
  [ADR-0075](0075-proxmoxmachine-inframachine-contract.md) Decision 5 (Proxmox's
  `network-config`) to every NoCloud provider; serves
  [ADR-0109](0109-isolated-sandbox-network-with-proxy-egress.md) Decision 7
  (sandboxes on static, gateway-less addresses) and ADR-0056 (pool
  addressing).

## Context

A `VMClass` interface can be static (`ipam.static`), a `VirtualMachine` can
override its address (`spec.networkOverrides`), and a `VirtualMachinePool`
stamps a per-member address the same way (`spec.addressing`, ADR-0056). The
controller resolves all of it into the infra CR's per-NIC `ipam`, for every
backend.

Only two backends deliver it to the guest. vSphere writes
`guestinfo.network.<i>.*` (ADR-0024, ADR-0111) and Proxmox writes a cloud-init
`network-config` into its seed (ADR-0075). **libvirt and Cloud Hypervisor drop
it**: their seed (`banlieue_provider_sdk::cloudinit::seed_files`) carries
`meta-data` and `user-data` only, and neither provider reads `ipam`. A
machine declared static comes up on DHCP, or with no address on a segment
that has no DHCP, and nothing reports the difference. ADR-0109's sandboxes
(one NIC, a static address, no gateway) cannot be built on either backend.

Two kinds of guest read a NoCloud seed, and they read it differently:

- **cloud-init** applies a `network-config` file (v1 or v2) from the seed
  before the network comes up, matching interfaces by MAC.
- **Kairos** (yip's `datasource` plugin) takes user-data, the hostname and SSH
  keys from the seed and applies **no** network configuration from it. A
  Kairos guest configures its network from its own cloud-config `stages`
  (networkd files). This is true of mediatore's sandbox images.

## Decision

1. **Every NoCloud seed carries a `network-config` (v2) when any NIC is
   static.** One renderer, `banlieue_provider_sdk::cloudinit::network_config`,
   moved from the Proxmox provider unchanged in output, serves Proxmox,
   libvirt and Cloud Hypervisor. Each NIC is matched by MAC and renamed to
   its spec name; a static NIC gets its address and prefix, a default route
   only when a `gateway` is set (a gateway-less NIC gets none, ADR-0109
   Decision 7), and its nameservers and search domain; every other NIC is
   `dhcp4: true`, as cloud-init's own default would make it. With no static
   NIC there is no `network-config` and the seed is unchanged.
2. **A static machine always has a seed.** Today libvirt always builds one,
   but Cloud Hypervisor builds one only for user-data; it now also builds one
   when a NIC is static, so a machine with no user-data still gets its
   address.
3. **MACs are known before the seed is written.** cloud-init matches by MAC.
   Cloud Hypervisor already derives every NIC's MAC from the machine UID
   (ADR-0062). libvirt let libvirtd pick one; it now derives one from the
   `LibvirtMachine`'s UID with libvirt's own prefix (`52:54:00`), for every
   NIC without an explicit `macAddress`, **when any NIC is static**: the
   rule Proxmox follows. A DHCP-only machine keeps libvirtd's choice, so an
   upgrade changes no running DHCP machine.
4. **A Kairos guest is configured through its user-data, not the seed's
   `network-config`.** banlieue renders the fixed ADR-0024 placeholders
   (`${IP}`, `${PREFIX}`, `${GATEWAY}`, `${DNS}`, `${DOMAIN}`) into
   `userData` from the first `networkOverrides` entry, on every backend,
   which covers a one-NIC sandbox: the image or pool template carries a
   Kairos stage that writes a networkd file from them. banlieue does not
   inject network stages into a guest's cloud-config: what a guest runs is
   image content (ADR-0108). Placeholders for further NICs are a separate
   change, made when a Kairos guest needs two static NICs.
5. **The seed is written once.** libvirt reuses an existing seed volume and
   Cloud Hypervisor rewrites an identical file, as today (ADR-0054): a
   machine created before this change keeps its seed, and its guest has
   already booted. The change applies to machines created after it.

## Consequences

- A static `ipam` now means the same thing on four backends. Pool
  addressing works on libvirt and Cloud Hypervisor for cloud-init images, and
  for Kairos images through placeholders.
- Data the cluster user controls (an address, a gateway, nameserver and
  domain strings) now crosses into the libvirt and Cloud Hypervisor seed as
  YAML, as it already did for Proxmox. The renderer single-quotes every
  value, so a value cannot add keys to the document; the guest trusts the
  seed exactly as it trusts user-data, which the same user already controls.
- libvirt domains with a static NIC get derived MACs; two machines can only
  collide if their UIDs hash alike in the 24 bits kept, which within one
  libvirt network is as unlikely as libvirtd's own random choice.
- **Verified live** (libvirt, Debian 13 cloud image, 2026-11-04): the guest
  applied the seed's address and route on the derived MAC (its console's
  cloud-init network table showed both), pinged its gateway, installed
  packages through that route, and its agent reported the address
  (`live_cloudinit.rs`, `a_guest_applies_a_static_address_from_the_seed`).
- **Verified live, Kairos** (libvirt, pre-laid Kairos Debian 13,
  2026-11-04): a `VirtualMachinePool` member drew its address from
  `spec.addressing`; the controller rendered it into user-data through the
  ADR-0024 placeholders, the Decision 4 networkd stage wrote it, and the
  guest agent reported it. The image's own `20-dhcp.network` also took one
  DHCP lease in early boot, before the stage ran; the running system used
  the static address. The libvirt guide records it.
- **Found on the v0.4.12 rollout: a static VM with no `userData` stayed on
  DHCP.** Its seed had `meta-data` and `network-config` but no `user-data`,
  and cloud-init rejects such a seed outright. Every seed now carries
  `user-data` (an empty cloud-config when none is set): ADR-0054, amended
  2026-11-04. v0.4.12 delivers a static address only to a VM that also sets
  `userData`, on all three NoCloud backends (Proxmox's static seed had the
  same gap since ADR-0075); v0.4.13 to every VM.
- **Found live: a static libvirt guest with no agent reports no address.**
  It asks for no DHCP lease, and libvirt's ARP source reported nothing even
  while the guest pinged its gateway every 2 s. The provider's address
  discovery falls back to exactly those two sources, so such a machine's
  `status.addresses` stays empty although the guest is up and addressed. A
  follow-up decides whether a static machine reports its declared address.
  Resolved 2026-11-06 by *Amendment: the declared address* (below): such a
  machine reports its declared address, labelled `addressSource: Static`.
- Not done: indexed placeholders for a second static NIC; IPv6; a status
  field saying which delivery path a guest used (the guest's own address,
  read back through DHCP leases or the agent, is the observable result).

## Amendment: the declared address (2026-11-06)

**The gap.** Found live under Consequences: a static libvirt guest with no
guest agent reports no address. It takes no DHCP lease, and libvirt's ARP
source reported nothing even while the guest pinged its gateway every 2 s.
The provider's discovery uses exactly those sources, so
`LibvirtMachine.status.addresses`, and the `VirtualMachine` and claim that
mirror it, stay empty although the guest is up on the address banlieue
gave it. Cloud Hypervisor has the same shape: it reads the host's neighbour
table, which is blind until the guest transmits. A pool member on an
ADR-0109 segment, static and agentless, would be invisible to a consumer
looking for its address.

banlieue already knows the answer: it wrote it. The question is whether
reporting it breaks non-negotiable 6, *status mirrors infra*.

**It does not, if the report says what it is.** The address goes into the
**infra CR's** status, written by the provider like any other observation,
and the controller mirrors it unchanged, so the `VirtualMachine` still
reflects only what its infrastructure reports. What must not happen is a
declared address passing as one the guest was seen to use. The
`addressSource` field exists for exactly that distinction, and Cloud
Hypervisor's `ChAddressSource` already defines `Static` ("known before boot,
from IPAM", ADR-0024, ADR-0033), declared but never set.

### Decision

1. **Observed always wins.** A provider reports declared addresses only when
   no observing source (guest agent, DHCP lease, ARP or neighbour table)
   reports any address for the machine. When one does, its result is
   reported as today and the declared address is not added to it.
2. **Only while the machine runs, and only for static NICs.** The fallback
   applies to a machine whose observed power state is `PoweredOn`, and it
   lists the `ipam.static` address of each statically addressed NIC, in spec
   order, as `InternalIP`. A DHCP NIC contributes nothing; a machine with no
   static NIC reports nothing, as now. A stopped machine reports no address
   at all.
3. **`addressSource: Static` labels it.** libvirt's `LibvirtAddressSource`
   gains `Static`, matching `ChAddressSource::Static`, which the Cloud
   Hypervisor provider now sets. The CAPI `MachineAddress` shape is
   unchanged: the source is banlieue's own field beside it, so CAPI
   consumers see an address and banlieue consumers can tell how it was
   known.
4. **It means "declared", not "applied".** `Static` says banlieue configured
   this address, not that the guest is answering on it: a guest that never
   applied its seed (a Kairos image without the placeholder stage, or one
   from before the ADR-0054 amendment) is reported the same way. Proof that
   the guest is up and configured stays where it is today: `GuestReady`, or
   an observed source.
5. **No CRD shape changes beyond the enum value.** `Static` is added to
   libvirt's enum (additive); the Cloud Hypervisor value already exists.

### Consequences

- An agentless static pool member shows its address in the `VirtualMachine`
  and the claim, which is what a consumer reaches it by.
- A consumer that treats any address as "reachable" must now check
  `addressSource`, or wait for `GuestReady`; the guides say so.
- The mismatch case (the guest reports an address that differs from the one
  declared) is not detected here: Decision 1 reports the observation, and a
  condition saying "observed differs from declared" is left to a later
  change if one is wanted.

Approved as written by Erick, 2026-11-06. The shared helper is
`banlieue_provider_sdk::status::declared_addresses`; libvirt applies it in
`converge` when no source answers and the domain runs, Cloud Hypervisor in
`build_status` from the plan's declared addresses.

