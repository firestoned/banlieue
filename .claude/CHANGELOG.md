# Changelog

## [2026-09-05] - Fix bootstrap script side effect + document getting-started flow

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-k0s-cluster.sh`: guard the top-level `mkdir -p "$WORKDIR"` /
  `mkdir -p "$POOL_DIR"` calls so `--print-env-template` has zero side
  effects, matching its documented purpose (print a template and exit).
- `README.md`: new "Getting started" section documenting
  `bootstrap-k0s-cluster.sh` usage, with vSphere/libvirt subsections.
- `docs/src/guides/end-to-end-setup.md`: phase 0 now includes concrete
  per-backend command sequences instead of prose only.

### Why
`--print-env-template` unconditionally hit the top-level `mkdir -p
"$POOL_DIR"` (defaulting to `/var/lib/libvirt/images/k0s-bootstrap`) before
`main()` ever dispatched to the requested subcommand, because `BACKEND`
defaults to `libvirt` when unset — which it is on a first run, since
generating the env file via `--print-env-template` is how a vSphere operator
is meant to get `BACKEND=vsphere` into that file in the first place. This
failed with a permission error on any host without a real libvirt
installation (e.g. macOS). The docs gap was separate: the bootstrap script
existed and was referenced in guides, but had no copy-pasteable command
sequence for either backend.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (plus a one-line script bug fix)

## [2026-09-04] - Add deferred install mode for TPM-sealed Kairos encryption (ADR-0040)

**Author:** Erick Bourgeois (found live testing ADR-0039's vTPM attach end-to-end)

### Why
Live testing of ADR-0039 against the real vCenter failed install with
`Could not find TPM 2.0 device at /dev/tpmrm0`. Deep research into
`kairos-io/kcrypt`/`kairos-io/kairos` established that Kairos disk
encryption is install-phase-only, with no supported mechanism to encrypt an
already-installed disk later — and banlieue's vSphere pipeline installs
Kairos once into a golden template, then clones it for every production VM,
so the clone never re-runs the installer and a post-clone vTPM has nothing
to seal against. A second finding (vSphere's default clone behavior
duplicates a source VM's vTPM *and its secrets* onto the clone) ruled out
the obvious workaround of encrypting the golden template once. The only
combination giving every VM a genuinely unique, install-time-sealed key is
deferring the install to each clone's own first boot — which turned out to
already be `VMImageTemplate.autoManageInstall: false`'s exact mechanics
(ADR-0020's original template shape, preserved by ADR-0021 as an escape
hatch), just never named or documented for this use case. See ADR-0040.

### Changed
- `crates/banlieue-api/src/banlieue/vmimage.rs`: `VMImageTemplate.
  autoManageInstall: Option<bool>` replaced with `installMode: InstallMode`
  (`Immediate` default / `Deferred` / `Manual`) — a rename/clarification,
  not new vSphere mechanics; `Deferred` and `Manual` are mechanically
  identical to the old `false` today.
- `crates/banlieue-api/src/banlieue/mod.rs`: re-export `InstallMode`.
- `crates/banlieue-provider-vsphere/src/client/{mod.rs,vim.rs}`:
  `IsoImportRequest.autoManageInstall: bool` → `installMode: InstallMode`;
  `import_iso_template` now branches on `InstallMode::Immediate` instead of
  a bool.
- `crates/banlieue-provider-vsphere/src/import.rs`: CLI flag
  `--auto-manage-install <bool>` → `--install-mode <immediate|deferred|manual>`.
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`:
  `ImportForce`/`ImportJobInputs` thread `Option<InstallMode>` instead of
  `Option<bool>`.
- `examples/13-vmimage-kairos-deferred-install-tpm.yaml` (new): a
  `deferred`-mode `VMImage` pairing with `examples/12-vmclass-tpm-encrypted.
  yaml`, documenting the inverted cloud-config contract (`install.reboot:
  true` / `poweroff: false`, no identity-wipe stage — the opposite of
  `immediate` mode's template-building contract).
- `examples/07-vmimage-kairos-url-source.yaml`,
  `docs/src/guides/using-banlieue-imagebuilder.md`: mechanical rename to
  `installMode`, plus new guidance on `deferred` mode for `tpmEnabled` VMClasses.
- `docs/adr/0040-deferred-install-for-vtpm-encryption.md` (new, amends
  ADR-0039's now-wrong claim that no `VMImage` schema change was needed),
  `docs/architecture/calm/architecture.json` (VMImage data-asset + vSphere
  backend relationship descriptions updated).
- `deploy/crds/banlieue.io_vmimages.yaml`, `docs/src/reference/api.md`:
  regenerated (`make crds`).

### Verified
Confirmed working end-to-end, live, against the real vCenter (2026-09-04):
a `VirtualMachine` using a `tpmEnabled: true` `VMClass` paired with a
`Deferred`-mode `VMImage` was created and validated over SSH on its first
boot (~1 minute uptime). `/dev/tpm0`/`/dev/tpmrm0` were present, `sda5`
(`COS_PERSISTENT`) was `crypto_LUKS` and mounted read-write across every
`/var/lib/*`/`/etc/*` bind target, and `dmsetup ls` showed the LUKS mapping
open — the encrypted partition was created during that VM's own install and
auto-unlocked via its own vTPM with no manual intervention. First live
confirmation that ADR-0039 + ADR-0040 together produce a genuinely unique,
install-time-sealed encryption key per VM, not just a unit-tested code path.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] New opt-in capability (`installMode` defaults to `Immediate`,
      identical behavior to the old default `true`; existing `VMImage`s
      using the old `autoManageInstall` field name need a mechanical rename)

## [2026-09-04] - Add vSphere vTPM support for Kairos disk encryption (ADR-0039)

**Author:** Erick Bourgeois

### Why
The maintainer's vCenter had a KMS (Key Management Server) newly registered
under Configure -> Key Providers, unlocking virtual TPM (vTPM) devices —
which Kairos's `kcrypt` uses to seal LUKS keys locally per-VM, no remote
unlock server required. Manual `govc` investigation against the real
environment (checked 0.52.0 and 0.56.0, plus the upstream `USAGE.md`)
confirmed govc has no subcommand for attaching a vTPM to a VM — the real API
is a standalone `ReconfigVM_Task` with a `VirtualDeviceConfigSpec`/
`VirtualTPM` add, the same call the vCenter UI and PowerCLI's `New-VTpm`
make. See ADR-0039 for the full design (why this is a class-level capability
like `firmware`, not a per-VM override or a `VMImage` field).

### Changed
- `crates/banlieue-api/src/banlieue/vmclass.rs`: `VMClassSpec.tpmEnabled: bool`
  (default `false`), sibling to `firmware`.
- `crates/banlieue-api/src/infrastructure/vsphere_machine.rs`:
  `VSphereMachineSpec.tpmEnabled: bool`; `VSphereMachineStatus.tpmAttached:
  Option<bool>`.
- `crates/banlieue-controller/src/reconciler/scheduler.rs`: new `FEATURE_VTPM`
  ("vtpm") constant and filter step — a candidate failure domain must
  advertise it when `tpmEnabled` is set, else `ScheduleError::TpmUnsupported`.
- `crates/banlieue-controller/src/reconciler/infra.rs`: resolves
  `VSphereMachineSpec.tpmEnabled` from `VMClass.spec.tpmEnabled`.
- `crates/banlieue-provider-vsphere/src/client/{mod.rs,vim.rs,fake.rs}`: new
  `VSphereClient::add_tpm_device` trait method; `vim.rs` issues the
  `ReconfigVM_Task`/`VirtualTPM` add; `fake.rs` tracks it for tests.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  `ensure_vm` calls `add_tpm_device` after `clone_vm` (already clones powered
  off) and before the power-on step, so the device exists before Kairos's
  first boot — a hard requirement for `kcrypt` to seal against it during
  unattended install. `ProvisionOutcome.tpm_attached` flows into
  `VSphereMachineStatus.tpmAttached`. `patch_status_success` now takes the
  whole `ProvisionOutcome` rather than separate fields, to stay under
  clippy's `too_many_arguments` threshold.
- `docs/adr/0039-vsphere-vtpm-support.md` (new), `docs/architecture/calm/
  architecture.json` (VMClass/Provider/vSphere-backend relationship
  descriptions updated), `examples/12-vmclass-tpm-encrypted.yaml` (new).
- `deploy/crds/banlieue.io_vmclasses.yaml`,
  `deploy/crds/infrastructure.banlieue.io_vspheremachines.yaml`,
  `deploy/crds/infrastructure.banlieue.io_vspheremachinetemplates.yaml`,
  `docs/src/reference/api.md`: regenerated (`make crds`).

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] New opt-in capability (`tpmEnabled` defaults to `false`; existing
      VMClasses/VirtualMachines are unaffected)

## [2026-09-03] - Fix vSphere provider hot-loop on desiredPowerState: PoweredOff

**Author:** Erick Bourgeois (found live testing ADR-0038 userData end-to-end)

### Why
Testing `VirtualMachine.spec.userData` (ADR-0038) end-to-end against the real
on-prem vCenter surfaced a pre-existing, unrelated bug: `ensure_vm`'s create
path clones the VM, then unconditionally calls `set_power_state(vm_ref,
spec.desired_power_state)`. `CloneVM_Task` always clones powered off
(ADR-0024's clone spec sets `power_on: false`), so requesting
`desiredPowerState: PoweredOff` drove a redundant `PowerOffVM_Task` on an
already-off VM. Real vCenter rejects that with `InvalidPowerState`, and
because that error propagates via `?` before `ensure_vm` returns, the caller
never learns the new `vm_ref` and never patches `VSphereMachine.status`. Every
subsequent reconcile re-clones from scratch and immediately fails with
`DuplicateName` — a hot loop that never self-heals and orphans a real VM in
vCenter. Every existing example manifest uses `PoweredOn`, which never
exercises this path (powering on a freshly-off clone is a valid transition),
so this had gone unnoticed.

### Changed
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  `ensure_vm` now skips the `set_power_state` call entirely when
  `spec.desired_power_state == PowerState::PoweredOff`, since the clone is
  already in that state.
- `crates/banlieue-provider-vsphere/src/client/fake.rs`: `FakeClient::set_power_state`
  now mirrors real vCenter's `InvalidPowerState` fault when asked to
  transition a VM to the state it's already in, and tracks call counts via
  the new `power_state_call_count` so tests can assert a redundant call was
  skipped, not merely that it "succeeded".
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine_ensure_tests.rs`:
  added `desired_power_off_skips_the_redundant_power_state_call`, a
  regression test reproducing the hot loop against the now-stricter
  `FakeClient`.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout (rebuild/redeploy `banlieue-provider-vsphere`)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-31] - Docs accuracy audit: align docs/, README, and examples with shipped code

**Author:** Devin (docs audit requested by Erick Bourgeois)

### Why
A full pass comparing `docs/src/`, `README.md`, `docs/README.md`, and
`examples/*.yaml` against the actual `crates/banlieue-api` types, controller/
provider reconciler code, and generated CRDs (`deploy/crds/`) turned up
several categories of drift accumulated across recent ADRs (0011, 0024,
0031, 0036, 0037, 0038): stale pre-refactor `VirtualMachine` field names,
a stale "not yet wired in" claim about the vSphere `VSphereMachine`
reconciler, `cloudConfig` (singular) vs. the current `cloudConfigs[]` list,
non-existent schema fields/enum values referenced in guides, and an outdated
"Proxmox and libvirt next" framing that no longer reflects libvirt's actual
(partial) implementation state.

### Changed
- `README.md`: repository layout tree (added the 5 crates missing from it —
  `banlieue-operator`, `banlieue-imagebuilder`, `banlieue-libvirt`,
  `banlieue-provider-libvirt`, `banlieue-vex`), CRD table (added
  `ProviderClass`), project status paragraph, and the vSphere/libvirt/Proxmox
  maturity framing.
- `docs/src/index.md`, `docs/src/overview.md`, `docs/src/concepts/virtualmachine.md`:
  replaced the pre-refactor `spec.class` / `spec.image` / `spec.providerRef`
  example shape with the actual `classRef` / `imageRef` /
  `placement.providerSelector`; fixed the condition-type table (`Ready`,
  `Scheduled`, `PlacementValid`, `InfrastructureReady`, `Migrating` — not
  `Provisioned`/`ImageReady`/`Failure`); added caveats where diagrams depict
  `ProxmoxMachine`/`LibvirtMachine` as if they exist today.
- `docs/src/concepts/providers.md`: removed the "govmomi"/"proxmoxer" claim
  (neither is usable from Rust; the real clients are the first-party `vim_rs`
  BYOC client and the first-party `banlieue-libvirt` RPC client);
  `ProviderClass` is implemented today, not "future"; refreshed the
  `banlieue-provider-vsphere` reconciler directory listing to include
  `vspheremachine.rs`.
- `docs/src/concepts/architecture.md`, `docs/src/guides/using-banlieue-imagebuilder.md`:
  `cloudConfig` (singular) → `cloudConfigs[]` (ADR-0037); `template.network`
  scalar → list-of-objects (ADR-0031).
- `docs/src/guides/vsphere-provider.md`, `docs/src/guides/environment-provider-isolation.md`:
  removed the non-existent `ipam.source` field; `reason=NoCandidates` →
  the real `NoProviderMatched`.
- `docs/src/guides/building-kairos-hadron-template.md`: removed a reference
  to a non-existent `ImageSourceKind::Oci` variant.
- `docs/src/guides/core-controller.md`: CRD-apply sample output now includes
  `providerclasses.banlieue.io`; admission-policy table expanded from 2 to
  all 7 shipped policies.
- `docs/README.md`: directory tree corrected (removed a nonexistent
  `getting-started/`, added `guides/`, `developer/`, `architecture/`).
- `docs/architecture/calm/architecture.json`: corrected the same
  class/image/providerRef → classRef/imageRef/placement.providerSelector
  drift, the stale "VSphereMachine not yet wired in" claim, the swap-provider
  flow (now framed around `placement.providerSelector` and honest about only
  being exercisable between two vSphere `Provider`s today), `cloudConfig` →
  `cloudConfigs[]`, added the missing `banlieue-providerclass-guardrails`
  admission control and ADRs 0016/0026–0038 to the model, and refreshed
  `metadata.phase`/`metadata.crds`. Regenerated `docs/src/architecture/{system,flows}.md`
  via `make calm-diagrams` (validated with `make calm-validate`).
- `crates/banlieue-provider-vsphere/src/reconciler/mod.rs`: fixed a stale
  module doc comment claiming the `VSphereMachine` reconciler isn't wired
  into `app.rs` — it is (create-path only; confirmed against `app.rs`).
- `examples/10-virtualmachine-static-ip.yaml`: fixed `userData.secretRef.key`
  being mis-indented as a sibling of `secretRef` instead of nested inside it
  (silently pruned by the CRD schema); updated the stale "not wired in yet"
  comment about the vSphere machine reconciler.
- `examples/03-vmclass-db-prod-large.yaml`: removed the non-existent
  `ipam.source` field; added a caveat that pool-based IPAM (ADR-0033) is
  schema-only and not yet reconciled.
- `examples/04-vmimage-ubuntu.yaml`: labeled the all-zeros `checksum` as an
  explicit placeholder.
- Regenerated `docs/src/reference/api.md` and `deploy/crds/*.yaml` via
  `make crds` to confirm they were already in sync (no diff).

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-27] - userData: support both Secret and ConfigMap sources — ADR-0038

**Author:** Cascade (pair-programming with Erick Bourgeois)

### Breaking Change
- `UserDataSpec` shape changed from `{ secretRef: LocalObjectReference, key: String }`
  to `{ secretRef: Option<KeySelector>, configMapRef: Option<KeySelector> }`. Exactly
  one must be set (follows `CABundleSource`'s exactly-one-of pattern). The `key` moves
  inside each ref (via `KeySelector`), defaulting to `user-data`. This is a `v1alpha1`
  breaking change to existing `VirtualMachine` manifests.

### Added
- `UserDataSpec.validate()`: enforces the exactly-one-of `secretRef`/`configMapRef`
  invariant.
- `DEFAULT_USER_DATA_KEY` constant (`"user-data"`) in `virtualmachine.rs`, exported
  from `banlieue_api::banlieue`.
- `resolve_configmap_data()` in `banlieue-controller`'s VM reconciler: reads a single
  key from a ConfigMap (mirrors the existing `resolve_secret_data` for Secrets).
- `docs/adr/0038-userdata-configmap-support.md`: records the design decision.
- `examples/11-virtualmachine-configmap-userdata.yaml`: ConfigMap-based userData example.
- 7 new/updated tests in `virtualmachine_tests.rs` covering both sources and all
  `validate()` branches.

### Changed
- `crates/banlieue-api/src/banlieue/virtualmachine.rs`: redesigned `UserDataSpec`.
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs`:
  `resolve_rendered_user_data` now dispatches to `resolve_secret_data` or
  `resolve_configmap_data` based on which source is set.
- `deploy/controller/rbac/role.yaml`: added `configmaps` to the `get` grant;
  renamed `banlieue-controller-secrets` → `banlieue-controller-userdata`.
- `deploy/controller/rbac/rolebinding.yaml`: updated to match renamed Role.
- `examples/05-virtualmachine.yaml`: updated to new `UserDataSpec` shape.
- Regenerated CRDs (`deploy/crds/banlieue.io_virtualmachines.yaml`) and API docs.

## [2026-08-25] - VMImage: layered cloud-config (base + overlays) — ADR-0037

**Author:** Cascade (pair-programming with Erick Bourgeois)

### Breaking Change
- `VMImageSpec.cloudConfig` (singular `Option<CloudConfigSource>`) replaced by
  `cloudConfigs` (plural `Vec<CloudConfigSource>`). Empty list = no cloud-config.

### Added
- `crates/banlieue-imagebuilder/src/cloud_config_merge.rs`: pure YAML deep-merge
  helper — maps deep-merge (later wins), lists concatenate, type-mismatch is a
  hard error. 10 unit tests.
- `merge_and_apply_cloud_configs()` in the imagebuilder reconciler: fetches each
  referenced Secret, merges their YAML, SSA-applies a single
  `<vmimage-name>-cloud-config-merged` Secret (owner-referenced to the VMImage),
  and passes *that* to `OSArtifact.spec.artifacts.cloudConfigRef`.
- `MergeError` variant added to `banlieue_imagebuilder::Error`.
- RBAC: `deploy/imagebuilder/rbac/clusterrole.yaml` now grants
  `get/list/watch/create/patch` on `secrets` (cloud-config content + merged
  Secret, not credentials).

### Changed
- `crates/banlieue-api/src/banlieue/vmimage.rs`: `cloud_config` → `cloud_configs`.
- `crates/banlieue-imagebuilder/Cargo.toml`: added `serde_yaml` dependency.
- Updated all test files across controller, provider-vsphere, provider-libvirt.
- `examples/07-vmimage-kairos-url-source.yaml`: `cloudConfig` → `cloudConfigs` (list).
- Regenerated CRDs (`deploy/crds/banlieue.io_vmimages.yaml`) and API docs.

## [2026-08-24 23:30] - Upgrade sha2 to 0.11.0 and vim_rs to 0.6.0

**Author:** Erick Bourgeois

### Changed
- `Cargo.toml`/`Cargo.lock`: `sha2` 0.10→0.11, `vim_rs` 0.5→0.6 (`digest`
  0.10→0.11, `block-buffer`/`crypto-common` bumped, `generic-array` replaced
  by `hybrid-array`, `vim_macros` 0.5→0.6) — this half already landed on
  this branch via a concurrent commit; verified here rather than re-applied.
- `crates/banlieue-provider-libvirt/src/import.rs`,
  `crates/banlieue-provider-vsphere/src/import.rs`: `verify_checksum`'s
  `format!("{:x}", h.finalize())` no longer compiles under `sha2` 0.11 —
  `digest`'s `Output` type moved from `generic_array::GenericArray` (which
  implemented `LowerHex`) to `hybrid_array::Array` (which does not). Added
  a small local `hex_encode(&[u8]) -> String` helper in each file (no new
  dependency) and switched both call sites to it.
- No code changes needed for the `vim_rs` 0.5→0.6 bump — the crate builds
  and every existing test passes unchanged; the version bump did not touch
  any API surface `banlieue-provider-vsphere`'s BYOC client actually uses.

### Why
Both were open, previously-unmerged Dependabot PRs (#13, #12) that failed
CI as originally opened. Picked up directly in this branch per request:
bump, let the compiler point at exactly what broke, fix it, verify nothing
else regressed. `sha2`'s break was real and mechanical (confirmed via the
actual compiler error, not guessed); `vim_rs`'s bump turned out to be a
no-op for this codebase's usage despite being a semver-breaking range for
a pre-1.0 crate (ADR-0008 already flags `vim_rs` as "expect breaking
changes pre-1.0" — this particular bump just didn't hit anything we call).

### Verification
- `cargo build --workspace`: clean.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --all-targets --all-features -- -D warnings`: clean.
- `cargo test --workspace`: every crate passes; the sole failure
  (`client::vim::vim_tests::request_times_out_against_a_hung_endpoint`) is
  the same pre-existing local-environment flake (proxy interception on
  `127.0.0.1`, confirmed via identical HTTP 403 signature) diagnosed
  earlier this session — reproduced in isolation, unrelated to this
  upgrade, and in an untouched test file.

### Impact
- [x] Requires cluster rollout (every provider/controller image rebuilds
      against the new lockfile)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-24 23:10] - Fix: FQDN placeholders/guestinfo double-appended domain onto an already-qualified VM name

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-sdk/src/guestdata.rs` (`render_placeholders`'s
  `${FQDN}` substitution) and
  `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`
  (`build_guestinfo_metadata`'s `local-hostname`): both unconditionally
  appended `.{domain}` to the VM name. `metadata.name` is a DNS-1123
  subdomain and permits dots (confirmed live earlier this session — a
  `VirtualMachine` named as a full FQDN applies cleanly with zero code
  changes), so a VM already named e.g. `db-01.example.com` with
  `domain: example.com` rendered as `db-01.example.com.example.com`.
  Both now check (case-insensitively, since DNS names are
  case-insensitive) whether the VM name already ends with `.{domain}`
  before appending — extracted as a small pure `fqdn`/`local_hostname`
  helper in each file so the fix is unit-tested directly.

### Why
Asked to stop double-appending the domain in the guestinfo calls, as a
direct follow-up to establishing that a `VirtualMachine`'s `metadata.name`
can itself already be a full FQDN.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding/redeploying `banlieue-controller` (uses
      `render_placeholders` for `spec.userData` rendering) and
      `banlieue-provider-vsphere`.

### Verification
- `cargo fmt` / `cargo clippy --all-targets --all-features -- -D warnings`
  for both crates ✅
- `cargo test -p banlieue-provider-sdk -p banlieue-provider-vsphere` ✅ —
  `banlieue-provider-sdk` 63 passed (+3), `banlieue-provider-vsphere` 180
  passed (+1)

## [2026-08-24 22:45] - Scope live migration: same-class first, cross-class deferred (ADR-0036)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0036-live-migration-phased-approach.md` — Proposed. Design-only,
  no code: splits "live migration" into same-provider-class (Phase A,
  e.g. vSphere `RelocateVM_Task`/vMotion-equivalent — buildable today) and
  cross-provider-class (Phase B, e.g. vsphere→libvirt — needs a portable
  disk artifact contract that doesn't exist anywhere in this codebase
  yet, explicitly out of scope for now). Also decides: `Recreate` needs
  its own distinct status reason so it's never visually conflated with a
  future real `Migrating` state.
- `~/dev/roadmaps/banlieue/51-LIVE-MIGRATION.md` — Phase A execution plan
  (preconditions, per-provider-class relocate-capability research,
  schema/reconciler/provider/test/doc tasks, open questions, gotchas),
  following the same "deferred feature" shape as `50-IPAM-POOL-INTEGRATION.md`.
  Added to the roadmap README's index table.

### Why
Directly motivated by ADR-0035 landing moments earlier: making placement
drift detection prompt (event-driven) means the existing recreate-only
migration path (`migration.rs`, already self-documented as "Phase 2 work"
for live migration) can now fire sooner and more often under
`migrationPolicy: Automatic` — the gap between detecting drift and
handling it gracefully is now more consequential than when both were
equally slow. User asked explicitly to "ADR this up and create a roadmap."

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only — no code changes; `migrationPolicy: Automatic`
      continues to mean destroy-and-rebuild until Phase A is actually
      implemented per the new roadmap.

## [2026-08-24 22:20] - VirtualMachine controller now watches Provider and VMClass (ADR-0035)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0035-virtualmachine-watches-provider-and-vmclass.md`.
- `crates/banlieue-controller/src/app.rs`: the `VirtualMachine` `Controller`
  now `.watches()` `VMClass` (filtered by `spec.classRef.name`, mirroring
  the existing `VMImage` watcher) and `Provider` (unfiltered — requeues
  every `VirtualMachine` in the store, mirroring the `VSphereCluster`
  controller's own existing `Provider` watcher, since
  `providerSelector`/`failureDomainSelector` match by label with no name
  to filter on).

### Why
Live report: editing a `Provider`'s labels to fix a selector mismatch
against an existing `VirtualMachine` didn't re-trigger scheduling —
`reconcile()` reads `Provider`/`VMClass` fresh every pass, but nothing
watched either for changes, so a VM only ever noticed on the next
`requeue_default()` tick. Confirmed self-corrects eventually via that
poll, but the point of `.owns()`/`.watches()` elsewhere in this codebase
is exactly to avoid depending on polling for reactions to real events.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding/redeploying `banlieue-controller`.

### Verification
- `cargo fmt -p banlieue-controller -- --check` ✅
- `cargo clippy -p banlieue-controller --all-targets --all-features -- -D warnings` ✅
- `cargo test -p banlieue-controller` ✅ (87 passed — watch-wiring itself
  isn't unit-testable without a fake kube client, per this codebase's own
  established convention; verified by build + live behavior only)

## [2026-08-24 21:50] - Fix banlieue-controller's own narrow status patches + detect-and-report self-heal (ADR-0034)

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs`: `patch_status`
  and `patch_status_conditions_only` had the *same* bug class as the
  vsphere-provider fix earlier today, but worse and pre-existing — they had
  never forwarded `initialization`/`addresses` at all (confirmed live:
  `VirtualMachine.status.initialization` stayed `{}` forever even on a
  fully provisioned VM), and once ADR-0034 added `observedPowerState` to
  the mirror, that got silently dropped too. Fixed by collapsing both
  functions into a single `patch_status(api, name, status: &VirtualMachineStatus)`
  that always sends the complete status object; `patch_scheduling_failure`,
  `patch_infra_build_failure`, and `patch_placement_invalid` now start from
  `vm.status.clone()` (not `Vec::new()`) so they preserve every field a
  prior successful patch had set, instead of retracting/wiping it under
  SSA's "same manager, narrower field set" semantics. `mirror_only_path`
  now calls the same unified `patch_status` — `next_status` already
  carries `scheduled`/`infrastructureRef` forward unchanged from
  `mirror_status_from_infra`, matching that path's existing "keep the
  placement" contract.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`
  (`refresh_power_state`): added detect-and-report self-healing for two
  inconsistent states — `provisioned=true` with no `vmRef`
  (`BackendRefMissing`) and a stored `vmRef` that no longer resolves in
  vCenter (`BackendMissing`, detected via the same `ManagedObjectNotFound`
  string match `destroy_vm` already uses). Both patch `Ready=False` with a
  descriptive message instead of silently doing nothing or retrying
  forever on an opaque error; `Ready` is restored to `True`/`Reconciled`
  automatically the next time a `power_state` read succeeds. Deliberately
  does **not** rediscover the VM by name or auto-recreate it — see
  ADR-0034 Decision #7 for why (round-trip cost, risk of adopting/creating
  the wrong VM).
- `docs/adr/0034-vspheremachine-observed-power-state.md`: added Decision
  #6 (full-status-always) and #7 (detect-and-report self-heal).

### Why
Live report: `kubectl get vm erick-rhel -o yaml | grep observedPowerState`
came back empty even after redeploying and recreating the VM — the
provider-side fix from the previous entry was correct, but
`banlieue-controller`'s own status patch was independently dropping the
field before it ever reached the parent `VirtualMachine`. Then, asked
whether startup reconciliation should "grab all CRs and update/remove
fields" — clarified to mean: make the already-provisioned fast path
self-healing for status inconsistencies, scoped to detect-and-report only
(not rediscovery/recreation, per explicit direction).

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding/redeploying both `banlieue-controller` and
      `banlieue-provider-vsphere`.

### Verification
- `cargo fmt` / `cargo clippy --all-targets --all-features -- -D warnings`
  for both crates ✅
- `cargo test -p banlieue-provider-vsphere -p banlieue-controller` ✅ —
  `banlieue-controller` 87 passed, `banlieue-provider-vsphere` 179 passed
  (+3: `is_backend_missing_error_*`,
  `status_reporting_backend_problem_preserves_every_other_field`,
  `status_with_observed_power_state_restores_ready_after_a_backend_problem_clears`)
- `make calm-validate` ✅

## [2026-08-24 21:05] - Fix: ADR-0034's power-state refresh orphaned backend VMs on delete

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`
  (`refresh_power_state`): the narrow `{observedPowerState,
  observedGeneration}` JSON patch it applied via SSA was a real data-loss
  bug, not just a style choice. Live report: after deleting two
  `VirtualMachine`s, both backend VMs stayed running in vCenter — the CRs
  themselves deleted cleanly (no stuck finalizer), but `destroy_vm` was
  never called. Root cause, confirmed from logs: the same field manager
  (`FIELD_MANAGER_PROVIDER_VSPHERE`) that previously applied the *full*
  `VSphereMachineStatus` struct (`patch_status_success`: `vmRef`,
  `conditions`, `initialization`, etc.) later re-applied a narrower object
  containing only `observedPowerState`/`observedGeneration`. Under SSA,
  the same manager re-applying a smaller field set makes the apiserver
  retract that manager's ownership of every omitted field — and since
  nothing else owned `vmRef`/`conditions`/`initialization`, they were
  wiped. `finalize()` then read `vm_ref` as `None` and skipped
  `destroy_vm` entirely. (A separate, real issue compounded this in the
  live report: the narrow patch had been erroring for a while first —
  `.status.observedPowerState: field not declared in schema` — meaning the
  live CRD was temporarily out of sync with the deployed binary; the wipe
  only actually landed once that schema mismatch got resolved and the
  patch started succeeding.)
- Fixed by always re-applying the *entire* current status (cloned from
  `machine.status`, only `observedPowerState`/`observedGeneration`
  overridden) rather than a hand-built partial object — extracted as a
  pure `status_with_observed_power_state` helper specifically so this
  field-preservation contract is unit-tested without a kube client.

### Why
Direct consequence of this session's earlier ADR-0034 change
(2026-08-24 20:10 entry) — found live immediately after deploying it.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding/redeploying `banlieue-provider-vsphere`. Any
      `VSphereMachine` that already lost `status.vmRef` to this bug will
      need its backend VM cleaned up manually in vCenter — banlieue has
      no way to find/destroy a VM it no longer has a moref for.

### Verification
- `cargo fmt -p banlieue-provider-vsphere -- --check` ✅
- `cargo clippy -p banlieue-provider-vsphere --all-targets --all-features -- -D warnings` ✅
- `cargo test -p banlieue-provider-vsphere` ✅ (176 passed, +1 — the new
  `status_with_observed_power_state_preserves_every_other_field` regression
  test)

## [2026-08-24 20:10] - Add observed VM power state (ADR-0034) + more info logging in ensure_vm

**Author:** Erick Bourgeois

### Added
- `docs/adr/0034-vspheremachine-observed-power-state.md` — new ADR, amending
  ADR-0024's create-path-only scoping decision.
- `crates/banlieue-api/src/infrastructure/vsphere_machine.rs`:
  `VSphereMachineStatus.observed_power_state: Option<PowerState>` + a new
  `Power` printcolumn (mirrors `VirtualMachine`'s own, which already existed
  but nothing ever populated it).
- `VSphereClient::power_state(&self, vm_moref) -> Result<PowerState>` — new
  read-only trait method reading `VirtualMachine.runtime.powerState`, the
  read-only counterpart to the existing `set_power_state`. Implemented for
  `VimClientImpl` (via new pure helper `map_vim_power_state`) and
  `FakeClient` (reusing its existing `power_states` fixture map).
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  once provisioned, `reconcile` now performs exactly one cheap
  `power_state` read per pass (`refresh_power_state`) and patches
  `status.observedPowerState` only when it actually changed — narrowing,
  not reversing, ADR-0024's "no vCenter round-trip once provisioned" rule.
  `ProvisionOutcome` gained a `power_state` field so the initial
  post-clone/power-on value is recorded without a second read.
- `crates/banlieue-controller/src/reconciler/status_mirror.rs`:
  `InfraMachineRead::observed_power_state()` + mirrored onto the parent
  `VirtualMachine.status.observedPowerState`.
- Added `info!` logs across every step of `ensure_vm` (datacenter/cluster/
  template/datastore/network resolution, clone submission/completion,
  power-on request/confirmation) — previously zero log lines existed
  between "reconciling" and "provisioned".
- CRDs regenerated (`make crds`); `docs/src/reference/api.md` and
  `docs/site/` rebuilt (`make docs`).

### Why
Asked for logging across the create-to-boot sequence, then clarified the
actual need: `VirtualMachine`/`VSphereMachine` never reported the backend
VM's power state at all (a `Power` printcolumn already existed on
`VirtualMachine` but nothing ever wrote to it) — live report: two VMs up
and running in vCenter, `Power` column empty on both.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding/redeploying `banlieue-provider-vsphere` and
      `banlieue-controller`, and re-applying regenerated CRDs.

### Verification
- `cargo fmt` / `cargo clippy --all-targets --all-features -- -D warnings`
  for `banlieue-api`, `banlieue-controller`, `banlieue-provider-vsphere` ✅
- `cargo test` ✅ — `banlieue-api` 284 passed, `banlieue-controller` 87
  passed (+1), `banlieue-provider-vsphere` 175 passed (+2)
- `make calm-validate` ✅ (no CALM model change needed — this refines an
  existing node's status/behavior, not the architecture graph)

## [2026-08-24 18:55] - Fix: VirtualMachine.status.Ready stuck at Scheduling forever

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs`: the
  success path's aggregate `Ready` condition never became `True` even once
  scheduling, infra provisioning, and the backend VM were all genuinely
  successful — found live, two VMs fully up and reachable in vCenter, both
  still reporting `VirtualMachine.status` `Ready=False reason=Scheduling`,
  `InfrastructureReady=True`, and a fully populated `status.scheduled`.
  Root cause: `status_mirror.rs`'s `mirror_status_from_infra` computes
  aggregate `Ready` from a `Scheduled` *condition* in `conditions[]`, not
  from the `status.scheduled` struct field — but the only two call sites
  that ever set that condition (`patch_scheduling_failure`,
  `patch_infra_build_failure`) both set it `False`; nothing on the success
  path ever set it `True`. The success path now seeds `Scheduled=True` on
  the pre-mirror status snapshot (`vm.status.clone()`) before calling
  `mirror_status_from_infra` directly (rather than the `mirror_onto_vm`
  wrapper), so the aggregate computation sees it in the same reconcile
  pass rather than one pass late.

### Why
Live report: two freshly created `VirtualMachine`s were up and running per
vCenter, but `status.conditions` never reflected it.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding/redeploying `banlieue-controller`.

### Verification
- `cargo fmt -p banlieue-controller -- --check` ✅
- `cargo clippy -p banlieue-controller --all-targets --all-features -- -D warnings` ✅
- `cargo test -p banlieue-controller` ✅ (86 passed) — the pre-existing pure
  test `aggregate_ready_is_true_when_scheduled_placement_valid_and_infra_ready`
  in `status_mirror_tests.rs` already covered the correct aggregation logic
  given `Scheduled=True`; the gap was entirely in the (deliberately
  untested-at-this-level, per this crate's own convention) caller never
  supplying that precondition.

## [2026-08-24 13:15] - Fix: force-reimport annotation caused a delete/recreate storm

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`: the
  `banlieue.io/force-reimport` annotation was read on every `reconcile()`
  pass but never cleared, so it behaved as a standing flag instead of a
  one-shot trigger. Because the `VMImage` controller also watches the
  import Job (event-driven reconciliation added earlier this session),
  every Job status change (a pod flipping `Pending`→`Running`, etc.)
  re-triggered `reconcile()`, which still saw the annotation `true` and
  deleted + recreated the Job again — an unbroken delete/recreate storm
  across all three failure domains, observed live as import pods being
  created and terminated within the same second, forever, with no import
  ever surviving long enough to progress.
- `reconcile()` now clears the annotation via a JSON Merge Patch
  immediately after acting on it in a given pass. Added
  `clear_force_reimport_patch()` (pure, unit-tested) building the patch
  value; `finalizer.rs`'s `ensure_finalizer`/`remove_finalizer` already
  established this same Merge Patch pattern for exactly this reason
  (doesn't disturb metadata owned by other controllers).
- Manually cleared the annotation on the live, already-looping `VMImage` to
  stop the storm immediately, ahead of deploying this fix.

### Why
Live report: after annotating `hadron-kairos-dev-v0.1.0` with
`force-reimport=true` to test the PCI-slot fix (previous two CHANGELOG
entries), the import Jobs never got past `Pending` — pods were being
deleted and recreated in a tight loop across all three zones.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding/redeploying this provider's image before
      `force-reimport` can be used again without re-triggering the loop.

### Verification
- `cargo fmt -p banlieue-provider-vsphere -- --check` ✅
- `cargo clippy -p banlieue-provider-vsphere --all-targets --all-features -- -D warnings` ✅
- `cargo test -p banlieue-provider-vsphere` ✅ (173 passed)

## [2026-08-24 00:35] - Fix: ethernetN.pciSlotNumber must be set in a separate ReconfigVM_Task

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: live validation via
  `govc vm.info -e` on templates rebuilt with the previous fix (removing the
  harmful structured-`slotInfo` post-create reconfigure) still showed
  `ethernet0.pciSlotNumber: 33`, not the requested `192` — proving that fix
  was necessary but not sufficient. Root cause: setting
  `ethernetN.pciSlotNumber` as an ExtraConfig entry *inline*, in the same
  `CreateVM_Task` that also creates the NIC device via `device_change`,
  doesn't stick either — the freshly created VM's own config still read
  back vCenter's auto-assigned slot. `build_template_config_spec` no longer
  sets `extra_config` at all. Added
  `build_nic_pci_slot_extra_config_reconfigure_spec` — a new, genuinely
  separate post-create `ReconfigVM_Task` with `device_change: None`, run
  after `CreateVM_Task` has already committed the NIC — matching
  `create-kairos-template.sh`'s own reference sequence exactly: a bare
  `govc vm.create`, then a wholly separate `govc vm.change -e
  "ethernet0.pciSlotNumber=192"`.
- `vim_tests.rs`: replaced
  `build_template_config_spec_sets_ethernet0_pci_slot_number_as_extra_config`
  (now asserts the CreateVM_Task spec leaves `extra_config` unset) with
  `build_nic_pci_slot_extra_config_reconfigure_spec_sets_ethernetn_pci_slot_number`,
  covering the new post-create-only reconfigure for multiple NICs.

### Why
Third attempt at the same live bug (`ens33` guest interface instead of the
requested `ens192`). Each prior attempt was disproven by direct evidence,
not assumption: `govc vm.info -e` against the actual rebuilt template is
what caught that the previous fix (this file, 2026-08-23 23:55 entry) was
incomplete.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding this provider's image AND force-recreating any
      existing templates (`banlieue.io/force-reimport`) — every template
      built before this fix still has the wrong `ethernet0.pciSlotNumber`
      baked in.

### Verification
- `cargo fmt -p banlieue-provider-vsphere -- --check` ✅
- `cargo clippy -p banlieue-provider-vsphere --all-targets --all-features -- -D warnings` ✅
- `cargo test -p banlieue-provider-vsphere` ✅ (172 passed)
- Not yet verified live — pending image rebuild/redeploy and a fresh
  `govc vm.info -e` check against a newly force-reimported template.

## [2026-08-23 23:55] - Fix: template's own PCI-slot fix was clobbering itself

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs` (`import_iso_template`):
  removed a leftover post-create `ReconfigVM_Task` that pinned every NIC's
  *structured* `VirtualDevice.slotInfo` field (`build_nics_pci_slot_reconfigure_spec`,
  added during an earlier, since-abandoned attempt at this same fix). Editing
  the NIC device in that separate reconfigure re-triggers vCenter's own slot
  (re)assignment, which also re-syncs the `ethernetN.pciSlotNumber` ExtraConfig
  mirror to match — silently overwriting the *correct* ExtraConfig value the
  initial `CreateVM_Task` had just set moments earlier. Confirmed live via a
  new diagnostic log: a template built with `pciSlot: 192` had
  `ethernet0.pciSlotNumber` reading `"33"` in its own live config, and
  `clone_vm`'s existing (correct) carry-forward logic was faithfully
  propagating that already-wrong value to every clone.
- Removed the now-unused `build_nics_pci_slot_reconfigure_spec` helper and its
  unit test (`vim_tests.rs`); `find_all_nic_keys` is now used only to validate
  `CreateVM_Task` produced the expected NIC count.
- Added a diagnostic `info!` log in `clone_vm` at the ExtraConfig carry-forward
  step, logging the resolved `template_pci_slot_number` (this is what actually
  pinpointed the bug — the structured-slot log alone didn't distinguish "not
  written" from "written then clobbered").

### Why
Live report: a `VirtualMachine` clone still came up as `ens33` in the guest
despite `VMImage.spec.template.network[].pciSlot: 192` and networking
otherwise working correctly. The template-creation path was undoing its own
fix on every template build.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Requires rebuilding this provider's image AND force-recreating any
      existing templates (`banlieue.io/force-reimport` annotation) — templates
      built before this fix already have the wrong `ethernet0.pciSlotNumber`
      baked in; a plain re-clone from an existing bad template will not help.

### Verification
- `cargo fmt -p banlieue-provider-vsphere -- --check` ✅
- `cargo clippy -p banlieue-provider-vsphere --all-targets --all-features -- -D warnings` ✅
- `cargo test -p banlieue-provider-vsphere` ✅ (172 passed, down from 173 —
  the removed test for the deleted helper)

## [2026-08-23 07:35] - ADR-0033: CAPI IPAM pool integration (deferred, not implemented)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0033-capi-ipam-pool-integration.md`: Proposed, not implemented.
  Records the design conversation on using CAPI IPAM pools
  (`cluster-api-ipam-provider-in-cluster`'s range-based `InClusterIPPool`,
  or `ipam.metal3.io`'s name→address `preAllocations` table) to migrate
  already-issued VM addresses and hand out fresh ones per drone cluster.
  Decision: use existing static `networkOverrides` (ADR-0024) +
  `perZoneSubnet` (ADR-0032) for the active virtrigaud migration — both
  already fully working and exact by construction, no allocator needed;
  defers the actual `IPAddressClaim`/`IPAddress` wiring (schema and RBAC
  already exist, unused) to a follow-up, and explicitly leaves the choice
  of IPAM provider open pending coordination with the org's existing
  self-service IPAM system's owning team, to avoid two systems believing
  they own the same address range.

### Why
Asked directly whether CAPI IPAM pools could preserve already-known VM→IP
assignments during a migration. Investigation found range-based pools
allocate the next free address per claim with no way to request a
specific one — the wrong tool for preserving an existing mapping — and
that wiring any upstream IPAM provider in at all risks conflicting with
the org's own already-authoritative IPAM system. Captured as an ADR now,
per explicit request, so the reasoning and constraints aren't re-derived
from scratch whenever this is picked up later.

### Impact
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-23 07:10] - ADR-0032: per-zone subnet shape for static network classes

**Author:** Erick Bourgeois

### Added
- `docs/adr/0032-per-zone-network-subnet-shape.md`: Accepted and
  implemented.
- `crates/banlieue-api/src/banlieue/provider.rs`: `NetworkClassMapping`
  gains `subnet: Option<SubnetShape>` (default) and
  `per_zone_subnet: Vec<ScopedSubnet>` (per-`(datacenter, cluster)`
  overrides), plus `subnet_for(datacenter, cluster)` mirroring ADR-0030's
  `target_for` precedence exactly. New `SubnetShape` (gateway,
  nameservers, domain — deliberately no `prefix`, which stays per-VM) and
  `ScopedSubnet` types.
- `crates/banlieue-controller/src/reconciler/infra.rs`:
  `build_vsphere_machine`'s `_provider` parameter is now genuinely used —
  each NIC resolves its `networkClass`'s `subnet_for(datacenter, cluster)`
  and `merge_ipam_override` fills in whichever of
  `gateway`/`nameservers`/`domain` the per-VM `networkOverrides` entry left
  unset, field by field. An explicit per-VM value for any of those three
  still always wins for that field.
- `deploy/crds/banlieue.io_providers.yaml`, `docs/src/reference/api.md`:
  regenerated — additive only (`subnet`/`perZoneSubnet`, both optional).
- `docs/src/guides/environment-provider-isolation.md`: new "Static
  addressing across the same clusters (no DHCP)" section.
- Tests (TDD): `subnet_for` precedence tests in `banlieue-api`'s
  `provider_tests.rs` (mirroring the existing `target_for` tests exactly);
  `merge_ipam_override` unit tests plus one full `build_vsphere_machine`
  end-to-end test in `banlieue-controller`'s `infra_tests.rs`.

### Why
Asked directly, in an environment where DHCP is not usable at all (every
`VirtualMachine` must be statically addressed): how does one `VMClass`
work across multiple clusters with different subnets without the
`VirtualMachine` needing to know each cluster's gateway/DNS/domain?
Tracing `merge_ipam_override` found the field that looked like the answer
— `VMClass.network.interfaces[].ipam.static` (`IpamShape.static_` /
`StaticNetworkShape`) — is **never read** by either branch of that
function; a per-VM override (required for any static address) discards it
entirely, and even with no override it's still discarded. Fixing that
merge alone wouldn't have solved the actual problem anyway: a class-level
subnet shape is one fixed value for a class ADR-0030 already made
portable across many clusters with genuinely different subnets. The
correct fix mirrors ADR-0030 exactly — the subnet facts move to the same
per-zone place the port group already lives, since a port group implies a
subnet.

### Impact
- [x] Requires cluster rollout (updated `banlieue-controller` image;
      regenerated CRD is additive, no forced re-apply)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-23 06:45] - Document "failure domain" / "availability zone" as synonyms

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-api/src/banlieue/provider.rs`: doc comments on
  `ProviderStatus.failure_domains` and the `FailureDomain` struct now state
  explicitly that "failure domain" and "availability zone" are synonyms,
  and that `failureDomain` was kept as the field/type name specifically to
  align with CAPI v1beta2's own vocabulary (`clusterv1.FailureDomain`,
  `Machine.spec.failureDomain`), not because it means something distinct
  from an AZ.
- `docs/src/concepts/providers.md`: added the same naming note in prose.
- `docs/src/reference/api.md`: regenerated (`make api-docs`) to pick up the
  updated doc comment.
- `docs/site/`: rebuilt (`make docs`) so the naming note reaches the
  rendered site, not just the Markdown source.

### Why
Discussed renaming `failureDomain` to `availabilityZone` outright; kept the
existing name (it's the term CAPI itself uses, and this project's provider
infra CRDs are built to satisfy the CAPI v1beta2 InfraMachine contract) but
documented the two terms as interchangeable so docs/CLI help text can use
whichever reads more naturally without implying a real distinction.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-23 06:30] - Mirror a failure domain's resolved name into its own labels

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-provider-vsphere/src/reconciler/provider.rs`:
  `build_failure_domain` now inserts `labels["name"] = <the resolved name>`
  (auto-computed, or an ADR-0023 `failureDomainNameOverrides` entry) —
  alongside the existing `datacenter`/`cluster` labels.
- `crates/banlieue-provider-libvirt/src/reconciler/provider.rs`: same fix —
  the single failure domain a libvirt Provider publishes now also carries
  `labels["name"]`, overriding any user-supplied `name` label on the
  Provider itself, since this IS that failure domain's real name.
- `crates/banlieue-api/src/banlieue/provider.rs`: `FailureDomain.labels`
  doc comment updated to document the new `name` key; `deploy/crds/banlieue.io_providers.yaml`
  / `docs/src/reference/api.md` regenerated (doc-only diff, no schema
  shape change).
- `docs/src/guides/vsphere-provider.md`: the existing ADR-0023 callout
  gained a short note + example showing `failureDomainSelector:
  matchLabels: { name: cluster-01 }`.
- Tests (TDD): `discover_inventory_labels_each_fd_with_its_own_resolved_name`
  (vsphere, covers both the override and auto-computed cases) and
  `failure_domain_labels_itself_with_its_own_name` (libvirt).

### Why
Asked directly: a `VirtualMachine.spec.placement.failureDomainSelector`
using `matchLabels: { name: cluster-01 }` (the friendly
`failureDomainNameOverrides` name from ADR-0023) silently matched zero
failure domains — `FailureDomain.labels` only ever carried `datacenter`/
`cluster` (the raw, backend-reported names), never the resolved `name`
itself, which is a top-level field a `LabelSelector` cannot reach directly.
The friendly-name feature existed but had no way to actually be selected
on; this closes that gap.

### Impact
- [x] Requires cluster rollout (updated vsphere/libvirt provider images;
      regenerated CRD is additive/doc-only, no live-cluster action needed
      unless re-applying `deploy/crds/banlieue.io_providers.yaml`)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-23 06:05] - Add "End-to-End Setup" guide (bootstrap to running VMs)

**Author:** Erick Bourgeois

### Added
- `docs/src/guides/end-to-end-setup.md`: new guide — a single phased Mermaid
  flowchart (bootstrap script → install banlieue → register a backend →
  build/register an image → define a VMClass → provision a VirtualMachine
  → optional CAPI-driven k0s workload cluster), plus a walkthrough section
  per phase linking out to the existing detailed guide for that step, a
  short note on why phase 0 (the management-cluster bootstrap) is
  deliberately the one piece that doesn't go through banlieue's own CRDs
  (no cluster yet exists to apply them against), and a phase → guide
  lookup table.
- `docs/mkdocs.yml`: nav entry, placed first under **Guides** as the entry
  point.
- `docs/src/guides/index.md`: card added to the top of the guides grid.

### Why
Asked directly for "a nice mermaid ... diagram of this whole setup, from
bootstrapping ... to a providerclass, provider, etc." No existing page
tied the full chain together — `overview.md`'s diagram covers only the
steady-state VM-provisioning path, and every other guide is scoped to one
phase. This synthesizes ADR-0001/0002 (CAPI delegation), ADR-0003/0012
(provider topology), ADR-0010/0020 (image pipeline), ADR-0013 (bootstrap
CLI), and ADR-0017/0018 (management-cluster bootstrap) into one navigable
picture without duplicating their detail.

### Impact
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-23 05:40] - Fix ADR-0029 numbering collision; rebuild stale docs/site

**Author:** Erick Bourgeois

### Fixed
- `docs/adr/0029-vmimage-template-multiple-nics.md` → renumbered to
  `docs/adr/0031-vmimage-template-multiple-nics.md`. A concurrent session
  independently landed its own, unrelated `ADR-0029` (`guestinfo.metadata`
  hostname/FQDN default) at the same time; `0030` was also already taken
  (per-zone capability targets), so mine became `0031`. Swept every file
  the multi-NIC change touched — `vmimage.rs`/`vmimage_tests.rs` (both
  `banlieue-api` and `banlieue-provider-vsphere`), `nic_flag.rs`/
  `nic_flag_tests.rs`, `import.rs`/`import_tests.rs`, `client/mod.rs`,
  `client/vim.rs`/`vim_tests.rs`, `deploy/crds/banlieue.io_vmimages.yaml`,
  `docs/src/reference/api.md`, `examples/07-vmimage-kairos-url-source.yaml`,
  `.wolf/anatomy.md`, and this CHANGELOG's own prior entry — fixing every
  `ADR-0029` reference that was actually about multi-NIC to `ADR-0031`,
  while leaving the other session's legitimate `ADR-0029`/`ADR-0030`
  mentions (`reconciler/vspheremachine.rs`,
  `reconciler/vspheremachine_tests.rs`, the per-zone-targets CALM
  description) untouched.
- `docs/site/` (the built MkDocs static site) rebuilt via `make docs` —
  it was stale after the `VMImageTemplate.network` schema change:
  `make crds` regenerates the *source* Markdown
  (`docs/src/reference/api.md`) but not the built HTML, so
  `docs/site/reference/api/index.html` still showed the old singular
  `networkAdapter`/`nicPciSlot` fields until this rebuild.

### Why
Asked directly: "have the docs been sync'ed with all of this
functionality?" Checking surfaced both issues above — a real ADR-numbering
collision with a concurrent session's own work, and a real stale-build gap
in the rendered docs site that `make crds`/`make calm-diagrams` alone don't
cover.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

### Verification
- `cargo build -p banlieue-provider-vsphere -p banlieue-api --tests` ✅
  (confirms the sed-based comment-only rename introduced no breakage).
- `make calm-validate` ✅.
- `docs/site/reference/api/index.html`: 0 occurrences of
  `networkAdapter`/`nicPciSlot` (down from 2), 2 occurrences of the new
  `VMImageTemplateNic`/`pciSlot` shape (up from 0) — confirms the rebuild
  actually picked up the schema change.
- Infra-name sweep across all tracked + untracked files ✅ clean.

## [2026-08-23 05:20] - Add "Environment / Provider Isolation" guide

**Author:** Erick Bourgeois

### Added
- `docs/src/guides/environment-provider-isolation.md`: new guide answering
  "does each environment (dev/qa/prod) need its own `Provider` CR?" — the
  rule (`Provider` = one backend connection, not a tenancy boundary,
  ADR-0003), the motivating case (same vCenter, same storage, a different
  network per environment on the *same* clusters), why ADR-0030's `perZone`
  alone can't express it (it's keyed by `(datacenter, cluster)`, and dev/qa
  VMs on the same cluster share that key — the fix is a second class name,
  not a second `Provider`), and when a separate `Provider` genuinely is the
  right call (different endpoint, or a deliberate credential/RBAC
  boundary). All example identifiers are placeholders, not the real
  on-prem values discussed while diagnosing the underlying issue.
- `docs/mkdocs.yml`: nav entry under **Guides**.

### Why
Follow-up to ADR-0030: once per-zone targets exist, the natural next
question is whether dev/qa/prod each need their own `Provider`. They don't,
usually — but the reasoning (connection vs. tenancy boundary; why the same
class name can't hold two different per-cluster targets when the
environment, not the cluster, is what's actually varying) is exactly the
kind of thing worth writing down once instead of re-deriving per
conversation.

### Impact
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-23 04:45] - ADR-0030: per-zone concrete targets for capability class mappings

**Author:** Erick Bourgeois

### Added
- `docs/adr/0030-per-zone-capability-targets.md`: Accepted and implemented.
- `crates/banlieue-api/src/banlieue/provider.rs`: new `ScopedTarget`
  (`datacenter`, `cluster`, `target`), keyed the same way ADR-0023's
  `failureDomainNameOverrides` is. `StorageClassMapping`/`NetworkClassMapping`
  gain `target: Option<BTreeMap<String, String>>` (was mandatory) as the
  Provider-wide default and `per_zone: Vec<ScopedTarget>` for overrides,
  plus a shared `target_for(datacenter, cluster)` method resolving the
  precedence (exact `per_zone` match, else default `target`, else `None`)
  in one place.
- `crates/banlieue-provider-vsphere/src/reconciler/provider.rs`:
  `compute_failure_domain_attributes` now resolves each class's target via
  `target_for(dc_name, cluster_name)` before checking reachability, instead
  of checking the Provider-wide default against every zone.
- `crates/banlieue-provider-vsphere/src/import.rs`: `resolve_storage_target`
  / `resolve_network_target` take the zone's `(datacenter, cluster)` (already
  resolved from the failure domain) and call `target_for`.
- `crates/banlieue-controller/src/reconciler/scheduler.rs`:
  `first_target_value` (used by `build_decision` to populate
  `VirtualMachine.status.scheduled.resolvedStorage[]` /
  `resolvedNetworks[].backendId`) takes the chosen failure domain's
  `(datacenter, cluster)` from `attributes.raw` and calls `target_for`.
- `crates/banlieue-provider-libvirt/`: `.target` accesses updated for the
  new `Option` type — libvirt's failure domains have no
  `(datacenter, cluster)` concept yet, so these always resolve the
  Provider-wide default (ADR-0030's explicit out-of-scope note).
- `deploy/crds/banlieue.io_providers.yaml`, `docs/src/reference/api.md`:
  regenerated (`make crds`).
- `docs/architecture/calm/architecture.json`: `rel-provider-vsphere-backend`
  extended to describe per-zone target resolution; `make calm-validate`
  passes.
- Tests (TDD): new `target_for` precedence tests in
  `banlieue-api/src/banlieue/provider_tests.rs`; new per-zone-override
  regression tests in `banlieue-provider-vsphere/src/import_tests.rs`
  (`resolve_zone`/`resolve_nic_networks`); existing construction sites
  across `banlieue-provider-vsphere`, `banlieue-provider-libvirt`, and
  `banlieue-controller` test files updated for the new field shape.

### Why
Found live: a `VMClass` (`hadron-small`) requesting `networkClass:
network-01` was only ever available on `cluster-01` of a three-cluster
vCenter Provider (`vcenter-ssc`) — not because `cluster-02`/`03` lack an
equivalent network, but because `Provider.spec.capabilities.networkClasses[]`
mapped `network-01` to `cluster-01`'s own specifically-named port group,
Provider-wide, with no way to declare a different concrete target per
cluster. The fix makes `VMClass`/`VMImage` genuinely portable across every
cluster of a Provider, and — since a `VMClass` has no binding to any
specific `Provider` — across multiple vCenters too, with zero changes to
`VMClass`, the scheduler's matching logic, or `VirtualMachine`.

### Impact
- [x] Requires cluster rollout (regenerated CRD + updated
      controller/provider-vsphere/provider-libvirt images)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-23 04:10] - VMImage templates support multiple NICs (ADR-0031)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0031-vmimage-template-multiple-nics.md` — new ADR.
- `crates/banlieue-api/src/banlieue/vmimage.rs`: `VMImageTemplateNic {
  network, adapter, pciSlot }`; `VMImageTemplate.network` changed from
  `Option<String>` (+ separate `networkAdapter`/`nicPciSlot` fields) to
  `Vec<VMImageTemplateNic>`. Empty list = today's exact single-NIC default
  (zone-derived network, vmxnet3, slot 192). An unset `pciSlot` on any
  entry defaults to `192 + that entry's index`, so a second/third NIC
  still gets predictable `ens193`/`ens194` naming without hand-picking
  every slot.
- `crates/banlieue-provider-vsphere/src/nic_flag.rs` (new module):
  `serialize_nic_flag`/`parse_nic_flag` — `--nic
  network=<name>,adapter=<type>,pciSlot=<n>` encoding, mirroring
  `banlieue_provider_sdk::scheduling`'s existing delimited-string CLI
  pattern for `--toleration`/`--node-selector`.
- `crates/banlieue-provider-vsphere/src/import.rs`: `ImportArgs.nics:
  Vec<String>` (repeatable `--nic`, replacing `--network`/
  `--network-adapter`/`--nic-pci-slot`); new `ResolvedNic` +
  `resolve_nic_networks` (per-NIC zone-network resolution, the same
  override-else-first-reachable-class rule `resolve_zone` used for its one
  NIC, applied per entry); `ZonePlan` no longer carries `network`.
- `crates/banlieue-provider-vsphere/src/client/mod.rs`: new `RequestedNic`
  (network + moref + distributed + adapter + pci_slot, fully resolved);
  `IsoImportRequest.nics: Vec<RequestedNic>` replaces the five singular
  network/adapter/slot fields.
- `crates/banlieue-provider-vsphere/src/client/vim.rs`:
  `build_template_config_spec` loops over N NICs (N ethernet devices, N
  `ethernetN.pciSlotNumber` extraConfig entries); new
  `find_all_nic_keys` (multi-NIC counterpart of `find_first_nic_key`);
  `build_nics_pci_slot_reconfigure_spec` (renamed from
  `build_nic_pci_slot_reconfigure_spec`) batches one post-create
  `ReconfigVM_Task` device_change per NIC instead of one Task per NIC.
  Removed now-dead `find_nic_key` (single adapter-type-specific lookup,
  superseded by `find_all_nic_keys`).
- `examples/07-vmimage-kairos-url-source.yaml`,
  `deploy/crds/banlieue.io_vmimages.yaml`, `docs/src/reference/api.md`:
  updated for the new list-shaped field.

### Why
Asked directly: "we need to support multiple ethernet slots, right now
there is no way to set more than [one NIC] per template." `VMImageTemplate`
had only ever modeled a single NIC (three independent singular fields);
`build_template_config_spec` built exactly one `VirtualEthernetCard`. This
project has no release and no consumers yet (explicitly confirmed: "i am ok
with breaking changes until we make an official release"), so the shape
changed outright rather than adding a parallel field alongside the old one.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet) — despite being
  a genuine breaking schema/CLI change, this box stays unchecked per this
  project's own standing convention: banlieue has no release or consumers,
  so "breaking change" framing doesn't apply yet.
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- New tests: `nic_flag_tests.rs` (12 tests, parse/serialize/round-trip),
  `import_tests.rs`'s `resolve_nic_networks_*` (4 tests, including the
  auto-increment-by-index case), `vim_tests.rs`'s
  `build_template_config_spec_sets_ethernet0_pci_slot_number_as_extra_config`
  (updated) and `build_nics_pci_slot_reconfigure_spec_edits_each_nic_with_its_own_pinned_slot`
  (2 NICs, distinct adapters and slots), `vmimage_tests.rs`'s
  `build_import_job_emits_one_nic_flag_per_declared_nic` (2 NICs, one
  fully-specified, one partial).
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures, 169/169 in `banlieue-provider-vsphere` alone).
- Infra-name sweep across all tracked + untracked files ✅ clean.
- CRDs regenerated (`make crds`); example YAML re-validated as parseable.

### Not addressed here (see ADR-0031's Follow-ups)
`clone_vm` remains single-NIC — this ADR scopes multi-NIC support to the
*template build* path only. `VSphereMachineSpec.network` is already
`Vec<VSphereNicSpec>` in the schema, so cloning a multi-NIC template today
would silently only wire up its first interface.

## [2026-08-23 03:15] - Clarify why vSphere gets no per-instance Jobs Role (bootstrap.rs)

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-operator/src/bootstrap.rs`: `backend_additional_rules`'s
  doc comment corrected — it previously claimed "vSphere has no such need
  and must not receive it" (Jobs access), which is false; vSphere's
  per-zone import Jobs (ADR-0020) do need Jobs access, they just get it
  entirely from the cluster-wide `ClusterRole`
  (`deploy/provider-vsphere/rbac/clusterrole.yaml`, already fixed in the
  previous entry) rather than this per-instance, namespace-scoped `Role`.
  A rule here could never reach vSphere's Jobs anyway, since they live in
  `banlieue-imagebuild` (ADR-0016), a different namespace than the one
  this `Role` is scoped to.
- `crates/banlieue-operator/src/bootstrap_tests.rs`:
  `only_libvirt_gets_the_job_grant`'s comment updated to match — empty
  for vSphere does not mean "no Jobs access," it means "not from here."

### Why
Investigated as a possible second instance of the RBAC drift found
earlier this session (the `deploy/provider-vsphere/rbac/clusterrole.yaml`
missing `list`/`watch` on `jobs`). Turned out NOT to be a permission gap —
`bootstrap.rs`'s ClusterRoles are `include_str!`-embedded directly from
`deploy/*/rbac/clusterrole.yaml` (confirmed at `bootstrap.rs:117`), so the
earlier fix to that file already covers `banlieue bootstrap
operator`/`provider vsphere` too. The actual problem was just a stale,
misleading doc comment that could send a future reader chasing a
non-existent gap, or worse, "fixing" it by adding a redundant/useless Jobs
rule to the wrong (namespaced) Role.

### Impact
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

## [2026-08-23 02:55] - Grant list/watch on Jobs to the vsphere provider (and operator) for the new event-driven VMImage watch

**Author:** Erick Bourgeois

### Changed
- `deploy/provider-vsphere/rbac/clusterrole.yaml`: `jobs` verbs
  `["get", "create", "patch", "delete"]` → `["get", "list", "watch",
  "create", "patch", "delete"]`.
- `deploy/operator/rbac/clusterrole.yaml`: same addition on its own `jobs`
  rule — a delegating ClusterRole cannot grant a permission it does not
  itself hold (ADR-0012), same trap as the earlier `vmimages` fix.

### Why
Found live: applying a `VirtualMachine` started throwing a new RBAC 403 —
`list` on `jobs` in `banlieue-imagebuild` — for
`system:serviceaccount:banlieue-system:banlieue-provider-vsphere-vcenter-ssc`.
Traced to the "event-driven VMImage reconciliation" change earlier this
session (`crates/banlieue-provider-vsphere/src/app.rs`): the `VMImage`
`Controller` now `.watches(import_job_api, ..., vmimage_ref_from_job)` the
per-zone import Job directly instead of polling on a timer. That change
updated the operator's `vmimages` grant (a different, unrelated resource)
but never touched either ClusterRole's `jobs` rule — kube-runtime's watcher
needs `list`+`watch` to build and maintain its informer cache for any
watched type, same as every other resource a controller watches, and
neither static YAML had them.

Also noted, not fixed here: `crates/banlieue-operator/src/bootstrap.rs`'s
`backend_additional_rules()` (the *other* RBAC source of truth, used by
`banlieue bootstrap operator`) grants Jobs access to the libvirt backend
only and explicitly comments that vSphere "has no such need" — which has
been false since ADR-0020 added vSphere's own per-zone import Job. The two
RBAC-generation paths (static `deploy/*/rbac/*.yaml` vs. `bootstrap.rs`)
have drifted apart for vSphere; worth a follow-up.

### Impact
- [x] Requires cluster rollout (reapply both ClusterRoles)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-23 02:40] - Fix ens192 pinning for real: ethernet0.pciSlotNumber is ExtraConfig, not slotInfo

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `build_template_config_spec`
  now sets `ethernet0.pciSlotNumber` as an `extraConfig` entry
  (`OptionValue`/`VimAny::Value(PrimitiveString(...))`) on the template's
  `CreateVM_Task`, reusing the exact same shape `clone_vm` already uses for
  `guestinfo.*` keys. `clone_vm` now also carries that same key forward
  explicitly onto the clone's own `extraConfig`, read off the template's
  `VirtualMachineConfigInfo.extra_config` (not re-derived from the
  structured `slotInfo` read, which can legitimately disagree with it).

### Why
Found live: the previous fix (a separate post-create `ReconfigVM_Task`
setting the structured `VirtualDevice.slotInfo` field) still produced the
identical auto-assigned slot both before and after, on two independent
force-recreated templates. Checking `create-kairos-template.sh` — the
actual, working reference this whole convention is modeled on — its `govc
vm.change -vm "${VM_PATH}" -e "ethernet0.pciSlotNumber=192"` call was
misread as manipulating the structured device object; `govc vm.change -h`'s
own docs confirm `-e` sets **ExtraConfig** (the same flag used for
`guestinfo.vmname` in govc's own example), a VMX-file-level key/value pair
— a completely different data path from `VirtualDevice.slotInfo`. Every
previous fix in this area (this session's structured-`slotInfo` read/reapply
on clone, then the post-create structured reconfigure) was correctly
implemented against the *wrong* mechanism; `ethernet0.pciSlotNumber` as
ExtraConfig is the field actually governing guest-visible PCI placement.

The structured `slotInfo` reconfigure from the previous fix is left in
place (harmless) rather than removed, in case it has some independent
effect on the device model — but it is no longer relied upon for the
guest-visible interface name.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`,
  then force-recreate any template built before this fix
- [ ] Config change only
- [ ] Documentation only

### Verification
- New test: `build_template_config_spec_sets_ethernet0_pci_slot_number_as_extra_config`.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures, 37/37 in `client::vim`).
- Infra-name sweep across all tracked + untracked files ✅ clean — also
  swept the real hostnames/datastore names read from live pod logs while
  diagnosing this, confirmed none written to any tracked file.

### Not yet confirmed live
Same as the prior (superseded) fix attempt: needs a force-recreated
template + fresh clone to confirm the guest actually comes up as `ens192`
this time. If it still doesn't, the remaining fallback hypothesis is that
Hadron's stripped-down image doesn't run `systemd-udev`'s `net_id`
predictable-naming at all, independent of anything vSphere reports.

## [2026-08-23 02:05] - Fix ens192 pinning: PCI slot must be set post-create, not in CreateVM_Task

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `build_template_config_spec`
  no longer sets `slot_info` on the NIC device it hands to `CreateVM_Task`.
  New `build_nic_pci_slot_reconfigure_spec` pins the NIC's PCI slot in a
  *separate*, post-create `ReconfigVM_Task` instead — run unconditionally
  right after `CreateVM_Task`, before the (optional) install-then-generalize
  sequence. Removed `find_nic_key` (adapter-type-specific device lookup),
  now dead code — the post-create step reuses `find_first_nic_key` instead
  (already resolves key + concrete adapter type together).

### Why
Found live, even after setting `nicPciSlot: 192` explicitly and force-
recreating the template: a diagnostic log added earlier this session
(`"template NIC resolved for clone"`) showed the *template's own* NIC
carried `pci_slot=Some(33)`, not 192 — proving the clone-side read/reapply
logic (this session's earlier fix) was working correctly; the bug was
upstream, at template-build time. `build_template_config_spec` requested
slot 192 for the NIC in the same `CreateVM_Task` that also creates the SCSI
controller, disk, IDE controller, and CD-ROM with no explicit slots of
their own (auto-assigned). That is exactly the unsupported mix `vim_rs`'s
own doc comment on `pci_slot_number` warns about: manual slot numbers
"should ... only [be specified] in a CreateVM operation if they are
specified for all devices" — vCenter silently reassigned the NIC instead of
honoring the request or erroring.

The working reference this logic is modeled on
(`~/dev/vm-build/bin/create-kairos-template.sh`) never actually does it the
way banlieue's code did: it runs `govc vm.create` first (everything
auto-assigned), then a *separate* `govc vm.change -e
"ethernet0.pciSlotNumber=192"` once the VM already exists and every sibling
device already has a concrete, locked-in slot. This is the same shape of
vCenter quirk `build_boot_order_reconfigure_spec` already worked around for
boot order (ADR-0021) — an attribute embedded in the initial `CreateVM_Task`
isn't reliably honored; it has to be set in a reconfigure once the devices
have real keys.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`,
  then force-recreate any template built before this fix (its NIC will
  still carry whatever slot it got auto-assigned to previously)
- [ ] Config change only
- [ ] Documentation only

### Verification
- New tests: `build_nic_pci_slot_reconfigure_spec_edits_the_given_nic_with_the_pinned_slot`,
  `build_template_config_spec_leaves_the_nic_slot_unset` (proves the
  `CreateVM_Task` spec itself carries no `slot_info`, the actual root cause).
- Deleted `find_nic_key_matches_the_requested_adapter_type_only` along with
  the now-dead `find_nic_key` it tested.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures, 36/36 in `client::vim`) — one transient failure mid-run,
  caused by a concurrent session's own in-progress edit landing on
  `vspheremachine_tests.rs` between compile and test execution, not a real
  regression; re-run clean once settled.
- Infra-name sweep across all tracked + untracked files ✅ clean.

### Not yet confirmed live
This fix has not yet been verified against a real vCenter — the next
force-recreated template + fresh clone needs to actually come up as
`ens192` in the guest to close this out. If it still doesn't, the next
hypothesis is that Hadron's stripped-down, no-package-manager image simply
doesn't run `systemd-udev`'s `net_id` predictable-naming the way a normal
distro does, independent of what PCI slot vSphere reports.

## [2026-08-23 01:40] - ADR-0029: default hostname/FQDN via guestinfo.metadata

**Author:** Erick Bourgeois

### Added
- `docs/adr/0029-guestinfo-metadata-hostname-fqdn-default.md`: Accepted and
  implemented.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  `build_guestinfo` now also sets `guestinfo.metadata` unconditionally on
  every clone — a base64 YAML document (`instance-id`, `local-hostname`)
  matching real cloud-init's own VMware GuestInfo datasource schema.
  `local-hostname` is the FQDN (`<vm-name>.<domain>`) when a static network
  override resolves a domain, else the plain VM name — no new domain is
  invented for a plain-DHCP VM. Independent of `spec.userData` entirely: no
  new CRD field, and the user's own cloud-config is never parsed or merged
  into.
- `docs/architecture/calm/architecture.json`: `rel-provider-vsphere-backend`
  description extended to cover `guestinfo.metadata`; `make calm-validate`
  passes.
- Tests (TDD, `vspheremachine_tests.rs`): updated the existing
  `dhcp_only_and_no_userdata_produces_only_hostname` expectation (now 2
  unconditional keys, not 1) plus four new tests covering short-hostname
  vs. FQDN `local-hostname`, independence from `userData`, and that
  `userData` is never touched.

### Why
Real cloud-init guests (`VirtualMachine.spec.guestAgent: cloud-init`) never
got a hostname set unless the user's own `userData` cloud-config happened
to include a `hostname:`/`fqdn:` directive — `guestinfo.network.hostname`
(banlieue's own flat convention, read only by the hand-rolled
`configure-network.sh` script documented in the Kairos Hadron guide) isn't
part of cloud-init's actual datasource contract. `guestinfo.metadata` is
the real contract; setting it makes hostname/FQDN a sane, zero-config
default for every VM without ever risking corrupting hand-authored
`userData`.

### Impact
- [x] Requires cluster rollout (new `banlieue-provider-vsphere` binary)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-23 01:05] - Event-driven VMImage reconciliation off the import Job, not a 5-minute poll

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`: extracted
  `LABEL_VMIMAGE` (`banlieue.io/vmimage`) as a `pub const` — was a bare
  string literal only `build_import_job` used; now shared with the new
  watch mapper below.
- `crates/banlieue-provider-vsphere/src/app.rs`: new pure
  `vmimage_ref_from_job(Job) -> Option<ObjectRef<VMImage>>`, reading
  `LABEL_VMIMAGE` off a Job; wired via `.watches(import_job_api,
  Config::default(), vmimage_ref_from_job)` on the `VMImage` `Controller`.
  A per-zone import Job's status change (created, completed, failed,
  deleted-and-recreated by a forced reimport) now re-triggers that
  `VMImage`'s reconciliation immediately.
- `deploy/operator/rbac/clusterrole.yaml`: added `update`/`patch` to the
  operator's own `vmimages` grant, matching the same addition the previous
  entry made to the provider's ClusterRole — a delegating ClusterRole
  cannot grant a permission it does not itself hold. Caught by
  `operator_cluster_role_covers_every_permission_it_grants_to_providers`,
  an existing test specifically written to catch exactly this class of
  drift — not a live failure.

### Why
Asked directly: "the image-import should watch the pod of the build, can
this be done instead of polling every 5 mins?" Confirmed by reading
`app.rs`: the `VMImage` `Controller` had no `.watches()`/`.owns()` on the
Job at all — `Controller::new(image_api, Config::default())` only watches
`VMImage` itself. Progress advanced solely via `requeue_default()` (30s)
while an import was in flight, falling back to `requeue_long()` (300s) once
it went quiet — exactly the "had a 5 min reconciliation timer" delay hit
earlier this session after deleting an artifact/Jobs to force a rebuild.
The import Job already carried a `banlieue.io/vmimage` label
(`build_import_job`) — the label existed for operational visibility
(`kubectl get jobs -l banlieue.io/vmimage=...`), not yet wired to anything
that acts on it.

The Job's actual Kubernetes `ownerReference` points at the `OSArtifact` it
mounts (a separate, unrelated concurrent-session change, ADR-0027) — that
serves a different purpose (rebuild-triggered garbage collection) and is
deliberately left untouched. Watching by label rather than by
`.owns()`/ownership avoids entangling the two mechanisms.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
  **and re-apply the operator's updated ClusterRole**
- [ ] Config change only
- [ ] Documentation only

### Verification
- New `app_tests.rs` tests (TDD): `vmimage_ref_from_job_maps_to_the_labeled_image`,
  `vmimage_ref_from_job_is_empty_when_the_label_is_missing`.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures) — caught and fixed one real regression mid-verification: the
  new `vmimages` `update`/`patch` grant on the provider's ClusterRole (added
  in the entry above) initially had no matching grant on the operator's own
  ClusterRole, which an existing RBAC-delegation test correctly failed on
  before this shipped.
- Infra-name sweep across all tracked + untracked files ✅ clean.

## [2026-08-23 00:20] - VMImage deletion lifecycle: destroy per-zone templates by default (ADR-0028)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0028-vmimage-template-deletion-lifecycle.md` — new ADR, same
  shape as ADR-0026 (`VSphereMachine` deletion lifecycle), for `VMImage`.
  (Originally drafted as ADR-0027; renumbered to 0028 after a concurrent
  session claimed 0027 for an unrelated change first.)
- `crates/banlieue-api/src/banlieue/vmimage.rs`: `VMImageTemplate.retainOnDelete`
  (default `false`) — opt out of the new default-delete behavior.
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`: new
  `banlieue.io/vmimage` finalizer; `reconcile` now checks
  `deletion_timestamp` first, and (unless `retainOnDelete`) destroys every
  per-zone template each `status.perProvider[]` row reports, reusing
  `VSphereClient::destroy_vm` (ADR-0026) — a template is a VM with the
  template bit set, so the teardown mechanics are identical.
- `deploy/provider-vsphere/rbac/clusterrole.yaml`: added `update`/`patch`
  on the main `vmimages` resource (previously `get`/`list`/`watch` only) —
  required for the finalizer merge-patch, which targets the main resource,
  not the `/status` subresource. Caught before deploy, not live: without
  this the finalizer add/remove would 403.
- `deploy/crds/banlieue.io_vmimages.yaml`, `docs/src/reference/api.md`:
  regenerated for the new field.
- `docs/architecture/calm/architecture.json`: vSphere provider node's
  description now covers both deletion finalizers.

### Why
Found live: deleting a `VMImage` CR removed it from Kubernetes immediately,
but every per-zone vCenter template it caused to be built stayed behind,
orphaned — the vsphere provider's `vmimage` reconciler had no
`deletion_timestamp` check and no finalizer at all, the same class of gap
ADR-0026 already found and fixed for `VSphereMachine`/cloned VMs, just for
the template-build path instead of the clone path. The user wants the
opposite of today's (accidental) behavior: delete by default, retain only
when explicitly asked.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
  **and re-apply the updated ClusterRole** (RBAC change, not just image)
- [ ] Config change only
- [ ] Documentation only

### Verification
- New `vmimage_finalize_tests.rs` (TDD, written before `destroy_zone_templates`):
  `destroys_the_template_when_found`, `is_a_noop_when_template_already_absent`,
  `skips_a_zone_with_no_resolved_ref`, `skips_a_zone_whose_failure_domain_is_gone`,
  `destroys_multiple_zones_in_their_own_folders_without_cross_zone_collision`
  (proves the ADR-0020 Decision #5 folder-scoping fix still holds for deletes).
- New `vmimage_tests.rs` (banlieue-api) tests for `retainOnDelete`'s
  default/omission/round-trip.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures, 31/31 in `reconciler::vmimage`, 52/52 in `banlieue::vmimage`)
  / `make calm-validate` ✅.
- Infra-name sweep across all tracked + untracked files ✅ clean.

### Not addressed here
Already-orphaned templates from before this fix ships (including any from
this session's own live testing) are not retroactively found or cleaned up
— their owning `VMImage`/`VSphereMachine` CRs are already gone, so there's
nothing left in the cluster pointing at them. Same caveat ADR-0026 already
documents for already-orphaned cloned VMs; needs manual identification and
cleanup directly in vCenter.

## [2026-08-22 23:40] - Fix unreadably small Mermaid diagram on the Architecture concepts page

**Author:** Erick Bourgeois

### Changed
- `docs/src/stylesheets/extra.css`: `.mermaid svg` no longer capped at
  `max-width: 100%` — mkdocs-material was scaling every diagram down to
  the content column width, and a wide `flowchart TB` with 6 subgraphs
  shrank until its text was unreadable. Now renders at native size with
  `pre.mermaid`'s existing `overflow-x: auto` providing a horizontal
  scrollbar, plus a `min-height: 420px` floor and the existing zoom/pan JS
  (`javascripts/mermaid-init.js`) for whatever still doesn't fit. Affects
  every Mermaid diagram on the site, including the CALM-generated ones.
- `docs/src/concepts/architecture.md`: split the single, overloaded
  "Components" diagram (structure + every watch/create/patch edge, ~24
  edges) into a structure-only overview plus three small `flowchart LR`
  diagrams placed directly in the existing "Provision a VM" / "Build and
  import an image" / "Register a backend" subsections — each now shows
  only the edges relevant to that one scenario, numbered to match its
  existing prose steps.

### Why
User reported the Mermaid diagram at
`/concepts/architecture/#components` as "way too small" to read on the
published site. Root cause was two compounding problems: a CSS cap
shrinking wide diagrams indiscriminately, and one diagram trying to carry
both the static object/controller graph and every fine-grained interaction
edge at once.

### Impact
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-22 23:10] - ADR-0027: own per-zone import Jobs by their OSArtifact

**Author:** Erick Bourgeois

### Added
- `docs/adr/0027-import-job-owned-by-osartifact.md`: Accepted and
  implemented. Each provider's per-zone import Job now carries an
  `ownerReference` to the `OSArtifact` it was created for, so Kubernetes
  garbage collection reaps a stale Job the moment its `OSArtifact` is
  deleted, instead of leaving it (and the artifacts PVC mount it holds)
  around for up to the existing 24h `ttlSecondsAfterFinished`.
- `crates/banlieue-api/src/banlieue/vmimage.rs`:
  `BuildArtifactStatus.os_artifact_uid: Option<String>` — the live
  `OSArtifact`'s `metadata.uid`, once observed.
- `crates/banlieue-provider-sdk/src/osartifact.rs` (new module):
  `owner_references(name, uid) -> Option<Value>` — shared by both
  providers (imagebuilder needs the full `OSArtifact` `ApiResource`
  for its own reasons and keeps its own separate constants; providers
  only need the GVK for an owner reference).
- `crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`:
  `compute_build_artifact_status` now takes and publishes
  `os_artifact_uid`, populated from the live `OSArtifact` object read at
  the top of `reconcile`.
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs` and
  `crates/banlieue-provider-libvirt/src/reconciler/vmimage.rs`:
  `build_import_job` sets `metadata.ownerReferences` from the new helper.
- `deploy/crds/banlieue.io_vmimages.yaml`, `docs/src/reference/api.md`:
  regenerated (`make crds`) for the new status field.
- `docs/architecture/calm/architecture.json`: `import-job-owned-by-osartifact`
  control added to both `flow-build-vmimage-from-oci` and
  `flow-import-vmimage-libvirt`.
- Tests: `banlieue-provider-sdk/src/osartifact_tests.rs` (new),
  `banlieue-api/src/banlieue/vmimage_tests.rs`,
  `banlieue-imagebuilder/src/reconciler/vmimage_tests.rs`,
  `banlieue-provider-vsphere/src/reconciler/vmimage_tests.rs`,
  `banlieue-provider-libvirt/src/reconciler/vmimage_tests.rs` — written
  first (TDD), covering the uid-known/uid-unknown owner-reference cases.

### Why
Found live: retriggering a `VMImage` build whose `OSArtifact` is judged
stale (source URL/checksum changed) deletes the old `OSArtifact`, whose PVC
kairos-operator then tries to delete too — but the provider's prior import
Job for that PVC had no owner reference, so it (and its Pod's mount)
lingered until its TTL fired. The PVC sat `Terminating` the entire time,
blocking the rebuild indefinitely with no error surfaced in
`VMImage.status`.

### Impact
- [x] Requires cluster rollout (regenerated CRD applied — new optional
      status field only, additive)
- [ ] Config change only
- [ ] Documentation only

## [2026-08-22 22:15] - Add guestinfo.network.hostname; log resolved NIC PCI slot at clone time

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  `build_guestinfo` now takes `vm_name` and unconditionally sets
  `guestinfo.network.hostname` (regardless of DHCP or static network) —
  sourced from the `VirtualMachine`'s own name, the same source as
  `userData`'s `${VM_NAME}` placeholder.
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `clone_vm` now logs
  the resolved NIC key/adapter type/PCI slot read off the template before
  cloning — diagnostic only, added because a live retest of the ADR-0026
  PCI-slot fix (deployed, on a freshly-recloned VM) still came up `ens33`,
  and there's no way to tell from the outside whether the read of the
  template's `slot_info` failed or something downstream overrode it
  without this visibility.
- `docs/architecture/calm/architecture.json`: updated the vSphere provider
  node's description to mention the unconditional hostname key.

### Why
Found live: `/opt/example-org/configure-network` (baked into the `hadron-kairos`
image, not part of banlieue) reads its hostname from
`guestinfo.network.hostname` directly — banlieue never set that key, only
`ip`/`prefix`/`gateway`/`dns`/`domain`. The user explicitly does not want a
per-host `userData` cloud-config for the k0s drone/worker fleet just to set
a hostname, so this needed to be a guestinfo key, not a cloud-config field.

Separately: this session's earlier `ens192`→`ens33` PCI-slot-pinning fix
(ADR-0026 CHANGELOG entry above, "Fix cloned VMs losing the template's
pinned NIC PCI slot") was confirmed deployed and exercised against a fresh
clone, and the guest NIC still came up as `ens33`. An investigation agent
confirmed the native `VMImage` → `import_iso_template` →
`build_template_config_spec` → `clone_vm` pipeline is correctly wired
end-to-end and reaches the fixed code — so the remaining unknown is
whether `vim_rs`'s `slot_info` deserialization/downcast round-trips
against a *real* vCenter response the way it does against this session's
synthetic unit-test fixtures. The new log line answers that directly on
the next live attempt instead of guessing further.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- `vspheremachine_tests.rs`: new `hostname_is_set_for_both_dhcp_and_static_interfaces`;
  `dhcp_only_and_no_userdata_produces_nothing` renamed/updated to
  `..._produces_only_hostname` (hostname is no longer conditional, so the
  old "produces nothing" premise no longer holds); every other
  `build_guestinfo` call site updated for the new `vm_name` parameter.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures, 24/24 in `reconciler::vspheremachine`) / `make calm-validate` ✅.
- Infra-name sweep across all tracked + untracked files ✅ clean.

### Not resolved yet
The `ens33` persistence itself is still open — the new log line
(`"template NIC resolved for clone"`, includes `pci_slot`) needs to be read
off the next live clone attempt to know which of the two remaining
hypotheses (slot read-back failed vs. vCenter overriding it post-edit
regardless) is actually happening.

## [2026-08-22 21:30] - Add "Building a Kairos Hadron VM Template" guide

**Author:** Erick Bourgeois

### Added
- `docs/src/guides/building-kairos-hadron-template.md`: new guide covering
  the multi-stage Dockerfile pattern for a custom Kairos Hadron (musl,
  no package manager) OCI image — payload assembly via an Alpine stage
  (gcompat + third-party agent + vmtoolsd extraction), the layered
  cloud-config (`90-base`/`91-custom`/`92-k0s`), a POSIX-`sh`
  guestinfo-driven `configure-network.sh`, building the ISO with
  `auroraboot`, and manually templating it on vSphere with `govc` as an
  alternative to the banlieue-native `VMImage` pipeline. Cross-references
  `using-banlieue-imagebuilder.md` and ADR-0021.
- `docs/mkdocs.yml`: nav entry under **Guides**.

### Why
Covers the step upstream of `using-banlieue-imagebuilder.md` — how to
produce a custom Hadron OCI image in the first place — generalized from a
real internal build pipeline (hostnames, registries, internal paths, and
company-specific identifiers replaced with placeholders per
`rules/no-real-infrastructure.md`).

### Impact
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-22 21:05] - VSphereMachine deletion lifecycle: finalizer + Destroy_Task (ADR-0026)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0026-vspheremachine-deletion-lifecycle.md` — new ADR amending
  ADR-0024's own "Follow-ups" (deletion was explicitly named there as
  deferred, pending the create path being proven).
- `crates/banlieue-provider-vsphere/src/client/mod.rs`:
  `VSphereClient::destroy_vm(vm_moref) -> Result<()>` trait method —
  moref-based (unlike the name+folder-based `destroy_if_present`, which
  belongs to the template import path).
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: real implementation,
  refactored out of `destroy_if_present`'s existing power-off-then-destroy
  sequence into a shared `power_off_and_destroy` helper; idempotent — a
  moref vCenter no longer recognizes is treated as already-destroyed
  success, not an error.
- `crates/banlieue-provider-vsphere/src/client/fake.rs`: `FakeClient`
  support (`destroy_vm` + `destroyed_vms()` accessor) for reconciler tests.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`: new
  `banlieue.io/vspheremachine` finalizer (mirrors the parent
  `VirtualMachine`'s `banlieue.io/virtualmachine`); `reconcile` now checks
  `deletion_timestamp` first, resolves the vCenter client the same way the
  create path does, destroys the backend VM via `finalize_vm` if one was
  ever cloned, then drops the finalizer.
- `docs/architecture/calm/architecture.json`: updated the
  `flow-delete-virtualmachine` control's evidence citation to include the
  provider-side reconciler now that it actually implements what the flow
  already described.

### Why
Found live: deleting a `VirtualMachine` CR removed it and its
`VSphereMachine` from Kubernetes immediately, but the cloned vSphere VM
behind it kept running, orphaned — nothing had ever added a finalizer to
`VSphereMachine`, so nothing blocked its deletion long enough to call
`Destroy_Task`. The parent controller's own `finalize_vm`
(`banlieue-controller/src/reconciler/virtualmachine.rs`) already documents
the intended guarantee ("we never leave the backend with a dangling VM")
and has held up its half since it shipped; this closes the other half.
`docs/architecture/calm/architecture.json`'s `flow-delete-virtualmachine`
already modeled the full two-finalizer chain (written ahead of this
implementation) — the code has now caught up to what was already designed.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- New `vspheremachine_finalize_tests.rs`: `destroys_the_backend_vm_when_one_exists`,
  `is_a_noop_when_no_vm_was_ever_created` (TDD — written before `finalize_vm`).
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures) / `make calm-validate` ✅.
- Infra-name sweep across all tracked + untracked files ✅ clean (also
  swept for the registry hostname pasted into chat this session — not
  written to any tracked file).
- RBAC: no change needed — `deploy/provider-vsphere/rbac/clusterrole.yaml`
  already grants `update`/`patch` on `vspheremachines` (the merge-patch
  finalizer helper targets the main resource, not the `/finalizers`
  subresource rule already present but unused).

### Not addressed here (deliberately, per user decision this session)
Power-state reconciliation on an already-provisioned VM (changing
`desiredPowerState` post-create currently does nothing) and honoring
`VirtualMachine.spec.paused` (schema-only today, never checked by the
controller's reconcile loop) — both real gaps, both scoped to a separate
follow-up ADR rather than folded into this one.

## [2026-08-22 20:10] - Fix cloned VMs losing the template's pinned NIC PCI slot (ens192 → ens33)

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `find_first_nic_key`
  now also reads the template NIC's existing `slot_info` (via `vim_rs`'s own
  documented `AsAny`/`downcast_ref` pattern) and returns it alongside the
  device key and adapter type; `build_nic_edit_device` takes that PCI slot
  and re-pins it (`VirtualDevicePciBusSlotInfo`) on the clone's NIC edit,
  instead of leaving it unset.

### Why
Found live: a VM cloned from a template built at the project's documented
default PCI slot 192 (`ens192`, see `VMImage.spec.template.nicPciSlot` /
`docs/src/reference/api.md`) came up with its NIC as `ens33` — vSphere's
default auto-assigned slot — inside the guest. Per vim_rs's own upstream doc
comment on `pci_slot_number`, the slot should be explicitly set "when the
virtual hardware configuration is duplicated," which is exactly cloning;
`clone_vm`'s NIC `deviceChange` (added for the prior `InvalidDeviceSpec` fix)
edits the NIC's backing in the *same* call that creates the destination VM,
which vCenter evidently treats more like fresh PCI placement than an
in-place `Reconfigure` of a long-lived VM — omitting `slotInfo` let it fall
back to auto-assignment rather than keeping the template's slot.

A stable, predictable interface name matters here because every downstream
piece of static networking (`guestinfo.network.*` + the cloud-config's own
`systemd-networkd` unit, ADR-0024) is written assuming the guest's primary
NIC is `ens192` — an unexpected `ens33` means none of that networking ever
takes effect inside the guest, which was reported live alongside this bug
(no networking on the cloned VM; CrowdStrike Falcon also failed to install,
consistent with no network being the root cause rather than a second, separate
bug).

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- New/updated unit tests in `vim_tests.rs`: `find_first_nic_key_also_reports_the_devices_pinned_pci_slot`,
  `build_nic_edit_device_pins_the_given_pci_slot_when_the_template_had_one`,
  `build_nic_edit_device_omits_slot_info_when_the_template_had_none`, plus
  the existing `find_first_nic_key`/`build_nic_edit_device` tests updated for
  the new `Option<i32>` slot in their signatures.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures)
- Infra-name sweep across all tracked + untracked files ✅ clean.

### Not addressed here
The same live report also mentioned a custom GRUB menu entry
(`install.grub-entry-name`) never taking effect on the clone. That is not a
banlieue bug: per ADR-0021, `install.*` cloud-config keys are read only by
Kairos's one-time disk installer, which already ran when the *template* was
built (from the ISO's own baked-in cloud-config) — a clone boots the
already-installed disk and never re-runs the installer, so a `userData`
Secret delivered at clone time (ADR-0024) can never change it. Setting a
custom GRUB entry name requires baking `install.grub-entry-name` into the
cloud-config embedded in the source ISO and re-importing the `VMImage`
template (`forceCreate`), not editing the per-VM `userData`.

## [2026-08-22 19:30] - Fix CloneVM_Task InvalidDeviceSpec: send the NIC's real concrete type

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `clone_vm`'s NIC
  reconfigure sent a bare `VirtualEthernetCard` — the abstract base type
  every NIC adapter (`VirtualVmxnet3`, `VirtualE1000`, etc.) inherits
  from. vCenter cannot instantiate the abstract base directly; a
  `deviceChange` entry must name a concrete, creatable subtype.
- `find_first_nic_key` now also returns the device's concrete
  `StructType` (previously only its device key); new `build_nic_edit_device`
  uses that type to construct the exact same concrete struct the
  template's own NIC already is, changing only `key` and `backing`.

### Why
Found live: `CloneVM_Task` faulted with `InvalidDeviceSpec`, `"Invalid
configuration for device '0'"`. Root cause was structural, not a value
mistake like the two datastore-name/template-folder fixes before it —
sending the wrong *type* of device object rather than a wrong *value*.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- 3 new pure-function unit tests in `vim_tests.rs`, reusing the existing
  `ethernet_device` fixture helper: `find_first_nic_key` reports the
  correct `(key, StructType)` pair and returns `None` with no NIC present;
  `build_nic_edit_device` round-trips to the *same* concrete type for
  every one of the five adapter types `find_first_nic_key` can report,
  proving the fix directly rather than just re-testing the old behavior.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full
  workspace, 0 failures)
- Infra-name sweep across all tracked + untracked files ✅ clean. Caught
  and fixed during this entry: the *previous* CHANGELOG entry (datastore
  moref fix) had quoted a real datastore name straight from the live
  error message pasted into chat — replaced with a placeholder before
  this landed.

## [2026-08-21 22:15] - Fix CloneVM_Task datastore fault: pass the moref, not the display name

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/mod.rs`: `CloneVmRequest.datastore`
  renamed to `datastore_moref` — its doc comment already said "every
  reference is already resolved to a concrete vCenter moref by the
  caller", but the field itself held a **display name**
  (`resolve_concrete_datastore`'s return value), not a moref. `vim.rs`'s
  `clone_vm` built a `ManagedObjectReference` directly from it, which
  vCenter rejected.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  `ensure_vm` now does a second lookup — find the `Datastore` in the
  already-fetched `list_datastores` result whose `.name` matches the
  resolved name, and pass its `.moref` into `CloneVmRequest`.

### Why
Found live: `CloneVM_Task` faulted with `ManagedObjectNotFound` on a
real datastore's display name (e.g. `compute-cluster-01-DS002`), which
was referenced by name instead of moref, so vCenter couldn't resolve it
as a `ManagedObjectReference`. `network_moref`/`template_moref`/
`cluster_moref`/`datacenter_moref` were all already correct (each came
directly from a `.moref` field); `datastore` was the one field missing
the `_moref` suffix that would have made this mismatch obvious at the
call site.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- Updated `ensure_vm` tests to assert against the seeded datastore's
  actual moref (e.g. `datastore-cluster-a-ds-fast-01`) rather than its
  display name — would have caught this the same way the earlier
  `find_template` folder-scoping regression tests did, had they checked
  the field type instead of the value shape.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full
  workspace, 0 failures)
- Infra-name sweep across all tracked + untracked files ✅ clean.

## [2026-08-21 11:45] - Fix cross-zone template collision on clone; add clone destination folder override

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`: `zone_from_job`
  set `ZoneImageStatus.resolvedRef` to `{failure_domain}/{job_name}` — the
  Kubernetes Job's own k8s object name, not the vCenter template's actual
  display name. `find_template` then failed outright (`template ... not
  found`), since no vCenter object is ever named after its import Job.
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: even with the right
  name, `find_template` rooted its `ContainerView` at the whole datacenter,
  not a folder — the exact cross-zone collision `find_vm_moref_by_name` was
  already fixed for (ADR-0020 Decision #5) but this separate,
  clone-source lookup function had not been. Every zone's template shares
  the same display name, so a `VirtualMachine` in one zone could silently
  clone from a *different* zone's template instead of failing.

### Added
- `ZoneImageStatus.templateFolder` (banlieue-api): the per-zone folder a
  `Url`-kind import's template lives in, kept as a separate structured
  field rather than encoding folder+name into one string.
- `VSphereMachineSpec.templateFolder`: threaded through from the above by
  `resolve_template_ref` (banlieue-controller), which now returns
  `(name, Option<folder>)` instead of one opaque string.
- `VSphereClient::find_template` gained a `folder: Option<&str>` parameter
  — folder-scoped when `Some` (per-zone `Url`-kind), datacenter-wide when
  `None` (`Template`-kind, unchanged behavior). New read-only
  `find_folder` helper in `vim.rs` (mirrors `ensure_folder` but never
  creates a missing segment — a lookup must not have that side effect).
- `VirtualMachineSpec.folder`: an explicit, user-settable destination
  placement override for the clone. `build_vsphere_machine` sets
  `VSphereMachineSpec.folder` (destination) to this override when set,
  else defaults to the same per-zone folder the source template lives in
  (`templateFolder`) — so clones land organized the same way templates
  already are, with an explicit escape hatch when that default isn't
  right.

### Why
Found live: `banlieue-provider-vsphere` reported `template "cluster-01/
import-hadron-kairos-v0-1-0-vcenter-ssc-cluster-01" not found` when
reconciling a `VSphereMachine` for a `Url`-kind (per-zone-imported)
`VMImage`. Root cause traced to `resolved_ref` carrying the Job's name
instead of the template's; fixing that alone would still have left a
latent, more serious bug (`find_template`'s datacenter-wide search
matching a different zone's identically-named template), so both are
fixed together. The destination-folder override was requested directly
alongside this fix.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — apply the regenerated CRDs
      (`VirtualMachine`, `VMImage`, `VSphereMachine`) and rebuild+redeploy
      both `banlieue-controller` and `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- New/updated tests: `banlieue-api` (`ZoneImageStatus.templateFolder`
  round-trip/omit, `VirtualMachineSpec.folder` round-trip/omit),
  `banlieue-controller` (`resolve_template_ref`'s Url-kind test rewritten
  to assert the bare name + folder, not the old buggy encoded string),
  `banlieue-provider-vsphere` (`zone_from_job`'s existing test corrected;
  3 new `ensure_vm` tests: finds the template in its own zone folder,
  does **not** find an identically-named template seeded in a *different*
  zone's folder — the exact bug, now a regression test — and the
  `Template`-kind datacenter-wide fallback still works with
  `template_folder: None`).
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full
  workspace: 263 banlieue-api, 82 banlieue-controller, 128
  banlieue-provider-vsphere, 0 failures)
- `make crds` ✅ / `kubectl apply --dry-run=server` on the three changed
  CRDs ✅
- Infra-name sweep across all tracked + untracked files ✅ clean.

## [2026-08-21 10:30] - Wire the hardwareOverride merge into build_vsphere_machine

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-api/src/banlieue/mod.rs`: `DiskOverride`/`HardwareOverride`
  were defined and tested at the type level but never re-exported from
  `banlieue_api::banlieue` — added to the existing `pub use virtualmachine::
  {...}` list.
- `crates/banlieue-controller/src/reconciler/infra.rs`: `build_vsphere_machine`
  had the schema (`VirtualMachineSpec.hardwareOverride`) but never actually
  applied it — `num_cpus`/`memory_mi_b`/each disk's `size_gi_b` were reading
  straight from `VMClass.spec.hardware` unconditionally. Added
  `merge_disk_size_override` (mirroring the existing `merge_ipam_override`
  pattern) and wired `hardware_override.cpus`/`.memory_mi_b` /
  `.disk_overrides` into the three call sites that build `VSphereMachineSpec`.

### Why
`HardwareOverride`/`DiskOverride` (this project's per-VM delta on a shared
`VMClass`'s hardware shape, following the same "delta, not the primary
definition" pattern as `networkOverrides`, ADR-0024) had its schema and
round-trip tests land, but the actual merge into the infra CR was never
implemented — the field would silently do nothing if set. Caught while
verifying this feature end-to-end before testing a live `VirtualMachine`.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy `banlieue-controller`
- [ ] Config change only
- [ ] Documentation only

### Verification
- 5 new tests in `banlieue-controller` (no-override passthrough, cpus+memory
  override, partial override — memory still inherited, disk-size override by
  name, disk override ignored for a non-matching disk name).
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace,
  0 failures)
- Infra-name sweep across all tracked + untracked files ✅ clean.

## [2026-08-21 09:55] - Fix template resolution for zone-based (Url-kind) VMImages

**Author:** Cascade (Windsurf)

### Fixed
- `crates/banlieue-controller/src/reconciler/infra.rs`: `resolve_template_ref`
  now falls back to per-zone `resolvedRef` when the top-level
  `perProvider[].resolvedRef` is `None`. This fixes `Url`-kind images
  (imported per failure domain) which only have zone-level refs — previously
  the controller errored with "has no resolved_ref for provider".
- `InfraBuildError::MissingResolvedImageRef` now includes the `zone` name
  for better diagnostics.

### Added
- `crates/banlieue-controller/src/reconciler/infra_tests.rs`: new test
  `build_vsphere_machine_resolves_template_from_per_zone_ref` covering the
  zone-based fallback path.

## [2026-08-21 09:15] - Remove redundant `source` field from IPAM types, make `prefix` optional

**Author:** Cascade (Windsurf)

### Changed
- `crates/banlieue-api/src/common.rs`: removed `source: IpamSource` field
  from both `IpamSpec` and `IpamShape`. IPAM mode is now inferred from which
  optional field is set: `static` → Static, `pool` → Pool, neither → DHCP.
  Added `pub fn source(&self) -> IpamSource` method to both types.
- `crates/banlieue-api/src/common.rs`: `StaticNetworkShape.prefix` changed
  from `u8` to `Option<u8>` — the class only declares shared parameters;
  per-VM overrides fill in the rest.
- All test files updated to remove `source:` from IPAM constructors and use
  `.source()` method for assertions.
- Provider code (`vspheremachine.rs`): static NIC detection now uses
  `n.ipam.static_.is_some()` instead of `n.ipam.source == IpamSource::Static`.
- All CRDs and API docs regenerated. VMClass no longer requires `source` or
  `prefix` in its IPAM schema.

## [2026-08-21 07:05] - Split VMClass static IPAM: remove per-VM address from class-level type

**Author:** Cascade (Windsurf)

### Added
- `crates/banlieue-api/src/common.rs`: new `StaticNetworkShape` struct
  (prefix, gateway, nameservers, domain — **no address**) for `VMClass`-level
  static IPAM declarations. A class is shared by many VMs, so a concrete
  address can only be expressed per-VM via `networkOverrides`.
- `crates/banlieue-api/src/common.rs`: new `IpamShape` struct (like `IpamSpec`
  but uses `StaticNetworkShape` instead of `StaticIpamConfig`).
- `crates/banlieue-api/src/common_tests.rs`: four new tests for
  `StaticNetworkShape` and `IpamShape` round-trips.

### Changed
- `crates/banlieue-api/src/banlieue/vmclass.rs`: `NetworkInterfaceSpec.ipam`
  changed from `IpamSpec` to `IpamShape` — VMClass CRD no longer has an
  `address` field under `ipam.static`.
- `crates/banlieue-controller/src/reconciler/infra.rs`: `merge_ipam_override`
  now takes `&IpamShape` (class-level) and returns `IpamSpec` (resolved).
- `crates/banlieue-api/src/banlieue/vmclass_tests.rs`,
  `crates/banlieue-controller/src/reconciler/infra_tests.rs`,
  `crates/banlieue-controller/src/reconciler/scheduler_tests.rs`: updated
  test helpers from `IpamSpec` to `IpamShape` where they construct VMClass data.
- `deploy/crds/banlieue.io_vmclasses.yaml`: regenerated (no `address` in static).
- `deploy/crds/infrastructure.banlieue.io_vspheremachines.yaml`: regenerated
  (still has `address` — infra CRs use the resolved `StaticIpamConfig`).
- `docs/src/reference/api.md`: regenerated.

## [2026-08-20 22:50] - Add HardwareOverride to VirtualMachine — per-VM CPU/memory/disk delta on top of VMClass

**Author:** Cascade (Windsurf)

### Added
- `crates/banlieue-api/src/banlieue/virtualmachine.rs`: new
  `HardwareOverride` struct (`cpus: Option<u32>`,
  `memory_mi_b: Option<u32>`, `disk_overrides: Vec<DiskOverride>`) and
  `hardware_override: Option<HardwareOverride>` field on
  `VirtualMachineSpec`. All fields optional — absent means "inherit from the
  VMClass unchanged". New `DiskOverride` struct (`name: String`,
  `size_gi_b: u32`) for per-VM disk size bumps, using the same
  `x-kubernetes-list-type: map` pattern as `networkOverrides`.
  End-user documentation on all override types and fields explains the
  delta-not-primary-definition contract.
- `crates/banlieue-api/src/banlieue/virtualmachine_tests.rs`: eight new tests
  covering hardware_override with cpus-only, memory-only, full, empty-struct,
  disk-only, cpus+memory+disks round-trips, and `x-kubernetes-list-type`
  validation for `diskOverrides`.

### Changed
- `crates/banlieue-api/src/banlieue/virtualmachine.rs`: enriched existing
  `NetworkInterfaceOverride` and `network_overrides` field docs with the same
  "delta, not the primary definition" language for consistency.
- `crates/banlieue-controller/src/reconciler/infra_tests.rs`,
  `migration_tests.rs`, `scheduler_tests.rs`: added `hardware_override: None`
  to `VirtualMachineSpec` initializers.
- `deploy/crds/banlieue.io_virtualmachines.yaml`: regenerated.
- `docs/src/reference/api.md`: regenerated.

## [2026-08-20 15:00] - Simplify userData RBAC: controller resolves + inlines, no operator changes (ADR-0025 revised)

**Author:** Erick Bourgeois

### Changed
- `docs/adr/0025-vspheremachine-userdata-secret-rbac.md`: replaced the
  first draft's operator-managed, per-Provider, per-VM `Role`
  recomputation with a far simpler decision — `banlieue-controller`
  resolves and renders `VirtualMachine.spec.userData` itself and inlines
  the *content* into `VSphereMachineSpec.userData`, gated by one static
  namespace-scoped `Role`/`RoleBinding` in `banlieue-system`. Rejected the
  operator-managed design as overbuilt for this project's current
  single-Provider, single-namespace reality — revisit if multi-tenancy
  becomes real.
- `crates/banlieue-api/src/infrastructure/vsphere_machine.rs`:
  `VSphereMachineSpec.userData` changed from `Option<UserDataSpec>` (a
  Secret reference) to `Option<String>` (the resolved, rendered content).
- `crates/banlieue-controller/src/reconciler/infra.rs`:
  `build_vsphere_machine` gains a `rendered_user_data: Option<&str>`
  parameter (still pure/sync — I/O stays in the caller) and inlines it
  onto `VSphereMachineSpec.userData` directly.
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs`: new
  `resolve_rendered_user_data` — reads `spec.userData`'s Secret and
  placeholder-substitutes it (reusing
  `banlieue_provider_sdk::guestdata::render_placeholders`, the same
  ADR-0024 fixed set), called before `build_vsphere_machine`.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  removed `resolve_rendered_userdata` and its Secret read entirely —
  `reconcile` now passes `machine.spec.user_data.as_deref()` straight
  through to `ensure_vm`; the provider never touches a Secret for this.
- `deploy/controller/rbac/role.yaml` + `rolebinding.yaml` (new): the one
  grant this decision needs — `get` on `secrets` in `banlieue-system`,
  bound to the `banlieue-controller` ServiceAccount. Not added to the
  cluster-wide `ClusterRole`.

### Why
Requested directly: "we should not need multi-tenancy at the moment,
KISS for now." The original ADR-0025 draft (operator watches
`VirtualMachine`, recomputes per-Provider `Role`s) is the right shape
*for* multi-tenancy, which isn't the current reality — a single static
namespaced `Role` covers 100% of the actual deployment (one Provider, one
namespace) with far less code and no new operator watch loop. Also
confirmed this doesn't reverse `banlieue-controller`'s existing "no
Secrets" stance (security review SEC-008) — that review's own comment
explicitly anticipated re-adding a *scoped* (namespace or resourceNames)
Secret rule if a reconciler ever needed one; this is exactly that case.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — apply `deploy/crds/
      infrastructure.banlieue.io_vspheremachines.yaml` (schema change),
      `deploy/controller/rbac/role.yaml` + `rolebinding.yaml` (new), and
      rebuild+redeploy both `banlieue-controller` and
      `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- Updated tests: `banlieue-api` (`vsphere_machine_spec_with_user_data_
  round_trip` now round-trips a plain string), `banlieue-controller`
  (`build_vsphere_machine_threads_rendered_user_data` /
  `_omits_user_data_when_none_rendered` replace the old
  reference-threading test). No unit test for
  `resolve_rendered_user_data` itself (needs a kube client — same posture
  as every other Secret-reading reconciler function in this codebase).
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full
  workspace, 0 failures)
- `make crds` (schema changed) / `make calm-validate` ✅ / `make
  calm-diagrams` ✅ (both the ADR-0024 clone-path relationship and the
  `flow-create-virtualmachine` step now describe *this* userData path,
  not the superseded draft)
- `kubectl apply --dry-run=server` on the regenerated CRD and the new
  `Role`/`RoleBinding` ✅
- Infra-name sweep across all tracked + untracked files ✅ clean.
- Caught and fixed during the mechanical edit: a scripted insertion
  (adding the new parameter to every `build_vsphere_machine(...)` test
  call site) matched `&parent_provider(),` in two tests that already had
  their own trailing argument on the next line, producing a duplicate/
  misordered argument — both fixed before the build was confirmed green.

## [2026-08-20 14:00] - VSphereMachine reconciler: clone + guestinfo, wired live (ADR-0024)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/infrastructure/vsphere_machine.rs`:
  `VSphereMachineSpec.desiredPowerState: PowerState` (defaults `PoweredOn`,
  mirroring `VirtualMachineSpec`'s own default) — threaded through
  `build_vsphere_machine` in `banlieue-controller`.
- `crates/banlieue-provider-vsphere/src/client/mod.rs`: `VSphereClient`
  gained `clone_vm(&CloneVmRequest) -> Result<String>` and
  `set_power_state(vm_moref, desired: PowerState) -> Result<()>`.
  `vim.rs`: real `CloneVM_Task` (relocate onto datastore/pool/folder,
  override CPU/memory, reconfigure the clone's first NIC device onto the
  target port group via a device-key edit, set `extraConfig` in the same
  call, always powered off) + `PowerOnVM_Task`/`PowerOffVM_Task`/
  `SuspendVM_Task`. New shared helpers extracted for reuse with
  `import_iso_template`: `build_nic_backing`, `find_first_nic_key`.
  `fake.rs`: `FakeClient` now records `clone_vm`/`set_power_state` calls
  (`Mutex`-guarded, since `VSphereClient` methods take `&self`) so
  reconciler tests can assert against them without a real vCenter.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`:
  `ensure_vm` — resolves `VSphereMachineSpec`'s names to concrete morefs
  (reusing `import.rs`'s `resolve_concrete_datastore` for SDRS datastore
  clusters), clones from the per-zone template with `build_guestinfo`'s
  `extraConfig`, drives `desiredPowerState` — but only on first
  provision (`status.vmRef` unset; a no-op otherwise, matching ADR-0024's
  create-path-only scope). `reconcile`/`error_policy`: the
  `Controller`-facing wrapper — reads the parent `Provider`, resolves
  credentials/CA bundle (reusing `provider::read_credentials`, now
  `pub(crate)`), resolves and renders `spec.userData` via the new
  `guestdata` module, patches `Ready`/`status.vmRef`/
  `initialization.provisioned`.
- `crates/banlieue-provider-vsphere/src/app.rs`: registers the
  `VSphereMachine` `Controller::new()` watch loop alongside `provider`/
  `vmimage` (namespaced, same `--namespace` scoping as `Provider`).
- `docs/adr/0025-vspheremachine-userdata-secret-rbac.md` (new ADR,
  Proposed — not yet implemented): `VSphereMachineSpec.userData` names an
  arbitrary, user-created Secret that neither the cluster-wide
  `ClusterRole` (deliberately no Secret access, CHAIN-002) nor the
  existing per-instance `Role` (ADR-0003, scoped only to the Provider's
  *own* connection Secrets) can grant access to. Decision: extend
  `banlieue-operator`'s per-instance Role with a `resourceNames` rule per
  userData Secret referenced by a `VirtualMachine` currently scheduled to
  that Provider, recomputed from the full scheduled set (not
  incrementally) so revoking one VM never drops a Secret another live VM
  still needs.

### Why
Continuing ADR-0024 past the TDD-only pass into the actual watch loop and
vCenter calls — the piece that makes a `VirtualMachine` with a static-IP
`networkOverride` actually clone and boot. Found the userData RBAC gap
while wiring `reconcile()`'s Secret read and wrote ADR-0025 rather than
either widen the cluster-wide ClusterRole (reopens CHAIN-002) or inline
cloud-config content into the CRD spec (exposes it via `kubectl get -o
yaml`) — implementation of ADR-0025 is deliberately not in this entry.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy
      `banlieue-provider-vsphere` (new watch loop; RBAC for
      `vspheremachines` was already present in
      `deploy/provider-vsphere/rbac/clusterrole.yaml` from before this
      entry, no change needed there)
- [ ] Config change only
- [ ] Documentation only

### Verification
- 9 new tests: 3 in `banlieue-api` (`desiredPowerState` default/omit/
  round-trip), 2 in `banlieue-controller` (threading), 9 in
  `banlieue-provider-vsphere` for `ensure_vm` (already-provisioned no-op,
  first-provision clone+power, desired-power-state drive, SDRS
  datastore-cluster resolution, guestinfo+userdata passthrough, 4
  not-found error cases) — `clone_vm`/`set_power_state`'s own vim_rs
  calls are, like every other real vCenter mutation in this crate,
  verified live rather than unit tested.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full
  workspace, 0 failures)
- `make calm-validate` ✅ (ADR-0025 added to the tracked list)
- Infra-name sweep across all tracked + untracked files ✅ clean.
- **Known limitation, tracked in ADR-0025**: `VSphereMachineSpec.userData`
  resolution will 403 against real RBAC until ADR-0025 is implemented.
  `networkOverrides` (static IP via `guestinfo.network.*`) is unaffected
  and works independently of that gap.

## [2026-08-20 12:30] - TDD implementation of ADR-0024's create-path logic (VSphereMachine)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api`: `VirtualMachineSpec.networkOverrides` +
  `NetworkInterfaceOverride` (keyed by VMClass interface name,
  `x-kubernetes-list-type: map` on `name`); `common::StaticIpamConfig.domain`;
  `infrastructure::VSphereMachineSpec.userData` (`Option<UserDataSpec>`).
- `crates/banlieue-controller/src/reconciler/infra.rs`: pure
  `merge_ipam_override(class_ipam, override)` — an override always replaces
  the class's `ipam` outright with an explicit static one; wired into
  `build_vsphere_machine`'s NIC-building loop (matches override by
  interface name, falls back to the class's own `ipam` — commonly `dhcp` —
  when none matches) and threads `vm.spec.userData` onto the new
  `VSphereMachineSpec.userData` field.
- `crates/banlieue-provider-sdk/src/guestdata.rs` (new module):
  `GuestDataContext` + `render_placeholders`, the fixed ADR-0024 placeholder
  set (`${VM_NAME}`, `${FQDN}`, `${IP}`, `${PREFIX}`, `${GATEWAY}`, `${DNS}`,
  `${DOMAIN}`) substituted into a raw cloud-config. Backend-agnostic —
  usable by libvirt/Proxmox once they deliver guest data their own way.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs` (new
  module, registered in `reconciler/mod.rs`): pure `build_guestinfo(nics,
  rendered_userdata)` — the `extraConfig` `guestinfo.*` key/value pairs a
  `CloneVM_Task` would set, matching this environment's existing
  hand-provisioned VM convention (`guestinfo.network.*` for the first
  statically-addressed NIC — the convention is flat/non-indexed, so it
  cannot represent more than one primary static network — plus
  `guestinfo.userdata`/`guestinfo.userdata.encoding=base64`).
- `base64 = "0.22"` added to `banlieue-provider-vsphere` (already present
  transitively at this exact version via reqwest/vim_rs, so this adds no
  new third-party code to the build).

### Why
Continuing ADR-0024 (TDD phase, after the ADR + CALM design pass). This
covers every piece of the create-path logic that's pure and unit-testable
today; the `VSphereMachine` watch loop and the actual `CloneVM_Task` /
`Reconfigure` vCenter calls are deliberately not included — consistent with
this crate's established posture (see `vim.rs`'s own doc comments) that
real vCenter mutation is verified live, not unit tested, and is scoped as
explicit follow-up work in both the ADR and `reconciler/mod.rs`'s doc
comment.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout (no watch loop wired in yet — nothing
      observable changes on a live cluster from this entry alone)
- [ ] Config change only
- [ ] Documentation only

### Verification
- 26 new tests across 4 crates, all TDD (written first, confirmed
  compiling-red, then green): 6 in `banlieue-api` (schema round-trips +
  admission-time duplicate rejection on `networkOverrides`), 6 in
  `banlieue-controller` (`merge_ipam_override` + `build_vsphere_machine`
  override/no-override/wrong-name/userData threading), 9 in
  `banlieue-provider-sdk` (`render_placeholders`), 9 in
  `banlieue-provider-vsphere` (`build_guestinfo`, including the
  first-static-nic-wins case and the userdata-together case).
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full
  workspace, 0 failures)
- `make crds` — no new run needed; the schema fields all landed before the
  regen already captured in the ADR/CALM changelog entry above.
- Infra-name sweep across all tracked + untracked files ✅ clean.
- Caught during TDD, both self-corrected before landing: a scripted
  regex fix for the new `StaticIpamConfig.domain` field duplicated the
  field in two just-written tests (fixed); a similar scripted fix for
  `VirtualMachineSpec.network_overrides` incorrectly matched two unrelated
  `ProviderSpec` literals sharing the `paused: false,` anchor line (fixed).

## [2026-08-20 11:00] - ADR + CALM for the VSphereMachine clone reconciler (design only, no code)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0024-vspheremachine-clone-static-ip-cloud-config.md` (new ADR):
  scopes the not-yet-implemented `VSphereMachine` reconciler's create path —
  clone from the per-zone template, static IP + templated cloud-config
  delivered via vCenter `guestinfo`. Decisions: a per-VM
  `VirtualMachineSpec.networkOverrides` (keyed by VMClass interface name)
  for static addressing, since a VMClass-level static IP can't serve more
  than one VM; a fixed, explicit `${VM_NAME}`/`${FQDN}`/`${IP}`/`${PREFIX}`/
  `${GATEWAY}`/`${DNS}`/`${DOMAIN}` placeholder set (not a general
  templating engine) substituted into `spec.userData` before delivery.
- `docs/architecture/calm/architecture.json`: updated `service-provider-vsphere`
  (dropped the stale "planned, no code in-tree" description — Provider +
  VMImage reconcilers are live), `rel-provider-vsphere-kube-api` and
  `rel-provider-vsphere-backend` (now describe the VSphereMachine clone +
  guestinfo path alongside the existing introspection/import-Job paths),
  `flow-create-virtualmachine` step 5, and the `adrs` list (added 0023,
  which was missing, and 0024). Regenerated `docs/src/architecture/*.md` via
  `make calm-diagrams`.

### Why
Following the ADD methodology (ADR → CALM → TDD): the `VSphereMachine`
clone reconciler is architecturally significant (new watch loop, new
spec fields, a new guest-data delivery mechanism) and doesn't exist in code
yet, so this is design-only — no Rust changes in this entry.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

### Verification
- `make calm-validate` ✅ (0 errors, 0 warnings)
- `make calm-diagrams` ✅ regenerated cleanly
- Infra-name sweep across all tracked + untracked files ✅ clean. Caught and
  fixed during drafting: an early version of the ADR (and, found by the same
  sweep, ADR-0023 from earlier this session) used a real hostname/IP/domain
  from the environment as a "confirmed by inspecting a live VM" example —
  replaced with placeholders before landing.

## [2026-08-20 10:15] - Scope vSphere template lookup-by-name to its own zone folder

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `find_vm_moref_by_name`
  now roots its `ContainerView` at a **folder** moref instead of the
  **datacenter** moref, and searches within it non-recursively-scoped to
  that folder's contents. `import_iso_template` resolves (`ensure_folder`)
  its zone's own template folder *before* the idempotency/forceCreate
  check, and passes that folder to both `find_vm_moref_by_name` calls
  (the pre-create idempotency check and the post-create relocate). The
  `VSphereClient::destroy_if_present` trait method gained a `folder: &str`
  parameter for the same reason, plumbed through from
  `crates/banlieue-provider-vsphere/src/import.rs`'s `--force-create` path
  (now computes `effective_folder` once, up front, and reuses it for both
  the early destroy and the later `IsoImportRequest`).

### Why
Every zone's template shares the same display name (the `VMImage` name) —
only the per-zone folder differs (ADR-0020 Decision #5). The old
datacenter-wide lookup-by-name meant one zone's `forceCreate` "destroy
anything already named this" check could match — and destroy — a
*different* zone's in-flight VM. Confirmed live: three concurrent per-zone
import Jobs for the same `VMImage` repeatedly destroyed each other's
just-created VMs (`cluster-01`'s job destroyed the exact moref
`cluster-03`'s job had created moments earlier, then hit the same fate
itself), surfacing as intermittent `ManagedObjectNotFound` /
`"already been deleted or has not been completely created"` faults.
Softening that fault into a warn-and-continue (the initial instinct) would
have masked the symptom without stopping the underlying cross-zone
destruction.

Separately (not fixed here — needs a vCenter admin, not code): the
service account's vCenter role is missing `VirtualMachine.Inventory.Delete`,
surfaced by the same investigation when a job tried to clean up a
genuinely stale, pre-fix orphaned template.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — rebuild + redeploy
      `banlieue-provider-vsphere`
- [ ] Config change only
- [ ] Documentation only

### Verification
- No new unit tests: this file's own doc comment states the vCenter
  mutation paths are "verified against a live vCenter, not here" — the
  fix is a scoping/ordering change to code that talks to `vim_rs`
  directly, with no pure-function surface to unit test. Verified by
  re-tracing the exact live log sequence (matching morefs/timestamps
  across the three zones' pods) that proved the cross-zone destruction,
  confirming the new folder-scoped lookup would not have matched across
  zones.
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets
  --all-features -- -D warnings` ✅ / `cargo test --all` ✅ (full
  workspace, 0 failures, no test count change — expected, per above)
- Pending: live re-run of the three per-zone import Jobs after rebuild to
  confirm no more cross-zone `ManagedObjectNotFound` faults.

## [2026-08-20 09:30] - Explicit failure-domain name override (ADR-0023)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0023-explicit-failure-domain-name-override.md` (new ADR).
- `crates/banlieue-api/src/banlieue/provider.rs`: new
  `FailureDomainNameOverride { datacenter, cluster, name }` struct and
  `Provider.spec.failureDomainNameOverrides: Vec<FailureDomainNameOverride>`
  — opt-in, `x-kubernetes-list-type: map` keyed on `[datacenter, cluster]`
  (rejects two overrides for the same zone at admission, same mechanism as
  `VMImageSpec.sources[]`'s `providerClass` uniqueness).
- `crates/banlieue-provider-vsphere/src/reconciler/provider.rs`:
  `find_failure_domain_name_override` (pure lookup); `build_failure_domain`
  and `discover_inventory` both gain an `overrides` parameter.
  `discover_inventory` now also fails closed (`Error::InvalidSpec`, a new
  variant) if the resulting failure-domain list has two entries with the
  same `.name` — guards against two *different* `(datacenter, cluster)`
  pairs being overridden to the same name, which schema-level uniqueness
  can't express and which would silently reintroduce the ADR-0020
  Decision #5 cross-zone collision.
- `docs/src/guides/vsphere-provider.md`: usage note with an example.

### Why
Requested directly, following ADR-0020 Decision #5 (per-zone template
folder isolation): the auto-computed, collision-safe failure-domain name is
correct but ugly for real enterprise vCenter naming schemes, and it's now
visible in more places than before (a vCenter folder segment, not just an
internal label) — an explicit, opt-in override lets an admin use whatever
simpler convention they already have, without losing the collision-safety
net for zones that don't get one.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout — `kubectl apply -f deploy/crds/` (schema)
      and a rebuild+redeploy of `banlieue-provider-vsphere` (reconciler
      logic), same rollout already in flight for the other pending fixes
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-api --lib` ✅ (237 passed, including 4 new:
  `failure_domain_name_override_round_trip`,
  `provider_spec_omits_failure_domain_name_overrides_when_empty`,
  `provider_spec_with_failure_domain_name_overrides_round_trip`,
  `failure_domain_name_overrides_rejects_duplicate_zone_at_admission`)
- `cargo test -p banlieue-provider-vsphere --lib` ✅ (107 passed, including
  4 new: `discover_inventory_uses_the_override_name_when_one_matches`,
  `discover_inventory_slugifies_an_override_name`,
  `discover_inventory_ignores_an_override_for_an_unmatched_zone`,
  `discover_inventory_fails_when_two_zones_override_to_the_same_name`)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features
  -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace, 0 failures)
- `make crds` / `make calm-validate` — CALM diagrams untouched (no new
  node/relationship, just a spec field on an existing one)
- `kubectl apply --dry-run=server -f deploy/crds/banlieue.io_providers.yaml` ✅
- Caught and fixed during review: an earlier draft of
  `failure_domain_name_override_round_trip` used the maintainer's real
  vCenter datacenter/cluster names as test fixture data — replaced with
  generic placeholders before this landed.

## [2026-08-20 08:10] - Reject duplicate `sources[].providerClass` entries at admission

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/banlieue/vmimage.rs`: `VMImageSpec.sources` gains
  `#[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys"
  = ["providerClass"]))]`. Kubernetes enforces list-map-key uniqueness at
  admission (not just an SSA-merge hint) — a `VMImage` with two `sources[]`
  entries for the same `providerClass` is now rejected by the API server,
  instead of `find_url_source`/`find_vsphere_source` silently picking
  whichever came first.
- `deploy/crds/banlieue.io_vmimages.yaml` / `docs/src/reference/api.md`
  regenerated (`make crds`).

### Why
Discussed while reviewing why `providerClass` lives per-`sources[]`-entry
rather than at `spec.providerClass` (intentional — one `VMImage` binds
multiple backends at once, "one name, many backends"): nothing enforced
that each backend gets *at most one* entry. `x-kubernetes-list-type: map`
is the existing Kubernetes-native mechanism for exactly this, already used
elsewhere on this same CRD (`status.perProvider`, `status.conditions`).

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout (same rebuild+redeploy already in flight)
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-api --lib` ✅ (233 passed, including new
  `sources_rejects_duplicate_provider_classes_at_admission` — written first,
  confirmed failing before the `schemars(extend(...))` attribute was added)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features
  -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace, 0 failures)
- `kubectl apply -f deploy/crds/banlieue.io_vmimages.yaml --dry-run=server` ✅
  confirms the updated schema itself is valid; confirming the actual
  duplicate-rejection behavior live needs the same CRD reapply the other
  pending fixes need — not yet done.

## [2026-08-20 07:45] - Rename `template.folder` to `template.rootFolder`

**Author:** Erick Bourgeois

### Changed
- `VMImageTemplate.folder` → `VMImageTemplate.rootFolder`
  (`crates/banlieue-api/src/banlieue/vmimage.rs`), and every downstream
  touchpoint: the `--folder` CLI flag on `image-import` → `--root-folder`
  (`crates/banlieue-provider-vsphere/src/import.rs`), `ImportForce.folder` /
  `ImportJobInputs.folder` → `root_folder`
  (`crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`), and all
  doc comments, the imagebuilder guide, `examples/07-vmimage-kairos-url-source.yaml`,
  the CALM architecture model (`docs/architecture/calm/architecture.json`,
  regenerated via `make calm-diagrams`), and generated CRDs/API docs
  (`make crds`).

### Why
Requested directly, following the folder-collision fix in the entry below:
`folder` reads as the literal target, which is exactly the wrong impression
now that it's always a root the per-zone import nests under
(`<rootFolder>/<failure-domain-name>`) — `rootFolder` says that plainly.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [ ] Requires cluster rollout (same rebuild+redeploy already in flight)
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test --all` ✅ (full workspace, 0 failures)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features
  -- -D warnings` ✅
- `make calm-validate` ✅ / `make calm-diagrams` ✅
- `kubectl apply --dry-run=client -f deploy/crds/ -f examples/` ✅

## [2026-08-20 07:20] - Fix cross-zone template folder collision (ADR-0020 Decision #5)

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/import.rs`: new `effective_folder`
  helper — `spec.template.folder` is now a *root*, not the literal target;
  the template always lands at `<folder>/<failure-domain-name>`. Wired into
  the `IsoImportRequest` construction in place of `args.folder.clone()`.
- Doc comments updated to match: `VMImageTemplate.folder`
  (`crates/banlieue-api/src/banlieue/vmimage.rs`), the `--folder` CLI flag,
  the imagebuilder guide, `examples/07-vmimage-kairos-url-source.yaml`, and
  the architecture docs (`flows.md`, `concepts/architecture.md`).
- `deploy/crds/banlieue.io_vmimages.yaml` / `docs/src/reference/api.md`
  regenerated (`make crds`) for the updated `folder` field description.

### Why
Found live, while watching the first-ever automated per-zone import run
across all three of a Provider's real failure domains: they share one
datacenter and differ only by cluster, but vSphere's VM/Template folder
hierarchy is scoped per-datacenter, not per-cluster — so `spec.template
.folder` resolved to the identical folder for all three zones, and all
three import Jobs called `CreateVM_Task` for the same template name in
that one folder, actively racing `--force-create` against each other.

### Impact
- [ ] Breaking change (unreleased — no prior stable placement contract to break;
      any template built at the old flat `<folder>/<template-name>` path
      while testing this feature is orphaned and needs manual cleanup in
      vCenter)
- [ ] Requires cluster rollout (same rebuild+redeploy already in flight for
      the Job-naming/RBAC fixes — no separate rollout needed)
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-provider-vsphere --lib` ✅ (103 passed, including
  4 new: `effective_folder_nests_the_zone_under_the_configured_root`,
  `effective_folder_strips_a_trailing_slash_on_the_root`,
  `effective_folder_is_just_the_zone_when_no_root_is_configured`,
  `effective_folder_is_just_the_zone_when_the_root_is_empty`)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features
  -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace, 0 failures)
- `kubectl apply --dry-run=client -f examples/ -f deploy/crds/` ✅

## [2026-08-20 06:35] - Add the missing `banlieue-import` ClusterRole for reading VMImage

**Author:** Erick Bourgeois

### Added
- `deploy/imagebuilder/rbac/clusterrole-import.yaml` (new): `ClusterRole` +
  `ClusterRoleBinding` granting `banlieue-import` `get` on `vmimages.banlieue.io`
  (cluster-scoped, ADR-0001 — a namespaced Role cannot grant access to a
  cluster-scoped kind at all). `image-import`
  (`crates/banlieue-provider-vsphere/src/import.rs`) reads exactly the one
  VMImage named on its own `--vmimage` command-line arg, for the OS ->
  `guestId` mapping — nothing else, so `get` only, not scoped by
  `resourceNames` (unlike the per-Provider Secret/Provider rules, which stay
  resourceNames-scoped): VMImage is catalog metadata, not a credential, so a
  blanket read grant here is proportionate.

### Why
Found live: with the missing `banlieue-import` ServiceAccount fixed, every
import Job's pod immediately started and immediately failed with
`vmimages.banlieue.io "..." is forbidden ... at the cluster scope` — the
next permission gap in the same never-before-exercised path. Audited every
other `Api<...>` call in `import.rs` (`Secret`, `Provider`) against the
operator's existing per-Provider `build_import_role` and confirmed both are
already covered, so this should be the last RBAC gap in this path.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — `kubectl apply -f
      deploy/imagebuilder/rbac/clusterrole-import.yaml`
- [ ] Config change only
- [ ] Documentation only

### Verification
- `kubectl apply --dry-run=client -f deploy/imagebuilder/rbac/clusterrole-import.yaml` ✅
- Live: confirmed via `kubectl logs` on a failed import Job pod — the exact
  `vmimages.banlieue.io ... forbidden ... at the cluster scope` error this
  fixes.

## [2026-08-20 06:10] - Add the missing `banlieue-import` ServiceAccount

**Author:** Erick Bourgeois

### Added
- `deploy/imagebuilder/rbac/serviceaccount-import.yaml` (new): the
  `banlieue-import` ServiceAccount, in the `banlieue-imagebuild` namespace.
  Every per-zone import Job (vsphere ADR-0020, libvirt ADR-0011) sets
  `serviceAccountName: banlieue-import` in its pod template, and
  `banlieue-operator` creates a per-Provider Role + RoleBinding naming it as
  the subject (`workload.rs::IMPORT_SERVICE_ACCOUNT`) — but nothing in
  `deploy/` ever created the ServiceAccount object itself. Starts with zero
  permissions of its own by design; the operator grants exactly the
  credentials-Secret read each import needs, per Provider.

### Why
Found live: the first per-zone import Jobs ever created by the automated
reconciler (after today's collision-name and RBAC fixes) all failed pod
creation with `serviceaccount "banlieue-import" not found`. Invisible until
now because every prior test in this session used the manual `image-import`
CLI subcommand directly, which runs as the operator's own kubeconfig
identity and never creates a Job pod (with a Kubernetes ServiceAccount) at
all.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — `kubectl apply -f
      deploy/imagebuilder/rbac/serviceaccount-import.yaml` (or re-run
      `kubectl apply -R -f deploy/imagebuilder/rbac/`, which already covers
      this new file)
- [ ] Config change only
- [ ] Documentation only

### Verification
- `kubectl apply --dry-run=client -f deploy/imagebuilder/rbac/serviceaccount-import.yaml` ✅
- Live: confirmed via `kubectl describe job` — the exact `FailedCreate` /
  `serviceaccount "banlieue-import" not found` event this fixes.

## [2026-08-20 02:45] - Fix per-zone import Job name collisions + missing RBAC (ADR-0020)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/src/k8s_name.rs` (new): `collision_safe_name(parts: &[&str])`
  — a shared, DNS-1123-safe, collision-resistant Kubernetes name builder.
  Hashes `parts` as structured, NUL-separated fields (not the
  already-joined display string) with a hand-rolled, toolchain-stable
  FNV-1a digest, extracted from `reconciler::provider::failure_domain_name`'s
  existing implementation (which had already hit and fixed this same bug
  once before).

### Fixed
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`:
  `import_job_name` naively truncated `import-<image>-<provider>-<zone>` at
  Kubernetes' 63-char name cap. Found live: real vCenter failure-domain
  names (datacenter + cluster) routinely exceed 63 chars and share
  everything except a short trailing suffix, so all three zones for a given
  `VMImage` truncated to the IDENTICAL Job name — only one zone's import
  ever actually ran per image, and the other two silently reported that
  one Job's status as their own. Now delegates to
  `k8s_name::collision_safe_name`, and takes a new `ImportJobIdentity {
  image, provider, failure_domain }` struct instead of three positional
  `&str` params — a call site can no longer silently compile after
  swapping `image` and `provider`.
- `reconciler::provider::failure_domain_name` now delegates to the same
  shared helper (behavior unchanged — all of its existing tests still pass
  unmodified) and likewise takes a new `FailureDomainIdentity { provider,
  dc, cluster }` struct instead of three positional `&str` params, for the
  same reason.
- `deploy/provider-vsphere/rbac/clusterrole.yaml`: the shared ClusterRole
  bound to every vSphere provider pod had **no `batch/jobs` permission at
  all** — this is the first time the automated per-zone-import reconciler
  ever ran against a real cluster (everything before was manual CLI
  `image-import`), so this was the first chance for the gap to surface.
  Added `get`/`create`/`patch`/`delete` — exactly what
  `ensure_import_jobs`/`create_import_job` use, no more (no `list`/`watch`/
  `update`, since nothing calls them).
- `deploy/operator/rbac/clusterrole.yaml`: added `delete` to its existing
  `batch/jobs` grant (previously `get`/`create`/`patch`, for the libvirt
  provider only, ADR-0011) so the operator's own ClusterRole covers the
  superset of what it now hands the vSphere provider too — caught by the
  existing `operator_cluster_role_covers_every_permission_it_grants_to_providers`
  test.

### Why
Both bugs were invisible until this session because the automated
reconciler path had never been exercised end-to-end before — every prior
test in this session's live-testing used the manual `image-import` CLI
subcommand directly, which talks to vCenter without ever creating a Job or
touching this RBAC at all.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — `kubectl apply` both changed
      `clusterrole.yaml` files, and the already-deployed
      `banlieue-provider-vsphere` image (no code rebuild needed for the RBAC
      fix; the Job-naming fix needs a rebuild+redeploy to take effect)
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-provider-vsphere --lib` ✅ (99 passed, including 8
  new/updated: `k8s_name`'s 6 tests, plus
  `import_job_name_does_not_collide_across_long_failure_domains_sharing_a_prefix`
  and `import_job_name_is_stable_for_the_same_inputs`; all pre-existing
  `failure_domain_name_*` tests pass unmodified against the refactored
  implementation)
- `cargo test -p banlieue-operator --lib bootstrap` ✅ (43 passed, including
  `operator_cluster_role_covers_every_permission_it_grants_to_providers` and
  `the_job_grant_is_the_minimum_the_reconciler_uses`)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features
  -- -D warnings` ✅ / `cargo test --all` ✅ (full workspace, proxy unset, 0
  failures)
- Live: reconciler observed hitting the exact `403 Forbidden` on
  `jobs.batch` predicted by the missing RBAC rule, confirming the diagnosis
  before the fix; the collision was independently reproduced by computing
  the truncated name for all three of `vcenter-ssc`'s real failure domains
  (redacted from all tracked files — see `k8s_name_tests.rs` for the
  synthetic equivalent).

## [2026-08-20 01:30] - Bump `h2` to 0.4.17 to fix RUSTSEC-2026-0258

**Author:** Erick Bourgeois

### Changed
- `Cargo.lock`: `cargo update -p h2 --precise 0.4.17` (transitive dependency
  of `hyper` via `kube`/`reqwest`).

### Why
CI's Security Vulnerability Scan and cargo-deny jobs on PR #20 both failed on
RUSTSEC-2026-0258 (GHSA-q83h-524g-xf6h), a low-severity DoS advisory against
`h2` <0.4.16 (unbounded empty DATA frames), published 2026-08-17 — unrelated
to this branch's own changes, just a newly-published advisory against the
pinned version. `h2` is not a direct dependency; no source code changed.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only
- [ ] Documentation only

## [2026-08-18 14:20] - Docs: cloud-config contract also requires an admin-group user (ADR-0021)

**Author:** Erick Bourgeois

### Changed
- `docs/adr/0021-vsphere-template-install-and-generalize.md`: Decision #1's
  cloud-config contract snippet and prose now include a `users` entry (or
  `install.nousers: true`), documented as found live 2026-08-18.
- `docs/src/guides/using-banlieue-imagebuilder.md`: the "Cloud-config
  contract (ADR-0021)" warning block updated the same way.
- `examples/07-vmimage-kairos-url-source.yaml`: the CONTRACT comment block
  updated the same way.

### Why
Live-testing the auto-manage-install flow against a real cloud-config Secret
that set `install.poweroff`/`reboot` and the identity-wipe stage correctly,
but no `users` entry, hit: `No users found in any stage that are part of the
'admin' group ... In Kairos 3.3.x we no longer ship a default hardcoded user
... require users to provide their own user.` Kairos halts the install stage
immediately in that case and never reaches `install.poweroff`, so the import
Job's wait times out with the exact same symptom as a missing
`poweroff`/`reboot` pair — this is a second, independent way to hit that
failure mode, not covered by the existing contract text. No banlieue code
changed: the contract is documentation only, per ADR-0021 Decision #1 and
non-negotiable #4 (explicit over implicit) — banlieue never reads or edits
the `cloudConfig` Secret.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-08-18 11:30] - Log ISO datastore-upload progress every 10%

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/src/import.rs`: `upload_progress_milestones`
  (pure helper) computes which 10%-multiples were newly crossed between two
  cumulative byte counts out of a known total. `upload_iso_to_datastore`'s
  streaming PUT now taps the `ReaderStream` with `futures::StreamExt::scan`
  (cumulative bytes sent as state) and logs an `info!` "ISO upload progress"
  line — `percent`, `bytes_sent`, `total_bytes` — each time a milestone is
  crossed, without buffering or otherwise altering the stream fed to
  `reqwest::Body::wrap_stream`.
- `UPLOAD_PROGRESS_STEP_PERCENT` constant (10) — the log cadence.

### Why
Multi-gigabyte ISO uploads to a zone's datastore could run for several
minutes with a single "uploading ISO to datastore (streaming)" line and then
silence until completion, making it impossible to tell a slow-but-healthy
upload from a hung one by watching the per-zone import Job's logs.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout (picked up on next `banlieue-provider-vsphere`
      rebuild, same as any other code change to the import Job)
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-provider-vsphere --lib import` ✅ (25 passed,
  including 7 new `upload_progress_*` tests: unknown total size, one
  milestone per normal chunk, no milestone within the same decile, one large
  chunk crossing every milestone, reaching exactly 100% at the last byte,
  capping at 100% if bytes overshoot the total, and a zero-length chunk
  crossing nothing)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features
  -- -D warnings` ✅
- `cargo test --all` ✅ (full workspace, proxy unset — see
  `onprem-env-constraints` — 0 failures)

## [2026-08-18 09:15] - Configurable OSArtifact importer image + pull secrets (ADR-0022 Decision #4)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-imagebuilder/src/importer_image.rs` (new): `ImporterImage {
  reference, pull_secrets }`, defaulting to the built-in `busybox:1.36` with
  no pull secrets; `from_flags` builds it from CLI values.
- `crates/banlieue-imagebuilder/src/app.rs`: two new `Cli` flags —
  `--build-importer-image` / `BANLIEUE_BUILD_IMPORTER_IMAGE` (full image
  reference, default `busybox:1.36`) and `--build-importer-image-pull-secret`
  (repeatable, CLI-only, same precedent as `--build-node-selector`).
- `crates/banlieue-imagebuilder/src/context.rs`: `Context.importer_image:
  ImporterImage`, constructed in `app::run` and threaded into the `VMImage`
  reconciler.
- `crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`:
  `desired_os_artifact` gained an `importer_image: &ImporterImage` parameter.
  The ISO-overlay materializer's `image` now comes from
  `importer_image.reference` instead of a hardcoded constant, and
  `importer_image.pull_secrets` (when non-empty) sets the `OSArtifact`'s
  pod-wide `spec.imagePullSecrets` — unconditionally, not gated on
  `iso_overlay`, since Kubernetes pull secrets are pod-scoped and the main
  build container's image may come from the same mirror.

### Why
The ADR-0022 Decision #3 workaround (dereferencing the ISO-overlay Secret via
a `busybox` init container) hardcoded a public-registry image with no pull
secret. A cluster whose nodes cannot reach public registries at all —
everything pulled through an internal mirror — had no way to run it. This
follows the same cluster-wide, install-time configuration shape as
`ProviderClass.spec.image` (`crates/banlieue-api/src/banlieue/providerclass.rs`):
which registry a cluster can reach is an operator decision made once on the
`banlieue-imagebuilder` Deployment, not a per-`VMImage` field.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout (no CRD change; existing `banlieue-imagebuilder`
      deployments keep working unmodified — new flags default to prior
      hardcoded behavior)
- [x] Config change only (new optional CLI flags / env var on
      `banlieue-imagebuilder`)
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-imagebuilder --lib` ✅ (46 passed, including 5 new/
  updated: `importer_image` module's 3 tests, `desired_os_artifact_uses_the_
  configured_importer_image`, `desired_os_artifact_sets_image_pull_secrets_
  when_configured`, `desired_os_artifact_omits_image_pull_secrets_when_unset`,
  `app::build_importer_image_overrides_parse`)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features
  -- -D warnings` ✅
- `cargo test --all` ✅ (full workspace, 0 failures with the corporate HTTP
  proxy unset — `request_times_out_against_a_hung_endpoint` binds a local
  `TcpListener` and needs a direct loopback connection; the proxy hijacks it)

## [2026-08-17 21:00] - Fix: dereference ISO-overlay Secret before auroraboot sees it (ADR-0022 Decision #3)

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`: `desired_os_artifact`
  no longer points `spec.artifacts.overlayISOVolume` at the raw Secret volume.
  It now adds a second `spec.volumes[]` entry (`iso-overlay`, an `emptyDir`)
  and a `spec.importers[]` init container (`iso-overlay-materialize`,
  `busybox:1.36`) that runs `find /overlay-src -mindepth 1 -maxdepth 1
  -not -name '.*' -exec cp -rL -t /overlay-dst/ {} +` to dereference the
  Secret's symlinks into plain files on the `emptyDir` before the build
  container runs `auroraboot build-iso --overlay-iso`. `overlayISOVolume` now
  names the `emptyDir` (`iso-overlay`); the Secret volume was renamed
  `iso-overlay-source` and is mounted read-only by the importer only.

### Why
Live-testing ADR-0022 against the real vCenter pipeline hit
`Failed creating ISO image: exit status 5` on every build with `isoOverlay`
set. Root-caused with a local, out-of-cluster `docker run
quay.io/kairos/auroraboot:v0.24.0 build-iso --overlay-iso` reproduction
(no Kubernetes involved): kubelet mounts Secret/ConfigMap volumes with each
top-level path component as a symlink into a hidden timestamped directory
(e.g. `boot -> ..data/boot`); `auroraboot`'s overlay-copy step collides that
symlink with the ISO's real `boot/` directory (already populated by an
earlier build step) and silently corrupts it — `xorriso` then fails opaquely
at `exit status 5`. Confirmed with a paired test: an identical overlay with
plain files (no symlinks) builds a valid ISO every time; the exact kubelet
symlink layout reproduces the crash every time. Filed upstream as
[kairos-io/kairos#4324](https://github.com/kairos-io/kairos/issues/4324) —
this change is a `banlieue-imagebuilder`-side workaround, kept independent of
whether/when that gets fixed.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout (existing `VMImage`s with `isoOverlay` set pick
      up the fixed `OSArtifact` wiring on next reconcile; no CRD schema
      change)
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-imagebuilder --lib reconciler::vmimage` ✅ (36 passed,
  including 2 new: `desired_os_artifact_wires_iso_overlay_volume` (updated for
  the two-volume shape) and `desired_os_artifact_wires_iso_overlay_materialize_importer`)
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ (full workspace, no regressions)
- Reproduction: paired `docker run` tests (`overlay-plain` succeeds, exit 0;
  `overlay-symlinked`, matching the exact kubelet mount structure captured
  live from a pod, reproduces `exit status 5` verbatim) — see
  kairos-io/kairos#4324 for the full repro.

## [2026-08-15 12:30] - ADR-0022: vSphere ISO overlay files via OSArtifact volumes

**Author:** Erick Bourgeois

### Added
- `VMImage.spec.isoOverlay` (optional `IsoOverlaySource`: `secretRef` +
  `files[]` of `{key, path}`): overlays additional files (e.g. a
  hand-verified `/boot/grub2/grub.cfg`) onto the ISO `banlieue-imagebuilder`
  builds for a vSphere `Url` source, via kairos-operator's own
  `OSArtifact.spec.artifacts.overlayISOVolume` — the same `auroraboot
  build-iso --overlay-iso` mechanism the maintainer's proven manual
  ISO-build pipeline (`build-kairos-iso.sh`) already relies on.
- `crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`:
  `desired_os_artifact` gained an `iso_overlay` parameter; when set with at
  least one file, it adds `spec.volumes: [{name: "iso-overlay", secret:
  {secretName, items: [{key, path}, ...]}}]` and `spec.artifacts.
  overlayISOVolume: "iso-overlay"` to the `OSArtifact` it already builds.

### Why
Live-testing ADR-0021 surfaced a VM that never completed install because its
ISO had a malformed multi-session structure (`xorriso`: "Chain of ISO session
headers broken at #2") — resolved operationally by deleting the stale
`OSArtifact`/PVC and letting `banlieue-imagebuilder` rebuild it fresh; no code
change was needed for that specific defect. While investigating it, the
maintainer's own proven ISO-build pipeline surfaced a capability
kairos-operator's plain `OSArtifact`-driven build has no equivalent for: a
custom `grub.cfg` overlay. This ADR closes that gap using a mechanism
kairos-operator's CRD already exposes — no kairos-operator changes needed.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout + CRD re-apply
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo test -p banlieue-api --lib` ✅ (232 passed) / `cargo test -p banlieue-imagebuilder --lib` ✅ (38 passed)
- `cargo fmt -p banlieue-api -p banlieue-imagebuilder` ✅ / clippy clean (exit 0, captured directly — not via `tail`)
- `make crds` ✅ (regenerated `deploy/crds/banlieue.io_vmimages.yaml` + `docs/src/reference/api.md`) — confirmed `isoOverlay` present in the generated CRD
- `make calm-validate` ✅ / `make calm-diagrams` ✅
- **No new RBAC for `banlieue-imagebuilder`**: `spec.volumes[].secret.secretName`
  + `items[].key` need only the Secret's name and the caller-declared key
  list, never its content — consistent with the "never touches Secrets"
  posture established for `autoManageInstall` (ADR-0021).
- Every other `auroraboot --set` flag in `build-kairos-iso.sh` was
  cross-checked against banlieue's existing `OSArtifact` generation;
  `iso.overlay_iso` was the only real gap (see conversation for the
  flag-by-flag comparison table).

## [2026-08-15 09:40] - Fix: boot-order Edit device_change needs controllerKey resent, not just key

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: the CD-ROM-connect
  `Edit` `device_change` in `build_boot_order_reconfigure_spec` now resends
  `controllerKey` / `unitNumber` / a rebuilt ISO `backing`, not just `key`.
  Found live: vCenter's `ReconfigVM_Task` rejected the previous version with
  `MissingController` ("Device requires a controller") — an `Edit` on a
  device with a controller apparently requires `controllerKey` to be present
  in the submitted object even when only an unrelated field
  (`connectable`) is actually changing, matching `create-vm.sh`'s
  `govc device.connect` pattern of always re-sending the whole device.
- An intermediate attempt to fix this by fetching and `.clone()`-ing the
  live `VirtualDevice` silently miscompiled: `VirtualDevice` does not
  implement `Clone` (its `backing` field is a boxed trait object), so
  `.clone()` resolved to cloning the `&VirtualDevice` *reference* instead
  (always `Clone`/`Copy`) — a false green that `cargo build --all-targets`
  didn't catch since it doesn't compile `#[cfg(test)]` code; `cargo test`
  caught the real `E0308` mismatch.
- New `CdromPlacement` (a small `Copy` struct: `key`, `controller_key`,
  `unit_number`) replaces the failed clone-the-whole-device approach —
  `find_cdrom_placement` extracts just those `Copy` fields, and the ISO
  `backing` is rebuilt from `IsoImportRequest.iso_datastore_path` (already
  known at the call site) rather than copied off the live device.

### Why
Two live-test cycles against the real vCenter this session: the boot order
itself was wrong (fixed earlier), then the CD-ROM connect step needed to
carry the controller placement along, matching the maintainer's own working
`create-vm.sh` reference exactly.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (no CRD/API change; part of the same pending
      vsphere provider rebuild as the rest of tonight's ADR-0021 work)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅ (exit 0) / `cargo clippy --all-targets --all-features -- -D warnings` ✅ (exit 0, full workspace)
- `cargo test -p banlieue-provider-vsphere --lib` ✅ (83 passed, exit 0) — extended `build_boot_order_reconfigure_spec_connects_cdrom_and_orders_cdrom_disk_ethernet` to assert `controllerKey`/`unitNumber`/`backing` are preserved on the edited device, not just `key`
- `cargo test --all` — full workspace, verifying now
- Note: exit codes from prior verification steps this session were
  momentarily misread when piped through `tail` (which reports its own exit
  status, not the piped command's) — corrected by capturing real exit codes
  to files (`cmd > file 2>&1; echo "EXIT=$?" >> file`) for the remainder of
  this fix.

## [2026-08-15 08:47] - Fix: destroy_if_present must power off a running target first

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `destroy_if_present`
  (added earlier this session for the `--force-create` NFC-lock ordering fix)
  now checks `runtime.powerState` and issues `PowerOffVM_Task` first when the
  target isn't already `poweredOff`, before `Destroy_Task`. Found live:
  `Destroy_Task` on a powered-on VM fails closed with `InvalidPowerState`
  ("cannot be performed in the current state (Powered on)") rather than
  powering it off implicitly. The specific VM hit this because an earlier
  live-test run left it stuck powered-on at the (now-fixed) Boot Manager
  screen, never reaching `install.poweroff`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (no CRD/API change; part of the same pending
      vsphere provider rebuild as the rest of tonight's ADR-0021 work)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features -- -D warnings` ✅ (full workspace)
- `cargo test -p banlieue-provider-vsphere --lib` ✅ (83 passed, proxy unset, no count change — this power-off branch mutates live vCenter and isn't unit-testable, same precedent as the rest of this function)
- `cargo test --all` ✅ — full workspace, 0 failed

## [2026-08-15 07:55] - Fix: boot order must be set post-create, by real device key, not baked into CreateVM_Task

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `import_iso_template`
  now applies boot order via a **separate `ReconfigVM_Task`** right after the
  VM is created (and before power-on), rather than embedding it in the
  initial `CreateVM_Task` config spec. Found live: a boot order referencing
  the create spec's *provisional* (negative) device keys was not reliably
  honored by EFI firmware — the VM still stopped at the interactive Boot
  Manager menu (confirmed by console: the menu's device order visibly
  changed to put the CD-ROM first, proving the boot-order write partially
  landed, but firmware still never auto-selected it).
- Fixed by mirroring `~/dev/vm-build/bin/create-vm.sh` exactly: after
  creation, resolve the CD-ROM/disk/NIC's **real** (positive) device keys
  from `VirtualMachine.config().hardware.device`, explicitly **connect** the
  CD-ROM (`connectable.connected = true`, not just `startConnected`), and set
  `boot_options.boot_order = [cdrom, disk, ethernet]` — matching `govc
  device.connect` + `device.boot -order cdrom,disk,ethernet` — all in one
  `ReconfigVM_Task`, one call before `PowerOnVM_Task`.
- Removed the earlier `boot_retry_enabled`/`boot_retry_delay` attempt (added
  then reverted this same session): unverified against any working
  reference, and the maintainer's own proven scripts
  (`create-kairos-template.sh` / `create-vm.sh`) never use it — the actual
  fix is the create/reconfigure split above.
- New pure helpers `find_disk_key`, `find_nic_key` (alongside the existing
  `find_cdrom_key`) and `build_boot_order_reconfigure_spec`, all
  unit-tested without a live VM.
- Added `info!` logging marking entry into the install phase
  ("entering auto-manage install phase") and the start of the long
  install-wait poll, so a running Job's logs make it obvious it's waiting on
  Kairos rather than looking stuck.

### Why
Live-tested against the real vCenter: after the previous (incorrect)
`boot_retry_enabled` fix, the VM still stopped at the interactive Boot
Manager screen. The maintainer identified the actual cause from their own
working `create-vm.sh`/`create-kairos-template.sh` scripts, which have solved
this exact problem before by never trusting a boot order set in the same
call as device creation.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (no CRD/API change; requires the vsphere provider
      image to be rebuilt to pick up this and the rest of tonight's ADR-0021 work)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features -- -D warnings` ✅ (full workspace)
- `cargo test -p banlieue-provider-vsphere --lib` ✅ (83 passed, proxy unset) — new tests for `find_disk_key`, `find_nic_key`, `build_boot_order_reconfigure_spec`; the 3 stale `build_template_config_spec` boot-order tests replaced (that function sets no boot order at all now, matching `create-kairos-template.sh`)
- `cargo test --all` ✅ — full workspace, 0 failed
- Root-caused by reading the maintainer's own `~/dev/vm-build/bin/create-vm.sh`
  and `create-kairos-template.sh` scripts directly, per their explicit
  instruction to replicate that exact approach rather than a generic
  VMware-quirk guess.

## [2026-08-15 07:10] - Fix: --force-create destroys the stale target too late, wasting a re-upload

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/import.rs`: `--force-create` now
  destroys any pre-existing VM/template of the target name **before** the
  datastore upload/reuse-check, not after. Found live: a stale
  `rhel98-kairos-url` template's CD-ROM still referenced the target ISO on
  `...DS001`; vCenter holds an NFC lock on any file referenced by a
  registered VM's config (even powered off, even disconnected), so its
  datastore HTTP API returned `500 Failed to open disk: NFC_FILE_LOCKED` for
  GET/HEAD on that file. `datastore_file_exists` correctly treats an
  inconclusive HEAD as "absent" (a deliberate fail-safe — see ADR-0008/0020),
  so it silently gave up on the locked datastore member and re-uploaded
  ~1.2GB fresh onto a different, emptier one instead of reusing what was
  already there.
- New `VSphereClient::destroy_if_present(datacenter_moref, name)` (in
  `client/mod.rs`, implemented in `vim.rs` by reusing the existing
  `find_vm_moref_by_name` + `wait_for_task` helpers, stubbed in `fake.rs`),
  called from `import.rs::run()` right after the datacenter resolves and
  before any datastore work. `import_iso_template`'s own existing-target
  check (the `!force_create` → skip-as-no-op path) is unchanged.

### Why
Discovered live while testing ADR-0021's install/generalize flow with
`--force-create` against a template left over from an earlier test run. Not
new to ADR-0021 — the upload-before-destroy ordering predates it (ADR-0020)
— it just never mattered until a real repeat `--force-create` cycle hit a
template that still locked its own source ISO.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (no CRD/API change, no image rebuild required beyond
      the existing ADR-0021 work already pending redeploy)
- [ ] Documentation only

### Verification
- `cargo fmt --all -- --check` ✅ / `cargo clippy --all-targets --all-features -- -D warnings` ✅ (full workspace)
- `cargo test -p banlieue-provider-vsphere --lib` ✅ (79 passed, proxy unset) — no new tests: `destroy_if_present` mutates live vCenter and is verified live only, same precedent as `import_iso_template`/`wait_for_task`
- `cargo test --all` ✅ — full workspace, 0 failed
- Root-caused live: confirmed the file existed on DS001 (`govc datastore.ls`)
  and reproduced the exact `500 NFC_FILE_LOCKED` via a direct `curl` HEAD/GET
  against vCenter's datastore file API with the real credentials, isolating
  the bug before writing the fix.

## [2026-08-14 21:50] - ADR-0021 amendment: autoManageInstall opt-out

**Author:** Erick Bourgeois

### Added
- `VMImage.spec.template.autoManageInstall` (optional `bool`, default `true`):
  `false` reverts a vSphere `Url` source's per-zone import to ADR-0020's
  original behavior — create the VM, attach the ISO, `MarkAsTemplate`
  immediately, no power-on — for a build that isn't Kairos-driven or whose
  install/generalize is managed some other way.
- `--auto-manage-install <true|false>` CLI flag on `banlieue provider vsphere
  image-import`, threaded through `ImportForce` / `ImportJobInputs` /
  `IsoImportRequest`. Declared `action = clap::ArgAction::Set` explicitly —
  clap's implicit bare-flag inference for `bool` fields can only ever set
  `true`, which can't express turning a default-`true` field off.

### Why
An earlier draft had `banlieue-imagebuilder` auto-inject the ADR-0021
cloud-config contract (`install.poweroff`/`after-install-chroot`) into the
user's Secret so they wouldn't have to author it by hand — rejected:
`banlieue-imagebuilder`'s RBAC is explicitly documented as never touching
Secrets of any kind (`deploy/imagebuilder/rbac/clusterrole.yaml`), and this
would have broken that boundary for a convenience served just as well by
documentation. `autoManageInstall` instead controls *whether the sequence
runs at all* — the contract itself stays manual and documented
(`docs/src/guides/using-banlieue-imagebuilder.md`,
`examples/07-vmimage-kairos-url-source.yaml`), and this flag is the escape
hatch for VMImages that don't need it.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout + CRD re-apply

### Verification
- `cargo test -p banlieue-api --lib` ✅ (228 passed) / `cargo test -p banlieue-provider-vsphere --lib` ✅ (79 passed, proxy unset)
- `cargo fmt -p banlieue-api -p banlieue-provider-vsphere` ✅ / clippy clean
- `make crds` ✅ (regenerated `deploy/crds/banlieue.io_vmimages.yaml` + `docs/src/reference/api.md`)
- Verified `--auto-manage-install false` actually parses (not silently
  ignored) by running the binary directly — clap's bare-bool-flag inference
  would otherwise have made the field un-settable to `false` from the CLI.

## [2026-08-14 20:30] - ADR-0021: install + generalize the vSphere template before MarkAsTemplate

**Author:** Erick Bourgeois

### Added
- `VMImage.spec.template.installTimeoutSeconds` (optional `i32`, default 1800):
  bound on how long the per-zone import Job waits for the unattended Kairos
  install to finish and the VM to power itself off before failing the Job.
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: `import_iso_template`
  now powers the created VM on, confirms it started, polls
  `runtime.powerState` for `poweredOff` (the cloud-config's
  `install.poweroff: true` firing once its `after-install-chroot` stage wipes
  `/etc/machine-id` / SSH host keys and the unattended install completes — the
  disk is never rebooted by the build), removes the CD-ROM device, and only
  then `MarkAsTemplate`. New pure helpers `install_poll_max_attempts` and
  `find_cdrom_key` (the latter identifies the CD-ROM via
  `VimObjectTrait::data_type()` rather than `Any` downcasting — vim_rs 0.5's
  generated `AsAny` blanket impl does not round-trip through the
  `VirtualDeviceTrait` object for every device type; `data_type()`'s
  `StructType` tag does).
- `--install-timeout-seconds` CLI flag on `banlieue provider vsphere
  image-import`, threaded through `ImportForce` / `ImportJobInputs` /
  `IsoImportRequest` alongside the existing template knobs.
- `build_template_config_spec` now always sets an explicit
  `boot_options.boot_order` (`[cdrom, disk]`), for every firmware type, not
  only when secure boot is requested. **Found via live testing** of this
  ADR's power-on step (the first time this VM is ever actually started):
  without it, EFI firmware with a blank disk and no prior successful boot
  stopped at the interactive UEFI Boot Manager menu instead of auto-booting
  the installer ISO, hanging the import Job until timeout. Harmless once the
  CD-ROM is later removed — vSphere skips a boot-order entry whose device no
  longer exists and falls through to the disk, which is what a real clone
  wants anyway.

### Changed
- Previously the "template" produced by the per-zone import was an empty VM
  with the ISO still attached — every clone had to boot the installer itself.
  Templates built after this change are fully installed and generalized, with
  no CD-ROM device at all, closing that gap.
- Documented the cloud-config **contract** this relies on
  (`install.poweroff: true` / `install.reboot: false` + an
  `after-install-chroot` identity-wipe stage) in
  `docs/src/guides/using-banlieue-imagebuilder.md` and
  `examples/07-vmimage-kairos-url-source.yaml`, plus a troubleshooting entry
  for the timeout failure mode.
- `docs/architecture/calm/architecture.json`: updated `rel-provider-vsphere-backend`
  and the `flow-build-vmimage-from-oci` flow's step 6 to describe the new
  sequence; `make calm-validate` passes, diagrams regenerated.

### Why
ADR-0020's per-zone import created a template that was never actually
installed, so every clone paid an 8-12 min unattended-install cost and still
needed the install ISO attached at boot. This closes that gap: install once,
generalize once, template once.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout + CRD re-apply
- [ ] Config change only
- [ ] Documentation only

### Verification
- `cargo fmt -p banlieue-api -p banlieue-provider-vsphere` ✅
- `cargo clippy -p banlieue-api -p banlieue-provider-vsphere --all-targets --all-features -- -D warnings` ✅
- `cargo test -p banlieue-api --lib` ✅ (226 passed) / `cargo test -p banlieue-provider-vsphere --lib` ✅ (79 passed, proxy unset)
- `cargo test --all` (full workspace) ✅ — all suites green, 0 failed
- `make calm-validate` ✅ / `make calm-diagrams` ✅ / `make crds` ✅ (regenerated `deploy/crds/banlieue.io_vmimages.yaml` + `docs/src/reference/api.md`)
- Example YAML **not** live-validated against a running API server this
  session (no reachable cluster); the new field's name/type were checked
  against the regenerated CRD instead (camelCase `installTimeoutSeconds`,
  `integer`/`int32`, nullable) plus the serde round-trip unit tests.

## [2026-08-09 21:15] - ADR-0020: fully parameterize the vSphere template (CPU/mem/firmware/NIC/guestId)

**Author:** Erick Bourgeois

### Added
- Every hardware value `create-kairos-template.sh` sets is now an **optional**
  `VMImage.spec.template.*` field; each falls back to the built-in default that
  was previously hardcoded in `vim.rs`:
  - `cpus` (`-c`, default 2), `memoryMib` (`-m`, default 4096)
  - `firmware` (`-firmware`, default `efi`) — reuses backend-agnostic
    `common::Firmware` (`bios` / `efi` / `efi-secure`); vSphere maps
    `efi-secure` → EFI + `bootOptions.efiSecureBootEnabled=true`
  - `networkAdapter` (`-net.adapter`, default `vmxnet3`) — new `NicAdapter`
    enum (`vmxnet3` / `vmxnet2` / `e1000` / `e1000e`); the config-spec builder
    picks the matching `vim_rs` device struct
  - `nicPciSlot` (`ethernet0.pciSlotNumber`, default 192)
  - `guestId` (`-g`) — override; when unset, derived from the VMImage OS
- Threaded end-to-end: `image-import` CLI flags (`--cpus` / `--memory-mib` /
  `--firmware` / `--network-adapter` / `--nic-pci-slot` / `--guest-id`) →
  `ImportForce` / `ImportJobInputs` → `IsoImportRequest` →
  `build_template_config_spec`.

### Changed
- `vim.rs`: dropped the `TEMPLATE_NUM_CPUS` / `TEMPLATE_MEMORY_MB` /
  `TEMPLATE_FIRMWARE_EFI` / `NIC_PCI_SLOT` constants (defaults now live on the
  CLI args); NIC built from a shared `VirtualEthernetCard` wrapped per adapter.
  Added `as_str`/`FromStr` to `common::Firmware` for CLI threading.
- Regenerated CRDs + API reference; updated `examples/07`. New unit tests:
  `import_force_reads_the_full_template_off_the_image`,
  `build_import_job_threads_all_template_hardware_knobs`.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout + CRD re-apply

## [2026-08-09 20:30] - ADR-0020: template network + structured disk (reuse common DiskProvisioning)

**Author:** Erick Bourgeois

### Changed
- **`spec.template.disk`** is now a structured `VMImageTemplateDisk`
  (`size` GiB, `type`, `controller`) replacing the flat `diskGib`. `type` reuses
  the backend-agnostic `common::DiskProvisioning` (`thin` / `thick` /
  `eagerZeroed`) already shared with `VMClass.disks[]` and
  `VSphereMachine.disks[]` — eager-zeroing is the `eagerZeroed` variant, **not** a
  separate `eager` flag (folded in to match the rest of the API and avoid a
  duplicate `DiskProvisioning` type / ambiguous glob re-export).
- `IsoImportRequest`, the `image-import` CLI (`--disk-type` now accepts
  `eagerZeroed`; dropped `--disk-eager`), and `build_template_config_spec`
  (`DiskProvisioning::EagerZeroed → thin=false, eagerlyScrub=true`) updated to
  match. Regenerated CRDs + API reference; updated `examples/07`.

### Added
- **`spec.template.network`**: port group the template's NIC attaches to,
  overriding the zone's first `availableNetworkClasses` target. Threaded via
  `image-import --network` → `ImportForce.network` → the NIC backing resolver.
- **`spec.template.disk.controller`** (`DiskController`: `pvscsi` default /
  `lsiLogic` / `lsiLogicSas` / `busLogic`) — selects the SCSI controller flavor
  in `CreateVM_Task`, mirroring `create-kairos-template.sh -disk.controller`.

### Fixed
- `vim_tests::request_times_out_against_a_hung_endpoint` was missing the
  `ensure_provider()` call every other client-building test makes, so it panicked
  ("no rustls crypto provider") whenever it ran before a test that installs the
  process-global provider. Added the call. (The separate corporate-proxy `127.0.0.1`
  hijack still requires running localhost tests with the proxy env unset.)

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout + CRD re-apply

## [2026-08-09 19:15] - ADR-0020: group template attrs under VMImage.spec.template + folder

**Author:** Erick Bourgeois

### Changed
- **API reorg**: moved `forceUpload` / `forceCreate` / `templateDiskGib` off the
  flat `VMImageSpec` into a nested `spec.template` (`VMImageTemplate`), and added
  `spec.template.folder`. Fields: `folder`, `diskGib`, `forceUpload`,
  `forceCreate`. Regenerated CRDs + API reference; updated `examples/07`.

### Added
- **`spec.template.folder`**: vCenter inventory folder (path under the
  datacenter VM folder, e.g. `templates/kairos`) to place the template in —
  find-or-created via `Folder.CreateFolder` (`VimClientImpl::ensure_folder`,
  mirroring `create-kairos-template.sh`'s `govc folder.create`), then used as
  the `CreateVM_Task` target folder. Threaded via `image-import --folder` +
  `ImportForce.folder` + `IsoImportRequest.folder`.

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet)
- [x] Requires cluster rollout + CRD re-apply

## [2026-08-09 18:30] - ADR-0020: template NIC + configurable disk + idempotent datastore placement

**Author:** Erick Bourgeois

### Added
- Template **NIC**: `CreateVM_Task` now adds a vmxnet3 NIC on the zone's port
  group — distributed-vDS port backing (portgroupKey + switchUuid, resolved via
  `resolve_dvs_port`) or standard device-name backing — at PCI slot 192
  (`slot_info`), matching `create-kairos-template.sh`.
- **Configurable disk**: `VMImageSpec.templateDiskGib` (default 100) + a
  `--disk-gb` flag on `image-import`; the reconciler threads it via
  `ImportForce.disk_gib` → `--disk-gb`. `IsoImportRequest` gains
  `network_moref` / `network_distributed` / `disk_gib`.
- **Idempotent placement**: `image-import` now scans **all** members of a
  datastore-cluster for the ISO and reuses the member already holding it;
  only a fresh placement (or `--force-upload`) falls back to the emptiest
  member (`candidate_datastores` + `pick_emptiest`). Stops re-uploads on re-run.

### Notes
- The template's NIC network is the zone's network; the disk is a template
  default. Per-VM sizing/network belongs to the VirtualMachine / VSphereMachine
  spec at clone time (future provisioning path), not the template.

### Impact
- [x] Requires cluster rollout (new provider image for the Job path)

## [2026-08-09 17:30] - ADR-0020: vSphere CreateVM_Task + MarkAsTemplate (template creation)

**Author:** Erick Bourgeois

### Added
- `VSphereClient::import_iso_template` (`vim.rs`) now actually creates the
  template: resolve the cluster `resourcePool` + datacenter `vmFolder`, build a
  `VirtualMachineConfigSpec` (EFI, pvscsi + blank thin 100 GiB disk, IDE CD-ROM
  backed by the uploaded ISO), `Folder.CreateVM_Task`, poll the task to
  completion (`Task.info` / `TaskInfoStateEnum`), then `MarkAsTemplate`.
  Idempotent by name; `forceCreate` destroys the existing VM/template first.
  Helpers `wait_for_task` + `find_vm_moref_by_name`; `IsoImportRequest` gains
  `datacenter_moref` / `cluster_moref`.
- Enabled the vim_rs `defaults` feature (was off under `default-features=false`)
  so the device structs can be built with `..Default::default()`.

### Not yet
- **NIC** — the DVS port backing needs the port-group key + switch UUID
  (extra DVPG/VDS introspection); the template is disk + ISO only for now, which
  still boots the Kairos installer. Adding the NIC is the immediate follow-up.

### Verification
- Unit-tested control plane (66 vsphere tests); the vCenter mutation is verified
  live (ADR-0019 footing) — run-local `image-import` against the real vCenter.

### Impact
- [x] Requires cluster rollout (new provider image for the Job path)

## [2026-08-09 16:30] - ADR-0020: forceUpload/forceCreate + emptiest-datastore selection

**Author:** Erick Bourgeois

### Added
- `VMImageSpec.forceUpload` / `forceCreate` (bool, default off) — threaded by the
  reconciler onto the import Job as `--force-upload` / `--force-create`, and
  accepted directly on the `image-import` subcommand. Split the old single
  `--force` into the two independent knobs so a bad ISO or a bad template can be
  replaced without manual vCenter cleanup.
- Import is now **idempotent**: default skips the (multi-GB) upload when the ISO
  is already on the datastore (HEAD check) and skips template creation when it
  exists; `forceUpload` deletes+re-uploads (the datastore file API does not
  overwrite in place), `forceCreate` destroys+recreates the template.
- `Datastore` projection gains `free_space_bytes` (`summary.freeSpace`);
  `resolve_concrete_datastore` now picks the **emptiest member** of an SDRS
  datastore-cluster (ties break lexicographically for determinism) instead of
  the first member.
- Reconciler force knobs bundled into `ImportForce { reimport, upload, create }`
  (`reimport` = `banlieue.io/force-reimport` re-run trigger; `upload`/`create`
  from spec). Regenerated CRDs + API reference.

### Impact
- [x] Requires cluster rollout (new provider image for the Job path)

## [2026-08-09 12:00] - ADR-0020: vSphere per-zone import control-plane + datastore upload + force

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`: replaced the
  `PerZoneImportNotImplemented` stub with real per-zone import-Job planning
  (`gate_on_build_artifact` gates on `iso` + `Ready`; `ensure_import_jobs` /
  `build_import_job` / `import_job_name` / `zone_from_job` mirror the libvirt
  pattern; `useContentLibrary` reported as a documented follow-up).
- `crates/banlieue-provider-vsphere/src/import.rs` (new) + `image-import`
  subcommand: resolves the zone (datacenter/cluster/datastore/network) from the
  Provider, verifies the ISO checksum (SEC-004), and **uploads the ISO to the
  zone's datastore** over the vCenter datastore HTTP API (BYOC reqwest, reusing
  Provider creds + CA — vim_rs 0.5 has no file-transfer API).
- `Context` gains `build_namespace` / `import_image` / `import_service_account`
  / `import_tolerations`; `app.rs` wires them + the subcommand.
- **Force re-import**: `--force` flag on `image-import` +
  `banlieue.io/force-reimport` VMImage annotation → the reconciler deletes and
  recreates the per-zone Jobs with `--force`, and the import deletes an existing
  template before recreating it.
- `image-import` gains `--datastore` / `--network` overrides (bypass FD
  capability introspection); **auto-mkdir** of the upload dir
  (`VSphereClient::ensure_datastore_dir` → `FileManager.MakeDirectory`); and
  **datastore-cluster resolution** (`resolve_concrete_datastore`) so an SDRS
  StoragePod name — or a declarative `storageClasses: { datastoreCluster: … }`
  — resolves to a concrete member datastore for the upload target.
- Datastore upload uses a dedicated client with a generous
  `DATASTORE_UPLOAD_TIMEOUT` (1h) instead of the 120s SOAP timeout — a 1.3 GB
  ISO cannot upload in 120s. **Streaming**: the ISO is fed from disk via
  `reqwest::Body::wrap_stream(tokio_util::io::ReaderStream)` with an explicit
  `Content-Length` (the datastore file API wants a known length, not chunked),
  so it never buffers multi-GB in memory. Adds the reqwest `stream` feature +
  `tokio-util` (`io`).

### Not yet (flagged for live-vCenter verification)
- `VSphereClient::import_iso_template` (CreateVM_Task device tree + MarkAsTemplate)
  returns a typed "pending live implementation" error. Everything up to it —
  Job planning, checksum, zone resolution, datastore ISO upload — is implemented
  and unit-tested (`FakeClient`); the vCenter mutation is verified on-prem like
  the ADR-0019 introspection walk. No CRD schema change (flag + annotation only).

### Why
Closes the iter-1 vSphere `Url` stub end-to-end except the final template
creation, which can only be behaviour-verified against a live vCenter.

### Impact
- [x] Requires cluster rollout (new image for the on-cluster provider/subcommand)

## [2026-08-08 15:30] - ADR-0020: typed build artifact + ISO build pipeline (CALM + implementation)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/common.rs`: `CloudConfigSource` (secretRef-first,
  mirrors `CABundleSource`) + `DEFAULT_CLOUD_CONFIG_KEY`.
- `crates/banlieue-api/src/banlieue/vmimage.rs`: `VMImageSpec.cloudConfig`;
  `BuildArtifactKind` (`cloudImage | iso`, kairos-`OSArtifactKind`-aligned).
- `crates/banlieue-api/src/banlieue/provider.rs`: `ProviderSpec.useContentLibrary`
  (bool, default off).
- `crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`:
  `artifact_kind_for_class` — vSphere `Url` sources now request an `iso`
  artifact (libvirt keeps `cloudImage`); `spec.artifacts.cloudConfigRef` is
  emitted from `VMImage.spec.cloudConfig`.
- `docs/architecture/calm/architecture.json` updated (imagebuilder / OSArtifact /
  vmimage / vsphere-backend nodes+relationships); `make calm-validate` passes,
  diagrams regenerated.

### Changed
- **Renamed** `VMImageStatus.rawDiskArtifact` → `buildArtifact`,
  `RawDiskArtifactStatus` → `BuildArtifactStatus` (gains `kind`),
  `RawDiskArtifactPhase` → `BuildArtifactPhase`, and the artifact's `diskFile`
  → `file`. Migrated the writer (`banlieue-imagebuilder`) and readers
  (`banlieue-provider-libvirt`, `-vsphere`) plus all tests/e2e; regenerated
  `deploy/crds/banlieue.io_{vmimages,providers}.yaml` + `docs/src/reference/api.md`.
- `examples/07-vmimage-kairos-url-source.yaml`: documents the ISO/`buildArtifact`
  flow and a `cloudConfig.secretRef` stanza.

### Why
Implements the ADR-0020 build side: kairos-operator builds the vSphere ISO
(`auroraboot build-iso`) with a baked cloud-config from a Secret, banlieue only
orchestrates and types the artifact. One status field now carries either a raw
cloud image (libvirt) or an ISO (vSphere).

### Impact
- [ ] Breaking change (unreleased — no consumers exist yet; CRDs regenerated)
- [x] Requires cluster rollout (new image with the imagebuilder/provider changes)

### Follow-up (NOT in this change)
- vSphere per-zone import Job + `image-import` subcommand + the vim_rs
  datastore-upload / CreateVM / attach-ISO / MarkAsTemplate entrypoint. The
  vSphere reader is migrated and still reports `PerZoneImportNotImplemented`
  until that lands; the vim work is verified against a live vCenter (like
  ADR-0019 introspection), so it is a separate increment.

## [2026-08-08 14:00] - ADR-0020: vSphere per-zone ISO image import (design)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0020-vsphere-per-zone-iso-import.md` (Status: Proposed): decision
  record for turning a `Url`-kind `VMImage` into a per-failure-domain vCenter
  template via an ISO, closing the iter-1 `PerZoneImportNotImplemented` stub.
  - kairos-operator builds the bootable ISO (`OSArtifact.artifacts.iso: true` →
    `auroraboot build-iso`) with a baked cloud-config (`cloudConfigRef`), stored
    in the artifacts PVC — no new banlieue build code.
  - `banlieue-imagebuilder` orchestrates that `OSArtifact` and mirrors status; the
    vSphere provider does the per-zone push (import Job → datastore-upload +
    empty EFI VM + attach ISO + `MarkAsTemplate`), mirroring ADR-0011's libvirt
    import-Job pattern.
  - API changes decided (not yet implemented): typed `VMImageStatus.buildArtifact`
    (`kind: cloudImage | iso`, aligned with kairos `OSArtifactKind`) replacing
    `rawDiskArtifact`; `VMImageSpec.cloudConfig` (`secretRef`-first, mirrors
    `CABundleSource`); `Provider.spec.useContentLibrary` toggle (default off).

### Why
Records the architecture before code per ADD. Confirms the build/push split with
the maintainer: kairos-operator owns the ISO build, banlieue only orchestrates
and distributes. No RPC — handoff is PVC + status only.

### Impact
- [x] Documentation only (this entry; implementation follows in later commits)

## [2026-08-09 01:30] - vSphere capability introspection, iteration 2 (ADR-0019)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0019-vsphere-capability-introspection-iter2.md` + CALM update:
  populate `Provider.status.failureDomains[].attributes` from
  `spec.capabilities` per failure domain.
- `crates/banlieue-provider-vsphere/src/client`: `VSphereClient::list_datastores`
  / `list_networks` + slim `Datastore { datastore_cluster }` / `Network
  { distributed }` types; vim_rs impl reads `ClusterComputeResource.datastore`/
  `.network`, resolves SDRS `StoragePod` membership and DVS port groups;
  `FakeClient`/`Inventory` gain `with_datastore`/`with_network`.
- `crates/banlieue-provider-vsphere/src/reconciler/provider.rs`:
  `discover_inventory` now takes `&ProviderCapabilities` and, per (dc, cluster),
  computes `availableStorageClasses` (`datastore` / `datastoreCluster` targets),
  `availableNetworkClasses` (`portGroup` / `distributedPortGroup`), and passes
  `features` through. Pure `compute_failure_domain_attributes` +
  reachability helpers, unit-tested with `FakeClient`.

### Why
Iteration 1 accepted `spec.capabilities` but left every failure domain's
attributes empty, so the scheduler could not filter by storage/network class.
Verified live against the real vCenter: each of the three failure domains
reports only its own reachable datastore cluster + DVS port group
(`dedicated-0N` / `data-0N`), confirming per-failure-domain precision.

### Notes / follow-ups (in ADR-0019)
- `tagCategory`/`tag` storage targets (need CIS REST) and feature-flag
  *downgrade* are deferred; features are passed through as asserted.
- On-cluster population requires rebuilding the banlieue image; run-local
  verified the behavior.

### Impact
- [ ] Breaking change
- [x] Feature (rebuild image for on-cluster effect); cargo fmt/clippy/test green;
      no real infrastructure identifiers.

## [2026-08-08 18:45] - Generic `bootstrap-operator` install target (any cluster)

**Author:** Erick Bourgeois

### Added
- `Makefile`: `bootstrap-operator` target — cluster-agnostic wrapper around
  `banlieue bootstrap operator` (ADR-0013) for any real cluster (`KUBECONFIG`
  from the env), the on-prem analog of the kind-only `kind-bootstrap-install`.
  Resolves the `$(ARCH)` image digest of `$(REGISTRY)[/$(ORG)]/banlieue:$(VERSION)`
  via `crane` and pins it (`--image-digest`), then waits for the operator +
  controller rollouts. Knobs: `REGISTRY`/`ORG`/`VERSION`/`ARCH`/`NAMESPACE`.

### Why
Deploying banlieue onto the on-prem k0s cluster from an internal registry needed
a repeatable, arch-pinned install path; `kind-bootstrap-install` is kind-locked.
Used to (re)deploy main banlieue with an internal `banlieue:local-dev` image —
controller, operator, and the operator-spawned vSphere provider all came up on
the new image with the provider reporting Ready against the real vCenter.

### Impact
- [x] Tooling only (Makefile; no real infrastructure identifiers — all
      estate values caller-supplied)

## [2026-08-08 18:15] - Generic `docker-image` build target (ARCH + registry)

**Author:** Erick Bourgeois

### Added
- `Makefile`: `docker-image` target — one generic, arch-parameterized image
  build for both `--load` and build+push (`PUSH=true`). Knobs: `ARCH`
  (amd64|arm64, default amd64), `IMAGE_REF` (derived from
  `REGISTRY`/`ORG`/`BINARY`/`IMAGE_TAG`, with `ORG` optional so
  `<registry>/banlieue:<tag>` — the layout `banlieue bootstrap --registry`
  expects — is producible via `ORG=`). Cross-compiles + stages
  `binaries/$(ARCH)` first, then `buildx`-builds with `TARGETARCH=$(ARCH)`.
  New vars `ARCH` / `IMAGE_REF`.

### Why
The existing `docker-build-amd64` / `docker-build-arm64` / `docker-buildx`
targets are arch-hardcoded, duplicated, and tag as `$(BINARY):$(IMAGE_TAG)-arch`
or `$(REGISTRY)/$(ORG)/$(BINARY)` (an `ORG` segment absent from
artifactory-root refs). `docker-image` collapses build/push × arch × registry
into one target that can emit an org-less ref for an internal mirror. Existing
targets are left intact (CI references them).

### Impact
- [x] Tooling only (Makefile; no real infrastructure identifiers — `REGISTRY`
      defaults to `ghcr.io`, all estate values are caller-supplied)
- Image builds/pushes remain operator-run (`rules`: banlieue never builds/pushes
      images itself).

## [2026-08-08 17:30] - Fix: imagebuilder emitted null nodeSelector/tolerations

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`: `desired_os_artifact`
  emitted `spec.nodeSelector: null` / `spec.tolerations: null` (and
  `metadata.ownerReferences: null` when unowned) — a `bool.then(|| …)` `None`
  becomes JSON `null` inside `json!`, not an omitted key. The OSArtifact CRD
  types these as object/array and **rejects `null` with a 422**, so every
  imagebuilder OSArtifact SSA-apply failed when no build scheduling was
  configured. Now the spec/metadata are built incrementally and the optional
  keys are omitted unless set.
- `crates/banlieue-imagebuilder/src/reconciler/vmimage_tests.rs`: regression
  tests asserting the keys are **absent** (via `contains_key`, since `.is_null()`
  can't distinguish absent from null) when empty, and present when set.

### Why
Surfaced running `banlieue imagebuilder` against the real kairos-operator CRD
on-prem: the VMImage reconcile hot-looped on
`OSArtifact … is invalid: spec.nodeSelector … must be of type object: "null"`.
vcsim/unit paths didn't catch it — only a real CRD enforces the field types.

### Impact
- [ ] Breaking change
- [x] Bug fix (rebuild the banlieue image for the on-cluster imagebuilder path;
      run-local already runs the fixed code)

## [2026-08-08 16:10] - Design note: private-CA image pulls for OSArtifact builds

**Author:** Erick Bourgeois

### Added
- `docs/design/kairos-operator-private-ca-unpack.md`: design/contribution note
  for pulling `OSArtifact` `image.ref` sources from an internal registry fronted
  by a **private CA**, **without** disabling TLS verification. Documents (A) the
  upstream kairos-operator change — mount a CA volume + set `SSL_CERT_FILE` on
  the `auroraboot unpack` container (`internal/controller/job.go`
  `unpackAndPackToArtifactsContainer`), plus the `OSArtifactSpec` field to add —
  and (B) the banlieue-side workaround available today: a CA-trusting auroraboot
  `--tool-image` (Dockerfile via `SSL_CERT_FILE`) wired through the operator's
  `--tool-image` flag. Explains why the existing `caCertificatesVolume` /
  `buildEnv` knobs cover only the buildah OCI-build path, not the unpack path.

### Why
Testing `banlieue-imagebuilder` on-prem surfaced that `auroraboot unpack` fails
`x509: certificate signed by unknown authority` against a private-CA mirror, and
kairos-operator exposes no CA mount for that path — only `pullInsecureRegistry`,
which the environment cannot use. Captures the proper upstream fix and the
no-insecure workaround.

### Impact
- [x] Documentation only (no code/CRD change; no real infrastructure identifiers)

## [2026-08-08 15:00] - Vault-backed flux-operator automation for k0s bootstrap

**Author:** Erick Bourgeois

### Added
- `docs/adr/0018-vault-flux-operator-bootstrap.md`: ADR for an opt-in
  (`FLUX_ENABLED=true`) `flux` step that fetches a registry credential from
  HashiCorp Vault via the `vault` CLI (token auth only, `vault kv get` —
  KV v1/v2-agnostic) and pushes flux-operator + `flux-core` manifests onto the
  cluster. Closes the last gap between `scripts/bootstrap-k0s-cluster.sh` and
  the `~/dev/mke-build` Ansible playbook it replaces.
- `scripts/bootstrap-k0s-cluster.sh`: `vault_kv_get`, `check_deps_flux`,
  `render_flux_{operator_install,prereqs,instance,bootstrap}`, and
  backend-agnostic `deploy_flux` (reuses `populate_node_table`/`NODE_TABLE`, so
  no per-backend branching). New `flux` subcommand, wired into `all`. New
  `VAULT_*`/`FLUX_*` env vars, documented in `--print-env-template` — all
  operator-supplied, no hardcoded Vault path, registry, or CA source.
- `docs/architecture/calm/architecture.json`: `network-vault-backend` node +
  `rel-bootstrap-vault` relationship; `system-k0s-bootstrap` description
  updated; ADR-0018 added to the `adrs` list.

### Changed
- `scripts/bootstrap-cluster.prompt.md`: §1 intake and §5 rewritten for the
  automated flux flow (Vault vars instead of manual secret staging); §3's
  command list and the intro paragraph mention the new `flux` step.

### Why
The bash script already mirrored `k0s-control-plane.yaml`'s VM-provisioning
and native k0s-install flow (ADR-0017); this closes the remaining gap —
`k0s-deploy-manifests.yaml` + `roles/flux-prereqs` + `roles/vault` — so the
script + prompt can fully replace the Ansible playbook. The reference repo's
Vault path, registry hostnames, and CA-bundle source are all environment-specific
and were generalized to operator-supplied env vars rather than copied.

### Impact
- [ ] Breaking change
- [x] Tooling only (opt-in; `FLUX_ENABLED` defaults to `false`)
- [x] No real infrastructure identifiers committed (Vault mount/path, registry,
      OCI URL, and CA bundle all come from the untracked `BANLIEUE_ENV_FILE`
      or ambient `VAULT_ADDR`/`VAULT_TOKEN`)

## [2026-08-08 14:20] - Fix: unique failure-domain names + stop reconcile hot-loop

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/reconciler/provider.rs`:
  - **Unique FailureDomain names.** `failure_domain_name` truncated to 63 chars
    from the front, so long cluster names that differ only in their `-01/-02/-03`
    suffix all collapsed to the same name (observed live: 3 clusters → 1 name).
    On overflow it now keeps a readable prefix and appends a stable 32-bit
    FNV-1a hash (`stable_hash8`) of the full slug — deterministic across Rust
    versions, unlike `DefaultHasher`.
  - **Stop the reconcile hot-loop.** `patch_status_success` / `patch_status_failed`
    now seed their conditions from the object's current `status.conditions` (via
    a new `existing` snapshot in `reconcile`), so `set_condition` preserves each
    `lastTransitionTime` when nothing changed. Previously they rebuilt from an
    empty Vec, re-stamping `now()` every pass → the SSA patch always differed →
    new resourceVersion → watch event → reconcile, ~1×/sec. `discover_inventory`
    also now sorts failure domains by name so vCenter's unstable ordering can't
    churn the status either. Result: steady-state reconcile is a no-op; only the
    5-min requeue re-polls.
- `crates/banlieue-provider-vsphere/src/reconciler/provider_tests.rs`: added
  uniqueness + determinism tests and a sorted-order assertion.

### Why
Both surfaced running the provider against the real on-prem vCenter: all three
compute clusters produced one colliding FailureDomain name, and the controller
reconciled ~once a second forever.

### Impact
- [ ] Breaking change
- [x] Bug fix (rebuild the banlieue image for the on-cluster provider path)

## [2026-08-08 13:10] - Fix: vSphere endpoint must be reduced to host for vim_rs

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/client/vim.rs`: added `server_address()`
  to reduce `Provider.spec.connection.endpoint` (a full URL, e.g.
  `https://vcenter/sdk`) to the bare `host[:port]` that `vim_rs` 0.5 expects,
  and used it at the `ClientBuilder::new` call site. vim_rs builds request URLs
  as `https://{server_address}/api/...`, so passing the full endpoint produced
  `https://https://vcenter/sdk/api/vcenter/system?action=hello` and every
  connect failed (`ConnectFailed`). Handles scheme stripping, explicit ports,
  bare hosts, trailing slash, and IPv6 literals.
- `crates/banlieue-provider-vsphere/src/client/vim_tests.rs`: 5 `server_address`
  unit tests.

### Why
Found running the vSphere provider against the real on-prem vCenter: the
`Provider` reconciled and wrote status, but `ProviderReachable=False` with a
mangled `https://https//…` URL. The live harness (`live_vcenter.rs`) shares the
same `build()` path, so it inherits the fix — it had only ever been given a
bare host before.

### Impact
- [ ] Breaking change
- [x] Bug fix (rebuild the banlieue image for the on-cluster provider path)

## [2026-08-08 12:30] - Fix: vSphere provider must accept `--import-image`

**Author:** Erick Bourgeois

### Fixed
- `crates/banlieue-provider-vsphere/src/app.rs`: added `--import-image`
  (`BANLIEUE_IMPORT_IMAGE`, default matches libvirt) to the provider `Cli`.
  `banlieue-operator` (`workload.rs`) passes `--import-image <ref>` to **every**
  spawned provider so the fleet runs one image, but the vSphere `Cli` didn't
  define it — so an operator-spawned vSphere provider Deployment crash-looped at
  arg-parse (`error: unexpected argument '--import-image'`). The flag is accepted
  for parity (vSphere's own import path is a later iteration), matching the
  existing `vsphere_task_timeout_secs` "stable flag matrix" convention.
- `crates/banlieue-provider-vsphere/src/app_tests.rs`: added
  `import_image_override_parses` + a default assertion.

### Why
Discovered while testing on the on-prem k0s cluster: `banlieue bootstrap
operator` (image `:local-dev`) came up, but creating a vSphere `Provider` made
the operator spawn a provider pod that CrashLoopBackOff'd on the unknown flag.
libvirt was unaffected (it already accepts `--import-image`).

### Impact
- [ ] Breaking change
- [x] Bug fix (requires rebuilding the banlieue image for the on-cluster path)
- Note: `make provider-vsphere-run-local` was never affected (it invokes the
  provider directly, without `--import-image`).

## [2026-08-07 11:45] - vSphere bootstrap disables konnectivity by default

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-k0s-cluster.sh`: new `K0S_DISABLE_KONNECTIVITY` (default
  `true`); the native controller install now passes
  `--disable-components=konnectivity-server`. `scripts/bootstrap-cluster.prompt.md`
  documents the rationale.

### Why
On a flat, routable on-prem network the API server reaches kubelets directly, so
the konnectivity tunnel is unnecessary. With it enabled on a multi-controller
cluster that has no single `externalAddress`/VIP, the konnectivity agents pin to
one controller and `kubectl logs/exec/port-forward` against any other controller
fails with "No agent available" (k0s #600/#5503). This bit the live banlieue
cluster; disabling konnectivity (matching the reference on-prem clusters) removes
the failure mode. Applied live to all 3 controllers (rolling restart).

### Impact
- [x] Tooling only (management-cluster bootstrap)

## [2026-08-07 00:00] - Unset proxies unconditionally, on every backend

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-k0s-cluster.sh`: `unset_proxy` now runs unconditionally at
  startup instead of only when `BACKEND=vsphere`. Both backends shell out to
  `kubectl`/`ssh` against on-prem endpoints, and a remote libvirt host
  (`LIBVIRT_URI=qemu+ssh://...`) is just as exposed to a black-holing corporate
  proxy as vCenter is.
- `scripts/bootstrap-cluster.prompt.md`: broadened the proxy guidance to cover
  `ssh` (not just `govc`/`kubectl`) and note the script now does this itself.

### Why
Corporate HTTP proxies black-hole direct on-prem calls; this must not depend
on which backend is selected.

### Impact
- [x] Tooling only

## [2026-08-04 11:00] - Reusable cluster-bootstrap prompt

**Author:** Erick Bourgeois

### Added
- `scripts/bootstrap-cluster.prompt.md`: a generic, placeholder-only prompt that
  drives a full vSphere k0s bootstrap (intake questions → env file → VMs → native
  k0s → MetalLB → flux-operator/`flux-core`). No real identifiers.

### Why
Make the ADR-0017 bootstrap repeatable for new clusters: hand the prompt to
Claude Code, answer the IP / k0s-version / placement questions, and it runs the
documented flow.

### Impact
- [x] Documentation/tooling only

## [2026-08-04 09:15] - vSphere bootstrap installs k0s natively (not k0sctl)

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-k0s-cluster.sh`: the vSphere backend now installs k0s
  **natively** instead of via k0sctl. Each node downloads the k0s binary from
  `K0S_BINARY_BASEURL` into `/opt/k0s/<ver>-amd64` (sha256-verified) and
  symlinks `/usr/local/bin/k0s`; the first controller runs `k0s install
  controller --enable-worker --no-taints -c /etc/k0s/k0s.yaml`, and the rest
  join via `k0s token create` (controller/worker). New `k0s_{config,apply,
  kubeconfig}` dispatch keeps libvirt on k0sctl. Generated `k0s.yaml` supports
  `K0S_NETWORK_PROVIDER` (calico), `K0S_IMAGE_REPOSITORY` (internal mirror),
  and SANs = `API_SAN` + every node FQDN/IP. Dropped k0sctl from the vSphere
  dependency check.
- `docs/adr/0017-vsphere-bootstrap-backend.md`: recorded the native-install
  decision and why k0sctl can't satisfy the `/opt/k0s` + symlink layout the
  estate's Kairos image expects.

### Why
The on-prem Kairos image persists `/opt/k0s`, `/var/lib/k0s`, `/etc/k0s` as
`COS_PERSISTENT` bind mounts and expects the k0s binary under `/opt/k0s`
symlinked to `/usr/local/bin/k0s`. k0sctl always installs the binary directly
to `/usr/local/bin/k0s` and cannot produce that layout, so the vSphere path
mirrors the maintainer's proven native Ansible flow instead.

### Impact
- [ ] Breaking change
- [x] Tooling only (vSphere management-cluster bootstrap)
- [x] No real infrastructure identifiers committed (Artifactory URL, image
      mirror, k0s version live in the untracked `BANLIEUE_ENV_FILE`)

## [2026-08-03 12:30] - vSphere backend for the k0s bootstrap script

**Author:** Erick Bourgeois

### Added
- `docs/adr/0017-vsphere-bootstrap-backend.md`: ADR for a pluggable
  `BACKEND={libvirt,vsphere}` in the management-cluster bootstrap.
- `docs/architecture/calm/architecture.json`: `system-k0s-bootstrap` node +
  `rel-bootstrap-vsphere` / `rel-bootstrap-libvirt` relationships; ADR-0017
  registered. `make calm-validate` passes; diagrams regenerated
  (`docs/src/architecture/system.md`).
- `scripts/bootstrap-k0s-cluster.sh`: vSphere backend — clones cluster-specific
  Kairos templates via `govc`, pins the NIC to `ens192`, sets static networking
  via `guestinfo.network.*` + a systemd-networkd cloud-config stage, injects
  cloud-init via `guestinfo.userdata`, reconfigures CPU/memory/disk, powers on,
  and waits for the baked installer to finish (root fs on `/dev/loop0`). The
  k0sctl/kubeconfig/label half is generalized over a shared node table and
  reused by both backends. Adds `--print-env-template`; unsets HTTP proxies
  before on-prem `govc`/`kubectl` calls.

### Why
banlieue now needs to run on-prem in a VMware vSphere estate to exercise the
vSphere provider against a real vCenter. The libvirt bootstrap can't stand up a
cluster there; rather than fork the script, the k0s half is shared and only VM
create/IP/destroy is backend-specific. Spreading nodes across three vSphere
compute clusters makes each an etcd failure domain (ADR-0002 reasoning).

### Impact
- [ ] Breaking change
- [x] Tooling only (management-cluster bootstrap; no CRD/runtime change)
- [x] No real infrastructure identifiers committed (env-driven + govc
      discovery; real values live in an untracked `BANLIEUE_ENV_FILE`)

## [2026-08-02 21:15] - Pin provider workloads by digest

**Author:** Erick Bourgeois

### Added
`ProviderImage.digest` on `ProviderClass`, and `--image-digest` on
`banlieue bootstrap`. `reference()` emits `repository:tag@digest` when both are
present — the tag documents what the digest is meant to be, the digest is what
actually gets pulled. Digest alone (`repository@digest`) and tag alone both
remain valid; a digest with an empty tag correctly omits the colon, since
`repo:@sha256:…` is not a valid reference.

### Why
A Deployment built from a mutable tag has a spec that is byte-identical across
image pushes, so a new image triggers **no rollout**. `imagePullPolicy: Always`
does not save it: that only applies when a pod is *created*. On this cluster a
provider pod ran an hour-old digest while reporting perfectly healthy, and the
same trap cost time twice in one session — once nearly reporting a fix as
verified while the old code was still running.

A digest changes the spec, so pushing a new image rolls the workload.

### Verified on hardware
All four workloads pinned and running the same digest:

```
controller         sha256:7fe16e14…  OK
imagebuilder       sha256:7fe16e14…  OK
operator           sha256:7fe16e14…  OK
provider-libvirt   sha256:7fe16e14…  OK
```

The provider Deployment is operator-managed, so its pin comes from the
`ProviderClass` rather than a direct patch — confirmed propagating end to end.
Still functional afterwards: `Provider Ready=True`, `VMImage Ready=True`.

### Note
`docker images` shows a local image **ID** (a digest of the image config);
registries report the **manifest** digest. They differ for the same image, so
comparing them means nothing. Kubernetes wants the registry manifest digest,
retrievable without a container runtime:

```sh
curl -sI -H "Authorization: Bearer $TOKEN" \
  https://ghcr.io/v2/<org>/<repo>/manifests/<tag> | grep -i docker-content-digest
```

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — CRD field added
- [ ] Config change only
- [ ] Documentation only

## [2026-08-02 20:00] - Verified: imports go only to declared pools

**Author:** Erick Bourgeois

### Verified on hardware
With the declared-pools fix deployed, the import produced **three** Jobs rather
than four, one per declared storage class, and left the undeclared pool alone:

```
default        3.05 GiB   (declared: standard)
k0s-bootstrap  3.05 GiB   (declared: bootstrap)
images         3.05 GiB   (declared: images)
boot           (none)     — exists on the host, never declared

Ready=True (Reconciled): image available on 1 provider(s)
```

Per-zone status lists exactly the three declared zones; `boot` no longer
appears at all, because it was never a target rather than a failed one.

### Note on rollout
The provider Deployment tracks the mutable `local-dev` tag, so pushing a new
image does not change the Deployment spec and no rollout is triggered —
`imagePullPolicy: Always` only takes effect when a pod is created. The pod kept
running the previous digest until explicitly restarted. Worth remembering
whenever a change appears not to have taken: check the running `imageID`, not
the tag.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Verification only — no source changes in this entry

## [2026-08-02 19:30] - Import only into declared storage pools

**Author:** Erick Bourgeois

### Fixed
**Images were imported into every pool on the host, not the declared ones.**
`target_pools()` read `status.failureDomains[].attributes.raw["pools"]` — the
*discovery* output, listing every pool libvirtd reports. The declared capability
list is `spec.capabilities.storageClasses`, narrowed by probing into
`attributes.availableStorageClasses`.

On the homelab Provider three classes were declared (`default`, `images`,
`k0s-bootstrap`) but four pools exist, so every import also wrote a full 3 GiB
copy into `boot` — storage the admin never asked banlieue to use. That
contradicts non-negotiable #4: capabilities are declared; auto-discovery is a
status-time concern, not a spec-time one.

`target_pools()` now maps declared storage classes to their `target["pool"]`,
filtered to those that survived verification, and **deduplicated**: two classes
may legitimately map to one pool, and without dedup the same multi-gigabyte
transfer would run twice into the same place with the second Job racing the
first.

The test that parsed `raw["pools"]` was removed rather than adapted — its
subject no longer exists.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

Existing volumes in undeclared pools are left alone; banlieue does not delete
what it did not decide to create.

## [2026-08-02 19:00] - VMImage Ready=True: all four pools imported

**Author:** Erick Bourgeois

### Verified
With the host's `images` pool repointed from a stale `/root/...` path to
`/var/lib/libvirt/pools/images` (and built, started, autostarted), the import
ran cleanly across **all four** storage pools:

```
default        3.05 GiB
boot           3.05 GiB
k0s-bootstrap  3.05 GiB
images         3.05 GiB

Ready=True (Reconciled): image available on 1 provider(s)
```

This is the first `Ready=True` on a `VMImage` end to end: OCI reference →
kairos build on the dedicated node → raw disk on a PVC → four import Jobs →
four libvirt volumes → per-zone status → aggregate condition.

It also exercises the ADR-0015 aggregate in both directions. Every earlier run
sat at `Ready=False (ImportFailed)` because one zone was unavailable; the moment
the last zone succeeded the controller flipped it to `True`. "Ready" means ready
everywhere, and that is now demonstrated rather than asserted.

Placement remains PVC-driven: the Jobs carry no `nodeSelector`, only a
toleration, and the scheduler puts them where the artifacts volume is.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Verification only — host pool definition corrected, no source changes

## [2026-08-02 18:30] - Verified: import placement follows the PVC, with no node selector

**Author:** Erick Bourgeois

### Verified on hardware
Re-ran the import end to end after deploying the PVC-driven correction. The
generated Job carries **no `nodeSelector`** — only a toleration — and every pod
still scheduled onto the dedicated build node:

```
nodeSelector: (none)
tolerations:  dedicated=imagebuild:NoSchedule
pods:         all 5 → k0s-04
```

That is the scheduler resolving placement from the bound PV's own
`nodeAffinity`, which is precisely what the removed selector had been
duplicating. The first re-run of the day had accidentally validated the *old*
design, because the running provider still predated the fix; this one exercises
the corrected code.

Volumes confirmed on the host at 3.05 GiB in each of `default`, `boot` and
`k0s-bootstrap`, ~27s per pool.

`images` fails, correctly and informatively: that pool is defined against
`/root/k0s-bootstrap/test-kairos/images`, a path that does not exist on the
host, so libvirt refuses to start it. Per-zone status reports `ImportFailed`
for it and `Reconciled` for the other three, and the ADR-0015 aggregate stays
`False` — "ready" means ready everywhere.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Verification only — no source changes in this entry

## [2026-08-02 17:30] - VMs autostart; loop-device cloud-config actually applies

**Author:** Erick Bourgeois

### Fixed
**The loop-device cloud-config would have been silently ignored.** `files:` had
been written as a *stage name*, a sibling of `after-install:` and `boot:` under
`stages:`. In Kairos/yip `files:` is a property of a stage **step**, so this
created a bogus stage whose entries carried `path`/`content` where a step
expects `name`/`commands`. yip has no such stage: nothing would have been
written, and nothing would have errored. Moved inside the boot step.

### Added
`scripts/bootstrap-k0s-cluster.sh` marks every VM `virsh autostart` before its
first start. libvirt defaults domains to `autostart=disable`, so an unplanned
host power loss leaves a cluster that is defined, healthy, and entirely down
until someone starts each VM by hand — which is exactly what happened today.
Set *before* the first start, so a host that dies mid-bootstrap still recovers.

Teardown needs no change: `virsh undefine` removes the autostart link with the
domain.

### Notes
A host power cut demonstrated the loop-device problem within hours of it being
identified. After the nodes rebooted, `/dev/loop1..7` were gone again — only
`loop-control` and `loop0` remained — because the earlier `mknod` was never
persistent and the running nodes predate the cloud-config fix.

The running cluster was brought to the same end state the corrected
cloud-config produces: `/etc/modules-load.d/banlieue-loop.conf` and
`/etc/modprobe.d/banlieue-loop.conf` written into Kairos's persistent `/etc` on
the build node, plus the device nodes recreated for the running kernel. A
rebuild therefore converges rather than diverges.

`max_loop` still reads `0` on the running kernel: module parameters apply only
at load time, which is why the `mknod` fallback exists in both the pod and the
cloud-config.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only — host/VM configuration and node OS config
- [ ] Documentation only

## [2026-08-02 16:40] - Import Job placement follows its PVC, not a node selector

**Author:** Erick Bourgeois

### Fixed
**A design error of mine, caught in review rather than by a test.** Import Jobs
were given a `nodeSelector` pinning them to the build node, and the operator
forwarded one to every provider to make that possible. That was wrong.

An import Job mounts the artifacts PVC. Kubernetes already decides where such a
pod may run: the scheduler honours the bound PV's own `nodeAffinity`. On
node-local storage it confines the Job to the volume's node without any help
from us; on network-attached storage there is nothing to confine and the volume
can attach anywhere.

The evidence was in the failure message the whole time:

```
0/4 nodes are available: 1 node(s) had untolerated taint(s),
                         3 node(s) didn't match PersistentVolume's node affinity
```

The scheduler had *already* applied the PV's affinity — that is what excluded
three nodes. Only the **taint** excluded the fourth. The selector was redundant
where storage is node-local and actively wrong where it is not, pinning a Job to
a node it has no reason to be on and making it unschedulable if that node is
full, cordoned, or gone.

I had generalised from `local-path` happening to be installed on the test
cluster into a property of the design.

### Changed
- Import Jobs carry **no** `nodeSelector`. Placement follows the PVC.
- Tolerations are kept and reframed: not a placement decision, but permission to
  land on a node the scheduler has already chosen — needed only because a
  dedicated build node is tainted.
- The operator forwards only `--build-toleration` to providers; the node
  selector is imagebuilder-only.
- ADR-0016 and the SDK docs corrected, including recording that node isolation
  has now landed and verifying what it does and does not buy.

**Build-pod pinning is unchanged.** That one is a genuine policy decision — the
pod is `privileged: true`, and confining it is the point of ADR-0016's
follow-up. The distinction is that a build pod is placed by *policy* and an
import Job by the *volume it mounts*.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

## [2026-08-02 16:10] - Loop devices at boot; installer accepts build scheduling

**Author:** Erick Bourgeois

### Fixed
**`modprobe loop` does not create loop devices.** The Kairos cloud-config
loaded the module at boot and its comment assumed `/dev/loop0..7` would appear.
Modern kernels default to `max_loop=0`, which creates loop devices **on demand**
through `/dev/loop-control` — so the module load yielded `loop-control` and
little else, and a privileged build container still could not see a device that
materialised after it started. Builds failed with
`gen-raw-efi-disk (error: open /dev/loop1: no such file or directory)`.

`scripts/bootstrap-k0s-cluster.sh` now:
- writes `/etc/modules-load.d/banlieue-loop.conf` and
  `/etc/modprobe.d/banlieue-loop.conf` (`options loop max_loop=8`), so the
  setting survives reboots without depending on stage ordering;
- runs `modprobe loop max_loop=8` at boot, **and** creates `loop0..loop7` with
  `mknod` if absent. Module parameters only apply at load time, so if anything
  loaded the module earlier the parameter is ignored — creating the nodes
  directly is idempotent and works either way.

### Added
`banlieue bootstrap` accepts `--build-node-selector` and `--build-toleration`
and passes them to the two roles that place build workloads: the imagebuilder
(which sets them on the `OSArtifact`) and the operator (which forwards them to
every provider workload). The controller is deliberately excluded — it schedules
no build workloads and does not declare the flags, so passing them would stop it
starting. Unset emits nothing, since clap rejects an empty value.

Previously these could only be patched onto the Deployments after install, which
meant the documented install path produced a cluster where privileged builds
could land on control-plane nodes.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — new install flags; node config applies on rebuild
- [ ] Config change only
- [ ] Documentation only

The running homelab cluster keeps the manually created loop devices until it
reboots; the cloud-config change takes effect on the next rebuild.

## [2026-08-02 15:00] - The ADR-0010 pipeline runs end to end on real hardware

**Author:** Erick Bourgeois

### Verified
On the rebuilt homelab cluster (Kairos Hadron, k0s v1.35.5, 3 control-plane +
1 dedicated worker), the whole pipeline ran for the first time:

```
OCI image → kairos OSArtifact build → raw disk on PVC → import Job → libvirt volume
```

Volumes confirmed **on the libvirt host**, read back with our own client:

```
default pool: kairos-ubuntu-2404.raw → /var/lib/libvirt/images/kairos-ubuntu-2404.raw
boot pool:    kairos-ubuntu-2404.raw → /var/lib/libvirt/boot/kairos-ubuntu-2404.raw
```

3.27 GB streamed per pool. Three of four pools imported; the fourth reported
`storage pool 'images' is not active` — a real host condition, surfaced with an
actionable message rather than a crash.

**ADR-0016 node isolation confirmed in practice.** The build pod ran
`privileged: true` on the dedicated tainted worker and nowhere else, with the
`nodeSelector` and `toleration` propagated banlieue-imagebuilder → OSArtifact →
kairos → pod.

**The status model behaved as designed**: per-zone `ready` for the three that
worked, `ImportFailed` for the inactive pool, and the ADR-0015 aggregate
correctly `False` — "ready" means ready everywhere.

### Fixed
- **The operator never forwarded build scheduling to providers.** Import Jobs
  carried no `nodeSelector`/`toleration`, and with node-local storage the
  artifacts PV is pinned to the (tainted) build node — the only node that could
  mount it was the one the Job could not tolerate. New operator flags forward
  both; tests assert they are also *omitted* when unset, since clap rejects
  empty values.
- **Import Jobs referenced an image that does not exist.** `--import-image`
  defaulted to the released `:v0.1.0` while the provider ran `:local-dev` —
  version skew by construction, since the Job runs the *same binary* as the
  provider. The operator now passes the resolved ProviderClass image.

### Notes
`/dev/loop*` had to be pre-created on the build node — a recurrence of bug-098
in a new guise. The module was loaded and `loop0`/`loop1` existed, but
`max_loop=0` means devices are created on demand, and a privileged container
only inherits device nodes that existed **at container-creation time**. The old
fix lived in the Debian cloud-init the Hadron rebuild discarded. The pre-creation
is **not persistent**; the durable fix belongs in the Kairos cloud-config
(`/etc/modules-load.d/loop.conf` + `options loop max_loop=8`).

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

### Follow-ups
- `bootstrap` does not yet accept or forward the build-scheduling flags, so they
  must be patched onto the operator Deployment after install.
- Persistent loop-device configuration for Kairos nodes.

## [2026-08-01 10:20] - Two live-only fixes from the homelab import run

**Author:** Erick Bourgeois

### Fixed
**The import Job mounted a Secret it could not reach.** The manifest projected
the Provider's credentials Secret as a volume, but the Secret lives with the
Provider while the Job runs in the build namespace — and a volume mount cannot
cross namespaces, exactly like the PVC. Pods sat in `ContainerCreating` with
`MountVolume.SetUp failed ... secret "libvirt-creds" not found`.

ADR-0016 §4 designed cross-namespace RBAC for API *reads* of that Secret; it
missed that the manifest also *mounted* it. The mount turned out to be
vestigial — `import.rs` never read the mounted path, it resolves credentials
through the Kubernetes API. Removed the volume and mount entirely, which is
strictly better than fixing the path: the Secret is no longer projected into the
filesystem of a pod running in a namespace with no admission floor. The test now
asserts **no** Secret is projected at all.

**The operator deleted its own import RBAC in a loop.** `prune_namespaced()`
took a single `keep` name, but one Provider now legitimately owns two Roles —
the controller's and the import identity's. The import objects carry the
provider labels, so the pruner matched them, saw a name it wasn't keeping, and
deleted them on every reconcile: create → prune → create → prune. `keep` is now
a slice, and the Role/RoleBinding call sites pass both names. Teardown passes an
empty slice, so deletion still removes everything.

### Verified on the homelab cluster
The **build half of ADR-0010 is fully working**:

```
OSArtifact:      Ready          (build pod admitted in banlieue-imagebuild)
PVC:             10Gi Bound     (local-path)
rawDiskArtifact: phase=Ready    diskFile=kairos-ubuntu-2404-build.raw
```

ADR-0016 verified in practice: the same kairos manifest that `restricted`
rejected is admitted in the privileged build namespace.

The import half reached: 4 Jobs created, one per storage pool, in the correct
namespace, with zone status translated onto `perProvider[].zones[]` and the
ADR-0015 aggregator reporting `Ready=False (Importing): 1 of 1 provider(s)`.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — image `sha256:92142433…` pushed, not yet deployed
- [ ] Config change only
- [ ] Documentation only

### Paused
The cluster is being rebuilt with a dedicated non-control-plane worker so builds
can be pinned to it — the node-isolation follow-up ADR-0016 names as the control
that actually bounds a privileged escape. Resume needs: deploy
`sha256:92142433…`, then `nodeSelector`/`tolerations` plumbing for build pods.

## [2026-08-01 21:50] - ADR-0016 §4: a dedicated read-only identity for import Jobs

**Author:** Erick Bourgeois

### Added
- `IMPORT_SERVICE_ACCOUNT` (`banlieue-import`), created in the build namespace
  by `bootstrap imagebuilder`.
- `build_import_role()` / `build_import_role_binding()` in the operator: a
  per-Provider Role in the **Provider's** namespace, bound to a subject in the
  **build** namespace. Applied for every Provider alongside the existing set.
- `Context::imagebuild_namespace` on the operator; `--import-service-account`
  on the libvirt provider.

### Why
The import Job runs in the privileged build namespace (ADR-0016) but must read
the `Provider` and its credentials, which live with the Provider. That is a
cross-namespace read, so the RoleBinding lives with the Role and the Secret it
grants, and names a subject in the build namespace.

**It does not reuse the provider controller's ServiceAccount.** That identity
can create Jobs (ADR-0011); a workload in a namespace with no admission floor
holding it could create further privileged pods. The import identity is
read-only by construction — every rule is `get` on a named object, pinned by a
test that walks the rules and rejects any other verb, any unscoped rule, and
`jobs` outright.

A ConfigMap rule is emitted only when the Provider actually names a CA
ConfigMap: a rule with an empty `resourceNames` grants access to *every*
ConfigMap in the namespace, so the absent case must omit the rule rather than
narrow it.

### Changed
- The libvirt provider's `import_service_account` is now a plain `String`
  defaulting to `banlieue-import`, replacing the `POD_SERVICE_ACCOUNT` downward-
  API lookup and the namespace-match guard. Both existed to hand the
  controller's own identity down safely; that approach is superseded.

### Verified
```
NS: banlieue-imagebuild  enforce=privileged  audit=restricted  warn=restricted
SA: banlieue-import      in banlieue-imagebuild
SA: banlieue-imagebuilder in banlieue-system
```

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — new ServiceAccount, new per-Provider RBAC
- [ ] Config change only
- [ ] Documentation only

### Follow-ups
- Node-level isolation for build pods (ADR-0016 "what this does NOT buy").
- Re-run the homelab install end to end on a rebuilt image.

## [2026-08-01 21:20] - ADR-0016: isolate image builds in their own PodSecurity domain

**Author:** Erick Bourgeois

### Added
- `docs/adr/0016-imagebuild-namespace-isolation.md` — amends ADR-0010.
- `build_imagebuild_namespace()` + `DEFAULT_IMAGEBUILD_NAMESPACE`; `bootstrap
  imagebuilder` now creates `banlieue-imagebuild`.
- A CALM control (`imagebuild-privilege-domain`) on the imagebuilder node,
  mapped to NIST SP 800-53 AC-6 / SC-39 / CM-7.

### Fixed
**Namespaces created by `banlieue bootstrap` carried no PodSecurity labels at
all.** `build_namespace()` set only the app label, while
`deploy/controller/namespace.yaml` sets `enforce/audit/warn: restricted`. The
documented CLI install path (ADR-0013) therefore produced a *less hardened*
cluster than the manifest path, and nothing tested it. That silently voided the
premise ADR-0016 rests on — that `banlieue-system` is `restricted`.

### Changed
- `banlieue-system`: `enforce/audit/warn: restricted`, from the CLI path too.
- `banlieue-imagebuild`: `enforce: privileged`, `audit/warn: restricted`.
- Both `--build-namespace` defaults → `banlieue-imagebuild` (they must agree; a
  PVC cannot be mounted across namespaces).

### Why
kairos-operator's `OSArtifact` builder needs `privileged: true` for loop devices
and mounts. `privileged` is denied by **`baseline` as well as `restricted`**, so
there is no intermediate profile — the hosting namespace must enforce
`privileged`, which is the *absence* of enforcement.

`banlieue-system` holds the controller, the operator (an RBAC **grantor**, so a
compromise there escalates by design), and one provider pod per backend, each
holding that backend's credentials. Relaxing it for one workload would remove
the admission floor from all of them, and the blast radius grows with every
backend added.

`audit`/`warn` stay `restricted` in the build namespace deliberately:
enforcement is off, so a *new* privileged workload appearing there must still be
visible rather than indistinguishable from the one knowingly allowed.

### Security limits — stated explicitly
This bounds **admission surface, not escape capability.** A privileged container
can access host devices, mount the host filesystem, and escape to its node
regardless of namespace, then read every secret the kubelet materialised there.
The control that actually bounds an escape is scheduling builds onto dedicated,
tainted nodes — complementary, **not** adopted here (needs a node pool a
single-node homelab lacks), tracked as a follow-up.

Until that lands the honest posture is: **a compromised kairos build image is a
node compromise.** The split limits which credentials sit beside it; it does not
sandbox the build. This should not be described as "isolating" the build in a
security sense.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — new namespace, changed namespace labels
- [ ] Config change only
- [ ] Documentation only

### Follow-ups
- The import Job needs a ServiceAccount in `banlieue-imagebuild` plus a
  `resourceNames`-scoped Role/RoleBinding back into the Provider's namespace
  (ADR-0016 §4). Not yet implemented — `import_service_account()` still drops
  the SA on namespace mismatch rather than using the build-namespace identity.
- Node-level isolation for build pods.

## [2026-07-31 20:20] - Prefer the host cross-linker over `cross`; publish an amd64 image

**Author:** Erick Bourgeois

### Fixed
`make build-linux-amd64` failed on Apple Silicon with `couldn't install
toolchain stable-x86_64-unknown-linux-gnu`. `_build-linux` tried `cross` first,
and `cross` attempted to install a **host** rustup toolchain for a foreign
architecture — which rustup refuses outright — even with a working cross-linker
on `PATH`.

Reordered: a host gcc cross-toolchain is now preferred, with `cross` as the
fallback. That is also what `~/dev/bindy` does, and it avoids a container build,
so it is substantially faster (26m48s for a cold release build of the whole
dependency tree).

### Verified
- `ghcr.io/firestoned/banlieue:local-dev` pushed, `linux/amd64`, digest
  `sha256:eb9812fd59df…9687`. The homelab k0s nodes are amd64, and the previous
  `local-dev` image was arm64 from the kind run — it would have failed to run
  there with an exec-format error.
- The package is **public**: an unauthenticated manifest fetch returns 200, so
  the nodes need no `imagePullSecret`.

### Notes
The x86_64 half of the homebrew `macos-cross-toolchains` pair was not installed
(only aarch64 was), so `x86_64-unknown-linux-gnu-gcc` was absent under both its
prefixed and Debian-style names. Installed from the tap that was already
configured. Worth knowing: that toolchain provides *both* naming conventions, so
the Makefile's Debian-style `x86_64-linux-gnu-gcc` resolves correctly once it is
present.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

Build tooling; no shipped code changed.

## [2026-07-31 20:40] - Live validation of the libvirt client against a real host

**Author:** Erick Bourgeois

### Verified
The whole `banlieue-libvirt` protocol stack, against a real libvirtd over mutual
TLS — the first time any of it has run outside unit tests and `FakeClient`:

- TLS handshake + the undocumented post-handshake confirmation byte
- `CONNECT_OPEN` (with the `AUTH_LIST` preamble)
- `CONNECT_LIST_ALL_STORAGE_POOLS` / `..._NETWORKS` — 4 pools, 1 network decoded
- `STORAGE_VOL_CREATE_XML` then `STORAGE_VOL_UPLOAD` — 4 MiB streamed as ~16
  packets, exercising the chunking loop rather than a single-packet path
- `STORAGE_POOL_LIST_ALL_VOLUMES` — the uploaded volume read back correctly

No protocol defects found. Given that the two bugs found previously (the TLS
confirmation byte and the missing `AUTH_LIST`) were invisible to 61 unit tests
*and* 100% mutation coverage, a clean live run is worth more than the same
assertions repeated in-process.

### Added
- `crates/banlieue-libvirt/tests/live_libvirtd.rs`: `list_volumes_in_a_real_pool`.
  `storage_pool_list_all_volumes` had no live coverage, and it is what makes the
  import idempotent — a decode bug there means re-uploading a multi-gigabyte
  disk on every retry, or failing because the previous attempt's volume still
  exists. Asserts every field decodes non-empty and reports the right pool,
  because a framing error surfaces as a garbled name long before it surfaces as
  an error.

### Notes
The upload test deliberately leaves `banlieue-live-upload-test.raw` in the
target pool so its contents can be compared against the source; remove it with
`virsh vol-delete`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Test-only — one new `#[ignore]`d live test, no shipped code changed

## [2026-07-31 21:40] - Upgrade kube 3 → kube 4.0 (+ k8s-openapi 0.28)

**Author:** Erick Bourgeois

### Changed
- `kube = "~4.0"` (resolves to 4.0.0) and `k8s-openapi = "0.28"` in
  `[workspace.dependencies]`. kube 4.0 pairs with k8s-openapi 0.28
  (Kubernetes v1.36 types) and requires **rust 1.88 — exactly our MSRV**.
  The pin is `~4.0` deliberately: kube 4.1/4.2 require rust 1.89, so a bare
  `4` would silently drift above the MSRV on the next `cargo update`.
- Only code change required: `RoleRef.api_group` became `Option<String>` in
  k8s-openapi 0.28 (Kubernetes 1.36 RBAC shape) — four literals wrapped in
  `Some(...)` in `bootstrap.rs` / `workload.rs`.

### Behaviour notes from the upstream changelog
- Non-watch queries now retry by default (`RetryPolicy::server_retry`),
  which suits reconcilers.
- The global read-timeout default was removed in favour of watcher-level
  timeouts.
- kube's own client tracing is now opt-in (`hyper-util-tracing` feature) —
  left off; `RUST_LOG=kube=debug` no longer emits wire-level spans unless we
  enable it.
- kubeconfig YAML parsing moved from serde-yaml to serde-saphyr (internal to
  kube; our own `serde_yaml` usage is unaffected).

### Verification
- Full workspace `cargo test` — 706 passed; clippy clean; `cargo deny` all
  gates ok. Generated CRDs are **byte-identical** under kube-derive 4.0 (no
  schema drift).
- Runtime smoke against a throwaway kind cluster (created and deleted for
  the purpose): imagebuilder reconciles a `VMImage` — OSArtifact created via
  SSA with the ownerReference, faked `Ready` mirrored into status, and the
  artifact garbage-collected on `VMImage` delete. No errors or panics.
- `cargo fmt --check` flags two files owned by in-flight work elsewhere
  (`bootstrap_tests.rs`, untracked `live_vcenter.rs`) — untouched here.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — new binary; CRDs unchanged
- [ ] Config change only
- [ ] Documentation only

## [2026-07-31 20:55] - Close out the security review: SEC-006 through SEC-017

**Author:** Erick Bourgeois

### Security
Every remaining actionable finding from `security-review-2026-07-31.md`:

- **SEC-006** — new admission policy `banlieue-providerclass-guardrails`:
  `spec.additionalRules` may not grant on `secrets`, use `*` resources/verbs,
  or use `escalate`/`bind`/`impersonate`; `spec.workloadNamespace` may not be
  a Kubernetes system namespace. Verified on kind (all three rejections, plus
  the shipped example classes still apply).
- **SEC-008** — deleted the controller ClusterRole's cluster-wide
  `secrets get,list,watch`: no code in `banlieue-controller` reads a Secret
  (verified by grep); dead privilege, also aggregated into the CAPI manager.
- **SEC-011** — schema constraints on `VMClass`/`VSphereMachine`: cpus
  1–256, memoryMiB 128–4 TiB, disks 1–32, NICs 1–16, disk size 1–65536 GiB,
  MTU 68–65535. CRDs regenerated; a `cpus: 0` object is now rejected by the
  apiserver schema itself.
- **SEC-012** — the vSphere HTTP client gains `connect_timeout` 10s /
  request `timeout` 120s (named constants; the long-running task-polling
  budget stays a separate knob for iter 2+). The libvirt transport was
  already timeout-protected on connect/recv (the review's premise was
  half-stale); the real gap was `Session::send`, which now times out too,
  and the error names the operation and duration.
- **SEC-013** — `Credentials` no longer derives `Debug`; a hand impl shows
  the username and `<redacted>` for the password, with a test.
- **SEC-014 / CHAIN-004** — the VEX tools fail closed: an empty/truncated
  `--binary-symbols` file or a valid-but-empty SBOM now exits non-zero
  instead of emitting `not_affected` for every mapped CVE.
- **SEC-015** — scheduler reject-reasons are capped at 10
  (`MAX_REJECT_REASONS`), rendered as first-N plus `"; … and M more"`, so a
  status condition can no longer exceed etcd's size limits at admin-scale
  topology.
- **SEC-016** — VEX input files are stat-capped at 256 MiB before slurping.
- **SEC-017** — repo-root `.dockerignore` (`.git`, `target/`, docs, deploy,
  …) so the build context can no longer leak into an image; `binaries/`
  stays — it is the only thing the Dockerfile COPYs.
- **SEC-007 / SEC-009 (accepted risks, now documented)** — provider-lifecycle
  guide documents the operator-audit-alert recommendation (alert on
  ClusterRoleBindings created by `banlieue-operator` whose roleRef is not
  `banlieue-provider-*`); the imagebuilder guide and
  `deploy/imagebuilder/namespace.yaml` now state that pod-create in
  `banlieue-imagebuild` is node-root-equivalent.

### Verification
- Full workspace `cargo test` — 704 passed; clippy + fmt clean;
  `make calm-validate` clean.
- Against the kind cluster (K8s 1.31): all 8 CRDs re-applied with the new
  schema constraints; `providerclass-guardrails` rejects secrets rules,
  `kube-system` workloads, and `escalate`, while allowing the benign
  per-backend rules; `cpus: 0` rejected by the CRD schema; shipped examples
  still apply.
- VEX fail-closed and timeout behavior covered by new unit tests (empty
  symbols, empty SBOM, oversized input, hung-endpoint request timeout,
  send-timeout against a non-reading peer).

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — RBAC + CRD schema + optional admission policies
- [ ] Config change only
- [ ] Documentation only

The ClusterRole strips (controller SEC-008, providers earlier) land with the
next deploy. With this entry, all 17 findings are either fixed or documented
as accepted-with-monitoring; the only open items are the deferred feature
follow-ups already tracked in ADRs.

## [2026-07-31 20:25] - SEC-005: bind OSArtifact lifecycle and status to its VMImage

**Author:** Erick Bourgeois

### Security
**SEC-005 — a mirrored `Ready` is now bound to the build that produced it.**
kairos' `OSArtifact.status` carries no `observedGeneration` and no digest
echo (checked the CRD schema: only `phase` and `message`), so there is
nothing kairos-side to tie a `Ready` to the requested spec. The binding is
therefore object identity:

- The `OSArtifact` is applied with an ownerReference to its `VMImage` (UID,
  `controller: true`; a cluster-scoped owner of a namespaced dependent —
  deleting the image garbage-collects the build).
- Each reconcile, `banlieue-imagebuilder` deletes and rebuilds any
  `OSArtifact` that lacks the current `VMImage` UID **or** whose spec does
  not request the current `importFrom`. A stale `Ready` from before a spec
  change — or a foreign pre-created `Ready` — is never mirrored.
- `deploy/imagebuilder/rbac/clusterrole.yaml` gains `delete` on
  `osartifacts` for exactly this (bootstrap embeds the same file).

### Fixed
**bug-119 (caught by the kind verification, before it could ship quietly):
the first cut of the UID check read `ownerReferences` from
`DynamicObject.data` — where `metadata` never is.** kube parses `metadata`
into the typed field and out of the flattened JSON, so `owned` was
permanently false and the controller deleted and recreated every artifact
on every watch event — ~7000 reconciles in 4 minutes, ending in a
tracing-subscriber panic under the load. Fixed to read
`obj.metadata.owner_references`; logged in `.wolf/buglog.json`.

### Changed
- `status.rawDiskArtifact.checksum` (SEC-004, see the previous entry) is part
  of the same regenerated CRD; `sha2` hashes in `HASH_CHUNK_BYTES` chunks.
- Guide: "Integrity and lifecycle" section in
  `docs/src/guides/using-banlieue-imagebuilder.md`.

### Verification
- Full workspace `cargo test` green, clippy + fmt clean; new unit tests for
  owner/spec matching, checksum threading, and `verify_checksum` (published
  sha256/sha512 vectors, mismatch, unsupported algorithm, malformed input);
  the import Job's argv is round-tripped through the real clap parser with
  and without `--checksum`.
- Against a fresh kind cluster (K8s 1.31) with the kairos CRD installed and
  `banlieue imagebuilder` running locally: ownerRef present on created
  artifacts; faked kairos `Ready` mirrors with checksum; changing
  `importFrom` deletes the stale artifact instead of publishing its `Ready`;
  a foreign pre-created `Ready` artifact (no ownerRef) is deleted, never
  mirrored; deleting the `VMImage` garbage-collects the `OSArtifact`; no
  delete-loop after bug-119.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — CRD schema + imagebuilder RBAC
- [ ] Config change only
- [ ] Documentation only

On first rollout, existing `OSArtifact`s lack the ownerReference and are
deleted and rebuilt once (a few GB of rebuild per `Url` image).

## [2026-07-31 20:10] - Document the SEC-004 checksum fix and its `sha2` dependency

**Author:** Erick Bourgeois

### Added
- **Dependency: `sha2 = "0.10"`**, pinned in `[workspace.dependencies]`, used by
  `banlieue-provider-libvirt`. From RustCrypto, the de-facto standard Rust
  hashing implementation and already present in the tree transitively. `0.10` is
  the current stable line (`0.11` is prerelease). This entry exists because the
  dependency rule in `CLAUDE.md` requires every new dep to be justified in the
  changelog, and this one landed without it.

### Changed
- `crates/banlieue-provider-libvirt/src/import.rs`: extracted the hashing read
  size to `HASH_CHUNK_BYTES`. `rules/rust-style.md` requires buffer sizes to be
  named constants; `vec![0u8; 1024 * 1024]` was inline.

### Why
**SEC-004 was recorded as an unaddressed residual and is now actually fixed**,
but nothing said so. The security-review entry of 2026-07-31 lists "SEC-004
(checksum never verified)" as a follow-up; the import subcommand now verifies
the artifact, and the changelog still claimed otherwise.

The chain is complete end to end: `VMImage.spec.sources[].checksum` →
`banlieue-imagebuilder` threads it to `status.rawDiskArtifact.checksum` → the
`VMImage` reconciler passes `--checksum` on the import Job → `verify_checksum`
hashes the artifact before any side effect.

Three properties make it worth trusting, all covered by tests:

- **Verification precedes every side effect.** It runs before the kube client is
  built, so a substituted artifact fails with no volume created and nothing to
  clean up.
- **It fails closed.** An unsupported algorithm is an error, not a skip — a
  declared-but-unverifiable checksum would defeat the entire point of the field.
- **It streams** in `HASH_CHUNK_BYTES` chunks, so a multi-gigabyte disk never
  sits in memory.

Covered by sha256 and sha512 happy paths against published test vectors, plus
mismatch, unsupported-algorithm, and malformed-format cases.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only — plus one constant extraction, no behaviour change

## [2026-07-31 19:40] - Scope the kind workflow to its own kubeconfig

**Author:** Erick Bourgeois

### Fixed
`make kind-e2e` failed outright with `error: context "kind-banlieue-dev" does
not exist`. The kind workflow was only half-scoped to its own kubeconfig: the
e2e *test* ran under `KUBECONFIG=$(KIND_KUBECONFIG)`, but all 24 other `kubectl`
invocations used a bare `kubectl --context kind-$(KIND_CLUSTER_NAME)`, which
resolves against whichever kubeconfig you have selected. When that file has no
such context — as it did here — every deploy step fails.

The mirror image of that bug is the serious one: `kind create cluster` **writes**
its context into the selected kubeconfig. Running the local e2e while pointed at
a real cluster's config would have modified that file as a side effect.

### Changed
- `Makefile`: added
  `KIND_KUBECTL = kubectl --kubeconfig $(KIND_KUBECONFIG) --context kind-$(KIND_CLUSTER_NAME)`
  and routed all 24 call sites through it.
- `kind-create` now runs `kind create` under `KUBECONFIG=$(KIND_KUBECONFIG)` and
  always refreshes that file before anything reads it, so a cluster created by
  an earlier run under a different kubeconfig is still reachable.

### Verification
- `make kind-e2e`: **7/7 passing**, including the libvirt workload-shape test.
- `kubectl config get-contexts` afterwards contains no `kind-banlieue-dev` — the
  workflow no longer touches the selected kubeconfig at all.
- The ADR-0015 SSA regression re-run against the CRDs **as deployed to the
  cluster** (not just as generated): `perProvider` reads `["vc-1", "kvm-1"]` and
  the deployed schema reports `list-type: map` on both lists.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

Developer workflow only; no shipped artefact changes.

## [2026-07-31 17:25] - Break the three High attack chains from the security review

**Author:** Erick Bourgeois

### Security
Implements the three chain-breakers from `security-review-2026-07-31.md`
(out-of-repo, per the planning-docs policy):

- **CHAIN-002 — stripped cluster-wide `secrets`/`configmaps` from the shared
  provider ClusterRoles** (`deploy/provider-vsphere/rbac/clusterrole.yaml`,
  `deploy/provider-libvirt/rbac/clusterrole.yaml`). Every provider credential
  / CA-bundle read is by name in the Provider's namespace and was already
  covered by the operator's `resourceNames`-scoped per-instance Role
  (ADR-0003); the cluster-wide rules were redundant privilege that defeated
  that design. `banlieue bootstrap` embeds the same files, so bootstrap
  installs pick the strip up automatically.
- **CHAIN-001 — two new ValidatingAdmissionPolicies** in `deploy/admission/`:
  `provider-connection` (absolute-URL endpoints, `https://` for
  vsphere/proxmox, no userinfo/fragment, and `insecureSkipTLSVerify: true`
  gated behind the opt-in annotation `banlieue.io/allow-insecure-tls: "true"`)
  and `provider-credentialsref-authorization` (CEL `authorizer`: the
  principal creating/updating a Provider must itself be able to `get` the
  credentialsRef Secret — the hop endpoint checks alone cannot close).
- **CHAIN-003 — new ValidatingAdmissionPolicy `vmimage-import-source`**:
  every `spec.sources[].importFrom` must pin an `@sha256:` digest and
  reference a registry in the new `banlieue-vmimage-allowed-registries`
  parameter ConfigMap (binding fails closed when it is missing).

### Changed
- **Standalone/static provider installs are namespace-scoped.** With no
  cluster-wide Secret access left, `banlieue bootstrap provider <backend>`
  now ships a namespaced Role+RoleBinding (secrets/configmaps `get` in the
  install namespace) and passes `--namespace` so the watch matches;
  `deploy/provider-vsphere/` gained `rbac/role.yaml` and the same
  `--namespace banlieue-system` scoping. Providers and their credentials
  must live in the install namespace on those paths.
- `examples/07-vmimage-kairos-url-source.yaml` and the imagebuilder guide
  pin the kairos image by digest (resolved from quay.io today:
  `sha256:e4860078…92a7`).

### Added
- Three admission policies under `deploy/admission/` (see its README for the
  full matrix); guide updates (`vsphere-provider`, `provider-lifecycle`,
  provider-vsphere README); CALM model updated and revalidated.

### Verification
- `cargo test -p banlieue-operator` — 109 passed (incl. new tests for the
  namespaced Role, binding, and `--namespace` args); clippy + fmt clean;
  `make calm-validate` clean.
- Against a fresh kind cluster (K8s 1.31): all six policies apply; Provider
  rejections verified for `http://`, userinfo, non-https vsphere scheme, and
  unannotated `insecureSkipTLSVerify` (allow with annotation); VMImage
  rejections for mutable tag, non-allowlisted registry, and malformed digest
  (allow for digest-pinned quay.io); all shipped examples still apply.
- CHAIN-001 replayed with an impersonated `team-delegate` user holding
  `create providers` but Secret `get` on only one named Secret: referencing
  the unreadable Secret is **denied**, referencing their own is allowed.
- `kubectl apply` of the stripped ClusterRoles confirms no secrets/configmaps
  rules remain.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — RBAC + optional admission policies
- [ ] Config change only
- [ ] Documentation only

The admission policies are optional hardening (apply `deploy/admission/`), but
the ClusterRole strip lands with the next deploy: any install still relying on
the shared ClusterRole for credential reads must move to the per-instance
(operator) or namespaced (standalone) Role. Residual from the review, not
addressed here: SEC-004 (checksum never verified) and SEC-005 (OSArtifact
ownerRef / generation binding) — follow-ups to ADR-0010.

## [2026-07-31 18:55] - Finish the conditions merge-key sweep (ADR-0015 follow-up)

**Author:** Erick Bourgeois

### Changed
- `x-kubernetes-list-type: map` keyed on `[type]` on every remaining
  `status.conditions` list: `Provider`, `ProviderClass`, `VirtualMachine`,
  `VSphereMachine`, `VSphereCluster`. `VMImage` already had it from ADR-0015.
- Regenerated `deploy/crds/` (5 files, 3 lines each) and the API reference.

### Why
These are single-writer today, so the contention ADR-0015 fixed was latent here
rather than live — but the annotation is the Kubernetes standard for condition
lists, and leaving it off is how the next component to write into one of these
rediscovers the same bug. `Provider` is the pointed case: it already carries a
comment explaining that `conditions` is atomic, and the operator was given a
disjoint field (`workload`) to route around it (ADR-0012). That workaround stays
correct and is now belt-and-braces rather than load-bearing.

### Verification
`x-kubernetes-list-map-keys` must name **required** fields or the apiserver
rejects the schema outright, which is not visible from the Rust types. Checked
both ways: `type` is required in every generated condition schema, and all eight
CRDs were applied to a kind cluster and accepted. The ADR-0015 SSA regression
test still passes against the full regenerated CRD set.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — CRD schema change
- [ ] Config change only
- [ ] Documentation only

Additive schema metadata; no controller behaviour changes.

## [2026-07-31 18:20] - ADR-0015: fix VMImage.status field-manager contention

**Author:** Erick Bourgeois

### Fixed
**Two providers reconciling one `VMImage` silently erased each other's status.**
`status.perProvider` and `status.conditions` carried no `x-kubernetes-list-type`,
so server-side apply treated them as **atomic**: one manager owns the whole
array, and `force()` hands it over wholesale. Each provider applies the full
list containing only its own rows, so the last writer won and the rest vanished.
Reproduced on a real apiserver before the fix:

```
perProvider rows  : ["kvm-1"]                                  # vSphere's row: gone
conditions        : [("Ready","False","libvirt importing")]    # vSphere's: gone
rawDiskArtifact   : Some(Ready)                                # survived
```

Not a corner case: `examples/04-vmimage-ubuntu.yaml` ships vsphere + proxmox +
libvirt sources, so it is the documented configuration.

### Added
- `docs/adr/0015-vmimage-status-merge-strategy.md`.
- `crates/banlieue-controller/src/reconciler/vmimage.rs` — aggregate-readiness
  reconciler and its `VMImage` watch. Pure aggregation, no backend calls.
- `crates/banlieue-provider-libvirt/tests/e2e_vmimage_ssa.rs` — the reproducer,
  now a regression test covering all four field managers.

### Changed
- `banlieue-api`: `status.perProvider` → `x-kubernetes-list-type: map` keyed on
  `[providerName, providerNamespace]`; `status.conditions` → keyed on `[type]`.
- **Providers no longer write `VMImage.status.conditions`.** A provider sees
  only its own rows, so any aggregate it computed answered a different question.
  Both providers' `aggregate_ready` moved to the controller — which also
  retired vSphere's `leak()` helper, needed only to force per-row `String`
  reasons into `&'static str`.
- The aggregate reason is chosen by provider identity, not list position: a
  merge-keyed list has no ordering guarantee, so "the first blocking provider"
  is not a stable concept and picking by position would flap the condition.

### Why
ADR-0010's split was right; the schema did not implement it. The same hazard was
already known for `Provider` — `provider.rs` documents it and works around it by
giving the operator a disjoint field (ADR-0012) — but that workaround cannot
apply where providers genuinely share one list.

Fixing the merge alone would have been insufficient. Both providers would still
have written `conditions[type=Ready]` from partial data, trading a visible
flip-flop for a subtler wrong answer. Ownership had to move too.

Blast radius was small because `banlieue-controller` reads only
`perProvider[].ready` and `.resolvedRef` for scheduling — nothing machine-
readable consumed `conditions`; it backs the `kubectl get vmimage` READY column.

### Verification
Re-ran the reproducer on kind against the regenerated CRD: `perProvider` reads
`["vc-1", "kvm-1"]`, a provider's later write updates its own row in place
without duplicating it, and the controller's condition survives untouched.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — CRD schema change plus a new controller watch
- [ ] Config change only
- [ ] Documentation only

### Follow-ups
- Apply `x-kubernetes-list-type: map` to every other `conditions` list in the
  API. Single-writer today, so latent rather than live; kept out of this change
  to keep the CRD diff reviewable.

## [2026-07-31 14:10] - Delete-and-recreate semantics, cluster-scoped name collision, ProviderClass status, and e2e coverage for all of it

**Author:** Erick Bourgeois

### Fixed
- **Cluster-scoped objects collided across namespaces.** The ClusterRoleBinding
  is cluster-scoped but was named `banlieue-provider-<class>-<provider>`, with
  no namespace. Two Providers sharing a name and class in different namespaces
  therefore landed on **one** object and fought over its subject — each
  server-side-applying its own namespace, last writer wins, and the loser
  silently lost its permissions. Reachable in any multi-tenant install, which is
  exactly what per-instance topology exists to serve. Cluster-scoped names are
  now namespace-qualified via `naming::cluster_scoped_name`; namespaced objects
  keep the shorter name, since their namespace already disambiguates them.
- **The operator had no delete-and-recreate semantics.** ADR-0007 ships the
  `providerClassRef` immutability policy as *optional* hardening and states the
  controller must not depend on it, "falling back to the controller's
  delete-and-recreate semantics" — which did not exist. On any cluster that
  never applied `deploy/admission/`, editing `providerClassRef` changed the
  derived name, so a second workload appeared while the first kept running: two
  provider pods for one backend, both holding credentials. The stale
  ClusterRoleBinding was worse — unowned, so GC could not reclaim it, and a
  name-based cleanup computed from the *current* class could never find it
  again, leaking permanently. `prune_orphans` now selects by label (pinned to
  provider name **and** namespace) across all namespaces, and `cleanup` deletes
  by the same selector rather than a recomputed name.

### Added
- **`ProviderClass` status reconciler.** ADR-0012 specified `status.providers`
  and conditions; the CRD shipped the fields and a `Providers` print column with
  nothing populating them, so the column was permanently blank. The `Ready`
  condition additionally reports whether the shared per-backend ClusterRole
  exists — surfacing bug-110's failure mode in `kubectl get providerclasses`
  before any Provider is created, instead of as 403s in a pod log afterwards.
  Reasons are a closed set of identifiers; the message names the exact missing
  ClusterRole.
- `clusterroles: get/list/watch` on the operator ClusterRole, **read-only**,
  with a test asserting it can never create or modify one.
- e2e coverage: class-change pruning, the update/roll path, `ProviderClass`
  paused, and the libvirt backend end to end.
- `make kind-verify-dry-run` — pipes `bootstrap --dry-run` through
  `kubectl apply --dry-run=server`, so ADR-0013's GitOps output is validated
  against real schema and admission (`--dry-run=client` would not catch a
  malformed manifest).
- `make kind-verify-escape-hatch` — asserts `bootstrap provider <backend>`
  installs a workload that is **unowned**, since the operator must neither adopt
  nor garbage-collect it.

### Why
Checking ADR-0007 before acting changed the fix. The obvious move — have
bootstrap install `deploy/admission/` — would have contradicted an Accepted ADR
that deliberately keeps those policies optional and out of the controller's
dependency graph. The defect was the missing fallback, not the missing install.

### Impact
- [x] Requires cluster rollout (operator ClusterRole gained a rule; existing
      ClusterRoleBindings are renamed — the new prune removes the old ones)
- [ ] Breaking change
- [x] Tests + CI + documentation

## [2026-07-31 16:40] - Shared caBundle resolver in the SDK; mutation testing closes two real gaps

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-sdk/src/ca_bundle.rs` — the CA-bundle resolver both
  TLS-speaking backends were carrying their own copy of. `plan()` classifies a
  `CABundleSource` with no I/O; `resolve()` reads the ConfigMap/Secret.
- `Error::Invalid(&'static str)` on the SDK error, carrying the validator's own
  message (which already names the field).
- `pem_from_secret_value()` — see below.

### Changed
- `banlieue-provider-vsphere/src/reconciler/ca_bundle.rs`: 139 → 43 lines.
- `banlieue-provider-libvirt/src/credentials.rs`: 115 → 76 lines.
- The six classification tests moved from vSphere to the SDK (with the exact-
  selector assertions preserved); the vSphere copy is deleted rather than kept
  as a test of a re-export.

### Why
The two copies had already drifted — different error types, `String` vs
`Vec<u8>`, and one enforcing "exactly one source" through a different variant —
while resolving the identical spec field. What is genuinely per-backend is
whether a bundle is *required*: vSphere falls back to system trust roots,
libvirt refuses because libvirtd's certificate comes from a private CA in every
realistic deployment. So `resolve()` returns `Option` and says nothing about
whether `None` is acceptable; each provider decides in three lines.

### Fixed
Mutation testing over the new modules (20 mutations, targeted at the pure
decision functions) initially left 3 survivors. One was an equivalent mutant —
`Option::or` against itself on a `Copy` option is a no-op, a bug in the harness,
not a test gap. The other two were real:

- **`target_pools()` would return a pool named `""`** from a blank or
  trailing-comma `pools` attribute. The filter dropping empty segments was
  pinned by nothing, and an import Job targeting `""` fails on the libvirt host
  rather than at the reconciler where the cause is legible.
- **The `caBundle` UTF-8 check was pinned by nothing at all.** It sat inside
  `read_secret_key`, reachable only through a kube API call, so deleting it
  outright kept the suite green — and a binary DER value would have reached the
  TLS stack to fail with a far less obvious message. Extracted as
  `pem_from_secret_value()` and tested both ways.

Re-run after both fixes: **20/20 killed, 0 survivors.**

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

Internal refactor plus test coverage; no behaviour change outside the two fixes
above, neither of which alters a healthy path.

## [2026-07-31 15:40] - `banlieue provider libvirt import`, and the RBAC + CLI chain it needs to actually run

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-libvirt/src/import.rs` — the data path of ADR-0010 /
  ADR-0011. Reads the `Provider`, resolves its TLS material, connects, creates a
  raw volume sized to the artifact, and streams the disk in over libvirt's
  stream protocol. Pure decision functions (`volume_name`, `find_pool`, `plan`,
  `source_length`) with 13 tests; the rest is I/O.
- `deploy/provider-libvirt/rbac/clusterrole.yaml` — the libvirt provider had no
  shipped ClusterRole at all, so `banlieue bootstrap provider libvirt` bailed
  with "no embedded ClusterRole".
- `banlieue-operator`: `backend_additional_rules()`, seeding the libvirt
  `ProviderClass` with `batch/jobs: get,create,patch`.
- `POD_SERVICE_ACCOUNT` via the downward API on every provider Deployment.
- `examples/09-providerclass-libvirt.yaml`, `docs/src/guides/libvirt-provider.md`
  (wired into `mkdocs.yml` and the guides index).

### Fixed
- **The libvirt provider crash-looped under the operator.** `build_args()` emits
  `--provider-name` for *every* backend, but only the vSphere `Cli` declared it,
  so the container failed clap parsing at startup and never reconciled. Added
  the flag and `provider_watch_config()` to narrow the watch server-side, as
  vSphere does. Neither side's tests could see this: each tested its own half.
- **The import Job referenced a subcommand that did not exist.** Every test
  asserted on the manifest as JSON, so nothing ever fed those args to a parser.
  There is now a round-trip test that pulls argv out of the generated manifest
  and parses it with the real parser.
- `examples/02-provider-libvirt-edge.yaml` used `qemu+ssh://` and no `caBundle`
  — both rejected at reconcile time since ADR-0011.

### Changed
- `credentials::resolve` takes a `&Client` rather than a `&Context`, so the Job
  resolves the same material without constructing a reconcile context.
- `build_import_job` takes an `ImportJobInputs` struct; the Job now carries
  `--provider-namespace` and `serviceAccountName`.
- `deploy/operator/rbac/clusterrole.yaml` gains `batch/jobs` — RBAC refuses to
  let a grantor hand out what it does not hold, the same trap Secrets hit
  before. The operator never creates a Job itself.
- `COMPILED_BACKENDS` includes `libvirt`, so `bootstrap operator` seeds its class.
- `#[allow(clippy::large_enum_variant)]` on `Command` / `ProviderBackend`: clap's
  `Subcommand` derive needs each payload to impl `Args`, which `Box` does not,
  and the enum is built once per process from argv.

### Why
The `VMImage` reconciler was complete but inert — it created Jobs that would
fail at exec, under a ServiceAccount with no Job permissions, in a binary that
could not parse the flags the operator passes it. Three separate breaks, each
invisible to the tests on either side of it, all on the path between "apply a
libvirt `Provider`" and "an image lands in a storage pool".

Least privilege drove the details. The Job runs as the **controller's own**
ServiceAccount rather than a fresh one, so it inherits the `resourceNames`
narrowing the operator already applied and gains nothing extra — and it is
dropped when the Job's namespace differs from the controller's, since a pod
cannot reference a ServiceAccount outside its own namespace. `batch/jobs` is
`get,create,patch` only: reads are by deterministic name so no list/watch, and
`ttlSecondsAfterFinished` reaps finished Jobs so no delete. Only libvirt gets
the grant; creating a Job is the ability to run an arbitrary pod as that
provider's identity.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — new flags, new RBAC, rebuilt image
- [ ] Config change only
- [ ] Documentation only

### Follow-ups
- Untested against a real libvirt host end-to-end; only `FakeClient` so far.
- `credentials.rs` still duplicates the vSphere CA-bundle resolver; both belong
  in `banlieue-provider-sdk`.

## [2026-07-31 14:05] - VMImage reconciler for banlieue-provider-libvirt (ADR-0010 / ADR-0011)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-libvirt/src/reconciler/vmimage.rs` — the libvirt half
  of ADR-0010's pipeline. Gates on `VMImage.status.rawDiskArtifact.phase == Ready`,
  then creates one import Job per storage pool the Provider advertises in
  `status.failureDomains[].attributes.raw["pools"]`, and translates Job status
  into `status.perProvider[].zones[]`.
- `crates/banlieue-provider-libvirt/src/reconciler/vmimage_tests.rs` — 16 tests
  over the pure decision functions (`find_libvirt_source`, `gate_on_raw_disk`,
  `target_pools`, `import_job_name`, `build_import_job`, `zone_from_job`,
  `aggregate_ready`). No kube, no TLS, no libvirt host.
- `banlieue-libvirt`: `storage_pool_list_all_volumes` (+ `STORAGE_VOL_LIST_MAX`),
  needed to tell "already imported" from "needs importing".

### Changed
- `crates/banlieue-provider-libvirt/src/context.rs`: added `build_namespace` and
  `import_image`, so the Job's namespace and image are configuration, not
  constants baked into the reconciler.
- `crates/banlieue-provider-libvirt/src/app.rs`: `--build-namespace` /
  `--import-image` flags and a second `Controller` watching `VMImage`.

### Why
The Provider reconciler proved the host is reachable and its declared pools
exist; this closes the loop by getting an actual guest image onto it. Three
decisions are load-bearing:

- **Job names are deterministic** (`import-<image>-<provider>-<pool>`, truncated
  to 63 chars). A re-reconcile therefore *adopts* a running import rather than
  starting a second copy of a multi-gigabyte transfer.
- **`backoffLimit: 1`.** A partial volume upload is not resumable — the retry
  restarts the whole stream — so retrying indefinitely would hammer the host for
  no benefit. One retry, then report `ImportFailed` and let a human look.
- **The Job mounts the artifacts PVC `readOnly` and runs the `banlieue` binary
  itself**, not a third-party `virsh`/`qemu-img` image (ADR-0011). The data path
  stays inside banlieue's own supply chain, and the same `banlieue-libvirt` code
  is exercised in both the controller and the Job.

This reconciler writes **only** `status.perProvider[]`. `status.rawDiskArtifact`
belongs to `banlieue-imagebuilder`'s field manager (ADR-0010) and
`status.workload` to the operator's (ADR-0012); the disjoint-field SSA split is
what keeps three managers off each other's toes.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout — new flags and a new watch; needs a rebuilt image
- [ ] Config change only
- [ ] Documentation only

### Follow-ups
- `banlieue provider libvirt import` — the Job manifest references this
  subcommand; it is not implemented yet, so Jobs would fail at exec today.
- `credentials.rs` duplicates the vSphere CA-bundle resolver; both belong in
  `banlieue-provider-sdk`.

## [2026-07-31 11:20] - kind-based e2e for the operator contract (ADR-0014); scrub a real hostname from the repo

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-operator/tests/e2e_provider_lifecycle.rs` — e2e suite against
  a real API server. Asserts the operator's whole contract: workload creation,
  Deployment shape (`provider vsphere --provider-name …`, selector/template
  agreement), the `resourceNames`-scoped Role, controlling owner references on
  the namespaced four, *no* owner reference on the ClusterRoleBinding,
  `status.workload` publication, and that deletion removes all five objects —
  garbage collection for the owned ones, the finalizer for the cluster-scoped
  one. Plus a paused-Provider case. `#[ignore]`d, like `live_libvirtd.rs`.
- `make kind-e2e` / `kind-e2e-ci` / `kind-e2e-logs` / `kind-kubeconfig`. The CI
  variant dumps cluster state on failure and always tears the cluster down.
  `KIND_KUBECONFIG` is cluster-scoped and gitignored, so the suite never depends
  on — or mutates — whichever context you have selected.
- `.github/workflows/e2e.yaml` — `workflow_call` + `workflow_dispatch` + path-
  filtered PR/push. Installs tools and calls `make kind-e2e-ci`; no logic in
  YAML, per `rules/github-workflows.md`.
- `.claude/rules/no-real-infrastructure.md` — new rule (below).

### Changed
- `crates/banlieue-libvirt/tests/live_libvirtd.rs`: a real maintainer hostname →
  `bar.foo.io`. (Naming the scrubbed value here would republish it — the rule
  lists the changelog explicitly.)
- `crates/banlieue-api/src/common_tests.rs`: `1.1.1.1` / `8.8.8.8` →
  `192.0.2.53` / `198.51.100.53` (RFC 5737 documentation range).

### Why
**The e2e closes a category of bug unit tests cannot reach.** All 78 operator
unit tests assert on objects built in memory; none prove the apiserver accepts
them. A selector that does not match its pod template, a `resourceNames` rule
RBAC rejects, an `ownerReference` with a wrong `apiVersion`, an SSA patch
silently dropped because the CRD schema disagrees with the Rust type — every one
of those is green in `cargo test` and red on first contact with Kubernetes.

**The suite deliberately does not wait for the provider pod to become Ready.**
Its Provider points at `vcenter.invalid` (RFC 2606 — can never resolve), so the
pod stays NotReady and `readyReplicas` stays `0` by design. That is correct:
the operator's contract is *producing a correctly shaped workload*, and it never
talks to a backend (ADR-0012). Asserting on readiness would be asserting on the
vSphere provider and on CI's DNS, and would make the job permanently red. This
is called out in the test's module docs, the ADR, and the guide, because it is
the one thing a future contributor is likely to "fix" and break.

**Real infrastructure identifiers must never be committed.** A previous session
wrote a real hypervisor hostname into a tracked test file. This is a public
repo: publishing a hostname names a host, implies what runs on it, and leaks the
naming scheme for its neighbours — and a later commit removing it does not
un-publish it. The new rule mandates `bar.foo.io`-style placeholders and RFC
5737 IPs, with real values taken from the environment at runtime. The
`authors = [… erick@jeb.ca]` package metadata is the one documented exception:
that is an author identity, not a host.

### Fixed
- **`deploy/operator/rbac/clusterrole.yaml`: the operator could not add its own
  finalizer** (buglog bug-104). The role granted `update` on
  `providers/finalizers` on the assumption that covered it. It does not — those
  are two different permissions:
  - `<resource>/finalizers` is the admission-time check the apiserver runs
    before accepting a *dependent* whose `ownerReference` sets
    `blockOwnerDeletion: true`.
  - Writing `metadata.finalizers` on the object is an ordinary write to the
    **main** resource, needing `update`/`patch` on `providers`.

  `ensure_finalizer()` runs first in the reconcile, so every Provider reconcile
  403'd and retried every 5s forever, creating nothing. A controller that uses a
  finalizer *and* sets `blockOwnerDeletion` needs both rules. Caught by the
  first kind e2e run; guarded now by two unit tests that parse the embedded
  ClusterRole.
- **`bootstrap::add_role` silently swallowed a missing ClusterRole.** It now
  errors. Omitting it produced an install that reported success while binding a
  ServiceAccount to a ClusterRole that does not exist — a pod with no
  permissions, failing later with opaque 403s instead of at install time.
- **The shared provider ClusterRole granted `secrets`/`configmaps: list,watch`
  cluster-wide** (buglog bug-109). Because that role is attached with a
  *ClusterRoleBinding*, those verbs let any provider pod read every Secret in
  the cluster — silently defeating the `resourceNames` narrowing per-instance
  topology exists to provide (ADR-0003). It also blocked the operator from
  creating the binding at all, since RBAC refuses to let a grantor hand out
  permissions it lacks. Narrowed to `get`: every call site
  (`reconciler/{provider,vmimage,ca_bundle}.rs`) reads by name. Widening the
  operator to match would have propagated the over-grant instead of removing it.
- **`bootstrap operator` never installed the shared per-backend ClusterRole**
  (buglog bug-110). The operator *binds* it to every per-instance
  ServiceAccount but cannot create it — minting the permissions it hands out is
  precisely the escalation path ADR-0012 refuses. So a real `bootstrap operator`
  followed by applying a Provider produced a ClusterRoleBinding pointing at a
  nonexistent role, leaving the provider pod with no permissions while the
  install looked healthy. Bootstrap now ships one ClusterRole per compiled-in
  backend and hard-errors, naming the backend, if a manifest is missing.
  `make kind-deploy-operator` applies `deploy/provider-*/rbac/clusterrole.yaml`
  so the local flow matches.
- **The RBAC drift guard compared only apiGroups, not verbs**, so it passed
  while the provider role granted `secrets: list,watch` and the operator held
  only `get`. Rewritten to compare `(apiGroup, resource, verb)` triples across
  every backend that ships a ClusterRole, plus a guard banning blanket Secret
  enumeration. Both were verified by reintroducing the bug and confirming they
  fail — after bug-105, an unverified guard is not a guard.
- **The paused-Provider e2e passed vacuously** (buglog bug-105). It asserted
  only that no workload appeared, which a dead operator satisfies equally well —
  and it went green on the very run where the operator was 403ing on everything.
  It now unpauses the Provider afterwards and requires the workload to appear,
  which is what makes the absence check mean anything.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout (operator ClusterRole gained a rule)
- [ ] Config change only
- [x] Tests + CI + documentation

`cargo test --all` → 582 passed, 0 failed, 2 ignored. `actionlint` clean.
The e2e suite has now been **executed against a live kind cluster**, where it
found bug-104 on its first run.

## [2026-07-31 09:40] - banlieue becomes a true operator: ProviderClass CRD, `banlieue operator`, `banlieue bootstrap` (ADR-0003/0012/0013)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-operator` — new library crate, the provider lifecycle
  controller. Applying a `Provider` CR now creates that backend's controller:
  one Deployment, ServiceAccount, Role, RoleBinding and ClusterRoleBinding
  **per Provider** (ADR-0003). Modules:
  - `naming` — derived names and labels, capped at 63 chars with a stable
    FNV-1a suffix. `DefaultHasher` is deliberately not used: it is not stable
    across Rust releases, and a name that changed with the compiler would
    orphan the previous Deployment on every upgrade.
  - `workload` — pure builders for the five objects. Shared with `bootstrap`,
    so a CLI-installed provider is identically shaped to an operator-spawned one.
  - `reconciler::provider` — reconcile + finalizer-driven cleanup.
  - `bootstrap` — the `banlieue bootstrap` install CLI.
- `ProviderClass` CRD (`banlieue.io/v1alpha1`, cluster-scoped) — install
  metadata for a backend class: backend, image, workload namespace, replicas,
  resources, node placement, logging, additional RBAC rules, paused.
  `Provider.spec.providerClassRef` finally resolves to something.
- `Provider.status.workload` — `{deploymentName, namespace, readyReplicas,
  observedGeneration}`, written **only** by `banlieue.io/operator`.
- `banlieue operator` and `banlieue bootstrap {operator,provider,imagebuilder}`
  subcommands; `COMPILED_BACKENDS` in the binary crate so a slim build cannot
  offer to install a backend it does not contain.
- `--provider-name` on `banlieue provider vsphere`, narrowing its Provider
  watch **server-side** via a field selector.
- `deploy/operator/` — ServiceAccount, ClusterRole, ClusterRoleBinding,
  ConfigMap, Deployment, Service.

### Changed
- ADR-0003 promoted Proposed → **Accepted**, with the decision changed from the
  drafted hybrid (`deploymentStrategy: Shared | PerInstance`) to **per-instance
  only**. The hybrid is recorded as considered-and-rejected: every driver it
  named is solved by `PerInstance` and none by `Shared`, `Shared` is the status
  quo rather than a capability, and the knob is a backward-compatible addition
  later.
- `docs/architecture/calm/architecture.json` — `service-banlieue-operator`
  node, `data-asset-providerclass-cr`, `rel-operator-kube-api`, and a
  `flow-provision-provider-workload` flow. `make calm-validate` → 0 errors,
  0 warnings; diagrams regenerated.
- `banlieue_api::crdgen_support` is no longer behind the `crdgen` feature, so
  `bootstrap` can build CRDs from the Rust types at runtime. Only `serde_yaml`
  stays gated.

### Why
Two decisions are worth the reading time.

**Disjoint status ownership.** The operator writes only `status.workload` and
never `status.conditions`. `conditions` is a plain list with no
`x-kubernetes-list-type: map` marker (schemars/kube-derive do not emit one), so
two field managers writing into it contend over the whole array instead of
merging per entry. A disjoint field keeps server-side apply conflict-free
without `force` papering over it.

**The operator holds what it grants.** Kubernetes forbids creating or binding a
Role carrying permissions the creator lacks unless it holds `escalate`/`bind` —
which would effectively make the operator cluster-admin. Instead
`deploy/operator/rbac/clusterrole.yaml` contains the union of what it hands to
provider Roles, so the escalation surface is bounded by, and auditable from,
one file. A unit test asserts the operator ClusterRole covers every apiGroup
the provider ClusterRole uses, so the two cannot drift into runtime rejections.

Credential access stays narrow: each generated Role grants `get` on exactly the
Secret its Provider names, via `resourceNames`. The generated Role never asks
for `list`/`watch` on Secrets, because Kubernetes ignores `resourceNames` for
those verbs and the grant would silently widen to every Secret in the namespace.

### Fixed
- `crates/banlieue-libvirt/tests/live_libvirtd.rs`: `&PathBuf` → `&Path`
  (`clippy::ptr_arg`). Pre-existing; surfaced by `--all-targets`.

### Impact
- [x] Requires cluster rollout (new CRD, new Deployment, new ClusterRole)
- [ ] Breaking change
- [ ] Config change only
- [ ] Documentation only

`cargo fmt` + `cargo clippy --all-targets --all-features -D warnings` +
`cargo test --all` → 572 tests, 0 failures.

## [2026-07-31 00:45] - CALM model for the libvirt provider; XDR codec (ADR-0011)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-libvirt` — new crate, the first-party libvirt RPC client.
  One dependency (`thiserror`, already in the workspace); no `kube` and no
  banlieue API types, so the future import Job can link it without the
  controller's dependency graph. Two modules so far, 39 unit tests:
  - `xdr` — **XDR (RFC 4506) codec**: `Encoder` / `Decoder` for int, unsigned
    int, hyper, bool, fixed/variable opaque, and string.
  - `rpc` — **message framing**: `virNetMessageHeader` (the 24-byte,
    six-field header), `virNetMessageType` / `virNetMessageStatus`, the
    length-prefix codec, and the procedure numbers banlieue uses.
  - `transport` — **session + TLS**: `Session<S>` (generic over
    `AsyncRead + AsyncWrite`, so the whole protocol is testable over
    `tokio::io::duplex` with a scripted peer — no socket, no libvirtd),
    serial allocation and reply matching, `virNetMessageError` decoding, and
    `connect_tls` for mutual-TLS connections.

  **Zero new third-party crates.** `tokio-rustls` and `rustls-pki-types` were
  already in `Cargo.lock` transitively via the vSphere provider's
  reqwest/hyper-rustls stack, so promoting them to direct dependencies
  compiles no additional code — verified: the only entry `Cargo.lock` gained
  is `banlieue-libvirt` itself. `rustls-pemfile` was deliberately avoided
  because `rustls-pki-types` already provides PEM parsing under its default
  `alloc` feature.

  Every constant, enum value, and field ordering was transcribed from
  libvirt's own `src/rpc/virnetprotocol.x` and `src/remote/remote_protocol.x`,
  fetched from upstream — not from documentation or memory. These values *are*
  the contract; a subtly wrong one round-trips against itself forever and only
  fails on contact with a real libvirtd.

### Changed
- `docs/architecture/calm/architecture.json`:
  - `service-provider-libvirt` promoted from "planned, Phase 1D" to the
    ADR-0011 design (in-process control plane, Job-based data plane).
  - `network-libvirt-backend` documents TLS-only on 16514 and records that
    plaintext 16509 is explicitly unsupported.
  - `rel-provider-libvirt-backend` gained a `libvirt-mutual-tls` control
    (NIST SP 800-53 SC-8 / SC-23 / IA-5).
  - New flow `flow-import-vmimage-libvirt` with a
    `control-plane-data-plane-split` control.
  - ADR-0011 added to `adrs[]`.
  `make calm-validate` → 0 errors, 0 warnings; diagrams regenerated.

### Why
Tests assert **exact encoded bytes**, not just round-trips: a codec that is
self-consistently wrong round-trips perfectly and still desynchronises against
a real libvirtd. The decoder also enforces RFC 4506's "padding MUST be zero"
rather than skipping padding, because non-zero padding is usually the first
observable symptom of a desynchronised stream.

### Fixed
- Integer overflow in `Decoder::read_opaque_fixed` (see buglog bug-101):
  `len + pad` was unchecked, but `len` is public API input *and* is fed a
  wire-read `u32` by `read_opaque_var`. `padding_for(usize::MAX) == 1`, so the
  sum overflows — panicking in debug and, worse, **wrapping to a small value in
  release**, which passes the bounds check and then corrupts the cursor. Now
  `checked_add`.

  Found by **mutation testing, not by the suite**: deleting the redundant
  bounds check in `read_opaque_var` left all 21 tests green, which prompted
  asking why the inner check was load-bearing. The mutation run also confirmed
  the padding and byte-order tests *do* fail when their logic is broken.

### Verification
Both modules were mutation-tested rather than trusted for being green. The
framing layer caught 5 of 6 deliberate defects — length prefix excluding
itself, swapped header field order, a dropped `VIR_NET_MESSAGE_MAX` bound,
unknown message types silently treated as `Call`, and a wrong `REMOTE_PROGRAM`
magic. The survivor (writing `proc` via `write_u32(v as u32)`) is an
**equivalent mutant**, not a test gap: two's complement makes those bytes
identical for every `i32`, verified across the full range, so no test could
distinguish them and there is nothing to fix.

The transport layer caught **6 of 6**: an unchecked reply serial, treating an
`Error` status as success, accepting any message type as a reply, `read`
instead of `read_exact` on the length prefix, serials starting at 0, and
skipping length-prefix validation entirely.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] New crate, not yet wired into any binary — no runtime behaviour changes

### Not yet done
Nothing in this crate has spoken to a real libvirtd. RPC framing, the
procedure definitions from `remote_protocol.x`, and the TLS transport are
still to come, and ADR-0011 records an integration test against a live host as
non-optional — procedure numbers and struct layouts are where desync bugs will
actually live.

## [2026-07-31 02:15] - Fix imagebuilder build pods rejected by banlieue-system's restricted PodSecurity (ADR-0010 amendment)

**Author:** Erick Bourgeois

### Changed
- `deploy/imagebuilder/namespace.yaml` (new): `banlieue-imagebuild` namespace,
  `pod-security.kubernetes.io/enforce: privileged`.
- `deploy/imagebuilder/configmap.yaml`: `BANLIEUE_BUILD_NAMESPACE` default
  `banlieue-system` → `banlieue-imagebuild`.
- `crates/banlieue-imagebuilder/src/app.rs`: `DEFAULT_BUILD_NAMESPACE`
  constant updated to match; doc comment on `--build-namespace` now states
  the constraint explicitly.
- `Makefile` (`imagebuilder-run-local`): updated hint text.
- `docs/adr/0010-vmimage-build-pipeline-imagebuilder.md`: amendment note —
  default build namespace changed, with rationale.

### Why
First real (non-smoke-test) `VMImage` build failed:

```
pods "kairos-ubuntu-2404-build-lx4b7" is forbidden: violates PodSecurity
"restricted:latest": privileged (container "build-cloud-image" must not set
securityContext.privileged=true), allowPrivilegeEscalation != false, ...
```

kairos-operator's `OSArtifact` build pods set `securityContext.privileged:
true` unconditionally — assembling a raw disk image needs loop-device
mount/chroot access, which is inherent to what the build does, not something
banlieue's manifests control (kairos-operator is a third-party CRD/operator,
ADR-0010). `banlieue-system`'s `restricted` Pod Security level (correct for
banlieue's own controller/provider pods) was also, incidentally, the default
`--build-namespace` since ADR-0010 — so every real build was rejected before
kairos-operator's controller could even schedule the pod. The earlier smoke
test (`scripts/bootstrap-kairos-operator.sh`) never caught this because it
runs its test `OSArtifact` in kairos-operator's own `operator-system`
namespace, not `banlieue-system`.

PSA is enforced per-namespace with no per-pod exception, so the fix is
isolating build workloads into their own, separately-labeled namespace —
never loosening `banlieue-system` itself, which stays `restricted` for the
controller/provider pods that actually need hardening.

### Impact
- [ ] Breaking change
- [x] Requires applying `deploy/imagebuilder/namespace.yaml` (new namespace)
      before the next `make imagebuilder-run-local` / imagebuilder deployment
- [ ] Config change only
- [ ] Documentation only

## [2026-07-30 23:15] - Fix two bugs in bootstrap-libvirt-tls.sh found while provisioning

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-libvirt-tls.sh` (`configure_libvirtd`): re-armed socket
  activation in the correct order — stop `libvirtd.service`, stop all
  `libvirtd*.socket`, disable the TCP socket, enable the TLS socket, start the
  wanted sockets, then start the service. Both `systemctl start` calls are now
  checked and fail loudly with the exact `journalctl` command.
- `scripts/bootstrap-libvirt-tls.sh` (`set_conf`): patterns no longer match
  commented lines.

### Why
1. **`systemctl enable --now libvirtd-tls.socket` failed** with *"Socket
   service libvirtd.service already active, refusing."* — systemd will not
   attach a new socket unit to an already-running service, and libvirtd
   typically has long uptime. Under `set -e` the script aborted there, before
   the restart, the verification, and the Secret generation. Compounding it,
   `systemctl disable --now libvirtd-tcp.socket` does **not** close port 16509
   on a running daemon: libvirtd keeps serving the fd it already inherited
   (confirmed still LISTENing on pid 1411 fd=6 after the "disable"). So the
   naive sequence could report success while plaintext stayed open — which is
   why `verify()` asserts 16509 is *absent* rather than trusting the disable.
2. **Duplicate config entries.** `libvirtd.conf` documents every option as a
   commented example far above the real settings; the `#?` in `set_conf`'s
   patterns matched those and *uncommented* them, leaving two active copies of
   `listen_tls`/`listen_tcp`. Benign only because both got the same value.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Dev/test tooling only (no banlieue runtime code touched)

### Validated
libvirt TLS provisioned and verified end to end on the reference host: 16514
listening, 16509 closed, `qemu+tls://` round-trip OK, guests survived the
daemon restart, and — from inside the cluster — the server certificate's SANs
include the libvirt bridge address the provider will actually dial.

## [2026-07-30 22:30] - ADR-0011: own the libvirt client (zero new deps); provision libvirt TLS

**Author:** Erick Bourgeois

### Added
- `docs/adr/0011-libvirt-provider-own-client.md` — decides that
  `banlieue-provider-libvirt` speaks libvirt's RPC protocol via a small
  first-party crate (`banlieue-libvirt`) rather than any third-party client.
- `scripts/bootstrap-libvirt-tls.sh` — provisions CA + server + client certs,
  switches libvirtd from plaintext TCP to mutual TLS, and emits a Kubernetes
  Secret manifest for the client credentials. Subcommands:
  `all|ca|server|client|configure|verify|secret|status`.

### Why
No usable libvirt client exists for this project: `virt`/`virt-sys` are FFI to
the C library (a native dep in a distroless image, and ~11 months stale),
while `libvirt` (2015) and `libvirt-rpc` (2018) are abandoned. Measured
against the protocol's actual size — a 24-byte header plus XDR, with stream
packets carrying raw unencoded bytes — a first-party client is ~750 lines and
needs **no new dependencies**: XDR/RPC/procedures are written here, and the
transport reuses `rustls` + `tokio`, already pinned for the vSphere BYOC work.

The reference libvirt host was found listening on plaintext TCP 16509 with
`auth_tcp = "sasl"` / `mech_list: digest-md5` — a mechanism RFC 6331 declared
obsolete, over an unencrypted channel carrying every disk-image byte. Moving
to TLS is both the security fix and a simplification: with libvirt's default
`auth_tls = "none"` the x509 client certificate *is* the credential, so no
SASL exchange and no MD5 dependency are needed at all. The provider therefore
supports **TLS only**, with no plaintext fallback.

An earlier draft of this ADR (run `virsh` inside Jobs for everything) was
rejected before implementation: it made a CLI's stdout a wire format and
required a Job per `Provider` probe. Its reasoning about the *data* path
survives — bulk transfer still runs in a Job, never in the reconcile loop.

### Impact
- [ ] Breaking change
- [x] Requires running `bootstrap-libvirt-tls.sh` on the libvirt host before
      the provider can connect (no plaintext fallback, by design)
- [ ] Config change only
- [x] Design/tooling only — no banlieue runtime code written yet

## [2026-07-30 10:30] - Purge stale known_hosts pins before k0sctl apply

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-k0s-cluster.sh`: `apply_k0sctl` now runs `ssh-keygen -R`
  for each host address in the generated k0sctl config before invoking
  `k0sctl apply` (new `purge_known_hosts` helper). Addresses are read back out
  of `$K0SCTL_CONFIG`, so a standalone `apply` works without a preceding
  `config` step in the same run.

### Why
A rebuild aborted with:

```
ssh: handshake failed: host key mismatch: knownhosts: key mismatch
  - <vm-ip>:22: retrying aborted
```

k0sctl brings its own SSH stack (the `rig` library) and reads
`~/.ssh/known_hosts` directly — it does **not** honour the
`StrictHostKeyChecking=no` / `UserKnownHostsFile=/dev/null` that the script
already sets for its own `ssh_run`, and k0sctl v0.32.1 exposes no flag to relax
it. Because libvirt recycles its DHCP pool across destroy/create cycles, a
freshly built VM lands on an address a *previous* VM's host key is still pinned
to; k0sctl then aborts the entire run after a single attempt. Confirmed on the
hypervisor: the reused address was pinned in root's `known_hosts` (hashed)
from an earlier VM.

Purging only the addresses about to be used keeps host-key verification on
everywhere else. `ssh-keygen -R` is used rather than a grep/sed purge because
the entries are hashed and would not match a literal search.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rebuild
- [ ] Config change only
- [x] Dev/test tooling only (no banlieue runtime code touched)

## [2026-07-30 10:00] - Add bootstrap-kairos-operator.sh; stop k0s tainting every node

**Author:** Erick Bourgeois

### Added
- `scripts/bootstrap-kairos-operator.sh` — installs a default StorageClass
  (Rancher local-path) + kairos-operator onto an existing cluster, with an
  optional OSArtifact smoke test. Subcommands:
  `all|storage|operator|smoke|status|destroy`. **Deliberately never sets
  `KUBECONFIG`** — the caller exports it, and the script echoes the target
  context/server before touching anything. Notable behaviour:
  - Refuses to run when every node carries a `NoSchedule` taint (nothing
    schedulable could start), printing the exact remediation.
  - Only claims the default-StorageClass role if the cluster has no default
    already, then **verifies** it rather than assuming the patch landed.
  - Deletes any prior smoke-test OSArtifact **and its PVC** before recreating.
  - On failure, dumps OSArtifact status, PVC events, pod events, and the logs
    of both the `pull-image-baseimage` init container and `build-cloud-image`.

### Changed
- `scripts/bootstrap-k0s-cluster.sh`: emit `noTaints: true` on
  `controller+worker` hosts (new `NO_TAINTS` override, default `true`).
- `Makefile`: new `K0S_NO_TAINTS` knob, forwarded through `K0S_ENV` and
  `K0S_REMOTE_ENV`.

### Why
The rebuilt cluster came up with `node-role.kubernetes.io/control-plane:NoSchedule`
on all three nodes — k0s's default for `controller+worker`. With *every* node a
controller, nothing without a matching toleration can schedule anywhere:
local-path-provisioner sat `Pending` for 12h, so no PVC could ever be
provisioned. `noTaints: true` is the documented k0sctl fix.

That cascaded into a second, unrecoverable failure. kairos-operator creates each
OSArtifact's PVC with no `storageClassName`, so it binds to whatever the
cluster's default StorageClass is *at creation time*. The smoke-test PVC was
created before any default existed, leaving `storageClassName` empty — and that
field is immutable, so the claim can never bind no matter what is installed
afterwards. Hence the script enforcing the ordering (storage ready → default
verified → only then create an OSArtifact) instead of leaving it to a doc.

### Impact
- [ ] Breaking change
- [x] Existing clusters need `kubectl taint nodes --all node-role.kubernetes.io/control-plane-`
      (no rebuild required); the script change only affects newly-built clusters
- [ ] Config change only
- [x] Dev/test tooling only (no banlieue runtime code touched)

## [2026-07-29 16:00] - Fix k0s-destroy leaving VMs defined (UEFI nvram + unmanaged storage)

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-k0s-cluster.sh` (`destroy_all`): `virsh undefine` now passes
  `--nvram --managed-save` and **no longer passes `--remove-all-storage``**.
- `scripts/bootstrap-k0s-cluster.sh` (`destroy_all`): stopped swallowing
  `undefine` errors (was `>/dev/null 2>&1 || true`); failures are now logged and
  the command exits non-zero. Added a post-teardown verification pass that
  re-lists domains matching `^$VM_PREFIX-[0-9]+$` and fails if any survive.

### Why
`make k0s-remote-destroy` reported success while leaving all three VMs defined
(visible in Cockpit, `shut off`). `virsh destroy` only force-powers-off; the
`undefine` that actually removes the domain was failing for two stacked reasons,
both silenced by `|| true`:

1. **UEFI NVRAM.** `create_vm` uses `--boot uefi`, so each domain carries an
   `<nvram>` varstore (`/var/lib/libvirt/qemu/nvram/k0s-0N_VARS.fd`). libvirt
   refuses to undefine such a domain unless told what to do with it — verified
   on the hypervisor: libvirt 11.3.0's `virsh undefine --help` still offers the
   mutually-exclusive `--nvram` / `--keep-nvram` pair.
2. **Unmanaged storage.** `vol-list default` shows libvirt manages
   `/var/lib/libvirt/images/k0s-bootstrap` as a single *directory* volume; the
   per-VM `k0s-0N.qcow2` / `-seed.iso` files inside it are not pool-managed
   volumes, so `--remove-all-storage` cannot resolve them and aborts the whole
   undefine. It was redundant anyway — `destroy_all` already `rm -f`s those
   files directly on the next lines.

This mattered beyond a messy teardown: `create_vm` short-circuits on any
already-defined domain (`dominfo` succeeds → just `start` it), so a subsequent
`k0s-remote-all` would have silently rebuilt "on top of" the old VMs rather than
fresh ones — masking whether the `externalAddress` fix had actually taken.

### Impact
- [ ] Breaking change
- [x] Requires re-running `make k0s-remote-destroy` (the previously-"destroyed"
      VMs are still defined and must be removed before a clean rebuild)
- [ ] Config change only
- [x] Dev/test tooling only (no banlieue runtime code touched)

## [2026-07-29 15:30] - Fix "No agent available" on the k0s dev cluster: pin spec.api.externalAddress

**Author:** Erick Bourgeois

### Changed
- `scripts/bootstrap-k0s-cluster.sh`: now emits `spec.api.externalAddress` in
  the generated k0sctl config, defaulting to the **first controller's internal
  DHCP address** (new `API_EXTERNAL_ADDRESS` override). The address is also
  force-added to `spec.api.sans` so a hand-set VIP/LB/DNS value is covered
  without also editing `EXTRA_SANS`.
- `scripts/bootstrap-k0s-cluster.sh`: `fetch_kubeconfig` now rewrites **only
  the `server:` line**, to the VPN overlay IP of the *same node*
  `externalAddress` points at (new `KUBECONFIG_SERVER` override; persisted
  between invocations via `$WORKDIR/kubeconfig-server`). Replaces a blanket
  search/replace of every internal IP that had no coordination with where the
  konnectivity agents actually point.
- `Makefile`: new `K0S_API_EXTERNAL_ADDRESS` / `K0S_KUBECONFIG_SERVER` knobs,
  forwarded through both `K0S_ENV` (local) and `K0S_REMOTE_ENV`
  (`k0s-remote-*` over SSH to the hypervisor).
- `Makefile`: new `imagebuilder-run-local` target, mirroring
  `provider-vsphere-run-local`, to run `banlieue imagebuilder` against the
  current kube-context without building/pushing an image.

### Why
Every `kubectl logs` / `exec` / `port-forward` against the dev k0s cluster
failed with `No agent available`, which surfaced while smoke-testing
kairos-operator (the `OSArtifact` builder pod's init container failed and its
logs were unreadable). Root cause: k0s's konnectivity agents all dial exactly
**one** address — `spec.api.externalAddress` when set, otherwise whichever
controller's own address won the race to write the DaemonSet
(k0sproject/k0s#600, #5503). With `externalAddress` unset on a 3-controller
cluster, all three agents pinned to one controller's internal DHCP address
while the kubeconfig pointed at a different controller's VPN overlay IP — so
the API server being queried had zero agents registered. `kubectl get` kept
working (it never leaves etcd), which masked the problem.

The fix aligns the two: in-cluster traffic (including konnectivity) uses the
internal libvirt DHCP network via `externalAddress`, while the VPN overlay
stays a pure external kubectl entry point via SANs + the kubeconfig `server:`
address, pointed at the same node. Konnectivity's port therefore never needs
exposing on the overlay network.

### Impact
- [ ] Breaking change
- [x] Requires cluster rebuild (`make k0s-remote-destroy` + `k0s-remote-all`) —
      the generated k0sctl config changes, and `externalAddress` is baked in at
      cluster-init time
- [ ] Config change only
- [x] Dev/test tooling only (no banlieue runtime code touched)

## [2026-07-29 13:30] - Fix anyhow RUSTSEC finding; VEX-exception the unfixable quick-xml one

**Author:** Erick Bourgeois

### Changed
- `Cargo.lock`: `anyhow` `1.0.102 → 1.0.104`, closing RUSTSEC-2026-0190
  (`Error::downcast_mut()` unsoundness). Plain `cargo update -p anyhow`.
- `deny.toml`: added a documented `[advisories] ignore` entry for
  RUSTSEC-2026-0194 / RUSTSEC-2026-0195 (quick-xml 0.39.4, quadratic-time and
  unbounded-memory DoS parsing crafted XML; fixed upstream in quick-xml
  0.41.0). Pulled in transitively via `vim_rs 0.5.0`, which pins
  `quick-xml = "^0.39"`.
- `.cargo/audit.toml` (new): the same RUSTSEC-2026-0194 / RUSTSEC-2026-0195
  ignore, but for `cargo-audit` — the "Security Vulnerability Scan" CI job
  (`firestoned/github-actions/rust/security-scan`) runs `cargo audit`
  directly and does not read `deny.toml`, so it was still failing after the
  cargo-deny fix. Both files must stay in sync for this advisory going
  forward.

### Why
A `[patch.crates-io]` override to quick-xml 0.41.0 was tried first but does
not work: Cargo's patch mechanism still enforces semver compatibility against
every dependent's declared range, and 0.41.0 doesn't satisfy vim_rs's `^0.39`
(each 0.x minor is semver-breaking), so Cargo silently drops the patch
(confirmed via `[[patch.unused]]` in `Cargo.lock`). The newest available
vim_rs release, 0.6.0 (also checked against its `main` branch), still only
bumps its own pin to `quick-xml = "0.40"` — short of the 0.41.0 fix — so there
is no upstream release to adopt yet.

The finding is accepted as a documented exception rather than left failing CI
because banlieue-provider-vsphere only uses vim_rs to parse SOAP/XML
responses from the single vCenter endpoint the operator explicitly
configured as that provider's backend — never attacker-controlled or
arbitrary network input, which is the threat model both advisories describe
(e.g. a public-facing RPKI/RRDP relying party).

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

## [2026-07-28 17:00] - VMImage build pipeline: banlieue-imagebuilder + kairos-operator (ADR-0010)

**Author:** Erick Bourgeois

### Changed
- **New crate `crates/banlieue-imagebuilder`** (library, per ADR-0004): watches
  `VMImage` for `spec.sources[].kind == Url`, server-side-applies a
  kairos-operator `OSArtifact` (`build.kairos.io/v1alpha2`, modeled as a
  `DynamicObject` — banlieue does not own or generate this CRD), and mirrors
  its build status into a new `VMImage.status.rawDiskArtifact` field. Wired
  into the unified `banlieue` binary as `banlieue imagebuilder`, gated behind
  a default-on `imagebuilder` Cargo feature.
- `crates/banlieue-api/src/banlieue/vmimage.rs`: added
  `VMImageStatus.rawDiskArtifact` (`RawDiskArtifactStatus` /
  `RawDiskArtifactPhase`), and `ImagePerProviderStatus.zones[]`
  (`ZoneImageStatus`) for per-failure-domain import progress. Both additive;
  no schema break for existing `Template`-source `VMImage`s.
- `crates/banlieue-provider-sdk/src/ssa.rs`: added
  `FIELD_MANAGER_IMAGEBUILDER` (`banlieue.io/imagebuilder`) — writes
  exclusively to `rawDiskArtifact`, never `perProvider[]`, so the two field
  managers never contend on the same `VMImage.status`.
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`:
  `find_vsphere_source` now also matches `Url`-kind sources (previously
  `Template` only). New `compute_url_source_status` gates readiness on
  `rawDiskArtifact.phase == Ready`, then reports one `ZoneImageStatus` row per
  `Provider.status.failureDomains[]`. Per-zone conversion (raw → VMDK) and the
  `vim_rs` upload/import are intentionally **not implemented** in this change
  — tracked as an ADR-0010 follow-up; zones report
  `PerZoneImportNotImplemented` rather than falsely claiming readiness.
- `deploy/imagebuilder/`: RBAC (ServiceAccount/ClusterRole/ClusterRoleBinding
  — no Secret/ConfigMap access, only `vmimages`, `vmimages/status`, and
  kairos-operator's `osartifacts`), ConfigMap, Deployment, Service.
- `docs/adr/0010-vmimage-build-pipeline-imagebuilder.md`: full ADR.
- `docs/architecture/calm/architecture.json`: added
  `banlieue-imagebuilder` / `kairos-operator` / `OSArtifact` nodes and
  relationships, a new `flow-build-vmimage-from-oci` flow; also backfilled
  the previously-missing ADR-0008/0009 entries in the `adrs[]` list.
- `docs/src/guides/kairos-operator-setup.md`,
  `docs/src/guides/using-banlieue-imagebuilder.md`: new guides, verified
  against kairos-operator's real docs (kustomize install, not Helm — an
  earlier disconnected prototype of this idea had guessed at nonexistent
  Helm chart names) and the regenerated CRD's real field names.
- `examples/07-vmimage-kairos-url-source.yaml`: new example.
- `deploy/crds/banlieue.io_vmimages.yaml`, `docs/src/reference/api.md`:
  regenerated via `make crds`.

### Why
The original design goal for banlieue's image handling — nightly Kairos
builds tested and distributed as per-zone vSphere templates — needs an
OCI-image → raw-disk build step that has no vSphere-specific content and
must be reusable when Proxmox/libvirt providers land later. `VMImage`'s
schema already anticipated this (`ImageSourceKind::Url` / `importFrom`
existed but were unimplemented); this change delivers the provider-agnostic
build half via a new crate, while per-zone import stays each provider's own
concern per the CRD-only contract.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] New optional component (`banlieue-imagebuilder` + `deploy/imagebuilder/`) — additive, opt-in via a `Url`-kind `VMImage` source
- [ ] Config change only
- [x] Documentation

## [2026-06-03 15:30] - cargo-deny: allow CDLA-Permissive-2.0; skip vim_rs phf dup

**Author:** Erick Bourgeois

### Changed
- `deny.toml`: allow `CDLA-Permissive-2.0` (the Mozilla CA root data bundle
  `webpki-root-certs`, pulled by reqwest 0.13's `rustls-platform-verifier`); add
  `phf@0.11.3` / `phf_shared@0.11.3` to the duplicate-version skip list (vim_rs
  0.5's `vim_macros` uses phf 0.11 while `vim_rs` uses 0.13 — internal, can't
  unify). `cargo deny check` → all four checks ok.

### Why
Fallout of the reqwest 0.12 → 0.13 bump (ADR-0009): the new rustls platform
verifier drags in a permissive *data*-licensed CA bundle the allowlist hadn't
seen, and vim_rs 0.5 carries two phf majors internally. Both are benign; this
makes the supply-chain gate green again.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build / CI policy only

## [2026-06-03 14:00] - Adopt vim_rs 0.5 (rustls/ring); delete the vendoring pipeline (ADR-0009)

**Author:** Erick Bourgeois

### Changed
- `Cargo.toml`: `vim_rs = "0.5"` (plain crates.io, `default-features = false` →
  BYOC mode); **deleted the `[patch.crates-io]` block** and the `=0.4.4` pin.
  `reqwest` 0.12 → **0.13** (`rustls-no-provider` + charset + http2). Added direct
  `rustls = "0.23"` (ring) dep.
- `client/vim.rs`: BYOC call site is now `ClientBuilder::new(endpoint, http)`
  (2-arg; vim_rs 0.5 has no `.http_client()` in `default-client`-off mode). Added
  `install_default_crypto_provider()` (ring), called at the top of the provider's
  `run()` before any TLS — reqwest 0.13 `rustls-no-provider` panics
  ("No provider set") otherwise. ring is the single crypto provider, shared with
  kube; no aws-lc-rs, no OpenSSL (verified: `openssl-sys`/`aws-lc-rs` absent).

### Removed
- The entire vendoring pipeline: `make vendor-vim-rs` (+ all `VIM_RS_*` vars,
  stamp, and prerequisites on build/test/lint/crds/etc.),
  `.github/actions/vendor-vim-rs` and its 9 CI invocations (build + codeql),
  `patches/` (vim_rs.patch + README), `third_party/vim_rs/`, the `.gitignore`
  entry. `make build` now compiles `vim_rs 0.5.0` from crates.io with no vendor
  step.

### Why
The upstream author shipped the fix as vim_rs 0.5.0 (reqwest 0.13/rustls + a
first-class BYOC mode, noclue/vim_rs#37). That resolves ADR-0008's build-side
caveat, so the fork-shaped vendor/patch/stamp/CI apparatus is deleted in favour
of a plain dependency. See ADR-0009.

### Notes
- The libssl Docker/Cross scaffolding was already removed earlier; nothing left
  to revert (Dockerfiles are single-stage rustls/no-OpenSSL).

### Impact
- [x] Breaking change (none for users; build/dev workflow: no more `make
      vendor-vim-rs`, plain crates.io)
- [x] Requires cluster rollout (rebuild images on reqwest 0.13/ring)
- [ ] Documentation only

## [2026-06-01 12:00] - Implement BYOC + value-or-source caBundle (ADR-0008)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/common.rs` — `CABundleSource` (inline / configMapRef /
  secretRef, exactly-one via `validate()`) + `KeySelector` (name + optional key,
  `key_or` default) + `DEFAULT_CA_BUNDLE_KEY = "ca.crt"`. Full unit tests.
- `crates/banlieue-provider-vsphere/src/reconciler/ca_bundle.rs` —
  `resolve_ca_bundle` (pure `plan()` classifier + namespace-local ConfigMap/Secret
  reads, failing closed on >1 source / missing object / missing key / non-UTF-8).
- BYOC in `client/vim.rs` — `build_http_client` builds the `reqwest::Client`
  (root certs via `from_pem_bundle`, **rejects a zero-cert bundle** so a bad PEM
  fails closed instead of silently using system roots) and injects it via
  `ClientBuilder::http_client`. `root_certs_from_pem` / `build_http_client` unit
  tested with a real self-signed fixture.
- `deploy/admission/provider-cabundle-source.yaml` — VAP enforcing exactly-one
  caBundle source at admission (defense-in-depth atop the controller check).
- `reqwest` as a direct dep of provider-vsphere (workspace-pinned, rustls
  features matching vim_rs so the graph stays OpenSSL-free).

### Changed
- `ProviderConnection.ca_bundle`: `Option<String>` → `Option<CABundleSource>`
  (breaking, pre-GA — caBundle is now an object). CRDs + API reference regenerated.
- `VSphereClientFactory::build` takes a resolved `ca_bundle_pem: Option<&str>`;
  both reconcile call sites (provider, vmimage) resolve then pass it.
- `deploy/provider-vsphere/rbac/clusterrole.yaml` — added `configmaps`
  get/list/watch (read-only) for `configMapRef`.
- Examples + guides (`examples/01-*`, vsphere guide, provider-vsphere README)
  show the value-or-source caBundle.

### Why
Implements ADR-0008: banlieue owns the HTTP transport (BYOC) so it owns TLS
trust, and `caBundle` finally works — from inline PEM, a ConfigMap, or a Secret.

### Impact
- [x] Breaking change (`caBundle` string → object; pre-GA v1alpha1)
- [x] Requires cluster rollout (provider RBAC adds configmaps; new VAP optional)
- [ ] Documentation only

## [2026-06-01 10:30] - ADR-0008: BYOC for the vSphere HTTP client (design only)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0008-byoc-vsphere-http-client.md` (Status: Proposed) — decide that
  the vSphere provider builds and injects its own `reqwest::Client` via
  `vim_rs` `ClientBuilder::http_client(...)`, owning TLS/transport policy and
  finally honouring `ProviderConnection.caBundle`.
- `docs/architecture/calm/architecture.json` — `tls-trust-byoc` control on the
  `rel-provider-vsphere-backend` relationship (NIST SC-8 / SC-23 / IA-5).
  `make calm-validate` passes (0/0); `make calm-diagrams` re-rendered.

### Why
`vim_rs`'s built-in builder exposes only a blunt `insecure` toggle and ignores a
CA bundle entirely, so `connection.caBundle` is currently dead config and the
only way to reach a private-CA vCenter is to disable verification — a
least-privilege violation. `http_client()` is a supported, fork-free seam that
lets banlieue own TLS trust.

### Notes / corrections
- BYOC is **independent of** the TLS-backend question. Verified empirically that
  unpatched `vim_rs` 0.4.4 pulls OpenSSL (`openssl v0.10.80` + native-tls);
  Cargo's additive features mean BYOC alone does **not** evict it. Removing
  OpenSSL still depends on the rustls patch / upstream noclue/vim_rs#37, so the
  `vendor-vim-rs` pipeline stays. (Supersedes an earlier mistaken claim that
  0.4.4 already used rustls — that read the patched working tree.)
- noclue/vim_rs#37 is **strictly** TLS-backend selection — no behavioural fixes —
  so it does not block or alter BYOC.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Documentation / design only (no code yet — TDD of `VimClientFactory::build`
      is the next step once the ADR is accepted)

## [2026-05-31 20:45] - Cache vendor-vim-rs behind a stamp file (stop rebuilds)

**Author:** Erick Bourgeois

### Context
`vendor-vim-rs` was `.PHONY`, so it ran on every `make`. Its `git reset --hard`
rewrote the vendored source mtimes each time, which cargo's fingerprint read as
"changed" → it recompiled `vim_rs` (~5 min) on every back-to-back target, e.g.
`make kind-deploy-controller` then `make kind-deploy-provider-vsphere`.

### Changed
- `Makefile`: the real vendoring recipe now hangs off a file target,
  `$(VIM_RS_DIR)/.vendor-stamp` (`VIM_RS_STAMP`), with prerequisites `Makefile`
  (carries `VIM_RS_REF`) and the patch (via `$(wildcard $(VIM_RS_PATCH))`).
  `vendor-vim-rs` is now a thin `.PHONY` alias depending on the stamp. When the
  stamp is newer than its inputs, make skips the recipe entirely — no
  `git reset`, no mtime churn, no cargo rebuild. Editing the patch or bumping
  `VIM_RS_REF` re-triggers; the resolved REF is written into the stamp.

### Why
The vendoring must stay a prerequisite of every cargo target (correctness on a
fresh clone), but doing the work only matters when the pin or patch changes.
A stamp file is the standard make idiom for "run this side-effecting step at
most once until its inputs change."

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build change only (faster incremental builds; `make clean` + re-vendor or
      deleting `third_party/vim_rs/.vendor-stamp` forces a re-vendor)
- [ ] Documentation only

## [2026-05-31 19:50] - CI: complete vendor-vim-rs coverage + pending-upstream NOTEs (noclue/vim_rs#37)

**Author:** Erick Bourgeois

### Changed
- `.github/workflows/build.yaml` — added the `vendor-vim-rs` composite to the `auto-vex-reachability` job (the one cargo job missing it; it failed with `failed to read .../third_party/vim_rs/vim_rs/Cargo.toml`).
- `.github/workflows/codeql.yaml` — added a `vendor-vim-rs` step gated on `matrix.language == 'rust'` before CodeQL init: the Rust extractor resolves the workspace via `cargo metadata`, which needs the `[patch.crates-io]` checkout present.
- Added a **TEMPORARY / pending-upstream NOTE referencing `https://github.com/noclue/vim_rs/issues/37`** to every place the vendoring surfaces: the composite `action.yml` (name + header), each `Vendor vim_rs` step in `build.yaml`, the new CodeQL step, the `make docs` step in `docs.yaml` (vendors transitively), and the canonical source comments in `Cargo.toml` (`[patch.crates-io]`), `Makefile` (`VIM_RS_*` vars), and `patches/README.md` (Retiring the patch).

### Why
Every cargo invocation in the workspace needs the gitignored `third_party/vim_rs` checkout materialised first (the rustls `[patch]`). Audited **all** workflows: `format`/`clippy`/`build`/`test`/`auto-vex-presence` already vendored; `auto-vex-reachability` and `codeql` (rust) did not; `docs` vendors transitively via `make docs` → `api-docs`. `calm`/`sast`/`scorecard` run no cargo. The NOTEs make the temporary nature discoverable so the whole apparatus can be retired once #37 ships.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] CI only
- [ ] Documentation only

Verified: all workflow YAML + `action.yml` parse; every raw-`cargo` job in `build.yaml` has `vendor=1`; CodeQL has the rust-gated vendor step; `#37` NOTE present in action.yml, build.yaml, codeql.yaml, docs.yaml, Cargo.toml, Makefile, patches/README.md.

## [2026-05-31 19:15] - vim_rs → rustls patch; revert OpenSSL scaffolding

**Author:** Erick Bourgeois

### Added
- `patches/vim_rs.patch` — one-hunk patch on the vendored `vim_rs` checkout's `vim_rs/Cargo.toml`: `reqwest = { version = "0.12" }` → `{ version = "0.12", default-features = false, features = ["rustls-tls-native-roots", "charset", "http2"] }`. Generated from the pinned commit so `make vendor-vim-rs` applies it cleanly (and reverse-detects it as already-applied). No source changes — `vim_rs`'s client uses only backend-agnostic reqwest APIs (`danger_accept_invalid_certs`/`_hostnames` are `__tls`-gated, not native-tls). **`rustls-tls-native-roots`** (not `rustls-tls`) uses the OS trust store (`rustls-native-certs`, already in the tree via kube) instead of bundling `webpki-roots` — which keeps the lockfile identical to bindy/5-spot and avoids `webpki-roots`'s `CDLA-Permissive-2.0` license tripping `cargo deny check licenses`.

### Changed
- This makes the whole workspace **OpenSSL-free**: `Cargo.lock` now shows `openssl-sys: 0, native-tls: 0, rustls: 1, webpki-roots: 0, rustls-native-certs: 1` — matching the bindy / 5-spot reference repos (rustls + ring, native trust roots). `cargo metadata` reports no "patch not used" warning; `cargo deny check licenses` → ok.
- **Reverted the interim OpenSSL scaffolding** (no longer needed): removed the `libssl` build stage + `LD_LIBRARY_PATH` from `Dockerfile` and `Dockerfile.chainguard` (back to plain single-stage COPY); deleted `Cross.toml`; reverted `Makefile` `kind-load` from `cross` back to the host gcc cross-toolchain (rustls/ring cross-compiles with just the cross-gcc + linker/CC env, like bindy); updated the provider crate's TLS comment and the developer doc.

### Why
`vim_rs` was the lone OpenSSL puller (via reqwest's default native-tls). Patching its reqwest to rustls — via the vendored-checkout + `[patch.crates-io]` mechanism, no fork — removes OpenSSL entirely, so cross-compiling from macOS and the distroless/Chainguard images "just work" with no libssl at build or runtime.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build / packaging change (run `make vendor-vim-rs` before bare `cargo`; rebuild images)
- [ ] Documentation only

Verified: patch applies idempotently via `make vendor-vim-rs`; `cargo tree -i openssl-sys` empty; lockfile `openssl-sys: 0 / rustls: 1`; `cargo check -p banlieue` exit 0 (full workspace compiles with rustls).

## [2026-05-31 18:45] - Makefile: RUST_LOG override on kind-deploy-{controller,provider-vsphere}

**Author:** Erick Bourgeois

### Changed
- `Makefile` — `kind-deploy-controller` and `kind-deploy-provider-vsphere` now `kubectl set env … RUST_LOG=$(RUST_LOG[_VSPHERE])` on the Deployment after applying, so the in-cluster log level is overridable the same way as `run-local`: `RUST_LOG=debug,kube=debug make kind-deploy-controller`. The container `env` overrides the ConfigMap's `RUST_LOG` for that key; default stays `info,kube=warn` (+`vim_rs=warn` for the provider).

### Why
Parity with `run-local` / `provider-vsphere-run-local` — debug an in-cluster deploy without hand-editing the ConfigMap.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Developer tooling only

Verified by `make -n` for default and overridden `RUST_LOG`.

## [2026-05-31 18:30] - Build vim_rs from a vendored checkout + local patch (no fork)

**Author:** Erick Bourgeois

### Context
We want to carry a local change to `noclue/vim_rs` that is being submitted
upstream, without owning a fork. Approach: build against a vendored checkout
pinned to an upstream commit, with a checked-in patch applied at build time and
wired in via `[patch.crates-io]`. The mechanism is fork-free and self-retiring —
once the change ships upstream, the build detects it and skips re-applying.

### Added
- `Makefile`: `vendor-vim-rs` target — clones `noclue/vim_rs` into
  `third_party/vim_rs` (gitignored), `reset --hard` to `VIM_RS_REF`, then applies
  `patches/vim_rs.patch` idempotently: applies if clean, **skips if already
  present** (merged upstream / reverse-applies), hard-errors if stale. Wired as a
  prerequisite of every cargo-invoking target — `build`, `build-debug`, `test`,
  `test-lib`, `lint`, `crds`, `api-docs`, `provider-vsphere-run-local`, `sbom`,
  `vex-auto-presence`, `vex-auto-reachability`, `_build-linux`, `kind-load`.
- `Cargo.toml`: `[patch.crates-io]` redirecting `vim_rs` to the crate's
  subdirectory in the vendored checkout — `third_party/vim_rs/vim_rs` (upstream
  is a multi-crate repo with no root manifest; the crate lives under `vim_rs/`).
  Dep pinned to **`=0.4.4`** exact (was `0.4`).
- `.github/actions/vendor-vim-rs/action.yml`: composite action that runs
  `make vendor-vim-rs`; dropped into every cargo-using job in `build.yaml`
  (format, clippy, build, test, security, cargo-deny, auto-vex-presence) right
  after checkout. `docs.yaml` vendors transitively via `make docs` → `api-docs`.
- `patches/README.md`: create / refresh / retire workflow for the patch.
- `.gitignore`: ignore the vendored `third_party/vim_rs/` checkout.

### Why
Avoids maintaining a full fork: the pin lives in the `Makefile`, the diff lives
in `patches/vim_rs.patch`, and the upstream-merged check means the build keeps
working across bumps. The pin is a **commit, not a tag**: the version we need
(0.4.4 — first to carry the `vcsim_compat` feature the provider uses) was
published to crates.io and lives on `main` but was never git-tagged; the newest
tag (v0.4.3) predates that feature. The `=0.4.4` exact pin is required so cargo's
resolver lands on that version and the patch actually takes effect — a range
(`0.4`) would let it pick crates.io 0.4.4 and silently ignore the path patch.
Because the patch source is gitignored and absent after `actions/checkout`, every
cargo step (local and CI) must vendor first or fail to read the manifest.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build / packaging change (run `make vendor-vim-rs` after clone; `make`
      targets and CI do it automatically — a bare `cargo build` needs the
      checkout first)
- [ ] Documentation only

> Pairs with the OpenSSL build entry below: if the upstream patch switches
> `vim_rs` off native-tls (rustls), the libssl runtime/image gymnastics there
> can later be reverted.

## [2026-05-31 18:00] - Build: system OpenSSL (dynamic) — libssl in images + `cross` for local

**Author:** Erick Bourgeois

### Context
`vim_rs`'s reqwest uses native-tls → OpenSSL on Linux (kube is rustls; vim_rs is the lone OpenSSL source). Chosen approach: use the **system OpenSSL, dynamically linked** (no vendoring, no vim_rs fork). That requires libssl at build time and `libssl.so.3` in the runtime images.

### Changed
- `crates/banlieue-provider-vsphere/Cargo.toml` — removed the interim `openssl = { vendored }` dependency; back to plain dynamic system OpenSSL.
- `Dockerfile` (distroless) and `Dockerfile.chainguard` — added a `libssl` build stage that stages `libssl.so.3` / `libcrypto.so.3` (Debian `libssl3` / Wolfi `openssl`) and copies them into the runtime image under `/usr/local/lib` with `LD_LIBRARY_PATH` (neither base ships OpenSSL, and there's no ldconfig). Fixes the `libssl.so.3: cannot open shared object file` runtime error. Built per-target-platform under buildx so the `.so` arch matches the binary.
- `Makefile` — `kind-load` now builds the Linux binary with **`cross`** (a Linux container that has `libssl-dev`, per the new `Cross.toml`) instead of the host gcc cross-toolchain, which can't link a Linux libssl from macOS. Native Linux still builds directly.
- `docs/src/developer/local-development.md` — documents `cargo install cross` for local image builds and why.

### Added
- `Cross.toml` — installs target-arch `libssl-dev` in `cross`'s build containers for both Linux targets.

### Why
CI builds the binary natively on Linux (libssl-dev present) — the release pipeline was never blocked. The two real gaps were the **runtime images** (no libssl) and **local macOS image builds** (cross-linking OpenSSL). Both are now closed without a vim_rs fork or vendoring.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Build / packaging change (rebuild images to pick up libssl; `cargo install cross` for local image builds)
- [ ] Documentation only

> `LIBSSL_IMAGE` (debian:trixie-slim / wolfi-base) is currently a floating tag — pin by digest (Dependabot, docker ecosystem) to match `BASE_IMAGE`.

## [2026-05-31 17:10] - ADR-0007 + CALM control for admission policies

**Author:** Erick Bourgeois

### Added
- `docs/adr/0007-admission-policies.md` — records the decision to enforce CRD invariants (immutability) via `ValidatingAdmissionPolicy` rather than a validating webhook or CRD-embedded CEL. Context, decision, consequences, and alternatives (webhook → extra service + cert lifecycle; CRD `x-kubernetes-validations` → most code-first but couples to schemagen and can't roll out report-only; controller-side → too late).
- `docs/architecture/calm/architecture.json` — new top-level control `admission-policy-validation` (K8s VAP reference + NIST SSDF PW.5/RV.1) and ADR-0007 added to the `adrs` list.

### Changed
- `deploy/admission/README.md` — links to ADR-0007.

### Why
ADD requires architecturally significant changes (a new security/deploy artifact) to be recorded as an ADR and modeled in CALM. This backfills both for `deploy/admission/`, added in the previous entry at the maintainer's direction.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation / architecture record only

Verified: `make calm-validate` → 0 errors / 0 warnings; `architecture.json` parses.

## [2026-05-31 17:00] - CI: deploy docs to GitHub Pages on merge to main (interim)

**Author:** Erick Bourgeois

### Changed
- `.github/workflows/docs.yaml` — the GitHub Pages deploy (Setup Pages, Upload Pages artifact, and the `deploy` job) now fires on a direct push to `main` in addition to the existing release path. Condition broadened to `(push && refs/heads/main) || (workflow_run release success)`. Header/comment blocks updated to reflect the interim "publish on every merge to main" policy.

### Why
Requested: deploy the documentation on merge to main "for now." Previously docs only published on a successful Build run for a release. PR builds still validate-only; the release-gated path is retained.

### Notes
- Path-filtered: a merge to main only redeploys when docs-affecting paths change (`docs/**`, `crates/**/*.rs`, the docs/calm workflows) — identical docs aren't needlessly republished.
- Requires the repo's Pages source set to "GitHub Actions" (already used by the release deploy). Top-level token already grants `pages: write` + `id-token: write`.
- Treated as a non-architectural CI-policy tweak (broadens an existing deploy trigger; no new topology), so no ADR/CALM per ADD.

### Impact
- [x] CI / docs deployment only
- [ ] Breaking change
- [ ] Requires cluster rollout

### Verification
`actionlint .github/workflows/docs.yaml` clean.

## [2026-05-31 16:30] - Docs: restructure into Guides / Developer + admission policies

**Author:** Erick Bourgeois

### Added
- `deploy/admission/` — ValidatingAdmissionPolicies (GA, K8s 1.30+, CEL, no webhook): `virtualmachine-immutability.yaml` (immutable `classRef`/`imageRef`), `provider-immutability.yaml` (immutable `providerClassRef.name`), each with a `Deny` binding, plus a README.
- `docs/src/guides/` — new top-level **Guides** tab (production, `ghcr.io/firestoned/banlieue:v0.1.0`): `index.md`, `core-controller.md` (CRDs → namespace → RBAC → configmap → deployment → ValidatingAdmissionPolicies → verify), `vsphere-provider.md` (ground-up: provider install → Secret → Provider → VMClass → VMImage → VirtualMachine → verify, every `kubectl apply`).
- `docs/src/developer/` — new top-level **Developer** tab: `index.md` + `local-development.md`, migrating the old build-from-source quickstart and the vSphere `vcsim`/`run-local`/`GOVC_*` content out of the user-facing pages.

### Changed
- `docs/mkdocs.yml` — **Why banlieue?** moved under **Home** (per request); new **Guides** and **Developer** tabs added to the nav.
- Cross-links updated in `concepts/providers.md`, `index.md`, `overview.md`, `reasoning/non-goals.md` to point at the new Guides/Developer pages. All quick-start/install paths now use `v0.1.0`.

### Removed
- `docs/src/getting-started/` (`quickstart.md`, `vsphere-provider.md`) — split into the production Guides (ghcr.io) and the Developer local-dev page.

### Why
The getting-started docs conflated production install with local development and predated the single-binary/v0.1.0 model. Splitting into release-oriented **Guides** and **Developer** local-dev, with admission hardening documented and shipped, gives a clean install path for the upcoming `v0.1.0` release.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation + optional deploy artifacts (admission policies)

Verified: `mkdocs build --strict` exits 0 (no broken links/nav); admission YAML parses and is valid against the GA `admissionregistration.k8s.io/v1` schema.

> Note (ADD): `deploy/admission/` is a new security/deploy artifact; per ADD it could be formalized with an ADR (e.g. `0007-admission-policies`). Authored here at the maintainer's direction as part of the controller guide — happy to add the ADR + CALM control if desired.

## [2026-05-31 16:00] - Auto-VEX: port presence + reachability tools from 5-spot

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-vex/` — new workspace crate (added to members) porting 5-spot's auto-VEX tooling verbatim (adjusted only for `banlieue_vex` / `pkg:oci/banlieue` / copyright):
  - `auto_vex_presence` module + `auto-vex-presence` bin — emit `not_affected + component_not_present` for Grype findings whose affected purl is absent from every image SBOM.
  - `auto_vex_reachability` module + `auto-vex-reachability` bin — emit `not_affected + vulnerable_code_not_in_execute_path` for Grype CVEs whose curated affected symbols (`.vex/.affected-functions.json`) are all absent from the release binary's `nm -D --undefined-only` table.
  - Full ported unit suites (41 tests): pure logic, deterministic sorted output, dedup, dotfile/metadata skipping, malformed-input errors.
- `.github/workflows/build.yaml` — new `grype-triage` (raw scan → JSON), `auto-vex-presence`, `auto-vex-reachability` jobs; `build-vex` now merges curated `.vex/*.json` **plus** both auto-derived documents before Cosign-attesting and feeding `grype --vex`.
- `Makefile` — `vex-auto-presence` / `vex-auto-reachability` local mirrors + `GRYPE_JSON`/`AFFECTED_FUNCTIONS`/`RELEASE_BINARY`/`SBOM_FILES` vars.
- `docs/adr/0006-*.md` — flipped the "Staged" section to "implemented" (the binaries are built/run in 5-spot, per maintainer); CALM `release-artifact-provenance` control de-staged.

### Fixed
- `crates/banlieue-api/src/{crddoc.rs,bin/crddoc.rs}`, `crates/banlieue-provider-vsphere/src/reconciler/{provider,vmimage}.rs` — collapsed nested `if let { if … }` into let-chains. These `clippy::collapsible_if` lints surfaced after the workspace MSRV bump to Rust 1.88 (let-chains stabilized) and were failing `clippy -D warnings --all-features`; pre-existing, unrelated to auto-vex, fixed so the workspace gate is green.

### Why
The maintainer corrected the prior turn's staging decision — the auto-vex binaries exist and run in `~/dev/5-spot` — so banlieue ports them rather than deferring. The full pipeline now derives VEX automatically (presence + reachability), merges with curated statements, attests, and scans.

### Impact
- [x] CI / release tooling (two new release binaries + three new CI jobs)
- [ ] Breaking change
- [ ] Requires cluster rollout

### Verification
`cargo fmt --all` + `cargo clippy --workspace --all-targets --all-features -D warnings` clean + `cargo test --workspace --all-features` (339) green; `actionlint .github/workflows/build.yaml` clean; `auto-vex-presence` smoke-tested locally (emits a valid `component_not_present` OpenVEX statement); `make calm-validate` clean.

## [2026-05-31 15:00] - Docs: fix stale provider-crate anatomy (single-binary, ADR-0004)

**Author:** Erick Bourgeois

### Changed
- `docs/src/concepts/providers.md` — the "Anatomy of a provider crate" section still showed `src/main.rs # binary entrypoint`. Per ADR-0004 each provider is a **library crate** (no `main.rs`); the single `banlieue` binary dispatches the `banlieue provider <name>` subcommand into the crate's `run()`. Updated the tree to the real layout (`lib.rs` re-exports `app::{Cli, run}`, `app.rs` holds the subcommand `Cli`/`run`, `Cargo.toml` is `[lib]`-only) and added a sentence explaining the binary↔library split.

### Why
Audit of docs vs. the single-binary model found this one stale section; everything else (architecture crates table, CALM system diagram, quickstart `banlieue completion`, vSphere guide, deploy manifests) already reflected ADR-0004.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified: `mkdocs build --strict` exits 0. No remaining `main.rs` / `cargo run -p banlieue-controller` references in `docs/src/`.

## [2026-05-31 14:30] - Release & supply-chain pipeline (binary, images, SBOM, SLSA, VEX) — ADR-0006

**Author:** Erick Bourgeois

### Added
- `docs/adr/0006-release-and-supply-chain-pipeline.md` (Accepted) — the `banlieue` binary is the core released artifact; every release ships signed multi-arch binaries, distroless + Chainguard images, SBOMs, SLSA L3 provenance, and an OpenVEX document. Models on `~/dev/5-spot`. Auto-VEX *derivation* binaries are explicitly staged.
- `.github/workflows/build.yaml` — rewritten to add the supply-chain jobs (the prior file deferred them with a note). New/changed jobs: `build` now emits `banlieue-linux-{amd64,arm64}` artifacts + CycloneDX SBOM (`make sbom`); `docker` (matrix Chainguard+Distroless, multi-arch buildx, push on non-PR, Cosign keyless sign by digest, BuildKit `sbom`+`provenance`, image SBOM via anchore/sbom-action); `attest` (GitHub build-provenance per image); `build-vex` (vexctl-merge `.vex/*.json` → Cosign `--type openvex` attest to each digest; empty-VEX-safe); `grype` (scan with `--vex` → SARIF to Code Scanning); `sign-artifacts` (tarball + Cosign + attest); `generate-provenance-subjects` + `slsa-provenance` (SLSA generator `@v2.1.0`); `package-deploy-manifests`; `upload-release-assets` (binaries + SBOMs + signatures + provenance + VEX + checksums). All firestoned composites reused; third-party actions SHA-pinned; SLSA generator tag-pinned.
- `.github/actions/prepare-docker-binaries/action.yml` — composite that stages the per-arch artifacts at `binaries/<arch>/banlieue` for the Dockerfiles.
- `Makefile` — `sbom`, `vexctl-install`, `vex-validate`, `vex-assemble` targets + `VEXCTL_VERSION`/`GRYPE_VERSION`/`PRODUCT_PURL` vars.
- `.vex/` — `README.md` (OpenVEX authoring spec), `.gitkeep`, `.affected-functions.json` (scaffold for the staged reachability tool).
- `docs/architecture/calm/architecture.json` — new `release-artifact-provenance` control (SLSA v1.0 Build L3 + SSDF), ADR-0006 registered. `make calm-validate` clean.

### Why
banlieue now has a deployable artifact (the single `banlieue` binary, ADR-0004), so the supply-chain pipeline that `build.yaml` had deferred is now warranted. Mirrors the maintainer's 5-spot pattern, adapted to banlieue's workspace.

### Staged (follow-up)
The automated VEX-derivation binaries `auto-vex-presence` (SBOM-absence) and `auto-vex-reachability` (symbol reachability) are **not** implemented — they are 5-spot's own Phase 2/3 and each warrant a TDD cycle. The VEX *assembly/attest/scan* plumbing is in place; `build-vex` has a documented seam where their artifacts merge in.

### Safety adaptation vs 5-spot
Images **build** on PRs (validates both Dockerfiles) but **push/sign/attest/scan only on push-to-main + release**, so fork PRs never require `packages:write`.

### Impact
- [x] CI / release tooling (new GHCR images, signing, SLSA, VEX on release + push-to-main)
- [ ] Breaking change
- [ ] Requires cluster rollout

### Verification
`actionlint .github/workflows/build.yaml` clean; `prepare-docker-binaries/action.yml` valid composite YAML; `make help` lists the new targets and `make -n sbom` expands; `.vex/.affected-functions.json` valid JSON; `make calm-validate` clean.

## [2026-05-31 13:00] - Bump workspace MSRV to Rust 1.88

**Author:** Erick Bourgeois

### Changed
- `Cargo.toml` — `[workspace.package] rust-version` `1.85` → `1.88`.
- `README.md`, `docs/src/index.md` — Rust MSRV badges `1.85+` → `1.88+`.

### Why
The lockfile already resolves `kube 3.1.0`, which declares `rust-version = 1.88`, so the previous `1.85` MSRV was inaccurate (it slipped through because `resolver = "2"` is not MSRV-aware). `cargo upgrade` — which *is* MSRV-aware — was flagging `kube` as "incompatible" because the newest kube compatible with a declared 1.85 MSRV is `2.0.1`. Bumping the declared MSRV to `1.88` makes it match what the project actually requires; `cargo upgrade` no longer flags `kube`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Config change only (toolchain MSRV)
- [ ] Documentation only

Verified: `cargo check --workspace --all-features` clean; `cargo upgrade --incompatible --dry-run` no longer lists `kube`.

> Follow-up option (not done): switch the workspace to `resolver = "3"` so cargo itself enforces MSRV during resolution, preventing a future silent overshoot.

## [2026-05-31 12:30] - CLI: `banlieue completion <shell>` subcommand

**Author:** Erick Bourgeois

### Added
- `crates/banlieue/src/cli.rs` — new `completion <shell>` subcommand on the unified binary. Generates a shell-completion script for the full command tree (`controller`, `provider <backend>`, `completion`) to stdout. Supports bash, zsh, fish, elvish, powershell via `clap_complete::Shell`. Logic in a testable `write_completion(shell, &mut impl Write)` helper.
- `crates/banlieue/src/cli_tests.rs` — 7 new tests: shell parsing (zsh + others), unknown/missing-shell errors, and generated-script content (zsh `#compdef banlieue` header + subcommand coverage; bash names the binary).
- `crates/banlieue/Cargo.toml` — `clap_complete = "4"` (part of the clap-rs project; tracks clap's major version). Single-crate dep, pinned directly.
- `docs/src/getting-started/quickstart.md` — "Shell completion" section with zsh/bash/fish install snippets.

### Why
Convenience: lets users install tab-completion (`banlieue completion zsh > "${fpath[1]}/_banlieue"`). Classified as a non-architectural CLI addition under the ADD methodology (no contract/topology/data-flow change), so TDD-only — no ADR/CALM.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] CLI / tooling only

### Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -D warnings` + `cargo test --all` (281) green; `banlieue completion zsh` emits a valid `#compdef banlieue` script; `mkdocs build --strict` clean.

## [2026-05-31 11:30] - Docs: remove roadmap from site; document CAPI cluster capability

**Author:** Erick Bourgeois

### Removed
- `docs/src/reference/roadmap.md` and its `mkdocs.yml` nav entry — roadmaps live outside the repo (project non-negotiable). All links repointed or dropped: `index.md` (status badge → GitHub repo; "full plan" wording; nav list), `overview.md`, `reasoning/non-goals.md`, `getting-started/{quickstart,vsphere-provider}.md`, `docs/README.md`. `concepts/virtualmachine.md` links that wrongly pointed VMClass/VMImage/API-reference at `roadmap.md` now point at `reference/api.md`. Remaining "roadmap" word-mentions reworded (`docs/adr/0003`, `architecture/index.md`).

### Changed (documentation of new CAPI work)
- `docs/src/reasoning/capi-relationship.md` — rewritten for the CAPI-native cluster decision: banlieue is a CAPI **infrastructure provider** implementing **both** the InfraMachine and InfraCluster contracts; clusters are built by CAPI core + a control-plane provider (k0smotron) over banlieue's infra CRs ("platinum = 6/6" = `replicas: 6`); corrected the contract status table to v1beta2 (`status.initialization.provisioned`, conditions-as-failures) — the page previously listed the deprecated `status.ready`/`failureReason` and claimed banlieue "never creates a cluster / takes only InfraMachine", contradicting ADR-0001/0002.
- `docs/src/concepts/infra-crds-capi.md` — intro now names both contracts; the contract field list corrected to v1beta2; added the `cluster.x-k8s.io/v1beta2` label note (ADR-0005).
- `docs/src/concepts/providers.md` — note that `VSphereCluster` aggregates Providers' failure domains across vCenters.

### Why
The user asked to remove roadmaps from the published docs and to ensure all new changes are comprehensively documented. The CAPI relationship page materially contradicted ADR-0001/0002 (it predated the InfraCluster/cluster-provisioning work) and listed CAPI fields deprecated under D-005.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [x] Documentation only

### Verification
`mkdocs build --strict` clean (no broken links after roadmap removal; new anchor cross-reference resolves). No Rust changes.

## [2026-05-31 10:00] - CAPI contract label emitted by crdgen (ADR-0005)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0005-capi-contract-label-codegen.md` (Accepted) — decision to emit the CAPI v1beta2 contract label from `crdgen` (code-first), not a kustomize overlay.
- `crates/banlieue-api/src/crdgen_support.rs` — `add_capi_contract_label()`, applied by `prepared()`: stamps `cluster.x-k8s.io/v1beta2: <served versions>` onto every `infrastructure.banlieue.io` CRD; leaves `banlieue.io` CRDs untouched. 5 new tests in `crdgen_support_tests.rs`.

### Changed
- `deploy/crds/infrastructure.banlieue.io_{vsphereclusters,vspheremachines,vspheremachinetemplates}.yaml` — regenerated; each now carries `metadata.labels."cluster.x-k8s.io/v1beta2": "v1alpha1"`. `banlieue.io` CRDs unchanged (no label).
- `crates/banlieue-api/src/infrastructure/{vsphere_machine,vsphere_cluster}.rs` — docstrings corrected: the contract label is emitted by crdgen, not "applied via kustomize".
- `docs/adr/0002-*.md` — consequence note updated to point at ADR-0005 (kustomize overlay superseded).
- `docs/architecture/calm/architecture.json` — CAPI InfraMachine/InfraCluster controls now cite the emitted label + `crdgen_support` as evidence; ADR-0005 added to `adrs`. `make calm-validate` clean; diagrams + `api.md` regenerated.

### Why
Closes the contract gap flagged in ADR-0002: without this label CAPI core does not recognise banlieue's infra CRDs as contract-compliant. Code-first emission keeps the label in the single-source-of-truth generated YAML, so it can't drift and covers future provider CRDs automatically.

### Impact
- [x] Requires cluster rollout (CRDs must be re-applied to gain the label)
- [ ] Breaking change
- [ ] Config change only

### Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -D warnings` + `cargo test --all-features --all` (292 tests) all green; label present on all 3 infra CRDs and absent on all 4 `banlieue.io` CRDs; `make calm-validate` + `mkdocs build --strict` clean.

## [2026-05-31 00:10] - Docs: comprehensive README badges + minimal docs landing badges

**Author:** Erick Bourgeois

### Changed
- `README.md` — replaced the 4 placeholder badges with a comprehensive set in two rows: CI/security (Build, Documentation, CodeQL via the native GitHub Actions workflow badges; OpenSSF Scorecard) and project (License via dynamic shields, Rust MSRV, Docs site, Status, open Issues, Last commit, PRs welcome).
- `docs/src/index.md` — added a minimal badge set (Build status + Rust MSRV) alongside the existing License + Status badges on the docs landing page.

### Why
The project had only stub badges. Comprehensive, mostly-dynamic badges surface CI/security health and project signals at a glance on GitHub; the docs landing page gets a light, non-cluttered subset.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified: `mkdocs build --strict` exits 0. Badge URLs target real workflow files (`build.yaml`, `docs.yaml`, `codeql.yaml`, `scorecard.yaml`) and the public repo `firestoned/banlieue`.

## [2026-05-30 23:30] - Single `banlieue` binary with subcommand dispatch (ADR-0004)

**Author:** Erick Bourgeois

### Added
- `docs/adr/0004-single-binary-subcommand-dispatch.md` — ADR: one `banlieue` executable packages every role; `banlieue controller` / `banlieue provider <name>` dispatch into independent library crates. Per-provider Cargo features (default = all); one image, role selected via container args.
- `docs/architecture/calm/architecture.json` — new `system-banlieue-binary` node + `rel-banlieue-binary-composed-of-roles` (`composed-of`) grouping the controller + provider services as roles of the one binary; registered ADR-0004. `make calm-validate` passes; diagrams regenerated.
- `crates/banlieue/` — new thin aggregator crate producing the single `banlieue` binary. `src/cli.rs` (clap subcommand tree + `dispatch`), `src/cli_tests.rs`, `src/main.rs`. Features: `default = ["vsphere"]`, `vsphere`, `vcsim` (pass-through).
- `crates/banlieue-provider-sdk/src/bootstrap.rs` (+ `_tests.rs`) — shared `init_tracing` / `serve_health` / `shutdown_signal`, eliminating the per-binary bootstrap duplication.
- `crates/banlieue-controller/src/app.rs` (+ `_tests.rs`) and `crates/banlieue-provider-vsphere/src/app.rs` (+ `_tests.rs`) — each role's `Cli` (`clap::Args`) + `pub async fn run(cli)`, ported from the deleted `main.rs` files.

### Changed
- `crates/banlieue-controller` and `crates/banlieue-provider-vsphere` are now **library-only** (removed `[[bin]]` + `src/main.rs`; export `Cli`/`run`). Trimmed tokio features (health/shutdown moved to the SDK) and dropped the now-unused `tracing-subscriber` dep.
- `crates/banlieue-provider-sdk` — added `bootstrap` module; tokio `net`/`io-util`/`signal` features + `tracing-subscriber` dep.
- `Cargo.toml` (workspace) — added `crates/banlieue` member.
- `Makefile` — `WORKSPACE_BINARIES`/`BINARY` default to `banlieue`; `run-local` → `cargo run -p banlieue -- controller`; `provider-vsphere-run-local` → `... -- provider vsphere`; `kind-load` no longer needs `BINARY=`.
- `Dockerfile` / `Dockerfile.chainguard` — default `ARG BINARY=banlieue`.
- `deploy/controller/deployment.yaml` / `deploy/provider-vsphere/deployment.yaml` — image → `ghcr.io/firestoned/banlieue:v0.1.0`; added role-selecting `args` (`["controller"]`, `["provider","vsphere"]`).
- `deploy/provider-vsphere/README.md`, `docs/src/getting-started/vsphere-provider.md` — updated build/run instructions to the single image + `banlieue provider vsphere` invocation.

### Why
One artifact to build, sign, scan, publish, and install — while keeping each role an independent crate with its own dependency graph (the CRD-only seam is intact; the controller still never links vSphere code unless a provider feature is on). Adding a provider becomes a feature + nested subcommand, not a new binary/image. See ADR-0004.

### Impact
- [x] Breaking change — image name changes (`banlieue-controller`/`banlieue-provider-vsphere` → `banlieue` + `args`); standalone per-role binaries no longer exist.
- [x] Requires cluster rollout — Deployments now reference the new image + args.
- [ ] Config change only
- [ ] Documentation / process only

## [2026-05-30 02:10] - Docs: root README intro + ADD methodology

**Author:** Erick Bourgeois

### Added
- `README.md` — replaced the empty stub with a full project intro: tagline + badges, what/why, a schema-correct `VirtualMachine` example (`classRef`/`imageRef`/`placement`), the "what banlieue is not" list, an Architecture section, a CRD resource table, repository layout, a Development section (incl. the ADD workflow + common `make` targets), project status, and license. The architecture section **references the single canonical diagram** at `docs/src/concepts/architecture.md` rather than duplicating a Mermaid block (one source of truth).
- `.claude/rules/architecture-driven-development.md` — new rule documenting **ADD (Architecture Driven Development)**: the governing `ADR → CALM → TDD → implement → docs` order, when full ADR+CALM applies vs TDD-only, and a checklist.

### Changed
- `.claude/CLAUDE.md` — added a top-level "GOVERNING METHODOLOGY: Architecture Driven Development (ADD)" section and an ADD entry in the CRITICAL Coding Patterns list.

### Why
The repo had no README. ADD is the maintainer's coined, governing methodology — architecture is decided (ADR) and visualized (CALM) before code (TDD) — and must steer all future work, so it's recorded in CLAUDE.md, a dedicated rule, and persistent memory.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation / process only

## [2026-05-30 22:30] - VSphereCluster (CAPI InfraCluster) + failure-domain aggregation

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/common.rs` — CAPI v1beta2 shared types `ApiEndpoint {host, port}` and `ClusterFailureDomain {name, controlPlane, attributes}` (the v1beta2 failure-domain *list* element), with round-trip tests in `common_tests.rs`.
- `crates/banlieue-api/src/infrastructure/vsphere_cluster.rs` (+ `_tests.rs`) — new `infrastructure.banlieue.io/v1alpha1` **`VSphereCluster`** CRD: banlieue's CAPI InfraCluster. Spec: `controlPlaneEndpoint`, `providerRefs`/`providerSelector` (aggregate FDs from one or more Providers), `controlPlaneFailureDomainSelector`, `paused`. Status: `initialization.provisioned`, `controlPlaneEndpoint`, `failureDomains[]`, `conditions`, `observedGeneration`. Wired into `crdgen`/`crddoc`/`lib.rs`; generated `deploy/crds/infrastructure.banlieue.io_vsphereclusters.yaml`.
- `crates/banlieue-controller/src/reconciler/vsphere_cluster.rs` (+ `_tests.rs`) — reconciler that aggregates selected `Provider.status.failureDomains[]` into the CAPI list (`build_status`/`select_providers`/`aggregate_failure_domains`, all unit-tested). No backend access. `controlPlaneFailureDomainSelector` sets per-FD `controlPlane`. Wired a second `Controller` in `main.rs` watching `VSphereCluster` + `Provider` (Provider changes requeue clusters).
- `deploy/controller/rbac/clusterrole.yaml` — least-privilege rules: `get/list/watch vsphereclusters`, `get/update/patch vsphereclusters/status` (no create/delete — CAPI/operator owns the lifecycle).
- `examples/06-vspherecluster-multi-vcenter.yaml` — a VSphereCluster spanning two vCenters.
- `docs/architecture/calm/architecture.json` — modeled the InfraCluster CR, CAPI-core node, `flow-provision-capi-cluster`, and the `capi-v1beta2-infra-cluster-contract` control; `make calm-validate` clean, diagrams regenerated.
- `docs/src/concepts/infra-crds-capi.md` — new "InfraCluster" section.

### Why
Implements ADR-0001/0002 (this turn) following the ADD methodology (ADR → CALM → TDD → implement → docs): banlieue becomes a CAPI infrastructure provider so k0s+k0smotron (and any CAPI consumer) drive cluster spread via `replicas`, with banlieue advertising failure domains aggregated across vCenters.

### Impact
- [x] Requires cluster rollout (new CRD + RBAC; controller now runs a second controller loop)
- [ ] Breaking change
- [ ] Config change only

### Follow-ups
- The CAPI contract label `cluster.x-k8s.io/v1beta2: v1alpha1` is not yet applied to any infra CRD at deploy time (no kustomize overlay exists — `VSphereMachine` has the same gap). Track separately.
- `cargo fmt` + `cargo clippy --all-targets --all-features -D warnings` + `cargo test --all` (261 tests) all green; `kubectl --dry-run=client` validates the CRD + RBAC; `mkdocs build --strict` clean.

## [2026-05-30 21:00] - ADRs: CAPI-native cluster provisioning + InfraCluster

**Author:** Erick Bourgeois

### Added
- `docs/adr/0001-capi-native-cluster-provisioning.md` (Accepted) — banlieue is a CAPI infrastructure provider; cluster lifecycle/spread/upgrades are CAPI's job (via k0smotron for k0s). No native `VMTier`/`VMCluster` CRD — "platinum = 6/6" is a CAPI `replicas: 6` over 6 failure domains.
- `docs/adr/0002-infracluster-failure-domain-aggregation.md` (Accepted) — add `infrastructure.banlieue.io/v1alpha1` `VSphereCluster` InfraCluster that aggregates failure domains from one or more `Provider`s into the CAPI v1beta2 `status.failureDomains` list. Reconciled by the main controller (pure CRD aggregation, no backend access). Capacity-awareness via provider FD gating + DRS host placement.
- `docs/adr/0003-provider-deployment-topology.md` (Proposed) — captures the per-class vs per-instance vs hybrid provider Deployment topology (O-003) for Phase 3; leans hybrid with a `deploymentStrategy` knob. Does not block 0001/0002.

### Why
Decision to keep cluster provisioning as close to CAPI as possible so banlieue works with k0s + k0smotron and any other CAPI consumer, rather than building a parallel native cluster/tier abstraction. Implementation of the `VSphereCluster` CRD and its reconciler follows.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (ADRs; code lands in follow-up entries)

## [2026-05-30 01:45] - CI: docs build regenerates the CRD API reference

**Author:** Erick Bourgeois

### Changed
- `Makefile` — the `docs` target now depends on `api-docs` (in addition to `calm-diagrams`), so `make docs` regenerates `docs/src/reference/api.md` from the Rust CRD types before building the MkDocs site.
- `.github/workflows/docs.yaml` — clarified the "Build documentation" step comment to note that `make docs` now also regenerates the API reference (the Rust toolchain was already installed for CALM-independent reasons). No new inline logic — the workflow stays Makefile-driven.

### Why
The published docs site must never show a stale CRD reference. Wiring `api-docs` into `make docs` means the Documentation workflow — which already runs `make docs` with cargo available — regenerates the reference from the committed types on every docs build (PR, push, and release deploy), catching any drift if a contributor forgets to run `make crds`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] CI / docs tooling only

Verified locally: `SKIP_CALM_DIAGRAMS=1 make docs` exits 0, regenerates `api.md`, and builds `docs/site/reference/api/index.html`.

## [2026-05-30 01:30] - Docs: generated CRD API reference page (crddoc)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/crddoc.rs` (+ `_tests.rs`) — `crdgen`-gated library module that renders every CRD as a single Markdown API-reference page. Walks each `openAPIV3Schema` (reusing `crdgen_support::prepared`), emitting per-CRD: metadata line, root "what/why" description, `kubectl get` printer columns, and recursive field tables (Field / Type / Required / Description) for `spec` and `status`. Nested objects, arrays-of-objects (`[]`), and maps (`map[string]T` / `{}`) each get their own sub-section; enum values render as "Allowed: …"; in-description Markdown headings are demoted to bold so they don't pollute the page TOC. 11 unit tests.
- `crates/banlieue-api/src/bin/crddoc.rs` — thin binary (`--out-file`, else stdout); `[[bin]] crddoc` with `required-features = ["crdgen"]`.
- `docs/src/reference/api.md` — generated API reference (all 6 CRDs), wired into the docs nav under **Reference → API Reference (CRDs)**.
- `Makefile` — `api-docs` target (`API_DOCS_OUT ?= docs/src/reference/api.md`); `make crds` now runs `api-docs` as its final step so the reference is refreshed on every CRD change.

### Changed
- `.claude/SKILL.md` — `regen-api-docs` skill updated from a Phase-4 stub to the real `make api-docs` flow.

### Why
Users (and the docs site) had no browsable schema reference — only raw CRD YAML. This renders the full CRD surface as HTML the docs site can navigate, generated from the Rust source of truth so it can never drift, and auto-refreshed whenever CRDs change.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (new generated reference page + tooling)

Verified: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -D warnings` (clean), `cargo test -p banlieue-api --all-features` (156 pass, +11 new). `make crds` regenerates YAML + `api.md`; `mkdocs build --strict` exits 0.

## [2026-05-30 01:00] - CRDs: comprehensive schema documentation in generated YAML

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-api/src/crdgen_support.rs` (+ `_tests.rs`) — new `crdgen`-gated library module with `promote_spec_description` / `prepared`. `kube-derive` hard-codes the root `openAPIV3Schema.description` to "Auto-generated derived type for `<T>` via `CustomResource`" and routes the spec struct's doc comment to the `spec` property instead. `prepared` promotes the authored spec description up to the CRD root so a bare `kubectl explain <kind>` shows the real "what is this resource" text. 2 unit tests (replace-boilerplate + no-op-without-spec-description).

### Changed
- `crates/banlieue-api/src/banlieue/{vmclass,vmimage,provider,virtualmachine}.rs`, `crates/banlieue-api/src/infrastructure/vsphere_machine.rs` — added comprehensive rustdoc to every CRD root spec struct (a "what is this / why create one / how it's used" narrative), every status struct, and the remaining nested structs / enums / fields that lacked descriptions. These flow into the generated CRD schemas (and `kubectl explain`).
- `crates/banlieue-api/src/bin/crdgen.rs` — each CRD is now run through `prepared(...)` before serialization; `render` takes the CRD by value.
- `crates/banlieue-api/src/lib.rs` — exposes `crdgen_support` under the `crdgen` feature.
- `deploy/crds/*.yaml` — regenerated. Every CRD root description is now the authored text (no more "Auto-generated derived type …" boilerplate); spec/status/field descriptions are richer throughout.

### Why
The generated CRDs are the schema users see via `kubectl explain` and IDE tooling. They previously carried kube-derive's placeholder root description and several undocumented fields. Documenting the Rust types (the code-first source of truth) is the only correct place to fix this — the YAML is generated, never hand-edited.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (schema descriptions; no field shape changes)

Verified: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -D warnings` (clean), `cargo test -p banlieue-api --all-features` (146 pass, +2 new). Generated CRDs `kubectl apply --dry-run` clean; `examples/` validate server-side dry-run.

## [2026-05-30 00:20] - Core docs: vSphere provider guide + Provider schema sync

**Author:** Erick Bourgeois

### Added
- `docs/src/getting-started/vsphere-provider.md` — new core-docs guide for the vSphere provider: credentials Secret creation (including a `GOVC_*` → Secret/Provider derivation flow with a mapping table), the minimal + capabilities-bearing `Provider` CR, running locally (`make provider-vsphere-run-local`, `RUST_LOG` override) and in-cluster, a `status` verification example, a `Ready=False` reason table (Provider + VMImage), and a `vcsim` local-dev walkthrough.
- `docs/mkdocs.yml` — added the new page to the nav under **Home → vSphere Provider**.

### Changed
- `docs/src/concepts/providers.md` — brought the `Provider` CR example in line with the actual `banlieue-api` schema: `spec.type` + `vsphere:` block → `spec.providerClassRef.name` + `spec.connection` + `spec.capabilities` (the docs had drifted from the code). Updated the provider-crate anatomy to the real layout (`client/{mod,vim,fake}.rs`, `reconciler/{provider,vmimage}.rs`, dual-Controller `main.rs`) and noted the trait-based fake-client testing seam. Linked to the new guide.
- `docs/src/getting-started/quickstart.md` — "Coming next" now links to the vSphere provider guide.

### Why
The GOVC Secret-creation how-to was only in `deploy/provider-vsphere/README.md`; the user asked for it in the published docs. While there, `concepts/providers.md` still documented an old `Provider` shape (`type:`/`vsphere:`) that no longer matches `crates/banlieue-api/src/banlieue/provider.rs`, so YAML copied from the docs would have been rejected by the CRD.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified with `mkdocs build --strict` (exit 0, no broken-link/nav warnings). All field names checked against `crates/banlieue-api/src/banlieue/provider.rs`.

## [2026-05-30 00:10] - Docs: create the vSphere Secret/Provider from GOVC_* env vars

**Author:** Erick Bourgeois

### Added
- `deploy/provider-vsphere/README.md` — new "Creating the Secret + Provider from your `GOVC_*` environment" section: a `GOVC_*` → banlieue field-mapping table and a copy-paste flow that builds the `vsphere-creds` Secret (`GOVC_USERNAME`/`GOVC_PASSWORD`) and a `Provider` whose `connection.endpoint` is normalised from `GOVC_URL` (strips scheme / `user:pass@` / trailing `/sdk`) and whose `insecureSkipTLSVerify` is derived from `GOVC_INSECURE`. Notes the `caBundle` alternative for CA-validated endpoints.

### Why
The provider is intentionally CRD/Secret-driven and does **not** read `GOVC_*` itself (explicit-over-implicit). Operators who already use `govc` had no documented path from their existing env to a working Provider; this closes that gap without weakening the spec-is-source-of-truth invariant.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

## [2026-05-30 00:00] - Makefile: RUST_LOG overridable on *-run-local targets

**Author:** Erick Bourgeois

### Changed
- `Makefile` — extracted the hardcoded `RUST_LOG=info,kube=warn` out of the `run-local` and `provider-vsphere-run-local` recipes into `RUST_LOG ?=` / `RUST_LOG_VSPHERE ?=` variables. `?=` yields to a value passed in the environment, so `RUST_LOG=debug,kube=debug make run-local` now actually uses `debug` instead of being clobbered by the recipe's literal. `RUST_LOG_VSPHERE` derives from `RUST_LOG` (appending `vim_rs=warn`) so a single override flows to both targets; it can also be overridden directly to control vim_rs verbosity.

### Why
The previous recipes hardcoded `RUST_LOG`, silently overriding any value the user set on the CLI — so `RUST_LOG=debug make run-local` had no effect.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Developer tooling only

## [2026-05-27 10:30] - Phase 1B iteration 2a: VMImage reconciler (template availability)

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs` — VMImage reconciler that walks each in-scope `Provider` of class `vsphere`, connects to its vCenter, and confirms the template named in `VMImage.spec.sources[].reference` is present in every failure-domain datacenter the Provider exposes. Writes `VMImage.status.perProvider[]` rows + an aggregate `Ready` condition. Stable per-row reasons: `Reconciled`, `TemplateNotFound`, `SecretUnavailable`, `ConnectFailed`, `LookupFailed`, `NoVSphereSource`. Pure helpers `find_vsphere_source`, `compute_template_status`, `aggregate_ready` (with bounded `&'static str` reason enum) keep the reconciler unit-testable without a kube cluster.
- `crates/banlieue-provider-vsphere/src/reconciler/vmimage_tests.rs` — 12 unit tests: source-selection variants (vsphere/Template vs others), `compute_template_status` happy path / template-absent / no-datacenters, `aggregate_ready` true/false/unknown including unknown-reason-bucketing guard, plus VMImage minimal-construct smoke for field-rename drift.
- `crates/banlieue-provider-vsphere/src/client/{mod,fake,vim}.rs` — `Template { name, moref, datacenter_moref }` slim type and `VSphereClient::find_template(dc, name) -> Result<Option<Template>>` trait method. `FakeClient` extended with `Inventory::builder().with_template("dc", "name")` (panics if the DC isn't seeded yet). Real `vim` impl uses `ViewManager::create_container_view` scoped to the datacenter MO with `VirtualMachine` filter, walks the morefs, calls `VirtualMachine::config().await` per VM and matches on `cfg.template == true && cfg.name == name`. Destroys the ContainerView eagerly.

### Changed
- `crates/banlieue-provider-vsphere/src/main.rs` — second `Controller::new(VMImage, ...)` runs alongside the Provider controller. Both controllers race against `shutdown_signal()` in one `tokio::select!`; either stream ending unwinds the binary. VMImage Api is unconditionally `Api::all(client)` (cluster-scoped CRD) regardless of `--namespace`.

### Why
After Phase 1A iteration 4 the smoke-test boundary was stuck at `Scheduled=False reason=ImageNotReady`: the main controller's scheduler filters out every Provider candidate because no provider flips `VMImage.status.perProvider[<provider>].ready=true`. Iteration 2a closes exactly that gate. With this iteration deployed, a `kubectl apply -f examples/05-virtualmachine.yaml` against a real vCenter (or vcsim) now produces `VirtualMachine.status.scheduled` populated and a `VSphereMachine` CR created in the same namespace — though the VSphereMachine itself remains unprovisioned until iteration 2b's VM-lifecycle reconciler lands.

Scope was deliberately constrained: only `ImageSourceKind::Template` is supported (no `Url`-import, no `BackingFile`); only the per-Provider readiness check (no template fingerprint / OVF re-import path). Both deferrals are recorded with `NoVSphereSource` / `TemplateNotFound` reasons so operators get actionable feedback instead of silent failures.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only
- [x] **New capability** — VMImage template-availability check; main controller's smoke test now proceeds past `ImageNotReady` once an admin populates the vSphere template in vCenter.

Verified by `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings` (clean), `cargo test --all` (144 api + 43 controller + 27 sdk + 21 provider-vsphere = 235 tests, all pass — +12 new VMImage tests).

## [2026-05-26 20:30] - Phase 1B iteration 1: vSphere provider scaffold + capability introspection

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-vsphere/` — new workspace crate, third member after `banlieue-controller` and `banlieue-provider-sdk`. Wires `vim_rs = "0.4"` with `default-features = false` (drops the `xml` SOAP transport — saves ~30-40% on debug compile). Optional `vcsim` feature flips on vim_rs's `vcsim_compat`. Cold build with vim_rs added: 3m 28s on the dev mac.
- `src/client/` — backend-agnostic `VSphereClient` trait + `VSphereClientFactory` trait so reconcilers can be unit-tested without `vim_rs`. Three modules: `mod.rs` (trait + slim domain projections `Datacenter` / `Cluster` / `Credentials`), `fake.rs` (`FakeClient` + ergonomic `Inventory::builder().with_dc("...").with_cluster(...)` for tests), `vim.rs` (production impl via `ClientBuilder::new(endpoint).basic_authn(...).insecure(...).build()` + `ViewManager` / `ContainerView` traversal).
- `src/reconciler/provider.rs` — `Provider` reconciler scoped to `spec.providerClassRef.name == "vsphere"`. Reads the `credentialsRef` Secret, connects to vCenter, walks DCs → clusters, builds one `FailureDomain` per (dc, cluster) with labels `{dc, cluster}` and `attributes.raw = {datacenter, cluster}`, then SSA-patches `Provider.status` with the FDs + `Ready=True` / `ProviderReachable=True`. Failure paths set typed conditions (`SecretMissing`, `SecretInvalid`, `ConnectFailed`, `InventoryFailed`) and short-requeue. Pure helper `failure_domain_name(provider, dc, cluster)` slugifies and truncates to 63 chars (k8s label-value cap).
- `src/reconciler/provider_tests.rs` — 9 unit tests covering the pure slug helper (basic / special-char stripping / consecutive-separator collapse / 63-char truncation), `discover_inventory` driven by `FakeClient` (count/shape, labels+raw, empty-DC, no-clusters), and a Datacenter `Clone+Eq` smoke test.
- `src/main.rs` — dual-purpose binary: CLI mirrors the main controller (`--kubeconfig`, `--namespace`, `--leader-election-*`, `--log-*`, `--health-port`, `--metrics-port`, plus `--vsphere-task-timeout-secs` reserved for iter 2). Reuses `banlieue_provider_sdk::leader::{acquire_or_wait, renew_forever}` and the same `shutdown_signal()` (SIGTERM + Ctrl-C) pattern. Default leader-election Lease: `banlieue-system/banlieue-provider-vsphere`.
- `deploy/provider-vsphere/{configmap,deployment,service,rbac/}.yaml` — full deploy manifests modeled on `deploy/controller/`. `ClusterRole` is cluster-wide (consistent with main controller's multi-tenancy story) and already includes the `infrastructure.banlieue.io/vspheremachines` verbs iteration 2 will use.
- `deploy/provider-vsphere/README.md` — operator-facing local-dev walkthrough: kind-up → vcsim-up → Secret → Provider → `provider-vsphere-run-local`. Documents the four `Ready=False` reason strings and how to recover.
- `Makefile` — new targets `vcsim-up` / `vcsim-down` / `vcsim-logs` (runs `vmware/vcsim:latest` on :8989), `provider-vsphere-run-local` (cargo run with `--features vcsim --no-leader-elect`), and `kind-deploy-provider-vsphere` (mirrors `kind-deploy-controller`).

### Changed
- `Cargo.toml` — workspace member list now includes `crates/banlieue-provider-vsphere`. New workspace dependency `vim_rs = { version = "0.4", default-features = false }` (pinned at workspace level so any future provider that needs it gets the same pin).

### Why
The roadmap's smoke-test boundary after Phase 1A iteration 3 was: "stops at `Scheduled=False reason=ImageNotReady` because no provider populates `VMImage.status.perProvider[].ready=true`." Phase 1B closes that. Iteration 1 ships the *capability-introspection* half — the binary connects to vCenter (real or `vcsim`), walks inventory, and writes `failureDomains[]` so the main controller's scheduler can place VMs. The VSphereMachine VM-lifecycle half (clone-from-template → power-on → status mirror) is iteration 2. Choosing `vim_rs` over hand-rolling VI bindings: actively maintained (v0.4.4 April 2026), tokio/reqwest async, ships a `vcsim_compat` feature for the simulator; the 3-5 minute cold compile is mitigated by isolating the dep to this one crate.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only
- [x] **New capability** — the binary can be deployed today to populate `Provider.status` for a vSphere-class Provider. VM lifecycle still NYI (iteration 2).

Verified by `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings` (clean across all four crates), `cargo test --all` (144 api + 43 controller + 27 sdk + 9 provider-vsphere = 223 tests, all pass). vcsim end-to-end smoke test is operator-driven via the manifest in `deploy/provider-vsphere/README.md` — not yet automated in CI.

## [2026-05-26 19:30] - Phase 1A iteration 4: leader election + CLI/log close-out

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-sdk/src/leader.rs` — lease-based leader election against `coordination.k8s.io/v1.Lease`. Pure decision function `decide_action(now, lease, cfg) -> LeaseAction` (AcquireNew | Renew | Wait | TakeOver) separated from the async I/O so the logic is unit-testable without a cluster. `LeaderConfig` carries namespace / lease name / identity / lease_duration / renew_period / retry_period with `validate()` rejecting zero durations, `renew >= lease`, and empty identity. `LeaderConfig::default_identity()` reads `POD_NAME` then `HOSTNAME` then falls back to `"unknown"`. Defaults match `kube-controller-manager`: 15s lease, 5s renew, 2s retry. Field manager `banlieue.io/leader-election`.
- `crates/banlieue-provider-sdk/src/leader_tests.rs` — 13 unit tests for `decide_action` and `LeaderConfig::validate`: no-lease → AcquireNew, no-holder → AcquireNew, held-by-us → Renew (even when our own renew is stale), held-by-other within duration → Wait, held-by-other at the renew_time+duration boundary → Wait, held-by-other past duration → TakeOver, held-by-other with no renew_time → TakeOver, no-spec → AcquireNew, plus the four config-validation cases.
- `crates/banlieue-controller/src/main.rs` — new CLI flags: `--kubeconfig` (env `KUBECONFIG`), `--log-level` (env `BANLIEUE_LOG_LEVEL`), `--no-leader-elect` (env `BANLIEUE_NO_LEADER_ELECT`), `--leader-election-namespace` (default `banlieue-system`), `--leader-election-id` (default `banlieue-controller`), `--leader-election-identity` (defaults to `POD_NAME` / `HOSTNAME`). New helpers `build_leader_config(&Cli)` and `shutdown_signal()` (SIGTERM + Ctrl-C tokio::select). `init_tracing` now honours `--log-level` as an override for `RUST_LOG`.

### Changed
- `crates/banlieue-controller/src/main.rs` — startup sequence now: parse CLI → init tracing → build client → spawn health server → (unless `--no-leader-elect`) `acquire_or_wait` for the Lease, then spawn `renew_forever` in a background task whose terminal failure calls `std::process::exit(1)` (Deployment restarts the pod). The controller stream now races against `shutdown_signal()` via `tokio::select!` so SIGTERM yields a clean exit instead of being orphaned.
- `crates/banlieue-provider-sdk/src/lib.rs` — `pub mod leader;` registered; module list in the crate-level doc updated.
- `deploy/controller/rbac/clusterrole.yaml` — comment on the `coordination.k8s.io/leases` rule updated to describe banlieue's actual usage (GET + CREATE + SSA PATCH); verbs unchanged (already adequate).

### Why
The roadmap's Phase 1A `Definition of done` was met by iteration 3 *except* for leader election and the few remaining CLI flags called out in `~/dev/roadmaps/banlieue/10-PHASE-1A-CONTROLLER-AND-SDK.md`. This iteration closes those out so multi-replica Deployments (or rolling restarts) can run without two controller pods racing to reconcile the same VirtualMachine and SSA-fighting each other's status patches. After this iteration, Phase 1A is fully done; Phases 1B / 1C / 1D / 1E are now unblocked per the dependency graph in `~/dev/roadmaps/banlieue/README.md`.

The decision logic is deliberately pure so it can be exhaustively tested without a kube cluster — the async loop is then a thin wrapper that the controller's smoke test exercises end-to-end (running it locally creates a Lease in `banlieue-system` named `banlieue-controller` and refreshes it on a 5s cadence).

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only
- [x] **New capability** — multi-replica controller HA enabled by default; opt out with `--no-leader-elect` for single-instance local dev.

Verified by `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings` (clean), `cargo test --all` (27 SDK tests including 13 new leader tests, 43 controller tests, 144 api tests; all pass).

## [2026-05-26 18:30] - CALM architecture index + deeper CAPI relationship doc + safer calm-* targets

**Author:** Erick Bourgeois

### Added
- `docs/src/architecture/index.md` — section landing page for the CALM-rendered docs. Explains why banlieue uses FINOS CALM, summarises what's in the model (16 nodes / 13 relationships / 3 flows / 4 controls), tabulates the controls with NIST references and evidence-file links, and documents the `make calm-validate` / `calm-diagrams` / `calm-docify` workflow.
- `docs/src/reasoning/capi-relationship.md` — deeper "Why" page on the CAPI relationship. Contrasts banlieue and CAPI head-to-head, tabulates the exact v1beta2 `InfraMachine` fields banlieue mirrors, enumerates what banlieue deliberately *does not* take from CAPI (`Cluster`, `Machine*`, bootstrap providers, control-plane providers, `clusterctl`), and explains the v1beta2 pin. Complements (does not replace) the existing `concepts/infra-crds-capi.md`.
- `Makefile` target `calm-docify` — invokes `calm docify` against the existing template directory and writes into `docs/src/architecture/`. Functionally equivalent to `calm-diagrams` today; documented as the forward-looking entry point for richer multi-page bundles.

### Changed
- `Makefile` (`calm-diagrams` and `calm-docify`) — replaced `--clear-output-directory` with an explicit `rm -f` of the two generated files plus any `.hbs` leftovers. The blanket clear would have deleted the new hand-maintained `architecture/index.md` on every re-render.
- `docs/mkdocs.yml` nav — promoted the CALM diagrams from "Concepts" into their own top-level section **Architecture (CALM)** with `index.md` as the landing page. Added `Relationship to Cluster API` under **Why banlieue?** between `CRD-Only Contract` and `Comparisons`.

### Why
The CALM rendering targets already existed (system.md / flows.md) and were in sync with `architecture.json`, but the section had no landing page — readers arriving at a Mermaid blob got no context. Likewise, `concepts/infra-crds-capi.md` answered *what* the CAPI contract is but not *why* banlieue chose contract-compatibility over full CAPI adoption, which is the question that recurs in conversations with reviewers.

The Makefile fix is load-bearing: without it the new section index would silently disappear the next time anyone ran `make docs` or `make calm-diagrams`.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

Verified by `make calm-validate` (0 issues), `make calm-diagrams` (rendered, index.md survived), and `cd docs && poetry run mkdocs build` (built in 1.87s, two expected first-render git-history warnings).

## [2026-05-26 17:00] - Phase 1A iteration 3: migration sub-loop + cascade-wait finalizer + image watcher + Provider threading

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-controller/src/reconciler/migration.rs` — pure function `migration::evaluate(vm, decision) -> MigrationAction`. Detects placement drift between the freshly-computed `Decision` and the previously-recorded `ScheduledPlacement`; decides among `InPlace` / `StickToOld` / `SurfaceOnly { reason }` / `Recreate { reason }` per `VirtualMachine.spec.migrationPolicy` (`Never` → stick; `Manual` → surface unless `banlieue.io/migrate=true` annotation is set; `Automatic` → recreate). Drift kinds: `ProviderChanged`, `FailureDomainChanged`, `StorageMappingChanged`, `NetworkMappingChanged` — each maps to a stable condition `reason` string for `PlacementValid=False`.
- `crates/banlieue-controller/src/reconciler/migration_tests.rs` — 12 unit tests covering the full matrix (drift kind × policy × annotation state) plus the stable-reason-string guarantee. Includes the explicit "provider-change wins when BOTH change" tiebreaker.

### Changed
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs` — reconcile loop now:
  - Calls `migration::evaluate` after the scheduler; branches on `MigrationAction`:
    - `InPlace` → existing apply-then-mirror flow.
    - `StickToOld` → `mirror_only_path` (read the existing infra CR, mirror status, **don't** apply a new placement; `PlacementValid` is left at its previous value because `Never` says drift is acceptable).
    - `SurfaceOnly { reason }` → `patch_placement_invalid` writes `PlacementValid=False reason=<reason>` + `Ready=False reason=PlacementInvalid`; infra CR untouched.
    - `Recreate { reason }` → `delete_existing_infra` (idempotent, 404-tolerant); `patch_placement_invalid`; the *next* reconcile pass creates a fresh `VSphereMachine`.
  - `finalize_vm` now does proper cascade-wait: looks up the owned `VSphereMachine`; if it exists, issues delete and requeues; only when it's fully GC'd does the parent's `banlieue.io/virtualmachine` finalizer get dropped. Guarantees no backend leak on `kubectl delete vm`.
  - `build_vsphere_machine` is now called with the chosen `&Provider`. (The vSphere builder doesn't read it yet — the `Decision` already carries the resolved backend IDs — but the signature establishes the contract for Phase 1C/1D where Proxmox needs `Provider.spec.connection.endpoint` to target a cluster and libvirt needs SSH transport settings.)
- `crates/banlieue-controller/src/reconciler/infra.rs` — `build_vsphere_machine` signature takes `&Provider` (currently `_provider`). Docstring explains why the parameter exists even though vSphere doesn't consume it yet.
- `crates/banlieue-controller/src/reconciler/infra_tests.rs` — every call-site updated; new `parent_provider()` test helper constructs a `Provider` with a default `ProviderConnection`.
- `crates/banlieue-controller/src/reconciler/mod.rs` — `pub mod migration;` registered.
- `crates/banlieue-controller/src/main.rs` — Controller setup now uses:
  - `Controller::owns(VSphereMachine, ...)` — owner-reference-driven event flow so status mirror reacts immediately when a provider patches infra status, instead of waiting for the 30s requeue. Closes the missed Phase 1A "Gotcha" #1 (`Watch infra CRs with a Controller::owns relationship`).
  - `Controller::watches(VMImage, ...)` with a closure-captured `Store<VirtualMachine>` — image watcher: when `VMImage.status.perProvider[].ready` flips, every VM with `spec.image_ref.name == image.name` is re-queued. The scan is linear over the store; VMImage updates are operator-driven and rare, so this is fine for v1.

### Why
Iteration-2 changelog explicitly listed four items deferred to iteration 3. All four land here, plus the `Controller::owns` wiring that was a Phase 1A "Gotcha" the iteration-2 work missed. After this iteration, the Phase 1A "Definition of done" is fully met *except* for leader election + a few CLI flags (deferred to iteration 4 / Phase 1A close-out — they're operational niceties, not contract gaps).

The migration sub-loop is the load-bearing piece: it's the user-visible enforcement of the [least-touch principle](../docs/src/reasoning/least-touch.md). A consumer changes `providerRef.name` and (with `migrationPolicy=Automatic`) the system rebuilds the infra against the new backend without further input. The whole point of banlieue is encoded in the `MigrationAction::Recreate` arm of `evaluate`.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅ (clean)
- `cargo test --all` ✅ — **201 tests pass** (144 api + 43 controller + 14 sdk; +12 controller tests this iteration: 12 new migration cases, infra tests updated to thread Provider).
- `cargo build -p banlieue-controller` ✅ — main.rs compiles with the new `owns` + `watches` wiring.

### Phase 1A status after this iteration
- ✅ Resolve refs + scheduler + status mirror + infra builder (iter 2).
- ✅ Migration sub-loop, recreate-only path (this iter).
- ✅ Cascade-wait finalizer (this iter).
- ✅ Provider threading for future providers (this iter).
- ✅ Image watcher / event-driven re-queue on `VMImage` flips (this iter).
- ✅ `Controller::owns(VSphereMachine)` for fast status feedback (this iter; was a missed Gotcha).
- ⏳ Leader election (`Lease`-based) — SDK module + main.rs flags. Deferred to iteration 4 or Phase 1A close-out.
- ⏳ CLI flags `--leader-election-namespace` / `--leader-election-id`. Tied to leader election above.

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout (controller behaviour materially changes; existing kind-deployed controllers should be redeployed)
- [ ] Config change only
- [ ] Documentation only

### Deferred to Phase 1B
- Without a real vSphere provider populating `Provider.status.failureDomains` and `VMImage.status.perProvider`, the smoke-test boundary remains `Scheduled=False reason=ImageNotReady`. The migration / cascade-wait / image-watcher paths are exercised by unit tests on synthetic inputs; end-to-end exercise lands when the provider does.

---

## [2026-05-26] - Address GHAS findings on PR #2 (Semgrep crdgen + CodeQL docs.yaml)

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-api/src/bin/crdgen.rs`: switched manual `std::env::args()` parsing to `clap::Parser`. Eliminates the Semgrep `rust.lang.security.args.args` finding by removing the direct `args()` call entirely, and adds free `--help` / `--version`. The CLI surface (`--out-dir <DIR>`) is unchanged.
- `crates/banlieue-api/Cargo.toml`: added `clap = { workspace = true, optional = true }` and extended the `crdgen` feature to `["dep:serde_yaml", "dep:clap"]`. clap is feature-gated so the library API surface is unchanged when `crdgen` is off.
- `.github/workflows/docs.yaml`: hard-gated the `build` job against `workflow_run` events that originated from a fork. Two layers of defence:
  1. Job-level `if:` — the build job runs only when the trigger is not `workflow_run`, OR when the `workflow_run.head_repository.full_name` equals the current repository.
  2. A new fail-fast "Verify trusted workflow_run source" step that runs **first** on `workflow_run` events and `exit 1`s before any checkout / install / cache step can execute.

### Why
GHAS surfaced 8 findings on PR #2 (https://github.com/firestoned/banlieue/pull/2):

- **Semgrep `rust.lang.security.args.args`** on `crdgen.rs:25` — the rule fires on any direct use of `std::env::args()`. Our code did `.skip(1)` to drop the program name (the actual security concern in the rule's docs), so this was a false-positive-shaped finding. Switching to clap silences it deterministically rather than via suppression comments.
- **CodeQL "Checkout of untrusted code in a privileged context"** ×5 + **"Cache Poisoning via caching of untrusted files"** ×2 on `docs.yaml` — these are *real*. `workflow_run` always executes with default-branch permissions, even when the upstream "Build" workflow was triggered by a fork's PR. Without a guard, the build job would check out the fork's SHA into a privileged context and run `poetry install` / `cargo build` / `npm install` on potentially malicious files, plus write to the default-branch GHA cache (cache poisoning). The job-level `if:` + fail-fast step refuse to run on fork-originated workflow_run events.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 189 tests pass (144 api + 31 controller + 14 sdk; unchanged from iteration 2).
- `cargo run -p banlieue-api --bin crdgen --features crdgen -- --help` ✅ — emits the expected usage block.
- `cargo run -p banlieue-api --bin crdgen --features crdgen -- --out-dir deploy/crds` ✅ — still writes all 6 CRDs.
- `python3 -c "yaml.safe_load_all(open('.github/workflows/docs.yaml'))"` ✅ — YAML parses.
- Inspected the rendered workflow: `build.if` carries the fork-blocking expression; the first step (`Verify trusted workflow_run source`) is gated on `workflow_run` events and exits non-zero on a fork mismatch before the checkout step runs.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only (Semgrep fix is internal tooling; CodeQL fix only changes CI workflow behaviour for fork-originated chained workflow_run events, which had no legitimate need to ever run)

### Remaining follow-up
- The 8 alerts on PR #2 will auto-close on the next CodeQL/Semgrep scan once this branch is rebased / re-pushed. Confirm via `gh pr view 2` after the next CI run that no GHAS comments remain.
- If CodeQL still flags the workflow after the next scan (static analysis sometimes can't see job-level `if:` guards), the proper next step is to split the workflow into a `docs-build.yaml` (push/PR triggered; no privileged context) and a `docs-deploy.yaml` (workflow_run; downloads the already-built artifact, never checks out user code). That refactor is deferred until we see whether the guard suffices.

---

## [2026-05-26] - Phase 1A iteration 2: scheduler + status mirror + infra builder

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-controller/src/reconciler/scheduler.rs` — pure function `schedule(vm, class, image, providers, existing_vms) → Result<Decision, ScheduleError>`. No I/O, no async. Filter chain: providerSelector → failureDomainSelector → image readiness → storage classes → network classes → features → firmware (`efi-secure` requires `efiSecureBoot` feature) → required anti-affinity. Tie-break: alphabetical by `(provider_name, fd_name)`. `Decision` is owned (no lifetimes); `.to_scheduled_placement(now)` projects it onto `VirtualMachineStatus.scheduled`. `ScheduleError` exposes stable `reason()` strings (`reasons::NO_PROVIDER`, `IMAGE_NOT_READY`, ...) for deterministic condition writes.
- `crates/banlieue-controller/src/reconciler/scheduler_tests.rs` — 16 table-driven tests: happy path, every filter step (including required anti-affinity collision and tiebreak), backend-id BTreeMap-first-value rule, `to_scheduled_placement` round-trip.
- `crates/banlieue-controller/src/reconciler/status_mirror.rs` — `InfraMachineRead` trait + impl for `VSphereMachine` + pure `mirror_status_from_infra(current, infra, generation) → VirtualMachineStatus`. Mirrors `initialization` and `addresses`, projects the infra `Ready` condition onto the parent's `InfrastructureReady`, and computes aggregate `Ready = Scheduled && PlacementValid && InfrastructureReady` (with `Pending` reason when the infra hasn't reported yet).
- `crates/banlieue-controller/src/reconciler/status_mirror_tests.rs` — 7 tests across every Ready combination + missing-status fallback.
- `crates/banlieue-controller/src/reconciler/infra.rs` — `build_vsphere_machine(vm, class, image, decision) → Result<VSphereMachine, InfraBuildError>`. Resolves datacenter/cluster from `failure_domain_raw`, datastore from the first resolved storage backend_id, template from `VMImage.status.perProvider[i].resolved_ref`. Sets controller-owning `OwnerReference` back to the parent VM. Propagates the VM's `app=*` labels and adds `banlieue.io/owned-by=<vm-name>`.
- `crates/banlieue-controller/src/reconciler/infra_tests.rs` — 5 tests: happy path, owner-reference shape, missing fd-raw attributes (datacenter / cluster), missing image resolved_ref, label propagation.

### Changed
- `crates/banlieue-controller/src/reconciler/virtualmachine.rs` — replaced the iteration-1 `SchedulerNotImplemented` stub with the real reconcile flow:
  1. Ensure finalizer (`banlieue.io/virtualmachine`).
  2. Resolve VMClass + VMImage (cluster-scoped via `Api::all`).
  3. List Providers + sibling VMs in the VM's namespace.
  4. Call `schedule`; on failure, surface `Scheduled=False` with the typed reason and requeue.
  5. Build the `VSphereMachine` via `infra::build_vsphere_machine`; SSA it (`field_manager=banlieue.io/controller`).
  6. Read it back; mirror its status onto the VM.
  7. Patch VM status (`scheduled`, `infrastructureRef`, conditions, `observedGeneration`).
- `crates/banlieue-controller/src/reconciler/mod.rs` — added `pub mod infra; pub mod scheduler; pub mod status_mirror;`.
- `crates/banlieue-controller/src/reconciler/virtualmachine_tests.rs` — replaced the iteration-1 stub tests with a stable assertion that the finalizer constant string never silently changes.

### Why
Iteration 1 shipped controller scaffolding + a stub reconciler that only wrote `Scheduled=False reason=SchedulerNotImplemented`. Iteration 2 makes the controller actually *do* the thing: it picks a `(provider, failure domain)` pair, projects the choice into a `VSphereMachine`, and mirrors the infra status back. Because the vSphere *provider* binary doesn't exist yet (Phase 1B), the system stops cleanly at `Scheduled=False reason=ImageNotReady` — the exact boundary between this iteration and the next.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — **189 tests pass** (144 api + 31 controller + 14 sdk; +29 controller tests new this iteration).
- **Smoke test on kind** (`kind-banlieue-dev` with examples pre-applied):
  - `./target/release/banlieue-controller` connects to the apiserver, watches `VirtualMachine` cluster-wide, reconciles `banlieue-system/db-prod-01`.
  - Resolves `VMClass` (`db-prod-large`) and `VMImage` (`ubuntu-22.04-cloudinit`); lists 2 Providers.
  - Runs the scheduler; hits `ImageNotReady` because no provider has populated `VMImage.status.perProvider`.
  - Writes `Scheduled=False reason=ImageNotReady` + `Ready=False reason=Scheduling` to the VM. Confirmed via `kubectl get virtualmachine db-prod-01 -o jsonpath='{.status.conditions[*].reason}' → "Scheduling ImageNotReady"`.
  - Requeues continuously (default 30 s), no `VSphereMachine` created (correct — scheduling failed pre-build).

### Impact
- [ ] Breaking change
- [x] Requires cluster rollout (manifests unchanged but the controller behaviour materially changes; if you have an old controller running, redeploy)
- [ ] Config change only
- [ ] Documentation only

### Deferred to iteration 3
- **Migration sub-loop** — when scheduler returns a different `(provider, fd)` than `status.scheduled`, set `PlacementValid=False`; act per `migrationPolicy` (`Automatic` → recreate, `Manual` → wait for the `banlieue.io/migrate=true` annotation, `Never` → leave alone).
- **Image watcher** — side reconciler that re-queues affected VMs when `VMImage.status.perProvider[].ready` flips.
- **Deletion-finalizer cascade waits** — block finalizer drop until the owned `VSphereMachine` has been fully GC'd.
- **Provider Spec usage at infra-build time** — the chosen Provider is looked up in the reconciler (`_chosen_provider`) but isn't passed to the builder yet; providers that need spec-level fields (libvirt SSH config etc.) will use it.

### Deferred to Phase 1B
- `crates/banlieue-provider-vsphere/` — without it, no provider populates `Provider.status.failureDomains` or `VMImage.status.perProvider`, so end-to-end provisioning stops at `ImageNotReady`. This is by design: the scheduler is now correct on synthetic inputs, and 1B fills in the real data.

---

## [2026-05-26 16:00] - Add Documentation GitHub Actions workflow + nav: Getting Started under Home

**Author:** Erick Bourgeois

### Added
- `.github/workflows/docs.yaml`: mirrors `~/dev/5-spot/.github/workflows/docs.yaml`. Two reusable-workflow calls into `.github/workflows/calm.yaml` (`validate` + `template`) run before the build job, which downloads the rendered CALM diagrams as an artifact and runs `make docs` with `SKIP_CALM_DIAGRAMS=1` (the diagrams already came from the previous job). PRs additionally get a linkinator broken-link check (`continue-on-error: true`). Deploy to GitHub Pages is gated through `workflow_run` against the existing **Build** workflow — docs only publish when Build succeeded for a `release` event, so a broken release never publishes docs for that tag. All third-party actions pinned by SHA.

### Changed
- `docs/mkdocs.yml`: **Getting Started** is now a sub-page of **Home** (using MkDocs Material's `navigation.indexes` so `index.md` is the section landing page and `Getting Started: getting-started/quickstart.md` sits beneath it in the left sidebar). The standalone top-level **Getting Started** section is removed.

### Why
- The reusable `.github/workflows/calm.yaml` workflow has been in the repo for a while but had no orchestrator wiring it into the CI pipeline. `docs.yaml` is that orchestrator. It enforces the same shape as 5-spot: validate the CALM JSON first, render diagrams second, build the site third, deploy only on release. This pattern keeps the documentation pipeline reproducible and prevents drift between architecture-as-code and the rendered diagrams.
- The Home → Getting Started nesting matches the user's intent that the Quick Start be the first thing a new visitor lands on after the homepage, surfaced in the left sidebar rather than buried in a separate top-level section.

### Verification
- `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/docs.yaml'))"` ✅ parses; jobs `calm-validate`, `calm-diagrams`, `build`, `deploy` resolved; both reusable calls point at `./.github/workflows/calm.yaml` which exists in-tree.
- `cd docs && poetry run mkdocs build` ✅ rebuilds in 1.74s with the new nav; warnings are the unrelated `git-revision-date-localized` plugin chatter about pages without git history, which clears once the files are committed.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

---

## [2026-05-26 15:30] - Bootstrap FINOS CALM architecture-as-code

**Author:** Erick Bourgeois

### Added
- `docs/architecture/calm/architecture.json`: CALM 1.2 architecture document for banlieue. Models 16 nodes (2 actors, 1 ecosystem, 5 services — incl. the three planned provider controllers, 3 networks for vSphere/Proxmox/libvirt backends, 5 data assets for every CRD), 13 relationships (every wire is HTTPS to the K8s API; no controller-to-controller arrow by design), and 3 flows: **Create**, **Swap**, **Delete**. Each flow encodes a project tenet — Swap is the canonical least-touch demo. Controls reference NIST SP 800-53 Rev. 5 and SP 800-218 (SSDF) and the CAPI v1beta2 InfraMachine contract.
- `docs/architecture/calm/templates/mermaid/system.md.hbs` + `flows.md.hbs`: Handlebars templates rendering one Mermaid `flowchart LR` of every node/relationship, and one `flowchart TD` per flow. Mirrors the 5-spot template style.
- `docs/architecture/calm/README.md`: contributor doc — what the architecture models, how to validate, how to render, how to extend.
- `docs/src/architecture/system.md` + `flows.md`: placeholder stubs so `mkdocs build` works on a fresh clone before `make calm-diagrams` has been run. Both are wiped + regenerated by the CALM CLI on `make calm-diagrams` (the CLI's `--clear-output-directory` flag).
- `docs/src/concepts/architecture.md`: cross-link admonition pointing at the new CALM pages, naming them as the canonical source of truth.
- `docs/mkdocs.yml`: nav now includes **System Diagram (CALM)** and **Architecture Flows (CALM)** under Concepts.

### Changed
- Root `Makefile`: added `CALM_CLI_VERSION` (1.37.0), `CALM_ARCH`, `CALM_TEMPLATES`, `CALM_DIAGRAMS_OUT` variables; added `calm-validate` and `calm-diagrams` targets; `docs` now depends on `calm-diagrams` so the rendered pages are always in sync before MkDocs runs; `docs-clean` also removes the generated `architecture/system.md` and `flows.md`. Honours `SKIP_CALM_DIAGRAMS=1` for air-gapped / offline builds.

### Why
The repository already shipped the reusable `.github/workflows/calm.yaml` workflow (mirrored from 5-spot earlier in the project) but had no actual CALM architecture document for it to validate. This change provides the missing input. Modelling banlieue's architecture in CALM gives:

- A **machine-validated** source of truth (`calm validate` runs in CI).
- A **single rendering pipeline** for system + flow diagrams, replacing hand-drawn Mermaid that drifts from code.
- A way to **encode project tenets as controls** (CRD-only contract → AC-4/SC-7; least-touch principle → CM-3/CM-4; code-first CRDs → SSDF PW.4/PS.1) with evidence pointing at the relevant repo paths.

The Swap flow is deliberately included even though no provider exists yet (Phase 1B+): it's the *defining* user-visible behaviour banlieue is built around, and having it in CALM forces every future change to preserve it.

### Verification
- `python3 -c "import json; json.load(open('docs/architecture/calm/architecture.json'))"` ✅
- mkdocs `nav:` audited — every entry resolves to a real file under `docs/src/`.
- `make calm-validate` not run here (requires `npx`); CI's `calm.yaml` reusable workflow exercises this path.
- `make calm-diagrams` not run here for the same reason; the stub `system.md` / `flows.md` files keep `mkdocs build` working until it runs.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

---

## [2026-05-26] - Default namespace `banlieue-system` + fix `insecureSkipTLSVerify` field rename

**Author:** Erick Bourgeois

### Changed
- `examples/0{1,2,5}-*.yaml`: `namespace: ops` → `namespace: banlieue-system`. All user-facing examples now target the same namespace as the controller, so a fresh `make kind-deploy-crds` followed by `kubectl apply -f examples/` works without first having to create another namespace.
- `Makefile` — `kind-deploy-crds` now also applies `deploy/controller/namespace.yaml`, so the namespace exists for examples even before `kind-deploy-controller` runs.
- `crates/banlieue-api/src/banlieue/provider.rs`: added `#[serde(rename = "insecureSkipTLSVerify")]` on `ProviderConnection.insecure_skip_tls_verify`. The auto-derived camelCase produced `insecureSkipTlsVerify` (lowercase `s` between TL/Verify); the CAPI convention (and what the example YAML already used) is `insecureSkipTLSVerify` with uppercase `TLS`.
- `crates/banlieue-api/src/banlieue/provider_tests.rs`: updated the JSON-roundtrip assertion to expect `insecureSkipTLSVerify`.
- `deploy/crds/banlieue.io_providers.yaml`: regenerated.

### Why
`make kind-deploy-crds` then `kubectl apply -f examples/` left users with a "no namespace `ops`" surprise, and the vSphere Provider example was rejected with:
```
error when creating "examples/01-provider-vsphere-dc1.yaml": Provider in version "v1alpha1"
cannot be handled as a Provider: strict decoding error:
unknown field "spec.connection.insecureSkipTLSVerify"
```
Two separate issues, fixed together: the examples now target the same default namespace as the controller, and the Provider type accepts CAPI-style `insecureSkipTLSVerify` on the wire.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 160 tests passed (144 api + 2 controller + 14 sdk).
- `make crds` ✅ — regenerated.
- `make kind-deploy-crds && kubectl apply -f examples/` ✅ — all four example resources land successfully in `banlieue-system`:
  ```
  provider.banlieue.io/vcenter-dc1            created
  provider.banlieue.io/libvirt-edge-host-7    created
  vmclass.banlieue.io/db-prod-large           created
  vmimage.banlieue.io/ubuntu-22.04-cloudinit  created
  virtualmachine.banlieue.io/db-prod-01       created
  ```

### Impact
- [x] **Breaking change** (pre-v1alpha1): wire field renamed `insecureSkipTlsVerify` → `insecureSkipTLSVerify`. No production users yet; YAML written against the previous CRD must update.
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

---

## [2026-05-26 14:30] - Bootstrap MkDocs documentation site

**Author:** Erick Bourgeois

### Added
- `docs/mkdocs.yml`: MkDocs Material configuration mirroring the `~/dev/5-spot` setup (Material theme, dark mode, search, mermaid via `pymdownx.superfences` + `mermaid@11` CDN, git-revision-date-localized plugin, Roboto fonts, full pymdownx extension set).
- `docs/pyproject.toml` + `docs/.python-version` + `docs/.gitignore` + `docs/README.md`: Poetry-managed Python deps (`mkdocs>=1.6,<2`, `mkdocs-material^9.5`, plugins), Python 3.11 pin, build-artefact ignores, contributor README.
- `docs/src/index.md`: project landing page with one-line pitch, what/why, status, links.
- `docs/src/overview.md` (NEW, per follow-up request): "what banlieue does, fundamentally" page with a high-level mermaid diagram showing user → K8s API → banlieue-controller → infra CRD → provider controllers → real backends. Linked right under Home in the nav.
- `docs/src/reasoning/`: the comprehensive *why* of the project — `index.md` (entrypoint), `problem.md` (fragmented VM control plane), `abstraction-principle.md` (least-touch principle), `least-touch.md` (swap / mix / onboard scenarios), `crd-only-contract.md` (no RPC; K8s API is the bus), `comparisons.md` (Kubevirt / CAPI / Crossplane / Terraform / hypervisor SDKs), `non-goals.md`.
- `docs/src/concepts/`: `index.md`, `architecture.md` (components, reconcile flow, watches, SSA), `virtualmachine.md` (CRD shape, status, lifecycle), `providers.md` (Provider CR + provider controller anatomy + SDK pointers), `infra-crds-capi.md` (why we satisfy the CAPI v1beta2 InfraMachine contract).
- `docs/src/getting-started/quickstart.md`: stubbed Phase 0/1A quick start with explicit "not production-ready" admonition.
- `docs/src/reference/roadmap.md` + `docs/src/reference/license.md`: public-facing roadmap (Phase 0 → 1E) and Apache-2.0 summary.
- `docs/src/stylesheets/extra.css`: neutral slate/sky/amber palette (no corporate branding from the 5-spot source), mermaid zoom/pan, TOC, mobile + print styles.
- `docs/src/javascripts/mermaid-init.js`: mermaid initialiser + zoom/pan handlers, supports Material's instant-navigation re-render via `document$`.
- Root `Makefile`: `docs`, `docs-serve`, `docs-clean`, `docs-deploy` targets — Poetry-based, all logic in the Makefile per the project's "workflows are Makefile-driven" rule.
- Root `.gitignore`: ignore `docs/site/`, `docs/.venv/`, `docs/__pycache__/`.

### Why
The repository shipped with an empty `docs/` directory and a stub `README.md`. The maintainer asked for comprehensive initial documentation of the project's *reasoning* — specifically the belief in abstracted APIs with "least touch" on the user's workflow, allowing providers to be swapped and mixed. The doc site is the right home for that long-form material, and `~/dev/5-spot` already has a polished MkDocs setup that other projects in this stack mirror. Mimicking that setup keeps the toolchain consistent (Poetry + MkDocs Material + the same plugins + Mermaid pattern).

A follow-up request added an `overview.md` page sitting between the home page and the `Why banlieue?` section: a fundamentals-first explainer with a single high-level mermaid diagram showing the three actors (user, banlieue controller, provider controllers) and the K8s API as the bus.

### Impact
- [ ] Breaking change
- [ ] Requires cluster rollout
- [ ] Config change only
- [x] Documentation only

### Verification
- `docs/mkdocs.yml` is syntactically valid YAML; nav references every file in `docs/src/`.
- All internal links from `index.md`, `overview.md`, and the reasoning pages resolve to files that exist on disk.
- `make docs-serve` will install Poetry deps and start MkDocs locally (not run here; the maintainer can verify with `cd docs && poetry install && poetry run mkdocs serve`).

---

## [2026-05-26] - Fix bug-027: PowerState YAML 1.1 boolean trap rejects CRD

**Author:** Erick Bourgeois

### Changed
- `crates/banlieue-api/src/common.rs`: Renamed `PowerState::On`/`Off`/`Suspended` → `PowerState::PoweredOn`/`PoweredOff`/`Suspended`. Removed the `#[serde(rename_all = "PascalCase")]` since the variant names are already the desired wire form.
- `crates/banlieue-api/src/banlieue/virtualmachine.rs`: `default_power_on` now returns `PowerState::PoweredOn`; docstring updated.
- `crates/banlieue-api/src/common_tests.rs` + `crates/banlieue-api/src/banlieue/virtualmachine_tests.rs`: updated assertions to the new variant names. Added a regression test (`power_state_rejects_legacy_short_form`) asserting that `"On"`/`"Off"` no longer deserialize.
- `examples/05-virtualmachine.yaml`: `desiredPowerState: "On"` → `desiredPowerState: PoweredOn`.
- `deploy/crds/banlieue.io_virtualmachines.yaml`: regenerated via `make crds`.
- `.wolf/buglog.json`: logged as bug-027 (related to bug-006).
- `.wolf/cerebrum.md`: added Do-Not-Repeat entry for the YAML 1.1 implicit-boolean trap.

### Why
`make kind-deploy-crds` failed with:
```
The CustomResourceDefinition "virtualmachines.banlieue.io" is invalid:
  spec.validation.openAPIV3Schema.properties[spec].properties[desiredPowerState].default:
  Invalid value: "boolean":  in body must be of type string: "boolean"
```
The generated CRD had `default: On` and `enum: - On - Off` (bare, unquoted). The kube apiserver's Go YAML 1.1 parser reads bare `On`/`Off` (regardless of case) as booleans — the classic "Norway problem" variant. So a `string`-typed field had a `boolean`-typed default and the schema was rejected.

Renaming the variants to `PoweredOn`/`PoweredOff` (vSphere/CAPI convention) makes the generated tokens unambiguous strings.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 160 tests passed (144 api after adding the regression test + 2 controller + 14 sdk).
- `make crds` ✅ — regenerated `deploy/crds/`. The `desiredPowerState` block is now:
  ```yaml
  desiredPowerState:
    default: PoweredOn
    enum:
    - PoweredOn
    - PoweredOff
    - Suspended
    type: string
  ```
- `kubectl --context kind-banlieue-dev apply -f deploy/crds/` ✅ — all six CRDs accepted (previously the `VirtualMachine` CRD was rejected).

### Impact
- [x] **Breaking change** — wire format of `PowerState` changes from `On`/`Off` to `PoweredOn`/`PoweredOff`. No production users yet (pre-v1alpha1 scaffolding), but anyone who had a local example with the old form must update.
- [ ] Requires cluster rollout
- [ ] Config change only
- [ ] Documentation only

---

## [2026-05-26] - Phase 1A scaffold: controller, SDK, Makefile, deploy manifests, kind setup

**Author:** Erick Bourgeois

### Added
- `crates/banlieue-provider-sdk/` — new library crate. Modules:
  - `client.rs` — kube::Client builder with explicit read/write timeouts.
  - `error.rs` — typed `Error` enum re-exported as `banlieue_provider_sdk::Error`.
  - `finalizer.rs` — pure `finalizer_list_with` / `finalizer_list_without` plus `ensure_finalizer` / `remove_finalizer` that JSON Merge Patch the K8s object.
  - `ssa.rs` — `server_side_apply` helper + `FIELD_MANAGER_*` constants (controller, vsphere, proxmox, libvirt).
  - `status.rs` — Kubernetes-idiomatic `set_condition` (upsert, sort, transition-time semantics) + `is_condition_true` + `find_condition`.
  - `reconciler.rs` — `requeue_default` / `requeue_on_error` / `requeue_long` / `no_requeue` helpers around `kube::runtime::controller::Action`.
- `crates/banlieue-controller/` — new binary crate. Phase 1A MVP scope: watches `VirtualMachine` resources, ensures finalizer, writes `Scheduled=False reason=SchedulerNotImplemented` and `Ready=False` conditions so users see the controller is wired up. Scheduler / status mirror / migration sub-loop deferred to the next iteration.
  - `main.rs` — clap CLI with `BANLIEUE_*` env-var fallbacks, tracing init (text or json), tiny TCP health server on `:8081`, `Controller::new(...).run(reconcile, error_policy, ctx)` wiring.
  - `reconciler/virtualmachine.rs` — reconcile + error_policy + finalize path + status patch via SSA.
- `Cargo.toml` — added `banlieue-controller` and `banlieue-provider-sdk` to workspace members; pinned `clap = "4"`, `chrono = "0.4"`, `async-trait = "0.1"` in `[workspace.dependencies]`; added `json` feature to `tracing-subscriber`.
- `crates/banlieue-api/src/bin/crdgen.rs` — now accepts `--out-dir <DIR>` and emits one file per CRD (`<group>_<plural>.yaml`, kubebuilder convention) in addition to the existing stdout multi-doc mode.
- `Makefile` — 5-spot-shaped workflow targets. All workflow logic lives here (per project conventions); workflows just call `make`. Notable targets:
  - `make crds` — regenerate `deploy/crds/` from Rust types.
  - `make run-local` — generate CRDs then `cargo run -p banlieue-controller` against the current kube-context.
  - `make kind-up` — one-shot: create kind cluster + apply CRDs. After this you can run the controller locally with `make run-local`.
  - `make kind-load BINARY=<bin>` — cross-compile the binary, build a docker image (host-arch), `kind load docker-image` it.
  - `make kind-deploy-controller` — apply manifests + override the deployment image to the locally-built `KIND_IMAGE`.
  - Per-binary docker targets (`docker-build`, `docker-build-chainguard`, `docker-buildx`, `docker-buildx-chainguard`) parameterised by `BINARY=<name>`.
- `Dockerfile` + `Dockerfile.chainguard` — single per-base Dockerfile parameterised by `BINARY` build-arg, so the same Dockerfile builds every banlieue binary (controller + future providers). Distroless `gcr.io/distroless/cc-debian13:nonroot` and Chainguard `cgr.dev/chainguard/glibc-dynamic:latest` bases, both pinned by digest. Pre-built binaries are copied in from `binaries/<arch>/<binary>` — we never compile inside the container.
- `deploy/crds/` — generated. 6 files, one per CRD.
- `deploy/controller/{namespace,configmap,deployment,service}.yaml` + `deploy/controller/rbac/{serviceaccount,clusterrole,clusterrolebinding}.yaml` — controller deployment manifests. ClusterRole grants full access on `banlieue.io/*` and `infrastructure.banlieue.io/*` (incl. finalizers subresources), read on Secrets, write on Events, full on `ipam.cluster.x-k8s.io/ipaddressclaims+ipaddresses`, and Lease CRUD for leader election. Pod-Security `restricted` profile labels on the namespace.
- `deploy/kind/cluster.yaml` — kind cluster config (single-node, control-plane labels for ingress-ready).

### Why
The roadmap's Phase 1A goal — "a VirtualMachine can go from creation through status.scheduled and status.infrastructureRef populated" — needs a controller binary and an SDK first. This commit lands the **scaffolding** so subsequent iterations can focus on business logic (scheduler, infra creation, status mirror, migration) without re-arguing crate shape, Makefile patterns, RBAC, or Dockerfile conventions. The "ideal" dev loop from the user instructions — `make kind-up` then `cargo run -p banlieue-controller` against the kind cluster — works as of this commit.

### Verification
- `cargo fmt --all -- --check` ✅
- `cargo clippy --all-targets --all-features -- -D warnings` ✅
- `cargo test --all` ✅ — 159 tests passed (143 api + 2 controller + 14 sdk).
- `cargo run -p banlieue-api --bin crdgen --features crdgen -- --out-dir deploy/crds` ✅ — 6 CRD files written.
- `python3 -c "yaml.safe_load_all(...)"` over every YAML in `deploy/crds/` and `deploy/controller/` ✅ — all parse.
- `make help` ✅ — renders the workflow target list with descriptions.

### Impact
- [x] Adds new crates (`banlieue-controller`, `banlieue-provider-sdk`); no API/CRD breaking changes.
- [ ] Breaking change
- [x] Requires cluster rollout (new Deployment manifests; users running an earlier dev build should re-apply `deploy/controller/`).
- [ ] Config change only
- [x] Documentation only — CHANGELOG only here; the next iteration will add `docs/user/` getting-started content and link the Makefile + kind dev loop from `README.md`.

### Deferred to follow-up iterations
- Phase 1A iteration 2: full scheduler (the pure function from the roadmap), provider-infra creation via SSA, status-mirror from `VSphereMachine` → `VirtualMachine`.
- Phase 1A iteration 3: migration sub-loop (recreate-only initially), image watcher, deletion-finalizer cascade waits.
- Phase 1B: `crates/banlieue-provider-vsphere/` with `vim_rs`, capability introspection, `GOVC_*` env-var pass-through for local-vSphere dev.

---

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
