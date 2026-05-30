<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0006 — Release artifacts and supply-chain pipeline

- **Status:** Accepted
- **Date:** 2026-05-31
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0004 (single `banlieue` binary); D-019 (container images), D-020 (Apache-2.0 / DCO); `rules/github-workflows.md` (Makefile-driven, `firestoned/github-actions` composites). Models on the reference pipeline in `~/dev/5-spot`.

## Context

banlieue now ships a deployable artifact: the single `banlieue` binary
(ADR-0004) that packages the controller and every provider as subcommands.
Until now `build.yaml` deliberately deferred the container/supply-chain jobs
(there was a literal note: "Docker / VEX / Cosign / SLSA / Grype … lands when a
deployable binary ships"). That time is now.

The reference implementation is `~/dev/5-spot`: a single-binary Rust service with
a full release + supply-chain pipeline. We adopt the same shape, adapted to
banlieue's Cargo workspace and the `banlieue` binary.

## Decision

**The `banlieue` binary is the core released artifact, and every release carries
the full supply-chain attestation set.** Concretely, on `release: published`
(with the same chain also exercised on push-to-main for staging images):

1. **Binary** — `banlieue` is cross-built for `linux/amd64` and `linux/arm64`,
   tarred, **Cosign-signed** (keyless / OIDC) and **GitHub build-provenance
   attested**, and attached to the GitHub Release.
2. **Container images — both variants**, multi-arch (`linux/amd64,arm64`),
   pushed to `ghcr.io/firestoned/banlieue`:
   - **Distroless** (`Dockerfile`, `gcr.io/distroless/cc-debian13:nonroot`) —
     tag suffix `-distroless`.
   - **Chainguard** (`Dockerfile.chainguard`, `cgr.dev/chainguard/glibc-dynamic`) —
     no suffix (the hardened default).
   Each image is Cosign-signed by digest, gets BuildKit `sbom: true` +
   `provenance: true` attestations, and a GitHub build-provenance attestation.
3. **SBOM** — CycloneDX. A per-crate `*.cdx.json` for the binary (via
   `cargo-cyclonedx`) and a per-image SBOM (via `anchore/sbom-action`). Both are
   release assets.
4. **SLSA** — provenance is generated for the binary tarballs at **SLSA Build
   L3** via `slsa-framework/slsa-github-generator` (the official reusable
   workflow, pinned to a release **tag**, never a SHA — slsa-verifier rejects
   non-released refs). The `.intoto.jsonl` is attached to the Release.
5. **VEX** — an OpenVEX document is assembled from hand-authored `.vex/*.json`
   statements (via `vexctl merge`), Cosign-attested to each image digest
   (`--type openvex`), and consumed by the Grype container scan (`grype --vex`)
   so suppressed-with-justification CVEs do not re-alarm. The Grype results are
   uploaded to GitHub Code Scanning as SARIF.

**Conventions retained from the repo rules:**
- Workflows use `firestoned/github-actions/*` composite actions where one
  exists; third-party actions are SHA-pinned (except the SLSA generator, a tag).
- Image/binary build *logic* that can live in the Makefile does (`sbom`,
  `vexctl-install`, `vex-validate`, `vex-assemble`, the `docker-*` targets); the
  CI `docker` job uses `docker/build-push-action` directly (multi-arch buildx +
  attestations), mirroring 5-spot.

## Automated VEX derivation (implemented)

> **Update:** an earlier revision of this ADR *staged* the two derivation
> binaries. That was reversed — they are built and run in the 5-spot reference
> pipeline, so banlieue ports them rather than deferring.

banlieue derives VEX statements automatically with two binaries, ported from
5-spot into a new workspace crate **`crates/banlieue-vex`** (pure logic in
`auto_vex_presence` / `auto_vex_reachability` modules; thin clap+I/O bins
`auto-vex-presence` / `auto-vex-reachability`; full unit-test suites carried
over):

- **`auto-vex-presence`** — emits `not_affected + component_not_present` for
  every Grype finding whose affected purl is absent from **every** image SBOM
  and not already covered by a curated `.vex/*.json`. The SBOM is the mechanical
  definition of "what's in the product"; this is the one justification with a
  purely mechanical basis.
- **`auto-vex-reachability`** — emits `not_affected +
  vulnerable_code_not_in_execute_path` for each Grype CVE present in the curated
  `.vex/.affected-functions.json` map whose listed library symbols are **all
  absent** from the release binary's dynamic symbol-import table
  (`nm -D --undefined-only`). "No map entry" means "do not auto-derive", not
  "reachable".

Both write OpenVEX documents that the `build-vex` job merges (`vexctl merge`)
together with the curated `.vex/*.json` into the attested/scanned document. The
CI flow is: `grype-triage` (raw scan, no VEX) → `auto-vex-presence` +
`auto-vex-reachability` (consume the triage JSON + image SBOMs + binary symbols)
→ `build-vex` (merge curated + auto docs) → `grype` (`--vex`). Both derivations
are conservative (suppress only with mechanical evidence) and deterministic
(sorted output) so CI artifacts are diffable.

## Consequences

**Positive**
- Every release is a signed, SBOM'd, SLSA-L3, VEX-annotated artifact set —
  consumable by `cosign verify`, `slsa-verifier`, and SBOM/VEX tooling.
- Both a broadly-compatible (distroless) and a hardened/zero-CVE (Chainguard)
  image ship from one binary, for regulated environments (D-019).
- The pipeline runs on push-to-main too, so staging images carry the same
  attestations — no release-only surprises.

**Negative / costs**
- `build.yaml` grows substantially (≈10 new jobs) and the release path now
  depends on GHCR, Sigstore (OIDC), and the SLSA generator being reachable.
- New `GITHUB_TOKEN` scopes per job: `packages: write` (push), `id-token: write`
  (keyless Cosign / SLSA OIDC), `attestations: write`, `security-events: write`
  (SARIF), `contents: write` (release upload). All declared per-job; the
  top-level token stays read-only (Scorecard Token-Permissions).
- A new workspace crate (`banlieue-vex`) and two CI tools to maintain; the
  `.affected-functions.json` map is hand-curated and only as good as its
  entries (an unmapped CVE is simply never auto-suppressed — the safe default).

## Alternatives considered

- **Distroless only.** Rejected: D-019 wants a hardened option for regulated
  environments; Chainguard is the zero-CVE default, distroless the fallback.
- **Build images from a compiler stage in the Dockerfile.** Rejected (already,
  pre-this-ADR): the Makefile cross-compiles and the Dockerfiles `COPY` the
  pre-built binary — faster CI, smaller images, reproducible base via digest pin.
- **Re-derive the auto-vex binaries from scratch / use an off-the-shelf tool.**
  Rejected: 5-spot already has a tested implementation; porting it verbatim
  (adjusting only the product purl and crate name) is faithful and lower-risk
  than re-deriving the semantics or adopting a different VEX generator.
