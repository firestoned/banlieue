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

## Using these with Claude Code / Windsurf

A suggested working pattern:

1. **Start a Claude Code session** with these three files always in context:
   - `00-OVERVIEW.md`
   - `01-DECISIONS.md`
   - `02-CONVENTIONS.md`
2. **Open the phase doc** for the current work and pin it.
3. **Don't let Claude Code re-litigate locked decisions.** If it
   suggests changing an architectural call, redirect it to propose
   an ADR in `docs/design/` instead.
4. **Treat task lists as the source of truth.** When something is
   done, check it off in the file.
5. **Annotate open questions back into `01-DECISIONS.md`** as they
   get resolved — this is the durable record.

## Phase dependencies

```
Phase 1A (controller + SDK)
   ├─→ Phase 1B (vSphere)
   ├─→ Phase 1C (Proxmox)       can run in parallel after 1A lands
   ├─→ Phase 1D (libvirt)
   └─→ Phase 1E (MkDocs site)   no preconditions; can start now
         │
         ▼
   Phase 2 (snapshots)            needs at least one provider
         │
         ▼
   Phase 3 (provider lifecycle)   needs the manual Deployment patterns
         │                        already established
         ▼
   Phase 4 (FINOS-ready)          everything that's left
```

## Updates to these docs

These roadmaps are living documents. Update them as design evolves
during implementation. Significant changes that affect architecture
should land as ADRs in `docs/design/adr-NNN-*.md` and be cross-linked
from the relevant phase doc and `01-DECISIONS.md`.
