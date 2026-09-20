# Roadmaps

High-level index of banlieue's roadmap documents. Full detail for each
phase/initiative lives in [`.github/community/`](.github/community/) — this
file tracks what each one is and its current completion status; the
detailed task lists and design rationale live in the linked doc itself.

Per [`.claude/rules/architecture-driven-development.md`](.claude/rules/architecture-driven-development.md),
architecturally significant work in any roadmap below still goes
**ADR → CALM → TDD → implement → docs → threat model**, in that order — a
roadmap entry describes *what* and *why*, it does not skip the ADR for *how*,
and an implemented ADR is not done until the threat-model pass
([`.claude/rules/threat-modeling.md`](.claude/rules/threat-modeling.md)) has run.

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
| [00](.github/community/00-overview.md) | Overview | 📄 | Read this first — project shape, principles, phase map |
| [01](.github/community/01-decisions.md) | Decisions | 📄 | The **pre-ADR** decision log (D-001…D-023), annotated 2026-09-19 with the ADRs that superseded individual entries. `docs/adr/` is now canonical; a new decision goes there |
| [02](.github/community/02-conventions.md) | Conventions | 📄 | Code style, error handling, testing, observability |
| [03](.github/community/03-availability-zones-and-datastore-tiering.md) | Availability zones & datastore tiering | ✅ | Failure-domain mapping implemented in `banlieue-provider-vsphere` (verified live against a real vCenter — 3 failure domains reported). Principle locked as **D-023** in [01-DECISIONS](.github/community/01-decisions.md) (2026-09-19); per-zone capability targets are ADR-0030 |
| [10](.github/community/10-phase-1a-controller-and-sdk.md) | Phase 1A — Controller + SDK | ✅ | `banlieue-controller` + `banlieue-provider-sdk` crates exist and are the foundation every provider builds on |
| [11](.github/community/11-phase-1b-vsphere-provider.md) | Phase 1B — vSphere provider | ✅ | `banlieue-provider-vsphere` — the most mature provider; validated end-to-end against real vCenter/ESXi infrastructure |
| [12](.github/community/12-phase-1c-proxmox-provider.md) | Phase 1C — Proxmox provider | ⛔ | No `banlieue-provider-proxmox` crate exists yet |
| [13](.github/community/13-phase-1d-libvirt-provider.md) | Phase 1D — Libvirt provider | ✅ | `banlieue-libvirt` is a first-party pure-Rust native-RPC client (ADR-0011); `LibvirtMachine` lifecycle (ADR-0050) and the NoCloud cloud-init seed (ADR-0054) are implemented and validated end-to-end against a real cluster and libvirt host — a `VirtualMachine` with `spec.userData` becomes a booted domain that receives it, and deleting it removes domain, disks and seed. Not in scope here: vTPM/swtpm (roadmap 70) and pool-based IPAM (roadmap 50) |
| [14](.github/community/14-phase-1e-docs.md) | Phase 1E — Docs site | 🔶 | MkDocs Material site scaffolded and building (`docs/`, `docs/site/`); ongoing content work |
| [20](.github/community/20-phase-2-snapshots.md) | Phase 2 — Snapshots | ⛔ | No `VirtualMachineSnapshot`/`SnapshotSchedule` CRDs yet |
| [30](.github/community/30-phase-3-provider-lifecycle.md) | Phase 3 — Provider lifecycle | 🔶 | `ProviderClass` CRD exists and `banlieue-operator` reconciles it into workloads (ADR-0012); deployment topology settled by ADR-0003, namespace isolation by ADR-0016. Lifecycle automation maturity not fully assessed |
| [40](.github/community/40-phase-4-finos-ready.md) | Phase 4 — FINOS-ready | 🔶 | Landed: `SECURITY.md`, 47 ADRs (duplicate numbers resolved 2026-09-19 — 0041/0042 collisions renumbered to 0051/0052), a published [threat model](docs/src/security/threat-model.md), admission policies (ADR-0007), signed/SBOM'd release pipeline (ADR-0006), `kind` E2E (ADR-0014). Outstanding: `GOVERNANCE.md`/`CODE_OF_CONDUCT.md`/`CONTRIBUTING.md`/`MAINTAINERS.md`, Helm chart, CAPI `clusterctl` integration |
| [50](.github/community/50-ipam-pool-integration.md) | IPAM pool integration (ADR-0033, ADR-0053) | ⛔ | Still deferred — needs a decision from the existing IPAM system's owning team on *which* CAPI IPAM provider before any code. Design gap now closed on banlieue's side: ADR-0033 covers `VirtualMachine`, ADR-0053 covers `VirtualMachinePool.spec.addressing` (pool stamps a `poolRef`, members own the claims) |
| [51](.github/community/51-live-migration.md) | Live migration (ADR-0036) | ⛔ | Only the `Recreate`-only placeholder exists today (`crates/banlieue-controller/src/reconciler/migration.rs`); graceful live migration itself not started |
| [60](.github/community/60-scorecard-remediation.md) | OSSF Scorecard remediation | 🔶 | Vulnerabilities fixed (2026-09-07); **Branch-Protection configured 2026-09-19** — ruleset `main` active with six required checks, which also unblocked Dependabot auto-merge. Code-Review / CII-Best-Practices still need a second reviewer or registration; Pinned-Dependencies capped at 9 by the SLSA generator's tag-only ref. ⚠️ Four required checks are `paths:`-filtered — docs-only PRs will block until an aggregator gate lands (roadmap 60 #1) |
| [70](.github/community/70-ephemeral-vm-pools.md) | Ephemeral single-use VM pools (AI agent sandboxes) | 🔶 | Phase D (`LibvirtMachine`, ADR-0050), B1 `VirtualMachinePool` (ADR-0046) and B2 `VirtualMachineClaim` (ADR-0047) landed and validated e2e against a real libvirt host; A2 `GuestReady` (ADR-0043) landed for libvirt, so Deferred/TPM-sealed pools are now achievable there. Open: A2's vSphere transport (no environment to verify against), `AgentSandbox` (ADR-0055, designed not built), a `ValidatingAdmissionPolicy` pinning a claim's `subject` to its caller (ADR-0047 Decision 10), and the section-0 slim-image experiment |

