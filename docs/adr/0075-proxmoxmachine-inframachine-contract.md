<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0075 — `ProxmoxMachine`: the InfraMachine contract on Proxmox VE

- **Status:** Accepted
- **Date:** 2026-09-28
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-09-28 (Decision 4: ownership is the machine UID in the VM
  description, not its name; Decision 5: a static address needs a seed even
  without `userData`)
- **Related:** [ADR-0074](0074-banlieue-proxmox-rest-client.md) (the client
  this machine is driven through), [ADR-0005](0005-capi-contract-label-codegen.md)
  (contract label), [ADR-0054](0054-nocloud-seed-iso-first-party.md) (the seed
  ISO), [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md) and
  [ADR-0050](0050-libvirtmachine-domain-lifecycle.md) (the siblings),
  [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md) (the
  resolved-spec pattern); [roadmap 06](../../.github/community/06-phase-1c-proxmox-provider.md).

## Context

Non-negotiable 2: every provider's infra CRD satisfies the CAPI v1beta2
InfraMachine contract, with a `*Template` kind beside it. Proxmox sits
between the two existing shapes:

- Like **vSphere**, the provider holds an API credential and talks to a
  management endpoint; nothing it is told is a host path. A node, storage id
  and bridge name are *names the API validates*, so the controller can
  resolve them into the spec and the provider acts on exactly those.
- Like **libvirt**, the hypervisor keeps the VM: a VMID and a config file
  outlive any process, so the machine can be found again after a crash.

What is distinctive:

- **The VMID is allocated by the cluster and is racy.** `GET /cluster/nextid`
  hands two callers the same id until one clones.
- **Every mutation is a task** (ADR-0074 Decision 7); a 200 means queued.
- **No snippet upload.** The API accepts `iso`, `vztmpl` and `import` content
  only (ADR-0074 Consequences), so roadmap 06's `cicustom` snippet cannot be
  delivered through it.
- **Clone source is a template VM**, prepared by an administrator
  (`scripts/bootstrap-proxmox-host.sh` can create one).

## Decision

### 1. `ProxmoxMachine` and `ProxmoxMachineTemplate`

In `crates/banlieue-api/src/infrastructure/proxmox_machine.rs`, group
`infrastructure.banlieue.io/v1alpha1`, with the CAPI fields the siblings
carry (`providerID`, `failureDomain`, `status.initialization.provisioned`,
`status.addresses`, `status.conditions`) and the contract label emitted by
`crdgen` (ADR-0005). The template is the `template.spec` wrapper, as in
ADR-0050 Decision 2.

### 2. Identity: `proxmox://<provider-name>/<vmid>`

The provider name rather than a node, because a hostname is a deployment
detail (ADR-0050 Decision 1) and a VM can move between nodes; the VMID
because it is unique cluster-wide and survives a move. Roadmap 06's draft
`proxmox://<vmid>@<node>` is superseded for that reason.

### 3. Spec: fully resolved, like `VSphereMachine`

```rust
pub struct ProxmoxMachineSpec {
    pub provider_id: Option<String>,
    pub failure_domain: Option<String>,
    pub provider_ref: LocalObjectReference,

    pub node: String,                 // resolved from the failure domain
    pub template_vmid: u32,           // clone source; must be a template
    pub storage: String,              // resolved from the storage class
    pub pool: Option<String>,         // Proxmox resource pool

    pub cores: u32,
    pub sockets: u32,                 // default 1
    pub memory_mi_b: u32,
    pub cpu_type: Option<String>,

    pub firmware: Firmware,           // Bios -> seabios; Efi/EfiSecure -> ovmf
    pub tpm_enabled: bool,            // tpmstate0 on `storage`, v2.0
    pub os_disk_size_gi_b: u32,       // grown to this, never shrunk
    pub data_disks: Vec<ProxmoxDataDisk>,
    pub nics: Vec<ProxmoxNicSpec>,    // bridge, vlan, model, mac, ipam

    pub iso_storage: Option<String>,  // where the NoCloud seed goes
    pub user_data: Option<String>,    // already resolved (ADR-0025/0038)
    pub desired_power_state: PowerState,
}
```

`bridge`, `node` and `storage` are cluster-supplied strings, and that is
acceptable here: unlike a Cloud Hypervisor host (ADR-0062 Decision 4) the
provider is not a privileged process acting on a local path. Each value is
a name the Proxmox API checks against its own inventory and the token's
ACLs, so an unknown or unauthorised name is refused by Proxmox, not
interpreted by banlieue. `iso_storage` must be set whenever `userData` is.

No `vmid` in the spec. The controller does not choose it.

### 4. VMID allocation is persisted before it is used

The provider records the VMID in **`status.vmid`** *before* cloning, then
clones onto exactly that id. Sequence:

1. If `status.vmid` is set and the VM there is ours, use it. If no VM is
   there yet, the previous pass died before cloning: reuse the id.
2. Otherwise look for a VM **named after the machine** that is ours (below):
   a previous attempt that cloned but crashed before patching status is
   adopted, not duplicated.
3. Otherwise `nextid`, patch `status.vmid`, then clone.

