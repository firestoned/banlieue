# Phase 1D — Libvirt Provider

> **Goal.** A `banlieue-provider-libvirt` binary that watches a new
> `LibvirtMachine` CRD and the corresponding `Provider`s of class
> `libvirt`, talking to libvirtd over its native protocol (local
> socket or `qemu+ssh://`).
>
> **Stop condition.** A `VirtualMachine` referencing a libvirt host
> provider can be cloned from a backing-file template, customized
> with cloud-init via NoCloud, and torn down. Capability
> introspection populates the Provider's failure domains based on
> admin-supplied mappings.

## Preconditions

- Phase 1A complete.
- Phase 1B and 1C complete (this is the easiest backend conceptually
  but the most foreign to most teams — having the patterns nailed in
  the richer backends makes this faster).
- A test libvirt host with at least one storage pool and one
  network/bridge.

## Add to banlieue-api

New CRD in `crates/banlieue-api/src/infrastructure/libvirt_machine.rs`:

- `LibvirtMachine` (namespaced) — InfraMachine contract compliant.
- `LibvirtMachineTemplate` (namespaced).

Fields:

```rust
pub struct LibvirtMachineSpec {
    pub provider_id: Option<String>,        // libvirt://<host>/<uuid>
    pub failure_domain: Option<String>,

    pub provider_ref: LocalObjectReference,

    pub backing_file: String,               // path to template qcow2 (resolved from VMImage)
    pub domain_name: String,                // libvirt domain name
    pub vcpus: u32,
    pub memory_mi_b: u32,
    pub firmware: Firmware,                 // bios → seabios, efi → ovmf
    pub machine_type: Option<String>,       // q35, pc

    pub disks: Vec<LibvirtDiskSpec>,
    pub network: Vec<LibvirtNicSpec>,

    /// Storage pool used for non-OS disks (resolved from storage class).
    pub pool: String,

    /// Path where the OS-disk overlay (cow on backing file) is written.
    /// Computed from pool path + domain name; not user-set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_disk_path: Option<String>,
}

pub struct LibvirtDiskSpec {
    pub name: String,
    pub size_gi_b: u32,
    pub pool: String,                       // resolved storage pool
    pub format: String,                     // qcow2 default
    pub bus: Option<String>,                // virtio (default), scsi, sata
}

pub struct LibvirtNicSpec {
    pub name: String,
    pub source: LibvirtNicSource,           // bridge | network
    pub model: Option<String>,              // virtio (default)
    pub mac_address: Option<String>,
    pub ipam: IpamSpec,
}

#[serde(tag = "type", rename_all = "lowercase")]
pub enum LibvirtNicSource {
    Bridge { bridge: String },              // direct bridge attachment
    Network { name: String },               // libvirt-managed network
}
```

Regenerate CRDs after adding.

## Crate

```
crates/banlieue-provider-libvirt/
```

## Libvirt client choice

Use the `virt` crate (libvirt-rs FFI bindings). It's the most mature
option. Wrap it behind a safe trait so test code can mock and so
fewer files have to deal with FFI quirks.

Dependencies:

```toml
[dependencies]
virt = "0.4"           # libvirt FFI; pin exactly
```

The container image must include `libvirt0` (Debian package) or the
equivalent on the chosen base. Use `debian:bookworm-slim` rather than
distroless for this provider.

## Module layout

```
src/
├── main.rs
├── lib.rs
├── client/
│   ├── mod.rs
│   ├── connection.rs        // libvirt URI handling, reconnect
│   ├── pools.rs             // storage pool operations
│   ├── networks.rs          // libvirt network operations
│   ├── domain.rs            // define / start / stop / destroy
│   └── volume.rs            // qcow2 overlays, cloud-init ISOs
├── xml/
│   ├── mod.rs
│   ├── domain.rs            // build domain XML
│   └── helpers.rs           // escaping, etc.
├── reconciler/
│   ├── mod.rs
│   ├── provider.rs          // capability "introspection" (mostly validation)
│   ├── libvirt_machine.rs   // VM lifecycle
│   └── image.rs             // backing file presence + import
├── cloudinit.rs             // NoCloud ISO generation
├── error.rs
└── context.rs
```