## Components tracked by ADR, not by a roadmap

These crates grew out of ADRs rather than a numbered phase, so they have no row
above. Their decision record is `docs/adr/`, and that is intentional — ADD
makes the ADR the canonical record, and not every component needs a roadmap:

| Crate | Record |
|---|---|
| `banlieue` (the single binary) | [ADR-0004](docs/adr/0004-single-binary-subcommand-dispatch.md) |
| `banlieue-operator` | [ADR-0012](docs/adr/0012-providerclass-crd-and-operator-role.md), [ADR-0003](docs/adr/0003-provider-deployment-topology.md) |
| `banlieue-imagebuilder` | [ADR-0010](docs/adr/0010-vmimage-build-pipeline-imagebuilder.md), [ADR-0016](docs/adr/0016-imagebuild-namespace-isolation.md) |
| `banlieue-libvirt` | [ADR-0011](docs/adr/0011-libvirt-provider-own-client.md), [ADR-0050](docs/adr/0050-libvirtmachine-domain-lifecycle.md) |
| `banlieue-vex` | supply-chain tooling; see [ADR-0006](docs/adr/0006-release-and-supply-chain-pipeline.md) |

Add a roadmap doc only when a component has multi-phase work to sequence.
A single decision belongs in an ADR.

## Keeping this current

When a roadmap item's status changes (something lands, something new
starts), update its row here in the same PR/commit that makes the change —
this file is a status board, not documentation of intent. Detailed
task-level tracking stays inside each roadmap doc; this file only tracks
the phase-level state.
