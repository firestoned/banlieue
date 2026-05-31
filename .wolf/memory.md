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
| 17:49 | Created ../../.claude/projects/-Users-erick-dev-banlieue/memory/feedback_git_hands_off.md | — | ~422 |
| 17:49 | Edited ../../.claude/projects/-Users-erick-dev-banlieue/memory/MEMORY.md | 3→4 lines | ~96 |
| 19:41 | Edited Cargo.toml | 7→8 lines | ~48 |
| 19:41 | Edited Cargo.toml | expanded (+7 lines) | ~131 |
| 19:41 | Created crates/banlieue-provider-vsphere/Cargo.toml | — | ~377 |
| 19:41 | Created crates/banlieue-provider-vsphere/src/lib.rs | — | ~246 |
| 19:41 | Created crates/banlieue-provider-vsphere/src/error.rs | — | ~328 |
| 19:42 | Created crates/banlieue-provider-vsphere/src/context.rs | — | ~342 |
| 19:42 | Created crates/banlieue-provider-vsphere/src/client/mod.rs | — | ~739 |
| 19:42 | Created crates/banlieue-provider-vsphere/src/client/fake.rs | — | ~875 |
| 19:42 | Created crates/banlieue-provider-vsphere/src/reconciler/mod.rs | — | ~93 |
| 19:43 | Created crates/banlieue-provider-vsphere/src/reconciler/provider_tests.rs | — | ~1484 |
| 19:43 | Edited crates/banlieue-provider-vsphere/src/client/fake.rs | modified new() | ~120 |
| 19:43 | Edited crates/banlieue-provider-vsphere/src/reconciler/provider_tests.rs | modified small_inventory() | ~149 |
| 19:44 | Created crates/banlieue-provider-vsphere/src/reconciler/provider.rs | — | ~3541 |
| 19:46 | Created crates/banlieue-provider-vsphere/src/client/vim.rs | — | ~1470 |
| 19:47 | Created crates/banlieue-provider-vsphere/src/main.rs | — | ~2506 |
| 19:47 | Created deploy/provider-vsphere/rbac/serviceaccount.yaml | — | ~84 |
| 19:47 | Created deploy/provider-vsphere/rbac/clusterrole.yaml | — | ~937 |
| 19:48 | Created deploy/provider-vsphere/rbac/clusterrolebinding.yaml | — | ~142 |
| 19:48 | Created deploy/provider-vsphere/configmap.yaml | — | ~162 |
| 19:48 | Created deploy/provider-vsphere/deployment.yaml | — | ~860 |
| 19:48 | Created deploy/provider-vsphere/service.yaml | — | ~166 |
| 19:49 | Edited Makefile | 9→12 lines | ~187 |
| 19:49 | Edited Makefile | expanded (+35 lines) | ~559 |
| 19:50 | Edited Makefile | expanded (+16 lines) | ~275 |
| 19:51 | Created deploy/provider-vsphere/README.md | — | ~1155 |
| 19:53 | Edited crates/banlieue-provider-vsphere/src/client/mod.rs | 2→2 lines | ~29 |
| 19:53 | Edited crates/banlieue-provider-vsphere/src/reconciler/provider_tests.rs | inline fix | ~20 |
| 19:54 | Edited crates/banlieue-provider-vsphere/src/reconciler/provider_tests.rs | modified as_client() | ~58 |
| 19:54 | Edited crates/banlieue-provider-vsphere/src/reconciler/provider_tests.rs | inline fix | ~11 |
| 19:58 | Edited .claude/CHANGELOG.md | expanded (+29 lines) | ~1312 |
| 16:05 | Phase 1B iter 1: banlieue-provider-vsphere crate scaffolded with vim_rs; VSphereClient trait + FakeClient + real vim impl; Provider reconciler (capability introspection) populates failureDomains; 9 new tests; clippy clean; build 3m28s cold; deploy manifests + vcsim Makefile targets + README | crates/banlieue-provider-vsphere/**, deploy/provider-vsphere/**, Cargo.toml, Makefile, .claude/CHANGELOG.md | ✓ | ~8200 |

## Session: 2026-05-27 09:51

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 22:15 | Edited crates/banlieue-provider-vsphere/src/client/mod.rs | expanded (+13 lines) | ~243 |
| 22:15 | Edited crates/banlieue-provider-vsphere/src/client/mod.rs | modified list_datacenters() | ~202 |
| 22:15 | Edited crates/banlieue-provider-vsphere/src/client/fake.rs | 9→11 lines | ~131 |
| 22:15 | Edited crates/banlieue-provider-vsphere/src/client/fake.rs | modified with_cluster() | ~383 |
| 22:15 | Edited crates/banlieue-provider-vsphere/src/client/fake.rs | modified list_clusters() | ~136 |
| 22:16 | Edited crates/banlieue-provider-vsphere/src/client/vim.rs | added 1 import(s) | ~138 |
| 22:16 | Edited crates/banlieue-provider-vsphere/src/client/vim.rs | 2→3 lines | ~43 |
| 22:16 | Edited crates/banlieue-provider-vsphere/src/client/vim.rs | modified find_template() | ~753 |
| 22:16 | Edited crates/banlieue-provider-vsphere/src/client/mod.rs | 2→6 lines | ~76 |
| 22:19 | Created crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs | — | ~3927 |
| 22:19 | Edited crates/banlieue-provider-vsphere/src/reconciler/mod.rs | 1→2 lines | ~10 |
| 22:20 | Created crates/banlieue-provider-vsphere/src/reconciler/vmimage_tests.rs | — | ~2923 |
| 22:21 | Edited crates/banlieue-provider-vsphere/src/reconciler/vmimage_tests.rs | 3→3 lines | ~34 |
| 22:21 | Edited crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs | 4→3 lines | ~37 |
| 22:21 | Edited crates/banlieue-provider-vsphere/src/main.rs | inline fix | ~13 |
| 22:21 | Edited crates/banlieue-provider-vsphere/src/main.rs | 5→6 lines | ~37 |
| 22:21 | Edited crates/banlieue-provider-vsphere/src/main.rs | controller() → controllers() | ~419 |
| 22:23 | Edited .claude/CHANGELOG.md | expanded (+26 lines) | ~1003 |
| 10:30 | Phase 1B iter 2a: VMImage reconciler. Extended VSphereClient with find_template; FakeClient with .with_template; vim impl walks VirtualMachine MOs filtered by config.template; 12 tests; 235 total; clippy clean | crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs+_tests.rs, client/{mod,fake,vim}.rs, main.rs, .claude/CHANGELOG.md | ✓ | ~7300 |

## Session: 2026-05-30 09:30

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 17:07 | Edited Makefile | expanded (+8 lines) | ~129 |
| 17:07 | Edited Makefile | inline fix | ~15 |
| 17:07 | Edited Makefile | 2→2 lines | ~31 |
| 17:08 | Edited .claude/CHANGELOG.md | expanded (+16 lines) | ~276 |
| 17:08 | Made RUST_LOG overridable on *-run-local targets (RUST_LOG ?= / RUST_LOG_VSPHERE ?=) | Makefile, .claude/CHANGELOG.md | done | ~3k |
| 17:24 | Edited deploy/provider-vsphere/README.md | expanded (+57 lines) | ~620 |
| 17:24 | Edited .claude/CHANGELOG.md | expanded (+16 lines) | ~304 |
| 17:24 | Added GOVC_* env -> Secret/Provider doc section | deploy/provider-vsphere/README.md, .claude/CHANGELOG.md | done | ~3k |
| 17:28 | Created docs/src/getting-started/vsphere-provider.md | — | ~2360 |
| 17:28 | Edited docs/src/concepts/providers.md | expanded (+17 lines) | ~438 |
| 17:28 | Edited docs/src/concepts/providers.md | expanded (+8 lines) | ~240 |
| 17:28 | Edited docs/mkdocs.yml | 4→5 lines | ~49 |
| 17:29 | Edited docs/src/getting-started/quickstart.md | 5→5 lines | ~88 |
| 17:29 | Edited .claude/CHANGELOG.md | expanded (+23 lines) | ~537 |
| 17:29 | Added core-docs vSphere provider guide (GOVC_* secret flow) + synced providers.md Provider schema to code; mkdocs --strict passes | docs/src/getting-started/vsphere-provider.md, docs/src/concepts/providers.md, docs/mkdocs.yml, docs/src/getting-started/quickstart.md | done | ~9k |

## Session: 2026-05-30 17:39

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 17:39 | Edited crates/banlieue-api/src/banlieue/vmclass.rs | expanded (+34 lines) | ~507 |
| 17:39 | Edited crates/banlieue-api/src/banlieue/vmclass.rs | 5→6 lines | ~71 |
| 17:39 | Edited crates/banlieue-api/src/banlieue/vmclass.rs | 6→8 lines | ~102 |
| 17:39 | Edited crates/banlieue-api/src/banlieue/vmclass.rs | 12→15 lines | ~170 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | expanded (+26 lines) | ~485 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | 19→23 lines | ~189 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | 4→6 lines | ~89 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | 2→3 lines | ~47 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | 4→6 lines | ~96 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/vmimage.rs | 4→5 lines | ~61 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/provider.rs | expanded (+22 lines) | ~358 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/provider.rs | 4→6 lines | ~80 |
| 17:40 | Edited crates/banlieue-api/src/banlieue/provider.rs | 4→8 lines | ~125 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/provider.rs | 4→5 lines | ~77 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/provider.rs | 4→5 lines | ~78 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/provider.rs | 4→6 lines | ~92 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/provider.rs | 4→7 lines | ~113 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/provider.rs | 4→6 lines | ~102 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | expanded (+23 lines) | ~361 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | 4→7 lines | ~106 |
| 17:41 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | 4→6 lines | ~94 |
| 17:42 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | 12→16 lines | ~170 |
| 17:42 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | 4→6 lines | ~102 |
| 17:42 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | 4→6 lines | ~92 |
| 17:42 | Edited crates/banlieue-api/src/banlieue/virtualmachine.rs | 4→6 lines | ~81 |
| 17:42 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine.rs | expanded (+24 lines) | ~391 |
| 17:42 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine.rs | expanded (+8 lines) | ~307 |
| 17:42 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine.rs | 4→6 lines | ~99 |
| 17:42 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine.rs | expanded (+12 lines) | ~267 |
| 17:44 | Edited crates/banlieue-api/src/bin/crdgen.rs | 4→8 lines | ~96 |
| 17:44 | Edited crates/banlieue-api/src/bin/crdgen.rs | 17→17 lines | ~182 |
| 17:44 | Edited crates/banlieue-api/src/bin/crdgen.rs | modified render() | ~411 |
| 17:44 | Edited crates/banlieue-api/src/bin/crdgen.rs | 4→8 lines | ~37 |
| 17:45 | Created crates/banlieue-api/src/bin/crdgen_tests.rs | — | ~951 |
| 17:45 | Edited crates/banlieue-api/src/bin/crdgen.rs | inline fix | ~26 |
| 17:45 | Edited crates/banlieue-api/src/bin/crdgen_tests.rs | 7→5 lines | ~61 |
| 17:45 | Edited crates/banlieue-api/src/bin/crdgen_tests.rs | modified render_emits_crd_yaml() | ~76 |
| 17:46 | Edited crates/banlieue-api/src/bin/crdgen.rs | 8→5 lines | ~64 |
| 17:47 | Edited crates/banlieue-api/src/bin/crdgen.rs | removed 37 lines | ~31 |
| 17:47 | Edited crates/banlieue-api/src/bin/crdgen.rs | 8→4 lines | ~21 |
| 17:47 | Created crates/banlieue-api/src/crdgen_support.rs | — | ~571 |
| 17:47 | Created crates/banlieue-api/src/crdgen_support_tests.rs | — | ~802 |
| 17:47 | Edited crates/banlieue-api/src/lib.rs | 3→8 lines | ~56 |
| 17:51 | Edited .claude/CHANGELOG.md | expanded (+24 lines) | ~642 |
| 17:52 | Documented all CRD spec/status/nested types; added crdgen_support promote_spec_description to surface "what is X" at CRD root; regenerated deploy/crds | crates/banlieue-api/src/**, src/crdgen_support.rs, src/bin/crdgen.rs, deploy/crds/* | done | ~25k |
| 18:23 | Created crates/banlieue-api/src/crddoc.rs | — | ~2867 |
| 18:24 | Created crates/banlieue-api/src/crddoc_tests.rs | — | ~1146 |
| 18:24 | Edited crates/banlieue-api/src/lib.rs | 4→9 lines | ~82 |
| 18:24 | Created crates/banlieue-api/src/bin/crddoc.rs | — | ~683 |
| 18:24 | Edited crates/banlieue-api/Cargo.toml | 4→9 lines | ~44 |
| 18:25 | Edited crates/banlieue-api/src/crddoc.rs | modified as_deref() | ~283 |
| 18:25 | Edited crates/banlieue-api/src/crddoc.rs | trim() → prose() | ~44 |
| 18:25 | Edited crates/banlieue-api/src/crddoc.rs | modified prose() | ~232 |
| 18:26 | Edited crates/banlieue-api/src/crddoc_tests.rs | modified heading_anchor_lowercases() | ~167 |
| 18:26 | Edited Makefile | 5→9 lines | ~127 |
| 18:26 | Edited Makefile | 2→5 lines | ~38 |
| 18:26 | Edited Makefile | inline fix | ~18 |
| 18:26 | Edited docs/mkdocs.yml | modified Reference() | ~40 |
| 18:27 | Edited .claude/SKILL.md | 10→14 lines | ~200 |
| 18:28 | Edited .claude/CHANGELOG.md | expanded (+24 lines) | ~560 |
| 18:28 | Added crddoc generator (lib crddoc.rs + bin) producing docs/src/reference/api.md; wired into make crds + mkdocs nav; regen-api-docs skill now real | crates/banlieue-api/src/crddoc.rs, src/bin/crddoc.rs, Makefile, docs/mkdocs.yml, docs/src/reference/api.md, .claude/SKILL.md | done | ~18k |
| 18:36 | Edited Makefile | 2→2 lines | ~73 |
| 18:36 | Edited .github/workflows/docs.yaml | 6→9 lines | ~144 |
| 18:37 | Created docs/adr/0001-capi-native-cluster-provisioning.md | — | ~1256 |
| 18:37 | Edited .claude/CHANGELOG.md | expanded (+19 lines) | ~346 |
| 18:37 | Wired api-docs into make docs so docs.yaml CI regenerates the CRD API reference on every docs build | Makefile, .github/workflows/docs.yaml | done | ~2k |
| 18:38 | Created docs/adr/0002-infracluster-failure-domain-aggregation.md | — | ~1915 |
| 18:38 | Created docs/adr/0003-provider-deployment-topology.md | — | ~821 |
| 18:38 | Edited .claude/CHANGELOG.md | expanded (+18 lines) | ~417 |
| 18:39 | Edited crates/banlieue-api/src/common.rs | expanded (+43 lines) | ~534 |
| 18:39 | Edited crates/banlieue-api/src/common_tests.rs | modified api_endpoint_round_trip() | ~916 |
| 18:40 | Created crates/banlieue-api/src/infrastructure/vsphere_cluster.rs | — | ~1983 |
| 18:40 | Edited crates/banlieue-api/src/infrastructure/vsphere_cluster.rs | 2→2 lines | ~30 |
| 18:40 | Edited crates/banlieue-api/src/infrastructure/vsphere_cluster.rs | modified label_selector_is_empty() | ~54 |
| 18:41 | Created crates/banlieue-api/src/infrastructure/vsphere_cluster_tests.rs | — | ~2030 |
| 18:41 | Edited crates/banlieue-api/src/infrastructure/mod.rs | 13→15 lines | ~166 |
| 18:42 | Edited crates/banlieue-api/src/bin/crdgen.rs | inline fix | ~25 |
| 18:42 | Edited crates/banlieue-api/src/bin/crdgen.rs | 4→8 lines | ~73 |
| 18:42 | Edited crates/banlieue-api/src/bin/crddoc.rs | inline fix | ~25 |
| 18:42 | Edited crates/banlieue-api/src/bin/crddoc.rs | 4→5 lines | ~48 |
| 18:42 | Edited crates/banlieue-api/src/lib.rs | 14→17 lines | ~219 |
| 18:45 | Session: ADRs 0001-0003 + VSphereCluster InfraCluster CRD landed | api crate + crds + docs/adr | cargo fmt/clippy/test all green; CRD dry-run validates; reconciler (task 5) next | ~milestone |
| 18:48 | Created .claude/rules/architecture-driven-development.md | — | ~879 |
| 18:48 | Edited .claude/CLAUDE.md | expanded (+15 lines) | ~432 |
| 18:49 | Created ../../.claude/projects/-Users-erick-dev-banlieue/memory/feedback_architecture_driven_development.md | — | ~488 |
| 18:49 | Edited ../../.claude/projects/-Users-erick-dev-banlieue/memory/MEMORY.md | 4→5 lines | ~142 |
| 18:49 | Codified ADD (Architecture Driven Development): ADR -> CALM -> TDD as governing methodology | .claude/CLAUDE.md, .claude/rules/architecture-driven-development.md, cerebrum, ~/.claude memory | done | ~4k |
| 18:56 | Edited docs/architecture/calm/architecture.json | modified objects() | ~670 |
| 18:56 | Edited docs/architecture/calm/architecture.json | 7→8 lines | ~76 |
| 18:56 | Edited docs/architecture/calm/architecture.json | 7→9 lines | ~90 |
| 18:56 | Edited docs/architecture/calm/architecture.json | expanded (+10 lines) | ~268 |
| 18:56 | Edited docs/architecture/calm/architecture.json | 2→2 lines | ~143 |
| 18:56 | Edited docs/architecture/calm/architecture.json | expanded (+14 lines) | ~234 |
| 18:57 | Edited docs/architecture/calm/architecture.json | expanded (+56 lines) | ~1240 |
| 18:57 | Edited docs/architecture/calm/architecture.json | expanded (+13 lines) | ~476 |
| 18:57 | Edited docs/architecture/calm/architecture.json | 10→14 lines | ~113 |
| 18:59 | Edited crates/banlieue-controller/src/reconciler/scheduler.rs | modified selector_matches() | ~84 |
| 19:00 | Created crates/banlieue-controller/src/reconciler/vsphere_cluster.rs | — | ~2652 |
| 19:00 | Edited crates/banlieue-controller/src/reconciler/mod.rs | 5→6 lines | ~34 |
| 19:01 | Created crates/banlieue-controller/src/reconciler/vsphere_cluster_tests.rs | — | ~3007 |
| 19:02 | Edited crates/banlieue-controller/src/main.rs | 2→2 lines | ~36 |

## Session: 2026-05-30 19:02

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 19:02 | Edited crates/banlieue-controller/src/main.rs | 4→5 lines | ~39 |
| 19:02 | Edited crates/banlieue-controller/src/main.rs | modified as_deref() | ~783 |
| 19:03 | Edited deploy/controller/rbac/clusterrole.yaml | expanded (+11 lines) | ~214 |
| 19:04 | Created examples/06-vspherecluster-multi-vcenter.yaml | — | ~666 |
| 19:04 | Edited docs/src/concepts/infra-crds-capi.md | expanded (+27 lines) | ~439 |
| 19:05 | Edited docs/src/concepts/infra-crds-capi.md | 4→4 lines | ~82 |
| 19:05 | Edited docs/src/concepts/infra-crds-capi.md | 2→2 lines | ~45 |
| 19:06 | Edited .claude/CHANGELOG.md | modified methodology() | ~787 |
| 19:06 | Edited .claude/CHANGELOG.md | 213 → 261 | ~54 |
| 19:10 | VSphereCluster reconciler + main.rs wiring + RBAC + example + CALM + docs | controller, deploy, examples, calm, docs | cargo test --all 261 green; clippy clean; mkdocs --strict clean; CALM validates | ~milestone |
| 19:10 | Created README.md | — | ~1806 |
| 19:10 | Edited README.md | reduced (-23 lines) | ~199 |
| 19:11 | Edited .claude/CHANGELOG.md | expanded (+20 lines) | ~416 |
| 19:11 | Wrote root README.md (intro/why/architecture/layout/dev) referencing the single canonical diagram in docs/src/concepts/architecture.md | README.md | done | ~4k |
| 19:16 | Created docs/adr/0004-single-binary-subcommand-dispatch.md | — | ~1556 |
| 19:17 | Edited docs/architecture/calm/architecture.json | expanded (+6 lines) | ~262 |
| 19:17 | Edited docs/architecture/calm/architecture.json | expanded (+15 lines) | ~213 |
| 19:17 | Edited docs/architecture/calm/architecture.json | 1→2 lines | ~32 |
| 19:19 | Created crates/banlieue-provider-sdk/src/bootstrap_tests.rs | — | ~356 |
| 19:19 | Created crates/banlieue-provider-sdk/src/bootstrap.rs | — | ~1585 |
| 19:20 | Edited crates/banlieue-provider-sdk/src/lib.rs | 4→6 lines | ~69 |
| 19:20 | Edited crates/banlieue-provider-sdk/src/lib.rs | 2→3 lines | ~14 |
| 19:20 | Edited crates/banlieue-provider-sdk/Cargo.toml | 6→9 lines | ~114 |
| 19:22 | Created crates/banlieue-controller/src/app.rs | — | ~2746 |
| 22:34 | Created crates/banlieue-controller/src/app_tests.rs | — | ~545 |
| 22:34 | Edited crates/banlieue-controller/src/lib.rs | 11→16 lines | ~160 |
| 22:34 | Edited crates/banlieue-controller/Cargo.toml | 7→6 lines | ~52 |
| 22:34 | Edited crates/banlieue-controller/Cargo.toml | 7→9 lines | ~96 |
| 22:35 | Created crates/banlieue-provider-vsphere/src/app.rs | — | ~2226 |
| 22:35 | Created crates/banlieue-provider-vsphere/src/app_tests.rs | — | ~523 |
| 22:36 | Edited crates/banlieue-provider-vsphere/src/lib.rs | 10→15 lines | ~127 |
| 22:36 | Edited crates/banlieue-provider-vsphere/Cargo.toml | 12→13 lines | ~138 |
| 22:36 | Edited crates/banlieue-provider-vsphere/Cargo.toml | 7→9 lines | ~96 |
| 22:37 | Created crates/banlieue/src/cli.rs | — | ~704 |
| 22:37 | Created crates/banlieue/src/cli_tests.rs | — | ~355 |
| 22:37 | Created crates/banlieue/src/main.rs | — | ~212 |
| 22:37 | Created crates/banlieue/Cargo.toml | — | ~317 |
| 22:37 | Edited Cargo.toml | 6→7 lines | ~47 |
| 22:38 | Edited Makefile | modified loop() | ~159 |
| 22:38 | Edited Makefile | 5→7 lines | ~80 |
| 22:39 | Edited Makefile | 11→11 lines | ~204 |
| 22:39 | Edited Makefile | 2→2 lines | ~40 |
| 22:39 | Edited Makefile | 3→3 lines | ~33 |
| 22:39 | Edited Dockerfile | 5→6 lines | ~67 |
| 22:39 | Edited Dockerfile | 8→9 lines | ~125 |
| 22:39 | Edited Dockerfile.chainguard | 1→4 lines | ~58 |
| 22:39 | Edited Dockerfile.chainguard | 3→3 lines | ~14 |
| 22:39 | Edited deploy/controller/deployment.yaml | 8→10 lines | ~149 |
| 22:39 | Edited deploy/provider-vsphere/deployment.yaml | 7→9 lines | ~132 |
| 22:40 | Edited deploy/provider-vsphere/README.md | 5→5 lines | ~50 |
| 22:40 | Edited deploy/provider-vsphere/README.md | 3→3 lines | ~22 |
| 22:40 | Edited docs/src/getting-started/vsphere-provider.md | 5→5 lines | ~51 |
| 22:44 | Edited .claude/CHANGELOG.md | modified run() | ~863 |
| 22:44 | Edited docs/src/concepts/architecture.md | 8→13 lines | ~323 |
| 22:44 | Edited README.md | 6→7 lines | ~104 |
| 23:30 | Session: unified single `banlieue` binary w/ subcommand dispatch (ADR-0004 + CALM); controller/vsphere → libs; sdk bootstrap module; Makefile/Docker/deploy wiring | many | fmt+clippy+test all green | ~- |
| 07:12 | Edited README.md | expanded (+8 lines) | ~409 |
| 07:12 | Edited docs/src/index.md | 2→4 lines | ~123 |
| 07:13 | Comprehensive badges in README (CI/CodeQL/Scorecard + license/rust/issues/last-commit/PRs); minimal badges (Build/Rust) added to docs/src/index.md | README.md, docs/src/index.md | done | ~2k |
| 07:13 | Edited .claude/CHANGELOG.md | expanded (+19 lines) | ~319 |
| 07:16 | Created docs/adr/0005-capi-contract-label-codegen.md | — | ~1093 |
| 07:16 | Edited docs/architecture/calm/architecture.json | 26→26 lines | ~616 |
| 07:17 | Edited docs/architecture/calm/architecture.json | 4→5 lines | ~48 |
| 07:17 | Edited crates/banlieue-api/src/crdgen_support.rs | modified prepared() | ~575 |
| 07:18 | Edited crates/banlieue-api/src/crdgen_support_tests.rs | added 1 import(s) | ~113 |
| 07:18 | Edited crates/banlieue-api/src/crdgen_support_tests.rs | modified contract_label() | ~526 |
| 07:19 | Edited crates/banlieue-api/src/infrastructure/vsphere_cluster.rs | 3→4 lines | ~72 |
| 07:19 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine.rs | 7→7 lines | ~124 |
| 07:19 | Edited crates/banlieue-api/src/infrastructure/vsphere_machine.rs | time() → labels() | ~73 |
| 07:20 | Edited docs/adr/0002-infracluster-failure-domain-aggregation.md | kustomize() → label() | ~94 |
| 07:21 | Edited .claude/CHANGELOG.md | expanded (+25 lines) | ~566 |
| 10:05 | ADR-0005 + contract label emitted by crdgen for infra CRDs | crdgen_support.rs, deploy/crds, docs/adr/0005, calm | 292 tests green; label on 3 infra CRDs, absent on 4 banlieue.io; mkdocs+calm clean | ~milestone |
| 08:13 | Edited README.md | inline fix | ~35 |
| 08:13 | Edited docs/src/index.md | inline fix | ~27 |
| 08:14 | Edited docs/mkdocs.yml | modified Reference() | ~29 |
| 08:14 | Edited docs/src/concepts/virtualmachine.md | 3→3 lines | ~46 |
| 08:14 | Edited docs/src/concepts/virtualmachine.md | 2→2 lines | ~37 |
| 08:15 | Edited docs/src/index.md | inline fix | ~31 |
| 08:15 | Edited docs/src/index.md | 3→3 lines | ~60 |
| 08:15 | Edited docs/src/index.md | 3→2 lines | ~22 |
| 08:15 | Edited docs/src/overview.md | 3→2 lines | ~26 |
| 08:15 | Edited docs/src/reasoning/non-goals.md | 4→4 lines | ~61 |
| 08:15 | Edited docs/src/reasoning/non-goals.md | 3→2 lines | ~36 |
| 08:15 | Edited docs/src/getting-started/quickstart.md | 3→2 lines | ~37 |
| 08:15 | Edited docs/src/getting-started/vsphere-provider.md | 2→2 lines | ~26 |
| 08:16 | Edited docs/README.md | inline fix | ~15 |
| 08:16 | Edited docs/adr/0003-provider-deployment-topology.md | inline fix | ~19 |
| 08:16 | Edited docs/src/architecture/index.md | inline fix | ~20 |
| 08:17 | Created docs/src/reasoning/capi-relationship.md | — | ~2412 |
| 08:18 | Edited docs/src/concepts/infra-crds-capi.md | 5→7 lines | ~107 |
| 08:18 | Edited docs/src/concepts/infra-crds-capi.md | expanded (+9 lines) | ~268 |
| 08:18 | Edited docs/src/concepts/providers.md | 4→8 lines | ~124 |
| 08:19 | Edited .claude/CHANGELOG.md | expanded (+23 lines) | ~609 |
| 11:35 | Removed roadmap.md from docs site + rewrote capi-relationship.md for CAPI cluster capability | docs/src, mkdocs.yml | mkdocs --strict clean; no roadmap links remain; deprecated CAPI fields corrected | ~docs |
| 09:02 | Edited crates/banlieue/Cargo.toml | 3→6 lines | ~70 |
| 09:02 | Edited crates/banlieue/src/cli_tests.rs | modified missing_subcommand_is_an_error() | ~621 |
| 09:02 | Edited crates/banlieue/src/cli.rs | 10→14 lines | ~145 |
| 09:02 | Edited crates/banlieue/src/cli.rs | modified Example() | ~148 |
| 09:02 | Edited crates/banlieue/src/cli.rs | modified dispatch() | ~214 |
| 09:03 | Edited docs/src/getting-started/quickstart.md | expanded (+33 lines) | ~224 |
| 09:04 | Edited .claude/CHANGELOG.md | modified banlieue() | ~431 |
| 12:30 | Added `banlieue completion <shell>` subcommand (clap_complete) | crates/banlieue/src/cli.rs, cli_tests.rs, Cargo.toml, quickstart.md | 281 tests green; zsh script emits #compdef banlieue; clippy+mkdocs clean | ~feature |
| 09:08 | Edited Cargo.toml | "1.85" → "1.88" | ~6 |
| 09:08 | Edited README.md | 1.85 → 1.88 | ~26 |
| 09:08 | Edited docs/src/index.md | 1.85 → 1.88 | ~27 |
| 09:09 | Edited .claude/CHANGELOG.md | modified option() | ~326 |
| 09:09 | Bumped workspace MSRV 1.85->1.88 to match kube 3.1.0 (needs Rust 1.88); fixed cargo upgrade incompatible flag; updated badges | Cargo.toml, README.md, docs/src/index.md | done | ~2k |
| 10:17 | Created docs/adr/0006-release-and-supply-chain-pipeline.md | — | ~1615 |
| 10:17 | Edited docs/architecture/calm/architecture.json | expanded (+21 lines) | ~636 |
| 10:17 | Edited docs/architecture/calm/architecture.json | 4→5 lines | ~50 |
| 10:21 | Created .github/workflows/build.yaml | — | ~8635 |
| 10:22 | Created .github/actions/prepare-docker-binaries/action.yml | — | ~325 |
| 10:22 | Edited .github/workflows/build.yaml | 3→3 lines | ~41 |
| 10:23 | Edited Makefile | 4→9 lines | ~65 |
| 10:23 | Edited Makefile | expanded (+35 lines) | ~506 |
| 10:24 | Edited Makefile | 2→3 lines | ~53 |
| 10:24 | Edited .github/workflows/build.yaml | expanded (+6 lines) | ~320 |
| 10:24 | Created .vex/README.md | — | ~694 |
| 10:25 | Created .vex/.affected-functions.json | — | ~172 |
| 10:25 | Edited .claude/CHANGELOG.md | expanded (+29 lines) | ~875 |
| 14:30 | Added release+supply-chain pipeline (binary artifact, distroless+chainguard images, SBOM, SLSA, VEX) ADR-0006 | build.yaml, prepare-docker-binaries, Makefile, .vex/, calm | actionlint clean; make targets parse; calm-validate clean; auto-vex derivation binaries staged | ~milestone |
| 10:38 | Edited docs/adr/0006-release-and-supply-chain-pipeline.md | expanded (+10 lines) | ~477 |
| 10:39 | Edited docs/adr/0006-release-and-supply-chain-pipeline.md | 3→5 lines | ~98 |
| 10:39 | Edited docs/adr/0006-release-and-supply-chain-pipeline.md | Deferred() → verbatim() | ~82 |
| 10:39 | Edited docs/src/concepts/providers.md | modified run() | ~408 |
| 10:39 | Edited docs/architecture/calm/architecture.json | inline fix | ~130 |
| 10:40 | Created crates/banlieue-vex/Cargo.toml | — | ~197 |
| 10:40 | Created crates/banlieue-vex/src/lib.rs | — | ~238 |
| 10:40 | Edited Cargo.toml | 3→4 lines | ~28 |
| 10:40 | Edited .claude/CHANGELOG.md | expanded (+18 lines) | ~331 |
| 10:40 | Audited docs+deploy vs single-binary (ADR-0004); fixed stale provider-crate anatomy in providers.md (main.rs -> library/app.rs) | docs/src/concepts/providers.md | done | ~3k |
| 10:40 | Created crates/banlieue-vex/src/auto_vex_presence.rs | — | ~2143 |
| 10:41 | Created crates/banlieue-vex/src/auto_vex_reachability.rs | — | ~2101 |
| 10:41 | Created crates/banlieue-vex/src/bin/auto_vex_presence.rs | — | ~1258 |
| 10:42 | Created crates/banlieue-vex/src/bin/auto_vex_reachability.rs | — | ~1378 |
| 10:43 | Created crates/banlieue-vex/src/auto_vex_presence_tests.rs | — | ~4900 |
| 10:43 | Created crates/banlieue-vex/src/auto_vex_reachability_tests.rs | — | ~3297 |
| 10:46 | Edited .github/workflows/build.yaml | expanded (+190 lines) | ~2845 |
| 10:46 | Edited Makefile | 4→9 lines | ~115 |
| 10:46 | Edited Makefile | expanded (+34 lines) | ~546 |
| 10:46 | Edited Makefile | 2→3 lines | ~44 |
| 10:48 | Edited crates/banlieue-api/src/crddoc.rs | modified is_some() | ~94 |
| 10:49 | Edited crates/banlieue-api/src/bin/crddoc.rs | modified create_dir_all() | ~69 |
| 10:49 | Edited crates/banlieue-provider-vsphere/src/reconciler/provider.rs | 7→7 lines | ~74 |
| 10:49 | Edited crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs | 5→5 lines | ~48 |
| 10:49 | Edited crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs | modified contains() | ~64 |
| 10:50 | Edited .claude/CHANGELOG.md | modified verbatim() | ~746 |
| 16:00 | Ported auto-vex-presence + auto-vex-reachability from 5-spot into crates/banlieue-vex; wired CI jobs + Makefile; fixed 1.88 collapsible_if | banlieue-vex, build.yaml, Makefile, ADR-0006, crddoc/provider/vmimage | 339 tests green; clippy -D warnings clean; actionlint clean; bin smoke-tested | ~milestone |
| 10:57 | Created deploy/admission/README.md | — | ~301 |
| 10:58 | Created deploy/admission/virtualmachine-immutability.yaml | — | ~526 |
| 10:58 | Created deploy/admission/provider-immutability.yaml | — | ~413 |
| 10:58 | Created docs/src/guides/index.md | — | ~441 |
| 10:59 | Created docs/src/guides/core-controller.md | — | ~2045 |
| 11:00 | Created docs/src/guides/vsphere-provider.md | — | ~2546 |
| 11:00 | Created docs/src/developer/index.md | — | ~284 |
| 11:01 | Created docs/src/developer/local-development.md | — | ~1476 |
| 11:01 | Edited docs/mkdocs.yml | modified Reference() | ~375 |
| 11:01 | Edited docs/src/concepts/providers.md | inline fix | ~15 |
| 11:02 | Edited docs/src/reasoning/non-goals.md | inline fix | ~12 |
| 11:03 | Edited docs/src/index.md | 5→6 lines | ~98 |
| 11:03 | Edited docs/src/overview.md | 2→2 lines | ~27 |
| 11:03 | Edited docs/src/developer/index.md | 10→9 lines | ~125 |
| 11:03 | Edited docs/src/developer/local-development.md | inline fix | ~8 |
| 11:04 | Edited .claude/CHANGELOG.md | modified ValidatingAdmissionPolicies() | ~664 |
| 11:04 | Restructured docs: Why->Home, new Guides (core-controller+vsphere, ghcr.io v0.1.0) + Developer (local-dev) tabs; removed getting-started; added deploy/admission VAPs | docs/src/**, docs/mkdocs.yml, deploy/admission/** | done | ~30k |
| 15:57 | Created docs/adr/0007-admission-policies.md | — | ~1629 |
| 15:57 | Edited .github/workflows/docs.yaml | 13→14 lines | ~317 |
| 15:57 | Edited .github/workflows/docs.yaml | fails() → release() | ~218 |
| 15:57 | Edited .github/workflows/docs.yaml | release() → Pages() | ~108 |
| 15:58 | Edited .claude/CHANGELOG.md | expanded (+23 lines) | ~391 |
| 17:00 | Deploy docs to GitHub Pages on merge to main (interim) | .github/workflows/docs.yaml | actionlint clean; broadened deploy gate to push+refs/heads/main, release path retained | ~ci |
| 15:58 | Edited docs/architecture/calm/architecture.json | expanded (+21 lines) | ~422 |
| 15:58 | Edited docs/architecture/calm/architecture.json | 3→4 lines | ~30 |
| 15:58 | Edited deploy/admission/README.md | 3→6 lines | ~91 |
| 15:59 | Edited .claude/CHANGELOG.md | expanded (+22 lines) | ~356 |
| 15:59 | Created ADR-0007 (admission policies: VAP over webhook/CRD-CEL); added CALM control admission-policy-validation + registered ADR; calm-validate clean | docs/adr/0007-admission-policies.md, docs/architecture/calm/architecture.json, deploy/admission/README.md | done | ~5k |