## Provider reconciler — capability validation

Unlike vSphere/Proxmox, libvirt has no notion of "datacenter" or
"cluster". The Provider reconciler's job is mostly to **validate**
that the admin-declared capability mappings actually correspond to
real things on the host, then advertise a single failure domain.

Default: one failure domain `<provider-name>-default`, labeled
`{ host: <hostname>, dc: <inferred-or-empty> }`. The admin can set
extra labels via spec extension (TODO: add a `Provider.spec.failureDomainOverrides`
field if needed).

For each declared `storageClass`:
- Target key: `pool: <name>`.
- Connect to libvirt, look up the pool by name, verify it's active.
- If valid, add to `availableStorageClasses`.

For each declared `networkClass`:
- Target keys: `network: <name>` (libvirt network) OR
  `bridge: <name>` (raw bridge).
- For network: verify the network exists and is active.
- For bridge: validation is best-effort (libvirt may not introspect
  host bridges fully); accept and mark available.

Feature detection:
- `nestedVirtualization`: check `/sys/module/kvm_intel/parameters/nested`
  or `/sys/module/kvm_amd/parameters/nested` via QEMU agent on the
  host — or just trust the admin's declaration.
- `efiSecureBoot`: requires OVMF.fd / OVMF_CODE.secboot.fd; check
  filesystem.
- `liveMigration`: false in v1.

## LibvirtMachine reconciler — VM lifecycle

```
Create
──────
1. Resolve Provider; open libvirt connection.
2. Locate the backing file (from VMImage's libvirt source).
3. Create the OS-disk overlay:
     qemu-img create -f qcow2 -F qcow2 -b <backing> <overlay-path> <size>
   Use libvirt volume APIs where possible to avoid shelling out.
4. For each extra disk: create a fresh volume in spec.pool.
5. Generate cloud-init NoCloud ISO:
     - meta-data (instance-id, hostname, network-config)
     - user-data (from VirtualMachine Secret)
   Write the ISO into the same pool; attach as a CD-ROM.
6. Build domain XML from spec (use xml/domain.rs templates).
7. virConnect.domain_define_xml(...)
8. Domain.create() to start.
9. status.providerID = libvirt://<host>/<uuid>
10. status.initialization.provisioned = true

Read
────
- Periodically poll domain state and addresses.
- IPs come from one of:
  - QEMU guest agent (if the image has qemu-guest-agent and the agent
    socket is configured)
  - DHCP lease file (for libvirt-managed networks)
  - ARP table on bridge networks (best effort)

Update
──────
- vCPUs: libvirt supports hot-plug if the domain XML declared
  cpu placement="static" with current/max.
- Memory: hot-add if the XML allows it (memballoon + maxMemory slots).
- Disk grow: virsh blockresize equivalent (volume resize + libvirt notify).
- Other: treat as immutable; PlacementValid=False if migration policy
  permits, else require recreate.

Delete
──────
- Domain.destroy() if running.
- Domain.undefine_flags(NVRAM | SAVED_STATE).
- Delete OS overlay volume, extra-disk volumes, cloud-init ISO.
- Remove finalizer.
```

## Cloud-init (NoCloud)

NoCloud is the standard cloud-init datasource for non-cloud
environments. It looks for a CD-ROM (or block device) labeled
`cidata` containing:

- `meta-data` (YAML)
- `user-data` (YAML)
- optionally `network-config` (v2 format)

