<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0062 — `CloudHypervisorMachine`: the InfraMachine contract on Cloud Hypervisor

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0050](0050-libvirtmachine-domain-lifecycle.md) (the
  `LibvirtMachine` this mirrors), [ADR-0005](0005-capi-contract-label-codegen.md)
  (contract label), [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (host-resident provider), [ADR-0061](0061-banlieue-cloud-hypervisor-vmm-client.md)
  (VMM client), [ADR-0063](0063-cloud-hypervisor-host-supervision.md)
  (supervision), [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md)
  (vTPM and `Deferred`); roadmap 09 phases 2 and 5.

## Context

Non-negotiable 2: every provider's infra CRD satisfies the CAPI v1beta2
InfraMachine contract, with a `*Template` kind alongside. ADR-0050 did this
for libvirt; its shape carries over. What differs on Cloud Hypervisor:

- **There is no persistent VM object on the host.** libvirt keeps a domain
  with a UUID; a Cloud Hypervisor VM exists only while its process does.
  The machine's identity has to come from the cluster.
- **Host facts are local paths.** Firmware, state directories, storage
  directories and bridges are paths and interface names on one host. The
  provider acts on them with host privileges (ADR-0063).
- **Boot order is disk order.** With no persistent UEFI variables, the
  firmware boots the first bootable disk (roadmap 09, Gotchas).
- **The phase 0 spike found three conditions for a working first boot:**
  the OS disk must be grown before first boot (Kairos's reset layout needs
  about 9 GiB beyond the image, and without it the guest stays in recovery
  with no user-data applied); every disk needs an explicit raw image type;
  and the seed is found by filesystem label only, because there is no
  CD-ROM device.

## Decision

### 1. `CloudHypervisorMachine` and `CloudHypervisorMachineTemplate`

In `crates/banlieue-api/src/infrastructure/cloud_hypervisor_machine.rs`,
group `infrastructure.banlieue.io/v1alpha1`, with the same CAPI fields as
`LibvirtMachine` (`providerID`, `failureDomain`,
`status.initialization.provisioned`, `status.addresses`,
`status.conditions`) and the contract label emitted by `crdgen`
(ADR-0005). The template is the `template.spec` wrapper, exactly as
ADR-0050 Decision 2.

### 2. Identity is the machine UID

`providerID` is `cloudhypervisor://<provider-name>/<machine-uid>`. The
provider name rather than a hostname, as in ADR-0050 Decision 1. The UID
because nothing on the host outlives the VMM process. The UID also names
the systemd units, the tap device and the state directory (ADR-0063), so
every host object traces back to exactly one CR.

### 3. Spec

```rust
pub struct CloudHypervisorMachineSpec {
    pub provider_id: Option<String>,
    pub failure_domain: Option<String>,
    pub provider_ref: LocalObjectReference,

    pub cpus: ChCpuSpec,             // boot, max (max >= boot; hotplug headroom)
    pub memory: ChMemorySpec,        // size_mi_b, hotplug_mi_b, hugepages: bool
    pub storage_class: String,       // a storage target the host declares
    pub boot_source: ChBootSource,   // Image { vmimage } | InstallMedia { vmimage }
    pub os_disk_size_gi_b: u32,      // >= image size; grown before first boot
    pub data_disks: Vec<ChDataDiskSpec>,
    pub nics: Vec<ChNicSpec>,        // network_class (a host bridge), mac: Option
    pub tpm_enabled: bool,
    pub user_data: Option<String>,   // already resolved (ADR-0025, ADR-0038)
    pub desired_power_state: PowerState,
}
```

The provider, not the user, orders the disks: OS disk first, install
media next (`InstallMedia` only, ADR-0065), NoCloud seed next, then data
disks. So the spec has no ordered disk list for anyone to get wrong. Every
disk is `image_type=raw` (ADR-0061 Decision 4).

`os_disk_size_gi_b` is required and must be at least the image's size.
The provider grows the disk with `ftruncate` before first boot and never
shrinks it. The spike showed this is required for a Kairos `cloudImage`,
not an optimisation.

### 4. Host paths stay on the host

Firmware path, state root, the swtpm binary, and the directory and bridge
behind each storage and network class live in a **host-local config file**
written at bootstrap (for example `/etc/banlieue/cloud-hypervisor.toml`).
They are **not** in any CR.

A machine names a `storage_class` and `network_class`. The provider
resolves them through the host config and publishes the names it serves
as the failure domain's `availableStorageClasses` and
`availableNetworkClasses`, which the scheduler already reads.

The reason is the credential ADR-0060 puts on the host. If paths came from
the cluster, anyone who can write a `Provider` or a machine could point a
privileged host process at `/etc`. With names resolved locally, a
cluster-side compromise can choose among targets the host owner declared,
and nothing else.

### 5. Status

`initialization`, `failureDomain`, `addresses`, `conditions`,
`observedGeneration` as in the contract, plus:

- `addressSource`: `Static` (known before boot from IPAM, ADR-0024 and
  ADR-0033) or `Neighbour` (the host's neighbour table for the NIC's MAC,
  read over netlink). There is no guest agent source. The spike confirmed
  the neighbour table sees a DHCP guest on the bridge.
- `observedPowerState`, `guestInstalled`, `installMediaDetached`,
  `tpmAttached`, `tpmEndorsementCertificates`, with the meanings ADR-0043,
  ADR-0044 and ADR-0045 give them on libvirt, so `status_mirror.rs` treats
  both backends alike.
- `hostUid`: the unprivileged uid the guest's units run as (ADR-0063).

### 6. Controller dispatch

`banlieue-controller`'s `infra.rs` gains `PROVIDER_CLASS_CLOUD_HYPERVISOR =
"cloud-hypervisor"` and a builder beside `build_libvirt_machine`, and
`status_mirror.rs` gains an `InfraMachineRead` impl. The scheduler needs no
change: capabilities arrive through the failure domain as for every other
class. `FEATURE_VTPM` is advertised only when the host has swtpm
(ADR-0065).

### 7. Power and migration

- `desiredPowerState: On` means the guest's unit is running and the VM is
  booted.
- `Off` means `vm.power-button`, a bounded wait, then `vm.shutdown` and
  stopping the unit.
- A guest-initiated reboot is handled by the VMM in place and is not a
  power transition (roadmap 09, Gotchas).
- `migrationPolicy` other than `Never` behaves as on libvirt: recreate,
  per ADR-0036, until roadmap 14 covers this class.

### 8. Deletion

The finalizer removes, in order, and verifies each by re-listing (the
ADR-0050 lesson):

1. the VMM unit and the swtpm unit;
2. the tap device;
3. the seed and disk files;
4. the state directory.

A step that fails keeps the finalizer and reports a condition. It is never
skipped.

## Consequences

**Positive**

- Same contract, same status vocabulary as libvirt. CAPI and banlieue's own
  controller consume it without special cases.
- Disk order, image type, disk growth and the seed's place are the
  provider's job. The spike's first-boot failures cannot be configured back
  in.
- Host paths never cross the cluster boundary.

**Negative / accepted costs**

- A host-local config file is one more thing to install and keep in step
  with what the `Provider` advertises. Mitigated by the provider reporting
  what it actually resolved, and a `Ready=False` condition on a machine
  naming a class the host does not declare.
- No ordered disk list means unusual layouts (several bootable disks) are
  not expressible. Accepted: this class is for virtio-only sandbox guests
  (roadmap 09, "What it is not").
- One more CRD pair to generate, document and keep in sync with
  `LibvirtMachine`'s shared fields.

**Follow-ups**

- `make crds`, API reference, examples, and the controller's dispatch arm,
  all TDD per the ADD cycle.
- Decide whether data disks and hotplug headroom ship in the first release
  or wait for roadmap 09 phase 8.
