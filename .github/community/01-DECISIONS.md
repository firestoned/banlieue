# banlieue — Locked Design Decisions

> Every decision in this file is **locked**. Do not re-litigate during
> implementation. If a decision needs to change, propose an ADR in
> `docs/adr/NNNN-title.md` (Status / Context / Decision / Consequences),
> get it merged, then update the entry here to point at it.
>
> **This file is the pre-ADR record.** It predates `docs/adr/`, and the ADR
> sequence — not this file — is now the canonical decision log
> (`rules/architecture-driven-development.md`). Where an ADR has superseded an
> entry below, the entry has been rewritten to state what is true today and to
> cite the ADR. Entries with no ADR cited are still original decisions that
> nothing has revisited.

## D-001 — Language and toolchain

- **Rust** edition **2024**, MSRV **1.88** (root `Cargo.toml`
  `edition` / `rust-version`). Bumped from the original edition 2021 /
  1.80 to match the kube-rs reference toolchain.
- `rustfmt` defaults, `clippy` clean with `-D warnings`.
- Workspace layout; one crate per logical component. Shared deps are pinned
  once in the root `[workspace.dependencies]` table; member crates use
  `<dep>.workspace = true` rather than re-pinning.

## D-002 — Operator framework

- **`kube-rs`** for client, derive, runtime (Controller, watcher).
- Pinned in the root `[workspace.dependencies]`: `kube = "~4.2"`
  (features `derive`, `client`, `runtime`) and `k8s-openapi = "0.28"`
  (features `latest`, `schemars`). The original `0.96` / `0.23` / `v1_31`
  pins are long superseded — read the root `Cargo.toml` for the current
  values, not this file.
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
- **Libvirt**: **first-party pure-Rust client**, `crates/banlieue-libvirt` —
  libvirt's native RPC protocol (XDR codec, framing, TLS session, streams),
  no C FFI and no `libvirt0` in the image
  ([ADR-0011](../../docs/adr/0011-libvirt-provider-own-client.md),
  [ADR-0050](../../docs/adr/0050-libvirtmachine-domain-lifecycle.md)).
  The original `virt` crate (C FFI) decision is **superseded**; adding it
  now would mean a second libvirt client, `unsafe` boundaries and a C
  library in the supply chain. New procedures go in
  `banlieue-libvirt/src/{rpc,procs}.rs`.

## D-007 — Provider model

- Each backend instance = one `Provider` CR.
  - One vCenter ⇒ one `Provider` of class `vsphere`.
  - One Proxmox cluster ⇒ one `Provider` of class `proxmox`.
  - One libvirtd endpoint ⇒ one `Provider` of class `libvirt`.
- `ProviderClass` **exists** and is reconciled by `banlieue-operator`
  ([ADR-0012](../../docs/adr/0012-providerclass-crd-and-operator-role.md));
  the "deferred to Phase 3 / hardcoded string set" note here is superseded.
  Deployment topology for provider workloads is
  [ADR-0003](../../docs/adr/0003-provider-deployment-topology.md)
  (accepted 2026-07-31, amending its own 2026-05-30 hybrid draft) — which
  also closes **O-003** below.

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

## D-018 — Admission control (ValidatingAdmissionPolicy, not webhooks)

**Superseded** by [ADR-0007](../../docs/adr/0007-admission-policies.md).
There are no admission webhooks and no webhook binaries — no serving
certs, no cert-manager dependency, no extra Deployment to keep alive, and
no failure mode where a down webhook blocks every write.

- Validation is **CEL `ValidatingAdmissionPolicy`**, one policy file per
  concern under `deploy/admission/`: immutability
  (`provider-immutability.yaml`, `virtualmachine-immutability.yaml`),
  connection/CA shape (`provider-connection.yaml`,
  `provider-cabundle-source.yaml`), authorization
  (`provider-credentialsref-authorization.yaml`,
  `virtualmachine-userdata-authorization.yaml`), import-source and
  `ProviderClass` guardrails. Splitting by concern lets a rule that needs a
  newer apiserver capability (e.g. the `authorizer` CEL variable) be gated
  on its own without holding back the rest.
- **Defaulting is in the schema, not in a mutating webhook** — `serde`
  defaults on the Rust types flow into the generated CRDs' `default:`,
  so `firmware` / `migrationPolicy` / `desiredPowerState` are filled in by
  the apiserver.
- SSA dry-run escape hatch on all `*Template` CRDs (CAPI ClusterClass
  requirement) still applies.

## D-019 — Container images

- **One image, not one per role** — the single `banlieue` binary
  ([ADR-0004](../../docs/adr/0004-single-binary-subcommand-dispatch.md))
  means one image whose Deployment picks the role via container `args`
  (`["controller"]`, `["provider","vsphere"]`).
