<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Providers

A **provider** is what turns banlieue's backend-neutral `VirtualMachine` into
a real VM on one kind of hypervisor or cloud. You never pick a backend in a
`VirtualMachine`: you name a `VMClass` and a `VMImage`, and the scheduler
places the VM on a `Provider` whose classes, images and capacity fit. This
section is about setting up the backends themselves: one page per provider
class, from preparing the host or account to a first VM.

## The three objects

| Object | Scope | Who writes it | What it says |
| --- | --- | --- | --- |
| `ProviderClass` | cluster | `banlieue bootstrap operator`, once per compiled backend | **What banlieue runs** for a class of backends: the provider image, and whether the process runs in the cluster (`Managed`) or on a host (`External`) |
| `Provider` | namespaced | you, one per backend instance | **Where**: one vCenter, one libvirt host, one Cloud Hypervisor host, one Proxmox cluster, one Azure subscription in one region. Its `connection`, its storage and network class mappings, and the features you assert |
| Infrastructure CR (`CloudHypervisorMachine`, `VSphereMachine`, `LibvirtMachine`, `ProxmoxMachine`, `AzureVirtualMachine`, ...) | namespaced | `banlieue-controller`, never you | **One scheduled VM** on that backend, satisfying the Cluster API v1beta2 InfraMachine contract |

Applying a `Provider` is what brings a backend up: `banlieue-operator` gives
each one its own ServiceAccount and a Role scoped to that one `Provider`, and,
for a `Managed` class, its own Deployment. The controller and the providers
talk only through these CRDs, never over RPC. The model is explained in
[Provider Model](../concepts/providers.md); installing the operator and the
day-two lifecycle of a `Provider` (upgrades, liveness, deletion) are in
[Provider Lifecycle & Install](lifecycle.md).

## The provider classes

| Class | Status | Where the provider runs | Credential | Start here |
| --- | --- | --- | --- | --- |
| `cloud-hypervisor` | Implemented, **the reference backend** | On the KVM host, as a systemd service (`External`) | A self-renewing ServiceAccount token on the host; no Secret | [Cloud Hypervisor](cloud-hypervisor.md), then [Automating the Install](cloud-hypervisor-automation.md) |
| `vsphere` | Implemented | In the cluster | vCenter username and password (Secret) | [vSphere](vsphere.md) |
| `libvirt` | Implemented | In the cluster | mTLS client certificate (Secret) | [libvirt](libvirt.md) |
| `proxmox` | Implemented | In the cluster | Proxmox API token (Secret) | [Proxmox VE](proxmox.md) |
| `firecracker` | **In progress** (roadmap 20): API only, nothing runs a guest yet | On the host (`External`) | n/a yet | [Firecracker](firecracker.md) |
| `azure` | **New, alpha** (roadmap 23) | In the cluster | Workload identity federation: a projected ServiceAccount token exchanged at Entra ID; no Secret | [Azure](azure.md) |

The [Provider Comparison](comparison.md) sets the classes side by side,
capability by capability, with performance measured through banlieue's own
API.

## Choosing a class

- **You have bare-metal Linux hosts and want the smallest trust base:**
  Cloud Hypervisor. No hypervisor daemon and no hypervisor credential, one
  uid per guest, a vTPM per guest, images pulled by digest, the fastest warm
  pools banlieue measures. The provider runs on the host, so each host
  carries one narrow cluster credential.
- **You have a vCenter estate:** vSphere. Several datacenters and clusters
  under one `Provider` become several failure domains.
- **You already run libvirt/KVM hosts**, or need QEMU's device model and UEFI
  Secure Boot: libvirt. The provider stays in the cluster and reaches
  `libvirtd` over mutual TLS.
- **You have a Proxmox VE node or cluster:** Proxmox, with a
  least-privilege API token.
- **You want VMs in a public cloud with no cloud key in the cluster:**
  Azure (alpha). One `Provider` per subscription and region; its
  availability zones are the failure domains.
- **You want many small, identical, short-lived sandboxes:** Firecracker
  is being built for that; until it runs guests, use Cloud Hypervisor pools.

Classes mix freely: one cluster can run several classes at once, and a
`VirtualMachine` moves between them by changing a placement selector, never
its shape. When one backend should serve several environments, or several
backends one environment, see
[Environment / Provider Isolation](../guides/environment-provider-isolation.md).

## Adding a provider class

The set of classes the controller can drive is closed today: a new backend is
a change to banlieue (a new infrastructure CRD, a provider built on
`banlieue-provider-sdk`, and a builder in `banlieue-controller`). The steps
are in [Provider Model: Adding a provider](../concepts/providers.md#adding-a-provider).
