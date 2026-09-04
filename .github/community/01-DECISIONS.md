# banlieue — Locked Design Decisions

> Every decision in this file is **locked**. Do not re-litigate during
> implementation. If a decision needs to change, propose an ADR in
> `docs/design/adr-NNN-*.md`, get it merged, then update this file.

## D-001 — Language and toolchain

- **Rust** edition 2021, MSRV 1.80.
- `rustfmt` defaults, `clippy` clean with `-D warnings`.
- Workspace layout; one crate per logical component.

## D-002 — Operator framework

- **`kube-rs`** for client, derive, runtime (Controller, watcher).
- Pin to `kube = "0.96"`, `k8s-openapi = "0.23"` with `v1_31` features.
- No higher-level frameworks (no shuttle, no operator-framework
  shimming).

## D-003 — Provider communication

- **CRD-to-CRD only.** No gRPC, no REST, no message bus between the
  main controller and providers.
- The main controller creates an `infrastructure.banlieue.io` CR; the
  relevant provider controller reconciles it. Status flows back via
  the K8s API.

## D-004 — API groups

- `banlieue.io/v1alpha1` — user-facing.
- `infrastructure.banlieue.io/v1alpha1` — provider-specific infra CRDs.

## D-005 — CAPI contract compliance

- Every `infrastructure.banlieue.io` machine CRD MUST satisfy the
  **CAPI v1beta2 InfraMachine contract**.
- The CRD MUST carry the label `cluster.x-k8s.io/v1beta2: v1alpha1`
  (applied at deploy time via kustomize).
- Aggregated RBAC label `cluster.x-k8s.io/aggregate-to-manager: "true"`
  on the ClusterRole granting CAPI access to our group.
- Status uses `status.initialization.provisioned`, not the deprecated
  v1beta1 `status.ready`.
- Status uses `metav1.Condition`, not the deprecated CAPI condition type.
- No `failureReason` / `failureMessage` — terminal failures are
  expressed as conditions.

## D-006 — Backend clients

- **vSphere**: `vim_rs` (VI-JSON API). Isolate in its own crate due to
  multi-minute compile times. Pin to exact version; expect breaking
  changes pre-1.0.
- **Proxmox**: TBD. Decision in Phase 1C. Default: roll a thin HTTP
  client with `reqwest`. Swap if a mature crate appears.
- **Libvirt**: `virt` crate (C FFI). Wrap behind a safe trait;
  consider `qemu+ssh` transports for remote.

## D-007 — Provider model

- Each backend instance = one `Provider` CR.
  - One vCenter ⇒ one `Provider` of class `vsphere`.
  - One Proxmox cluster ⇒ one `Provider` of class `proxmox`.
  - One libvirtd endpoint ⇒ one `Provider` of class `libvirt`.
- `ProviderClass` CRD is deferred to Phase 3. Until then,
  `Provider.spec.providerClassRef.name` is a string from a hardcoded
  set: `vsphere`, `proxmox`, `libvirt`.

## D-008 — Capability advertisement

- **Explicit name mapping in `Provider.spec.capabilities`**. The admin
  lists every storage class and network class with its concrete backend
  target.
- The provider controller verifies these on reconcile and reports
  per-failure-domain availability in `Provider.status.failureDomains[].attributes`.
- Providers that natively lack a concept (e.g. libvirt + storage tiers)
  participate via admin-supplied mappings.

## D-009 — Scheduling and placement

- **Non-sticky.** Scheduler runs on every reconcile. If the current
  placement no longer satisfies the spec, the `PlacementValid=False`
  condition is set.
- The `VirtualMachine.spec.migrationPolicy` field controls the action:
  - `Automatic` (default): migrate (live where possible, else recreate).
  - `Manual`: surface condition; act only when the annotation
    `banlieue.io/migrate=true` is set.
  - `Never`: do nothing.
- Scheduler runs in the main controller, **not** in providers.

## D-010 — Storage and network classes

- Abstract names (strings). Examples: `gold`, `silver`, `standard`,
  `prod`, `mgmt`.
- A `VMClass` requests classes by name.
- A `Provider` advertises classes and maps them to concrete backend
  targets.
- The scheduler matches requested classes against the candidate
  failure domain's `availableStorageClasses` / `availableNetworkClasses`.

## D-011 — IPAM

- **Pluggable** via `TypedObjectReference` (apiGroup, kind, name).
- **CAPI IPAM contract is the default**: pool refs may point at
  `ipam.cluster.x-k8s.io/IPAddressClaim` flows out of the box.
