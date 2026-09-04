# Phase 2 — Snapshots with GFS Scheduling

> **Goal.** Two new CRDs (`VirtualMachineSnapshot` and
> `SnapshotSchedule`), a snapshot controller, and per-provider
> implementations that take and prune snapshots according to
> Grandfather-Father-Son retention.
>
> **Stop condition.** A user can declare a SnapshotSchedule with
> hourly/daily/weekly tiers, and snapshots are created and pruned per
> tier according to the cron and keep counts. Manual one-off
> snapshots also work via VirtualMachineSnapshot.

## Preconditions

- Phase 1 complete for at least one provider (preferably vSphere or
  Proxmox; libvirt is trickier — see "Gotchas").
- VirtualMachine CRUD is solid in production-like usage.

## Add to banlieue-api

Two new CRDs in `crates/banlieue-api/src/banlieue/`:

### VirtualMachineSnapshot

```rust
pub struct VirtualMachineSnapshotSpec {
    /// Reference to the VirtualMachine.
    pub vm_ref: LocalObjectReference,

    /// Include guest memory state in the snapshot. Provider may ignore
    /// if not supported.
    #[serde(default)]
    pub memory: bool,

    /// Optional free-form description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Tier label set by a SnapshotSchedule; empty/None for manual snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

pub struct VirtualMachineSnapshotStatus {
    /// Provider-side identifier (vSphere snapshot moref, Proxmox snap
    /// name, libvirt snapshot name).
    pub provider_snapshot_id: Option<String>,

    /// Size delta in bytes; provider best-effort.
    pub size_bytes: Option<u64>,

    /// Time of completion as observed by the provider.
    pub created_at: Option<Time>,

    /// Standard conditions: Ready, Creating, Deleting.
    pub conditions: Vec<Condition>,

    pub observed_generation: Option<i64>,
}
```

Scope: namespaced. Printer columns: `VM`, `Tier`, `Ready`, `Age`,
`SizeBytes` (priority 1).

### SnapshotSchedule

```rust
pub struct SnapshotScheduleSpec {
    /// Target VM: either a direct ref or a label selector across the
    /// namespace.
    pub target: ScheduleTarget,

    /// Retention tiers, evaluated independently.
    pub tiers: Vec<RetentionTier>,

    /// Suspend the entire schedule.
    #[serde(default)]
    pub paused: bool,
}

#[serde(untagged)]
pub enum ScheduleTarget {
    Direct { vm_ref: LocalObjectReference },
    Selector { vm_selector: LabelSelector },
}

pub struct RetentionTier {
    /// Tier name (lowercase, alphanumeric + dashes). Used as a label
    /// on produced snapshots.
    pub name: String,

    /// Cron expression. 5-field standard cron.
    pub schedule: String,

    /// Number of snapshots to retain in this tier; oldest beyond
    /// `keep` are pruned.
    pub keep: u32,

    /// Whether to include memory state. Default false.
    #[serde(default)]
    pub memory: bool,

    /// Optional description template; supports {{ tier }} and {{ vm }}.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

pub struct SnapshotScheduleStatus {
    pub last_run_by_tier: BTreeMap<String, Time>,
    pub next_run_by_tier: BTreeMap<String, Time>,
    pub conditions: Vec<Condition>,
    pub observed_generation: Option<i64>,
}
```

Scope: namespaced. Printer columns: `Target`, `Tiers`, `Paused`,
`Age`.

Labels banlieue applies to snapshots it creates:
- `banlieue.io/snapshot-tier: <tier-name>`
- `banlieue.io/snapshot-schedule: <schedule-name>`
- `banlieue.io/snapshot-vm: <vm-name>`

## Architecture

```
SnapshotSchedule controller       VirtualMachineSnapshot controller        Provider's per-snapshot reconciler
─────────────────────────────     ─────────────────────────────────       ──────────────────────────────────
* Holds a cron-driven scheduler.  * Watches all VMSnapshots.               * Watches its own InfraMachineSnapshot
* When a tier fires:              * For each: ensures a corresponding        kind (see below) and effects
  - Create VMSnapshot with tier     InfraMachineSnapshot exists on the       provider operations.
    label and ownerRef to schedule. provider side.
  - Update status.lastRunByTier.  * Mirrors status back from infra ref.
* On reconcile, evaluate retention
  per tier and DELETE excess
  VMSnapshots (which cascades to
  the provider).
```