Build the ISO programmatically. Recommended library:
[`iso9660`](https://crates.io/crates/iso9660) for read; for writing,
shell out to `genisoimage`/`xorrisofs` (pre-installed in the container
image) or use the `iso9660-rs` writer if it's mature enough.

Place the resulting ISO in `spec.pool` as a volume named
`banlieue-<domain>-cidata.iso`, attached as a SATA CD-ROM in the
domain XML.

## IPAM

Same pattern as the other providers: claim an `IPAddressClaim`,
include the assigned IP in NoCloud `network-config`.

## Domain XML

Keep XML construction in `xml/domain.rs`. Use the `quick-xml` crate
or hand-rolled templating with safe escaping. **Never `format!()`
user-controlled strings into XML without escaping** — domain names
and disk paths are user-influenced.

Domain XML skeleton:

```xml
<domain type='kvm'>
  <name>{name}</name>
  <uuid>{uuid}</uuid>
  <memory unit='MiB'>{memory_mi_b}</memory>
  <currentMemory unit='MiB'>{memory_mi_b}</currentMemory>
  <vcpu placement='static'>{vcpus}</vcpu>
  <os>
    {firmware-bits}
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/><apic/>
  </features>
  <cpu mode='host-passthrough'/>
  <devices>
    {disks}
    {nics}
    <controller type='virtio-serial'/>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <serial type='pty'/>
    <console type='pty'/>
  </devices>
</domain>
```

## Tasks

- [ ] Add `LibvirtMachine` + template to `banlieue-api`. Regenerate.
- [ ] Scaffold the provider crate; add the `virt` dependency.
- [ ] Implement `client/connection.rs` with URI parsing
      (`qemu+ssh://user@host/system` etc.) and reconnect.
- [ ] Implement `client/{pools,networks,volume,domain}.rs`.
- [ ] Implement `xml/domain.rs` with thorough escaping and tests.
- [ ] Implement `cloudinit.rs` (NoCloud ISO build).
- [ ] Implement `reconciler/{provider,libvirt_machine,image}.rs`.
- [ ] Implement deletion finalizer (volumes + ISO cleanup).
- [ ] Multi-stage Dockerfile based on `debian:bookworm-slim`,
      installing `libvirt-clients`, `genisoimage`, `qemu-utils`.
- [ ] RBAC: same shape as other providers, namespaced to
      `LibvirtMachine`.
- [ ] **SSH key Secret support**: when the URI is `qemu+ssh://`, the
      provider needs a private key. Read from
      `Provider.spec.connection.credentialsRef` Secret under key
      `sshPrivateKey`; write to a tmpfs file in the container; export
      `LIBVIRT_DEFAULT_URI` and `SSH` env appropriately.

## Tests

- [ ] XML rendering tests with golden files.
- [ ] Cloud-init ISO content tests (build then re-read).
- [ ] Integration against a local libvirt running in CI (KVM nested
      virt in GHA runners is unreliable; use `test-driver` mock OR a
      self-hosted runner with libvirt).
- [ ] Mock the libvirt client trait for reconciler unit tests.

## Definition of done

- VM provisioned end-to-end via NoCloud, reachable on the network,
  visible in `virsh list --all`.
- VM deletion removes all owned volumes and the cloud-init ISO.
- The "providers without native tiering" path works: a libvirt
  Provider with admin-supplied `gold→nvme-pool` mapping can host a
  VMClass that requires the `gold` storage class.

## Gotchas

- **FFI safety**: the `virt` crate exposes `unsafe` boundaries.
  Wrap every call in a small safe method on the client struct;
  don't sprinkle `unsafe` through the reconciler.
- **Connection lifetime**: libvirt connections are per-thread in C
  and per-instance in Rust. Don't share a single `virt::Connect`
  across async tasks without a Mutex. Prefer per-reconcile connections
  with a short connection pool keyed by URI.
- **Pool refresh**: after creating a volume, the pool may need a
  refresh before libvirt sees it. Call `pool.refresh()` defensively.
- **NVRAM cleanup**: EFI domains have an NVRAM file alongside the
  domain. `undefine` without `VIR_DOMAIN_UNDEFINE_NVRAM` flag leaves
  stale files; always pass the flag.
- **`qemu+ssh` and host keys**: known-hosts is a deployment problem.
  Either pre-populate the container with known-hosts via ConfigMap,
  or set `StrictHostKeyChecking=accept-new` (less safe). Document
  the tradeoff.
- **NoCloud network-config v1 vs v2**: cloud-init versions vary in
  support. v2 is more widely supported in modern Ubuntu; older RHEL
  may need v1. Default to v2 and document the override knob.
- **Live migration is out of scope for v1**. If migrationPolicy is
  `Automatic` and migration is requested, fall back to recreate with
  a warning condition.
