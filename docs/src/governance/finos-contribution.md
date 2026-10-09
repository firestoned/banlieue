<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Contributing banlieue to FINOS

This is the plan for moving banlieue from `firestoned/banlieue` to the FINOS
organisation. Contribution is a legal and Technical Oversight Committee
process, run with FINOS staff, so the checklist below is checked against
FINOS's current requirements when it starts rather than assumed.

## Readiness checklist

| Item | Where | State |
| --- | --- | --- |
| Apache-2.0 licence, SPDX header on every source file | `LICENSE`, CI licence check | Done |
| `NOTICE` | `NOTICE` | Done |
| README with what it is and a quickstart | `README.md`, these docs | Done |
| Contributing guide with DCO | `CONTRIBUTING.md` | Done |
| Code of Conduct (Contributor Covenant 2.1) | `CODE_OF_CONDUCT.md` | Done |
| Governance and maintainer criteria | `GOVERNANCE.md`, `MAINTAINERS.md` | Done |
| Security policy and threat model | `SECURITY.md`, [threat model](../security/threat-model.md) | Done |
| Signed releases, SBOMs, SLSA provenance | release workflow (ADR-0006) | Done |
| DCO enforced on every pull request | DCO GitHub App on the repository | To do (repository setting) |
| OpenSSF Best Practices badge | bestpractices.dev registration | To do (roadmap 16) |
| Charter | [charter](charter.md) | Draft |

## Moving the repository

1. **Transfer, do not fork.** Transfer `firestoned/banlieue` to the FINOS
   organisation. GitHub redirects the old URL for clones, issues and pull
   requests, and keeps stars, history and releases.
2. **Images.** Publish under the new organisation's registry path from the
   first release after the move. Keep `ghcr.io/firestoned/banlieue` for the
   releases already published there, and note the new path in that
   release's notes. Signatures and provenance follow the new repository
   identity, so verification commands in the docs change with it.
3. **Keep the API group.** `banlieue.io` and `infrastructure.banlieue.io` do
   not change. Renaming an API group breaks every stored object and every
   manifest, for no user benefit. This includes the CAPI contract label
   `cluster.x-k8s.io/v1beta2`.
4. **Docs and links.** Move the documentation site to the new organisation's
   GitHub Pages, keeping a redirect from the old site. Update `site_url` and
   `repo_url` in `docs/mkdocs.yml`, and the absolute links in the root
   governance files.
5. **Shared CI actions.** Workflows use composite actions from
   `firestoned/github-actions`. Either keep them pinned by digest (they are
   public), or move them alongside the project.
6. **Identity.** Release signing is keyless, tied to the repository's
   workflow identity, so it changes with the transfer. Document the new
   identity in `SECURITY.md` before the first release from the new home.
