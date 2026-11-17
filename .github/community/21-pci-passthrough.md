# 21: PCI passthrough for Cloud Hypervisor guests

> **Goal.** A `VirtualMachine`, or a Cluster API `MachineDeployment` over a
> `CloudHypervisorMachineTemplate`, gets a whole host PCI device, typically
> a GPU, through VFIO, with the VMM still sandboxed as the guest's own uid
> under Landlock. The host declares which devices may be used; a machine
> names a class; a device belongs to one guest at a time and is released
> when that guest is deleted.
>
> **Stop condition.** On a real Cloud Hypervisor host with one GPU in its
> own IOMMU group: a `VirtualMachine` whose `VMClass` names a device class
> is scheduled, its guest sees the GPU and the vendor driver works, while
> its VMM runs as its uid with Landlock on; a second VM of the same class
> waits; deleting the first releases the GPU to the second; a power cycle
> and a provider restart keep the assignment. A Cluster API
> `MachineDeployment` of one GPU worker (`maxSurge: 0`) joins its cluster,
> and a replacement `Machine` gets the GPU too.
>
> **Status (2026-11-16): not started.** ADR-0133 is Proposed.

Baseline when written: ADRs 0001 to 0132 exist or are reserved. **This
roadmap reserves ADR-0133.** Issue #190.

## Why

GPU worker nodes as VMs: a `MachineDeployment` of GPU workers on Cloud
Hypervisor hosts, with upgrades and MachineHealthCheck replacement. The
pinned VMM can already pass a GPU through (#190); what is missing is the
API, the host inventory, the scheduling, and a way to give a sandboxed VMM
exactly one device.

## Phases

| Phase | What | Status |
| --- | --- | --- |
| A | API (`VMClass.spec.hardware.devices`, `CloudHypervisorMachine(Template).spec.devices`, `status.devices`, `DevicesAssigned`), host config `[device_classes]` and `--device-class`, install / preflight / selftest checks, per-class device capacity, `gpuPassthrough` published only when served | ⛔ |
| B | `banlieue-vfio@<uid>` root grant and `vfio-grant`, polkit, guest unit `LimitMEMLOCK` and VFIO `DeviceAllow`, assignment and release on the host, orphan release, wire format and the Landlock symlink layout (cloud-hypervisor#9399) | ⛔ |
| C | Scheduler device step, controller class resolution, moves of device guests refused, Cluster API guide and example (`maxSurge: 0`) | ⛔ |
| D | Live test on a real GPU host, both paths (not committed: no host details in the repo); nested-VFIO e2e attempted, or documented as manual | ⛔ |
| E | Drop the Landlock symlink layout once banlieue pins a Cloud Hypervisor release containing cloud-hypervisor#9405 | ⛔ |

## Out of scope (later ADRs)

SR-IOV virtual functions, mdev / vGPU and MIG; GPUDirect P2P and
multi-device NUMA topology; hotplug; moving a guest that holds a device;
passing a pre-opened device FD instead of a path; libvirt (`<hostdev>`),
vSphere (DirectPath I/O) and Proxmox (`hostpci`).

## Read first

ADR-0133, then ADR-0062 Decision 4, ADR-0063 (Decisions 1, 3 and 6),
ADR-0065 Decision 2, ADR-0107 and ADR-0128.
