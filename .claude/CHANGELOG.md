# Changelog

## [2026-05-26 19:30] - Phase 1A iteration 4: leader election + CLI/log close-out

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-sdk/src/leader.rs` — lease-based leader election against `coordination.k8s.io/v1.Lease`. Pure decision function `decide_action(now, lease, cfg) -> LeaseAction` (AcquireNew | Renew | Wait | TakeOver) separated from the async I/O so the logic is unit-testable without a cluster. `LeaderConfig` carries namespace / lease name / identity / lease_duration / renew_period / retry_period with `validate()` rejecting zero durations, `renew >= lease`, and empty identity. `LeaderConfig::default_identity()` reads `POD_NAME` then `HOSTNAME` then falls back to `"unknown"`. Defaults match `kube-controller-manager`: 15s lease, 5s renew, 2s retry. Field manager `banlieue.io/leader-election`.
- `crates/banlieue-provider-sdk/src/leader_tests.rs` — 13 unit tests for `decide_action` and `LeaderConfig::validate`: no-lease → AcquireNew, no-holder → AcquireNew, held-by-us → Renew (even when our own renew is stale), held-by-other within duration → Wait, held-by-other at the renew_time+duration boundary → Wait, held-by-other past duration → TakeOver, held-by-other with no renew_time → TakeOver, no-spec → AcquireNew, plus the four config-validation cases.
- `crates/banlieue-controller/src/main.rs` — new CLI flags: `--kubeconfig` (env `KUBECONFIG`), `--log-level` (env `BANLIEUE_LOG_LEVEL`), `--no-leader-elect` (env `BANLIEUE_NO_LEADER_ELECT`), `--leader-election-namespace` (default `banlieue-system`), `--leader-election-id` (default `banlieue-controller`), `--leader-election-identity` (defaults to `POD_NAME` / `HOSTNAME`). New helpers `build_leader_config(&Cli)` and `shutdown_signal()` (SIGTERM + Ctrl-C tokio::select). `init_tracing` now honours `--log-level` as an override for `RUST_LOG`.

### Changed
- `crates/banlieue-controller/src/main.rs` — startup sequence now: parse CLI → init tracing → build client → spawn health server → (unless `--no-leader-elect`) `acquire_or_wait` for the Lease, then spawn `renew_forever` in a background task whose terminal failure calls `std::process::exit(1)` (Deployment restarts the pod). The controller stream now races against `shutdown_signal()` via `tokio::select!` so SIGTERM yields a clean exit instead of being orphaned.
- `crates/banlieue-provider-sdk/src/lib.rs` — `pub mod leader;` registered; module list in the crate-level doc updated.
- `deploy/controller/rbac/clusterrole.yaml` — comment on the `coordination.k8s.io/leases` rule updated to describe banlieue's actual usage (GET + CREATE + SSA PATCH); verbs unchanged (already adequate).

### Why
The roadmap's Phase 1A `Definition of done` was met by iteration 3 *except* for leader election and the few remaining CLI flags called out in `~/dev/roadmaps/banlieue/10-PHASE-1A-CONTROLLER-AND-SDK.md`. This iteration closes those out so multi-replica Deployments (or rolling restarts) can run without two controller pods racing to reconcile the same VirtualMachine and SSA-fighting each other's status patches. After this iteration, Phase 1A is fully done; Phases 1B / 1C / 1D / 1E are now unblocked per the dependency graph in `~/dev/roadmaps/banlieue/README.md`.

The decision logic is deliberately pure so it can be exhaustively tested without a kube cluster — the async loop is then a thin wrapper that the controller's smoke test exercises end-to-end (running it locally creates a Lease in `banlieue-system` named `banlieue-controller` and refreshes it on a 5s cadence).

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only
- [x] **New capability** — multi-replica controller HA enabled by default; opt out with `--no-leader-elect` for single-instance local dev.

Verified by `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings` (clean), `cargo test --all` (27 SDK tests including 13 new leader tests, 43 controller tests, 144 api tests; all pass).

## [2026-05-26 18:30] - CALM architecture index + deeper CAPI relationship doc + safer calm-* targets

**Author:** Erick Bourgeois

### Added
- `docs/src/architecture/index.md` — section landing page for the CALM-rendered docs. Explains why banlieue uses FINOS CALM, summarises what's in the model (16 nodes / 13 relationships / 3 flows / 4 controls), tabulates the controls with NIST references and evidence-file links, and documents the `make calm-validate` / `calm-diagrams` / `calm-docify` workflow.
- `docs/src/reasoning/capi-relationship.md` — deeper "Why" page on the CAPI relationship. Contrasts banlieue and CAPI head-to-head, tabulates the exact v1beta2 `InfraMachine` fields banlieue mirrors, enumerates what banlieue deliberately *does not* take from CAPI (`Cluster`, `Machine*`, bootstrap providers, control-plane providers, `clusterctl`), and explains the v1beta2 pin. Complements (does not replace) the existing `concepts/infra-crds-capi.md`.
- `Makefile` target `calm-docify` — invokes `calm docify` against the existing template directory and writes into `docs/src/architecture/`. Functionally equivalent to `calm-diagrams` today; documented as the forward-looking entry point for richer multi-page bundles.

### Changed
- `Makefile` (`calm-diagrams` and `calm-docify`) — replaced `--clear-output-directory` with an explicit `rm -f` of the two generated files plus any `.hbs` leftovers. The blanket clear would have deleted the new hand-maintained `architecture/index.md` on every re-render.
- `docs/mkdocs.yml` nav — promoted the CALM diagrams from "Concepts" into their own top-level section **Architecture (CALM)** with `index.md` as the landing page. Added `Relationship to Cluster API` under **Why banlieue?** between `CRD-Only Contract` and `Comparisons`.

### Why
The CALM rendering targets already existed (system.md / flows.md) and were in sync with `architecture.json`, but the section had no landing page — readers arriving at a Mermaid blob got no context. Likewise, `concepts/infra-crds-capi.md` answered *what* the CAPI contract is but not *why* banlieue chose contract-compatibility over full CAPI adoption, which is the question that recurs in conversations with reviewers.

The Makefile fix is load-bearing: without it the new section index would silently disappear the next time anyone ran `make docs` or `make calm-diagrams`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified by `make calm-validate` (0 issues), `make calm-diagrams` (rendered, index.md survived), and `cd docs && poetry run mkdocs build` (built in 1.87s, two expected first-render git-history warnings).

## [2026-05-26 17:00] - Phase 1A iteration 3: migration sub-loop + cascade-wait finalizer + image watcher + Provider threading

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-controller/src/reconciler/migration.rs` — pure function `migration::evaluate(vm, decision) -> MigrationAction`. Detects placement drift between the freshly-computed `Decision` and the previously-recorded `ScheduledPlacement`; decides among `InPlace` / `StickToOld` / `SurfaceOnly { reason }` / `Recreate { reason }` per `VirtualMachine.spec.migrationPolicy` (`Never` → stick; `Manual` → surface unless `banlieue.io/migrate=true` annotation is set; `Automatic` → recreate). Drift kinds: `ProviderChanged`, `FailureDomainChanged`, `StorageMappingChanged`, `NetworkMappingChanged` — each maps to a stable condition `reason` string for `PlacementValid=False`.
- `crates/banlieue-controller/src/reconciler/migration_tests.rs` — 12 unit tests covering the full matrix (drift kind × policy × annotation state) plus the stable-reason-string guarantee. Includes the explicit "provider-change wins when BOTH change" tiebreaker.

