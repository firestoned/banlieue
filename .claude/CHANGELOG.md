# Changelog

## [2026-05-31 19:15] - vim_rs → rustls patch; revert OpenSSL scaffolding

**Author:** Erick Bourgeois

### Added
- `patches/vim_rs.patch` — one-hunk patch on the vendored `vim_rs` checkout's `vim_rs/Cargo.toml`: `reqwest = { version = "0.12" }` → `{ version = "0.12", default-features = false, features = ["rustls-tls-native-roots", "charset", "http2"] }`. Generated from the pinned commit so `make vendor-vim-rs` applies it cleanly (and reverse-detects it as already-applied). No source changes — `vim_rs`'s client uses only backend-agnostic reqwest APIs (`danger_accept_invalid_certs`/`_hostnames` are `__tls`-gated, not native-tls). **`rustls-tls-native-roots`** (not `rustls-tls`) uses the OS trust store (`rustls-native-certs`, already in the tree via kube) instead of bundling `webpki-roots` — which keeps the lockfile identical to bindy/5-spot and avoids `webpki-roots`'s `CDLA-Permissive-2.0` license tripping `cargo deny check licenses`.

### Changed
- This makes the whole workspace **OpenSSL-free**: `Cargo.lock` now shows `openssl-sys: 0, native-tls: 0, rustls: 1, webpki-roots: 0, rustls-native-certs: 1` — matching the bindy / 5-spot reference repos (rustls + ring, native trust roots). `cargo metadata` reports no "patch not used" warning; `cargo deny check licenses` → ok.
- **Reverted the interim OpenSSL scaffolding** (no longer needed): removed the `libssl` build stage + `LD_LIBRARY_PATH` from `Dockerfile` and `Dockerfile.chainguard` (back to plain single-stage COPY); deleted `Cross.toml`; reverted `Makefile` `kind-load` from `cross` back to the host gcc cross-toolchain (rustls/ring cross-compiles with just the cross-gcc + linker/CC env, like bindy); updated the provider crate's TLS comment and the developer doc.

