<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# `.vex/` — OpenVEX statements

This directory holds **curated, hand-authored** [OpenVEX](https://openvex.dev/)
statements that suppress (or annotate) container/binary CVEs with a documented
justification. The release pipeline merges every `.vex/*.json` here into one
OpenVEX document, attests it to each image digest with Cosign, and feeds it to
the Grype scan (`grype --vex`) so justified findings do not re-alarm. See
[ADR-0006](../docs/adr/0006-release-and-supply-chain-pipeline.md).

## Files

- **`*.json`** — one curated OpenVEX document per advisory. Merged by
  `vexctl merge` in CI (the `build-vex` job) and locally via `make vex-assemble`.
  Validate with `make vex-validate`.
- **`.affected-functions.json`** — dot-prefixed (so the `*.json` glob skips it).
  A curated `CVE → [library symbol names]` map for the *future*
  `auto-vex-reachability` tool (ADR-0006 "Staged"). Not a VEX document.
- **`.gitkeep`** — keeps the directory tracked when no curated statements exist.

When there are no curated statements, CI emits a valid **empty** OpenVEX
document, so the pipeline works from day one.

## Statement shape

```json
{
  "@context": "https://openvex.dev/ns/v0.2.0",
  "@id": "https://banlieue/vex/CVE-XXXX-NNNN",
  "author": "Erick Bourgeois",
  "timestamp": "2026-01-01T00:00:00Z",
  "version": 1,
  "statements": [
    {
      "vulnerability": { "name": "CVE-XXXX-NNNN" },
      "products": [{ "@id": "pkg:oci/banlieue" }],
      "status": "not_affected",
      "justification": "vulnerable_code_not_in_execute_path",
      "impact_statement": "Why banlieue is not affected.",
      "timestamp": "2026-01-01T00:00:00Z"
    }
  ]
}
```

The document-level `@id` / `author` / `timestamp` are replaced by CI at merge
time; statement-level fields ship as-authored.

- **`status`**: `not_affected` | `affected` | `fixed` | `under_investigation`.
- **`justification`** (required when `not_affected`): `component_not_present`,
  `vulnerable_code_not_present`, `vulnerable_code_not_in_execute_path`,
  `vulnerable_code_cannot_be_controlled_by_adversary`,
  `inline_mitigations_already_exist`.
- **`products[].@id`**: the product purl — `pkg:oci/banlieue`.
- Accepted vulnerability id shapes: `CVE-…`, `GHSA-…`, `RUSTSEC-…`.

## Adding a suppression

1. Create `.vex/<ADVISORY>.json` following the shape above with an honest
   `justification` and `impact_statement`.
2. Run `make vex-validate` to confirm it parses and merges.
3. On merge to `main` / release, CI assembles, attests, and applies it.
