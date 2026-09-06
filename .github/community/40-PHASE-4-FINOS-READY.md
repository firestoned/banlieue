# Phase 4 — FINOS-Ready Polish

> **Goal.** Bring banlieue to a quality bar suitable for donation to
> FINOS: docs, governance, CAPI integration, observability,
> security hardening, release engineering.
>
> **Stop condition.** A new user can install banlieue from a Helm
> chart, follow a quickstart, get a VM running on any of the three
> providers. The project meets FINOS donation requirements.

## Preconditions

- Phases 1–3 stable across at least the vSphere and Proxmox
  providers.
- Some real users have run banlieue in test environments and given
  feedback.

## Workstreams

These can largely run in parallel; they're grouped here rather than
sequenced.

## 4.1 Documentation

Move `docs/roadmap/` to a private folder (these become internal
contributor docs) and build out:

```
docs/
├── user/
│   ├── getting-started.md
│   ├── concepts.md                    // Provider/VMClass/VMImage/VM
│   ├── how-to/
│   │   ├── multi-vcenter-setup.md
│   │   ├── snapshot-schedules.md
│   │   ├── migration-policies.md
│   │   ├── custom-ipam.md
│   │   └── cloud-init-recipes.md
│   ├── reference/
│   │   ├── crd-reference.md           // generated from CRDs
│   │   └── conditions.md              // every condition type/reason
│   └── troubleshooting.md
├── operator/
│   ├── install.md                     // Helm chart usage
│   ├── upgrade.md
│   ├── observability.md
│   ├── security.md
│   └── multi-tenancy.md
├── developer/
│   ├── architecture.md                // the decisions doc, polished
│   ├── adding-a-provider.md           // step-by-step from a new CRD up
│   ├── capi-integration.md
│   └── testing.md
└── adr/                               // architecture decision records
```

Generator: write a small `xtask` that emits `crd-reference.md` from
the CRD YAMLs. Don't hand-maintain.

## 4.2 CAPI integration

Test banlieue's provider CRDs as **CAPI infrastructure providers**.
Without code changes, a `clusterv1.Machine` referencing a
`VSphereMachine` should work.

Concrete tasks:

- [ ] Stand up CAPI in a `kind` cluster.
- [ ] Apply banlieue's CRDs with the
      `cluster.x-k8s.io/v1beta2: v1alpha1` label.
- [ ] Apply the aggregated ClusterRole
      (`cluster.x-k8s.io/aggregate-to-manager: "true"`).
