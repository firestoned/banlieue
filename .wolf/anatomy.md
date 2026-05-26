# anatomy.md

> Auto-maintained by OpenWolf. Last scanned: 2026-05-26T20:33:08.466Z
> Files: 109 tracked | Anatomy hits: 0 | Misses: 0

## ../../.claude/projects/-Users-erick-dev-banlieue/memory/

- `feedback_least_privilege.md` (~880 tok)
- `MEMORY.md` — Memory index (~55 tok)

## ../roadmaps/banlieue/

- `14-PHASE-1E-DOCS.md` — Phase 1E — Documentation site (MkDocs Material) (~3258 tok)
- `README.md` — Project documentation (~650 tok)

## ./

- `.gitignore` — Git ignore rules (~205 tok)
- `Cargo.toml` — Rust package manifest (~328 tok)
- `deny.toml` — cargo-deny configuration (~730 tok)
- `Dockerfile` — Docker container definition (~545 tok)
- `Dockerfile.chainguard` — SPDX-License-Identifier: Apache-2.0 (~364 tok)
- `Makefile` — SPDX-License-Identifier: Apache-2.0 (~4987 tok)

## .claude/

- `CHANGELOG.md` — Changelog (~13967 tok)
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
- `docs.yaml` — SPDX-License-Identifier: Apache-2.0 (~2995 tok)
- `sast.yaml` — SPDX-License-Identifier: Apache-2.0 (~518 tok)
- `scorecard.yaml` — SPDX-License-Identifier: Apache-2.0 (~1094 tok)

## .wolf/


## .wolf/hooks/


## crates/banlieue-api/

- `Cargo.toml` — Rust package manifest (~222 tok)

## crates/banlieue-api/src/

- `common_tests.rs` — Unit tests for `common.rs`. (~5343 tok)
- `common.rs` — Common types shared across banlieue API groups. (~2248 tok)

## crates/banlieue-api/src/banlieue/

- `provider_tests.rs` — Unit tests for `provider.rs`. (~3227 tok)
- `provider.rs` — `banlieue.io/v1alpha1` Provider CRD. (~2456 tok)
- `virtualmachine_tests.rs` — Unit tests for `virtualmachine.rs`. (~3403 tok)
- `virtualmachine.rs` — `banlieue.io/v1alpha1` VirtualMachine CRD. (~2467 tok)
- `vmclass_tests.rs` — Unit tests for `vmclass.rs`. (~2399 tok)
- `vmclass.rs` — `banlieue.io/v1alpha1` VMClass CRD. (~984 tok)
- `vmimage_tests.rs` — Unit tests for `vmimage.rs`. (~2891 tok)
- `vmimage.rs` — `banlieue.io/v1alpha1` VMImage CRD. (~1533 tok)

## crates/banlieue-api/src/bin/

- `crdgen.rs` — Emit every banlieue CRD as YAML. (~876 tok)

## crates/banlieue-api/src/infrastructure/

- `vsphere_machine_tests.rs` — Unit tests for `vsphere_machine.rs`. (~3084 tok)
- `vsphere_machine.rs` — `infrastructure.banlieue.io/v1alpha1` VSphereMachine CRD. (~2061 tok)

## crates/banlieue-controller/

- `Cargo.toml` — Rust package manifest (~260 tok)

## crates/banlieue-controller/src/

- `context.rs` — Shared reconcile context — the only value that all reconcilers receive. (~222 tok)
- `error.rs` — Typed errors for the main controller. (~282 tok)
- `lib.rs` — # banlieue-controller (~199 tok)
- `main.rs` — # banlieue-controller entry point (~2772 tok)

## crates/banlieue-controller/src/reconciler/

- `infra_tests.rs` — Unit tests for [`super::super::infra`]. (~2856 tok)
- `infra.rs` — Build provider-specific infrastructure CRs from a scheduler [`Decision`]. (~2033 tok)
- `migration_tests.rs` — Unit tests for [`super::super::migration`]. (~2667 tok)
- `migration.rs` — Migration sub-loop — recreate-only path for Phase 1A iteration 3. (~2101 tok)
- `mod.rs` — Controller reconcilers. (~96 tok)
- `scheduler_tests.rs` — Unit tests for [`super::super::scheduler`]. (~6549 tok)
- `scheduler.rs` — Scheduler — the pure placement function. (~4988 tok)
- `status_mirror_tests.rs` — Unit tests for [`super::super::status_mirror`]. (~2200 tok)
- `status_mirror.rs` — `VirtualMachine` status mirror. (~1639 tok)
- `virtualmachine_tests.rs` — Unit tests for [`super::super::virtualmachine`]. (~258 tok)
- `virtualmachine.rs` — `VirtualMachine` reconciler — Phase 1A iteration 2. (~4199 tok)

## crates/banlieue-provider-sdk/

- `Cargo.toml` — Rust package manifest (~204 tok)

