# Phase 1B — vSphere Provider

> **Goal.** A `banlieue-provider-vsphere` binary that watches
> `VSphereMachine` CRs and `Provider` CRs of class `vsphere`, talks to
> vCenter via `vim_rs`, and reconciles real VMs end-to-end.
>
> **Stop condition.** A user can `kubectl apply` a `VirtualMachine`
> referencing a real vCenter and end up with a powered-on Ubuntu VM
> with the requested resources, network, and cloud-init applied.

## Preconditions

- Phase 1A complete: main controller can create a `VSphereMachine` and
  mirror its status.
- A test vCenter (or vcsim) reachable from the dev cluster.
- An Ubuntu 22.04 cloud-init template pre-imported into vCenter.

## Crate

```
crates/banlieue-provider-vsphere/
```

Add to workspace. **Keep its dependency tree isolated** — `vim_rs`
compiles in 2–5 minutes cold; do not let it leak into the main
controller's build graph.

## Module layout

```
src/
├── main.rs               // CLI, tracing, controller setup
├── lib.rs
├── client/
│   ├── mod.rs
│   ├── connection.rs     // login, session refresh, retry
│   ├── inventory.rs      // find datacenter / cluster / datastore / network / folder
│   ├── template.rs       // template lookup + import
│   ├── vm.rs             // clone, customize, power ops, delete
│   └── tags.rs           // tag/category helpers (capability discovery)
├── reconciler/
│   ├── mod.rs
│   ├── provider.rs       // Provider reconciler: capability introspection
│   ├── vsphere_machine.rs // VSphereMachine reconciler: VM lifecycle
│   └── image.rs          // VMImage reconciler: template / OVA presence
├── customize.rs          // cloud-init via vAppConfig / guestinfo
├── error.rs
└── context.rs
```

## Two reconcilers in this binary

The provider watches **two** kinds:

1. **`Provider`** (when `spec.providerClassRef.name == "vsphere"`):
   - Reconcile = introspect vCenter and populate
     `status.failureDomains[]`.
   - Reconcile period: 5 minutes (cap discovery cost).

2. **`VSphereMachine`**:
   - Reconcile = drive VM toward desired state on vCenter.
   - Reconcile period: 30 s when active, 5 min when steady-state.

A `VMImage` reconciler may also live here (split from the main image
controller because import is a per-provider concern); see "Image
management" below.

## vSphere client wrapper

`vim_rs` exposes the raw VI-JSON types. Wrap it in a small,
banlieue-shaped API:

```rust
pub struct VSphereClient { /* session, base URL, http client */ }

impl VSphereClient {
    pub async fn connect(endpoint: &str, user: &str, pass: &str, ca: Option<&[u8]>, insecure: bool) -> Result<Self>;

    pub async fn list_datacenters(&self) -> Result<Vec<Datacenter>>;
    pub async fn list_clusters(&self, dc: &Datacenter) -> Result<Vec<Cluster>>;
    pub async fn list_datastores(&self, cluster: &Cluster) -> Result<Vec<Datastore>>;
    pub async fn list_networks(&self, cluster: &Cluster) -> Result<Vec<Network>>;

    pub async fn resolve_tag_category(&self, cat: &str) -> Result<TagCategory>;
    pub async fn list_objects_with_tag(&self, cat: &str, tag: &str) -> Result<Vec<ManagedObjectRef>>;

    pub async fn find_template(&self, dc: &Datacenter, name: &str) -> Result<Option<Vm>>;
    pub async fn clone_from_template(&self, template: &Vm, spec: &CloneSpec) -> Result<Vm>;
    pub async fn power_on(&self, vm: &Vm) -> Result<()>;
    pub async fn power_off(&self, vm: &Vm, hard: bool) -> Result<()>;
    pub async fn delete(&self, vm: &Vm) -> Result<()>;
    pub async fn get_vm_state(&self, vm: &Vm) -> Result<VmState>;
}
```

Notes:

- The wrapper's domain types (`Datacenter`, `Cluster`, `Vm`, etc.) are
  **slim Rust structs**, *not* re-exported VIM types. Project from
  VIM into these at the boundary. (See `01-DECISIONS.md` D-006 for why.)