### Why
`vim_rs` was the lone OpenSSL puller (via reqwest's default native-tls). Patching its reqwest to rustls — via the vendored-checkout + `[patch.crates-io]` mechanism, no fork — removes OpenSSL entirely, so cross-compiling from macOS and the distroless/Chainguard images "just work" with no libssl at build or runtime.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build / packaging change (run `make vendor-vim-rs` before bare `cargo`; rebuild images)
- [ ] Documentation only

Verified: patch applies idempotently via `make vendor-vim-rs`; `cargo tree -i openssl-sys` empty; lockfile `openssl-sys: 0 / rustls: 1`; `cargo check -p banlieue` exit 0 (full workspace compiles with rustls).

## [2026-05-31 18:45] - Makefile: RUST_LOG override on kind-deploy-{controller,provider-vsphere}

**Author:** Erick Bourgeois

### Changed
- `Makefile` — `kind-deploy-controller` and `kind-deploy-provider-vsphere` now `kubectl set env … RUST_LOG=$(RUST_LOG[_VSPHERE])` on the Deployment after applying, so the in-cluster log level is overridable the same way as `run-local`: `RUST_LOG=debug,kube=debug make kind-deploy-controller`. The container `env` overrides the ConfigMap's `RUST_LOG` for that key; default stays `info,kube=warn` (+`vim_rs=warn` for the provider).

### Why
Parity with `run-local` / `provider-vsphere-run-local` — debug an in-cluster deploy without hand-editing the ConfigMap.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Developer tooling only

Verified by `make -n` for default and overridden `RUST_LOG`.

## [2026-05-31 18:30] - Build vim_rs from a vendored checkout + local patch (no fork)

**Author:** Erick Bourgeois

### Context
We want to carry a local change to `noclue/vim_rs` that is being submitted
upstream, without owning a fork. Approach: build against a vendored checkout
pinned to an upstream commit, with a checked-in patch applied at build time and
wired in via `[patch.crates-io]`. The mechanism is fork-free and self-retiring —
once the change ships upstream, the build detects it and skips re-applying.

### Added
- `Makefile`: `vendor-vim-rs` target — clones `noclue/vim_rs` into
  `third_party/vim_rs` (gitignored), `reset --hard` to `VIM_RS_REF`, then applies
  `patches/vim_rs.patch` idempotently: applies if clean, **skips if already
  present** (merged upstream / reverse-applies), hard-errors if stale. Wired as a
  prerequisite of every cargo-invoking target — `build`, `build-debug`, `test`,
  `test-lib`, `lint`, `crds`, `api-docs`, `provider-vsphere-run-local`, `sbom`,
  `vex-auto-presence`, `vex-auto-reachability`, `_build-linux`, `kind-load`.
- `Cargo.toml`: `[patch.crates-io]` redirecting `vim_rs` to the crate's
  subdirectory in the vendored checkout — `third_party/vim_rs/vim_rs` (upstream
  is a multi-crate repo with no root manifest; the crate lives under `vim_rs/`).
  Dep pinned to **`=0.4.4`** exact (was `0.4`).
- `.github/actions/vendor-vim-rs/action.yml`: composite action that runs
  `make vendor-vim-rs`; dropped into every cargo-using job in `build.yaml`
  (format, clippy, build, test, security, cargo-deny, auto-vex-presence) right
  after checkout. `docs.yaml` vendors transitively via `make docs` → `api-docs`.
- `patches/README.md`: create / refresh / retire workflow for the patch.
- `.gitignore`: ignore the vendored `third_party/vim_rs/` checkout.

### Why
Avoids maintaining a full fork: the pin lives in the `Makefile`, the diff lives
in `patches/vim_rs.patch`, and the upstream-merged check means the build keeps
working across bumps. The pin is a **commit, not a tag**: the version we need
(0.4.4 — first to carry the `vcsim_compat` feature the provider uses) was
published to crates.io and lives on `main` but was never git-tagged; the newest
tag (v0.4.3) predates that feature. The `=0.4.4` exact pin is required so cargo's
resolver lands on that version and the patch actually takes effect — a range
(`0.4`) would let it pick crates.io 0.4.4 and silently ignore the path patch.
Because the patch source is gitignored and absent after `actions/checkout`, every
cargo step (local and CI) must vendor first or fail to read the manifest.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build / packaging change (run `make vendor-vim-rs` after clone; `make`
      targets and CI do it automatically — a bare `cargo build` needs the
      checkout first)
- [ ] Documentation only

> Pairs with the OpenSSL build entry below: if the upstream patch switches
> `vim_rs` off native-tls (rustls), the libssl runtime/image gymnastics there
> can later be reverted.

## [2026-05-31 18:00] - Build: system OpenSSL (dynamic) — libssl in images + `cross` for local

**Author:** Erick Bourgeois

### Context
`vim_rs`'s reqwest uses native-tls → OpenSSL on Linux (kube is rustls; vim_rs is the lone OpenSSL source). Chosen approach: use the **system OpenSSL, dynamically linked** (no vendoring, no vim_rs fork). That requires libssl at build time and `libssl.so.3` in the runtime images.

### Changed
- `crates/banlieue-provider-vsphere/Cargo.toml` — removed the interim `openssl = { vendored }` dependency; back to plain dynamic system OpenSSL.
- `Dockerfile` (distroless) and `Dockerfile.chainguard` — added a `libssl` build stage that stages `libssl.so.3` / `libcrypto.so.3` (Debian `libssl3` / Wolfi `openssl`) and copies them into the runtime image under `/usr/local/lib` with `LD_LIBRARY_PATH` (neither base ships OpenSSL, and there's no ldconfig). Fixes the `libssl.so.3: cannot open shared object file` runtime error. Built per-target-platform under buildx so the `.so` arch matches the binary.
- `Makefile` — `kind-load` now builds the Linux binary with **`cross`** (a Linux container that has `libssl-dev`, per the new `Cross.toml`) instead of the host gcc cross-toolchain, which can't link a Linux libssl from macOS. Native Linux still builds directly.
- `docs/src/developer/local-development.md` — documents `cargo install cross` for local image builds and why.

### Added
- `Cross.toml` — installs target-arch `libssl-dev` in `cross`'s build containers for both Linux targets.

### Why
CI builds the binary natively on Linux (libssl-dev present) — the release pipeline was never blocked. The two real gaps were the **runtime images** (no libssl) and **local macOS image builds** (cross-linking OpenSSL). Both are now closed without a vim_rs fork or vendoring.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build / packaging change (rebuild images to pick up libssl; `cargo install cross` for local image builds)
- [ ] Documentation only

> `LIBSSL_IMAGE` (debian:trixie-slim / wolfi-base) is currently a floating tag — pin by digest (Dependabot, docker ecosystem) to match `BASE_IMAGE`.

## [2026-05-31 17:10] - ADR-0007 + CALM control for admission policies

**Author:** Erick Bourgeois

### Added
- `docs/adr/0007-admission-policies.md` — records the decision to enforce CRD invariants (immutability) via `ValidatingAdmissionPolicy` rather than a validating webhook or CRD-embedded CEL. Context, decision, consequences, and alternatives (webhook → extra service + cert lifecycle; CRD `x-kubernetes-validations` → most code-first but couples to schemagen and can't roll out report-only; controller-side → too late).
- `docs/architecture/calm/architecture.json` — new top-level control `admission-policy-validation` (K8s VAP reference + NIST SSDF PW.5/RV.1) and ADR-0007 added to the `adrs` list.

### Changed
- `deploy/admission/README.md` — links to ADR-0007.

### Why
ADD requires architecturally significant changes (a new security/deploy artifact) to be recorded as an ADR and modeled in CALM. This backfills both for `deploy/admission/`, added in the previous entry at the maintainer's direction.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation / architecture record only

Verified: `make calm-validate` → 0 errors / 0 warnings; `architecture.json` parses.

## [2026-05-31 17:00] - CI: deploy docs to GitHub Pages on merge to main (interim)

**Author:** Erick Bourgeois

### Changed
- `.github/workflows/docs.yaml` — the GitHub Pages deploy (Setup Pages, Upload Pages artifact, and the `deploy` job) now fires on a direct push to `main` in addition to the existing release path. Condition broadened to `(push && refs/heads/main) || (workflow_run release success)`. Header/comment blocks updated to reflect the interim "publish on every merge to main" policy.

### Why
Requested: deploy the documentation on merge to main "for now." Previously docs only published on a successful Build run for a release. PR builds still validate-only; the release-gated path is retained.

### Notes
- Path-filtered: a merge to main only redeploys when docs-affecting paths change (`docs/**`, `crates/**/*.rs`, the docs/calm workflows) — identical docs aren't needlessly republished.
- Requires the repo's Pages source set to "GitHub Actions" (already used by the release deploy). Top-level token already grants `pages: write` + `id-token: write`.
- Treated as a non-architectural CI-policy tweak (broadens an existing deploy trigger; no new topology), so no ADR/CALM per ADD.

### Impact
- [x] CI / docs deployment only
- [ ] Breaking change
- [ ] Requires cluster rollout

### Verification
`actionlint .github/workflows/docs.yaml` clean.

## [2026-05-31 16:30] - Docs: restructure into Guides / Developer + admission policies

**Author:** Erick Bourgeois

### Added
- `deploy/admission/` — ValidatingAdmissionPolicies (GA, K8s 1.30+, CEL, no webhook): `virtualmachine-immutability.yaml` (immutable `classRef`/`imageRef`), `provider-immutability.yaml` (immutable `providerClassRef.name`), each with a `Deny` binding, plus a README.
- `docs/src/guides/` — new top-level **Guides** tab (production, `ghcr.io/firestoned/banlieue:v0.1.0`): `index.md`, `core-controller.md` (CRDs → namespace → RBAC → configmap → deployment → ValidatingAdmissionPolicies → verify), `vsphere-provider.md` (ground-up: provider install → Secret → Provider → VMClass → VMImage → VirtualMachine → verify, every `kubectl apply`).
- `docs/src/developer/` — new top-level **Developer** tab: `index.md` + `local-development.md`, migrating the old build-from-source quickstart and the vSphere `vcsim`/`run-local`/`GOVC_*` content out of the user-facing pages.

### Changed
- `docs/mkdocs.yml` — **Why banlieue?** moved under **Home** (per request); new **Guides** and **Developer** tabs added to the nav.
- Cross-links updated in `concepts/providers.md`, `index.md`, `overview.md`, `reasoning/non-goals.md` to point at the new Guides/Developer pages. All quick-start/install paths now use `v0.1.0`.

### Removed
- `docs/src/getting-started/` (`quickstart.md`, `vsphere-provider.md`) — split into the production Guides (ghcr.io) and the Developer local-dev page.

### Why
The getting-started docs conflated production install with local development and predated the single-binary/v0.1.0 model. Splitting into release-oriented **Guides** and **Developer** local-dev, with admission hardening documented and shipped, gives a clean install path for the upcoming `v0.1.0` release.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation + optional deploy artifacts (admission policies)

Verified: `mkdocs build --strict` exits 0 (no broken links/nav); admission YAML parses and is valid against the GA `admissionregistration.k8s.io/v1` schema.

> Note (ADD): `deploy/admission/` is a new security/deploy artifact; per ADD it could be formalized with an ADR (e.g. `0007-admission-policies`). Authored here at the maintainer's direction as part of the controller guide — happy to add the ADR + CALM control if desired.

## [2026-05-31 16:00] - Auto-VEX: port presence + reachability tools from 5-spot

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-vex/` — new workspace crate (added to members) porting 5-spot's auto-VEX tooling verbatim (adjusted only for `banlieue_vex` / `pkg:oci/banlieue` / copyright):
  - `auto_vex_presence` module + `auto-vex-presence` bin — emit `not_affected + component_not_present` for Grype findings whose affected purl is absent from every image SBOM.
  - `auto_vex_reachability` module + `auto-vex-reachability` bin — emit `not_affected + vulnerable_code_not_in_execute_path` for Grype CVEs whose curated affected symbols (`.vex/.affected-functions.json`) are all absent from the release binary's `nm -D --undefined-only` table.
  - Full ported unit suites (41 tests): pure logic, deterministic sorted output, dedup, dotfile/metadata skipping, malformed-input errors.
- `.github/workflows/build.yaml` — new `grype-triage` (raw scan → JSON), `auto-vex-presence`, `auto-vex-reachability` jobs; `build-vex` now merges curated `.vex/*.json` **plus** both auto-derived documents before Cosign-attesting and feeding `grype --vex`.
- `Makefile` — `vex-auto-presence` / `vex-auto-reachability` local mirrors + `GRYPE_JSON`/`AFFECTED_FUNCTIONS`/`RELEASE_BINARY`/`SBOM_FILES` vars.
- `docs/adr/0006-*.md` — flipped the "Staged" section to "implemented" (the binaries are built/run in 5-spot, per maintainer); CALM `release-artifact-provenance` control de-staged.

### Fixed
- `crates/banlieue-api/src/{crddoc.rs,bin/crddoc.rs}`, `crates/banlieue-provider-vsphere/src/reconciler/{provider,vmimage}.rs` — collapsed nested `if let { if … }` into let-chains. These `clippy::collapsible_if` lints surfaced after the workspace MSRV bump to Rust 1.88 (let-chains stabilized) and were failing `clippy -D warnings --all-features`; pre-existing, unrelated to auto-vex, fixed so the workspace gate is green.

### Why
The maintainer corrected the prior turn's staging decision — the auto-vex binaries exist and run in `~/dev/5-spot` — so banlieue ports them rather than deferring. The full pipeline now derives VEX automatically (presence + reachability), merges with curated statements, attests, and scans.

### Impact
- [x] CI / release tooling (two new release binaries + three new CI jobs)
- [ ] Breaking change
- [ ] Requires cluster rollout

### Verification
`cargo fmt --all` + `cargo clippy --workspace --all-targets --all-features -D warnings` clean + `cargo test --workspace --all-features` (339) green; `actionlint .github/workflows/build.yaml` clean; `auto-vex-presence` smoke-tested locally (emits a valid `component_not_present` OpenVEX statement); `make calm-validate` clean.

## [2026-05-31 15:00] - Docs: fix stale provider-crate anatomy (single-binary, ADR-0004)

**Author:** Erick Bourgeois

### Changed
- `docs/src/concepts/providers.md` — the "Anatomy of a provider crate" section still showed `src/main.rs # binary entrypoint`. Per ADR-0004 each provider is a **library crate** (no `main.rs`); the single `banlieue` binary dispatches the `banlieue provider <name>` subcommand into the crate's `run()`. Updated the tree to the real layout (`lib.rs` re-exports `app::{Cli, run}`, `app.rs` holds the subcommand `Cli`/`run`, `Cargo.toml` is `[lib]`-only) and added a sentence explaining the binary↔library split.

### Why
Audit of docs vs. the single-binary model found this one stale section; everything else (architecture crates table, CALM system diagram, quickstart `banlieue completion`, vSphere guide, deploy manifests) already reflected ADR-0004.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified: `mkdocs build --strict` exits 0. No remaining `main.rs` / `cargo run -p banlieue-controller` references in `docs/src/`.

## [2026-05-31 14:30] - Release & supply-chain pipeline (binary, images, SBOM, SLSA, VEX) — ADR-0006

**Author:** Erick Bourgeois

### Added
- `docs/adr/0006-release-and-supply-chain-pipeline.md` (Accepted) — the `banlieue` binary is the core released artifact; every release ships signed multi-arch binaries, distroless + Chainguard images, SBOMs, SLSA L3 provenance, and an OpenVEX document. Models on `~/dev/5-spot`. Auto-VEX *derivation* binaries are explicitly staged.
- `.github/workflows/build.yaml` — rewritten to add the supply-chain jobs (the prior file deferred them with a note). New/changed jobs: `build` now emits `banlieue-linux-{amd64,arm64}` artifacts + CycloneDX SBOM (`make sbom`); `docker` (matrix Chainguard+Distroless, multi-arch buildx, push on non-PR, Cosign keyless sign by digest, BuildKit `sbom`+`provenance`, image SBOM via anchore/sbom-action); `attest` (GitHub build-provenance per image); `build-vex` (vexctl-merge `.vex/*.json` → Cosign `--type openvex` attest to each digest; empty-VEX-safe); `grype` (scan with `--vex` → SARIF to Code Scanning); `sign-artifacts` (tarball + Cosign + attest); `generate-provenance-subjects` + `slsa-provenance` (SLSA generator `@v2.1.0`); `package-deploy-manifests`; `upload-release-assets` (binaries + SBOMs + signatures + provenance + VEX + checksums). All firestoned composites reused; third-party actions SHA-pinned; SLSA generator tag-pinned.
- `.github/actions/prepare-docker-binaries/action.yml` — composite that stages the per-arch artifacts at `binaries/<arch>/banlieue` for the Dockerfiles.
- `Makefile` — `sbom`, `vexctl-install`, `vex-validate`, `vex-assemble` targets + `VEXCTL_VERSION`/`GRYPE_VERSION`/`PRODUCT_PURL` vars.
- `.vex/` — `README.md` (OpenVEX authoring spec), `.gitkeep`, `.affected-functions.json` (scaffold for the staged reachability tool).
- `docs/architecture/calm/architecture.json` — new `release-artifact-provenance` control (SLSA v1.0 Build L3 + SSDF), ADR-0006 registered. `make calm-validate` clean.

### Why
banlieue now has a deployable artifact (the single `banlieue` binary, ADR-0004), so the supply-chain pipeline that `build.yaml` had deferred is now warranted. Mirrors the maintainer's 5-spot pattern, adapted to banlieue's workspace.

### Staged (follow-up)
The automated VEX-derivation binaries `auto-vex-presence` (SBOM-absence) and `auto-vex-reachability` (symbol reachability) are **not** implemented — they are 5-spot's own Phase 2/3 and each warrant a TDD cycle. The VEX *assembly/attest/scan* plumbing is in place; `build-vex` has a documented seam where their artifacts merge in.

### Safety adaptation vs 5-spot
Images **build** on PRs (validates both Dockerfiles) but **push/sign/attest/scan only on push-to-main + release**, so fork PRs never require `packages:write`.

### Impact
- [x] CI / release tooling (new GHCR images, signing, SLSA, VEX on release + push-to-main)
- [ ] Breaking change
- [ ] Requires cluster rollout

### Verification
`actionlint .github/workflows/build.yaml` clean; `prepare-docker-binaries/action.yml` valid composite YAML; `make help` lists the new targets and `make -n sbom` expands; `.vex/.affected-functions.json` valid JSON; `make calm-validate` clean.

## [2026-05-31 13:00] - Bump workspace MSRV to Rust 1.88

**Author:** Erick Bourgeois

### Changed
- `Cargo.toml` — `[workspace.package] rust-version` `1.85` → `1.88`.
- `README.md`, `docs/src/index.md` — Rust MSRV badges `1.85+` → `1.88+`.

### Why
The lockfile already resolves `kube 3.1.0`, which declares `rust-version = 1.88`, so the previous `1.85` MSRV was inaccurate (it slipped through because `resolver = "2"` is not MSRV-aware). `cargo upgrade` — which *is* MSRV-aware — was flagging `kube` as "incompatible" because the newest kube compatible with a declared 1.85 MSRV is `2.0.1`. Bumping the declared MSRV to `1.88` makes it match what the project actually requires; `cargo upgrade` no longer flags `kube`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (toolchain MSRV)
- [ ] Documentation only

Verified: `cargo check --workspace --all-features` clean; `cargo upgrade --incompatible --dry-run` no longer lists `kube`.

> Follow-up option (not done): switch the workspace to `resolver = "3"` so cargo itself enforces MSRV during resolution, preventing a future silent overshoot.

## [2026-05-31 12:30] - CLI: `banlieue completion <shell>` subcommand

**Author:** Erick Bourgeois

### Added
- `crates/banlieue/src/cli.rs` — new `completion <shell>` subcommand on the unified binary. Generates a shell-completion script for the full command tree (`controller`, `provider <backend>`, `completion`) to stdout. Supports bash, zsh, fish, elvish, powershell via `clap_complete::Shell`. Logic in a testable `write_completion(shell, &mut impl Write)` helper.
- `crates/banlieue/src/cli_tests.rs` — 7 new tests: shell parsing (zsh + others), unknown/missing-shell errors, and generated-script content (zsh `#compdef banlieue` header + subcommand coverage; bash names the binary).
- `crates/banlieue/Cargo.toml` — `clap_complete = "4"` (part of the clap-rs project; tracks clap's major version). Single-crate dep, pinned directly.
- `docs/src/getting-started/quickstart.md` — "Shell completion" section with zsh/bash/fish install snippets.

### Why
Convenience: lets users install tab-completion (`banlieue completion zsh > "${fpath[1]}/_banlieue"`). Classified as a non-architectural CLI addition under the ADD methodology (no contract/topology/data-flow change), so TDD-only — no ADR/CALM.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] CLI / tooling only

### Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -D warnings` + `cargo test --all` (281) green; `banlieue completion zsh` emits a valid `#compdef banlieue` script; `mkdocs build --strict` clean.

## [2026-05-31 11:30] - Docs: remove roadmap from site; document CAPI cluster capability

**Author:** Erick Bourgeois

### Removed
- `docs/src/reference/roadmap.md` and its `mkdocs.yml` nav entry — roadmaps live outside the repo (project non-negotiable). All links repointed or dropped: `index.md` (status badge → GitHub repo; "full plan" wording; nav list), `overview.md`, `reasoning/non-goals.md`, `getting-started/{quickstart,vsphere-provider}.md`, `docs/README.md`. `concepts/virtualmachine.md` links that wrongly pointed VMClass/VMImage/API-reference at `roadmap.md` now point at `reference/api.md`. Remaining "roadmap" word-mentions reworded (`docs/adr/0003`, `architecture/index.md`).

### Changed (documentation of new CAPI work)
- `docs/src/reasoning/capi-relationship.md` — rewritten for the CAPI-native cluster decision: banlieue is a CAPI **infrastructure provider** implementing **both** the InfraMachine and InfraCluster contracts; clusters are built by CAPI core + a control-plane provider (k0smotron) over banlieue's infra CRs ("platinum = 6/6" = `replicas: 6`); corrected the contract status table to v1beta2 (`status.initialization.provisioned`, conditions-as-failures) — the page previously listed the deprecated `status.ready`/`failureReason` and claimed banlieue "never creates a cluster / takes only InfraMachine", contradicting ADR-0001/0002.
- `docs/src/concepts/infra-crds-capi.md` — intro now names both contracts; the contract field list corrected to v1beta2; added the `cluster.x-k8s.io/v1beta2` label note (ADR-0005).
- `docs/src/concepts/providers.md` — note that `VSphereCluster` aggregates Providers' failure domains across vCenters.

### Why
The user asked to remove roadmaps from the published docs and to ensure all new changes are comprehensively documented. The CAPI relationship page materially contradicted ADR-0001/0002 (it predated the InfraCluster/cluster-provisioning work) and listed CAPI fields deprecated under D-005.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Documentation only

### Verification
`mkdocs build --strict` clean (no broken links after roadmap removal; new anchor cross-reference resolves). No Rust changes.

## [2026-05-31 10:00] - CAPI contract label emitted by crdgen (ADR-0005)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0005-capi-contract-label-codegen.md` (Accepted) — decision to emit the CAPI v1beta2 contract label from `crdgen` (code-first), not a kustomize overlay.
- `crates/banlieue-api/src/crdgen_support.rs` — `add_capi_contract_label()`, applied by `prepared()`: stamps `cluster.x-k8s.io/v1beta2: <served versions>` onto every `infrastructure.banlieue.io` CRD; leaves `banlieue.io` CRDs untouched. 5 new tests in `crdgen_support_tests.rs`.

### Changed
- `deploy/crds/infrastructure.banlieue.io_{vsphereclusters,vspheremachines,vspheremachinetemplates}.yaml` — regenerated; each now carries `metadata.labels."cluster.x-k8s.io/v1beta2": "v1alpha1"`. `banlieue.io` CRDs unchanged (no label).
- `crates/banlieue-api/src/infrastructure/{vsphere_machine,vsphere_cluster}.rs` — docstrings corrected: the contract label is emitted by crdgen, not "applied via kustomize".
- `docs/adr/0002-*.md` — consequence note updated to point at ADR-0005 (kustomize overlay superseded).
- `docs/architecture/calm/architecture.json` — CAPI InfraMachine/InfraCluster controls now cite the emitted label + `crdgen_support` as evidence; ADR-0005 added to `adrs`. `make calm-validate` clean; diagrams + `api.md` regenerated.

### Why
Closes the contract gap flagged in ADR-0002: without this label CAPI core does not recognise banlieue's infra CRDs as contract-compliant. Code-first emission keeps the label in the single-source-of-truth generated YAML, so it can't drift and covers future provider CRDs automatically.

### Impact
- [x] Requires cluster rollout (CRDs must be re-applied to gain the label)
- [ ] Breaking change
- [ ] Config change only

### Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -D warnings` + `cargo test --all-features --all` (292 tests) all green; label present on all 3 infra CRDs and absent on all 4 `banlieue.io` CRDs; `make calm-validate` + `mkdocs build --strict` clean.

## [2026-05-31 00:10] - Docs: comprehensive README badges + minimal docs landing badges

**Author:** Erick Bourgeois

### Changed
- `README.md` — replaced the 4 placeholder badges with a comprehensive set in two rows: CI/security (Build, Documentation, CodeQL via the native GitHub Actions workflow badges; OpenSSF Scorecard) and project (License via dynamic shields, Rust MSRV, Docs site, Status, open Issues, Last commit, PRs welcome).
- `docs/src/index.md` — added a minimal badge set (Build status + Rust MSRV) alongside the existing License + Status badges on the docs landing page.

### Why
The project had only stub badges. Comprehensive, mostly-dynamic badges surface CI/security health and project signals at a glance on GitHub; the docs landing page gets a light, non-cluttered subset.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified: `mkdocs build --strict` exits 0. Badge URLs target real workflow files (`build.yaml`, `docs.yaml`, `codeql.yaml`, `scorecard.yaml`) and the public repo `firestoned/banlieue`.

## [2026-05-30 23:30] - Single `banlieue` binary with subcommand dispatch (ADR-0004)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0004-single-binary-subcommand-dispatch.md` — ADR: one `banlieue` executable packages every role; `banlieue controller` / `banlieue provider <name>` dispatch into independent library crates. Per-provider Cargo features (default = all); one image, role selected via container args.
- `docs/architecture/calm/architecture.json` — new `system-banlieue-binary` node + `rel-banlieue-binary-composed-of-roles` (`composed-of`) grouping the controller + provider services as roles of the one binary; registered ADR-0004. `make calm-validate` passes; diagrams regenerated.
- `crates/banlieue/` — new thin aggregator crate producing the single `banlieue` binary. `src/cli.rs` (clap subcommand tree + `dispatch`), `src/cli_tests.rs`, `src/main.rs`. Features: `default = ["vsphere"]`, `vsphere`, `vcsim` (pass-through).
- `crates/banlieue-provider-sdk/src/bootstrap.rs` (+ `_tests.rs`) — shared `init_tracing` / `serve_health` / `shutdown_signal`, eliminating the per-binary bootstrap duplication.
- `crates/banlieue-controller/src/app.rs` (+ `_tests.rs`) and `crates/banlieue-provider-vsphere/src/app.rs` (+ `_tests.rs`) — each role's `Cli` (`clap::Args`) + `pub async fn run(cli)`, ported from the deleted `main.rs` files.

### Changed
- `crates/banlieue-controller` and `crates/banlieue-provider-vsphere` are now **library-only** (removed `[[bin]]` + `src/main.rs`; export `Cli`/`run`). Trimmed tokio features (health/shutdown moved to the SDK) and dropped the now-unused `tracing-subscriber` dep.
- `crates/banlieue-provider-sdk` — added `bootstrap` module; tokio `net`/`io-util`/`signal` features + `tracing-subscriber` dep.
- `Cargo.toml` (workspace) — added `crates/banlieue` member.
- `Makefile` — `WORKSPACE_BINARIES`/`BINARY` default to `banlieue`; `run-local` → `cargo run -p banlieue -- controller`; `provider-vsphere-run-local` → `... -- provider vsphere`; `kind-load` no longer needs `BINARY=`.
- `Dockerfile` / `Dockerfile.chainguard` — default `ARG BINARY=banlieue`.
- `deploy/controller/deployment.yaml` / `deploy/provider-vsphere/deployment.yaml` — image → `ghcr.io/firestoned/banlieue:v0.1.0`; added role-selecting `args` (`["controller"]`, `["provider","vsphere"]`).
- `deploy/provider-vsphere/README.md`, `docs/src/getting-started/vsphere-provider.md` — updated build/run instructions to the single image + `banlieue provider vsphere` invocation.

### Why
One artifact to build, sign, scan, publish, and install — while keeping each role an independent crate with its own dependency graph (the CRD-only seam is intact; the controller still never links vSphere code unless a provider feature is on). Adding a provider becomes a feature + nested subcommand, not a new binary/image. See ADR-0004.

### Impact
- [x] Breaking change — image name changes (`banlieue-controller`/`banlieue-provider-vsphere` → `banlieue` + `args`); standalone per-role binaries no longer exist.
- [x] Requires cluster rollout — Deployments now reference the new image + args.
- [ ] Config change only
- [ ] Documentation / process only

## [2026-05-30 02:10] - Docs: root README intro + ADD methodology

**Author:** Erick Bourgeois

### Added
- `README.md` — replaced the empty stub with a full project intro: tagline + badges, what/why, a schema-correct `VirtualMachine` example (`classRef`/`imageRef`/`placement`), the "what banlieue is not" list, an Architecture section, a CRD resource table, repository layout, a Development section (incl. the ADD workflow + common `make` targets), project status, and license. The architecture section **references the single canonical diagram** at `docs/src/concepts/architecture.md` rather than duplicating a Mermaid block (one source of truth).
- `.claude/rules/architecture-driven-development.md` — new rule documenting **ADD (Architecture Driven Development)**: the governing `ADR → CALM → TDD → implement → docs` order, when full ADR+CALM applies vs TDD-only, and a checklist.

### Changed
- `.claude/CLAUDE.md` — added a top-level "GOVERNING METHODOLOGY: Architecture Driven Development (ADD)" section and an ADD entry in the CRITICAL Coding Patterns list.

### Why
The repo had no README. ADD is the maintainer's coined, governing methodology — architecture is decided (ADR) and visualized (CALM) before code (TDD) — and must steer all future work, so it's recorded in CLAUDE.md, a dedicated rule, and persistent memory.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation / process only

## [2026-05-30 22:30] - VSphereCluster (CAPI InfraCluster) + failure-domain aggregation

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/common.rs` — CAPI v1beta2 shared types `ApiEndpoint {host, port}` and `ClusterFailureDomain {name, controlPlane, attributes}` (the v1beta2 failure-domain *list* element), with round-trip tests in `common_tests.rs`.
- `crates/banlieue-api/src/infrastructure/vsphere_cluster.rs` (+ `_tests.rs`) — new `infrastructure.banlieue.io/v1alpha1` **`VSphereCluster`** CRD: banlieue's CAPI InfraCluster. Spec: `controlPlaneEndpoint`, `providerRefs`/`providerSelector` (aggregate FDs from one or more Providers), `controlPlaneFailureDomainSelector`, `paused`. Status: `initialization.provisioned`, `controlPlaneEndpoint`, `failureDomains[]`, `conditions`, `observedGeneration`. Wired into `crdgen`/`crddoc`/`lib.rs`; generated `deploy/crds/infrastructure.banlieue.io_vsphereclusters.yaml`.
- `crates/banlieue-controller/src/reconciler/vsphere_cluster.rs` (+ `_tests.rs`) — reconciler that aggregates selected `Provider.status.failureDomains[]` into the CAPI list (`build_status`/`select_providers`/`aggregate_failure_domains`, all unit-tested). No backend access. `controlPlaneFailureDomainSelector` sets per-FD `controlPlane`. Wired a second `Controller` in `main.rs` watching `VSphereCluster` + `Provider` (Provider changes requeue clusters).
- `deploy/controller/rbac/clusterrole.yaml` — least-privilege rules: `get/list/watch vsphereclusters`, `get/update/patch vsphereclusters/status` (no create/delete — CAPI/operator owns the lifecycle).
- `examples/06-vspherecluster-multi-vcenter.yaml` — a VSphereCluster spanning two vCenters.
- `docs/architecture/calm/architecture.json` — modeled the InfraCluster CR, CAPI-core node, `flow-provision-capi-cluster`, and the `capi-v1beta2-infra-cluster-contract` control; `make calm-validate` clean, diagrams regenerated.
- `docs/src/concepts/infra-crds-capi.md` — new "InfraCluster" section.

### Why
Implements ADR-0001/0002 (this turn) following the ADD methodology (ADR → CALM → TDD → implement → docs): banlieue becomes a CAPI infrastructure provider so k0s+k0smotron (and any CAPI consumer) drive cluster spread via `replicas`, with banlieue advertising failure domains aggregated across vCenters.

### Impact
- [x] Requires cluster rollout (new CRD + RBAC; controller now runs a second controller loop)
- [ ] Breaking change
- [ ] Config change only

### Follow-ups
- The CAPI contract label `cluster.x-k8s.io/v1beta2: v1alpha1` is not yet applied to any infra CRD at deploy time (no kustomize overlay exists — `VSphereMachine` has the same gap). Track separately.
- `cargo fmt` + `cargo clippy --all-targets --all-features -D warnings` + `cargo test --all` (261 tests) all green; `kubectl --dry-run=client` validates the CRD + RBAC; `mkdocs build --strict` clean.

## [2026-05-30 21:00] - ADRs: CAPI-native cluster provisioning + InfraCluster

**Author:** Erick Bourgeois

### Added
- `docs/adr/0001-capi-native-cluster-provisioning.md` (Accepted) — banlieue is a CAPI infrastructure provider; cluster lifecycle/spread/upgrades are CAPI's job (via k0smotron for k0s). No native `VMTier`/`VMCluster` CRD — "platinum = 6/6" is a CAPI `replicas: 6` over 6 failure domains.
- `docs/adr/0002-infracluster-failure-domain-aggregation.md` (Accepted) — add `infrastructure.banlieue.io/v1alpha1` `VSphereCluster` InfraCluster that aggregates failure domains from one or more `Provider`s into the CAPI v1beta2 `status.failureDomains` list. Reconciled by the main controller (pure CRD aggregation, no backend access). Capacity-awareness via provider FD gating + DRS host placement.
- `docs/adr/0003-provider-deployment-topology.md` (Proposed) — captures the per-class vs per-instance vs hybrid provider Deployment topology (O-003) for Phase 3; leans hybrid with a `deploymentStrategy` knob. Does not block 0001/0002.

### Why
Decision to keep cluster provisioning as close to CAPI as possible so banlieue works with k0s + k0smotron and any other CAPI consumer, rather than building a parallel native cluster/tier abstraction. Implementation of the `VSphereCluster` CRD and its reconciler follows.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (ADRs; code lands in follow-up entries)

## [2026-05-30 01:45] - CI: docs build regenerates the CRD API reference

**Author:** Erick Bourgeois

### Changed
- `Makefile` — the `docs` target now depends on `api-docs` (in addition to `calm-diagrams`), so `make docs` regenerates `docs/src/reference/api.md` from the Rust CRD types before building the MkDocs site.
- `.github/workflows/docs.yaml` — clarified the "Build documentation" step comment to note that `make docs` now also regenerates the API reference (the Rust toolchain was already installed for CALM-independent reasons). No new inline logic — the workflow stays Makefile-driven.

### Why
The published docs site must never show a stale CRD reference. Wiring `api-docs` into `make docs` means the Documentation workflow — which already runs `make docs` with cargo available — regenerates the reference from the committed types on every docs build (PR, push, and release deploy), catching any drift if a contributor forgets to run `make crds`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] CI / docs tooling only

Verified locally: `SKIP_CALM_DIAGRAMS=1 make docs` exits 0, regenerates `api.md`, and builds `docs/site/reference/api/index.html`.

## [2026-05-30 01:30] - Docs: generated CRD API reference page (crddoc)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/crddoc.rs` (+ `_tests.rs`) — `crdgen`-gated library module that renders every CRD as a single Markdown API-reference page. Walks each `openAPIV3Schema` (reusing `crdgen_support::prepared`), emitting per-CRD: metadata line, root "what/why" description, `kubectl get` printer columns, and recursive field tables (Field / Type / Required / Description) for `spec` and `status`. Nested objects, arrays-of-objects (`[]`), and maps (`map[string]T` / `{}`) each get their own sub-section; enum values render as "Allowed: …"; in-description Markdown headings are demoted to bold so they don't pollute the page TOC. 11 unit tests.
- `crates/banlieue-api/src/bin/crddoc.rs` — thin binary (`--out-file`, else stdout); `[[bin]] crddoc` with `required-features = ["crdgen"]`.
- `docs/src/reference/api.md` — generated API reference (all 6 CRDs), wired into the docs nav under **Reference → API Reference (CRDs)**.
- `Makefile` — `api-docs` target (`API_DOCS_OUT ?= docs/src/reference/api.md`); `make crds` now runs `api-docs` as its final step so the reference is refreshed on every CRD change.

### Changed
- `.claude/SKILL.md` — `regen-api-docs` skill updated from a Phase-4 stub to the real `make api-docs` flow.

### Why
Users (and the docs site) had no browsable schema reference — only raw CRD YAML. This renders the full CRD surface as HTML the docs site can navigate, generated from the Rust source of truth so it can never drift, and auto-refreshed whenever CRDs change.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (new generated reference page + tooling)

Verified: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -D warnings` (clean), `cargo test -p banlieue-api --all-features` (156 pass, +11 new). `make crds` regenerates YAML + `api.md`; `mkdocs build --strict` exits 0.

## [2026-05-30 01:00] - CRDs: comprehensive schema documentation in generated YAML

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/crdgen_support.rs` (+ `_tests.rs`) — new `crdgen`-gated library module with `promote_spec_description` / `prepared`. `kube-derive` hard-codes the root `openAPIV3Schema.description` to "Auto-generated derived type for `<T>` via `CustomResource`" and routes the spec struct's doc comment to the `spec` property instead. `prepared` promotes the authored spec description up to the CRD root so a bare `kubectl explain <kind>` shows the real "what is this resource" text. 2 unit tests (replace-boilerplate + no-op-without-spec-description).

### Changed
- `crates/banlieue-api/src/banlieue/{vmclass,vmimage,provider,virtualmachine}.rs`, `crates/banlieue-api/src/infrastructure/vsphere_machine.rs` — added comprehensive rustdoc to every CRD root spec struct (a "what is this / why create one / how it's used" narrative), every status struct, and the remaining nested structs / enums / fields that lacked descriptions. These flow into the generated CRD schemas (and `kubectl explain`).
- `crates/banlieue-api/src/bin/crdgen.rs` — each CRD is now run through `prepared(...)` before serialization; `render` takes the CRD by value.
- `crates/banlieue-api/src/lib.rs` — exposes `crdgen_support` under the `crdgen` feature.
- `deploy/crds/*.yaml` — regenerated. Every CRD root description is now the authored text (no more "Auto-generated derived type …" boilerplate); spec/status/field descriptions are richer throughout.

### Why
The generated CRDs are the schema users see via `kubectl explain` and IDE tooling. They previously carried kube-derive's placeholder root description and several undocumented fields. Documenting the Rust types (the code-first source of truth) is the only correct place to fix this — the YAML is generated, never hand-edited.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (schema descriptions; no field shape changes)

Verified: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -D warnings` (clean), `cargo test -p banlieue-api --all-features` (146 pass, +2 new). Generated CRDs `kubectl apply --dry-run` clean; `examples/` validate server-side dry-run.

## [2026-05-30 00:20] - Core docs: vSphere provider guide + Provider schema sync

**Author:** Erick Bourgeois

### Added
- `docs/src/getting-started/vsphere-provider.md` — new core-docs guide for the vSphere provider: credentials Secret creation (including a `GOVC_*` → Secret/Provider derivation flow with a mapping table), the minimal + capabilities-bearing `Provider` CR, running locally (`make provider-vsphere-run-local`, `RUST_LOG` override) and in-cluster, a `status` verification example, a `Ready=False` reason table (Provider + VMImage), and a `vcsim` local-dev walkthrough.
- `docs/mkdocs.yml` — added the new page to the nav under **Home → vSphere Provider**.

### Changed
- `docs/src/concepts/providers.md` — brought the `Provider` CR example in line with the actual `banlieue-api` schema: `spec.type` + `vsphere:` block → `spec.providerClassRef.name` + `spec.connection` + `spec.capabilities` (the docs had drifted from the code). Updated the provider-crate anatomy to the real layout (`client/{mod,vim,fake}.rs`, `reconciler/{provider,vmimage}.rs`, dual-Controller `main.rs`) and noted the trait-based fake-client testing seam. Linked to the new guide.
- `docs/src/getting-started/quickstart.md` — "Coming next" now links to the vSphere provider guide.

### Why
The GOVC Secret-creation how-to was only in `deploy/provider-vsphere/README.md`; the user asked for it in the published docs. While there, `concepts/providers.md` still documented an old `Provider` shape (`type:`/`vsphere:`) that no longer matches `crates/banlieue-api/src/banlieue/provider.rs`, so YAML copied from the docs would have been rejected by the CRD.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified with `mkdocs build --strict` (exit 0, no broken-link/nav warnings). All field names checked against `crates/banlieue-api/src/banlieue/provider.rs`.

## [2026-05-30 00:10] - Docs: create the vSphere Secret/Provider from GOVC_* env vars

**Author:** Erick Bourgeois

### Added
- `deploy/provider-vsphere/README.md` — new "Creating the Secret + Provider from your `GOVC_*` environment" section: a `GOVC_*` → banlieue field-mapping table and a copy-paste flow that builds the `vsphere-creds` Secret (`GOVC_USERNAME`/`GOVC_PASSWORD`) and a `Provider` whose `connection.endpoint` is normalised from `GOVC_URL` (strips scheme / `user:pass@` / trailing `/sdk`) and whose `insecureSkipTLSVerify` is derived from `GOVC_INSECURE`. Notes the `caBundle` alternative for CA-validated endpoints.

### Why
The provider is intentionally CRD/Secret-driven and does **not** read `GOVC_*` itself (explicit-over-implicit). Operators who already use `govc` had no documented path from their existing env to a working Provider; this closes that gap without weakening the spec-is-source-of-truth invariant.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-05-30 00:00] - Makefile: RUST_LOG overridable on *-run-local targets

**Author:** Erick Bourgeois

### Changed
- `Makefile` — extracted the hardcoded `RUST_LOG=info,kube=warn` out of the `run-local` and `provider-vsphere-run-local` recipes into `RUST_LOG ?=` / `RUST_LOG_VSPHERE ?=` variables. `?=` yields to a value passed in the environment, so `RUST_LOG=debug,kube=debug make run-local` now actually uses `debug` instead of being clobbered by the recipe's literal. `RUST_LOG_VSPHERE` derives from `RUST_LOG` (appending `vim_rs=warn`) so a single override flows to both targets; it can also be overridden directly to control vim_rs verbosity.

### Why
The previous recipes hardcoded `RUST_LOG`, silently overriding any value the user set on the CLI — so `RUST_LOG=debug make run-local` had no effect.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Developer tooling only

## [2026-05-27 10:30] - Phase 1B iteration 2a: VMImage reconciler (template availability)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs` — VMImage reconciler that walks each in-scope `Provider` of class `vsphere`, connects to its vCenter, and confirms the template named in `VMImage.spec.sources[].reference` is present in every failure-domain datacenter the Provider exposes. Writes `VMImage.status.perProvider[]` rows + an aggregate `Ready` condition. Stable per-row reasons: `Reconciled`, `TemplateNotFound`, `SecretUnavailable`, `ConnectFailed`, `LookupFailed`, `NoVSphereSource`. Pure helpers `find_vsphere_source`, `compute_template_status`, `aggregate_ready` (with bounded `&'static str` reason enum) keep the reconciler unit-testable without a kube cluster.
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage_tests.rs` — 12 unit tests: source-selection variants (vsphere/Template vs others), `compute_template_status` happy path / template-absent / no-datacenters, `aggregate_ready` true/false/unknown including unknown-reason-bucketing guard, plus VMImage minimal-construct smoke for field-rename drift.
- `crates/banlieue-provider-vsphere/src/client/{mod,fake,vim}.rs` — `Template { name, moref, datacenter_moref }` slim type and `VSphereClient::find_template(dc, name) -> Result<Option<Template>>` trait method. `FakeClient` extended with `Inventory::builder().with_template("dc", "name")` (panics if the DC isn't seeded yet). Real `vim` impl uses `ViewManager::create_container_view` scoped to the datacenter MO with `VirtualMachine` filter, walks the morefs, calls `VirtualMachine::config().await` per VM and matches on `cfg.template == true && cfg.name == name`. Destroys the ContainerView eagerly.

### Changed
- `crates/banlieue-provider-vsphere/src/main.rs` — second `Controller::new(VMImage, ...)` runs alongside the Provider controller. Both controllers race against `shutdown_signal()` in one `tokio::select!`; either stream ending unwinds the binary. VMImage Api is unconditionally `Api::all(client)` (cluster-scoped CRD) regardless of `--namespace`.

### Why
After Phase 1A iteration 4 the smoke-test boundary was stuck at `Scheduled=False reason=ImageNotReady`: the main controller's scheduler filters out every Provider candidate because no provider flips `VMImage.status.perProvider[<provider>].ready=true`. Iteration 2a closes exactly that gate. With this iteration deployed, a `kubectl apply -f examples/05-virtualmachine.yaml` against a real vCenter (or vcsim) now produces `VirtualMachine.status.scheduled` populated and a `VSphereMachine` CR created in the same namespace — though the VSphereMachine itself remains unprovisioned until iteration 2b's VM-lifecycle reconciler lands.

Scope was deliberately constrained: only `ImageSourceKind::Template` is supported (no `Url`-import, no `BackingFile`); only the per-Provider readiness check (no template fingerprint / OVF re-import path). Both deferrals are recorded with `NoVSphereSource` / `TemplateNotFound` reasons so operators get actionable feedback instead of silent failures.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only
- [x] **New capability** — VMImage template-availability check; main controller's smoke test now proceeds past `ImageNotReady` once an admin populates the vSphere template in vCenter.

Verified by `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings` (clean), `cargo test --all` (144 api + 43 controller + 27 sdk + 21 provider-vsphere = 235 tests, all pass — +12 new VMImage tests).

## [2026-05-26 20:30] - Phase 1B iteration 1: vSphere provider scaffold + capability introspection

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/` — new workspace crate, third member after `banlieue-controller` and `banlieue-provider-sdk`. Wires `vim_rs = "0.4"` with `default-features = false` (drops the `xml` SOAP transport — saves ~30-40% on debug compile). Optional `vcsim` feature flips on vim_rs's `vcsim_compat`. Cold build with vim_rs added: 3m 28s on the dev mac.
- `src/client/` — backend-agnostic `VSphereClient` trait + `VSphereClientFactory` trait so reconcilers can be unit-tested without `vim_rs`. Three modules: `mod.rs` (trait + slim domain projections `Datacenter` / `Cluster` / `Credentials`), `fake.rs` (`FakeClient` + ergonomic `Inventory::builder().with_dc("...").with_cluster(...)` for tests), `vim.rs` (production impl via `ClientBuilder::new(endpoint).basic_authn(...).insecure(...).build()` + `ViewManager` / `ContainerView` traversal).
- `src/reconciler/provider.rs` — `Provider` reconciler scoped to `spec.providerClassRef.name == "vsphere"`. Reads the `credentialsRef` Secret, connects to vCenter, walks DCs → clusters, builds one `FailureDomain` per (dc, cluster) with labels `{dc, cluster}` and `attributes.raw = {datacenter, cluster}`, then SSA-patches `Provider.status` with the FDs + `Ready=True` / `ProviderReachable=True`. Failure paths set typed conditions (`SecretMissing`, `SecretInvalid`, `ConnectFailed`, `InventoryFailed`) and short-requeue. Pure helper `failure_domain_name(provider, dc, cluster)` slugifies and truncates to 63 chars (k8s label-value cap).
- `src/reconciler/provider_tests.rs` — 9 unit tests covering the pure slug helper (basic / special-char stripping / consecutive-separator collapse / 63-char truncation), `discover_inventory` driven by `FakeClient` (count/shape, labels+raw, empty-DC, no-clusters), and a Datacenter `Clone+Eq` smoke test.
- `src/main.rs` — dual-purpose binary: CLI mirrors the main controller (`--kubeconfig`, `--namespace`, `--leader-election-*`, `--log-*`, `--health-port`, `--metrics-port`, plus `--vsphere-task-timeout-secs` reserved for iter 2). Reuses `banlieue_provider_sdk::leader::{acquire_or_wait, renew_forever}` and the same `shutdown_signal()` (SIGTERM + Ctrl-C) pattern. Default leader-election Lease: `banlieue-system/banlieue-provider-vsphere`.
- `deploy/provider-vsphere/{configmap,deployment,service,rbac/}.yaml` — full deploy manifests modeled on `deploy/controller/`. `ClusterRole` is cluster-wide (consistent with main controller's multi-tenancy story) and already includes the `infrastructure.banlieue.io/vspheremachines` verbs iteration 2 will use.
- `deploy/provider-vsphere/README.md` — operator-facing local-dev walkthrough: kind-up → vcsim-up → Secret → Provider → `provider-vsphere-run-local`. Documents the four `Ready=False` reason strings and how to recover.
- `Makefile` — new targets `vcsim-up` / `vcsim-down` / `vcsim-logs` (runs `vmware/vcsim:latest` on :8989), `provider-vsphere-run-local` (cargo run with `--features vcsim --no-leader-elect`), and `kind-deploy-provider-vsphere` (mirrors `kind-deploy-controller`).

### Changed
- `Cargo.toml` — workspace member list now includes `crates/banlieue-provider-vsphere`. New workspace dependency `vim_rs = { version = "0.4", default-features = false }` (pinned at workspace level so any future provider that needs it gets the same pin).

### Why
The roadmap's smoke-test boundary after Phase 1A iteration 3 was: "stops at `Scheduled=False reason=ImageNotReady` because no provider populates `VMImage.status.perProvider[].ready=true`." Phase 1B closes that. Iteration 1 ships the *capability-introspection* half — the binary connects to vCenter (real or `vcsim`), walks inventory, and writes `failureDomains[]` so the main controller's scheduler can place VMs. The VSphereMachine VM-lifecycle half (clone-from-template → power-on → status mirror) is iteration 2. Choosing `vim_rs` over hand-rolling VI bindings: actively maintained (v0.4.4 April 2026), tokio/reqwest async, ships a `vcsim_compat` feature for the simulator; the 3-5 minute cold compile is mitigated by isolating the dep to this one crate.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only
- [x] **New capability** — the binary can be deployed today to populate `Provider.status` for a vSphere-class Provider. VM lifecycle still NYI (iteration 2).

Verified by `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings` (clean across all four crates), `cargo test --all` (144 api + 43 controller + 27 sdk + 9 provider-vsphere = 223 tests, all pass). vcsim end-to-end smoke test is operator-driven via the manifest in `deploy/provider-vsphere/README.md` — not yet automated in CI.

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
