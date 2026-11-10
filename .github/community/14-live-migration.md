# Live Migration (same-class first, cross-class deferred)

> **Goal.** Replace destroy-and-rebuild (`MigrationAction::Recreate`) with
> a graceful migration path for `VirtualMachine`s whose placement legitimately
> needs to change — starting with **same-provider-class** migration (e.g.
> vSphere-to-vSphere, using vMotion-equivalent relocation), where a native
> per-backend mechanism already exists.
>
> **Stop condition (Phase A only — this roadmap does not cover Phase B).**
> A `VirtualMachine` with `migrationPolicy: Automatic` whose new `Decision`
> lands on a different `Provider`/failure domain *of the same
> `providerClassRef`* migrates without being destroyed and rebuilt —
> ideally with no guest downtime for classes that support it (vSphere
> vMotion), or at minimum without losing disk contents for classes that
> don't support live relocation but do support offline move.
>
> **Status: not started.** Deferred by explicit decision — see
> [ADR-0036](../../docs/adr/0036-live-migration-phased-approach.md)
> for the full design conversation, why same-class and cross-class are
> split into separate problems, and why cross-class is explicitly
> unscoped here. **Read that ADR before touching this roadmap** — it is
> the source of truth for the reasoning; this file is only the execution
> plan for Phase A once someone picks the work up.
>
> *Updated 2026-11-10:* the Cloud Hypervisor class has its own design,
> [ADR-0127](../../docs/adr/0127-move-a-cloud-hypervisor-vm-between-hosts-in-a-zone.md)
> (Proposed): a move between two hosts of one zone on local storage, the
> disk copied by the host providers, requested by faubourg's host drain
> (faubourg ADR-0017). Its tasks are in
> [Cloud Hypervisor (ADR-0127)](#cloud-hypervisor-adr-0127) and do not wait
> for the vSphere tasks.

## Why this exists as its own roadmap entry

[ADR-0035](../../docs/adr/0035-virtualmachine-watches-provider-and-vmclass.md)
made placement-drift detection event-driven (a `Provider` label edit now
re-triggers scheduling immediately instead of waiting out a poll
interval). That's strictly a correctness improvement, but it also means
the existing `MigrationAction::Recreate` path — destroy the backend VM,
rebuild from scratch, zero preservation — can now fire sooner and more
often under `migrationPolicy: Automatic`. This roadmap is the follow-up
work to make that response graceful, at least for the case that's
actually buildable today (same provider class).

## Preconditions

- [x] ADR-0036 accepted (Accepted 2026-09-19).
- [ ] Decide and document, per provider class, whether it has *any*
      native in-place relocation mechanism at all:
  - vSphere: yes — `RelocateVM_Task` (vim_rs already used throughout
    `banlieue-provider-vsphere`), the same primitive vMotion/Storage
    vMotion use. Cross-datastore and cross-cluster (same vCenter) relocate
    are both real vim_rs operations; cross-vCenter (two different
    `Provider`s) is a materially harder vim_rs case (`RelocateVM_Task`
    doesn't cross vCenter boundaries — that needs the newer
    cross-vCenter vMotion APIs, confirm vim_rs 0.5 exposes them before
    committing to cross-*Provider* same-class migration, not just
    cross-failure-domain-within-one-Provider).
  - Proxmox: has its own live-migration API over shared storage —
    unresearched as of this writing; confirm before scoping Proxmox Phase
    A tasks.
  - libvirt: no live migration in the currently-scaffolded v1 design
    (per `migration.rs`'s own existing doc comment) — Phase A likely
    never applies to libvirt; confirm this is still true before writing
    libvirt-specific tasks, and if so, libvirt's `MigrationPolicy:
    Automatic` may always mean recreate, by design, indefinitely.
  - Cloud Hypervisor: answered by ADR-0127 (2026-11-10). Upstream migrates
    a VM between two VMMs but not its disk (cloud-hypervisor#8006, open) or
    its vTPM (no support in `devices/src/tpm.rs`), so banlieue's host
    providers copy the disk and a vTPM guest waits for the vTPM to move
    inside Cloud Hypervisor's own stream.

## Per-class capability

What each provider class could offer Phase A, and what it does today.
Update a row when a backend's answer changes.

| Class | Today | Native mechanism | Phase A outlook |
|---|---|---|---|
| vSphere | `Recreate` | `RelocateVM_Task` (vMotion, Storage vMotion) within one vCenter; cross-vCenter needs newer APIs | The first target: in place, no guest downtime, within one `Provider` |
| libvirt | `Recreate` | libvirt can migrate, but banlieue's provider implements none of it, and pools are host-local | Likely `Recreate` by design (see Preconditions) |
| Cloud Hypervisor | `Recreate` | Upstream `vm.send-migration` / `vm.receive-migration` between two VMMs, only with the disk already on both hosts (block migration is cloud-hypervisor#8006, open); the vTPM is not migrated, and swtpm serves one control client (the VMM), so only the VMM can carry its state | ADR-0127 (Proposed 2026-11-10): a move between two hosts with the same `banlieue.io/zone`, on local storage. The host providers copy the disk (reflink precopy, delta at cutover) and relay the VMM stream over one per-move TLS link; the controller coordinates through the two `CloudHypervisorMachine`s, so no RPC. Stage 1 moves VMs without a vTPM, pausing for the delta plus all guest memory; vTPM guests, live memory and upstream block migration are Stages 2 to 4 |
| Proxmox | no provider | Live migration over shared storage (unresearched) | Unscoped until roadmap 06 lands |

## Current state (confirmed in code, as of this writing)

- `crates/banlieue-controller/src/reconciler/migration.rs`: `evaluate()`
  is pure and already correctly detects drift and dispatches on
  `MigrationPolicy` — `MigrationAction::Recreate` is the only "do
  something about drift" outcome that exists. No per-provider-class
  distinction exists anywhere in this logic today; it doesn't know or
  care whether old and new placement share a `providerClassRef`.
- `crates/banlieue-api/src/infrastructure/vsphere_machine.rs`: no field
  anywhere resembling "can relocate in place" or a stable disk handle
  that would survive a relocate — `disks: Vec<VSphereDiskSpec>` always
  describes a clone-time request, never a live reference to existing
  vmdks.
- `VirtualMachineStatus.conditions`'s doc comment lists an optional
  `Migrating` condition type; nothing sets it anywhere in the codebase.
- *Updated 2026-09-27:* `VSphereMachine`, `LibvirtMachine` and
  `CloudHypervisorMachine` are real infra CRDs
  (`banlieue-api/src/infrastructure/`); only Proxmox has no provider. None
  implements any relocation: see [Per-class capability](#per-class-capability).
  Phase A should still start with vSphere, the only class with an in-place
  mechanism that needs no shared storage.

## Design (recap from ADR-0036 — read the ADR for full reasoning)

1. Add a per-`Provider`-pair (or per-provider-class) capability signal:
   "can a VM currently on Provider X migrate in place to Provider Y
   without recreate?" — likely `Provider.status` gains a field, or this
   becomes a small trait-like contract each provider's own reconciler
   satisfies, read by `migration.rs`'s `evaluate()` as an extra input
   alongside the existing drift comparison. `banlieue-controller` itself
   must never contain vSphere-specific relocate logic (non-negotiable:
   main controller never talks to a backend) — it only ever asks "can
   you migrate," and if yes, creates/patches something the *provider*
   acts on (most likely: a new field on `VSphereMachine.spec`, e.g.
   `relocateTo: { datacenter, cluster, datastore }`, that the vSphere
   provider's own reconciler notices and executes via `RelocateVM_Task`
   — mirroring how `desiredPowerState` already works, not a new
   cross-crate RPC).
2. New `MigrationAction` variant (name TBD, e.g. `Relocate`) distinct from
   both `InPlace` and `Recreate` — chosen only when the capability signal
   says the same-class in-place path is available; `Recreate` remains the
   fallback for everything else (cross-class, no relocate support, or the
   capability signal says no).
3. `VirtualMachineStatus.conditions` gets a real `Migrating=True` while a
   `Relocate` is in flight, and (per ADR-0036 Decision #2) `Recreate`
   should get its own distinct reason so the two are never visually
   conflated even before Phase A ships.
4. vSphere provider: new reconcile step watching for a `relocateTo`
   request on an already-`vmRef`-populated `VSphereMachine`, issuing
   `RelocateVM_Task`, and clearing the request + patching status once the
   task completes — same "wait_for_task" pattern already used everywhere
   else in `client/vim.rs`.

## Tasks

### Schema / plumbing

- [ ] Decide the exact shape of the "can migrate in place" capability
      signal (Provider status field vs. a lookup banlieue-controller does
      some other way) — this is a real design decision, not a mechanical
      task; don't start implementation until it's settled.
- [ ] Add `VSphereMachineSpec.relocateTo` (or equivalent) — datacenter/
      cluster/datastore, same shape family as the existing scheduling
      fields.
- [ ] Regenerate CRDs (`make crds`) once the schema is settled.

### Reconciler (banlieue-controller)

- [ ] New `MigrationAction::Relocate` variant in `migration.rs`, with pure
      unit tests (same style as the existing `MigrationAction` variants'
      tests) covering: same-class + capability=yes → `Relocate`;
      same-class + capability=no → `Recreate`; cross-class → always
      `Recreate` regardless of capability signal.
- [ ] `virtualmachine.rs`: new branch alongside the existing
      `MigrationAction` match arms, patching `relocateTo` onto the
      `VSphereMachine` instead of deleting it.
- [ ] `Migrating=True` condition while relocate is in flight;
      `PlacementValid`/`Ready` semantics during that window need explicit
      design (is the VM "Ready" mid-relocate? almost certainly not the
      same `Ready=True` as steady-state).

### Provider (vsphere)

- [ ] New reconcile branch in `vspheremachine.rs`: `relocateTo` present +
      `vmRef` already set → issue `RelocateVM_Task`, wait, clear the
      request field, patch status.
- [ ] Confirm cross-vCenter relocate (two different `Provider`s, not just
      two failure domains under one `Provider`) is actually reachable via
      vim_rs 0.5 before writing tasks assuming it is — this may turn out
      to be a Phase A.1 (same-Provider-different-failure-domain) vs. Phase
      A.2 (cross-Provider-same-class) split once researched.

### Tests (TDD, per this project's mandatory workflow)

- [ ] Pure `MigrationAction`/`evaluate()` tests for the new variant, per
      the existing test file's style (`migration_tests.rs`).
- [ ] `FakeClient`-based tests for the new vSphere relocate reconcile
      branch, mirroring `vspheremachine_ensure_tests.rs`'s existing
      pattern.
- [ ] An e2e scenario for `migrationPolicy` `Automatic` and `Manual`
      (carried over from roadmap 12 §4.5 when it closed, 2026-10-09).

### Docs (mandatory per this project's process — ADR → CALM → TDD → implement → docs)

- [ ] `docs/architecture/calm/architecture.json`: new flow for the
      relocate path, alongside the existing `flow-provision-a-vm`-style
      entries.
- [ ] `docs/src/guides/` — likely a new guide once the capability-signal
      design is final; users need to know which `Provider` pairs actually
      support this before setting `migrationPolicy: Automatic` and
      expecting graceful behavior.
- [ ] `.claude/CHANGELOG.md` entry, `**Author:**` line.
- [x] Flip ADR-0036's status from Proposed to Accepted once Phase A's
      design is final (even before full implementation lands, per this
      project's ADR-first convention), and open the still-deferred
      Phase B (cross-class) as its own future ADR only once Phase A
      exists and there's an actual disk-artifact concept to design
      against.

## Cloud Hypervisor (ADR-0127)

Read [ADR-0127](../../docs/adr/0127-move-a-cloud-hypervisor-vm-between-hosts-in-a-zone.md)
first; the stages are its Decision 13. faubourg's side is its ADR-0017
(`Host.spec.maintenance: Drain`). Depends on ADR-0125 (a placed VM keeps
its placement, merged in #178), without which the next scheduling pass
would undo a move.

### Design

- [x] ADR-0127 drafted (Proposed, 2026-11-10), with faubourg ADR-0017.
- [x] CALM: `data-asset-virtualmachinemove-cr`, `service-faubourg-controller`,
      the host-to-host link (`rel-provider-cloud-hypervisor-move-peer`), the
      VMM relay and `flow-move-virtualmachine-cloud-hypervisor`, all marked
      PROPOSED.
- [ ] ADR-0127 and faubourg ADR-0017 accepted.
- [ ] Spikes before Stage 1 code: reflink delta on XFS (`FIEMAP_EXTENT_SHARED`
      after guest writes and discards, against a running Cloud Hypervisor
      guest); a paused-to-paused `send-migration` over a relayed UNIX socket
      on the pinned VMM; the pause cost of `fsync` plus `FICLONE` on a busy
      disk.

### Stage 1: VMs without a vTPM

- [ ] `VirtualMachineMove` CRD (`banlieue-api`), `make crds`, examples.
- [ ] `CloudHypervisorMachine`: `spec.identity`, `spec.moveIn`/`moveOut`,
      `status.moveIn`/`moveOut`; derive MAC, taps, machine dir and EK
      subject from `spec.identity`.
- [ ] Controller: admission checks (zone label, `move` feature, VMM and
      CPU model, capacity, placement, policy, no vTPM), orchestration,
      `Migrating` and `Ready=False/Moving`, commit and abort.
- [ ] Provider: `[move] listen`, the `move` feature, per-move keys, the
      TLS 1.3 link (`X25519MLKEM768` only, pinned peer), reflink precopy and
      delta, VMM relay, resume on commit, abort paths.
- [ ] Tests: unit (admission, single-runner state machine, delta
      computation over a fake extent map); live tier on two real hosts
      (same MAC and address after a move, abort at every phase leaves one
      running guest); e2e through faubourg's drain.
- [ ] Docs: a guide (zone networking promise, `[move] listen`, firewall),
      CHANGELOG, threat-model pass (TB-10 host to host, the zone as a trust
      unit).

### Stage 2: VMs with a vTPM

- [ ] Upstream Cloud Hypervisor: the TPM device fetches and loads swtpm's
      `permanent` and `volatile` blobs in its migration stream, as QEMU does.
- [ ] A pinned Cloud Hypervisor release with it (and the vTPM response fix,
      cloud-hypervisor#9053).
- [ ] Destination re-certifies the moved EK with its own `swtpm_localca`.

### Stage 3: live memory

- [ ] Upstream: a destination-stays-paused option for `send-migration`
      (wire `CompletePaused` exists; cloud-hypervisor#8792).

### Stage 4: no banlieue disk copy

- [ ] Adopt upstream block migration (cloud-hypervisor#8006) when released.

## Open questions (answer before or during implementation)

- Does a relocate-in-flight VM's `providerID`/`instanceUuid` change? (It
  shouldn't for vMotion — same VM, same instance UUID, different host/
  datastore — confirm this holds for whatever vim_rs call gets used, since
  a changing `providerID` mid-relocate would break CAPI contract
  assumptions elsewhere.)
- What happens if a `Relocate` fails partway through (task errors, target
  datastore fills up mid-copy)? Needs a defined failure status distinct
  from both "still relocating" and "recreate needed" — falling back to
  `Recreate` automatically on a failed relocate may or may not be the
  right default; needs an explicit decision, not an assumption.
- Should `migrationPolicy: Manual` ever get access to the graceful
  `Relocate` path, or is Phase A relocate always automatic-only until
  proven safe in practice? (Leaning toward: expose it to Manual too once
  built — Manual only gates *whether* to act, not *how* — but this should
  be a stated decision, not an accident of implementation order.)

## Gotchas (anticipated)

- **Silent scope creep into Phase B.** The single biggest risk this
  roadmap entry exists to prevent: "just make migrate() smarter" style
  changes that quietly start assuming disk export/import exists. Any task
  here that would require moving a disk *between* provider classes is
  Phase B and does not belong in this file — cross-reference ADR-0036
  before adding it.
- **vMotion licensing/feature availability.** Not every vSphere deployment
  has vMotion licensed or configured (shared storage, vMotion network,
  etc.) — the capability signal must reflect *actually usable* relocate,
  not just "this is a vSphere Provider" (a vSphere `Provider` without
  vMotion configured should report capability=no, falling back to
  `Recreate`, not attempt and fail a `RelocateVM_Task`).
- **`RelocateVM_Task` failure modes are numerous and vSphere-specific**
  (insufficient resources on target, incompatible CPU features for
  live/vMotion specifically vs. cold relocate, network connectivity
  between hosts) — do not assume a clean success/fail binary; budget real
  investigation time for vim_rs's actual fault surface here before
  estimating this phase's size.
