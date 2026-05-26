# Memory

> Chronological action log. Hooks and AI append to this file automatically.
> Old sessions are consolidated by the daemon weekly.

## Session: 2026-05-24

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 17:58 | Created CLAUDE.md | — | ~60 |
| 18:00 | Created .claude/CLAUDE.md | — | ~2734 |
| 18:01 | Created .claude/SKILL.md | — | ~2669 |

## Session: 2026-05-24 18:19

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-05-24 18:22

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 19:13 | Edited Cargo.toml | 2→2 lines | ~23 |
| 19:14 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | 2→1 lines | ~17 |

## Session: 2026-05-24 19:15

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 19:15 | Edited crates/banlieue-api/Cargo.toml | 14→15 lines | ~101 |
| 19:16 | Edited crates/banlieue-api/src/common.rs | 11→14 lines | ~140 |
| 19:17 | Created crates/banlieue-api/src/common_tests.rs | — | ~4607 |
| 19:18 | Edited crates/banlieue-api/src/common.rs | 2→3 lines | ~16 |
| 19:18 | Edited crates/banlieue-api/src/common_tests.rs | 2→2 lines | ~16 |
| 19:18 | Edited crates/banlieue-api/src/common_tests.rs | modified disk_provisioning_all_variants_round_trip() | ~255 |
| 19:18 | Edited crates/banlieue-api/src/banlieue/provider.rs | modified is_false() | ~31 |
| 19:19 | Created crates/banlieue-api/src/banlieue/provider_tests.rs | — | ~3204 |
| 19:19 | Edited crates/banlieue-api/src/banlieue/provider_tests.rs | 5→5 lines | ~52 |
| 19:20 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | modified is_false() | ~34 |
| 19:20 | Created crates/banlieue-api/src/banlieue/virtualmachine_tests.rs | — | ~3391 |
| 19:21 | Edited crates/banlieue-api/src/banlieue/vmclass.rs | 4→8 lines | ~59 |
| 19:21 | Created crates/banlieue-api/src/banlieue/vmclass_tests.rs | — | ~2413 |
| 19:22 | Edited crates/banlieue-api/src/banlieue/provider_tests.rs | 5→4 lines | ~45 |
| 19:22 | Edited crates/banlieue-api/src/banlieue/virtualmachine_tests.rs | 5→4 lines | ~42 |
| 19:22 | Edited crates/banlieue-api/src/banlieue/vmclass_tests.rs | 2→1 lines | ~7 |
| 19:22 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | 4→8 lines | ~52 |
| 19:23 | Created crates/banlieue-api/src/banlieue/vmimage_tests.rs | — | ~2891 |
| 19:23 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine.rs | 5→9 lines | ~71 |
| 19:24 | Created crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs | — | ~3050 |
| 19:24 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs | 2→1 lines | ~7 |
| 19:25 | Edited crates/banlieue-api/src/banlieue/vmclass.rs | 3→4 lines | ~62 |
| 19:30 | Comprehensive unit tests added | common+4 banlieue+vsphere `_tests.rs` files | 139 passing | ~16000 |
| 19:30 | Fixed pre-existing build break | Cargo.toml workspace + banlieue-api | added `schemars` feature to k8s-openapi; gated `serde_yaml` on `crdgen` feature | ~150 |
| 19:28 | Created .claude/CHANGELOG.md | — | ~1089 |
| 20:47 | Created .github/actions/extract-version/action.yml | — | ~1075 |
| 20:48 | Created .github/workflows/build.yaml | — | ~3932 |
| 20:48 | Edited .github/workflows/build.yaml | 5→5 lines | ~72 |
| 20:49 | add common build.yaml + extract-version composite, mirrored from 5-spot, scoped to Phase 0 (no Docker/VEX/SLSA yet) | .github/workflows/build.yaml, .github/actions/extract-version/action.yml | success | ~5000 |

