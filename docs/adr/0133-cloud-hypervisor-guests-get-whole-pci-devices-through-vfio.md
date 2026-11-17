<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0133: Cloud Hypervisor guests get whole PCI devices through VFIO

- **Status:** Proposed
- **Date:** 2026-11-16
- **Deciders:** Erick Bourgeois
- **Related:** Follows [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md)
  Decision 4 (hosts declare, machines name a class) for a third kind of
  class; extends [ADR-0063](0063-cloud-hypervisor-host-supervision.md)
  (template units, per-guest uids, the polkit rule),
  [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md) Decision 2
  (a feature is published only where the host can serve it),
  [ADR-0107](0107-capacity-aware-scheduling.md) and
  [ADR-0128](0128-every-provider-publishes-measured-capacity.md) (typed,
  measured capacity); constrains
  [ADR-0127](0127-move-a-cloud-hypervisor-vm-between-hosts-in-a-zone.md)
  (moves); [ADR-0129](0129-capi-inframachines-receive-the-machines-bootstrap-data.md)
  (CAPI machines on this kind). Roadmap 21. Issue #190.

## Context

A guest cannot be given a host PCI device. A GPU worker, made as a
`VirtualMachine` or as a Cluster API `MachineDeployment` over a
`CloudHypervisorMachineTemplate`, is therefore impossible on a Cloud
Hypervisor host, although the pinned VMM (v53.0) runs one: #190 shows a GPU
passed through with `--device path=/sys/bus/pci/devices/<address>/`, the
vendor driver loading in the guest, and the card surviving a power cycle,
with the VMM run by hand as root and unsandboxed.

What exists today:

- **API.** `VMClass.spec.hardware` has CPUs, memory and disks;
  `CloudHypervisorMachineSpec` has no devices. `gpuPassthrough` is a
  well-known string in `ProviderCapabilities.features`. The scheduler does
  act on it (step 6: every `VMClass.spec.features` entry must be in the
  failure domain's `attributes.features`), but the Cloud Hypervisor provider
  publishes whatever features the admin declares, except `vtpm`, which it
  publishes only when the host can run one (ADR-0065 Decision 2). A class
  that requires `gpuPassthrough` can therefore be placed on a host that
  declares it, and get no device.
- **Wire.** `VmConfigRequest::for_guest` sends no `devices`. The pinned
  API's `DeviceConfig` takes the device's sysfs `path`, or a vfio cdev FD
  passed with `SCM_RIGHTS` on the API socket instead of a path.
- **Landlock.** With `landlock_enable`, the VMM adds `/dev/vfio/vfio` and
  one rule per device itself. On v53.0, v54.x and `main` until 2026-11-16
  that per-device rule read the IOMMU group from the link target of the
  device path instead of `<device path>/iommu_group`, so `vm.create` fails
  for any VFIO device with Landlock on (cloud-hypervisor#9399, fixed by
  cloud-hypervisor#9405, in no release yet). Reproduced on a real GPU host
  on v53.0: with the documented trailing slash, `Error opening path …
  Invalid argument (os error 22)`; without it, `failed to open /dev/vfio/1
  group: Permission denied`. The fixed build boots the same configuration
  with Landlock on. banlieue's own `landlock_rules` cover each tap's sysfs
  directory only.
- **Guest unit.** `banlieue-ch@.service` is `DevicePolicy=closed` with
  `/dev/kvm` and `/dev/net/tun`, sees `/dev` read-only, and sets no
  `LimitMEMLOCK`. VFIO's type-1 IOMMU pins all guest memory against the
  VMM's `RLIMIT_MEMLOCK`.
- **Provider.** Unprivileged (`banlieue`, `CAP_NET_ADMIN CAP_CHOWN
  CAP_FOWNER`), with `/dev`, `/sys` and `/proc` read-only to it and only
  `/dev/net/tun` openable (ADR-0063 Decision 6, amended 2026-11-15): it can
  neither change who owns a device node nor open one. Its polkit rule lets
  it start and stop instances of root-owned templates for uids in the guest
  range.
- **Host config.** `HostConfig` declares `storage_classes` and
  `network_classes`; there are no device classes. `banlieue host
  cloud-hypervisor install` preflight and selftest check KVM, not the IOMMU.
- **Scheduling.** `FailureDomainCapacity` has CPUs, memory and storage.
  Cluster API machines name `providerRef` directly and never pass through
  the scheduler, so the scheduler alone cannot keep a device exclusive.

## Decision

### 1. Hosts declare device classes; machines name a class

The host config gains `[device_classes]`, written by `banlieue host
cloud-hypervisor install --device-class <name>=<address>[,<address>…]`
(repeatable), root-owned like the rest of the file:

```toml
[device_classes]
gpu = ["0000:01:00.0"]
```

- A class is a set of PCI functions a guest may use interchangeably. A
  machine names a class, never an address, so a cluster-side principal or a
  stolen provider credential chooses only among devices root declared, as
  with storage and network classes (ADR-0062 Decision 4).
- **Identity is the PCI address** (`DDDD:BB:DD.F`): stable per host, and
  what the host knows. A `vendor:device` selector can come later as install
  sugar that expands to addresses; it is not a second identity.
- **Only the declared function is attached.** A GPU's audio, USB and UCSI
  functions stay bound to `vfio-pci` and unattached, as in #190.
- **One declared device per IOMMU group.** A group is opened as a unit and
  can belong to one guest, so install refuses two declared devices in the
  same group, and a device in two classes. The unit of assignment is then
  both a device and a group.

### 2. The host is checked, not configured

banlieue does not bind drivers or edit the bootloader: the IOMMU on the
kernel command line (`intel_iommu=on` or `amd_iommu=on`, with `iommu=pt`)
and `vfio-pci` binding at boot are host prerequisites, documented in the
host guide. They need a reboot, which an idempotent installer must not do.

Install, preflight and selftest refuse, as hard failures that name what is
missing:

- no IOMMU (`/sys/kernel/iommu_groups` empty);
- a declared device that does not exist, is not bound to `vfio-pci`, or has
  no reset method (`/sys/bus/pci/devices/<address>/reset_method` absent or
  empty);
- a group that is not viable: an endpoint in it bound to anything but
  `vfio-pci` (PCI bridges excepted);
- a missing `/dev/vfio/<group>`;
- two declared devices in one group, or one device in two classes.

`--print-vfio-setup` (emit the kernel command line and modprobe files for
the declared devices) is deferred: the guide shows them.

### 3. The provider publishes device inventory, and the feature is verified

- `FailureDomainCapacity` gains `devices: [{deviceClass, total, free,
  vendorDevice}]`, one entry per host class. Counts are integers and never
  overcommitted. `free` counts devices no guest uid holds (Decision 6).
- `Provider.spec.capabilities` gains `deviceClasses: [{name, target:
  {hostClass}}]`, the shape of `storageClasses` and `networkClasses`. A
  `VMClass` names the abstract class; the scheduler resolves it to the host
  class, as it does storage and network classes.
- **`gpuPassthrough` is published only when declared and served**, as
  `vtpm` is (ADR-0065 Decision 2): at least one host device class passes
  Decision 2's checks. This also ends today's case of a declared,
  unserved `gpuPassthrough`.

### 4. API

- **`VMClass.spec.hardware.devices`**: `[{name, deviceClass}]`, the
  abstract class. A list, capped at one entry by validation for now, so the
  shape survives several devices.
- **`CloudHypervisorMachine(Template).spec.devices`**: `[{name,
  deviceClass}]`, the host class, like this kind's `storageClass` and
  `networkClass`. Immutable, by CRD validation (`self == oldSelf`, with a
  spec-level rule that it cannot be added or removed), as this kind's other
  immutable fields are.
- **`CloudHypervisorMachine.status.devices`**: `[{name, deviceClass,
  address, iommuGroup}]`, written by the provider when it assigns.
- **Condition `DevicesAssigned`**, with reasons `NoFreeDevice`,
  `UnknownDeviceClass` and `DeviceNotUsable` (the device fails a Decision 2
  check now). A machine with no free device does not start and retries: a
  device frees when another guest is deleted, so this is not terminal.
- **Pinned memory.** A machine with a device has all its memory pinned. The
  field docs say so; `hugepages` stays optional.

### 5. Root grants the guest uid one group, and nothing else can

The VMM runs as the guest's uid under `DevicePolicy=closed`, Landlock and
`ProtectSystem=strict`. It needs its group node, the container
`/dev/vfio/vfio`, and enough locked memory.

- **A root-owned oneshot template, `banlieue-vfio@<uid>.service`**, runs
  `banlieue host cloud-hypervisor vfio-grant`. The provider writes the
  device address to `<state root>/units/vfio-<uid>.env` and starts the
  instance, as it does `banlieue-swtpm-setup@` — but this unit runs as
  **root**, so the file is untrusted input. The grant re-validates against
  the root-owned host config, and refuses unless every check holds:
  - the address is in a declared class, and passes Decision 2's checks now;
  - `/dev/vfio/<group>` is owned by root, or already by this uid (a
    restart): **the node's owner is the lock**, so no uid takes a group
    another holds;
  - the uid is in the guest range (polkit already allows only those).

  It then sets `/dev/vfio/<group>` to `<uid>:<uid> 0600`. `ExecStop`
  returns it to `root:root 0600` only if this uid still owns it.
- The provider cannot do this itself: `/dev` is read-only to it (Decision 6
  of ADR-0063, amended). Root, re-checking against root's own config, is the
  only writer of a group node's owner.
- polkit gains `banlieue-vfio` in the per-uid lifecycle list (start, stop,
  reset-failed), nothing more.
- **The provider starts the grant explicitly** before the VMM unit, as it
  does `banlieue-swtpm-setup@`, rather than through `Requires=`: one start
  path, and the provider sees the grant's own failure.
- `banlieue-ch@.service` gains `DeviceAllow=/dev/vfio/vfio rw` and
  `DeviceAllow=char-vfio rw` (`vfio` is the group nodes' major in
  `/proc/devices`; the container is a misc device, hence its own line). The
  device rule is coarse (every VFIO group node); ownership confines each
  uid to its own group. A guest without a device gains nothing it can open:
  every group node is root `0600` or another uid's. Its read-only `/dev`
  stays: a device node opens read-write on a read-only mount, as
  `/dev/net/tun` already does.
- The grant unit is the one banlieue unit with a writable `/dev`, because
  changing a group node's owner is its whole job; it writes nothing else
  (`ProtectSystem=strict`, no `ReadWritePaths=`).

### 6. Memory lock: `LimitMEMLOCK=infinity`, bounded by `MemoryMax`

`LimitMEMLOCK` is an exec property: the provider cannot set it per guest at
runtime. `banlieue-ch@.service` sets `LimitMEMLOCK=infinity`. The real bound
is the guest's `MemoryMax` (guest memory plus `VMM_MEMORY_OVERHEAD_MIB`),
which the provider sets on every VMM it starts (`plan::vmm_unit`); what a
VMM pins counts against it. A second template only for device guests was
considered and rejected (Alternatives).

### 7. The Landlock rule, until the pinned VMM is fixed

Until banlieue pins a Cloud Hypervisor release that contains
cloud-hypervisor#9405, the provider does not send the sysfs path itself.
It creates, in the guest's run directory:

```
<run>/<uid>/vfio/<address>         -> <run>/<uid>/vfio/group/<group>
<run>/<uid>/vfio/group/<group>     -> /sys/bus/pci/devices/<address>
```

and sends `path=<run>/<uid>/vfio/<address>`. The unfixed rule reads that
link, takes `<group>` as the group, and adds `/dev/vfio/<group>`; the VMM
opens the device through the second link. The fixed rule reads
`<path>/iommu_group`, which resolves through the same links, so the layout
needs no change when the pin moves; it is removed then as dead weight. The
issue reports this layout booting on v54.0 with Landlock and seccomp on;
banlieue's live test confirms it on the pinned version.

banlieue adds no further `landlock_rules` for a device: the VMM's own
`/dev/vfio/vfio` and group rules sufficed on the fixed build in the
reproduction above. The live test (Decision 12) checks this with the guest
driver loaded, and a unit test pins the rule list.

### 8. Assignment and release happen on the host

- **Assign at creation.** The provider picks the lowest free address in the
  class (deterministic), re-checks Decision 2 for it, writes
  `status.devices`, starts the grant, then creates the VM with
  `devices: [{id: <name>, path: …}]`.
- **Power-off keeps the assignment;** the uid keeps the group.
- **Deletion releases it:** stop the guest, stop `banlieue-vfio@<uid>`
  (which returns the node to root), clear `status.devices`.
- **Crash safety.** On start, and each pass, the provider compares each
  declared group node's owner with its machines; a node owned by a guest
  uid that no machine uses is released by stopping that uid's grant unit.
  The provider re-adopts and never trusts its memory (ADR-0063 Decision 2).
- **Reset** between guests is the kernel's (FLR or bus reset), which is why
  Decision 2 refuses a device without one.

### 9. Scheduling

- A new step after network classes: every device's abstract class must
  resolve through the Provider's `deviceClasses`, and the failure domain's
  `capacity.devices` entry for the host class must have `free` at least the
  number requested plus the devices of in-flight VMs there (ADR-0128's
  in-flight rule, applied to devices).
- **A failure domain that publishes no device capacity cannot host a device
  request.** This is the exception to ADR-0107's "not filtered when
  absent": a device is never assumed. It is also what keeps a device class
  off libvirt, vSphere and Proxmox until they implement one.
- Memory needs no change: ADR-0107 already counts every VM's full memory,
  and ADR-0128's in-flight term covers a new guest whose pinned memory the
  measurement has not seen yet.
- Cluster API machines skip the scheduler; Decision 8's host-side
  assignment is what keeps them exclusive, and `NoFreeDevice` is what they
  report.

### 10. Moves are refused

A `VirtualMachineMove` (ADR-0127) of a VM whose class has devices is
refused by the controller with a condition until a later ADR decides how a
device is re-granted on the destination.

### 11. Cluster API

Nothing beyond `spec.devices` on the template. A `MachineDeployment` of
single-device workers with `maxSurge: 1` deadlocks: the surge machine waits
`NoFreeDevice` for a device the old machine holds. The guide and an example
use `maxSurge: 0, maxUnavailable: 1`.

### 12. Tests

- **Unit**, `*_tests.rs` first: host config parsing and refusals; preflight
  over the fake host (`banlieue-host/src/fake.rs`): no IOMMU, not on
  `vfio-pci`, another endpoint on a host driver, a bridge in the group
  (allowed), no reset method, all good; the wire body and Landlock rules
  (snapshot); the symlink layout; assignment (free pick, `NoFreeDevice`,
  idempotent, release, orphan release); `vfio-grant` against a temporary
  directory standing for `/dev/vfio` behind a trait (undeclared, held by
  another uid, not viable: refused, nothing changed); controller
  resolution and `UnresolvedClass`; the scheduler step; CRD immutability.
- **No faked attach.** CI has no device; nothing mocks the VMM's attach end
  to end. A nested-VFIO e2e (an L1 with a virtual IOMMU and an emulated NIC
  bound to `vfio-pci`) is attempted within a time box and otherwise left a
  documented manual procedure; it skips with a reason, never a pass,
  without `/dev/kvm` or a virtual IOMMU.
- **Live**, on a real GPU host, not committed: the `VirtualMachine` path
  and the Cluster API path each give a sandboxed guest the GPU and the
  vendor driver works; exclusivity, power cycle, release and provider
  restart behave as above.

### 13. Milestones

1. API, host config, install checks, inventory and the verified feature.
2. Grant template, assignment and release, wire format, the Landlock
   layout, unit changes.
3. Scheduler and controller, Cluster API docs and example, the live test.

## Alternatives considered

- **Identify devices by `vendor:device`.** Two identical cards would be
  indistinguishable, and the host knows addresses. Kept as possible install
  sugar.
- **Attach the whole IOMMU group.** Hands the guest functions it did not
  ask for (a GPU's USB controller) and makes the guest's device list depend
  on the platform's grouping.
- **A udev group shared by guest uids, plus per-guest `DeviceAllow` by
  `set-property`.** Every guest uid could open every group; exclusivity
  would rest on the provider alone.
- **Pass a pre-opened vfio cdev FD (cloud-hypervisor#8287).** The VMM then
  needs no open permission and skips the per-device Landlock rule, the
  strongest confinement. But the caller of the API socket is the
  unprivileged provider: it would have to hold a live device FD, or a root
  helper would have to speak the VMM's API, and the VMM also needs an
  iommufd FD at `vm.create`. Larger than Decision 5 for no gain the group
  owner lock does not already give. The follow-up direction, also needed
  for moving a guest that holds a device.
- **A second template, `banlieue-ch-vfio@`, for device guests only.** Keeps
  non-device guests' unit unchanged, at the cost of two templates kept in
  step; `MemoryMax` bounds both the same way.
- **Run device guests' VMM as root, or with Landlock off.** Gives up the
  sandbox ADR-0063 exists for.
- **Wait for a Cloud Hypervisor release with the fix.** Blocks the feature
  on an upstream release date for a layout that is forward compatible.

## Consequences

- Hosts declare devices once, at install; reboot-time host setup stays the
  admin's, with preflight naming what is missing.
- Every guest VMM unit gains VFIO device access and unlimited memory lock;
  file ownership and `MemoryMax` are what bound them. A guest without a
  device can open no group node.
- polkit gains one template name; root gains one oneshot whose input it
  re-validates.
- A device guest pins all its memory, and cannot be moved.
- A device used by one tenant's guest and then another's may keep state:
  FLR does not guarantee device memory is cleared. Device classes are not
  for mutually distrusting tenants sharing a device; this is recorded as an
  accepted risk, alongside the namespace scope of ADR-0130 and ADR-0131.
  The device's firmware runs with device-level privilege; out of scope.
- PCI addresses and `vendor:device` appear in `Provider` status. They are
  not secrets.
- The symlink layout of Decision 7 is removed when the pinned Cloud
  Hypervisor contains the fix.
- libvirt, vSphere and Proxmox have their own passthrough mechanisms
  (`<hostdev>`, DirectPath I/O, `hostpci`); none is implemented, and
  Decision 9 keeps device requests off them.