## Per-provider snapshot CRD

Following the same CR-to-CR pattern, each provider needs an infra-side
snapshot CRD:

- `VSphereMachineSnapshot` in `infrastructure.banlieue.io`
- `ProxmoxMachineSnapshot`
- `LibvirtMachineSnapshot`

Each has:
- Spec: `machineRef: LocalObjectReference` (to the parent
  ProviderMachine), `memory: bool`, `description: Option<String>`.
- Status: `providerSnapshotId`, `sizeBytes`, `createdAt`, conditions.

The main snapshot controller creates one of these for each
`VirtualMachineSnapshot`, owned by the snapshot CR. The provider
reconciler does the work.

## Snapshot controller (`banlieue-controller`)

Add a new reconciler module:

```
crates/banlieue-controller/src/snapshot/
├── mod.rs
├── snapshot_reconciler.rs       // VirtualMachineSnapshot
├── schedule_reconciler.rs       // SnapshotSchedule
├── cron.rs                      // tokio-cron-scheduler integration
└── retention.rs                 // per-tier pruning logic
```

Dependencies:

```toml
tokio-cron-scheduler = "0.10"  # pin exactly; verify API
chrono = { version = "0.4", features = ["serde"] }
cron = "0.12"
```

### SnapshotSchedule reconciler

On each reconcile:

1. Validate cron expressions; fail with `ScheduleValid=False` if bad.
2. Resolve target VM(s): direct ref or selector.
3. Register / refresh cron jobs in the scheduler keyed by
   `(schedule-name, namespace, tier-name)`.
4. For each tier:
   - If a fire happened since last reconcile (recoverable on
     restart): emit a `VirtualMachineSnapshot` per resolved VM with
     the tier label.
   - Compute `nextRunByTier` from cron + now.
5. Evaluate retention:
   - For each `(vm, tier)`, list snapshots labeled
     `banlieue.io/snapshot-tier=<tier>` and
     `banlieue.io/snapshot-vm=<vm>`, sorted by `status.createdAt`.
   - Delete oldest until count ≤ `keep`.

