# Phase 1C — Proxmox Provider

> **Goal.** A `banlieue-provider-proxmox` binary that watches a new
> `ProxmoxMachine` CRD and the corresponding `Provider`s of class
> `proxmox`, talking to Proxmox VE via its REST API.
>
> **Stop condition.** A `VirtualMachine` referencing a real Proxmox
> cluster can be provisioned, customized via cloud-init, and torn
> down. Capability introspection populates the Provider's failure
> domains based on Proxmox node + storage layout.

## Preconditions

- Phase 1A complete.
- Phase 1B complete (the patterns established there inform this one;
  diff intentionally).
- A test Proxmox VE installation (single node or cluster) reachable
  from the dev cluster.
- An Ubuntu cloud-init template prepared in Proxmox (or a recipe to
  generate one from `importFrom`).

## Add to banlieue-api

New CRD in `crates/banlieue-api/src/infrastructure/proxmox_machine.rs`:

- `ProxmoxMachine` (namespaced) — InfraMachine contract compliant.
- `ProxmoxMachineTemplate` (namespaced).

Fields specific to Proxmox:

```rust
pub struct ProxmoxMachineSpec {
    pub provider_id: Option<String>,    // proxmox://<vmid>@<node>
    pub failure_domain: Option<String>,

    pub provider_ref: LocalObjectReference,

    pub template_vmid: u32,             // template to clone
    pub node: String,                   // target node
    pub vmid: Option<u32>,              // explicit; default: nextid
    pub pool: Option<String>,           // Proxmox resource pool

    pub cores: u32,
    pub sockets: u32,                   // default 1
    pub memory_mi_b: u32,
    pub cpu_type: Option<String>,       // host, kvm64, etc.

    pub disks: Vec<ProxmoxDiskSpec>,    // includes storage selector
    pub network: Vec<ProxmoxNicSpec>,
    pub firmware: Firmware,
    pub machine_type: Option<String>,   // q35, pc-i440fx
    pub bios: Option<String>,           // ovmf or seabios (often derived from firmware)
}

pub struct ProxmoxDiskSpec {
    pub name: String,
    pub size_gi_b: u32,
    pub storage: String,                // resolved storage name on Proxmox
    pub format: Option<String>,         // qcow2, raw, vmdk
    pub iothread: Option<bool>,
    pub discard: Option<bool>,
    pub ssd: Option<bool>,
}

pub struct ProxmoxNicSpec {
    pub name: String,
    pub bridge: String,                 // resolved (e.g. vmbr0)
    pub vlan: Option<u16>,
    pub model: Option<String>,          // virtio (default), e1000, ...
    pub mac_address: Option<String>,
    pub ipam: IpamSpec,
}
```

After adding the types, rerun `cargo run --bin crdgen --features
crdgen` and commit the regenerated CRD YAML.

## Crate

```
crates/banlieue-provider-proxmox/
```

## Open decision: which Proxmox client?

**O-001 from `01-DECISIONS.md`.** Survey crates.io first:

```sh
cargo search proxmox
```

Possible candidates:
- `proxmox-api` family (Proxmox's own Rust libraries; check API
  surface — they may target PBS/PMG, not PVE)
- Community crates

**Default if nothing fits**: roll a thin client with `reqwest`. The
Proxmox VE API is well-documented at <https://pve.proxmox.com/pve-docs/api-viewer/>.

Record the decision in `01-DECISIONS.md` D-006 once made.

## Module layout

```
src/
├── main.rs
├── lib.rs
├── client/
│   ├── mod.rs
│   ├── auth.rs              // API token OR ticket-based auth
│   ├── nodes.rs             // list nodes, status
│   ├── storage.rs           // list storage, capacity, content
│   ├── networks.rs          // bridges per node
│   ├── vm.rs                // create/clone/start/stop/destroy
│   └── tasks.rs             // task polling (Proxmox API is async via UPID)
├── reconciler/
│   ├── mod.rs
│   ├── provider.rs          // capability introspection
│   ├── proxmox_machine.rs   // VM lifecycle
│   └── image.rs             // template availability / import
├── customize.rs             // cloud-init (cicustom snippet)
├── error.rs
└── context.rs
```

## Provider reconciler — capability introspection

When a `Provider` of class `proxmox` reconciles:

1. Connect (token preferred; ticket as fallback).
2. List nodes (`/nodes`). Each node is a candidate failure domain
   anchor — but for clustered Proxmox the cluster itself is the
   failure domain unit when shared storage is available. Default
   policy:
   - Standalone node ⇒ one failure domain `<provider>-<node>`.
   - Clustered Proxmox ⇒ one failure domain per node, plus an
     optional `<provider>-cluster-shared` aggregate if every storage
     class is reachable cluster-wide.
3. For each declared `storageClass`:
   - Target keys: `storage: <name>`.
   - Probe each node's `/nodes/<node>/storage` to see whether the
     storage is enabled and supports `images` content. Populate
     `availableStorageClasses` per failure domain accordingly.
4. For each declared `networkClass`:
   - Target keys: `bridge: <name>`, optional `vlan: <tag>`.
   - Verify the bridge exists on the target node(s).
5. Feature detection:
   - `liveMigration`: true iff there's any pair of nodes with shared
     storage.
   - `nestedVirtualization`: per-node CPU feature.
6. Patch status.

## ProxmoxMachine reconciler — VM lifecycle