## Session: 2026-05-25 22:47

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 22:47 | Created .github/workflows/sast.yaml | — | ~446 |
| 22:48 | Created .github/workflows/codeql.yaml | — | ~634 |
| 22:48 | Created .github/codeql/codeql-config.yml | — | ~192 |
| 22:48 | Created .github/workflows/scorecard.yaml | — | ~1018 |
| 22:48 | Created .github/workflows/calm.yaml | — | ~2023 |
| 22:49 | Created .github/workflows/calm-test.yaml | — | ~1661 |
| 22:49 | add SPDX headers to 16 .rs + 5 yaml example files (tests still green); copy SAST/CodeQL/CALM/Scorecard workflows + calm-args.sh/bats + codeql-config.yml from 5-spot | crates/banlieue-api/src/**, examples/**, .github/workflows/{sast,codeql,calm,calm-test,scorecard}.yaml, .github/scripts/calm-args.{sh,bats}, .github/codeql/codeql-config.yml | success (139 tests + 26 bats tests pass) | ~6500 |
| 10:41 | Created Cargo.toml | — | ~267 |
| 10:42 | Edited crates/banlieue-api/src/banlieue/provider_tests.rs | 3→3 lines | ~42 |
| 10:43 | Edited crates/banlieue-api/src/banlieue/provider_tests.rs | chrono_now() → parse_time() | ~99 |
| 10:45 | Edited .claude/CHANGELOG.md | expanded (+32 lines) | ~687 |
| 11:00 | Upgrade summary | Cargo.toml + provider_tests.rs + CHANGELOG + cerebrum | kube 0.96→3, k8s-openapi 0.23→0.27 (latest, schemars), schemars 0.8→1, thiserror 1→2, edition 2021→2024, MSRV 1.85; added tokio/tracing-subscriber/futures/anyhow workspace deps; aligned with kube-rs/controller-rs conventions; 139 tests + clippy + fmt all pass | — |
| 12:17 | Edited crates/banlieue-api/src/common.rs | expanded (+8 lines) | ~407 |
| 12:20 | Edited crates/banlieue-api/src/common.rs | modified shape() | ~572 |
| 12:21 | Edited crates/banlieue-api/src/common_tests.rs | modified ipam_source_default_is_dhcp() | ~1403 |
| 12:21 | Edited crates/banlieue-api/src/banlieue/vmclass_tests.rs | modified sample_nic() | ~66 |
| 12:22 | Edited crates/banlieue-api/src/banlieue/vmclass_tests.rs | modified network_interface_spec_with_mtu_round_trip() | ~367 |
| 12:22 | Edited crates/banlieue-api/src/banlieue/vmclass_tests.rs | modified vmclass_crd_metadata_matches_kube_attributes() | ~209 |
| 12:22 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs | modified sample_nic() | ~66 |
| 12:22 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs | modified vsphere_nic_spec_with_mac_address_round_trip() | ~406 |
| 12:23 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs | modified vsphere_machine_crd_metadata_matches_kube_attributes() | ~315 |
| 12:23 | Edited examples/03-vmclass-db-prod-large.yaml | 6→7 lines | ~53 |
| 12:25 | Edited .claude/CHANGELOG.md | expanded (+55 lines) | ~940 |
| 13:06 | Edited .claude/CLAUDE.md | "t relax the principle. (S" → "t relax the principle. (R" | ~51 |
| 13:06 | Edited .claude/CLAUDE.md | 12→7 lines | ~175 |
| 13:06 | Edited .claude/CLAUDE.md | 26→28 lines | ~266 |
| 13:06 | Edited .claude/CLAUDE.md | "docs/roadmap/" → "~/dev/roadmaps/banlieue/" | ~25 |
| 13:06 | Edited .claude/SKILL.md | inline fix | ~36 |
| 13:07 | Edited .claude/SKILL.md | 4→4 lines | ~75 |
| 13:07 | Edited .claude/SKILL.md | inline fix | ~44 |
| 13:07 | Edited .claude/SKILL.md | inline fix | ~24 |
| 13:07 | Edited .github/workflows/build.yaml | inline fix | ~15 |
| 13:08 | Edited .claude/CHANGELOG.md | expanded (+24 lines) | ~542 |
| 15:51 | Edited .github/workflows/calm-test.yaml | expanded (+25 lines) | ~575 |
| 15:58 | Edited .github/workflows/build.yaml | expanded (+12 lines) | ~265 |
| 15:58 | fix package-crds CI failure: kubectl --dry-run=client tried localhost:8080 OpenAPI fetch; add --validate=false + structural CRD-kind count check | .github/workflows/build.yaml | success (6 CRDs generated locally, parses) | ~800 |
| 15:58 | Created deny.toml | — | ~655 |
| 16:00 | Edited deny.toml | reduced (-9 lines) | ~180 |
| 16:00 | Edited deny.toml | expanded (+9 lines) | ~211 |
| 16:01 | Edited .github/workflows/build.yaml | 6→8 lines | ~120 |
| 22:55 | Edited .github/workflows/sast.yaml | 5→9 lines | ~105 |
| 22:55 | Edited .github/workflows/codeql.yaml | 4→8 lines | ~91 |
| 22:55 | Edited .github/workflows/scorecard.yaml | 8→12 lines | ~172 |
| 22:55 | fix upload-sarif permission failure across sast/codeql/scorecard workflows: add actions:read at job level | .github/workflows/sast.yaml, codeql.yaml, scorecard.yaml | success (yaml parses) | ~600 |
| 23:02 | Edited .github/workflows/build.yaml | modified at() | ~591 |
| 23:02 | replace kubectl --dry-run validation with inline Python YAML check (kubectl discovery still requires a cluster even with --validate=false; no offline flag exists) | .github/workflows/build.yaml | success (6 CRDs validated locally) | ~600 |
| 23:05 | Edited Cargo.toml | "banlieue contributors" → "Erick Bourgeois <erick@je" | ~12 |

## Session: 2026-05-26 23:14

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 23:24 | Edited Cargo.toml | 5→7 lines | ~37 |
| 23:24 | Edited Cargo.toml | expanded (+7 lines) | ~68 |
| 23:24 | Created crates/banlieue-provider-sdk/Cargo.toml | — | ~204 |
| 23:24 | Created crates/banlieue-provider-sdk/src/lib.rs | — | ~290 |
| 23:24 | Created crates/banlieue-provider-sdk/src/error.rs | — | ~383 |
| 23:24 | Created crates/banlieue-provider-sdk/src/client.rs | — | ~384 |
| 23:25 | Created crates/banlieue-provider-sdk/src/status.rs | — | ~896 |
| 23:25 | Created crates/banlieue-provider-sdk/src/status_tests.rs | — | ~935 |
| 23:25 | Created crates/banlieue-provider-sdk/src/finalizer.rs | — | ~675 |
| 23:26 | Created crates/banlieue-provider-sdk/src/finalizer.rs | — | ~850 |
| 23:26 | Created crates/banlieue-provider-sdk/src/finalizer_tests.rs | — | ~497 |
| 23:26 | Created crates/banlieue-provider-sdk/src/ssa.rs | — | ~579 |
| 23:26 | Created crates/banlieue-provider-sdk/src/reconciler.rs | — | ~426 |
| 23:26 | Created crates/banlieue-provider-sdk/src/reconciler_tests.rs | — | ~274 |
| 23:27 | Created crates/banlieue-controller/Cargo.toml | — | ~239 |
| 23:27 | Created crates/banlieue-controller/src/lib.rs | — | ~199 |
| 23:27 | Created crates/banlieue-controller/src/error.rs | — | ~282 |
| 23:27 | Created crates/banlieue-controller/src/context.rs | — | ~222 |
| 23:27 | Created crates/banlieue-controller/src/reconciler/mod.rs | — | ~90 |
| 23:28 | Created crates/banlieue-controller/src/reconciler/virtualmachine.rs | — | ~1423 |
| 23:28 | Created crates/banlieue-controller/src/reconciler/virtualmachine_tests.rs | — | ~297 |
| 23:28 | Created crates/banlieue-controller/src/main.rs | — | ~1249 |
| 23:29 | Edited crates/banlieue-provider-sdk/src/error.rs | 3→7 lines | ~85 |
| 23:29 | Edited crates/banlieue-provider-sdk/src/status.rs | 2→2 lines | ~24 |
| 23:29 | Edited crates/banlieue-provider-sdk/src/status_tests.rs | modified cond() | ~147 |
| 23:29 | Edited crates/banlieue-controller/Cargo.toml | 2→2 lines | ~33 |
| 23:30 | Created ../roadmaps/banlieue/14-PHASE-1E-DOCS.md | — | ~3475 |
| 23:30 | Edited ../roadmaps/banlieue/README.md | 2→3 lines | ~67 |
| 23:30 | Edited ../roadmaps/banlieue/README.md | 8→9 lines | ~80 |
| 23:30 | Edited crates/banlieue-provider-sdk/src/reconciler_tests.rs | modified requeue_intervals_are_monotonic() | ~56 |
| 23:31 | Created crates/banlieue-api/src/bin/crdgen.rs | — | ~772 |
| 23:32 | Created Makefile | — | ~3990 |
| 23:33 | Created Dockerfile | — | ~545 |
| 23:33 | Created Dockerfile.chainguard | — | ~364 |
| 23:33 | Created deploy/kind/cluster.yaml | — | ~149 |
| 23:33 | Created deploy/controller/namespace.yaml | — | ~134 |
| 23:33 | Created deploy/controller/rbac/serviceaccount.yaml | — | ~80 |
| 23:33 | Created deploy/controller/rbac/clusterrole.yaml | — | ~856 |
| 23:33 | Created deploy/controller/rbac/clusterrolebinding.yaml | — | ~135 |
| 23:33 | Created deploy/controller/configmap.yaml | — | ~122 |
| 23:34 | Created deploy/controller/service.yaml | — | ~161 |
| 23:34 | Created deploy/controller/deployment.yaml | — | ~863 |
| 23:36 | Edited .claude/CHANGELOG.md | modified from() | ~1588 |
| 00:28 | Edited Makefile | exist() → cluster() | ~45 |
| 00:28 | Edited Makefile | exist() → cluster() | ~40 |
| 00:30 | Edited crates/banlieue-api/src/common.rs | serde() → rule() | ~135 |
| 00:30 | Edited crates/banlieue-api/src/common_tests.rs | modified power_state_default_is_powered_on() | ~416 |
| 00:30 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | modified default_power_on() | ~18 |
| 00:30 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | "On" → "PoweredOn" | ~15 |
| 00:30 | Edited crates/banlieue-api/src/banlieue/virtualmachine_tests.rs | inline fix | ~6 |
| 00:31 | Edited crates/banlieue-api/src/banlieue/virtualmachine_tests.rs | inline fix | ~14 |
| 00:31 | Edited examples/05-virtualmachine.yaml | inline fix | ~9 |
| 00:33 | Edited .claude/CHANGELOG.md | expanded (+48 lines) | ~751 |

## Session: 2026-05-26 00:38

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 00:46 | Created docs/mkdocs.yml | — | ~1350 |
| 00:46 | Created docs/pyproject.toml | — | ~237 |
| 00:46 | Created docs/.python-version | — | ~2 |
| 00:47 | Created docs/.gitignore | — | ~37 |
| 00:47 | Created docs/README.md | — | ~499 |
| 00:47 | Created docs/src/stylesheets/extra.css | — | ~1274 |
| 00:47 | Created docs/src/javascripts/mermaid-init.js | — | ~693 |
| 00:48 | Created docs/src/index.md | — | ~1059 |
| 00:48 | Created docs/src/reasoning/index.md | — | ~512 |
| 00:49 | Created docs/src/reasoning/problem.md | — | ~1292 |
| 00:49 | Created docs/src/reasoning/abstraction-principle.md | — | ~1654 |
| 00:50 | Created docs/src/reasoning/least-touch.md | — | ~1643 |
| 00:51 | Created docs/src/reasoning/crd-only-contract.md | — | ~1828 |
| 00:52 | Created docs/src/reasoning/comparisons.md | — | ~2326 |
| 00:52 | Created docs/src/reasoning/non-goals.md | — | ~1436 |
| 00:52 | Created docs/src/concepts/index.md | — | ~152 |
| 00:53 | Created docs/src/concepts/architecture.md | — | ~1096 |
| 00:53 | Created docs/src/concepts/virtualmachine.md | — | ~861 |
| 00:53 | Created docs/src/concepts/providers.md | — | ~1397 |
| 00:54 | Created docs/src/concepts/infra-crds-capi.md | — | ~1209 |
| 00:54 | Created docs/src/getting-started/quickstart.md | — | ~524 |
| 00:54 | Created docs/src/reference/roadmap.md | — | ~677 |
| 00:55 | Created docs/src/reference/license.md | — | ~266 |
| 00:55 | Edited Makefile | 7→8 lines | ~142 |
| 00:55 | Edited Makefile | expanded (+24 lines) | ~362 |
| 00:56 | Created docs/src/overview.md | — | ~1653 |
| 00:56 | Edited docs/mkdocs.yml | 5→6 lines | ~27 |
| 00:56 | Edited docs/src/index.md | 7→8 lines | ~86 |
| 00:56 | Edited .gitignore | 3→8 lines | ~43 |
| 00:57 | Edited .claude/CHANGELOG.md | expanded (+36 lines) | ~1045 |

## Session summary — 2026-05-26 ~00:57

Bootstrapped the MkDocs Material doc site at docs/, mirroring the ~/dev/5-spot setup. Wrote the project's foundational reasoning content — the "why banlieue exists" case for an abstracted, provider-agnostic VM API with a CRD-only contract. Followed up with an Overview page (fundamentals + high-level mermaid diagram) requested mid-session. Wired Makefile targets `docs`, `docs-serve`, `docs-clean`, `docs-deploy`. Updated root .gitignore, .claude/CHANGELOG.md, and anatomy.md. No production code touched; documentation-only change.
| 01:07 | Edited examples/01-provider-vsphere-dc1.yaml | 2→2 lines | ~14 |
| 01:07 | Edited examples/02-provider-libvirt-edge.yaml | 2→2 lines | ~16 |
| 01:07 | Edited examples/05-virtualmachine.yaml | 2→2 lines | ~14 |
| 01:07 | Edited Makefile | 2→3 lines | ~74 |
| 01:08 | Edited crates/banlieue-api/src/banlieue/provider.rs | expanded (+7 lines) | ~102 |
| 01:08 | Edited crates/banlieue-api/src/banlieue/provider_tests.rs | "insecureSkipTlsVerify" → "insecureSkipTLSVerify" | ~15 |
| 01:09 | Edited .claude/CHANGELOG.md | expanded (+42 lines) | ~716 |
| 01:17 | Created crates/banlieue-controller/src/reconciler/scheduler.rs | — | ~5028 |
| 01:17 | Created docs/architecture/calm/architecture.json | — | ~7752 |
| 01:18 | Created docs/architecture/calm/templates/mermaid/system.md.hbs | — | ~477 |
| 01:18 | Created docs/architecture/calm/templates/mermaid/flows.md.hbs | — | ~352 |
| 01:18 | Created docs/architecture/calm/README.md | — | ~1066 |
| 01:19 | Created crates/banlieue-controller/src/reconciler/scheduler_tests.rs | — | ~6281 |
| 01:19 | Edited Makefile | expanded (+6 lines) | ~81 |
| 01:19 | Edited crates/banlieue-controller/src/reconciler/mod.rs | 1→2 lines | ~12 |
| 01:19 | Edited Makefile | 1→2 lines | ~23 |
| 01:19 | Edited Makefile | expanded (+31 lines) | ~519 |
| 01:19 | Edited crates/banlieue-controller/src/reconciler/scheduler.rs | 1→6 lines | ~51 |
| 01:19 | Edited Makefile | 3→4 lines | ~63 |
| 01:19 | Edited crates/banlieue-controller/src/reconciler/scheduler_tests.rs | 6→6 lines | ~50 |
| 01:19 | Edited crates/banlieue-controller/src/reconciler/scheduler.rs | removed 12 lines | ~4 |
| 01:19 | Edited docs/mkdocs.yml | modified Diagram() | ~106 |
| 01:19 | Created docs/src/architecture/system.md | — | ~162 |
| 01:20 | Created docs/src/architecture/flows.md | — | ~270 |
| 01:20 | Edited docs/src/concepts/architecture.md | expanded (+10 lines) | ~211 |
| 01:20 | Edited crates/banlieue-controller/src/reconciler/scheduler_tests.rs | inline fix | ~7 |
| 01:20 | Edited crates/banlieue-controller/src/reconciler/scheduler_tests.rs | 13→14 lines | ~213 |
| 01:20 | Edited crates/banlieue-controller/src/reconciler/scheduler_tests.rs | default() → into() | ~171 |
| 01:20 | Edited crates/banlieue-controller/src/reconciler/scheduler_tests.rs | expanded (+7 lines) | ~156 |
| 01:20 | Edited crates/banlieue-controller/src/reconciler/scheduler_tests.rs | modified as_mut() | ~75 |
| 01:20 | Edited .claude/CHANGELOG.md | modified yet() | ~956 |
| 01:20 | Edited crates/banlieue-controller/src/reconciler/scheduler.rs | 4→4 lines | ~57 |
| 01:21 | Created crates/banlieue-controller/src/reconciler/status_mirror.rs | — | ~1639 |
| 01:22 | Created crates/banlieue-controller/src/reconciler/status_mirror_tests.rs | — | ~2232 |
| 01:22 | Edited crates/banlieue-controller/src/reconciler/mod.rs | 2→3 lines | ~18 |
| 01:22 | Edited crates/banlieue-controller/src/reconciler/status_mirror_tests.rs | inline fix | ~7 |
| 01:22 | Edited crates/banlieue-controller/src/reconciler/status_mirror_tests.rs | inline fix | ~11 |
| 01:23 | Created crates/banlieue-controller/src/reconciler/infra.rs | — | ~1888 |
| 01:23 | Created crates/banlieue-controller/src/reconciler/infra_tests.rs | — | ~2454 |
| 01:23 | Edited crates/banlieue-controller/src/reconciler/mod.rs | 3→4 lines | ~22 |
| 01:24 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | 2→2 lines | ~20 |
| 01:24 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | 5→5 lines | ~54 |
| 01:25 | Edited docs/mkdocs.yml | modified Diagram() | ~250 |
| 01:25 | Created crates/banlieue-controller/src/reconciler/virtualmachine.rs | — | ~2582 |
| 01:25 | Created crates/banlieue-controller/src/reconciler/virtualmachine_tests.rs | — | ~258 |
| 01:25 | Edited crates/banlieue-controller/src/reconciler/scheduler.rs | map_or() → is_some_and() | ~43 |
| 01:25 | Edited crates/banlieue-controller/src/reconciler/scheduler.rs | inline fix | ~20 |
| 01:26 | Created .github/workflows/docs.yaml | — | ~2495 |
| 01:27 | Edited .claude/CHANGELOG.md | expanded (+26 lines) | ~675 |
| 01:29 | Edited .claude/CHANGELOG.md | expanded (+55 lines) | ~1681 |
| 01:47 | Edited crates/banlieue-api/Cargo.toml | 15→16 lines | ~116 |
| 01:47 | Created crates/banlieue-api/src/bin/crdgen.rs | — | ~876 |
| 01:48 | Edited .github/workflows/docs.yaml | expanded (+32 lines) | ~742 |
| 01:49 | Edited .claude/CHANGELOG.md | modified 2() | ~1071 |
| 01:58 | Created crates/banlieue-controller/src/reconciler/migration.rs | — | ~2101 |
| 01:58 | Created crates/banlieue-controller/src/reconciler/migration_tests.rs | — | ~2667 |
| 01:59 | Edited crates/banlieue-controller/src/reconciler/mod.rs | 12→12 lines | ~95 |
| 01:59 | Edited crates/banlieue-controller/src/reconciler/infra.rs | 12→12 lines | ~108 |
| 01:59 | Edited crates/banlieue-controller/src/reconciler/infra.rs | modified build_vsphere_machine() | ~233 |
| 02:00 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | modified parent_provider() | ~347 |
| 02:00 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | modified parent_provider() | ~243 |
| 02:00 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | 6→7 lines | ~54 |
| 02:00 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | 9→10 lines | ~81 |
| 02:00 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | 8→9 lines | ~81 |
| 02:00 | Edited crates/banlieue-controller/src/reconciler/infra_tests.rs | 3→8 lines | ~58 |
| 02:01 | Edited crates/banlieue-controller/src/reconciler/virtualmachine.rs | modified build_vsphere_machine() | ~220 |
| 02:01 | Edited crates/banlieue-controller/src/reconciler/virtualmachine.rs | added 1 import(s) | ~276 |
| 02:01 | Edited crates/banlieue-controller/src/reconciler/virtualmachine.rs | modified is_some() | ~175 |
| 02:02 | Edited crates/banlieue-controller/src/reconciler/virtualmachine.rs | modified mirror_only_path() | ~1374 |
| 02:02 | Edited crates/banlieue-controller/src/reconciler/virtualmachine.rs | modified finalize_vm() | ~491 |
| 02:02 | Edited crates/banlieue-controller/src/reconciler/virtualmachine.rs | modified patch_placement_invalid() | ~276 |
| 02:04 | Edited crates/banlieue-controller/src/main.rs | added 1 import(s) | ~97 |
| 02:04 | Edited crates/banlieue-controller/src/main.rs | modified as_deref() | ~487 |

## Session: 2026-05-26 02:05

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 02:05 | Edited .claude/CHANGELOG.md | expanded (+56 lines) | ~1662 |
| 14:25 | Edited Makefile | 2→2 lines | ~26 |
| 14:25 | Edited Makefile | expanded (+15 lines) | ~199 |
| 14:25 | Created docs/src/architecture/index.md | — | ~1402 |
| 14:26 | Created docs/src/reasoning/capi-relationship.md | — | ~1813 |
| 14:26 | Edited docs/mkdocs.yml | modified Architecture() | ~246 |
| 14:28 | Edited Makefile | 14→14 lines | ~197 |
| 14:28 | Edited Makefile | 14→14 lines | ~192 |
| 14:29 | Edited .claude/CHANGELOG.md | expanded (+26 lines) | ~753 |
| 14:32 | Session summary: added CALM section index + CAPI relationship doc; fixed --clear-output-directory clobber in calm-* Makefile targets; mkdocs build green | docs/src/architecture/index.md, docs/src/reasoning/capi-relationship.md, docs/mkdocs.yml, Makefile, .claude/CHANGELOG.md | ✓ | ~3200 |
| 14:44 | Created crates/banlieue-provider-sdk/src/leader_tests.rs | — | ~1696 |
| 14:46 | Created crates/banlieue-provider-sdk/src/leader.rs | — | ~3406 |
| 15:03 | Edited crates/banlieue-provider-sdk/src/lib.rs | 15→18 lines | ~181 |
| 17:19 | Created crates/banlieue-provider-sdk/src/leader.rs | — | ~3398 |
| 17:19 | Created crates/banlieue-provider-sdk/src/leader_tests.rs | — | ~1616 |
| 17:20 | Edited crates/banlieue-provider-sdk/src/leader_tests.rs | inline fix | ~18 |
| 17:20 | Edited crates/banlieue-provider-sdk/src/leader.rs | modified create_owned_lease() | ~195 |
| 17:21 | Edited crates/banlieue-controller/src/main.rs | expanded (+46 lines) | ~998 |
| 17:21 | Created ../../.claude/projects/-Users-erick-dev-banlieue/memory/feedback_least_privilege.md | — | ~918 |
| 17:21 | Created ../../.claude/projects/-Users-erick-dev-banlieue/memory/MEMORY.md | — | ~58 |
| 17:22 | Edited crates/banlieue-controller/src/main.rs | modified main() | ~1467 |
| 17:31 | Edited deploy/controller/rbac/clusterrole.yaml | 5→6 lines | ~96 |
| 17:33 | Edited .claude/CHANGELOG.md | expanded (+28 lines) | ~1095 |
| 14:55 | Phase 1A iteration 4 leader election: pure decide_action + LeaderConfig + acquire_or_wait + renew_forever; 13 tests pass; CLI flags --leader-election-* and --log-level added; SIGTERM handler; clippy clean; 214 tests green | leader.rs, leader_tests.rs, controller/main.rs, rbac/clusterrole.yaml, CHANGELOG.md | ✓ | ~5200 |