- Session refresh: vCenter sessions expire after 30 min idle. The
  client should transparently re-login on session-invalid errors and
  retry once.
- Retry: only on transient errors (HTTP 5xx, connection reset).
  Permission errors propagate immediately.

## Provider reconciler — capability introspection

When a `Provider` of class `vsphere` is created/updated:

1. Connect to vCenter using `spec.connection.credentialsRef` Secret.
2. Walk inventory: list datacenters, then clusters within them.
3. Each `(datacenter, cluster)` becomes one `FailureDomain` entry:
   - `name`: `<provider-name>-<datacenter>-<cluster>` (slugified)
   - `labels`: `{ dc: <datacenter>, cluster: <cluster> }`
     (admin can override via `Provider.spec` overlay — TODO)
   - `attributes.raw`: `{ datacenter, cluster, datastoreClusters: [...] }`
4. For each declared `storageClass` in `spec.capabilities`:
   - Resolve its target. Supported target keys:
     - `datastore: <name>` — single datastore
     - `datastoreCluster: <name>` — SDRS-managed cluster
     - `tagCategory: <cat>` + `tag: <t>` — all tagged datastores
   - For each `(datacenter, cluster)`, check whether the resolved
     datastore(s) are reachable from that cluster. If yes, add the
     class name to `attributes.availableStorageClasses`.
5. For each declared `networkClass`:
   - Targets: `portGroup: <name>` (standard PG) or
     `distributedPortGroup: <name>` (vDS).
   - Check reachability per `(datacenter, cluster)`; populate
     `attributes.availableNetworkClasses`.
6. Detect features:
   - `hotAddCPU`, `hotAddMemory`: cluster capability flags.
   - `efiSecureBoot`: vCenter ≥ 6.5 generally yes; check VirtualMachineCapability.
   - `nestedVirtualization`: cluster CPU feature flag.
7. Patch `Provider.status` with the computed `failureDomains[]` and
   conditions (`Ready`, `ProviderReachable`).

Reconcile period: 5 minutes. Also reconcile on Secret updates (RBAC
permitting).

## VSphereMachine reconciler — VM lifecycle

```
Phase: VM does not exist yet
─────────────────────────────
1. Read spec; resolve the Provider via spec.providerRef
2. Open vSphere client (cached per Provider, reused across VMs)
3. Find the template by name in the spec.datacenter
4. Resolve concrete folder, resource pool, datastore
5. Build a CloneSpec:
   - target folder
   - target datastore
   - cluster resource pool
   - NICs mapped to port groups
   - reconfigure spec: CPU, memory, disks
   - customization (cloud-init via guestinfo OVF properties)
6. Issue Clone; await Task completion
7. Power on
8. Record vm-NNNN moref and instanceUUID in status
9. Set spec.providerID = "vsphere://<instanceUUID>"
10. Patch status: initialization.provisioned=true, conditions, addresses

Phase: VM exists
────────────────
- Periodically refresh: power state, guest IPs, tools status
- Mirror addresses (collected from VMware Tools) into status.addresses
- Surface PowerState condition

Phase: Spec changed
───────────────────
- numCPUs / memoryMiB changed:
  - If powered off OR hotAdd supported: apply via ReconfigVM
  - Otherwise: signal back to main controller via condition
    InfrastructureReady=False reason=ResizeRequiresPowerOff
- Disk grow: best-effort online resize
- Other changes (template, datastore, network): treat as immutable;
  surface condition InfrastructureMutationNotSupported=True

Phase: Deletion (finalizer)
───────────────────────────
1. Power off (hard if necessary)
2. Delete VM
3. Remove finalizer
```

## Image reconciler — template availability

For each `VMImage` with a source matching `providerClass: vsphere`:

- If `kind: Template`: verify the template exists in every reachable
  datacenter. If not and `importFrom` is set, OVF-import it.
- If `kind: Url`: download OVA and deploy as template.
- Update `VMImage.status.perProvider[i]` for this Provider.

## IPAM integration

For each NIC in `VSphereMachineSpec.network`:

- `ipam.source == Dhcp`: do nothing extra; vCenter customization sets
  DHCP.
- `ipam.source == Static`: include address/gateway/nameservers in the
  customization spec.