- Static IPs and DHCP are also first-class (`IpamSpec` enum).
- Banlieue-native pool CRDs may come later; they would not change the
  schema.

## D-012 — CRD scope

| CRD | Scope |
|---|---|
| `Provider` | Namespaced |
| `VirtualMachine` | Namespaced |
| `VMClass` | **Cluster-scoped** (like `StorageClass`) |
| `VMImage` | **Cluster-scoped** |
| `VSphereMachine` (and other infra) | Namespaced |
| `VSphereMachineTemplate` (and others) | Namespaced |

## D-013 — Snapshots

- Two CRDs: `VirtualMachineSnapshot` (single point-in-time) and
  `SnapshotSchedule` (recurring).
- `SnapshotSchedule` uses **GFS retention**: a list of tiers, each with
  a cron schedule and a `keep` count.
- Each snapshot is labeled with the tier that produced it; retention
  enforcement is per-tier.
- Provider controllers implement the actual snapshot take / delete.
  The snapshot controller orchestrates scheduling and pruning.

## D-014 — Image management

- `VMImage` is cluster-scoped, with **per-provider source mappings**.
- Each provider may have a different ref (template name, VMID, file path).
- `importFrom` URL is best-effort import for providers that can pull;
  others require the admin to pre-stage the artifact.
- The image controller maintains
  `VMImage.status.perProvider[i].ready` and gates scheduling.

## D-015 — Migration

- Two modes: **live migration** (where supported by provider class and
  source/target failure domain) and **recreate** (destroy + recreate
  on new placement).
- vSphere: vMotion within / across clusters with shared storage.
- Proxmox: live migration with shared storage; offline migration via
  `qm move`.
- Libvirt: no live migration support in v1; `Never` policy enforced or
  controller falls back to recreate with warning.
- The provider declares its migration capabilities in
  `Provider.status.failureDomains[].attributes.features` (well-known:
  `liveMigration`, `crossClusterMigration`).

## D-016 — Error handling

- Libraries use `thiserror` to define typed errors.
- Application code uses `Result<T, MyTypedError>`. **No `anyhow`** in
  library crates. Binaries may use `eyre` at the top level for nicer
  panics, but reconcilers always return typed errors.

## D-017 — Logging and observability

- `tracing` for all logs and spans.
- Every reconcile is a span with `kind`, `namespace`, `name`,
  `resource_version`.
- Phase 4 adds Prometheus metrics (controller-runtime style) and
  OpenTelemetry traces.

## D-018 — Webhook validation

- Validation webhook for every CRD enforces:
  - Immutable fields stay immutable
  - References point at valid kinds
  - Capability strings used in `VMClass` must look syntactically valid
    (the existence check happens at scheduling time)
- Defaulting webhook fills in `firmware`, `migrationPolicy`,
  `desiredPowerState` etc. where omitted.
- SSA dry-run escape hatch on all `*Template` CRDs (CAPI ClusterClass
  requirement).

## D-019 — Container images

- Multi-stage builds.
- Final image: `gcr.io/distroless/cc-debian12:nonroot` for most;
  libvirt provider needs `libvirt0` and uses a thin debian image.
- Multi-arch: linux/amd64 and linux/arm64.
- Signed with cosign (Phase 4).
- Hosted at `ghcr.io/firestoned/banlieue-*` until FINOS donation.

## D-020 — License and governance

- **Apache-2.0** throughout.
- DCO sign-off enforced on every commit (FINOS requirement).
- Code of Conduct: Contributor Covenant v2.1.
- Governance file added in Phase 4.

## D-021 — Naming

- Github org: **`firestoned`**.
- Repo: **`firestoned/banlieue`**.
- Crate prefix: **`banlieue-`**.
- Container images: **`ghcr.io/firestoned/banlieue-*`**.
- API group base domain: **`banlieue.io`**.

## D-022 — Test strategy

- Unit tests adjacent to code in `#[cfg(test)] mod tests`.
- Integration tests in each crate's `tests/` directory; reconcilers
  exercise against a fake client.
- E2E tests in `/e2e` using a `kind` cluster with the three providers
  in dry-run / mock modes. Phase 4.
- Coverage target: 70% for libraries, no hard target for binaries.

## Open decisions

| ID | Topic | Resolution deadline |
|---|---|---|
| O-001 | Proxmox Rust client choice | Start of Phase 1C |
| O-002 | Live migration semantics across providers | Phase 2 design review |
| O-003 | Multi-tenancy boundaries within a single Provider | Phase 3 |
| O-004 | CAPI `clusterctl` integration shape | Phase 4 |