**Ownership is the machine's UID, not its name.** The clone carries a
`description` holding the marker line `banlieue-machine-uid=<uid>`, set
atomically by the clone itself, so there is no window in which a banlieue VM
exists unmarked. A VM is ours only if a whole line of its description equals
that marker (an exact match, so `abc` never claims `abcd`). The VM's *name* is
the machine's name for humans, and is deliberately **not** the test:
Kubernetes names are unique per namespace while Proxmox names are unique per
nothing, so two namespaces' `web-1`, or an administrator's own VM called
`web-1`, would otherwise be adopted, reconfigured and finally destroyed by a
machine that never created them. A VM that carries the name but not the
marker is left completely alone; the machine clones a fresh one beside it
(Proxmox permits duplicate names). Deletion applies the same test and never
touches a VM that is not ours.

If two controllers collide on `nextid`, Proxmox refuses the second clone
("config file already exists"); that surfaces as a reconcile error and the
retry abandons `status.vmid` when the VM at that id is not ours. Status is a
cache, not the source of truth. `status.vmid` and not `spec` because the
controller owns the spec's field manager; a provider write there would
contend under server-side apply.

### 5. Cloud-init is a NoCloud ISO, not `cicustom`

`userData` is rendered into a NoCloud `CIDATA` ISO with the first-party
builder (ADR-0054), uploaded to `isoStorage` (a storage dedicated to seeds,
`banlieue-seed` by default; ADR-0074 Decision 4, amended) as
`banlieue-<machine-uid>.iso`
and attached as a CD-ROM (`ide2`). It replaces roadmap 06's snippet
delivery, which the API cannot do (ADR-0074). Consequences that follow:

- the ISO is an artifact to clean up: deleting the machine deletes the
  volume (`DELETE …/content/{volid}`), and a delete that finds it already
  gone succeeds;
- the guest image must run cloud-init with the NoCloud datasource, which
  every cloud image does;
- static addressing travels in `network-config` inside the seed (ADR-0024,
  ADR-0033), so `ipconfigN` is not used and there is one path for DHCP and
  static;
- a machine with a static address needs a seed even with no `userData`,
  because the address travels in its `network-config`; `isoStorage` is
  therefore required in that case too, and NIC MACs are derived from the
  machine UID so `network-config` can match them (amended 2026-09-28);
- the seed stays attached for the machine's life in v1; detaching it after
  first boot belongs with a `Deferred`-style install, which is out of scope.

### 5a. Template requirements

`templateVmid` must name a **template** (`template: 1`) on the machine's
node or on shared storage. The provider verifies this before cloning and
fails with a message naming the VMID rather than letting Proxmox clone a
running guest. Full clones only (ADR-0074 `CloneParams`): a linked clone
would tie a guest's disk to its template's lifetime.

### 6. Status

`initialization`, `failureDomain`, `addresses`, `conditions`,
`observedGeneration` as in the contract, plus:

- `vmid` and `node` (where the VM was last seen; a migration changes it);
- `observedPowerState`, `tpmAttached`, mirrored as on the siblings so
  `status_mirror.rs` treats all backends alike;
- `addressSource`: `Static` (IPAM, known before boot) or `GuestAgent`
  (`agent/network-get-interfaces`, requires the agent in the template and
  `agent=1` in the config, which the provider sets).

`provisioned` becomes true only when the VM exists, is configured and has
reached its desired power state (non-negotiable 6); a machine whose agent
never reports is provisioned but has no dynamic addresses.

### 7. Lifecycle

- **Create:** Decision 4, clone (task), configure (`PUT config`: cores,
  memory, `bios`, `machine`, NICs, `agent=1`, seed CD-ROM), grow the OS
  disk, then start (task).
- **Update:** `PUT config` for CPU, memory and NICs; `resize` for disk
  growth; storage, node and template are immutable (admission policy, as
  for the siblings).
- **Delete:** stop if running (task), delete the VM with
  `purge` + `destroy-unreferenced-disks` (task), delete the seed volume,
  then release the finalizer. A missing VM or volume is success.

### 8. Controller dispatch

`banlieue-controller` gains `PROVIDER_CLASS_PROXMOX = "proxmox"`, a builder
beside `build_vsphere_machine`, and an `InfraMachineRead` impl in
`status_mirror.rs`. The scheduler needs no change.

## Consequences

- Two more CRDs and two more CRD-schema tests; `deploy/crds/` and the API
  reference regenerate.
- Nothing about a VM can be recovered from `status` alone: the UID marker in
  the VM's description is load-bearing (Decision 4). Renaming the VM in the
  Proxmox UI is harmless; **deleting the marker line orphans it**, and the
  machine will clone a new one. A `banlieue-machine=<uid>` tag would make the
  marker visible in `cluster/resources` without a config read, and is the
  follow-up if the per-candidate read ever matters.
- The provider needs `VM.Config.CDROM` and `Datastore.AllocateTemplate`
  (already in ADR-0074 Decision 4's role) and nothing else new.
- v1 leaves the seed attached and offers no live migration or snapshots
  (roadmap 06 scope).

## Alternatives considered

- **`proxmox://<vmid>@<node>`.** Encodes a location that changes on migration.
- **`spec.vmid`.** Lets the controller pick, but then two VMs can be asked
  for the same id and the controller must know the cluster's free ids —
  Proxmox's job. Also contends on the spec's field manager.
- **`cicustom` snippets.** Not deliverable through the API (ADR-0074).
- **Built-in cloud-init keys (`ciuser`, `sshkeys`, `ipconfigN`).** Cannot
  carry arbitrary `user-data`, which banlieue's resolved payload is
  (ADR-0025), and would split static and DHCP into two mechanisms.
