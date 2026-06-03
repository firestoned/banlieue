# anatomy.md

> Auto-maintained by OpenWolf. Last scanned: 2026-06-03T15:53:19.288Z
> Files: 248 tracked | Anatomy hits: 0 | Misses: 0

## ../../.claude/plans/

- `piped-wobbling-trinket.md` — Add `binaries/` to .gitignore (~144 tok)

## ../../.claude/projects/-Users-erick-dev-banlieue/memory/

- `MEMORY.md` — Memory index (~229 tok)
- `project_availability_zones.md` (~575 tok)
- `project_vsphere_byoc_tls.md` — Declares BYOC (~415 tok)

## ../roadmaps/banlieue/

- `README.md` — Project documentation (~694 tok)

## ../vim_rs/

- `CHANGELOG.md` — Changelog (~5601 tok)
- `README.md` — Project documentation (~8214 tok)

## ../vim_rs/vim_rs/

- `Cargo.toml` — Rust package manifest (~745 tok)

## ./

- `.gitignore` — Git ignore rules (~216 tok)
- `Cargo.toml` — Rust package manifest (~753 tok)
- `CLAUDE.md` — OpenWolf (~57 tok)
- `Cross.toml` — SPDX-License-Identifier: Apache-2.0 (~275 tok)
- `deny.toml` — cargo-deny configuration (~966 tok)
- `Dockerfile` — Docker container definition (~612 tok)
- `Dockerfile.chainguard` — SPDX-License-Identifier: Apache-2.0 (~423 tok)
- `LICENSE` — Project license (~3029 tok)
- `Makefile` — SPDX-License-Identifier: Apache-2.0 (~7481 tok)
- `README.md` — Project documentation (~1876 tok)

## .claude/

- `CHANGELOG.md` — Changelog (~30333 tok)
- `CLAUDE.md` — Project Instructions for Claude Code (~2903 tok)
- `settings.json` (~462 tok)
- `settings.local.json` (~408 tok)
- `SKILL.md` — Claude Skills Reference (~2613 tok)

## .claude/rules/

- `architecture-driven-development.md` — Architecture Driven Development (ADD) (~824 tok)
- `documentation.md` — Documentation Standards (~858 tok)
- `github-workflows.md` — GitHub Workflows & CI/CD Standards (~922 tok)
- `openwolf.md` (~313 tok)
- `rust-style.md` — Rust Style Guide (~2417 tok)
- `testing.md` — Testing Standards (~1549 tok)

## .github/actions/extract-version/

- `action.yml` — SPDX-License-Identifier: Apache-2.0 (~1075 tok)

## .github/actions/prepare-docker-binaries/

- `action.yml` — SPDX-License-Identifier: Apache-2.0 (~325 tok)

## .github/actions/vendor-vim-rs/

- `action.yml` — SPDX-License-Identifier: Apache-2.0 (~409 tok)

## .github/codeql/

- `codeql-config.yml` — , .github/, etc. (~192 tok)

## .github/scripts/

- `calm-args.bats` — SPDX-License-Identifier: Apache-2.0 (~1774 tok)
- `calm-args.sh` — SPDX-License-Identifier: Apache-2.0 (~787 tok)

## .github/workflows/

