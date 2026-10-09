<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Governance

This document describes how banlieue is run today and how that changes as the
project grows. It is written to be compatible with contribution to
[FINOS](https://www.finos.org/); once banlieue is a FINOS project, the FINOS
project governance and the FINOS Technical Oversight Committee's requirements
take precedence over anything here that conflicts with them.

## Mission

banlieue gives Kubernetes a single, provider-neutral API for virtual machines.
A user declares a `VirtualMachine`; banlieue places it on vSphere, Proxmox,
libvirt or Cloud Hypervisor through provider CRDs that satisfy the Cluster API
v1beta2 InfraMachine contract, so the same machines can also back Cluster API
clusters.

## Roles

**Users** run banlieue. They shape it through issues, discussions and
feedback.

**Contributors** submit changes: code, docs, ADRs, reviews, triage. Anyone can
contribute; see [CONTRIBUTING.md](https://github.com/firestoned/banlieue/blob/main/CONTRIBUTING.md).

**Maintainers** are listed in [MAINTAINERS.md](https://github.com/firestoned/banlieue/blob/main/MAINTAINERS.md). They:

- review and merge pull requests;
- decide on ADRs and on the roadmap;
- cut releases;
- triage security reports under [SECURITY.md](https://github.com/firestoned/banlieue/blob/main/SECURITY.md);
- enforce the [Code of Conduct](https://github.com/firestoned/banlieue/blob/main/CODE_OF_CONDUCT.md).

Path-level review ownership is recorded in
[`.github/CODEOWNERS`](https://github.com/firestoned/banlieue/blob/main/.github/CODEOWNERS).

## How decisions are made

- **Day to day: lazy consensus.** A pull request that has passed CI and
  maintainer review merges unless a maintainer objects. Once there are two or
  more maintainers, a change needs approval from a maintainer other than its
  author.
- **Architecture: ADRs.** Any architecturally significant change is proposed
  as an ADR in [`docs/adr/`](https://github.com/firestoned/banlieue/tree/main/docs/adr/) (status *Proposed*) and becomes
  *Accepted* when the maintainers agree. ADRs are never deleted; a reversed
  decision is marked *Superseded* and links forward.
- **When consensus fails: a vote.** Any maintainer may call a vote on the
  issue or pull request. It passes with a simple majority of all maintainers,
  open for at least five business days. With a single maintainer, that
  maintainer decides, and records the reasoning in the ADR or pull request.
- **Changes to this document** need a two-thirds majority of maintainers.

## Becoming a maintainer

A contributor becomes a maintainer when they have shown, over several months:

- sustained, high-quality contributions (code, reviews, docs or ADRs);
- sound judgement in review, including on security and API compatibility;
- that they work by the project's methodology (ADR-first, test-first) and its
  Code of Conduct.

Any maintainer may nominate a contributor. The nomination passes with a
two-thirds majority of the existing maintainers, and the new maintainer is
added to [MAINTAINERS.md](https://github.com/firestoned/banlieue/blob/main/MAINTAINERS.md) and `.github/CODEOWNERS` in the same
pull request.

A maintainer can step down at any time and becomes an *emeritus* maintainer.
A maintainer inactive for six months may be moved to emeritus by a two-thirds
majority of the others, after being asked.

## Releases and support

Releases are cut from `main` by a maintainer, tagged `vX.Y.Z`
([Semantic Versioning](https://semver.org/)), signed, and published with SBOMs
and provenance. The release changelog is generated from Conventional Commits.

Before 1.0, only the latest release and `main` are supported (see
[SECURITY.md](https://github.com/firestoned/banlieue/blob/main/SECURITY.md)). From 1.0, the latest minor release of the current
major version receives bug and security fixes, and the previous minor receives
security fixes for three months after its successor ships. Fixes land on
`main` first and are cherry-picked to a `release-X.Y` branch.

## Code of Conduct

Everyone taking part in the project follows the
[Code of Conduct](https://github.com/firestoned/banlieue/blob/main/CODE_OF_CONDUCT.md). Maintainers are responsible for
enforcing it.

## Contribution to FINOS

banlieue intends to be contributed to FINOS. The charter and the migration
plan are in the [governance section of the docs](https://firestoned.github.io/banlieue/governance/charter/).
