# Phase 1A — Main Controller + Provider SDK

> **Goal.** A `banlieue-controller` binary that watches
> `VirtualMachine` CRs, schedules them onto a `Provider`+failure
> domain, creates the appropriate `VSphereMachine` (or other provider
> infra CR), and mirrors the infra CR's status back. Plus a
> `banlieue-provider-sdk` crate that providers will use to avoid
> re-implementing the boilerplate.
>
> **Stop condition.** A `VirtualMachine` can go from creation through
> `status.scheduled` and `status.infrastructureRef` populated. The
> infra CR may be stubbed for now (no real backend); status mirroring
> from a manually-edited infra CR back to the parent VM must work
> end-to-end.

## Preconditions

- `banlieue-api` compiles, CRDs generate cleanly, examples apply
  cleanly to a `kind` cluster.
- A local `kind` cluster is available for testing.

## Crates to create

```
crates/banlieue-provider-sdk/
crates/banlieue-controller/
```

Add both to the workspace members in the root `Cargo.toml`.

## banlieue-provider-sdk

Shared runtime helpers used by both the main controller and every
provider. Pure library crate.

### Module layout

```
src/
├── lib.rs
├── client.rs           // build a kube::Client from env/kubeconfig
├── reconciler.rs       // Action helpers, requeue defaults
├── status.rs           // condition helpers, observedGeneration patch
├── finalizer.rs        // ensure / remove finalizer helpers
├── error.rs            // shared error types
├── ssa.rs              // server-side apply helper
└── leader.rs           // lease-based leader election
```

### Key API surface

```rust
// status.rs
pub fn set_condition(
    conditions: &mut Vec<Condition>,
    type_: &str,
    status: ConditionStatus,
    reason: &str,
    message: impl Into<String>,
    observed_generation: i64,
);

pub fn is_condition_true(conditions: &[Condition], type_: &str) -> bool;

// finalizer.rs
pub async fn ensure_finalizer<K: Resource>(
    api: &Api<K>,
    obj: &K,
    finalizer: &str,
) -> Result<()>;

pub async fn remove_finalizer<K: Resource>(
    api: &Api<K>,
    obj: &K,
    finalizer: &str,
) -> Result<()>;

// ssa.rs
pub async fn server_side_apply<K: Resource>(
    api: &Api<K>,
    field_manager: &str,
    obj: &K,
) -> Result<K>;
```

### Tasks

- [ ] Create the crate skeleton with the modules above.
- [ ] Implement `client::build_client()` that respects `KUBECONFIG`,
      in-cluster config, and a `--kubeconfig` CLI flag.
- [ ] Implement condition helpers using
      `banlieue_api::common::condition_types` / `condition_reasons`.
- [ ] Implement finalizer helpers with patch-based add/remove.
- [ ] Implement SSA helper with `PatchParams::apply(field_manager).force()`.
- [ ] Implement leader election using a `coordination.k8s.io/Lease` —
      run reconcilers only while leader.
- [ ] Unit tests for condition helpers (idempotency, type uniqueness).

### Definition of done

- `cargo test -p banlieue-provider-sdk` passes.
- A simple example binary in `examples/` (within the SDK crate) shows
  acquiring leadership and printing reconcile events.

## banlieue-controller

The main controller. Watches `VirtualMachine`, runs the scheduler,
manages the infra ref, mirrors status.

### Module layout

```
src/
├── main.rs              // CLI, tracing, signal handling, controller setup
├── lib.rs
├── reconciler/
│   ├── mod.rs
│   ├── virtualmachine.rs   // main reconcile loop
│   ├── scheduler.rs        // placement decision
│   ├── infra.rs            // create / update / observe infra CR
│   └── status_mirror.rs    // pull status from infra CR up to parent
├── migration/
│   └── mod.rs           // PlacementValid + migrationPolicy enforcement
├── image_watcher.rs     // watches VMImage readiness for gating
└── context.rs           // shared reconcile context (client, lister caches)
```

### Reconcile flow for VirtualMachine