Persistence: the cron scheduler is in-memory. On controller restart,
reload schedules from the CRs and resume; misses are tolerated (we
don't try to backfill firings missed during downtime).

### VirtualMachineSnapshot reconciler

```
1. Resolve vm_ref. If VM not found: condition Ready=False reason=MissingVM,
   eventually fail.
2. Determine the provider class from the VM's status.scheduled.providerClass.
3. Ensure an Infra snapshot CR exists (e.g. VSphereMachineSnapshot)
   owned by this VirtualMachineSnapshot. SSA it.
4. Mirror status from the Infra snapshot back to VirtualMachineSnapshot.
5. On deletion: trigger finalizer; delete owned Infra snapshot;
   await cascade.
```

## Provider snapshot reconcilers

In each provider crate, add a reconciler for the infra snapshot kind.

### vSphere

- `create`: `Snapshot_Task(name, description, memory, quiesce)` on
  the VM moref; await task; record snapshot moref.
- `delete`: `RemoveSnapshot_Task(snapshot, removeChildren=true)`.
- Provider snapshot size: each delta file's size; sum
  reports best-effort.
- **Consolidation watchdog**: if `needsConsolidation` flag is set on
  the VM, emit a warning event. Do not auto-consolidate in v1.

### Proxmox

- `create`: `POST /nodes/{node}/qemu/{vmid}/snapshot` with snapname,
  vmstate (= memory). Poll UPID.
- `delete`: `DELETE /nodes/{node}/qemu/{vmid}/snapshot/{name}`. Poll.
- Naming: `banlieue-<vmsnap-name>` (Proxmox snapnames are restricted
  to alphanumeric + underscore).

### Libvirt

- The general libvirt snapshot model is complex; multiple modes
  (internal qcow2, external, with/without memory). For v1:
  - **Power-off snapshots** via `virDomainSnapshotCreateXML` with
    internal qcow2 mode; reliable, supported widely.
  - Memory snapshots: only supported if backing volume is qcow2 and
    domain is running; gate behind a feature.
- `delete`: `virDomainSnapshotDelete`.
- **Warning**: libvirt snapshot semantics are storage-driver specific.
  Document precisely which configs are supported; refuse the others
  via `Capable=False`.

## Retention algorithm

Per tier, per VM:

```python
snapshots = list_snapshots(vm=vm, tier=tier, sort=desc-created_at)
to_keep = snapshots[:tier.keep]
to_delete = snapshots[tier.keep:]
for snap in to_delete:
    delete(snap)
```

Edge cases:
- Snapshots still in `Creating` state are not eligible for pruning.
- Snapshots marked with `banlieue.io/snapshot-protected: "true"`
  annotation are never pruned (lets users pin individual snapshots).
- If a tier's `keep` decreases, the controller prunes down on the
  next reconcile.

## Tasks

- [ ] Add `VirtualMachineSnapshot`, `SnapshotSchedule`,
      `VSphereMachineSnapshot` (+ Proxmox/Libvirt counterparts) to
      `banlieue-api`. Regenerate CRDs.
- [ ] Scaffold `crates/banlieue-controller/src/snapshot/`.
- [ ] Implement cron driver + persistence-on-restart.
- [ ] Implement `schedule_reconciler.rs` (cron jobs + retention).
- [ ] Implement `snapshot_reconciler.rs` (infra CR
      management + status mirror).
- [ ] In each provider crate, add the snapshot reconciler.
  - [ ] vSphere
  - [ ] Proxmox
  - [ ] Libvirt (with documented limitations)
- [ ] RBAC updates for all controllers.
- [ ] Webhook validation for SnapshotSchedule (valid cron, valid
      target).

## Tests

- [ ] Retention algorithm unit tests with table-driven inputs.
- [ ] Schedule reconciler integration test: synthetic clock,
      verify firings produce snapshots.
- [ ] Per-provider snapshot create/delete with mocks.
- [ ] End-to-end: schedule with 3 tiers, wait for a few firings (use
      sub-minute cron in test), verify retention.

## Definition of done

- Manual snapshots work end-to-end on all available providers.
- A SnapshotSchedule with hourly/daily/weekly tiers operates
  correctly for 24 hours of operation.
- Retention prunes correctly per-tier.
- Cron expressions are validated at admission.
- Documentation in `docs/user/snapshots.md` (Phase 4 finalizes; stub
  here).

## Gotchas

- **Time skew**: the cron scheduler runs in controller-local time;
  if the controller node clocks are skewed, snapshots fire at "wrong"
  times. Document NTP requirement.
- **Cron expression dialects**: 5-field standard cron only; reject
  6-field (with seconds) and `@yearly`-style macros at admission to
  keep behavior portable.
- **vSphere snapshot delta blowup**: a long chain of snapshots
  causes significant storage and IO overhead. Document the
  consolidation requirement; consider a future ADR for automatic
  consolidation policies.
- **Proxmox memory snapshots and ballooning**: ballooning can fight
  with memory snapshots. The provider should disable ballooning
  before memory snapshot and restore after, or document the caveat.
- **Libvirt external snapshots vs internal**: pick one model and
  stick with it for v1. Internal qcow2 is simpler and more common.
- **Schedule controller restart**: missed firings during downtime
  are not backfilled. Document this explicitly. A future ADR may
  add an `onMissed: skip | backfill-one` policy.
- **Snapshot during migration**: refuse to take a snapshot while
  the VM has `Migrating=True`. Surface as `Ready=False
  reason=VMMigrating` on the VMSnapshot.
- **Webhook order**: validating webhooks should run after defaulting
  ones; the SnapshotSchedule defaulting webhook may set
  `tier.memory` defaults that the validator then checks.
