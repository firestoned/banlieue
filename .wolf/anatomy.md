# anatomy.md

> Auto-maintained by OpenWolf. Last scanned: 2026-05-26T03:05:55.968Z
> Files: 27 tracked | Anatomy hits: 0 | Misses: 0

## ./

- `Cargo.toml` — Rust package manifest (~270 tok)
- `deny.toml` — cargo-deny configuration (~730 tok)

## .claude/

- `CHANGELOG.md` — Changelog (~2994 tok)
- `CLAUDE.md` — Project Instructions for Claude Code (~2633 tok)
- `SKILL.md` — Claude Skills Reference (~2546 tok)

## .claude/rules/


## .github/actions/extract-version/

- `action.yml` — Composite action; emits version/tag/image-tag for pr|main|release events. Mirrored from 5-spot. (~1075 tok)

## .github/codeql/

- `codeql-config.yml` — CodeQL paths-ignore config consumed by codeql.yaml; excludes target/** and deploy/crds/**. (~192 tok)

## .github/workflows/

- `build.yaml` — SPDX-License-Identifier: Apache-2.0 (~4504 tok)
- `calm-test.yaml` — SPDX-License-Identifier: Apache-2.0 (~1952 tok)
- `calm.yaml` — Reusable workflow (workflow_call) wrapping @finos/calm-cli (validate|generate|template|docify). (~2023 tok)
- `codeql.yaml` — /*.rs (beta in CodeQL; stable enough for (~700 tok)
- `sast.yaml` — SPDX-License-Identifier: Apache-2.0 (~518 tok)
- `scorecard.yaml` — SPDX-License-Identifier: Apache-2.0 (~1094 tok)

## .wolf/


## .wolf/hooks/


## crates/banlieue-api/

- `Cargo.toml` — Rust package manifest (~207 tok)

## crates/banlieue-api/src/

- `common_tests.rs` — Unit tests for `common.rs`. (~5186 tok)
- `common.rs` — Common types shared across banlieue API groups. (~2178 tok)

## crates/banlieue-api/src/banlieue/

- `provider_tests.rs` — Unit tests for `provider.rs`. (~3221 tok)
- `provider.rs` — `banlieue.io/v1alpha1` Provider CRD. (~2374 tok)
- `virtualmachine_tests.rs` — Unit tests for `virtualmachine.rs`. (~3384 tok)
- `virtualmachine.rs` — `banlieue.io/v1alpha1` VirtualMachine CRD. (~2440 tok)
- `vmclass_tests.rs` — Unit tests for `vmclass.rs`. (~2399 tok)
- `vmclass.rs` — `banlieue.io/v1alpha1` VMClass CRD. (~984 tok)
- `vmimage_tests.rs` — Unit tests for `vmimage.rs`. (~2891 tok)
- `vmimage.rs` — `banlieue.io/v1alpha1` VMImage CRD. (~1533 tok)

## crates/banlieue-api/src/bin/


## crates/banlieue-api/src/infrastructure/

- `vsphere_machine_tests.rs` — Unit tests for `vsphere_machine.rs`. (~3084 tok)
- `vsphere_machine.rs` — `infrastructure.banlieue.io/v1alpha1` VSphereMachine CRD. (~2061 tok)

## docs/roadmap/


## examples/

- `03-vmclass-db-prod-large.yaml` — SPDX-License-Identifier: Apache-2.0 (~230 tok)
