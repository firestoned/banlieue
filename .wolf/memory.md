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
