<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Contributing to banlieue

Thank you for helping. This page covers what every contribution needs, how a
change moves from idea to merge, and how to build and test locally.

By taking part you agree to follow the [Code of Conduct](https://github.com/firestoned/banlieue/blob/main/CODE_OF_CONDUCT.md).
Security issues do **not** go in public issues or pull requests: report them
privately as described in [SECURITY.md](https://github.com/firestoned/banlieue/blob/main/SECURITY.md).

## Every commit: sign-off (DCO) and signature

banlieue uses the [Developer Certificate of Origin](https://developercertificate.org/)
(DCO) instead of a CLA. A sign-off certifies that you wrote the change, or
otherwise have the right to submit it under the project's
[Apache-2.0 licence](https://github.com/firestoned/banlieue/blob/main/LICENSE).

**Every commit needs both:**

1. **A `Signed-off-by:` trailer** matching the commit author (`git commit -s`).
2. **A cryptographic signature**, GPG or SSH (`git commit -S`). CI verifies
   signatures on every pull request, and a pull request with an unsigned
   commit cannot merge.

```sh
git commit -s -S -m "fix(controller): describe the change"
```

To fix commits that are missing either, rewrite your branch before pushing
again:

```sh
git rebase --signoff -S origin/main     # add sign-off and signature to every commit
git push --force-with-lease
```

The DCO check runs on every pull request. A missing sign-off is the most
common reason a first contribution is held up, so set
`git config commit.gpgsign true` and add `-s` to your habits (or an alias)
before you start.

## How a change gets in

banlieue is built **ADR-first**, under
[Architecture Driven Development](https://github.com/firestoned/banlieue/blob/main/.claude/rules/architecture-driven-development.md):

```
ADR  →  CALM  →  TDD  →  implement  →  docs  →  threat model
```

- **Architecturally significant changes** (a new CRD, controller, provider or
  binary; a contract change; anything worth a "why A over B") start with an
  ADR in [`docs/adr/`](https://github.com/firestoned/banlieue/tree/main/docs/adr/) and an update to the CALM model in
  [`docs/architecture/calm/`](https://github.com/firestoned/banlieue/tree/main/docs/architecture/calm/). Open the ADR as its own
  pull request, or as the first commit of the change, so the decision can be
  discussed before code is reviewed.
- **Everything else** (bug fixes, docs, refactors that keep behaviour) needs
  tests, written first.
- An ADR is only implemented once the [threat model](https://firestoned.github.io/banlieue/security/threat-model/)
  has had a full pass against it.

Roadmaps live in [`.github/community/`](https://github.com/firestoned/banlieue/tree/main/.github/community/), indexed by
[`ROADMAPS.md`](https://github.com/firestoned/banlieue/blob/main/ROADMAPS.md). If your change completes a roadmap item, update
both in the same pull request.

### Pull requests

- Keep one logical change per pull request.
- Use [Conventional Commits](https://www.conventionalcommits.org/) for commit
  subjects (`feat(scope): …`, `fix(scope): …`, `docs: …`). The release
  changelog is generated from them.
- Fill in what changed and why, and how you tested it.
- CI must be green: formatting, clippy with `-D warnings`, tests, `cargo-deny`,
  CodeQL, licence headers and commit signatures.
- Every source file carries an SPDX header
  (`SPDX-License-Identifier: Apache-2.0`); CI enforces it.

### Rules worth knowing before you write code

- **Never commit real infrastructure identifiers.** No real hostnames, IPs,
  usernames or account IDs, anywhere: code, tests, docs, examples, commit
  messages. Use the placeholders in
  [`no-real-infrastructure.md`](https://github.com/firestoned/banlieue/blob/main/.claude/rules/no-real-infrastructure.md)
  (`bar.foo.io`, RFC 5737 addresses).
- **The CRD schema is code-first.** `crates/banlieue-api` is the source of
  truth; regenerate `deploy/crds/` with `make crds` and never edit the
  generated YAML by hand.
- **No RPC between the controller and providers.** They talk through CRDs and
  the Kubernetes API only.
- Style and testing rules: [`rust-style.md`](https://github.com/firestoned/banlieue/blob/main/.claude/rules/rust-style.md) and
  [`testing.md`](https://github.com/firestoned/banlieue/blob/main/.claude/rules/testing.md). Tests go in separate `*_tests.rs`
  files.

## Building and testing

Prerequisites, the `kind` and `vcsim` loop, and running components
out-of-cluster are in the
[developer guide](https://firestoned.github.io/banlieue/developer/local-development/). The gate every
change must pass:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

| Target | What it does |
| --- | --- |
| `make test` | All workspace tests |
| `make lint` | Formatting check and clippy with `-D warnings` |
| `make crds` | Regenerate `deploy/crds/` from the Rust types |
| `make calm-validate` / `make calm-diagrams` | Validate the CALM model and render its diagrams |
| `make docs` / `make docs-serve` | Build or serve the documentation site |
| `make kind-e2e` | The `kind`-based end-to-end suites |

Suites that need a real hypervisor (`make libvirt-live-test`,
`make vsphere-live-test`, `make proxmox-live-test`) take their endpoints from
the environment and never run in CI.

## Getting help

Open a [GitHub issue](https://github.com/firestoned/banlieue/issues) for bugs
and feature requests, or a draft pull request to discuss a change in code.
How decisions are made, and how to become a maintainer, is in
[GOVERNANCE.md](https://github.com/firestoned/banlieue/blob/main/GOVERNANCE.md).
