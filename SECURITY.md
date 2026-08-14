<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Security Policy

## Supported Versions

banlieue is pre-1.0 and under active development. Only the latest commit on
`main` and the most recent release receive security fixes.

| Version | Supported |
| ------- | --------- |
| latest release / `main` | ✅ |
| anything older | ❌ |

## Reporting a Vulnerability

**Do not open a public issue for a suspected vulnerability.**

Report it privately via GitHub's
[private vulnerability reporting](https://github.com/firestoned/banlieue/security/advisories/new)
("Report a vulnerability" on the repository's Security tab).

Include, where possible:

- the affected component (controller, operator, provider, imagebuilder, scripts) and version/commit;
- steps to reproduce or a proof of concept;
- the impact you believe it has.

You can expect an acknowledgement within **3 business days** and a triage
decision (accepted / needs-more-info / not-a-vulnerability) within **7**.
Accepted reports get a fix or mitigation plan, and credit in the release notes
unless you ask otherwise.

## Scope Notes

- The intended deployment trust model is documented in `deploy/admission/` and
  the ADRs under `docs/adr/` — issues that require already-cluster-admin
  privileges are generally out of scope.
- Supply-chain controls (SLSA provenance, SBOM, VEX) are described in
  [ADR-0006](docs/adr/0006-release-and-supply-chain-pipeline.md); known,
  justified non-issues live in `.vex/` and `deny.toml`.