### Changed
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs` — reconcile loop now:
  - Calls `migration::evaluate` after the scheduler; branches on `MigrationAction`:
    - `InPlace` → existing apply-then-mirror flow.
    - `StickToOld` → `mirror_only_path` (read the existing infra CR, mirror status, **don't** apply a new placement; `PlacementValid` is left at its previous value because `Never` says drift is acceptable).
    - `SurfaceOnly { reason }` → `patch_placement_invalid` writes `PlacementValid=False reason=<reason>` + `Ready=False reason=PlacementInvalid`; infra CR untouched.
    - `Recreate { reason }` → `delete_existing_infra` (idempotent, 404-tolerant); `patch_placement_invalid`; the *next* reconcile pass creates a fresh `VSphereMachine`.
  - `finalize_vm` now does proper cascade-wait: looks up the owned `VSphereMachine`; if it exists, issues delete and requeues; only when it's fully GC'd does the parent's `banlieue.io/virtualmachine` finalizer get dropped. Guarantees no backend leak on `kubectl delete vm`.
  - `build_vsphere_machine` is now called with the chosen `&Provider`. (The vSphere builder doesn't read it yet — the `Decision` already carries the resolved backend IDs — but the signature establishes the contract for Phase 1C/1D where Proxmox needs `Provider.spec.connection.endpoint` to target a cluster and libvirt needs SSH transport settings.)
- `crates/banlieue-controller/src/reconciler/infra.rs` — `build_vsphere_machine` signature takes `&Provider` (currently `_provider`). Docstring explains why the parameter exists even though vSphere doesn't consume it yet.
- `crates/banlieue-controller/src/reconciler/infra_tests.rs` — every call-site updated; new `parent_provider()` test helper constructs a `Provider` with a default `ProviderConnection`.
- `crates/banlieue-controller/src/reconciler/mod.rs` — `pub mod migration;` registered.
- `crates/banlieue-controller/src/main.rs` — Controller setup now uses:
  - `Controller::owns(VSphereMachine, ...)` — owner-reference-driven event flow so status mirror reacts immediately when a provider patches infra status, instead of waiting for the 30s requeue. Closes the missed Phase 1A "Gotcha" #1 (`Watch infra CRs with a Controller::owns relationship`).
  - `Controller::watches(VMImage, ...)` with a closure-captured `Store<VirtualMachine>` — image watcher: when `VMImage.status.perProvider[].ready` flips, every VM with `spec.image_ref.name == image.name` is re-queued. The scan is linear over the store; VMImage updates are operator-driven and rare, so this is fine for v1.

### Why
Iteration-2 changelog explicitly listed four items deferred to iteration 3. All four land here, plus the `Controller::owns` wiring that was a Phase 1A "Gotcha" the iteration-2 work missed. After this iteration, the Phase 1A "Definition of done" is fully met *except* for leader election + a few CLI flags (deferred to iteration 4 / Phase 1A close-out — they're operational niceties, not contract gaps).

The migration sub-loop is the load-bearing piece: it's the user-visible enforcement of the [least-touch principle](../docs/src/reasoning/least-touch.md). A consumer changes `providerRef.name` and (with `migrationPolicy=Automatic`) the system rebuilds the infra against the new backend without further input. The whole point of banlieue is encoded in the `MigrationAction::Recreate` arm of `evaluate`.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅ (clean)
- `cargo test --all` ✅ — **201 tests pass** (144 api + 43 controller + 14 sdk; +12 controller tests this iteration: 12 new migration cases, infra tests updated to thread Provider).
- `cargo build -p banlieue-controller` ✅ — main.rs compiles with the new `owns` + `watches` wiring.

### Phase 1A status after this iteration
- ✅ Resolve refs + scheduler + status mirror + infra builder (iter 2).
- ✅ Migration sub-loop, recreate-only path (this iter).
- ✅ Cascade-wait finalizer (this iter).
- ✅ Provider threading for future providers (this iter).
- ✅ Image watcher / event-driven re-queue on `VMImage` flips (this iter).
- ✅ `Controller::owns(VSphereMachine)` for fast status feedback (this iter; was a missed Gotcha).
- ⏳ Leader election (`Lease`-based) — SDK module + main.rs flags. Deferred to iteration 4 or Phase 1A close-out.
- ⏳ CLI flags `--leader-election-namespace` / `--leader-election-id`. Tied to leader election above.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout (controller behaviour materially changes; existing kind-deployed controllers should be redeployed)
- [ ] Config change only
- [ ] Documentation only

### Deferred to Phase 1B
- Without a real vSphere provider populating `Provider.status.failureDomains` and `VMImage.status.perProvider`, the smoke-test boundary remains `Scheduled=False reason=ImageNotReady`. The migration / cascade-wait / image-watcher paths are exercised by unit tests on synthetic inputs; end-to-end exercise lands when the provider does.

---

## [2026-05-26] - Address GHAS findings on PR #2 (Semgrep crdgen + CodeQL docs.yaml)

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-api/src/bin/crdgen.rs`: switched manual `std::env::args()` parsing to `clap::Parser`. Eliminates the Semgrep `rust.lang.security.args.args` finding by removing the direct `args()` call entirely, and adds free `--help` / `--version`. The CLI surface (`--out-dir <DIR>`) is unchanged.
- `crates/banlieue-api/Cargo.toml`: added `clap = { workspace = true, optional = true }` and extended the `crdgen` feature to `["dep:serde_yaml", "dep:clap"]`. clap is feature-gated so the library API surface is unchanged when `crdgen` is off.
- `.github/workflows/docs.yaml`: hard-gated the `build` job against `workflow_run` events that originated from a fork. Two layers of defence:
  1. Job-level `if:` — the build job runs only when the trigger is not `workflow_run`, OR when the `workflow_run.head_repository.full_name` equals the current repository.
  2. A new fail-fast "Verify trusted workflow_run source" step that runs **first** on `workflow_run` events and `exit 1`s before any checkout / install / cache step can execute.

### Why
GHAS surfaced 8 findings on PR #2 (https://github.com/firestoned/banlieue/pull/2):

