<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0111 — A cloned VM gets one NIC per declared interface

- **Status:** Accepted
- **Date:** 2026-10-30
- **Deciders:** Erick Bourgeois
- **Related:** Amends [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)
  (clone, static IP, `guestinfo.network.*`: single NIC today). Builds on
  [ADR-0031](0031-vmimage-template-multiple-nics.md) (multi-NIC *templates*; the
  clone path was left single-NIC), and on the `VMClass.spec.network.interfaces` /
  `VirtualMachine.spec.networkOverrides` shape of [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md).
  Serves [ADR-0109](0109-isolated-sandbox-network-with-proxy-egress.md) (the
  proxy VMs have a routable adapter and a sandbox adapter).

## Context

A `VMClass` may declare several interfaces and a `VirtualMachine` may override
the static address of each. The controller turns both into the
`VSphereMachine.spec.network` list, and the provider resolves every entry to a
port group. But the clone path uses only the first entry:

- `ensure_vm` reads `spec.network.first()` and resolves **one** network;
- `CloneVmRequest` carries **one** network;
- `clone_vm` edits the template's single NIC onto that network and adds nothing;
- `build_guestinfo` publishes one flat set, `guestinfo.network.{ip,prefix,gateway,dns,domain}`,
  taken from the first interface that has a static address.

So a VM declared with two interfaces is created with one. Found live (2026-10-30):
three egress gateway VMs whose class declares a routable and a sandbox interface
were created with the routable NIC only; `govc` listed a single device and the
second interface's address and port group were resolved, shown in the
`VSphereMachine`, and never used.

The guest side has the same limit. The in-guest `configure-network` script
writes one `10-static.network` that matches every `en*` interface and assigns it
the single address, and it does nothing without a gateway. With two NICs both
would receive the same address, and a segment with no gateway, which is the
point of an isolated sandbox segment, could not be configured at all.

## Decision

1. **The clone adds one NIC per declared interface beyond the first.** The
   template's own NIC is edited onto interface 0's port group, as today. Each
   further interface gets a new NIC of the **same adapter type** as the
   template's NIC, backed by that interface's port group (a distributed
   port-group backing when the network is distributed, as for NIC 0). They are
   added in declared order, in the same `CloneVM_Task` as everything else.
   Every interface's network must resolve from the target cluster; if any does
   not, the clone is not attempted and the error names the interface.

2. **Guest configuration is published per NIC.** Interface 0 keeps the flat keys
   (`guestinfo.network.ip` and the rest) so images already deployed keep
   working. Interface `i >= 1` publishes `guestinfo.network.<i>.ip`,
   `.prefix`, `.gateway`, `.dns`, `.domain`, each only when set. A static
   interface with no gateway is valid and publishes no gateway. The flat keys
   now come from interface 0 only (before: from the first interface that was
   static); for a single-NIC VM, the only kind that worked, nothing changes.

3. **The guest maps NIC `i` to the `i`-th ethernet interface in PCI address
   order.** vCenter does not tell the guest which device is which, and the
   interface names depend on the slot vCenter assigns. NICs are added in
   order, so slot order equals declared order. Only interface 0's slot is
   pinned (as today); the others are left to vCenter and are not named in any
   key. The in-guest `configure-network` is changed to write one networkd file
   per NIC: the default route, DNS and domain only from NIC 0, and only an
   address and prefix from a NIC with no gateway.

4. **No API change.** `VMClass`, `VirtualMachine` and `VSphereMachine` already
   carry the list; this makes the provider honour it.

## Consequences

- A VM can have a routable adapter and an isolated one. This is what the egress
  gateways need (ADR-0109): a default route on one side and a bare layer 2
  address on the other, with forwarding off in the guest.
- A VM declared with two interfaces now **gets two**. Anything that declared a
  second interface and was silently getting one will change on its next create;
  existing VMs are untouched (`ensure_vm` does nothing for a VM already
  provisioned). Reconciling a changed NIC list on a running VM stays out of scope.
- **Reach.** A VM spec can already attach a VM to any port group the Provider
  advertises; interfaces beyond the first widen that, and a VM with two NICs
  joins two segments. The controls are unchanged: the Provider only offers the
  network classes the operator lists, and the admission policies that govern
  `VMClass` and `VirtualMachine` apply to every interface. A VM that bridges two
  segments must have forwarding off in the guest, which is the guest image's job
  and is checked by the egress gateway's own cloud-config tests.
- The guestinfo keys are readable by the guest, as the flat ones already are;
  they hold addresses and names, no credentials.
- **Guest contract.** An image must carry the multi-NIC `configure-network`
  (vm-build) to configure NIC 1 and beyond. **An older image is worse than
  one that ignores the second NIC:** its script writes one `Name=en*` file, which
  matches *every* ethernet interface, so a two-NIC VM gets NIC 0's address and
  default route on both adapters, a duplicate address on two segments. Never
  create a multi-interface VM from an image that lacks the new script. Today
  only the egress class declares two interfaces. Rebuilding the image is the
  operator's step.
- Adapter type follows the template. A template with an unusual NIC (E1000)
  gives every added NIC the same type; there is no per-interface adapter field.
- Not done: removing or changing NICs of a provisioned VM; hot-adding after the
  clone; pinning slots for NICs beyond the first; templates that already carry
  more than one NIC (ADR-0031) are not reconciled by the clone, which edits the
  first and adds the rest.
