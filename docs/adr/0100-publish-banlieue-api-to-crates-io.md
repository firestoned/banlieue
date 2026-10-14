<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0100: Publish banlieue-api to crates.io on every release

- **Status:** Accepted
- **Date:** 2026-10-14
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 12 §4.8 (the `cargo publish` item this closes);
  extends [ADR-0006](0006-release-and-supply-chain-pipeline.md) (the release
  pipeline); [ADR-0004](0004-single-binary-subcommand-dispatch.md) (why the
  other crates stay internal); [ADR-0097](0097-api-ends-at-virtualmachineclaim.md)
  (the API surface this crate carries)

## Context

`crates/banlieue-api` is the source of truth for every banlieue CRD. Anything
outside this repository that wants to create, read or watch banlieue objects
from Rust (mediatore's broker, an operator's own tooling, a CAPI integration)
either vendors the crate by git reference or re-derives the types from the
CRD YAML. A git dependency pins nothing a consumer can reason about, and
re-derived types drift silently from the schema the API server enforces.

Roadmap 12 left `cargo publish` of `banlieue-api` open at close (2026-10-09)
for want of three things: a crates.io owner, a publish token, and a decision
on whether the crate is a supported public surface. The token now exists as
the repository secret `CARGO_REGISTRY_TOKEN`; this ADR is the decision.

Two facts shape how it is published:

- **k8s-openapi forbids a library from choosing the Kubernetes version.** Its
  build script fails when more than one version feature is enabled, and its
  documentation says a library must enable none in its normal dependency, only
  in a dev-dependency. `banlieue-api` inherited the workspace dependency, which
  enables `latest`; published as is, it would break every consumer that picks
  its own version.
- **The release workflow stamps the tag's version into the workspace**
  (`make set-version`), so the crate's version is only correct inside the
  release job, after that stamp.

## Decision

1. **`banlieue-api` is published; no other crate is.** It is the API, and
   ADR-0097 bounds what that API is. The provider SDK, the backend clients and
   the binaries are implementation details of one binary (ADR-0004) and carry
   no compatibility promise. Publishing them would create one.

2. **The crate's version is the release's version.** It is published from the
   `release: published` run of `build.yaml`, after `make set-version` has
   stamped the tag's version and after the test job passed, so
   `banlieue-api 0.4.3` holds exactly the types behind the `v0.4.3` CRDs and
   binary. Under Cargo's rules for `0.x`, a minor bump may break; the CRD
   group versions are `v1alpha1`, and the crate promises nothing more stable
   than they do.

3. **The crate does not choose a Kubernetes version.** Its normal dependency
   on k8s-openapi enables only `schemars`. The dev-dependency enables
   `latest`, so the crate's own tests build; the `crdgen` feature enables
   `latest`, so `make crds` still builds the generator binaries; docs.rs
   builds with all features. A consumer enables the version feature it
   targets, as k8s-openapi requires.

4. **Publishing goes through `firestoned/github-actions/rust/publish-crate`**,
   SHA-pinned like every other action, in a job of its own
   (`publish-crate`) that holds `contents: read` and nothing else. The token
   is read only there, only on the release event. A version already on
   crates.io is skipped (`make crate-published`), so re-running a release
   does not fail on an immutable upload.

5. **Packaging is checked on every pull request.** `make crate-package` runs
   `cargo package -p banlieue-api` in the test job: the packaged crate is
   built on its own against crates.io dependencies, which is exactly what a
   consumer gets. A crate that only fails to package at release time would
   fail after the images were already published.

6. **The token is long-lived for the first publish only.** crates.io
   Trusted Publishing (GitHub OIDC, no stored token) can be configured once
   the crate exists. Moving to it needs an OIDC input on the publish action,
   which belongs in `firestoned/github-actions`; until then the token should
   be scoped to `publish-new` and `publish-update`, and to `banlieue-api`
   once the name is taken.

## Consequences

- A Rust consumer depends on `banlieue-api = "0.4"` and gets types that match
  a released set of CRDs, with docs on docs.rs.
- A change under `crates/banlieue-api/` that breaks packaging (a file read
  from outside the crate, a path dependency, a version feature on
  k8s-openapi) fails the pull request, not the release.
- `cargo build -p banlieue-api` on its own, without dev-dependencies or
  `crdgen`, now needs `K8S_OPENAPI_ENABLED_VERSION` set, the same as for any
  library built on k8s-openapi. Workspace builds are unaffected: the other
  crates enable `latest`, and features unify.
- A crates.io upload is immutable and carries no Sigstore signature or SLSA
  provenance, unlike the binary and images (ADR-0006). A bad version can be
  yanked, not replaced. Recorded in the threat model's §8.
- A new credential, `CARGO_REGISTRY_TOKEN`, sits in the repository's Actions
  secrets. Whoever holds it can publish any version of `banlieue-api`.
  Recorded as asset A-21 and on TB-6.