```
1. Handle deletion (finalizer):
   - Delete owned infra CR (VSphereMachine/etc)
   - Wait for it to actually disappear
   - Remove finalizer

2. Ensure finalizer banlieue.io/virtualmachine

3. Resolve refs:
   - Fetch VMClass (cluster-scoped)
   - Fetch VMImage (cluster-scoped)
   - Validate image readiness on candidate providers

4. Schedule:
   - List Providers in namespace
   - Filter by placement.providerSelector
   - For each candidate Provider, filter failure domains
   - Capability filter: storage classes, network classes, features
   - Anti-affinity filter (other VMs already scheduled)
   - Pick one (deterministic if tied: alphabetical, with VM name salt)
   - If no candidate: condition Scheduled=False, requeue

5. If status.scheduled differs from new pick:
   - If migrationPolicy=Never: keep old; condition PlacementValid=False
   - If Manual: condition PlacementValid=False; act only if annotation present
   - If Automatic: trigger migration path

6. Apply infra CR:
   - Resolve storage class names → concrete backend targets
   - Resolve network class names → concrete backend targets
   - Resolve image source for this provider class
   - SSA the infra CR (e.g. VSphereMachine) with owner ref

7. Status mirror:
   - Read infra CR status
   - Mirror initialization.provisioned, addresses, failureDomain
   - Mirror Ready as InfrastructureReady
   - Compute aggregate Ready condition

8. Return Action::requeue(30s)
```

### Scheduler details

The scheduler is a pure function: `(VirtualMachine, [Provider],
[VMClass], [VMImage], [VirtualMachine]) → Decision`.

```rust
pub struct Decision {
    pub provider: ObjectRef,         // chosen Provider
    pub provider_class: String,
    pub failure_domain: String,
    pub resolved_storage: Vec<ResolvedResource>,
    pub resolved_networks: Vec<ResolvedResource>,
}

pub fn schedule(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    providers: &[Provider],
    existing_vms: &[VirtualMachine],
) -> Result<Decision, ScheduleError>;
```

Keep it pure → unit-testable with synthetic input.

Selection algorithm (greedy, deterministic):
1. Filter providers by `placement.providerSelector` matching
   `Provider.metadata.labels`.
2. For each provider, walk `status.failureDomains`. Each domain is a
   tuple `(provider, fd)`.
3. Filter `(provider, fd)` by `placement.failureDomainSelector`
   matching `fd.labels`.
4. Filter by image readiness: `VMImage.status.perProvider[i]` must
   have `ready=true` for this provider.
5. Filter by storage classes: every disk's `storageClass` must be in
   `fd.attributes.availableStorageClasses`.
6. Filter by network classes: every NIC's `networkClass` must be in
   `fd.attributes.availableNetworkClasses`.
7. Filter by features: every feature in `VMClass.spec.features` must
   be in `fd.attributes.features`.
8. Filter by firmware support.
9. Apply anti-affinity: for each `required` rule, drop domains where
   another matching VM is already scheduled and the domain's
   `labels[topologyKey]` collides.
10. If any candidates remain, score them:
    - Soft anti-affinity (`preferred`): penalty per collision.
    - Tie-break: stable hash of `(vm.name, provider.name, fd.name)`.
11. Return highest-scoring; if empty, return
    `ScheduleError::NoCandidate { reasons }`.

### Status mirroring

```rust
async fn mirror_status_from_infra(
    vm: &mut VirtualMachine,
    infra: &dyn InfraMachineRead,
) -> Result<()>;

trait InfraMachineRead {
    fn initialization(&self) -> &InitializationStatus;
    fn addresses(&self) -> &[MachineAddress];
    fn failure_domain(&self) -> Option<&str>;
    fn provider_id(&self) -> Option<&str>;
    fn conditions(&self) -> &[Condition];
}
```

Implement `InfraMachineRead` for `VSphereMachine` (and later
`ProxmoxMachine`, `LibvirtMachine`).

The Ready condition from the infra CR becomes `InfrastructureReady` on
the VM. The VM's own `Ready` is true iff `Scheduled=True` AND
`PlacementValid=True` AND `InfrastructureReady=True`.