## crates/banlieue-provider-sdk/src/

- `client.rs` — Kubernetes client construction with timeouts. (~384 tok)
- `error.rs` — Shared error type for the SDK. (~428 tok)
- `finalizer_tests.rs` — Unit tests for [`super::super::finalizer`]. (~497 tok)
- `finalizer.rs` — Patch-based finalizer add and remove helpers. (~850 tok)
- `leader_tests.rs` — Unit tests for [`super::super::leader`]. (~1614 tok)
- `leader.rs` — Lease-based leader election for banlieue controllers. (~3401 tok)
- `lib.rs` — # banlieue-provider-sdk (~324 tok)
- `reconciler_tests.rs` — Unit tests for [`super::super::reconciler`]. (~282 tok)
- `reconciler.rs` — Small helpers around [`kube::runtime::controller::Action`]. (~426 tok)
- `ssa.rs` — Server-side apply helper. (~579 tok)
- `status_tests.rs` — Unit tests for [`super::super::status`]. (~940 tok)
- `status.rs` — Helpers for managing `metav1.Condition` lists on CR status. (~901 tok)

## deploy/controller/

- `configmap.yaml` — SPDX-License-Identifier: Apache-2.0 (~122 tok)
- `deployment.yaml` — SPDX-License-Identifier: Apache-2.0 (~863 tok)
- `namespace.yaml` — SPDX-License-Identifier: Apache-2.0 (~134 tok)
- `service.yaml` — SPDX-License-Identifier: Apache-2.0 (~161 tok)

## deploy/controller/rbac/

- `clusterrole.yaml` — SPDX-License-Identifier: Apache-2.0 (~886 tok)
- `clusterrolebinding.yaml` — SPDX-License-Identifier: Apache-2.0 (~135 tok)
- `serviceaccount.yaml` — SPDX-License-Identifier: Apache-2.0 (~80 tok)

## deploy/kind/

- `cluster.yaml` — SPDX-License-Identifier: Apache-2.0 (~149 tok)

## docs/

- `.gitignore` — Git ignore rules (~37 tok)
- `.python-version` (~2 tok)
- `mkdocs.yml` — SPDX-License-Identifier: Apache-2.0 (~1421 tok)
- `pyproject.toml` — SPDX-License-Identifier: Apache-2.0 (~237 tok)
- `README.md` — Project documentation (~468 tok)

## docs/architecture/calm/

- `architecture.json` — Declares by (~7752 tok)
- `README.md` — Project documentation (~999 tok)

## docs/architecture/calm/templates/mermaid/

- `flows.md.hbs` — Architecture Flows (~352 tok)
- `system.md.hbs` — System Architecture (~477 tok)

## docs/roadmap/


## docs/src/

- `index.md` — banlieue (~1012 tok)
- `overview.md` — Overview (~1550 tok)

## docs/src/architecture/

- `flows.md` — Architecture Flows (CALM) (~253 tok)
- `index.md` — Architecture (CALM) (~1314 tok)
- `system.md` — System Diagram (CALM) (~152 tok)

## docs/src/concepts/

- `architecture.md` — Architecture (~1168 tok)
- `index.md` — Concepts (~142 tok)
- `infra-crds-capi.md` — Infrastructure CRDs & CAPI (~1133 tok)
- `providers.md` — Provider Model (~1309 tok)
- `virtualmachine.md` — VirtualMachine (~807 tok)

## docs/src/getting-started/

- `quickstart.md` — Quick Start (~491 tok)

## docs/src/javascripts/

- `mermaid-init.js` — SPDX-License-Identifier: Apache-2.0 (~693 tok)

## docs/src/reasoning/

- `abstraction-principle.md` — The abstraction principle (~1550 tok)
- `capi-relationship.md` — Relationship to Cluster API (CAPI / CAPM) (~1700 tok)
- `comparisons.md` — Comparisons (~2180 tok)
- `crd-only-contract.md` — CRD-only contract (~1714 tok)
- `index.md` — Why banlieue? (~480 tok)
- `least-touch.md` — Least-touch workflow (~1540 tok)
- `non-goals.md` — Non-goals (~1346 tok)
- `problem.md` — The problem (~1211 tok)

## docs/src/reference/

- `license.md` — License (~250 tok)
- `roadmap.md` — Roadmap (~635 tok)

## docs/src/stylesheets/

- `extra.css` — banlieue Documentation - Custom Styles for MkDocs Material (~1274 tok)

## examples/

- `01-provider-vsphere-dc1.yaml` — SPDX-License-Identifier: Apache-2.0 (~529 tok)
- `02-provider-libvirt-edge.yaml` — SPDX-License-Identifier: Apache-2.0 (~292 tok)
- `03-vmclass-db-prod-large.yaml` — SPDX-License-Identifier: Apache-2.0 (~230 tok)
- `05-virtualmachine.yaml` — SPDX-License-Identifier: Apache-2.0 (~402 tok)
