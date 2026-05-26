@.claude/SKILL.md

# Project Instructions for Claude Code

> banlieue — a Kubernetes-native abstract virtualization API.
> Users create `VirtualMachine` CRs; banlieue's controllers schedule them onto a backend (vSphere, Proxmox, libvirt) via provider-specific infrastructure CRDs that satisfy the **CAPI v1beta2 InfraMachine contract**.
> Communication between main controller and providers is **CRD-only** — no gRPC, no REST.

**Source of truth:** `crates/banlieue-api` is the authoritative type system. CRD YAMLs in `deploy/crds/` (once generated) are produced from it via the `crdgen` binary.

**CRITICAL Coding Patterns** (full details in `rules/`):
- **TDD**: Write tests FIRST — `rules/testing.md` + `tdd-workflow` skill
- **After ANY Rust change**: run `cargo-quality` skill (NON-NEGOTIABLE)
- **Early returns / magic numbers / style**: `rules/rust-style.md`
- **Event-Driven controllers**: use watch API, never polling (kube-runtime `Controller::new(...)`)

---

## 🚨 CRITICAL: CRD Schema is Code-First

`crates/banlieue-api/src/banlieue/` and `crates/banlieue-api/src/infrastructure/` are the source of truth for all CRD shapes. **Never hand-edit generated CRD YAMLs.**

Regenerate after any type change with the `regen-crds` skill:

```sh
cargo run -p banlieue-api --bin crdgen --features crdgen
```

Schema mismatches between Rust types and deployed CRDs cause silent failures — patches succeed (HTTP 200) but fields don't persist.

**When to check:** reconciliation loops, "field not appearing in kubectl output", after edits to any file under `crates/banlieue-api/src/`, when status patches don't persist.

---

## 🚨 The Non-Negotiables

These are locked. If a tradeoff seems to argue against one, find a different tradeoff — don't relax the principle. (Rationale is in the maintainer's private roadmap, kept outside this repo.)

1. **No RPC between main controller and providers.** CRDs + K8s API only. If you want HTTP/gRPC, you're solving the wrong problem.
2. **Provider infra CRDs satisfy CAPI v1beta2 InfraMachine contract.** This is what makes them reusable as CAPI infra providers and gives a battle-tested status model.
3. **`VirtualMachine` is independent of CAPI.** It is NOT `clusterv1.Machine`. It can coexist with CAPI but does not depend on it.
4. **Explicit over implicit.** Capabilities, image sources, credentials — declared. Auto-discovery is a status-time concern, not spec-time.
5. **Idempotent reconciliation.** Patch status, never replace. Use server-side apply for owned objects.
6. **Status mirrors infra.** A `VirtualMachine` is `provisioned=true` only when its infra ref says so.

---

## 🚨 CRITICAL: Always Review Official Documentation

When unsure of a decision, ALWAYS read official docs (kube-rs, k8s API, CAPI contract, provider SDKs) before implementing. Never take shortcuts based on assumptions. Research first, implement second.

---

## 🔍 MANDATORY: Use ripgrep

ALWAYS use `rg` for code search. NEVER use `grep`, `find`, or `lsof`.

- Rust files: `rg -trs "pattern" . -g '!target/'`

---

## 🚫 Cluster Operations Restrictions

**Allowed kubectl:** `get`, `describe`, `logs`, `annotate` (read-only + annotations only)

**FORBIDDEN:** `kubectl apply`, `kubectl patch`, `kubectl rollout restart`, `kubectl delete pods` (unless explicitly requested).

**NEVER build or push container images.** The user manages all image operations.

After code changes: run `cargo fmt`, `cargo clippy`, `cargo test`, then inform the user changes are ready to build and deploy.

---

## 🚨 Plans and Roadmaps live outside the repo

This is an OSS project; planning documents are **not** checked in. The maintainer keeps the roadmap in a private directory (`~/dev/roadmaps/banlieue/` on this machine). Do not create roadmap files inside the repository, and do not commit any `docs/roadmap/` directory.

If a planning question requires reading the roadmap, read from `~/dev/roadmaps/banlieue/` directly. New roadmap entries follow the existing numeric-prefix style there (`NN-TITLE.md`, `NN-PHASE-N{LETTER}-NAME.md`, UPPERCASE-with-hyphens).

ADRs are different — those *do* belong in the repo, at `docs/adr/NNNN-title.md` (lowercase-hyphen).

---

## 🔧 GitHub Workflows & CI/CD

See `rules/github-workflows.md` for full standards. Key rules:

- All workflows MUST delegate logic to Makefile targets (no inline bash scripts)
- New workflows MUST support `workflow_call` for reusability
- Prefer composite actions / reusable workflows over duplicated YAML

---

## 📝 Documentation Requirements

See `rules/documentation.md` for full workflow.

- Ask "Does documentation need to be updated?" before marking ANY task complete
- Update `.claude/CHANGELOG.md` with `**Author:**` on EVERY code change (MANDATORY — no exceptions)
- For ADRs: create `/docs/adr/NNNN-title.md` with Status / Context / Decision / Consequences

---

## 🦀 Rust Workflow

Full style guide: `rules/rust-style.md`. Full testing standards: `rules/testing.md`.

This is a **Cargo workspace** (`crates/banlieue-api`, future `banlieue-controller`, `banlieue-provider-sdk`, `banlieue-provider-{vsphere,proxmox,libvirt}`). Use `-p <crate>` to scope commands. The workspace pins shared deps in the root `Cargo.toml` `[workspace.dependencies]` table — prefer `<dep>.workspace = true` in member crates over re-pinning versions.

