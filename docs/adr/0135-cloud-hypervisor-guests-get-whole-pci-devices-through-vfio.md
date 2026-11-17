<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0135: Cloud Hypervisor guests get whole PCI devices through VFIO

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
  measured capacity) and
  [ADR-0134](0134-cloud-hypervisor-serves-tenant-namespaces.md) (a host
  serving tenant namespaces, whose guest-uid ledger spans them); constrains
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
- **Only the declared function is attached**, but **access is per group**.
  A GPU's audio, USB and UCSI functions stay bound to `vfio-pci` and
  unattached, as in #190; the VMM that holds the group could still open
  any of them, because VFIO grants a group as a unit. The group, not the
  function, is therefore what a class hands out: install prints every
  endpoint in each declared device's group, and the host guide says that
  declaring a device declares its group.
- **One declared device per IOMMU group.** Install refuses two declared
  devices in the same group, and a device in two classes. The unit of
  assignment is then both a device and a group.

### 2. The host is checked, not configured

banlieue does not bind drivers or edit the bootloader: the IOMMU on the
kernel command line (`intel_iommu=on` or `amd_iommu=on`, with `iommu=pt`)
and `vfio-pci` binding at boot are host prerequisites, documented in the
host guide. They need a reboot, which an idempotent installer must not do.

Install, preflight and selftest refuse, as hard failures that name what is
missing, with no flag to override:

- no IOMMU (`/sys/kernel/iommu_groups` empty);
- **isolation weakened by the host**: `pcie_acs_override` on
  `/proc/cmdline` (it splits groups the hardware does not isolate), or
  `/sys/module/vfio_iommu_type1/parameters/allow_unsafe_interrupts` set
  (VFIO without interrupt remapping). Passthrough guides recommend both;
  with either, the IOMMU no longer confines what the device can reach, and
  the group check below would pass on a group that is not one;
- a declared device that does not exist, is not bound to `vfio-pci`, or has
  no reset method (`/sys/bus/pci/devices/<address>/reset_method` absent or
  empty);
- a group that is not viable: an endpoint in it bound to anything but
  `vfio-pci` (PCI bridges excepted);
- a group node `/dev/vfio/<group>` that is missing, is not a character
  device of the `vfio` major, or is in any state but `root:root` with no
  group or other bits, or `<guest uid>:<guest uid> 0600` (held, Decision
  5). A node another tool manages (a udev rule making group nodes
  `root:kvm 0660`, which every guest VMM could open, or libvirt's
  `<hostdev>` re-owning nodes on its own) is refused, not shared;
- two declared devices in one group, or one device in two classes;
- a device class on a host whose guests slice has no memory ceiling
  (Decision 6).

`--print-vfio-setup` (emit the kernel command line and modprobe files for
the declared devices) is deferred: the guide shows them.

### 3. The provider publishes device inventory, and the feature is verified

- `FailureDomainCapacity` gains `devices: [{deviceClass, total, free,
  vendorDevice}]`, one entry per host class. Counts are integers and never
  overcommitted. `free` counts devices no machine has reserved (Decision 8).