- `build.yaml` — SPDX-License-Identifier: Apache-2.0 (~11461 tok)
- `calm-test.yaml` — SPDX-License-Identifier: Apache-2.0 (~1952 tok)
- `calm.yaml` — SPDX-License-Identifier: Apache-2.0 (~2023 tok)
- `codeql.yaml` — /*.rs (beta in CodeQL; stable enough for (~802 tok)
- `docs.yaml` — SPDX-License-Identifier: Apache-2.0 (~3154 tok)
- `sast.yaml` — SPDX-License-Identifier: Apache-2.0 (~518 tok)
- `scorecard.yaml` — SPDX-License-Identifier: Apache-2.0 (~1094 tok)

## .vex/

- `.affected-functions.json` (~172 tok)
- `README.md` — Project documentation (~651 tok)

## crates/banlieue-api/

- `Cargo.toml` — Rust package manifest (~262 tok)

## crates/banlieue-api/src/

- `common_tests.rs` — Unit tests for `common.rs`. (~7707 tok)
- `common.rs` — Common types shared across banlieue API groups. (~3744 tok)
- `crddoc_tests.rs` — Tests for the CRD Markdown reference generator. (~1380 tok)
- `crddoc.rs` — Render banlieue CRDs as a single Markdown API-reference page. (~3153 tok)
- `crdgen_support_tests.rs` — Tests for the crdgen post-generation fix-ups. (~1257 tok)
- `crdgen_support.rs` — Post-generation fix-ups applied to CRDs by the `crdgen` binary. (~1041 tok)
- `lib.rs` — API types and CRD generation for **banlieue**, a Kubernetes-native (~502 tok)

## crates/banlieue-api/src/banlieue/

- `mod.rs` — `banlieue.io/v1alpha1` API group. (~250 tok)
- `provider_tests.rs` — Unit tests for `provider.rs`. (~3256 tok)
- `provider.rs` — `banlieue.io/v1alpha1` Provider CRD. (~3138 tok)
- `virtualmachine_tests.rs` — Unit tests for `virtualmachine.rs`. (~3646 tok)
- `virtualmachine.rs` — `banlieue.io/v1alpha1` VirtualMachine CRD. (~3264 tok)
- `vmclass_tests.rs` — Unit tests for `vmclass.rs`. (~2570 tok)
- `vmclass.rs` — `banlieue.io/v1alpha1` VMClass CRD. (~1704 tok)
- `vmimage_tests.rs` — Unit tests for `vmimage.rs`. (~3122 tok)
- `vmimage.rs` — `banlieue.io/v1alpha1` VMImage CRD. (~2261 tok)

## crates/banlieue-api/src/bin/

- `crddoc.rs` — Generate the Markdown API reference for every banlieue CRD. (~694 tok)
- `crdgen.rs` — Emit every banlieue CRD as YAML. (~1040 tok)

## crates/banlieue-api/src/infrastructure/

- `mod.rs` — `infrastructure.banlieue.io/v1alpha1` API group. (~203 tok)
- `vsphere_cluster_tests.rs` — Unit tests for `vsphere_cluster.rs`. (~2185 tok)
- `vsphere_cluster.rs` — `infrastructure.banlieue.io/v1alpha1` VSphereCluster CRD. (~1989 tok)
- `vsphere_machine_tests.rs` — Unit tests for `vsphere_machine.rs`. (~3304 tok)
- `vsphere_machine.rs` — `infrastructure.banlieue.io/v1alpha1` VSphereMachine CRD. (~2795 tok)

## crates/banlieue-controller/

- `Cargo.toml` — Rust package manifest (~348 tok)

## crates/banlieue-controller/src/

- `app_tests.rs` — Unit tests for [`super::super::app`]. (~584 tok)
- `app.rs` — # `banlieue controller` entry point (~2942 tok)
- `context.rs` — Shared reconcile context — the only value that all reconcilers receive. (~238 tok)
- `error.rs` — Typed errors for the main controller. (~302 tok)
- `lib.rs` — # banlieue-controller (~269 tok)

## crates/banlieue-controller/src/reconciler/

- `infra_tests.rs` — Unit tests for [`super::super::infra`]. (~3060 tok)
- `infra.rs` — Build provider-specific infrastructure CRs from a scheduler [`Decision`]. (~2178 tok)
- `migration_tests.rs` — Unit tests for [`super::super::migration`]. (~2892 tok)
- `migration.rs` — Migration sub-loop — recreate-only path for Phase 1A iteration 3. (~2242 tok)
- `mod.rs` — Controller reconcilers. (~110 tok)
- `scheduler_tests.rs` — Unit tests for [`super::super::scheduler`]. (~7583 tok)
- `scheduler.rs` — Scheduler — the pure placement function. (~5386 tok)
- `status_mirror_tests.rs` — Unit tests for [`super::super::status_mirror`]. (~2357 tok)
- `status_mirror.rs` — `VirtualMachine` status mirror. (~1777 tok)
- `virtualmachine_tests.rs` — Unit tests for [`super::super::virtualmachine`]. (~276 tok)
- `virtualmachine.rs` — `VirtualMachine` reconciler — Phase 1A iteration 2. (~4498 tok)
- `vsphere_cluster_tests.rs` — Unit tests for [`super::super::vsphere_cluster`]. (~3271 tok)
- `vsphere_cluster.rs` — `VSphereCluster` reconciler — CAPI InfraCluster failure-domain aggregation. (~2840 tok)

## crates/banlieue-provider-sdk/

- `Cargo.toml` — Rust package manifest (~296 tok)

## crates/banlieue-provider-sdk/src/

- `bootstrap_tests.rs` — Unit tests for [`super::super::bootstrap`]. (~381 tok)
- `bootstrap.rs` — Shared process bootstrap helpers. (~1694 tok)
- `client.rs` — Kubernetes client construction with timeouts. (~412 tok)
- `error.rs` — Shared error type for the SDK. (~459 tok)
- `finalizer_tests.rs` — Unit tests for [`super::super::finalizer`]. (~533 tok)
- `finalizer.rs` — Patch-based finalizer add and remove helpers. (~910 tok)
- `leader_tests.rs` — Unit tests for [`super::super::leader`]. (~1738 tok)
- `leader.rs` — Lease-based leader election for banlieue controllers. (~3644 tok)
- `lib.rs` — # banlieue-provider-sdk (~390 tok)
- `reconciler_tests.rs` — Unit tests for [`super::super::reconciler`]. (~302 tok)
- `reconciler.rs` — Small helpers around [`kube::runtime::controller::Action`]. (~457 tok)
- `ssa.rs` — Server-side apply helper. (~620 tok)
- `status_tests.rs` — Unit tests for [`super::super::status`]. (~1014 tok)
- `status.rs` — Helpers for managing `metav1.Condition` lists on CR status. (~965 tok)

## crates/banlieue-provider-vsphere/

- `Cargo.toml` — Rust package manifest (~641 tok)

## crates/banlieue-provider-vsphere/src/

- `app_tests.rs` — Unit tests for [`super::super::app`]. (~561 tok)
- `app.rs` — # `banlieue provider vsphere` entry point (~2313 tok)
- `context.rs` — Shared reconcile context for the vSphere provider. (~366 tok)
- `error.rs` — Typed errors for the vSphere provider's reconcilers. (~351 tok)
- `lib.rs` — # banlieue-provider-vsphere (~321 tok)

## crates/banlieue-provider-vsphere/src/client/

- `fake.rs` — In-memory `VSphereClient` used by reconciler tests. (~1266 tok)
- `mod.rs` — vSphere client surface used by the reconcilers. (~1140 tok)
- `vim_tests.rs` — Unit tests for the BYOC HTTP-client helpers in `vim.rs` (ADR-0008). (~1171 tok)
- `vim.rs` — Production `VSphereClient` implementation backed by `vim_rs`. (~3158 tok)

## crates/banlieue-provider-vsphere/src/reconciler/

- `ca_bundle_tests.rs` — Unit tests for caBundle classification (`plan`) in `ca_bundle.rs`. (~600 tok)
- `ca_bundle.rs` — Resolve `Provider.spec.connection.caBundle` to PEM text (ADR-0008). (~1420 tok)
- `mod.rs` — vSphere provider reconcilers. (~103 tok)
- `provider_tests.rs` — Unit tests for [`super::super::provider`]. (~1351 tok)
- `provider.rs` — `Provider` reconciler — capability introspection against vCenter. (~3731 tok)
- `vmimage_tests.rs` — Unit tests for [`super::super::vmimage`]. (~3164 tok)
- `vmimage.rs` — `VMImage` reconciler — template-availability check on vSphere. (~4024 tok)

## crates/banlieue-vex/

- `Cargo.toml` — Rust package manifest (~197 tok)

## crates/banlieue-vex/src/

- `auto_vex_presence_tests.rs` — Unit tests for the `auto_vex_presence` module. (~4900 tok)
- `auto_vex_presence.rs` — Presence-based auto-VEX generation. (~2143 tok)
- `auto_vex_reachability_tests.rs` — Unit tests for the `auto_vex_reachability` module. (~3297 tok)
- `auto_vex_reachability.rs` — Symbol-import-based auto-VEX. (~2101 tok)
- `lib.rs` — # banlieue-vex (~238 tok)

## crates/banlieue-vex/src/bin/

- `auto_vex_presence.rs` — # Presence-based auto-VEX generator (CI tool) (~1258 tok)
- `auto_vex_reachability.rs` — # Symbol-import reachability auto-VEX (CI tool) (~1378 tok)

## crates/banlieue/

- `Cargo.toml` — Rust package manifest (~355 tok)

## crates/banlieue/src/

- `cli_tests.rs` — Unit tests for the unified `banlieue` CLI dispatch tree. (~884 tok)
- `cli.rs` — Top-level command-line interface for the unified `banlieue` binary. (~1018 tok)
- `main.rs` — # banlieue (~228 tok)

## deploy/admission/

- `provider-cabundle-source.yaml` — SPDX-License-Identifier: Apache-2.0 (~583 tok)
- `provider-immutability.yaml` — SPDX-License-Identifier: Apache-2.0 (~413 tok)
- `README.md` — Project documentation (~349 tok)
- `virtualmachine-immutability.yaml` — SPDX-License-Identifier: Apache-2.0 (~526 tok)

## deploy/controller/

- `configmap.yaml` — SPDX-License-Identifier: Apache-2.0 (~122 tok)
- `deployment.yaml` — SPDX-License-Identifier: Apache-2.0 (~892 tok)
- `namespace.yaml` — SPDX-License-Identifier: Apache-2.0 (~134 tok)
- `service.yaml` — SPDX-License-Identifier: Apache-2.0 (~161 tok)

## deploy/controller/rbac/

- `clusterrole.yaml` — SPDX-License-Identifier: Apache-2.0 (~1008 tok)
- `clusterrolebinding.yaml` — SPDX-License-Identifier: Apache-2.0 (~135 tok)
- `serviceaccount.yaml` — SPDX-License-Identifier: Apache-2.0 (~80 tok)

## deploy/crds/

- `banlieue.io_providers.yaml` — K8s CustomResourceDefinition: providers.banlieue.io (~4554 tok)
- `banlieue.io_virtualmachines.yaml` — K8s CustomResourceDefinition: virtualmachines.banlieue.io (~5722 tok)
- `banlieue.io_vmclasses.yaml` — K8s CustomResourceDefinition: vmclasses.banlieue.io (~3645 tok)
- `banlieue.io_vmimages.yaml` — K8s CustomResourceDefinition: vmimages.banlieue.io (~3400 tok)
- `infrastructure.banlieue.io_vsphereclusters.yaml` — K8s CustomResourceDefinition: vsphereclusters.infrastructure.banlieue.io (~3914 tok)
- `infrastructure.banlieue.io_vspheremachines.yaml` — K8s CustomResourceDefinition: vspheremachines.infrastructure.banlieue.io (~4891 tok)
- `infrastructure.banlieue.io_vspheremachinetemplates.yaml` — K8s CustomResourceDefinition: vspheremachinetemplates.infrastructure.banlieue.io (~3326 tok)

## deploy/kind/

- `cluster.yaml` — SPDX-License-Identifier: Apache-2.0 (~149 tok)

## deploy/provider-vsphere/

- `configmap.yaml` — SPDX-License-Identifier: Apache-2.0 (~162 tok)
- `deployment.yaml` — SPDX-License-Identifier: Apache-2.0 (~890 tok)
- `README.md` — Project documentation (~1655 tok)
- `service.yaml` — SPDX-License-Identifier: Apache-2.0 (~166 tok)

## deploy/provider-vsphere/rbac/

- `clusterrole.yaml` — SPDX-License-Identifier: Apache-2.0 (~1025 tok)
- `clusterrolebinding.yaml` — SPDX-License-Identifier: Apache-2.0 (~142 tok)
- `serviceaccount.yaml` — SPDX-License-Identifier: Apache-2.0 (~84 tok)

## docs/

- `.gitignore` — Git ignore rules (~37 tok)
- `.python-version` (~2 tok)
- `mkdocs.yml` — SPDX-License-Identifier: Apache-2.0 (~1492 tok)
- `pyproject.toml` — Python project configuration (~254 tok)
- `README.md` — Project documentation (~470 tok)

## docs/adr/

- `0001-capi-native-cluster-provisioning.md` — 0001 — CAPI-native cluster provisioning (no native cluster/tier abstraction) (~1178 tok)
- `0002-infracluster-failure-domain-aggregation.md` — 0002 — InfraCluster CRD with multi-Provider failure-domain aggregation (~1837 tok)
- `0003-provider-deployment-topology.md` — 0003 — Provider deployment topology (per-instance vs per-class) (~769 tok)
- `0004-single-binary-subcommand-dispatch.md` — 0004 — Single `banlieue` binary with subcommand dispatch (~1459 tok)
- `0005-capi-contract-label-codegen.md` — 0005 — Emit the CAPI contract label from crdgen (not kustomize) (~1025 tok)
- `0006-release-and-supply-chain-pipeline.md` — 0006 — Release artifacts and supply-chain pipeline (~1737 tok)
- `0007-admission-policies.md` — 0007 — ValidatingAdmissionPolicies for CRD invariants (~1527 tok)
- `0008-byoc-vsphere-http-client.md` — 0008 — Bring-Your-Own-Client (BYOC) for the vSphere HTTP transport (~3752 tok)
- `0009-vim-rs-0.5-rustls-ring-retire-vendoring.md` — 0009 — Adopt vim_rs 0.5 (rustls/ring, BYOC); retire the vendoring pipeline (~2110 tok)

## docs/architecture/calm/

- `architecture.json` — Declares by (~12132 tok)
- `README.md` — Project documentation (~999 tok)

## docs/architecture/calm/templates/mermaid/

- `flows.md.hbs` — Architecture Flows (~352 tok)
- `system.md.hbs` — System Architecture (~477 tok)

## docs/site/

- `404.html` — banlieue — Kubernetes-Native Abstract Virtualization API (~8789 tok)
- `index.html` — banlieue - banlieue — Kubernetes-Native Abstract Virtualization API (~11901 tok)
- `sitemap.xml` (~942 tok)

## docs/site/architecture/

- `index.html` — Overview - banlieue — Kubernetes-Native Abstract Virtualization API (~12163 tok)

## docs/site/architecture/flows/

- `index.html` — Architecture Flows - banlieue — Kubernetes-Native Abstract Virtualization API (~12439 tok)

## docs/site/architecture/system/

- `index.html` — System Diagram - banlieue — Kubernetes-Native Abstract Virtualization API (~10473 tok)

## docs/site/assets/javascripts/

- `bundle.79ae519e.min.js.map` (~273864 tok)

## docs/site/assets/javascripts/lunr/

- `tinyseg.js` — export the module via AMD, CommonJS or as a browser global (~5698 tok)
- `wordcut.js` — e: s (~110353 tok)

## docs/site/assets/javascripts/workers/

- `search.2c215733.min.js.map` — \n * lunr - http://lunrjs.com - A bit like Solr, but much smaller and not as bright - 2.3.9\n * Copyright (C) 2020 Oliver Nightingale\n * @license ... (~57608 tok)

## docs/site/assets/stylesheets/

- `main.484c7ddc.min.css.map` (~12691 tok)
- `palette.ab4e12ef.min.css.map` (~979 tok)

## docs/site/concepts/

- `index.html` — Overview - banlieue — Kubernetes-Native Abstract Virtualization API (~9528 tok)

## docs/site/concepts/architecture/

- `index.html` — Architecture - banlieue — Kubernetes-Native Abstract Virtualization API (~12238 tok)

## docs/site/concepts/infra-crds-capi/

- `index.html` — Infrastructure CRDs & CAPI - banlieue — Kubernetes-Native Abstract Virtualization API (~13273 tok)

## docs/site/concepts/providers/

- `index.html` — Provider Model - banlieue — Kubernetes-Native Abstract Virtualization API (~14297 tok)

## docs/site/concepts/virtualmachine/

- `index.html` — VirtualMachine - banlieue — Kubernetes-Native Abstract Virtualization API (~12142 tok)

## docs/site/css/

- `timeago.css` — Styles: 2 rules, 1 media queries (~112 tok)

## docs/site/getting-started/quickstart/

- `index.html` — Getting Started - banlieue — Kubernetes-Native Abstract Virtualization API (~11362 tok)

## docs/site/getting-started/vsphere-provider/

- `index.html` — vSphere Provider - banlieue — Kubernetes-Native Abstract Virtualization API (~19359 tok)

## docs/site/javascripts/

- `mermaid-init.js` — SPDX-License-Identifier: Apache-2.0 (~693 tok)

## docs/site/js/

- `timeago_mkdocs_material.js` — Script to ensure timeago keeps working when (~255 tok)

## docs/site/overview/

- `index.html` — Overview - banlieue — Kubernetes-Native Abstract Virtualization API (~12930 tok)

## docs/site/reasoning/

- `index.html` — Overview - banlieue — Kubernetes-Native Abstract Virtualization API (~9912 tok)

## docs/site/reasoning/abstraction-principle/

- `index.html` — The Abstraction Principle - banlieue — Kubernetes-Native Abstract Virtualization API (~13038 tok)

## docs/site/reasoning/capi-relationship/

- `index.html` — Relationship to Cluster API - banlieue — Kubernetes-Native Abstract Virtualization API (~12969 tok)

## docs/site/reasoning/comparisons/

- `index.html` — Comparisons - banlieue — Kubernetes-Native Abstract Virtualization API (~13311 tok)

## docs/site/reasoning/crd-only-contract/

- `index.html` — CRD-Only Contract - banlieue — Kubernetes-Native Abstract Virtualization API (~13422 tok)

## docs/site/reasoning/least-touch/

- `index.html` — Least-Touch Workflow - banlieue — Kubernetes-Native Abstract Virtualization API (~15046 tok)

## docs/site/reasoning/non-goals/

- `index.html` — Non-Goals - banlieue — Kubernetes-Native Abstract Virtualization API (~13146 tok)

## docs/site/reasoning/problem/

- `index.html` — The Problem - banlieue — Kubernetes-Native Abstract Virtualization API (~12412 tok)

## docs/site/reference/api/

- `index.html` — API Reference (CRDs) - banlieue — Kubernetes-Native Abstract Virtualization API (~37349 tok)

## docs/site/reference/license/

- `index.html` — License - banlieue — Kubernetes-Native Abstract Virtualization API (~10350 tok)

## docs/site/reference/roadmap/

- `index.html` — Roadmap - banlieue — Kubernetes-Native Abstract Virtualization API (~11673 tok)

## docs/site/search/

- `search_index.json` — Declares system (~54388 tok)

## docs/site/stylesheets/

- `extra.css` — banlieue Documentation - Custom Styles for MkDocs Material (~1274 tok)

## docs/src/

- `index.md` — banlieue (~1104 tok)
- `overview.md` — Overview (~1536 tok)

## docs/src/architecture/

- `flows.md` — Architecture Flows (~1838 tok)
- `index.md` — Architecture (CALM) (~1315 tok)
- `system.md` — System Architecture (~985 tok)

## docs/src/concepts/

- `architecture.md` — Architecture (~1332 tok)
- `index.md` — Concepts (~142 tok)
- `infra-crds-capi.md` — Infrastructure CRDs & CAPI (~1726 tok)
- `providers.md` — Provider Model (~1867 tok)
- `virtualmachine.md` — VirtualMachine (~804 tok)

## docs/src/developer/

- `index.md` — Developer (~254 tok)
- `local-development.md` — Local Development (~1480 tok)

## docs/src/getting-started/

- `quickstart.md` — Quick Start (~699 tok)
- `vsphere-provider.md` — vSphere Provider (~2208 tok)

## docs/src/guides/

- `core-controller.md` — Guide: Core Controller (~1917 tok)
- `index.md` — Guides (~413 tok)
- `vsphere-provider.md` — Guide: vSphere Provider (~2450 tok)

## docs/src/javascripts/

- `mermaid-init.js` — SPDX-License-Identifier: Apache-2.0 (~693 tok)

## docs/src/reasoning/

- `abstraction-principle.md` — The abstraction principle (~1550 tok)
- `capi-relationship.md` — Relationship to Cluster API (CAPI / CAPM) (~2261 tok)
- `comparisons.md` — Comparisons (~2180 tok)
- `crd-only-contract.md` — CRD-only contract (~1714 tok)
- `index.md` — Why banlieue? (~480 tok)
- `least-touch.md` — Least-touch workflow (~1540 tok)
- `non-goals.md` — Non-goals (~1328 tok)
- `problem.md` — The problem (~1211 tok)

## docs/src/reference/

- `api.md` — API Reference (~14696 tok)
- `license.md` — License (~250 tok)
- `roadmap.md` — Roadmap (~635 tok)

## docs/src/stylesheets/

- `extra.css` — banlieue Documentation - Custom Styles for MkDocs Material (~1274 tok)

## examples/

- `01-provider-vsphere-dc1.yaml` — SPDX-License-Identifier: Apache-2.0 (~706 tok)
- `02-provider-libvirt-edge.yaml` — SPDX-License-Identifier: Apache-2.0 (~292 tok)
- `03-vmclass-db-prod-large.yaml` — SPDX-License-Identifier: Apache-2.0 (~230 tok)
- `04-vmimage-ubuntu.yaml` — SPDX-License-Identifier: Apache-2.0 (~282 tok)
- `05-virtualmachine.yaml` — SPDX-License-Identifier: Apache-2.0 (~402 tok)
- `06-vspherecluster-multi-vcenter.yaml` — SPDX-License-Identifier: Apache-2.0 (~666 tok)

## patches/

- `README.md` — Project documentation (~901 tok)