**After ANY `.rs` change:** run `cargo-quality` skill (`cargo fmt` + `cargo clippy` + `cargo test`). Task is NOT complete until all three pass.

### TDD (mandatory)

Write failing tests FIRST, then implement minimum code to pass. See `tdd-workflow` skill.

Test file pattern: `src/foo.rs` → `#[cfg(test)] mod foo_tests;` at bottom → `src/foo_tests.rs` (separate file, not embedded `#[cfg(test)] mod tests`).

### Dependency Management

Before adding deps: verify actively maintained (commits in last 6 months), prefer well-known crates, pin in `[workspace.dependencies]` if used by more than one crate, document reason in CHANGELOG.

---

## ☸️ Kubernetes Operator Patterns

### CRD Development — Rust as Source of Truth

`crates/banlieue-api/src/banlieue/` and `crates/banlieue-api/src/infrastructure/` are the source of truth. Generated YAML lives in `deploy/crds/` and is never edited directly.

> CRD changes: `regen-crds` skill → update `examples/` → `validate-examples` skill → `regen-api-docs` skill (LAST).

Adding a new CRD: follow `add-new-crd` skill.

### CRD Documentation Examples

ALWAYS read `deploy/crds/*.yaml` or the Rust source under `crates/banlieue-api/src/` before writing any YAML examples. Never guess field names.

### Controllers: Event-Driven (Watch, Not Poll)

Use `Controller::new()` from kube-runtime. Never poll in a loop.

```rust
// ✅ CORRECT
Controller::new(api, Config::default())
    .run(reconcile, error_policy, context)
    .for_each(|_| futures::future::ready(()))
    .await;
```

**Best practices:** set `ownerReferences`, use finalizers, exponential backoff, log reconciliation start/end, server-side apply for owned objects.

### Status Conditions

```rust
Condition {
    type_: "Ready".to_string(),
    status: "True".to_string(),
    reason: "ReconcileSucceeded".to_string(),
    message: "VirtualMachine provisioned".to_string(),
    last_transition_time: Some(Time(Utc::now())),
    observed_generation: Some(vm.metadata.generation.unwrap_or(0)),
}
```

`VirtualMachine` status MUST be derived from the infrastructure ref's status — never set `provisioned=true` independently.

---

## 🧪 Testing

See `rules/testing.md` for full standards.

- Every public function MUST have unit tests
- Tests in separate `_tests.rs` files (never embedded in source)
- Integration / e2e tests in `e2e/` (Phase 4)
- Run: `cargo-quality` skill. Specific module: `cargo test -p banlieue-api --lib <module>`. Verbose: `cargo test -- --nocapture`

---

## 📁 Target File Organization

```
banlieue/
├── Cargo.toml                       # workspace
├── crates/
│   ├── banlieue-api/                # ✅ Phase 0
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── common.rs
│   │       ├── banlieue/            # banlieue.io/v1alpha1
│   │       └── infrastructure/      # infrastructure.banlieue.io/v1alpha1
│   ├── banlieue-controller/         # Phase 1A
│   ├── banlieue-provider-sdk/       # Phase 1A
│   └── banlieue-provider-{vsphere,proxmox,libvirt}/  # 1B / 1C / 1D
├── deploy/
│   ├── crds/                        # generated via crdgen
│   ├── kustomize/
│   └── helm/                        # Phase 4
├── docs/
│   ├── adr/                         # ADRs (lowercase-hyphen, NNNN-title.md)
│   ├── design/                      # contract docs
│   └── user/                        # Phase 4
├── examples/
└── e2e/                             # Phase 4
```

Phase plans / roadmap docs live **outside the repo** (`~/dev/roadmaps/banlieue/`).

---

## 🚫 Things to Avoid

- `unwrap()` in production — use `?` or explicit error handling
- Hardcoded namespaces — make them configurable
- `sleep()` for synchronization — use k8s watch/informers
- Ignoring errors in finalizers — blocks resource deletion
- State outside Kubernetes — controllers must be stateless
- Inventing CRD fields on the fly — edit `banlieue-api` first, then regenerate

---

## 💡 Helpful Commands

```bash
# Workspace-wide checks
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all

# Generate CRDs from Rust types
cargo run -p banlieue-api --bin crdgen --features crdgen > /tmp/banlieue-crds.yaml

# Validate examples and generated CRDs
kubectl apply --dry-run=client -f /tmp/banlieue-crds.yaml
kubectl apply --dry-run=client -f examples/
```

Skills: `regen-crds`, `regen-api-docs`, `validate-examples`, `cargo-quality`, `verify-crd-sync`, `tdd-workflow`, `pre-commit-checklist`, `update-changelog`, `update-docs`, `add-new-crd`.

---

## 📋 PR/Commit Checklist

**Run `pre-commit-checklist` skill before EVERY commit. A task is NOT complete until it passes.**

Documentation is NOT optional — it is a critical requirement equal in importance to the code.

---

## 🔗 Project References

- [kube-rs documentation](https://kube.rs/)
- [Kubernetes API conventions](https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md)
- [Cluster API v1beta2 contracts](https://cluster-api.sigs.k8s.io/developer/providers/contracts/)
- [Operator pattern](https://kubernetes.io/docs/concepts/extend-kubernetes/operator/)
- Phase plans: kept outside the repo (`~/dev/roadmaps/banlieue/` on the maintainer's machine)