- [ ] Create a Cluster + Machine pair pointing at a VSphereMachine
      directly (bypass banlieue's VirtualMachine).
- [ ] Verify CAPI's Machine controller sets
      `status.initialization.infrastructureProvisioned=true` once
      the VSphereMachine reports it.

`clusterctl` integration:

- [ ] Add `metadata.yaml` per provider crate at `config/clusterctl/`.
- [ ] Add `clusterctl.yaml` config entries to the docs so users can
      `clusterctl init --infrastructure banlieue-vsphere`.
- [ ] Document the constraints (banlieue providers are designed for
      arbitrary VMs, so the "infrastructure" provider role works but
      the bootstrap/control-plane providers come from upstream).

## 4.3 Observability

- [ ] **Metrics**: implement `controller-runtime`-style metrics in
      every controller via `prometheus-client`:
  - `banlieue_reconcile_total{controller,result}`
  - `banlieue_reconcile_duration_seconds{controller}`
  - `banlieue_reconcile_errors_total{controller,kind}`
  - `banlieue_provider_failure_domains{provider,kind}`
  - `banlieue_vm_state{namespace,name,state}`
  - `banlieue_snapshot_size_bytes{vm,tier}`
- [ ] **Tracing**: wire OpenTelemetry exporter; spans for reconcile,
      scheduling, backend API calls.
- [ ] **Structured logs**: JSON output mode behind a CLI flag.
- [ ] **Healthchecks**: `/healthz` and `/readyz` already in place
      from Phase 1; ensure they reflect leader-election state.
- [ ] Sample Grafana dashboards in `deploy/dashboards/`.

## 4.4 Security hardening

- [ ] **Pod Security Standards**: every Deployment runs as nonroot,
      no privilege escalation, drops all capabilities, read-only
      root FS. Libvirt provider needs an exception documented.
- [ ] **NetworkPolicy** templates restricting controller pods to
      egress only to required endpoints (vCenter, Proxmox, libvirt
      hosts, the K8s apiserver).
- [ ] **Secret rotation**: providers re-read credentials on Secret
      change events (already watching, just ensure cache invalidates).
- [ ] **cosign-signed images**: keyless signing in CI via
      `cosign sign --keyless`.
- [ ] **SBOM**: generate SPDX SBOM per image via `cargo-sbom` or
      similar.
- [ ] **CVE scanning**: trivy in CI; gating on `HIGH`+.
- [ ] **SECURITY.md** with disclosure policy.

## 4.5 E2E testing

- [ ] **`/e2e/`** directory with Rust-based or shell-based scenarios.
- [ ] Use `kind` + a simulated backend (vcsim, Proxmox in a VM, libvirt
      in a VM) per provider.
- [ ] Scenarios:
  - Create/read/update/delete VirtualMachine
  - Migration policy: Automatic + Manual paths
  - Snapshot schedule: cron firings + retention
  - Provider lifecycle: install/upgrade/uninstall ProviderClass
  - CAPI integration: Machine + VSphereMachine pair
- [ ] CI matrix per backend; nightly runs against real vCenter/Proxmox
      where possible (self-hosted runners).

## 4.6 Helm chart

`deploy/helm/banlieue/`:

```yaml
# values.yaml
image:
  repository: ghcr.io/firestoned/banlieue-controller
  tag: ""           # default = chart appVersion
  pullPolicy: IfNotPresent

leaderElection:
  enabled: true
  namespace: banlieue-system

webhook:
  enabled: true
  certManager: true

providerClasses:
  vsphere:
    enabled: true
    image: ghcr.io/firestoned/banlieue-provider-vsphere
    replicas: 2
  proxmox:
    enabled: true
    image: ghcr.io/firestoned/banlieue-provider-proxmox
    replicas: 2
  libvirt:
    enabled: false
    image: ghcr.io/firestoned/banlieue-provider-libvirt
    replicas: 1

monitoring:
  serviceMonitor:
    enabled: false
  grafanaDashboards:
    enabled: false
```

Tasks:

- [ ] Chart skeleton with CRDs, controller Deployment, webhook,
      ProviderClasses gated by `values.providerClasses.*.enabled`.
- [ ] Lint with `helm lint` and `kubeval`.
- [ ] Render fixtures in CI to catch regressions.

## 4.7 Container images

- [ ] Multi-arch (linux/amd64, linux/arm64) via `docker buildx`.
- [ ] Distroless base for controller + provider-vsphere +
      provider-proxmox; thin debian for provider-libvirt.
- [ ] Signed and SBOM-attested.
- [ ] Published to `ghcr.io/firestoned/banlieue-*` until donation.

## 4.8 Release engineering

- [ ] Conventional commit history; use `git-cliff` or similar to
      auto-generate CHANGELOG.
- [ ] Semantic version tags `vX.Y.Z` trigger GH Actions:
      - cargo publish (only crates we want to publish; probably
        skip the provider binaries and only publish `banlieue-api`)
      - container image build/sign/push
      - helm chart package + push (chartmuseum or GH pages)
      - CRD YAMLs attached to GH release
      - changelog entry
- [ ] Branch protection on `main`: PR with signed-off commits,
      passing CI required.
- [ ] Backport policy for `v1.x` once we hit GA.

## 4.9 Governance and FINOS-readiness

FINOS donation checklist (verify current FINOS docs for exact
requirements when ready):

- [ ] **LICENSE**: Apache-2.0 ✓
- [ ] **NOTICE**: copyright + attributions
- [ ] **README.md** with a clear "what this is" + quickstart.
- [ ] **CONTRIBUTING.md** with DCO instructions and dev setup.
- [ ] **CODE_OF_CONDUCT.md** (Contributor Covenant v2.1).
- [ ] **GOVERNANCE.md** describing maintainers, decision process,
      maintainer addition criteria.
- [ ] **SECURITY.md** with disclosure email and supported versions.
- [ ] **MAINTAINERS.md** listing current maintainers with contact.
- [ ] **DCO** enforced via GitHub app on all commits.
- [ ] **OWNERS** files for sub-areas (optional but useful).
- [ ] Project metadata: name, mission statement, charter draft.
- [ ] Migration plan from `firestoned/banlieue` to
      `finos/banlieue`: redirect, image republish under new path,
      CRD API group rename considerations (probably keep
      `banlieue.io` to avoid breaking users).

## 4.10 ADRs (Architecture Decision Records)

Replace `01-DECISIONS.md` with a numbered ADR sequence:

```
docs/adr/
├── 0001-language-rust.md
├── 0002-no-rpc-providers.md
├── 0003-capi-contract-shape.md
├── 0004-explicit-capability-mapping.md
├── 0005-non-sticky-placement.md
├── 0006-pluggable-ipam.md
├── 0007-cluster-vs-namespaced-scopes.md
├── 0008-gfs-snapshot-retention.md
└── ...
```

Each ADR follows the standard template (context / decision /
consequences). They become the canonical record once FINOS-donated.

## Tasks summary

Tackle in roughly this order, but parallelize:

1. Documentation skeleton (4.1) — start early; write as you go.
2. CAPI integration test (4.2) — validates a core design assumption.
3. Observability (4.3) — operators need this for early adoption.
4. Security hardening (4.4) — required for FINOS.
5. E2E (4.5) — gates everything else.
6. Helm chart (4.6) — required for usability.
7. Container images and release engineering (4.7, 4.8).
8. Governance and FINOS submission (4.9).
9. ADRs (4.10).

## Definition of done

- A new operator can install banlieue via Helm and provision a VM in
  under 30 minutes following the docs.
- E2E test matrix passes on every PR.
- Container images are signed; SBOMs published.
- All FINOS donation checklist items satisfied.
- Project is ready for `finos/banlieue` migration.

## Gotchas

- **Doc drift**: API reference must be generated, not
  hand-maintained. Same for sample manifests in the docs — pull from
  `examples/` via include.
- **Helm + CRDs**: there's a long-standing Helm + CRD lifecycle
  pain. Recommended approach: ship CRDs in the chart's `crds/`
  directory (which doesn't manage updates), and document
  `kubectl apply -f` for upgrades. Or, more invasively, use a CRD
  controller — overkill for our scope.
- **CAPI version compatibility**: pin to a specific CAPI version in
  integration tests. v1beta2 stabilization is ongoing.
- **DCO enforcement**: easy to enable, easy to break a contributor
  who didn't sign off. Document loudly in CONTRIBUTING.md.
- **FINOS process**: donation is a multi-step legal and TSC review.
  Engage FINOS staff early to scope it; don't try to surprise-drop
  the donation.