- **Semgrep `rust.lang.security.args.args`** on `crdgen.rs:25` — the rule fires on any direct use of `std::env::args()`. Our code did `.skip(1)` to drop the program name (the actual security concern in the rule's docs), so this was a false-positive-shaped finding. Switching to clap silences it deterministically rather than via suppression comments.
- **CodeQL "Checkout of untrusted code in a privileged context"** ×5 + **"Cache Poisoning via caching of untrusted files"** ×2 on `docs.yaml` — these are *real*. `workflow_run` always executes with default-branch permissions, even when the upstream "Build" workflow was triggered by a fork's PR. Without a guard, the build job would check out the fork's SHA into a privileged context and run `poetry install` / `cargo build` / `npm install` on potentially malicious files, plus write to the default-branch GHA cache (cache poisoning). The job-level `if:` + fail-fast step refuse to run on fork-originated workflow_run events.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 189 tests pass (144 api + 31 controller + 14 sdk; unchanged from iteration 2).
- `cargo run -p banlieue-api --bin crdgen --features crdgen -- --help` ✅ — emits the expected usage block.
- `cargo run -p banlieue-api --bin crdgen --features crdgen -- --out-dir deploy/crds` ✅ — still writes all 6 CRDs.
- `python3 -c "yaml.safe_load_all(open('.github/workflows/docs.yaml'))"` ✅ — YAML parses.
- Inspected the rendered workflow: `build.if` carries the fork-blocking expression; the first step (`Verify trusted workflow_run source`) is gated on `workflow_run` events and exits non-zero on a fork mismatch before the checkout step runs.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (Semgrep fix is internal tooling; CodeQL fix only changes CI workflow behaviour for fork-originated chained workflow_run events, which had no legitimate need to ever run)

### Remaining follow-up
- The 8 alerts on PR #2 will auto-close on the next CodeQL/Semgrep scan once this branch is rebased / re-pushed. Confirm via `gh pr view 2` after the next CI run that no GHAS comments remain.
- If CodeQL still flags the workflow after the next scan (static analysis sometimes can't see job-level `if:` guards), the proper next step is to split the workflow into a `docs-build.yaml` (push/PR triggered; no privileged context) and a `docs-deploy.yaml` (workflow_run; downloads the already-built artifact, never checks out user code). That refactor is deferred until we see whether the guard suffices.

---

## [2026-05-26] - Phase 1A iteration 2: scheduler + status mirror + infra builder

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-controller/src/reconciler/scheduler.rs` — pure function `schedule(vm, class, image, providers, existing_vms) → Result<Decision, ScheduleError>`. No I/O, no async. Filter chain: providerSelector → failureDomainSelector → image readiness → storage classes → network classes → features → firmware (`efi-secure` requires `efiSecureBoot` feature) → required anti-affinity. Tie-break: alphabetical by `(provider_name, fd_name)`. `Decision` is owned (no lifetimes); `.to_scheduled_placement(now)` projects it onto `VirtualMachineStatus.scheduled`. `ScheduleError` exposes stable `reason()` strings (`reasons::NO_PROVIDER`, `IMAGE_NOT_READY`, ...) for deterministic condition writes.
- `crates/banlieue-controller/src/reconciler/scheduler_tests.rs` — 16 table-driven tests: happy path, every filter step (including required anti-affinity collision and tiebreak), backend-id BTreeMap-first-value rule, `to_scheduled_placement` round-trip.
- `crates/banlieue-controller/src/reconciler/status_mirror.rs` — `InfraMachineRead` trait + impl for `VSphereMachine` + pure `mirror_status_from_infra(current, infra, generation) → VirtualMachineStatus`. Mirrors `initialization` and `addresses`, projects the infra `Ready` condition onto the parent's `InfrastructureReady`, and computes aggregate `Ready = Scheduled && PlacementValid && InfrastructureReady` (with `Pending` reason when the infra hasn't reported yet).
- `crates/banlieue-controller/src/reconciler/status_mirror_tests.rs` — 7 tests across every Ready combination + missing-status fallback.
- `crates/banlieue-controller/src/reconciler/infra.rs` — `build_vsphere_machine(vm, class, image, decision) → Result<VSphereMachine, InfraBuildError>`. Resolves datacenter/cluster from `failure_domain_raw`, datastore from the first resolved storage backend_id, template from `VMImage.status.perProvider[i].resolved_ref`. Sets controller-owning `OwnerReference` back to the parent VM. Propagates the VM's `app=*` labels and adds `banlieue.io/owned-by=<vm-name>`.
- `crates/banlieue-controller/src/reconciler/infra_tests.rs` — 5 tests: happy path, owner-reference shape, missing fd-raw attributes (datacenter / cluster), missing image resolved_ref, label propagation.

### Changed
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs` — replaced the iteration-1 `SchedulerNotImplemented` stub with the real reconcile flow:
  1. Ensure finalizer (`banlieue.io/virtualmachine`).
  2. Resolve VMClass + VMImage (cluster-scoped via `Api::all`).
  3. List Providers + sibling VMs in the VM's namespace.
  4. Call `schedule`; on failure, surface `Scheduled=False` with the typed reason and requeue.
  5. Build the `VSphereMachine` via `infra::build_vsphere_machine`; SSA it (`field_manager=banlieue.io/controller`).
  6. Read it back; mirror its status onto the VM.
  7. Patch VM status (`scheduled`, `infrastructureRef`, conditions, `observedGeneration`).
- `crates/banlieue-controller/src/reconciler/mod.rs` — added `pub mod infra; pub mod scheduler; pub mod status_mirror;`.
- `crates/banlieue-controller/src/reconciler/virtualmachine_tests.rs` — replaced the iteration-1 stub tests with a stable assertion that the finalizer constant string never silently changes.

### Why
Iteration 1 shipped controller scaffolding + a stub reconciler that only wrote `Scheduled=False reason=SchedulerNotImplemented`. Iteration 2 makes the controller actually *do* the thing: it picks a `(provider, failure domain)` pair, projects the choice into a `VSphereMachine`, and mirrors the infra status back. Because the vSphere *provider* binary doesn't exist yet (Phase 1B), the system stops cleanly at `Scheduled=False reason=ImageNotReady` — the exact boundary between this iteration and the next.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — **189 tests pass** (144 api + 31 controller + 14 sdk; +29 controller tests new this iteration).
- **Smoke test on kind** (`kind-banlieue-dev` with examples pre-applied):
  - `./target/release/banlieue-controller` connects to the apiserver, watches `VirtualMachine` cluster-wide, reconciles `banlieue-system/db-prod-01`.
  - Resolves `VMClass` (`db-prod-large`) and `VMImage` (`ubuntu-22.04-cloudinit`); lists 2 Providers.
  - Runs the scheduler; hits `ImageNotReady` because no provider has populated `VMImage.status.perProvider`.
  - Writes `Scheduled=False reason=ImageNotReady` + `Ready=False reason=Scheduling` to the VM. Confirmed via `kubectl get virtualmachine db-prod-01 -o jsonpath='{.status.conditions[*].reason}' → "Scheduling ImageNotReady"`.
  - Requeues continuously (default 30 s), no `VSphereMachine` created (correct — scheduling failed pre-build).

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout (manifests unchanged but the controller behaviour materially changes; if you have an old controller running, redeploy)
- [ ] Config change only
- [ ] Documentation only

### Deferred to iteration 3
- **Migration sub-loop** — when scheduler returns a different `(provider, fd)` than `status.scheduled`, set `PlacementValid=False`; act per `migrationPolicy` (`Automatic` → recreate, `Manual` → wait for the `banlieue.io/migrate=true` annotation, `Never` → leave alone).
- **Image watcher** — side reconciler that re-queues affected VMs when `VMImage.status.perProvider[].ready` flips.
- **Deletion-finalizer cascade waits** — block finalizer drop until the owned `VSphereMachine` has been fully GC'd.
- **Provider Spec usage at infra-build time** — the chosen Provider is looked up in the reconciler (`_chosen_provider`) but isn't passed to the builder yet; providers that need spec-level fields (libvirt SSH config etc.) will use it.

### Deferred to Phase 1B
- `crates/banlieue-provider-vsphere/` — without it, no provider populates `Provider.status.failureDomains` or `VMImage.status.perProvider`, so end-to-end provisioning stops at `ImageNotReady`. This is by design: the scheduler is now correct on synthetic inputs, and 1B fills in the real data.

---

## [2026-05-26 16:00] - Add Documentation GitHub Actions workflow + nav: Getting Started under Home

**Author:** Erick Bourgeois

### Added
- `.github/workflows/docs.yaml`: mirrors `~/dev/5-spot/.github/workflows/docs.yaml`. Two reusable-workflow calls into `.github/workflows/calm.yaml` (`validate` + `template`) run before the build job, which downloads the rendered CALM diagrams as an artifact and runs `make docs` with `SKIP_CALM_DIAGRAMS=1` (the diagrams already came from the previous job). PRs additionally get a linkinator broken-link check (`continue-on-error: true`). Deploy to GitHub Pages is gated through `workflow_run` against the existing **Build** workflow — docs only publish when Build succeeded for a `release` event, so a broken release never publishes docs for that tag. All third-party actions pinned by SHA.

### Changed
- `docs/mkdocs.yml`: **Getting Started** is now a sub-page of **Home** (using MkDocs Material's `navigation.indexes` so `index.md` is the section landing page and `Getting Started: getting-started/quickstart.md` sits beneath it in the left sidebar). The standalone top-level **Getting Started** section is removed.

### Why
- The reusable `.github/workflows/calm.yaml` workflow has been in the repo for a while but had no orchestrator wiring it into the CI pipeline. `docs.yaml` is that orchestrator. It enforces the same shape as 5-spot: validate the CALM JSON first, render diagrams second, build the site third, deploy only on release. This pattern keeps the documentation pipeline reproducible and prevents drift between architecture-as-code and the rendered diagrams.
- The Home → Getting Started nesting matches the user's intent that the Quick Start be the first thing a new visitor lands on after the homepage, surfaced in the left sidebar rather than buried in a separate top-level section.

### Verification
- `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/docs.yaml'))"` ✅ parses; jobs `calm-validate`, `calm-diagrams`, `build`, `deploy` resolved; both reusable calls point at `./.github/workflows/calm.yaml` which exists in-tree.
- `cd docs && poetry run mkdocs build` ✅ rebuilds in 1.74s with the new nav; warnings are the unrelated `git-revision-date-localized` plugin chatter about pages without git history, which clears once the files are committed.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

---

## [2026-05-26 15:30] - Bootstrap FINOS CALM architecture-as-code

**Author:** Erick Bourgeois

### Added
- `docs/architecture/calm/architecture.json`: CALM 1.2 architecture document for banlieue. Models 16 nodes (2 actors, 1 ecosystem, 5 services — incl. the three planned provider controllers, 3 networks for vSphere/Proxmox/libvirt backends, 5 data assets for every CRD), 13 relationships (every wire is HTTPS to the K8s API; no controller-to-controller arrow by design), and 3 flows: **Create**, **Swap**, **Delete**. Each flow encodes a project tenet — Swap is the canonical least-touch demo. Controls reference NIST SP 800-53 Rev. 5 and SP 800-218 (SSDF) and the CAPI v1beta2 InfraMachine contract.
- `docs/architecture/calm/templates/mermaid/system.md.hbs` + `flows.md.hbs`: Handlebars templates rendering one Mermaid `flowchart LR` of every node/relationship, and one `flowchart TD` per flow. Mirrors the 5-spot template style.
- `docs/architecture/calm/README.md`: contributor doc — what the architecture models, how to validate, how to render, how to extend.
- `docs/src/architecture/system.md` + `flows.md`: placeholder stubs so `mkdocs build` works on a fresh clone before `make calm-diagrams` has been run. Both are wiped + regenerated by the CALM CLI on `make calm-diagrams` (the CLI's `--clear-output-directory` flag).
- `docs/src/concepts/architecture.md`: cross-link admonition pointing at the new CALM pages, naming them as the canonical source of truth.
- `docs/mkdocs.yml`: nav now includes **System Diagram (CALM)** and **Architecture Flows (CALM)** under Concepts.

### Changed
- Root `Makefile`: added `CALM_CLI_VERSION` (1.37.0), `CALM_ARCH`, `CALM_TEMPLATES`, `CALM_DIAGRAMS_OUT` variables; added `calm-validate` and `calm-diagrams` targets; `docs` now depends on `calm-diagrams` so the rendered pages are always in sync before MkDocs runs; `docs-clean` also removes the generated `architecture/system.md` and `flows.md`. Honours `SKIP_CALM_DIAGRAMS=1` for air-gapped / offline builds.

### Why
The repository already shipped the reusable `.github/workflows/calm.yaml` workflow (mirrored from 5-spot earlier in the project) but had no actual CALM architecture document for it to validate. This change provides the missing input. Modelling banlieue's architecture in CALM gives:

- A **machine-validated** source of truth (`calm validate` runs in CI).
- A **single rendering pipeline** for system + flow diagrams, replacing hand-drawn Mermaid that drifts from code.
- A way to **encode project tenets as controls** (CRD-only contract → AC-4/SC-7; least-touch principle → CM-3/CM-4; code-first CRDs → SSDF PW.4/PS.1) with evidence pointing at the relevant repo paths.

The Swap flow is deliberately included even though no provider exists yet (Phase 1B+): it's the *defining* user-visible behaviour banlieue is built around, and having it in CALM forces every future change to preserve it.

### Verification
- `python3 -c "import json; json.load(open('docs/architecture/calm/architecture.json'))"` ✅
- mkdocs `nav:` audited — every entry resolves to a real file under `docs/src/`.
- `make calm-validate` not run here (requires `npx`); CI's `calm.yaml` reusable workflow exercises this path.
- `make calm-diagrams` not run here for the same reason; the stub `system.md` / `flows.md` files keep `mkdocs build` working until it runs.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

---

## [2026-05-26] - Default namespace `banlieue-system` + fix `insecureSkipTLSVerify` field rename

**Author:** Erick Bourgeois

### Changed
- `examples/0{1,2,5}-*.yaml`: `namespace: ops` → `namespace: banlieue-system`. All user-facing examples now target the same namespace as the controller, so a fresh `make kind-deploy-crds` followed by `kubectl apply -f examples/` works without first having to create another namespace.
- `Makefile` — `kind-deploy-crds` now also applies `deploy/controller/namespace.yaml`, so the namespace exists for examples even before `kind-deploy-controller` runs.
- `crates/banlieue-api/src/banlieue/provider.rs`: added `#[serde(rename = "insecureSkipTLSVerify")]` on `ProviderConnection.insecure_skip_tls_verify`. The auto-derived camelCase produced `insecureSkipTlsVerify` (lowercase `s` between TL/Verify); the CAPI convention (and what the example YAML already used) is `insecureSkipTLSVerify` with uppercase `TLS`.
- `crates/banlieue-api/src/banlieue/provider_tests.rs`: updated the JSON-roundtrip assertion to expect `insecureSkipTLSVerify`.
- `deploy/crds/banlieue.io_providers.yaml`: regenerated.

### Why
`make kind-deploy-crds` then `kubectl apply -f examples/` left users with a "no namespace `ops`" surprise, and the vSphere Provider example was rejected with:
```
error when creating "examples/01-provider-vsphere-dc1.yaml": Provider in version "v1alpha1"
cannot be handled as a Provider: strict decoding error:
unknown field "spec.connection.insecureSkipTLSVerify"
```
Two separate issues, fixed together: the examples now target the same default namespace as the controller, and the Provider type accepts CAPI-style `insecureSkipTLSVerify` on the wire.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 160 tests passed (144 api + 2 controller + 14 sdk).
- `make crds` ✅ — regenerated.
- `make kind-deploy-crds && kubectl apply -f examples/` ✅ — all four example resources land successfully in `banlieue-system`:
  ```
  provider.banlieue.io/vcenter-dc1            created
  provider.banlieue.io/libvirt-edge-host-7    created
  vmclass.banlieue.io/db-prod-large           created
  vmimage.banlieue.io/ubuntu-22.04-cloudinit  created
  virtualmachine.banlieue.io/db-prod-01       created
  ```

### Impact
- [x] **Breaking change** (pre-v1alpha1): wire field renamed `insecureSkipTlsVerify` → `insecureSkipTLSVerify`. No production users yet; YAML written against the previous CRD must update.
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

---

## [2026-05-26 14:30] - Bootstrap MkDocs documentation site

**Author:** Erick Bourgeois

### Added
- `docs/mkdocs.yml`: MkDocs Material configuration mirroring the `~/dev/5-spot` setup (Material theme, dark mode, search, mermaid via `pymdownx.superfences` + `mermaid@11` CDN, git-revision-date-localized plugin, Roboto fonts, full pymdownx extension set).
- `docs/pyproject.toml` + `docs/.python-version` + `docs/.gitignore` + `docs/README.md`: Poetry-managed Python deps (`mkdocs>=1.6,<2`, `mkdocs-material^9.5`, plugins), Python 3.11 pin, build-artefact ignores, contributor README.
- `docs/src/index.md`: project landing page with one-line pitch, what/why, status, links.
- `docs/src/overview.md` (NEW, per follow-up request): "what banlieue does, fundamentally" page with a high-level mermaid diagram showing user → K8s API → banlieue-controller → infra CRD → provider controllers → real backends. Linked right under Home in the nav.
- `docs/src/reasoning/`: the comprehensive *why* of the project — `index.md` (entrypoint), `problem.md` (fragmented VM control plane), `abstraction-principle.md` (least-touch principle), `least-touch.md` (swap / mix / onboard scenarios), `crd-only-contract.md` (no RPC; K8s API is the bus), `comparisons.md` (Kubevirt / CAPI / Crossplane / Terraform / hypervisor SDKs), `non-goals.md`.
- `docs/src/concepts/`: `index.md`, `architecture.md` (components, reconcile flow, watches, SSA), `virtualmachine.md` (CRD shape, status, lifecycle), `providers.md` (Provider CR + provider controller anatomy + SDK pointers), `infra-crds-capi.md` (why we satisfy the CAPI v1beta2 InfraMachine contract).
- `docs/src/getting-started/quickstart.md`: stubbed Phase 0/1A quick start with explicit "not production-ready" admonition.
- `docs/src/reference/roadmap.md` + `docs/src/reference/license.md`: public-facing roadmap (Phase 0 → 1E) and Apache-2.0 summary.
- `docs/src/stylesheets/extra.css`: neutral slate/sky/amber palette (no RBC branding from the 5-spot source), mermaid zoom/pan, TOC, mobile + print styles.
- `docs/src/javascripts/mermaid-init.js`: mermaid initialiser + zoom/pan handlers, supports Material's instant-navigation re-render via `document$`.
- Root `Makefile`: `docs`, `docs-serve`, `docs-clean`, `docs-deploy` targets — Poetry-based, all logic in the Makefile per the project's "workflows are Makefile-driven" rule.
- Root `.gitignore`: ignore `docs/site/`, `docs/.venv/`, `docs/__pycache__/`.

### Why
The repository shipped with an empty `docs/` directory and a stub `README.md`. The maintainer asked for comprehensive initial documentation of the project's *reasoning* — specifically the belief in abstracted APIs with "least touch" on the user's workflow, allowing providers to be swapped and mixed. The doc site is the right home for that long-form material, and `~/dev/5-spot` already has a polished MkDocs setup that other projects in this stack mirror. Mimicking that setup keeps the toolchain consistent (Poetry + MkDocs Material + the same plugins + Mermaid pattern).

A follow-up request added an `overview.md` page sitting between the home page and the `Why banlieue?` section: a fundamentals-first explainer with a single high-level mermaid diagram showing the three actors (user, banlieue controller, provider controllers) and the K8s API as the bus.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

### Verification
- `docs/mkdocs.yml` is syntactically valid YAML; nav references every file in `docs/src/`.
- All internal links from `index.md`, `overview.md`, and the reasoning pages resolve to files that exist on disk.
- `make docs-serve` will install Poetry deps and start MkDocs locally (not run here; the maintainer can verify with `cd docs && poetry install && poetry run mkdocs serve`).

---

## [2026-05-26] - Fix bug-027: PowerState YAML 1.1 boolean trap rejects CRD

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-api/src/common.rs`: Renamed `PowerState::On`/`Off`/`Suspended` → `PowerState::PoweredOn`/`PoweredOff`/`Suspended`. Removed the `#[serde(rename_all = "PascalCase")]` since the variant names are already the desired wire form.
- `crates/banlieue-api/src/banlieue/virtualmachine.rs`: `default_power_on` now returns `PowerState::PoweredOn`; docstring updated.
- `crates/banlieue-api/src/common_tests.rs` + `crates/banlieue-api/src/banlieue/virtualmachine_tests.rs`: updated assertions to the new variant names. Added a regression test (`power_state_rejects_legacy_short_form`) asserting that `"On"`/`"Off"` no longer deserialize.
- `examples/05-virtualmachine.yaml`: `desiredPowerState: "On"` → `desiredPowerState: PoweredOn`.
- `deploy/crds/banlieue.io_virtualmachines.yaml`: regenerated via `make crds`.
- `.wolf/buglog.json`: logged as bug-027 (related to bug-006).
- `.wolf/cerebrum.md`: added Do-Not-Repeat entry for the YAML 1.1 implicit-boolean trap.

### Why
`make kind-deploy-crds` failed with:
```
The CustomResourceDefinition "virtualmachines.banlieue.io" is invalid:
  spec.validation.openAPIV3Schema.properties[spec].properties[desiredPowerState].default:
  Invalid value: "boolean":  in body must be of type string: "boolean"
```
The generated CRD had `default: On` and `enum: - On - Off` (bare, unquoted). The kube apiserver's Go YAML 1.1 parser reads bare `On`/`Off` (regardless of case) as booleans — the classic "Norway problem" variant. So a `string`-typed field had a `boolean`-typed default and the schema was rejected.

Renaming the variants to `PoweredOn`/`PoweredOff` (vSphere/CAPI convention) makes the generated tokens unambiguous strings.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 160 tests passed (144 api after adding the regression test + 2 controller + 14 sdk).
- `make crds` ✅ — regenerated `deploy/crds/`. The `desiredPowerState` block is now:
  ```yaml
  desiredPowerState:
    default: PoweredOn
    enum:
    - PoweredOn
    - PoweredOff
    - Suspended
    type: string
  ```
- `kubectl --context kind-banlieue-dev apply -f deploy/crds/` ✅ — all six CRDs accepted (previously the `VirtualMachine` CRD was rejected).

### Impact
- [x] **Breaking change** — wire format of `PowerState` changes from `On`/`Off` to `PoweredOn`/`PoweredOff`. No production users yet (pre-v1alpha1 scaffolding), but anyone who had a local example with the old form must update.
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

---

## [2026-05-26] - Phase 1A scaffold: controller, SDK, Makefile, deploy manifests, kind setup

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-sdk/` — new library crate. Modules:
  - `client.rs` — kube::Client builder with explicit read/write timeouts.
  - `error.rs` — typed `Error` enum re-exported as `banlieue_provider_sdk::Error`.
  - `finalizer.rs` — pure `finalizer_list_with` / `finalizer_list_without` plus `ensure_finalizer` / `remove_finalizer` that JSON Merge Patch the K8s object.
  - `ssa.rs` — `server_side_apply` helper + `FIELD_MANAGER_*` constants (controller, vsphere, proxmox, libvirt).
  - `status.rs` — Kubernetes-idiomatic `set_condition` (upsert, sort, transition-time semantics) + `is_condition_true` + `find_condition`.
  - `reconciler.rs` — `requeue_default` / `requeue_on_error` / `requeue_long` / `no_requeue` helpers around `kube::runtime::controller::Action`.
- `crates/banlieue-controller/` — new binary crate. Phase 1A MVP scope: watches `VirtualMachine` resources, ensures finalizer, writes `Scheduled=False reason=SchedulerNotImplemented` and `Ready=False` conditions so users see the controller is wired up. Scheduler / status mirror / migration sub-loop deferred to the next iteration.
  - `main.rs` — clap CLI with `BANLIEUE_*` env-var fallbacks, tracing init (text or json), tiny TCP health server on `:8081`, `Controller::new(...).run(reconcile, error_policy, ctx)` wiring.
  - `reconciler/virtualmachine.rs` — reconcile + error_policy + finalize path + status patch via SSA.
- `Cargo.toml` — added `banlieue-controller` and `banlieue-provider-sdk` to workspace members; pinned `clap = "4"`, `chrono = "0.4"`, `async-trait = "0.1"` in `[workspace.dependencies]`; added `json` feature to `tracing-subscriber`.
- `crates/banlieue-api/src/bin/crdgen.rs` — now accepts `--out-dir <DIR>` and emits one file per CRD (`<group>_<plural>.yaml`, kubebuilder convention) in addition to the existing stdout multi-doc mode.
- `Makefile` — 5-spot-shaped workflow targets. All workflow logic lives here (per project conventions); workflows just call `make`. Notable targets:
  - `make crds` — regenerate `deploy/crds/` from Rust types.
  - `make run-local` — generate CRDs then `cargo run -p banlieue-controller` against the current kube-context.
  - `make kind-up` — one-shot: create kind cluster + apply CRDs. After this you can run the controller locally with `make run-local`.
  - `make kind-load BINARY=<bin>` — cross-compile the binary, build a docker image (host-arch), `kind load docker-image` it.
  - `make kind-deploy-controller` — apply manifests + override the deployment image to the locally-built `KIND_IMAGE`.
  - Per-binary docker targets (`docker-build`, `docker-build-chainguard`, `docker-buildx`, `docker-buildx-chainguard`) parameterised by `BINARY=<name>`.
- `Dockerfile` + `Dockerfile.chainguard` — single per-base Dockerfile parameterised by `BINARY` build-arg, so the same Dockerfile builds every banlieue binary (controller + future providers). Distroless `gcr.io/distroless/cc-debian13:nonroot` and Chainguard `cgr.dev/chainguard/glibc-dynamic:latest` bases, both pinned by digest. Pre-built binaries are copied in from `binaries/<arch>/<binary>` — we never compile inside the container.
- `deploy/crds/` — generated. 6 files, one per CRD.
- `deploy/controller/{namespace,configmap,deployment,service}.yaml` + `deploy/controller/rbac/{serviceaccount,clusterrole,clusterrolebinding}.yaml` — controller deployment manifests. ClusterRole grants full access on `banlieue.io/*` and `infrastructure.banlieue.io/*` (incl. finalizers subresources), read on Secrets, write on Events, full on `ipam.cluster.x-k8s.io/ipaddressclaims+ipaddresses`, and Lease CRUD for leader election. Pod-Security `restricted` profile labels on the namespace.
- `deploy/kind/cluster.yaml` — kind cluster config (single-node, control-plane labels for ingress-ready).

### Why
The roadmap's Phase 1A goal — "a VirtualMachine can go from creation through status.scheduled and status.infrastructureRef populated" — needs a controller binary and an SDK first. This commit lands the **scaffolding** so subsequent iterations can focus on business logic (scheduler, infra creation, status mirror, migration) without re-arguing crate shape, Makefile patterns, RBAC, or Dockerfile conventions. The "ideal" dev loop from the user instructions — `make kind-up` then `cargo run -p banlieue-controller` against the kind cluster — works as of this commit.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 159 tests passed (143 api + 2 controller + 14 sdk).
- `cargo run -p banlieue-api --bin crdgen --features crdgen -- --out-dir deploy/crds` ✅ — 6 CRD files written.
- `python3 -c "yaml.safe_load_all(...)"` over every YAML in `deploy/crds/` and `deploy/controller/` ✅ — all parse.
- `make help` ✅ — renders the workflow target list with descriptions.

### Impact
- [x] Adds new crates (`banlieue-controller`, `banlieue-provider-sdk`); no API/CRD breaking changes.
- [ ] Breaking change
- [x] Requires cluster rollout (new Deployment manifests; users running an earlier dev build should re-apply `deploy/controller/`).
- [ ] Config change only
- [x] Documentation only — CHANGELOG only here; the next iteration will add `docs/user/` getting-started content and link the Makefile + kind dev loop from `README.md`.

### Deferred to follow-up iterations
- Phase 1A iteration 2: full scheduler (the pure function from the roadmap), provider-infra creation via SSA, status-mirror from `VSphereMachine` → `VirtualMachine`.
- Phase 1A iteration 3: migration sub-loop (recreate-only initially), image watcher, deletion-finalizer cascade waits.
- Phase 1B: `crates/banlieue-provider-vsphere/` with `vim_rs`, capability introspection, `GOVC_*` env-var pass-through for local-vSphere dev.

---

## [2026-05-25] - Move roadmap out of repo

**Author:** Erick Bourgeois

### Changed
- Moved `docs/roadmap/` → `~/dev/roadmaps/banlieue/` (out-of-repo). Reason: OSS projects should not ship the maintainer's planning artifacts. The numeric-prefix filename convention (`00-OVERVIEW.md`, `10-PHASE-1A-...`, etc.) is preserved at the new location.
- `.claude/CLAUDE.md`: Replaced the "Plans and Roadmaps → `docs/roadmap/`" rule with a "Plans and Roadmaps live outside the repo" rule. Updated the target file-organization tree to drop `docs/roadmap/` and add `docs/adr/` instead (ADRs stay in-repo because they're public technical records).
- `.claude/SKILL.md`: Stripped `docs/roadmap/` references from `regen-api-docs`, `update-docs`, `add-new-crd`, and the pre-commit checklist; clarified that phase plans live out-of-repo.
- `.github/workflows/build.yaml`: Removed the `# See docs/roadmap/10-PHASE-1A-...` comment pointer.
- `.wolf/cerebrum.md`: Updated the Phase-0 layout learning and the 2026-05-22 decision-log entry to point at the new location; added a new 2026-05-25 decision entry recording the move.

### What stays in-repo
- `docs/adr/` — Architecture Decision Records (lowercase-hyphen, `NNNN-title.md`).
- `docs/design/` — contract docs, diagrams.
- `docs/user/` — user-facing documentation (Phase 4).
- `examples/` — runnable YAML examples.

### Verification
- `cargo test --workspace --all-features` ✅ — 143 passed, 0 failed (no code changes; this just confirms nothing on the docs side broke compilation).
- `cargo run -p banlieue-api --bin crdgen --features crdgen` ✅ — still emits 6 CRDs.
- `grep -rln "docs/roadmap" --include="*.md" --include="*.toml" --include="*.yaml" --include="*.rs" .` returns only **intentional** mentions: the prohibition rule in `.claude/CLAUDE.md`, the decision-log entry in `.wolf/cerebrum.md`, and historical entries in `.wolf/buglog.json` and `.wolf/memory.md` (those are append-only audit logs and stay as-is).

---

## [2026-05-25] - Fix bug-006: IpamSpec CRD-generation

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-api/src/common.rs`: `IpamSpec` redesigned from a serde-tagged enum (`#[serde(tag = "source")]` with `Dhcp` / `Static` / `Pool` variants) into a flat struct: `IpamSpec { source: IpamSource, static: Option<StaticIpamConfig>, pool: Option<PoolIpamConfig> }`. New `IpamSource` is a plain enum that serializes as a lower-case string (`dhcp` / `static` / `pool`). Defaults to `Dhcp` with both sub-configs `None`.
- `crates/banlieue-api/src/common_tests.rs`: 4 new tests added (`ipam_source_default_is_dhcp`, `ipam_spec_default_is_dhcp_with_no_sub_configs`, `ipam_source_all_variants_round_trip`, `ipam_source_rejects_unknown_variant`). Existing IpamSpec tests rewritten for the flat shape.
- `crates/banlieue-api/src/banlieue/vmclass_tests.rs`: replaced `vmclass_crd_currently_panics_due_to_ipam_spec_bug` with `vmclass_crd_metadata_matches_kube_attributes` (positive assertion that the CRD generates and is cluster-scoped).
- `crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs`: replaced the two panic-pin tests with positive CRD-metadata assertions for both `VSphereMachine` and `VSphereMachineTemplate`.
- `examples/03-vmclass-db-prod-large.yaml`: rewrote the `pool` example to nest `poolRef` under `pool:` (matches new wire format).

### Why
On the upgraded toolchain (schemars 1 + kube 3), removing the variant-level doc comments from `IpamSpec` only changed the panic location: the new error makes clear that kube-derive's schema flattener *requires identical schemas for any property shared across oneOf subschemas*. By construction, every variant of a `#[serde(tag = "x")]` enum has a different value for `x`, so the panic is fundamental — not a metadata mismatch.

The Kubernetes-idiomatic shape (used by CAPI and others) is a flat struct whose discriminator is just a string field, with per-variant data nested under a sibling field of the matching name. That's what we adopted. Cross-field validation is intentionally left to the controller / future CEL rules.

This was the right time to break the wire format: there are no consumers yet (Phase 0), so the migration cost is zero. Once Phase 1A ships, breaking the wire format would require a CRD storage migration.

### Wire format change
**Before** (the tagged-enum shape, never actually deployable because CRD-gen panicked):
```yaml
ipam:
  source: pool
  poolRef:
    apiGroup: ipam.cluster.x-k8s.io
    kind: IPAddressClaim
    name: prod-pool
```

**After:**
```yaml
ipam:
  source: pool
  pool:
    poolRef:
      apiGroup: ipam.cluster.x-k8s.io
      kind: IPAddressClaim
      name: prod-pool
```

`static` follows the same nesting; `dhcp` needs nothing besides `source: dhcp`.

### Impact
- [x] Breaking change to the `IpamSpec` wire format (no consumers exist; safe)
- [ ] Requires cluster rollout (no controller yet)
- [x] Closes `.wolf/buglog.json` bug-006 — `cargo run -p banlieue-api --bin crdgen --features crdgen` now succeeds and emits all 6 CRDs

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features` ✅ — **143 passed** (was 139; +4 new `IpamSource` tests)
- `cargo run -p banlieue-api --bin crdgen --features crdgen | python3 -c "import yaml,sys; print(len(list(yaml.safe_load_all(sys.stdin))))"` → `6` ✅

---

## [2026-05-25] - Dependency + Edition Upgrade (align with kube-rs/controller-rs)

**Author:** Erick Bourgeois

### Changed
- `Cargo.toml`: Workspace dep & edition bump to match the kube-rs reference controller (`kube-rs/controller-rs`, pushed 2026-05-19).
  - `kube` `0.96` → `3` — features changed from `["derive", "client", "rustls-tls"]` (with `default-features = false`) to `["derive", "client", "runtime"]` (default TLS). The `runtime` feature is what unlocks `Controller::new`, `watcher`, `reflector`, `finalizer`, etc., for the upcoming `banlieue-controller` crate.
  - `k8s-openapi` `0.23` → `0.27`, feature `v1_31` → `latest` (auto-tracks the newest supported Kubernetes API). `schemars` feature retained.
  - `schemars` `0.8` → `1`.
  - `thiserror` `1` → `2`.
  - Added `tokio = "1"`, `tracing-subscriber = "0.3"`, `futures = "0.3"`, `anyhow = "1"` to `[workspace.dependencies]` so the upcoming controller/provider crates can pull them via `.workspace = true`.
  - Edition `2021` → `2024`. MSRV `1.80` → `1.85`.
- `crates/banlieue-api/src/banlieue/provider_tests.rs`: replaced `chrono_now()` helper (used the now-gone `k8s_openapi::chrono` re-export) with `parse_time(rfc3339)` that round-trips an RFC3339 string through `Time`'s `Deserialize` impl — works whether `Time` wraps `chrono::DateTime<Utc>` (old) or `jiff::Timestamp` (new in 0.27).
- Edition 2024 rustfmt rewrapped two `assert!(crd.spec.versions.iter()...)` chains into the new block style.

### Why
The user asked to align the project with kube-rs's own recommendations (`kube-rs/controller-rs`) and upgrade all deps to latest before the controller crate is implemented. Doing this now avoids a much larger rebase later, when the controller and 3+ provider crates have all locked onto the old versions.

### Impact
- [x] Breaking change for **downstream Rust consumers** (kube 3 reshaped its API surface — `kube::CustomResource` derive macro and runtime types). No external consumers exist yet.
- [ ] Requires cluster rollout (no controller yet)
- [x] Config change only (workspace `Cargo.toml`)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features` ✅ — 139 passed, 0 failed
- IpamSpec / kube-derive CRD-gen panic (bug-006) **still present** on schemars 1 + kube 3; the `*_currently_panics_due_to_ipam_spec_bug` pin tests continue to catch the panic, so no test had to be updated.

---

## [2026-05-24 19:30] - Comprehensive Unit Tests + Build Fixes

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/common_tests.rs`: 40 tests covering `InitializationStatus`, `MachineAddress`/`MachineAddressType`, `LocalObjectReference`, `TypedObjectReference`, `LabelSelector`/`Requirement`/`Operator`, `DiskProvisioning`, `Firmware`, `PowerState`, `IpamSpec` (Dhcp/Static/Pool), and the `condition_reasons`/`condition_types` constants. Positive (round-trip), negative (rejects unknown variant), and exception (missing required field) cases for every public type.
- `crates/banlieue-api/src/banlieue/provider_tests.rs`: 20 tests covering `ProviderCapabilities::is_empty` exhaustively, `ProviderSpec`/`ProviderStatus`/`ProviderConnection` round-trips, skip-serialization of `paused=false`/empty capabilities, `StorageClassMapping`/`NetworkClassMapping`, and `Provider::crd()` metadata.
- `crates/banlieue-api/src/banlieue/virtualmachine_tests.rs`: 23 tests covering `AffinityMode`/`MigrationPolicy` defaults + variants, `default_power_on`/`default_userdata_key` defaults via deserialization, `PlacementSpec`/`AntiAffinityRule`, `VirtualMachineSpec`/`Status`, `ScheduledPlacement`, `ResolvedResource`, and `VirtualMachine::crd()` metadata.
- `crates/banlieue-api/src/banlieue/vmclass_tests.rs`: 15 tests covering `HardwareSpec`/`DiskSpec`/`NetworkInterfaceSpec`, camelCase `memoryMiB`/`sizeGiB`/`storageClass` field naming, firmware/provisioning defaults, missing-required-field rejections, plus a pinned panic test for the IpamSpec/kube-derive CRD bug.
- `crates/banlieue-api/src/banlieue/vmimage_tests.rs`: 22 tests covering `OsFamily`/`Architecture`/`GuestAgent`/`ImageSourceKind` exhaustively, `ImageSource` with the `ref` rename and optional `importFrom`/`checksum`, `VMImageSpec`/`Status`, `ImagePerProviderStatus`, and `VMImage::crd()` metadata (cluster-scoped).
- `crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs`: 19 tests covering `VSphereDiskSpec`/`VSphereNicSpec`, the `providerID` rename, optional `folder`/`resourcePool`/`failureDomain`/`macAddress`, full `VSphereMachineSpec`/`Status` round-trips, `VSphereMachineTemplate`, plus pinned panic tests for both vSphere CRDs.

### Fixed
- `Cargo.toml`: Added the `schemars` feature to `k8s-openapi` so `Condition` and `Time` implement `JsonSchema`. Without this, the lib failed to compile because several CRD status structs contain `Vec<Condition>` / `Option<Time>` fields.
- `crates/banlieue-api/Cargo.toml`: Moved `serde_yaml` from `[dev-dependencies]` to an optional `[dependencies]` entry and made the `crdgen` feature pull it in (`crdgen = ["dep:serde_yaml"]`). The `crdgen` binary uses `serde_yaml` and previously could not link.
- `crates/banlieue-api/src/banlieue/vmimage.rs`: Removed unused `use crate::common::*;` import (was warning under `-D warnings`).
- `crates/banlieue-api/src/banlieue/vmclass.rs`: Inserted a blank line in a rustdoc block so clippy's `doc-lazy-continuation` lint is satisfied.

### Known Issues
- `VMClass::crd()`, `VSphereMachine::crd()`, and `VSphereMachineTemplate::crd()` panic at runtime because `IpamSpec` is a tagged enum and kube-derive's schema flattener disallows divergent discriminator metadata across variants. Logged as `bug-006` in `.wolf/buglog.json`; pinned by `*_currently_panics_due_to_ipam_spec_bug` tests so the fix surfaces automatically.

### Why
Adds a comprehensive unit test floor (139 tests) per the project's TDD rules, and unblocks the workspace which would not previously compile. Tests follow the project convention: separate `_tests.rs` files with `#[cfg(test)] #[path = "..."] mod foo_tests;` and an inner `mod tests`.

### Impact
- [x] Documentation only / non-breaking
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (Cargo.toml / Cargo.toml of `banlieue-api`)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features` ✅ — 139 passed, 0 failed
