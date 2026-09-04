# Roadmaps

High-level index of banlieue's roadmap documents. Full detail for each
phase/initiative lives in [`.github/community/`](.github/community/) — this
file tracks what each one is and its current completion status; the
detailed task lists and design rationale live in the linked doc itself.

Per [`rules/architecture-driven-development.md`](rules/architecture-driven-development.md),
architecturally significant work in any roadmap below still goes
**ADR → CALM → TDD → implement → docs**, in that order — a roadmap entry
describes *what* and *why*, it does not skip the ADR for *how*.

## Status legend

| Symbol | Meaning |
|---|---|
| ✅ | Done — implemented, tested, in the codebase today |
| 🔶 | In progress — some of it exists, not complete |
| ⛔ | Not started |
| 📄 | Reference doc — not a phase with a completion state (principles/conventions) |

## Index

| # | Roadmap | Status | Notes |
|---|---|---|---|
| [00](.github/community/00-OVERVIEW.md) | Overview | 📄 | Read this first — project shape, principles, phase map |
| [01](.github/community/01-DECISIONS.md) | Decisions | 📄 | Locked design decisions with rationale; propose an ADR to change one |
| [02](.github/community/02-CONVENTIONS.md) | Conventions | 📄 | Code style, error handling, testing, observability |
| [03](.github/community/03-AVAILABILITY-ZONES-AND-DATASTORE-TIERING.md) | Availability zones & datastore tiering | ✅ | Failure-domain mapping implemented in `banlieue-provider-vsphere` (verified live against a real vCenter — 3 failure domains reported) |
| [10](.github/community/10-PHASE-1A-CONTROLLER-AND-SDK.md) | Phase 1A — Controller + SDK | ✅ | `banlieue-controller` + `banlieue-provider-sdk` crates exist and are the foundation every provider builds on |
| [11](.github/community/11-PHASE-1B-VSPHERE-PROVIDER.md) | Phase 1B — vSphere provider | ✅ | `banlieue-provider-vsphere` — the most mature provider; validated end-to-end against real vCenter/ESXi infrastructure |
| [12](.github/community/12-PHASE-1C-PROXMOX-PROVIDER.md) | Phase 1C — Proxmox provider | ⛔ | No `banlieue-provider-proxmox` crate exists yet |
| [13](.github/community/13-PHASE-1D-LIBVIRT-PROVIDER.md) | Phase 1D — Libvirt provider | 🔶 | `banlieue-provider-libvirt` crate exists with a real reconciler and tests; maturity behind the vSphere provider |
| [14](.github/community/14-PHASE-1E-DOCS.md) | Phase 1E — Docs site | 🔶 | MkDocs Material site scaffolded and building (`docs/`, `docs/site/`); ongoing content work |
| [20](.github/community/20-PHASE-2-SNAPSHOTS.md) | Phase 2 — Snapshots | ⛔ | No `VirtualMachineSnapshot`/`SnapshotSchedule` CRDs yet |
| [30](.github/community/30-PHASE-3-PROVIDER-LIFECYCLE.md) | Phase 3 — Provider lifecycle | 🔶 | `ProviderClass` CRD exists and `banlieue-operator` consumes it; lifecycle automation maturity not fully assessed |
| [40](.github/community/40-PHASE-4-FINOS-READY.md) | Phase 4 — FINOS-ready | 🔶 | `SECURITY.md` and ADRs exist; `GOVERNANCE.md`/`CODE_OF_CONDUCT.md`/`CONTRIBUTING.md` and CAPI integration not yet done |
| [50](.github/community/50-IPAM-POOL-INTEGRATION.md) | IPAM pool integration (ADR-0033) | ⛔ | Deferred — needs a decision from the existing IPAM system's owning team first |
| [51](.github/community/51-LIVE-MIGRATION.md) | Live migration (ADR-0036) | ⛔ | Only the `Recreate`-only placeholder exists today (`crates/banlieue-controller/src/reconciler/migration.rs`); graceful live migration itself not started |

## Keeping this current

When a roadmap item's status changes (something lands, something new
starts), update its row here in the same PR/commit that makes the change —
this file is a status board, not documentation of intent. Detailed
task-level tracking stays inside each roadmap doc; this file only tracks
the phase-level state.