- Binary is built in CI (`firestoned/github-actions` `rust/build-binary`)
  and copied in; the `Dockerfile` itself has no Rust builder stage.
- Final image: **`gcr.io/distroless/cc-debian13:nonroot`**, digest-pinned on
  a literal `FROM` line so Dependabot's Docker parser can see it. The
  libvirt provider needs **no** `libvirt0` and no thin-debian base — its
  client is pure Rust (D-006). `Dockerfile.chainguard` is the alternate base.
- `USER nonroot`; `ENTRYPOINT ["/usr/local/bin/banlieue"]`.
- Multi-arch: linux/amd64 and linux/arm64.
- Signed with cosign; SBOM + SLSA provenance in the release pipeline
  ([ADR-0006](../../docs/adr/0006-release-and-supply-chain-pipeline.md)).
- Hosted at `ghcr.io/firestoned/banlieue` until FINOS donation.

## D-020 — License and governance

- **Apache-2.0** throughout.
- DCO sign-off enforced on every commit (FINOS requirement).
- Code of Conduct: Contributor Covenant v2.1.
- Governance file added in Phase 4.

## D-021 — Naming

- Github org: **`firestoned`**.
- Repo: **`firestoned/banlieue`**.
- Crate prefix: **`banlieue-`**.
- Container image: **`ghcr.io/firestoned/banlieue`** (singular — one image
  for every role, per D-019 / ADR-0004).
- API group base domain: **`banlieue.io`**.

## D-022 — Test strategy

- **TDD is mandatory** — failing test first, then the minimum
  implementation (`rules/testing.md`, `tdd-workflow` skill).
- **Unit tests live in a separate `_tests.rs` file, never embedded in the
  source file.** `src/foo.rs` declares `#[cfg(test)] mod foo_tests;` at the
  bottom; `src/foo_tests.rs` holds `#[cfg(test)] mod tests { use
  super::super::*; … }`. This supersedes the original "adjacent
  `#[cfg(test)] mod tests`" wording — `rules/testing.md` is the binding
  statement of this rule and the tree has 60+ `_tests.rs` files following it.
- Integration tests in each crate's `tests/` directory; reconcilers
  exercise against a fake client (`FakeClient`).
- E2E on a `kind` cluster
  ([ADR-0014](../../docs/adr/0014-kind-e2e-operator-contract.md)).
- Coverage target: 70% for libraries, no hard target for binaries.

## D-023 — Availability zones: local compute + local storage

Locks the principle from
[`03-availability-zones-and-datastore-tiering.md`](03-availability-zones-and-datastore-tiering.md),
which asked for exactly this entry as its first roadmap action.

- **An Availability Zone = a unit of local compute plus its local
  datastore(s), which fail together and nothing else.** In CRD terms, a
  `Provider.status.failureDomains[]` entry.
- **Uniform tiering, not per-datastore tiering.** A tier is a property of the
  zone, not of an individual datastore or host. There is no "gold datastore"
  next to a "silver datastore" in the same zone.
- **Local datastores only, always.** A VM's storage must be local to the
  compute it runs on, and *all* of a VM's disks resolve to the same
  placement's local datastore(s).
- **No cross-datastore access, ever** — it couples independent failure units,
  puts a fabric hop in the storage data path, and breaks the AZ boundary.
- **Spread evenly for availability.** Even spread across many small
  independent zones *is* the availability mechanism; placement never prefers
  a "better" datastore over even spread.
- The provider must refuse a non-local plan with a status error, never
  silently satisfy it.

Storage/network classes in `Provider.spec.capabilities` therefore express
*tier intent*, resolved to a concrete local target **per failure domain**
([ADR-0030](../../docs/adr/0030-per-zone-capability-targets.md)) — the same
class name resolves to a different physical datastore in each zone.

Open sub-questions (per-backend locality models, capacity-aware spread)
remain in the roadmap doc; they refine this decision, they do not reopen it.

## Open decisions

| ID | Topic | Resolution deadline |
|---|---|---|
| O-001 | Proxmox Rust client choice | Start of Phase 1C |
| O-002 | Live migration semantics across providers | Phase 2 design review |
| ~~O-003~~ | ~~Multi-tenancy boundaries within a single Provider~~ | **Closed** by [ADR-0003](../../docs/adr/0003-provider-deployment-topology.md) (accepted 2026-07-31) + [ADR-0016](../../docs/adr/0016-imagebuild-namespace-isolation.md) |
| O-004 | CAPI `clusterctl` integration shape | Phase 4 |
