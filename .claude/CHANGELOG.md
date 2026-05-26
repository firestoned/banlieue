# Changelog

## [2026-05-25] - Move roadmap out of repo

**Author:** Erick Bourgeois

### Changed
- Moved `docs/roadmap/` → `~/dev/roadmaps/banlieue/` (out-of-repo). Reason: OSS projects should not ship the maintainer's planning artifacts. The numeric-prefix filename convention (`00-OVERVIEW.md`, `10-PHASE-1A-...`, etc.) is preserved at the new location.
- `.claude/CLAUDE.md`: Replaced the "Plans and Roadmaps → `docs/roadmap/`" rule with a "Plans and Roadmaps live outside the repo" rule. Updated the target file-organization tree to drop `docs/roadmap/` and add `docs/adr/` instead (ADRs stay in-repo because they're public technical records).
- `.claude/SKILL.md`: Stripped `docs/roadmap/` references from `regen-api-docs`, `update-docs`, `add-new-crd`, and the pre-commit checklist; clarified that phase plans live out-of-repo.
- `.github/workflows/build.yaml`: Removed the `# See docs/roadmap/10-PHASE-1A-...` comment pointer.
- `.wolf/cerebrum.md`: Updated the Phase-0 layout learning and the 2026-05-22 decision-log entry to point at the new location; added a new 2026-05-25 decision entry recording the move.

### What stays in-repo
- `docs/adr/` — Architecture Decision Records (lowercase-hyphen, `NNNN-title.md`).
- `docs/design/` — contract docs, diagrams.
- `docs/user/` — user-facing documentation (Phase 4).
- `examples/` — runnable YAML examples.

### Verification
- `cargo test --workspace --all-features` ✅ — 143 passed, 0 failed (no code changes; this just confirms nothing on the docs side broke compilation).
- `cargo run -p banlieue-api --bin crdgen --features crdgen` ✅ — still emits 6 CRDs.
- `grep -rln "docs/roadmap" --include="*.md" --include="*.toml" --include="*.yaml" --include="*.rs" .` returns only **intentional** mentions: the prohibition rule in `.claude/CLAUDE.md`, the decision-log entry in `.wolf/cerebrum.md`, and historical entries in `.wolf/buglog.json` and `.wolf/memory.md` (those are append-only audit logs and stay as-is).

---

## [2026-05-25] - Fix bug-006: IpamSpec CRD-generation

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-api/src/common.rs`: `IpamSpec` redesigned from a serde-tagged enum (`#[serde(tag = "source")]` with `Dhcp` / `Static` / `Pool` variants) into a flat struct: `IpamSpec { source: IpamSource, static: Option<StaticIpamConfig>, pool: Option<PoolIpamConfig> }`. New `IpamSource` is a plain enum that serializes as a lower-case string (`dhcp` / `static` / `pool`). Defaults to `Dhcp` with both sub-configs `None`.
- `crates/banlieue-api/src/common_tests.rs`: 4 new tests added (`ipam_source_default_is_dhcp`, `ipam_spec_default_is_dhcp_with_no_sub_configs`, `ipam_source_all_variants_round_trip`, `ipam_source_rejects_unknown_variant`). Existing IpamSpec tests rewritten for the flat shape.
- `crates/banlieue-api/src/banlieue/vmclass_tests.rs`: replaced `vmclass_crd_currently_panics_due_to_ipam_spec_bug` with `vmclass_crd_metadata_matches_kube_attributes` (positive assertion that the CRD generates and is cluster-scoped).
- `crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs`: replaced the two panic-pin tests with positive CRD-metadata assertions for both `VSphereMachine` and `VSphereMachineTemplate`.
- `examples/03-vmclass-db-prod-large.yaml`: rewrote the `pool` example to nest `poolRef` under `pool:` (matches new wire format).

### Why
On the upgraded toolchain (schemars 1 + kube 3), removing the variant-level doc comments from `IpamSpec` only changed the panic location: the new error makes clear that kube-derive's schema flattener *requires identical schemas for any property shared across oneOf subschemas*. By construction, every variant of a `#[serde(tag = "x")]` enum has a different value for `x`, so the panic is fundamental — not a metadata mismatch.

The Kubernetes-idiomatic shape (used by CAPI and others) is a flat struct whose discriminator is just a string field, with per-variant data nested under a sibling field of the matching name. That's what we adopted. Cross-field validation is intentionally left to the controller / future CEL rules.

This was the right time to break the wire format: there are no consumers yet (Phase 0), so the migration cost is zero. Once Phase 1A ships, breaking the wire format would require a CRD storage migration.

### Wire format change
**Before** (the tagged-enum shape, never actually deployable because CRD-gen panicked):
```yaml
ipam:
  source: pool
  poolRef:
    apiGroup: ipam.cluster.x-k8s.io
    kind: IPAddressClaim
    name: prod-pool
```

**After:**
```yaml
ipam:
  source: pool
  pool:
    poolRef:
      apiGroup: ipam.cluster.x-k8s.io
      kind: IPAddressClaim
      name: prod-pool
```

`static` follows the same nesting; `dhcp` needs nothing besides `source: dhcp`.

### Impact
- [x] Breaking change to the `IpamSpec` wire format (no consumers exist; safe)
- [ ] Requires cluster rollout (no controller yet)
- [x] Closes `.wolf/buglog.json` bug-006 — `cargo run -p banlieue-api --bin crdgen --features crdgen` now succeeds and emits all 6 CRDs

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features` ✅ — **143 passed** (was 139; +4 new `IpamSource` tests)
- `cargo run -p banlieue-api --bin crdgen --features crdgen | python3 -c "import yaml,sys; print(len(list(yaml.safe_load_all(sys.stdin))))"` → `6` ✅

---

## [2026-05-25] - Dependency + Edition Upgrade (align with kube-rs/controller-rs)

**Author:** Erick Bourgeois

### Changed
- `Cargo.toml`: Workspace dep & edition bump to match the kube-rs reference controller (`kube-rs/controller-rs`, pushed 2026-05-19).
  - `kube` `0.96` → `3` — features changed from `["derive", "client", "rustls-tls"]` (with `default-features = false`) to `["derive", "client", "runtime"]` (default TLS). The `runtime` feature is what unlocks `Controller::new`, `watcher`, `reflector`, `finalizer`, etc., for the upcoming `banlieue-controller` crate.
  - `k8s-openapi` `0.23` → `0.27`, feature `v1_31` → `latest` (auto-tracks the newest supported Kubernetes API). `schemars` feature retained.
  - `schemars` `0.8` → `1`.
  - `thiserror` `1` → `2`.
  - Added `tokio = "1"`, `tracing-subscriber = "0.3"`, `futures = "0.3"`, `anyhow = "1"` to `[workspace.dependencies]` so the upcoming controller/provider crates can pull them via `.workspace = true`.
  - Edition `2021` → `2024`. MSRV `1.80` → `1.85`.
- `crates/banlieue-api/src/banlieue/provider_tests.rs`: replaced `chrono_now()` helper (used the now-gone `k8s_openapi::chrono` re-export) with `parse_time(rfc3339)` that round-trips an RFC3339 string through `Time`'s `Deserialize` impl — works whether `Time` wraps `chrono::DateTime<Utc>` (old) or `jiff::Timestamp` (new in 0.27).
- Edition 2024 rustfmt rewrapped two `assert!(crd.spec.versions.iter()...)` chains into the new block style.

### Why
The user asked to align the project with kube-rs's own recommendations (`kube-rs/controller-rs`) and upgrade all deps to latest before the controller crate is implemented. Doing this now avoids a much larger rebase later, when the controller and 3+ provider crates have all locked onto the old versions.

### Impact
- [x] Breaking change for **downstream Rust consumers** (kube 3 reshaped its API surface — `kube::CustomResource` derive macro and runtime types). No external consumers exist yet.
- [ ] Requires cluster rollout (no controller yet)
- [x] Config change only (workspace `Cargo.toml`)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features` ✅ — 139 passed, 0 failed
- IpamSpec / kube-derive CRD-gen panic (bug-006) **still present** on schemars 1 + kube 3; the `*_currently_panics_due_to_ipam_spec_bug` pin tests continue to catch the panic, so no test had to be updated.

---

## [2026-05-24 19:30] - Comprehensive Unit Tests + Build Fixes

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/common_tests.rs`: 40 tests covering `InitializationStatus`, `MachineAddress`/`MachineAddressType`, `LocalObjectReference`, `TypedObjectReference`, `LabelSelector`/`Requirement`/`Operator`, `DiskProvisioning`, `Firmware`, `PowerState`, `IpamSpec` (Dhcp/Static/Pool), and the `condition_reasons`/`condition_types` constants. Positive (round-trip), negative (rejects unknown variant), and exception (missing required field) cases for every public type.
- `crates/banlieue-api/src/banlieue/provider_tests.rs`: 20 tests covering `ProviderCapabilities::is_empty` exhaustively, `ProviderSpec`/`ProviderStatus`/`ProviderConnection` round-trips, skip-serialization of `paused=false`/empty capabilities, `StorageClassMapping`/`NetworkClassMapping`, and `Provider::crd()` metadata.
- `crates/banlieue-api/src/banlieue/virtualmachine_tests.rs`: 23 tests covering `AffinityMode`/`MigrationPolicy` defaults + variants, `default_power_on`/`default_userdata_key` defaults via deserialization, `PlacementSpec`/`AntiAffinityRule`, `VirtualMachineSpec`/`Status`, `ScheduledPlacement`, `ResolvedResource`, and `VirtualMachine::crd()` metadata.
- `crates/banlieue-api/src/banlieue/vmclass_tests.rs`: 15 tests covering `HardwareSpec`/`DiskSpec`/`NetworkInterfaceSpec`, camelCase `memoryMiB`/`sizeGiB`/`storageClass` field naming, firmware/provisioning defaults, missing-required-field rejections, plus a pinned panic test for the IpamSpec/kube-derive CRD bug.
- `crates/banlieue-api/src/banlieue/vmimage_tests.rs`: 22 tests covering `OsFamily`/`Architecture`/`GuestAgent`/`ImageSourceKind` exhaustively, `ImageSource` with the `ref` rename and optional `importFrom`/`checksum`, `VMImageSpec`/`Status`, `ImagePerProviderStatus`, and `VMImage::crd()` metadata (cluster-scoped).
- `crates/banlieue-api/src/infrastructure/vsphere_machine_tests.rs`: 19 tests covering `VSphereDiskSpec`/`VSphereNicSpec`, the `providerID` rename, optional `folder`/`resourcePool`/`failureDomain`/`macAddress`, full `VSphereMachineSpec`/`Status` round-trips, `VSphereMachineTemplate`, plus pinned panic tests for both vSphere CRDs.

### Fixed
- `Cargo.toml`: Added the `schemars` feature to `k8s-openapi` so `Condition` and `Time` implement `JsonSchema`. Without this, the lib failed to compile because several CRD status structs contain `Vec<Condition>` / `Option<Time>` fields.
- `crates/banlieue-api/Cargo.toml`: Moved `serde_yaml` from `[dev-dependencies]` to an optional `[dependencies]` entry and made the `crdgen` feature pull it in (`crdgen = ["dep:serde_yaml"]`). The `crdgen` binary uses `serde_yaml` and previously could not link.
- `crates/banlieue-api/src/banlieue/vmimage.rs`: Removed unused `use crate::common::*;` import (was warning under `-D warnings`).
- `crates/banlieue-api/src/banlieue/vmclass.rs`: Inserted a blank line in a rustdoc block so clippy's `doc-lazy-continuation` lint is satisfied.

### Known Issues
- `VMClass::crd()`, `VSphereMachine::crd()`, and `VSphereMachineTemplate::crd()` panic at runtime because `IpamSpec` is a tagged enum and kube-derive's schema flattener disallows divergent discriminator metadata across variants. Logged as `bug-006` in `.wolf/buglog.json`; pinned by `*_currently_panics_due_to_ipam_spec_bug` tests so the fix surfaces automatically.

### Why
Adds a comprehensive unit test floor (139 tests) per the project's TDD rules, and unblocks the workspace which would not previously compile. Tests follow the project convention: separate `_tests.rs` files with `#[cfg(test)] #[path = "..."] mod foo_tests;` and an inner `mod tests`.

### Impact
- [x] Documentation only / non-breaking
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (Cargo.toml / Cargo.toml of `banlieue-api`)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features` ✅ — 139 passed, 0 failed