- `Provider.spec.capabilities` gains `deviceClasses: [{name, target:
  {hostClass}, namespaceSelector, maxPerNamespace}]`, the shape of
  `storageClasses` and `networkClasses` plus two limits for a shared host
  (ADR-0133, ADR-0134). A `VMClass` names the abstract class; the scheduler
  resolves it to the host class, as it does storage and network classes.
  - `namespaceSelector`: which of the namespaces this host serves may use
    the class. Absent, only the `Provider`'s home namespace may, as an
    absent serving selector means home only (ADR-0134 Decision 2). A
    machine elsewhere gets `DeviceClassNotAllowed`.
  - `maxPerNamespace`: how many of the class's devices machines of one
    namespace may hold together, counting powered-off machines. Absent,
    no limit. A machine past it gets `NamespaceDeviceLimit` and retries.
  - Both are enforced by the provider at assignment (so Cluster API
    machines, which skip the scheduler, are bound too) and by the
    scheduler's device step. `ResourceQuota` cannot do this: it does not
    count a device named inside a CR's spec.
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
  address, iommuGroup}]`, the machine's **reservation**, written by the
  provider (Decision 8). It is the source of truth for who holds a device.
- **Condition `DevicesAssigned`**: `True` only once the grant has
  succeeded. Reasons `NoFreeDevice` and `NamespaceDeviceLimit` (retry),
  `UnknownDeviceClass`, `DeviceClassNotAllowed`, `DeviceNotUsable` (the
  device fails a Decision 2 check now) and `GrantFailed` (retry). A machine
  that waits does not start.
- **Pinned memory.** A machine with a device has all its memory pinned. The
  field docs say so; `hugepages` stays optional.

### 5. Root grants the guest uid one group, and nothing else can

The VMM runs as the guest's uid under `DevicePolicy=closed`, Landlock and
`ProtectSystem=strict`. It needs its group node, the container
`/dev/vfio/vfio`, and enough locked memory.

- **A root-owned oneshot template, `banlieue-vfio@<uid>.service`**, with
  `RemainAfterExit=yes`, so the grant is held while the unit is active.
  `ExecStart=banlieue host cloud-hypervisor vfio-grant --uid %i`,
  `ExecStop=banlieue host cloud-hypervisor vfio-release --uid %i`.
- **No provider input reaches root's environment.** Unlike
  `banlieue-swtpm-setup@`, which runs as the provider's own user, this unit
  has **no `EnvironmentFile=`**: a provider-written env file read by a
  dynamically linked binary running as root would let the provider set
  `LD_PRELOAD`, `LD_AUDIT` or `GCONV_PATH` and run code as root before
  `main`, and would let it set the host CLI's `BANLIEUE_HOST_*` variables,
  which change what root checks against (the guest uid range among them).
  `vfio-grant` and `vfio-release` define no `env =` arguments, take only
  `--uid`, and read the host config only from its fixed path, refusing it
  unless it is a regular file owned by root and writable by no one else.
- **The request is read as data.** The provider writes the address it
  picked to `<state root>/units/vfio-<uid>.request`. The grant opens it
  with `O_NOFOLLOW | O_NOCTTY | O_CLOEXEC`, refuses it unless `fstat`
  shows a regular file owned by the provider's user with one link, reads at
  most a small fixed size, and parses exactly one line `address=DDDD:BB:DD.F`;
  anything else is refused. Then every check must hold:
  - the uid is in the guest range of root's host config;
  - the address is in a declared class, and passes Decision 2's checks now;
  - the group node is `root:root` with no group or other bits, or already
    `<uid>:<uid> 0600` (a restart); any other state is refused, never
    repaired. **The node's owner is the host-side lock**: no uid takes a
    group another holds;
  - no other declared group node is owned by this uid (one group per guest
    for now).

  It then sets the node to `<uid>:<uid> 0600` through a handle opened
  without following links and re-checked as a `vfio` character device, and
  creates the Landlock layout of Decision 7.
- **Release reads nothing the provider wrote.** `vfio-release` walks the
  declared devices in root's own config and returns every group node owned
  by this uid to `root:root 0600`, then removes the layout. Stopping the
  unit is the only way the provider releases a device, and it cannot name
  a group that is not this uid's.
- **The unit is hardened** to what changing an owner needs:
  `User=root`, `CapabilityBoundingSet=CAP_CHOWN CAP_FOWNER`,
  `NoNewPrivileges=yes`, `ProtectSystem=strict`, `ProtectHome=yes`,
  `PrivateTmp=yes`, `PrivateNetwork=yes`, `RestrictAddressFamilies=none`,
  `ProtectKernelTunables=yes` (sysfs stays readable), `ProtectKernelModules=yes`,
  `ProtectControlGroups=yes`, `LockPersonality=yes`,
  `MemoryDenyWriteExecute=yes`, `SystemCallArchitectures=native`,
  `SystemCallFilter=@system-service`, `DevicePolicy=closed` with no
  `DeviceAllow=` (changing a node's owner does not open it),
  `ReadOnlyPaths=/dev` with `ReadWritePaths=/dev/vfio <run root>/vfio`, and
  nothing else writable. A unit test pins these lines.
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
  uid to its own group, which is why Decision 2 refuses any node mode that
  opens a group to more than its owner. A guest without a device gains
  nothing it can open. Its read-only `/dev` stays: a device node opens
  read-write on a read-only mount, as `/dev/net/tun` already does.

### 6. Memory lock: `LimitMEMLOCK=infinity`, bounded by root's slice ceiling

`LimitMEMLOCK` is an exec property: the provider cannot set it per guest at
runtime. `banlieue-ch@.service` sets `LimitMEMLOCK=infinity`. Pinning
allocates nothing: the pages VFIO pins are the guest memory the VMM has
already been charged for, so they count against the VMM's memory cgroup.

- **Per guest**, the bound is the guest's `MemoryMax` (guest memory plus
  `VMM_MEMORY_OVERHEAD_MIB`), set by the provider on every VMM it starts
  (`plan::vmm_unit`). The provider sets it, so it bounds an honest
  provider's guests, not a compromised one.
- **On the host**, the bound is the guests slice's `MemoryMax`
  (`--guest-memory-max`, ADR-0105), which root writes into a root-owned
  unit and the provider cannot change. Install refuses a device class
  without it (Decision 2). Memory a guest pins can never be reclaimed by
  the host, so this ceiling is what keeps the host itself alive.

A second template only for device guests was considered and rejected
(Alternatives): the provider chooses which template to start, so it would
bound nothing the slice does not.

### 7. The Landlock rule, until the pinned VMM is fixed

Until banlieue pins a Cloud Hypervisor that contains
cloud-hypervisor#9405, the provider does not send the sysfs path itself.
The **grant** (root) creates, under a root-owned directory the guest can
read but not write:

```
<run root>/vfio/<uid>/<address>        -> <run root>/vfio/<uid>/group/<group>
<run root>/vfio/<uid>/group/<group>    -> /sys/bus/pci/devices/<address>
```

and the provider sends `path=<run root>/vfio/<uid>/<address>`. The unfixed
rule reads that link, takes `<group>` as the group, and adds
`/dev/vfio/<group>`; the VMM opens the device through the second link. The
fixed rule reads `<path>/iommu_group`, which resolves through the same
links, so the layout needs no change when the pin moves; it is removed then
as dead weight.

- **Not in the guest's run directory.** `<run>/<uid>` is owned by the
  guest uid; links there could be swapped by the guest between the grant's
  checks and the VMM's open. Root creates each directory with `mkdirat`
  and each link with `symlinkat` relative to handles opened with
  `O_DIRECTORY | O_NOFOLLOW` from a root-owned parent, after removing
  whatever was there; `vfio-release` removes them the same way.
- **A gate on the pinned version.** The issue reports this layout booting
  on v54.0, not on the pinned v53.0. Milestone 2 starts by booting it on
  v53.0 on a real GPU host, with Landlock and seccomp on. If it does not
  boot, the layout is dropped and the pin moves to a Cloud Hypervisor build
  that contains cloud-hypervisor#9405 before milestone 2 continues; the
  rest of this ADR does not change.

banlieue adds no further `landlock_rules` for a device: the VMM's own
`/dev/vfio/vfio` and group rules sufficed on the fixed build in the
reproduction above. The live test (Decision 12) checks this with the guest
driver loaded, and a unit test pins the rule list.

### 8. Assignment and release happen on the host

**The source of truth is `status.devices` across this host's machines**,
not the group node's owner. Device nodes are recreated `root:root 0600`
when `vfio-pci` binds at boot, so ownership does not survive a reboot; a
reservation in the API does. The node owner is the second, host-side check
(Decision 5), so a mistake in one is caught by the other.

- **Which machines count.** This host's machines in every namespace it
  serves, listed the way the guest-uid ledger lists them (ADR-0134
  Decision 5). A machine in a namespace that stops being served keeps its
  reservation until it is deleted, the safe direction, as it keeps its uid.
- **Assign, serialised.** All assignment on a host goes through one lock in
  the provider, so two reconciles cannot pick the same device. Under it the
  provider takes the set of declared addresses, removes every address in
  any counted machine's `status.devices` and every group node a guest uid
  owns, checks `namespaceSelector` and `maxPerNamespace` (Decision 3), and
  picks the lowest remaining address (deterministic). It re-checks Decision
  2 for it and writes `status.devices` with the machine's
  `resourceVersion` as a precondition; a conflict starts over.
- **Then grant.** It writes the request, starts `banlieue-vfio@<uid>`,
  sets `DevicesAssigned=True` only when the grant succeeds, and only then
  creates the VM with `devices: [{id: <name>, path: …}]`. A failed grant
  keeps the reservation and reports `GrantFailed` (retry); a grant refused
  because another uid holds the node clears the reservation and assigns
  again.
- **Every start re-grants.** Before each VMM start (first boot, power-on,
  provider restart, host reboot) the provider starts the grant again for
  the reserved address; on an unchanged node it is a no-op.
- **Power-off keeps the reservation**; after a reboot the node is root's
  again until the machine next starts, and no other machine can reserve it
  meanwhile.
- **Deletion releases it:** stop the guest, stop `banlieue-vfio@<uid>`
  (which returns the node to root), and clear `status.devices` only once
  the stop succeeded. A release that fails keeps the finalizer and retries.
- **A uid is not reused while it holds anything.** The guest-uid ledger
  treats a uid as in use while any declared group node is owned by it or
  its `banlieue-vfio@` instance is active or failed, so a new machine never
  inherits a device whose release failed.
- **Orphan release is careful.** A node owned by a guest uid with no
  counted machine is released by stopping that uid's grant unit, but only
  when all of these hold: the machine watch has completed its initial list
  (never from a cache that has not synced), the list came from the same
  scope the ledger uses, no counted machine carries that uid, and the uid's
  `banlieue-ch@` instance is inactive. Otherwise it is logged and left. A
  stale grant on an idle uid costs a device until the next pass; taking a
  group from a running VMM costs the guest.
- **Waiters are woken.** Releasing a device re-queues this host's machines
  waiting with `NoFreeDevice` or `NamespaceDeviceLimit` for that class, and
  waiting machines also requeue on a bounded backoff, so a Cluster API
  machine, which no scheduler will re-place, does not wait on an event it
  missed.
- **Reset** between guests is the kernel's (FLR or bus reset), which is why
  Decision 2 refuses a device without one.

### 9. Scheduling

- A new step after network classes: every device's abstract class must
  resolve through the Provider's `deviceClasses`, the VM's namespace must
  match the class's `namespaceSelector`, and, **for a new placement only**,
  the failure domain's `capacity.devices` entry for the host class must
  have `free` at least the number requested plus the devices of in-flight
  VMs there (ADR-0128's in-flight rule, applied to devices). A placed VM
  is not re-checked against `free`: its own device is already out of it,
  and checking would find its own host full, fail its placement and recreate
  it in a loop (ADR-0125: a placed VM keeps a placement that still fits).
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
  over the fake host (`banlieue-host/src/fake.rs`): no IOMMU,
  `pcie_acs_override`, unsafe interrupts, not on `vfio-pci`, another
  endpoint on a host driver, a bridge in the group (allowed), no reset
  method, a group node `root:kvm 0660`, no slice memory ceiling, all good;
  the wire body and Landlock rules (snapshot); the symlink layout (created
  under root's directory, a pre-existing link replaced, never followed);
  assignment (free pick, two concurrent picks get different devices,
  `NoFreeDevice`, `NamespaceDeviceLimit`, `DeviceClassNotAllowed`,
  idempotent, release, a failed release keeping the uid out of the ledger,
  orphan release refused before sync and while the VMM is active, waiters
  re-queued); `vfio-grant` and `vfio-release` against a temporary directory
  standing for `/dev/vfio` behind a trait (undeclared, held by another uid,
  a request that is a symlink or not the provider's, a malformed request,
  not viable: refused, nothing changed); the grant and VMM unit files
  (no `EnvironmentFile=`, the hardening lines, `RemainAfterExit=yes`); no
  `env =` argument on `vfio-grant` or `vfio-release`; controller resolution
  and `UnresolvedClass`; the scheduler step, including a placed VM on a
  full host keeping its placement; CRD immutability.
- **No faked attach.** CI has no device; nothing mocks the VMM's attach end
  to end. A nested-VFIO e2e (an L1 with a virtual IOMMU and an emulated NIC
  bound to `vfio-pci`) is attempted within a time box and otherwise left a
  documented manual procedure; it skips with a reason, never a pass,
  without `/dev/kvm` or a virtual IOMMU.
- **Live**, on a real GPU host, not committed: the `VirtualMachine` path
  and the Cluster API path each give a sandboxed guest the GPU and the
  vendor driver works; exclusivity, power cycle, host reboot, release and
  provider restart behave as above.

### 13. Milestones

1. API, host config, install checks, inventory and the verified feature.
2. The Decision 7 gate on the pinned VMM; grant template, assignment and
   release, wire format, the Landlock layout, unit changes.
3. Scheduler and controller, Cluster API docs and example, the live test.

## Alternatives considered

- **Identify devices by `vendor:device`.** Two identical cards would be
  indistinguishable, and the host knows addresses. Kept as possible install
  sugar.
- **Attach the whole IOMMU group.** Hands the guest functions it did not
  ask for (a GPU's USB controller) and makes the guest's device list depend
  on the platform's grouping. Access is per group either way (Decision 1).
- **A udev group shared by guest uids, plus per-guest `DeviceAllow` by
  `set-property`.** Every guest uid could open every group; exclusivity
  would rest on the provider alone.
- **Pass the request in an `EnvironmentFile=`, as `banlieue-swtpm-setup@`
  does.** Safe there only because that unit runs as the provider's user;
  as root it hands the provider the dynamic loader and the CLI's settings
  (Decision 5).
- **Put the address in the instance name** (`banlieue-vfio@<uid>-<address>`).
  No file at all, but the address needs systemd escaping, the polkit rule
  would have to parse it, and release would still have to find the
  instance by uid. A request read as data, with root's config deciding,
  is simpler to check.
- **Node ownership as the only record.** Lost at every reboot (Decision 8).
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
  step; the provider chooses the template, so the slice ceiling is the
  bound either way.
- **Run device guests' VMM as root, or with Landlock off.** Gives up the
  sandbox ADR-0063 exists for.
- **Wait for a Cloud Hypervisor release with the fix.** Blocks the feature
  on an upstream release date for a layout that is forward compatible;
  Decision 7's gate moves the pin instead if the layout fails on v53.0.

## Consequences

- Hosts declare devices once, at install; reboot-time host setup stays the
  admin's, with preflight naming what is missing and refusing setups that
  weaken the IOMMU.
- Every guest VMM unit gains VFIO device access and unlimited memory lock;
  node ownership and the slice ceiling are what bound them. A guest without
  a device can open no group node.
- polkit gains one template name; root gains one hardened oneshot that
  reads one small file as data and takes nothing from its environment.
- A device guest pins all its memory, and cannot be moved.
- **A guest holds its whole group.** Functions declared with the device's
  group are reachable by that guest's VMM even when not attached.
- **Hoarding within the limits.** A namespace allowed a class can hold up
  to `maxPerNamespace` of its devices indefinitely, powered off or not
  (Cluster API machines included). With no limit set, one tenant can hold
  every device of the class. Recorded as an accepted risk; revisit when
  device requests need quota beyond a per-namespace cap.
- A device used by one tenant's guest and then another's may keep state:
  FLR does not guarantee device memory is cleared. Device classes are not
  for mutually distrusting tenants sharing a device; this is recorded as an
  accepted risk, alongside the namespace scope of ADR-0130 and ADR-0131 and
  the shared hosts of ADR-0133 and ADR-0134, where one host's devices can
  serve several tenants' namespaces; `namespaceSelector` narrows which.
  The device's firmware runs with device-level privilege; out of scope.
- A host cannot share a declared device with libvirt, or with a udev rule
  that re-owns VFIO nodes: preflight refuses the node states they leave.
- PCI addresses and `vendor:device` appear in `Provider` status. They are
  not secrets.
- The symlink layout of Decision 7 is removed when the pinned Cloud
  Hypervisor contains the fix.
- libvirt, vSphere and Proxmox have their own passthrough mechanisms
  (`<hostdev>`, DirectPath I/O, `hostpci`); none is implemented, and
  Decision 9 keeps device requests off them.