### Migration controller (sub-loop)

Lives in `src/migration/`. Watches VirtualMachines with
`PlacementValid=False`. For each:

- If `migrationPolicy=Never`: do nothing.
- If `Manual`: check for `banlieue.io/migrate=true` annotation.
- If `Automatic` (or `Manual` with annotation):
  - Set `Migrating=True`.
  - **For v1A**, implement recreate-only: delete old infra CR, wait
    for owner refs to GC, create new infra CR with new placement.
  - **Live migration is Phase 2 work** — leave a TODO and a stub.
  - On completion: clear `Migrating`, update `status.scheduled`.

### CLI flags (clap)

```
banlieue-controller [--kubeconfig PATH]
                    [--namespace NS]              # watch one namespace
                    [--leader-election-namespace] # lease namespace
                    [--leader-election-id]
                    [--log-level info|debug|trace]
                    [--health-port 8081]
                    [--metrics-port 8080]         # placeholder; Phase 4
```

### Tasks

- [ ] Scaffold the crate, wire workspace.
- [ ] Implement `context.rs` with shared `Arc<Context>` containing
      kube client and lister caches.
- [ ] Implement `reconciler/scheduler.rs` as a pure module with
      heavy unit-test coverage.
- [ ] Implement `reconciler/infra.rs` to SSA a `VSphereMachine` from
      a decision (other provider kinds: TODO, added in 1C/1D).
- [ ] Implement `reconciler/status_mirror.rs`.
- [ ] Implement `reconciler/virtualmachine.rs` end-to-end.
- [ ] Implement deletion finalizer flow.
- [ ] Implement `migration/` recreate-only path.
- [ ] Implement `image_watcher.rs` (watches VMImage, requeues affected
      VMs when image readiness flips).
- [ ] Wire `main.rs` with tracing, leader election, signal handling
      (SIGTERM, SIGINT).
- [ ] Dockerfile (multi-stage) → distroless image.
- [ ] Helm-less raw manifest under `deploy/controller/`:
      Deployment + ServiceAccount + ClusterRole + ClusterRoleBinding.
- [ ] RBAC: full access to `banlieue.io/*`, `infrastructure.banlieue.io/*`,
      read on Secrets in watched namespaces, write on Events.

### Tests

- [ ] `scheduler.rs` unit tests for each filter step.
- [ ] `status_mirror.rs` table-driven tests with fixtures of infra CR
      status → expected parent status.
- [ ] Integration test: create a Provider with fake failure domains,
      create a VirtualMachine, assert that a VSphereMachine is created
      with the expected fields.

### Definition of done

- `kubectl apply -f examples/` (after manually pre-creating fake
  Provider failure domains) results in:
  - The VirtualMachine getting `status.scheduled` populated
  - A VSphereMachine created in the same namespace, owned by the VM
  - Manual edit to `VSphereMachine.status.initialization.provisioned=true`
    flips `VirtualMachine.status.initialization.provisioned=true` on
    the next reconcile
- `cargo test -p banlieue-controller` passes.
- Container image builds.

### Gotchas

- **Don't mutate the input object in reconciliation.** Build a patch.
- **Don't trust caches blindly.** When status mirroring, ensure the
  infra CR observed_generation is current.
- **Owner references go on the infra CR, not the parent.** The
  VirtualMachine *owns* the VSphereMachine, not vice versa.
- **Server-side apply field manager** must be unique per controller:
  use `banlieue.io/controller` for the main controller, and provider
  controllers use `banlieue.io/provider-vsphere` etc. Otherwise SSA
  fights itself on co-owned fields.
- **Watch infra CRs** with a `Controller::owns` relationship so a
  VirtualMachine reconciles when its infra CR's status changes.
- **Cluster-scoped CR lookups** for VMClass and VMImage use
  `Api::all(client)`, not `Api::namespaced(...)`.

## Open items

- **O-001 / O-002 / O-003** still open — see `01-DECISIONS.md`. None
  block Phase 1A.