```
Create
──────
1. Read spec; resolve Provider; build client.
2. If spec.vmid is None: GET /cluster/nextid; persist on the
   ProxmoxMachine to make subsequent reconciles idempotent.
3. Resolve template node: where does the template VMID live? If on a
   different node than spec.node, clone with target (which migrates
   on creation).
4. POST /nodes/<node>/qemu/<template>/clone:
   - newid = vmid
   - storage = first disk's storage
   - full = true (full clone, not linked)
   - pool, name, target, etc.
   Capture the UPID and poll until done.
5. PUT /nodes/<node>/qemu/<vmid>/config:
   - cores, sockets, memory, cpu, bios, machine
   - per-NIC: net0=<model>,bridge=<br>,...,mac=<mac>,tag=<vlan>
   - per-disk: scsi0=<storage>:<size>,iothread=on,discard=on,...
   - efidisk0 if firmware=efi
   - cloud-init: ciuser, cipassword (don't), sshkeys (no), cicustom
     pointing at a snippet in a content storage
6. POST /nodes/<node>/qemu/<vmid>/status/start; poll.
7. status.providerID = proxmox://<vmid>@<node>
8. status.initialization.provisioned = true

Read
────
- Periodically poll /nodes/<node>/qemu/<vmid>/status/current
- Update power state, addresses (from QEMU guest agent if enabled)

Update
──────
- Resize CPUs / memory: PUT config; Proxmox supports hot-add on most
  configs.
- Disk grow: PUT /nodes/<node>/qemu/<vmid>/resize.
- Network changes: PUT config (NIC hot-plug works).
- Storage change: treat as immutable.

Delete
──────
- Stop (if running)
- DELETE /nodes/<node>/qemu/<vmid>
```

## Customization

Proxmox supports cloud-init **natively** via the `cloudinit` config
keys. Two delivery modes:

1. **Snippet-based (`cicustom`)** — write the user-data file to a
   snippet-enabled storage (typically `local:snippets/<name>.yaml`)
   and reference it. Most flexible.
2. **Built-in keys (`ciuser`, `sshkeys`, `ipconfigN`)** — limited but
   no snippet storage required.

Use snippet-based:
- Write user-data to `local:snippets/banlieue-<vmid>.yaml` via the
  snippets API on the target node.
- Set `cicustom=user=local:snippets/banlieue-<vmid>.yaml`.
- For network config: prefer `ipconfigN=ip=...,gw=...` since it's
  simple and reliable; or use a `network=` snippet.

Cleanup: remove the snippet on VM deletion.

## IPAM integration

Same pattern as vSphere — pre-claim from CAPI IPAM pool, then push
into `ipconfigN`.

## Tasks

- [ ] Decide and document Proxmox client (O-001).
- [ ] Add `ProxmoxMachine` + `ProxmoxMachineTemplate` to `banlieue-api`.
- [ ] Regenerate CRD YAML.
- [ ] Scaffold the provider crate.
- [ ] Implement `client/auth.rs` (token + ticket).
- [ ] Implement `client/tasks.rs` (UPID polling utility).
- [ ] Implement `client/{nodes,storage,networks,vm}.rs`.
- [ ] Implement `reconciler/provider.rs`.
- [ ] Implement `reconciler/proxmox_machine.rs`.
- [ ] Implement `reconciler/image.rs` (template clone-source check;
      optional import via `qm importdisk` proxy — usually requires
      shell access, so document as out-of-scope for v1).
- [ ] Implement `customize.rs` (snippet upload + cicustom wiring).
- [ ] Implement deletion finalizer including snippet cleanup.
- [ ] Container image (small, debian-slim or alpine).
- [ ] RBAC: as per vSphere provider.

## Tests

- [ ] Mock client behind a trait.
- [ ] If possible, integration against a real Proxmox VE node in CI
      (test cluster maintained by the project).
- [ ] Snippet lifecycle test: create-VM-then-delete leaves no
      orphaned snippets.

## Definition of done

- VirtualMachine on a Proxmox provider reaches provisioned with
  cloud-init applied and reachable via the configured network.
- Provider capability introspection reflects real node/storage state.
- VM deletion removes the snippet.

## Gotchas

- **UPID polling**: every Proxmox mutating call returns a Unique
  Process IDentifier; do not assume the work is done on 200 OK. Poll
  `/nodes/<node>/tasks/<upid>/status` until `status=stopped` and
  `exitstatus=OK`.
- **VMID conflicts**: nextid is racy across multiple controllers.
  Persist the chosen VMID on the CR after first allocation; subsequent
  reconciles use that.
- **Standalone vs clustered**: many endpoints differ in path shape;
  `/cluster` endpoints don't exist on standalone. Detect at
  connection.
- **Storage content types**: a storage may exist on a node but not
  allow `images` content. Always check `content` includes `images`
  before assuming it's usable for a disk.
- **API tokens with separator `!`**: format `<user>!<tokenid>` and
  the value is the secret. Easy to mis-paste; validate at connect.
- **Cloud-init regen**: changes to `cicustom` require a
  `qm cloudinit update` (or equivalent API call) to take effect on
  next boot; don't assume PUT alone is sufficient.
- **Snippet storage requirement**: snippets must live on a storage
  with `snippets` content type. If the admin's configured storage
  doesn't include it, fail loudly at Provider reconcile.