- `ipam.source == Pool`:
  1. Before issuing the Clone, create an `IPAddressClaim` (CAPI IPAM
     contract) referencing the pool.
  2. Wait (requeue) for `IPAddress` to be assigned.
  3. Use the assigned address in static customization.
  4. Set owner ref on the claim so it GC's with the VSphereMachine.

## Customization

Default to **cloud-init via guestinfo** OVF properties:

```
guestinfo.userdata          = base64(user-data from Secret)
guestinfo.userdata.encoding = "base64"
guestinfo.metadata          = base64(meta-data: instance-id, hostname, network)
guestinfo.metadata.encoding = "base64"
```

This works for all images using the `nocloud-net` cloud-init
datasource (Ubuntu 18.04+, common cloud images).

For Windows / sysprep: defer to Phase 4.

## CLI flags

```
banlieue-provider-vsphere [--kubeconfig PATH]
                          [--namespace NS]
                          [--leader-election-id banlieue-vsphere]
                          [--log-level ...]
                          [--vsphere-task-timeout 600s]
                          [--health-port 8081]
```

## Tasks

- [ ] Scaffold crate and Dockerfile (multi-stage; vim_rs compile is
      slow, so use `cargo chef` or sccache to cache deps).
- [ ] Implement `client/connection.rs` with session refresh + retry.
- [ ] Implement `client/inventory.rs` (datacenter/cluster/datastore/network walks).
- [ ] Implement `client/template.rs` (find + OVF import).
- [ ] Implement `client/vm.rs` (clone, power, reconfigure, delete).
- [ ] Implement `reconciler/provider.rs` (capability introspection).
- [ ] Implement `reconciler/vsphere_machine.rs` (VM lifecycle).
- [ ] Implement `reconciler/image.rs` (template availability).
- [ ] Implement `customize.rs` (cloud-init via guestinfo).
- [ ] IPAM claim/wait helper (use `kube::Api` against
      `ipam.cluster.x-k8s.io/IPAddressClaim`).
- [ ] Wire main with leader election, signal handling, dual
      controllers (Provider + VSphereMachine).
- [ ] RBAC: read/patch `infrastructure.banlieue.io/vspheremachines`,
      patch `banlieue.io/providers` status, read Secrets in watched
      namespaces, CRUD `ipam.cluster.x-k8s.io/ipaddressclaims`,
      patch `banlieue.io/vmimages` status.

## Tests

- [ ] Mock client behind a trait; reconciler tests assert correct
      sequence of client calls for create/update/delete.
- [ ] Integration against `vcsim` (lightweight vCenter simulator):
      end-to-end clone + power on + delete.
- [ ] Capability introspection against `vcsim`: synthesize known
      inventory, assert expected `failureDomains[]` output.

## Definition of done

- E2E: applying the example `VirtualMachine` against a vcsim-backed
  cluster results in a `VSphereMachine` reaching
  `initialization.provisioned=true` with addresses populated.
- Capability introspection populates `Provider.status.failureDomains[]`
  with the expected entries given known vcsim inventory.
- Container image builds and runs.

## Gotchas

- **vim_rs compile time**: keep this crate isolated; use sccache; let
  CI do clean builds infrequently.
- **vim_rs has no PartialEq/Eq/Hash/Clone by default.** Project into
  local types at the boundary.
- **Session expiry** is silent — clients see 401-ish errors that look
  like permission errors. Always treat session-expired as recoverable
  and retry once with re-login.
- **Customization specs vs guestinfo**: vCenter's built-in
  customization is sysprep-style and doesn't compose well with
  cloud-init. Use guestinfo OVF properties for cloud images; don't
  set both.
- **OVF import is slow** (minutes); never block the main reconcile
  loop on it. Run import in a detached task and reconcile back via
  status.
- **vCenter tags and categories** require the vCenter REST API
  (CIS endpoint), not the legacy SOAP/SDK endpoint. `vim_rs` may
  handle this, verify before assuming.
- **Cluster vs ESX host placement**: by default, vCenter DRS picks
  the host. Avoid pinning to a host unless explicitly requested.
- **Datastore clusters with SDRS** auto-pick a datastore at clone
  time; this is desirable. But the clone API requires a *concrete*
  datastore selection — use the SDRS recommendation API to get one.
