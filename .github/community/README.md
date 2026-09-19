# banlieue Roadmap Index

This directory holds the roadmap docs for building banlieue.

## Reading order

| # | File | What | When to read |
|---|---|---|---|
| 00 | `00-OVERVIEW.md` | The shape of the project, principles, phases | First |
| 01 | `01-DECISIONS.md` | Locked design decisions with rationale | Before any phase |
| 02 | `02-CONVENTIONS.md` | Code style, error handling, testing, observability | Before writing code |
| 03 | `03-AVAILABILITY-ZONES-AND-DATASTORE-TIERING.md` | AZ model: uniform tiering, local-only datastores, no cross-datastore access | Before failure-domain / scheduler work |
| 10 | `10-PHASE-1A-CONTROLLER-AND-SDK.md` | Main controller + provider SDK | Phase 1A |
| 11 | `11-PHASE-1B-VSPHERE-PROVIDER.md` | vSphere provider (`vim_rs`) | Phase 1B |
| 12 | `12-PHASE-1C-PROXMOX-PROVIDER.md` | Proxmox provider | Phase 1C |
| 13 | `13-PHASE-1D-LIBVIRT-PROVIDER.md` | Libvirt provider | Phase 1D |
| 14 | `14-PHASE-1E-DOCS.md` | MkDocs Material site scaffold (mirrors 5-spot) | Parallel with 1A–1D |
| 20 | `20-PHASE-2-SNAPSHOTS.md` | Snapshots + GFS scheduling | Phase 2 |
| 30 | `30-PHASE-3-PROVIDER-LIFECYCLE.md` | ProviderClass + auto-Deployment | Phase 3 |
| 40 | `40-PHASE-4-FINOS-READY.md` | Polish, governance, CAPI integration, release | Phase 4 |
| 50 | `50-IPAM-POOL-INTEGRATION.md` | CAPI IPAM pool integration (ADR-0033) — deferred, not started | After the virtrigaud migration; needs a decision from the existing IPAM system's owning team first |
| 51 | `51-LIVE-MIGRATION.md` | Same-class live migration, e.g. vSphere relocate (ADR-0036) — deferred, not started | After ADR-0035's placement-drift watch made `Recreate` fire more often; cross-class migration explicitly out of scope |
| 60 | `60-SCORECARD-REMEDIATION.md` | OSSF Scorecard: what to click, and which checks are deliberately capped | Before touching repo settings or "fixing" a Scorecard alert |
| 70 | `70-ephemeral-vm-pools.md` | `VirtualMachinePool` + `VirtualMachineClaim`: warm, never-reused, TPM-sealed single-use VMs | Largest open initiative; read after 13 (libvirt is its first live target) |

Status for every row above lives in [`ROADMAPS.md`](../../ROADMAPS.md) at the
repo root — this table is the reading order, that one is the status board.

## Using these with Claude Code / Windsurf

A suggested working pattern:

1. **Start a Claude Code session** with these three files always in context:
   - `00-OVERVIEW.md`
   - `01-DECISIONS.md`
   - `02-CONVENTIONS.md`
2. **Open the phase doc** for the current work and pin it.
3. **Don't let Claude Code re-litigate locked decisions.** If it
   suggests changing an architectural call, redirect it to write an
   ADR in `docs/adr/NNNN-title.md` instead — and note that `docs/adr/`,
   not `01-DECISIONS.md`, is now the canonical decision log.
4. **Treat task lists as the source of truth.** When something is
   done, check it off in the file.
5. **Record a resolved open question as an ADR** in `docs/adr/`, then
   annotate the `01-DECISIONS.md` entry to point at it. The ADR is the
   durable record; the entry is the breadcrumb.

## Phase dependencies

```
Phase 1A (controller + SDK)  ✅
   ├─→ Phase 1B (vSphere)    ✅
   ├─→ Phase 1C (Proxmox)    ⛔   can run in parallel after 1A lands
   ├─→ Phase 1D (libvirt)    🔶
   └─→ Phase 1E (MkDocs)     🔶   no preconditions
         │
         ▼
   Phase 2 (snapshots)       ⛔   needs at least one provider
         │
         ▼
   Phase 3 (lifecycle)       🔶   ProviderClass + banlieue-operator exist
         │
         ▼
   Phase 4 (FINOS-ready)     🔶   everything that's left

   70 (ephemeral VM pools)   ⛔   gated on 1D for live testing
```

## Updates to these docs

These roadmaps are living documents. Update them as design evolves
during implementation. Significant changes that affect architecture
land as ADRs in **`docs/adr/NNNN-title.md`** (lowercase-hyphen,
zero-padded, one decision per ADR) and are cross-linked from the relevant
phase doc.

Two rules that are easy to miss:

- **Update the status row in [`ROADMAPS.md`](../../ROADMAPS.md) in the same
  commit** that changes an item's state. It is a status board, not a
  description of intent.
- **ADRs, not `01-DECISIONS.md`, are the canonical record.** That file is
  the pre-ADR log, kept and annotated for history; a new decision goes in
  `docs/adr/`.
