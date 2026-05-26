# Cerebrum

> OpenWolf's learning memory. Updated automatically as the AI learns from interactions.
> Do not edit manually unless correcting an error.
> Last updated: 2026-05-24

## User Preferences

<!-- How the user likes things done. Code style, tools, patterns, communication. -->

## Key Learnings

- **Project:** banlieue
- **Description:** A Kubernetes-native abstract virtualization API. Users create `VirtualMachine` CRs; controllers schedule them onto vSphere / Proxmox / libvirt backends via provider-specific infrastructure CRDs that satisfy the CAPI v1beta2 InfraMachine contract.
- **Architecture invariant:** No RPC between main controller and providers — communication is **CRD-only**. If a design wants HTTP/gRPC between them, it's wrong.
- **Workspace layout:** Cargo workspace. Phase 0 (done) ships `crates/banlieue-api` with two API groups: `banlieue.io/v1alpha1` (`Provider`, `VMClass`, `VMImage`, `VirtualMachine`) and `infrastructure.banlieue.io/v1alpha1` (`VSphereMachine`, `VSphereMachineTemplate`). Phase 1+ adds `banlieue-controller`, `banlieue-provider-sdk`, and `banlieue-provider-{vsphere,proxmox,libvirt}` per the out-of-repo roadmap at `~/dev/roadmaps/banlieue/`.
- **CRD generation:** `cargo run -p banlieue-api --bin crdgen --features crdgen` — the `crdgen` binary is feature-gated, so the flag is required. `serde_yaml` is also gated on the `crdgen` feature via `crdgen = ["dep:serde_yaml"]`.
- **License:** Apache-2.0 (workspace-wide, set in `[workspace.package]`).
- **Roadmap filename convention:** UPPERCASE with hyphens, numeric prefix (`00-OVERVIEW.md`, `10-PHASE-1A-CONTROLLER-AND-SDK.md`). ADRs use lowercase-hyphen.
- **k8s-openapi needs the `schemars` feature** for `Condition`, `Time`, and other meta/v1 types to implement `JsonSchema`. Without it, any field like `Vec<Condition>` or `Option<Time>` in a `JsonSchema`-deriving struct fails to compile. Pinned in workspace `[workspace.dependencies]`.
- **Test file colocation requires `#[path]`** because `src/foo.rs` is a leaf file-module. Pattern: at the bottom of `src/foo.rs` put `#[cfg(test)] #[path = "foo_tests.rs"] mod foo_tests;`, then put a `mod tests { use super::super::*; ... }` inside `src/foo_tests.rs`. Without `#[path]`, rustc looks for `src/foo/foo_tests.rs`. Test files do **not** need to re-import `crate::common::*` — `use super::super::*` already pulls in everything in scope in the parent module (child modules see private `use` imports from the parent).
- **kube-derive's CRD flattener cannot handle serde-tagged enums** (the actual root cause behind the original buglog bug-006). Each variant subschema of a `#[serde(tag = "x")]` enum naturally fixes `x` to a different enum value, but kube-core demands identical schemas for properties shared across subschemas. The CRD-generation panic is therefore fundamental to tagged enums, not a metadata bug. **The correct Kubernetes pattern is a flat struct** with a discriminator-string field plus optional sibling fields per variant. This is how `IpamSpec` was redesigned on 2026-05-25 (bug-006 fixed). Future learning: do not introduce `#[serde(tag = ...)]` enums on any type that needs to land in a CRD.
- **k8s-openapi 0.27 uses `jiff::Timestamp` for `Time`, not `chrono`.** Code that referenced `k8s_openapi::chrono::DateTime<Utc>` fails to compile. To produce a `Time` in tests without a direct jiff dep, deserialize an RFC3339 string: `serde_json::from_str::<Time>(&format!("\"{}\"", rfc3339)).unwrap()` — this works because `Time` is `#[derive(Deserialize)]`.
- **kube-rs reference layout (kube-rs/controller-rs):** single-crate, with `src/lib.rs` (Error enum + re-exports), `src/main.rs` (actix-web + tokio), `src/controller.rs` (CustomResource + reconciler + Context + finalizer + events), `src/crdgen.rs`, `src/metrics.rs`, `src/telemetry.rs`, `src/fixtures.rs`. Root-level: `Dockerfile`, `Tiltfile`, `justfile`, `charts/`, `yaml/`. **banlieue is multi-crate by design** (controller + provider SDK + 3 providers) so we adopt their *per-crate conventions*, not their single-crate layout.
- **Pinned dep versions (2026-05-25, matching controller-rs):** `kube = "3"` (features `derive`, `client`, `runtime`), `k8s-openapi = "0.27"` (features `latest`, `schemars`), `schemars = "1"`, `thiserror = "2"`, `tokio = "1"`, `tracing-subscriber = "0.3"`, `futures = "0.3"`, `anyhow = "1"`. Edition `2024`, MSRV `1.85`.

## Do-Not-Repeat

<!-- Mistakes made and corrected. Each entry prevents the same mistake recurring. -->
<!-- Format: [YYYY-MM-DD] Description of what went wrong and what to do instead. -->

## Decision Log

<!-- Significant technical decisions with rationale. Why X was chosen over Y. -->

- **[2026-05-22] CRDs are code-first via `banlieue-api`.** No hand-written CRD YAML. The `crdgen` binary is the only producer of `deploy/crds/`. Rationale: prevents drift between Rust types and deployed schemas (which silently breaks server-side apply).
- **[2026-05-22] Provider communication is CRD-only.** Explicitly rejected gRPC/HTTP between main controller and providers — the K8s API is the bus. Rationale: makes providers reusable as CAPI infra providers, removes a whole class of auth/networking/version-skew problems. See `~/dev/roadmaps/banlieue/00-OVERVIEW.md` (out-of-repo).
- **[2026-05-25] Multi-crate workspace, not single-crate.** Explicitly considered flattening to a single crate like `kube-rs/controller-rs`. Rejected because the roadmap requires 4+ provider crates that ship as independent images. Adopt controller-rs *per-crate conventions* (Error type, telemetry, metrics, fixtures.rs) when each crate lands; don't preload them now.
- **[2026-05-25] Edition 2024, MSRV 1.85.** Bumped from `2021` / `1.80` to match controller-rs. Rationale: gives access to async closures, new capture rules, and stays aligned with the kube-rs reference. Toolchain check is the only cost.
- **[2026-05-25] Roadmap lives outside the repo.** Moved `docs/roadmap/` to `~/dev/roadmaps/banlieue/` and stripped references from in-repo files (`CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/SKILL.md`, `.github/workflows/build.yaml`, `.wolf/cerebrum.md`). Rationale: OSS projects shouldn't ship the maintainer's planning artifacts. ADRs (`docs/adr/`) and design docs (`docs/design/`) **do** stay in-repo — those are public-facing technical records. Roadmap docs follow the same numeric-prefix convention out of tree.
