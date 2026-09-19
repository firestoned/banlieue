# banlieue — Roadmap Overview

> **Read this first.** All other roadmap documents assume the context here.

## What banlieue is

A Kubernetes-native abstract virtualization API. Users create
`VirtualMachine` CRs; banlieue's controllers schedule them onto one of
several backends (vSphere, Proxmox, libvirt) via provider-specific
infrastructure CRDs that satisfy the **CAPI v1beta2 InfraMachine
contract**.

The whole control plane is K8s-native: providers communicate with the
main controller through CRDs, not gRPC and not REST. This is the
primary architectural delta from `virtrigaud`.

## What's already done — Phase 0

The `banlieue-api` crate exists with these types:

- `banlieue.io/v1alpha1`: `Provider`, `VMClass`, `VMImage`, `VirtualMachine`
- `infrastructure.banlieue.io/v1alpha1`: `VSphereMachine`, `VSphereMachineTemplate`

CRD YAML can be generated with:

```sh
cargo run -p banlieue-api --bin crdgen --features crdgen
```

Examples in `examples/` show every CRD wired together.

**Before doing anything else**, confirm `cargo check -p banlieue-api`
passes on your machine. If schemars complains about
`rename_all_fields`, see `02-CONVENTIONS.md` for the patch.

## Phase plan

Phases are sequential. Sub-phases within Phase 1 are parallelizable
after 1A lands.

| Phase | What | Why first |
|---|---|---|
| **1A** | Main controller + provider SDK | Everything else needs this |
| **1B** | vSphere provider (`vim_rs`) | Richest backend; constraints found here shape the abstractions |
| **1C** | Proxmox provider | Second-richest; sanity-check the abstractions |
| **1D** | Libvirt provider | Tests the "providers without native tiering" path |
| **2** | Snapshots + GFS scheduling | Most-requested feature beyond CRUD |
| **3** | Provider lifecycle (auto-Deployment) | Operational nicety; defer until 1+2 are stable |
| **4** | FINOS-ready polish | Governance, docs, CAPI integration, release |

## How to use these docs with Claude Code / Windsurf

Each phase document is self-contained enough to be the active context
for a coding session. Suggested workflow:

1. Open the phase doc you're working on in the editor.
2. Start a Claude Code session with that file plus `01-DECISIONS.md`
   and `02-CONVENTIONS.md` as context.
3. Reference the existing `banlieue-api` crate as the source of truth
   for type shapes — do not invent new CRD fields on the fly; if you
   need one, edit `banlieue-api` first and rerun `crdgen`.
4. Architecturally significant work follows **ADD**:
   `ADR → CALM → TDD → implement → docs → threat model`, in that order
   (`rules/architecture-driven-development.md`). A roadmap entry says *what*
   and *why*; it never substitutes for the ADR on *how*.

Open questions in phase docs are marked **OPEN:** — answer those before
writing code, and record the answer back in `01-DECISIONS.md`.

## Non-negotiables (the principles)

These are locked. If a tradeoff seems to argue against one of these,
the answer is to find a different tradeoff, not to relax the principle.

1. **No RPC between main controller and providers.** Communication is
   only via CRDs and the K8s API. If you find yourself wanting an HTTP
   or gRPC channel, you're solving the wrong problem.
2. **Provider CRDs satisfy the CAPI v1beta2 InfraMachine contract.**
   This is what makes them potentially reusable as CAPI infra providers
   and gives us a battle-tested status model.
3. **The user-facing CR (`VirtualMachine`) is independent of CAPI.**
   It is *not* a `clusterv1.Machine`. It can coexist with CAPI but does
   not depend on it.
4. **Explicit over implicit.** Capabilities, image sources,
   credentials — all declared. Auto-discovery is a status-time concern,
   not a spec-time one.
5. **Idempotent reconciliation.** Every controller loop must be safely
   re-entrant. Patch status, never replace. Use server-side apply for
   owned objects.
6. **Status mirrors infra.** The `VirtualMachine` status is derived
   from the infrastructure ref's status. Never set
   `status.initialization.provisioned=true` on a `VirtualMachine`
   without the underlying infra CR saying so.

## Repository layout (actual)

```
banlieue/
├── Cargo.toml                       # workspace
├── ROADMAPS.md                      # status board — one row per roadmap
├── .github/community/               # ← you are here (roadmap docs)
├── docs/
│   ├── adr/                         # ADRs, NNNN-title.md — the decision log
│   ├── architecture/calm/           # FINOS CALM model + generated diagrams
│   ├── design/                      # contract / design notes
│   └── src/                         # MkDocs Material site (Phase 1E)
├── crates/
│   ├── banlieue/                    # the ONLY binary; subcommand dispatch (ADR-0004)
│   ├── banlieue-api/                # CRD types — source of truth
│   ├── banlieue-controller/         # main controller
│   ├── banlieue-provider-sdk/       # shared runtime helpers
│   ├── banlieue-provider-vsphere/   # vSphere provider
│   ├── banlieue-provider-libvirt/   # libvirt provider
│   ├── banlieue-libvirt/            # pure-Rust libvirt native-RPC client (ADR-0011/0050)
│   ├── banlieue-operator/           # ProviderClass → workloads (ADR-0012)
│   ├── banlieue-imagebuilder/       # VMImage build pipeline (ADR-0010)
│   └── banlieue-vex/                # VEX / supply-chain tooling
├── deploy/
│   ├── crds/                        # generated via crdgen — never hand-edited
│   ├── admission/                   # ValidatingAdmissionPolicies (ADR-0007)
│   ├── controller/ operator/ imagebuilder/ provider-*/
│   └── kind/                        # dev only
└── examples/
```

`banlieue-provider-proxmox` does not exist yet (roadmap 12, ⛔). There is no
Helm chart yet (Phase 4 §4.6).

> **Note.** `banlieue-libvirt`, `banlieue-operator`, `banlieue-imagebuilder`
> and `banlieue-vex` grew out of ADRs rather than roadmap phases; their
> decision record is in `docs/adr/`, not in this directory. See the note at
> the foot of [`ROADMAPS.md`](../../ROADMAPS.md).

## Pre-flight checks before starting any phase

Run these and make sure they all pass:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo run -p banlieue-api --bin crdgen --features crdgen > /tmp/banlieue-crds.yaml
kubectl apply --dry-run=client -f /tmp/banlieue-crds.yaml
kubectl apply --dry-run=client -f examples/
```

If any of those fail in `main`, fix them before starting new work.

## Versioning and stability

- API version: `v1alpha1` until we ship a feature-complete Phase 2.
- Then `v1beta1` with conversion webhooks.
- `v1` happens at FINOS-readiness (Phase 4).

We follow Kubernetes deprecation policy for any field changes.

## Order of operations within a phase

For every phase doc, treat the task list as a topological order. You
may parallelize anything not in a dependency line, but don't skip
ahead — earlier items often establish patterns the later items reuse.
