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

> **Reversed.** This originally said to move `docs/roadmap/` to a private
> folder. Roadmaps are now **checked in** at `.github/community/`, indexed by
> [`ROADMAPS.md`](../../ROADMAPS.md) at the repo root. Do not move or privatise
> them. (The MkDocs scaffold itself already shipped — roadmap 08, Phase 1E —
> so what is left here is content, not tooling.)

Build out:

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
      every controller via `prometheus-client`. **Not started** — every
      binary already accepts `--metrics-port` (`BANLIEUE_METRICS_PORT`),
      but the port is reserved, not served:
  - `banlieue_reconcile_total{controller,result}`
  - `banlieue_reconcile_duration_seconds{controller}`
  - `banlieue_reconcile_errors_total{controller,kind}`
  - `banlieue_provider_failure_domains{provider,kind}`
  - `banlieue_vm_state{namespace,name,state}`
  - `banlieue_snapshot_size_bytes{vm,tier}`
- [ ] **Tracing**: wire OpenTelemetry exporter; spans for reconcile,
      scheduling, backend API calls.
- [x] ~~**Structured logs**: JSON output mode behind a CLI flag.~~
      **Done** — `--log-format json|text` (`BANLIEUE_LOG_FORMAT`) on every
      binary, via `banlieue-provider-sdk::bootstrap::init_tracing`.
- [ ] **Healthchecks**: `/healthz` and `/readyz` already in place
      from Phase 1; ensure they reflect leader-election state.
      **Half done** — `serve_health` is wired into every binary, but it
      answers `200 ok` to any request without inspecting the path or the
      lease, so a non-leader standby reports ready. The reflection half is
      what is left.
- [ ] Sample Grafana dashboards in `deploy/dashboards/`. **Blocked on
      metrics above** — nothing to graph yet.

## 4.4 Security hardening

- [x] ~~**Pod Security Standards**: every Deployment runs as nonroot,
      no privilege escalation, drops all capabilities, read-only
      root FS. Libvirt provider needs an exception documented.~~ **Done**
      — `runAsNonRoot` + `seccompProfile` + `readOnlyRootFilesystem` in
      `deploy/{controller,operator,provider-vsphere,imagebuilder}/deployment.yaml`,
      and the same `SecurityContext` is built into every operator-spawned
      workload (`banlieue-operator/src/workload.rs`). No libvirt exception
      was needed: ADR-0011's own client talks native RPC over mTLS from an
      ordinary pod, so the provider never needs host access.
- [ ] **NetworkPolicy** templates restricting controller pods to
      egress only to required endpoints (vCenter, Proxmox, libvirt
      hosts, the K8s apiserver). **Still open** — nothing under `deploy/`
      ships a `NetworkPolicy`, and the threat model does not yet record the
      gap either; the next full pass should either add the templates or
      put this in its §8 accepted risks with a *Revisit when*.
- [ ] **Secret rotation**: providers re-read credentials on Secret
      change events (already watching, just ensure cache invalidates).
      **Still open** — the premise turned out to be wrong: providers do
      **not** watch Secrets. Credentials are read per reconcile, so a
      rotated Secret is picked up on the next requeue, but a rotation does
      not itself trigger one.
- [x] ~~**cosign-signed images**: keyless signing in CI via
      `cosign sign --keyless`.~~ **Done** — `build.yaml` keyless-signs
      every pushed digest and `cosign attest`s the OpenVEX predicate for
      both image variants.
- [x] ~~**SBOM**: generate SPDX SBOM per image via `cargo-sbom` or
      similar.~~ **Done** — `make sbom` for the source tree plus
      `anchore/sbom-action` per image variant, attached to the release
      (ADR-0006).
- [x] ~~**CVE scanning**: trivy in CI; gating on `HIGH`+.~~ **Done with a
      different scanner** — `grype` against the published digests, fed the
      OpenVEX document so triaged findings do not re-raise (`banlieue-vex`),
      plus `cargo-audit`, `cargo-deny`, Semgrep, CodeQL and ClusterFuzzLite;
      `osv-scanner.toml` mirrors the same suppressions for Scorecard's
      Vulnerabilities check.
      trivy was not adopted; the VEX loop is what makes gating survivable.
- [x] ~~**SECURITY.md** with disclosure policy.~~ **Done** — `SECURITY.md`,
      with private vulnerability reporting.

## 4.5 E2E testing

- [x] ~~**`/e2e/`** directory with Rust-based or shell-based scenarios.~~
      **Done, elsewhere** — ADR-0014 put e2e suites in the crate that owns
      the behaviour (`crates/*/tests/e2e_*.rs`) instead of a top-level
      `/e2e/`, so a suite compiles against the types it exercises. One
      `make kind-e2e-<suite>` target each, runnable individually.
- [x] ~~Use `kind` + a simulated backend (vcsim, Proxmox in a VM, libvirt
      in a VM) per provider.~~ **Partly superseded** — `kind` is the
      cluster (`make kind-e2e`), but the backend half went the other way:
      vcsim cannot exercise the JSON transport we ship (roadmap 05), so
      the backend-touching suites run against a **real** libvirt host
      (`make pool-claim-e2e`, `make libvirt-e2e`) or real vCenter
      (`make vsphere-live-test`), and stay out of CI.
- [ ] Scenarios:
  - [x] Create/read/update/delete VirtualMachine — `e2e_pool_claim.rs`
        (pool → VMs → real domains → claim → release) and
        `e2e_import_pipeline.rs`.
  - [ ] Migration policy: Automatic + Manual paths — roadmap 14; only the
        `Recreate` placeholder exists.
  - [ ] Snapshot schedule: cron firings + retention — roadmap 10; no CRDs
        yet.
  - [x] Provider lifecycle: install/upgrade/uninstall ProviderClass —
        `e2e_provider_{class,workload,pause}.rs`, `e2e_workload_namespace.rs`,
        `e2e_bootstrap_install.rs` (roadmap 11).
  - [ ] CAPI integration: Machine + VSphereMachine pair — §4.2 above,
        not started.
- [ ] CI matrix per backend; nightly runs against real vCenter/Proxmox
      where possible (self-hosted runners). **Still open** — `e2e.yaml`
      fans the `kind` suites out one job each, but every backend-touching
      suite is `#[ignore]`d and local-only; there are no self-hosted
      runners.

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

- [x] ~~Multi-arch (linux/amd64, linux/arm64) via `docker buildx`.~~
      **Done** — `platforms: linux/amd64,linux/arm64` in `build.yaml`.
- [x] ~~Distroless base for controller + provider-vsphere +
      provider-proxmox; thin debian for provider-libvirt.~~ **Done, and the
      debian exception is gone** — ADR-0004's single binary ships as two
      variants, `Dockerfile` (digest-pinned distroless) and
      `Dockerfile.chainguard`. The libvirt provider needed no `virsh` or
      `genisoimage` in the image: ADR-0011 speaks the RPC protocol itself
      and ADR-0054 writes the NoCloud seed ISO in-process.
- [x] ~~Signed and SBOM-attested.~~ **Done** — see §4.4.
- [x] ~~Published to `ghcr.io/firestoned/banlieue-*` until donation.~~
      **Done** — `ghcr.io/firestoned/banlieue`, one repository with a
      variant tag rather than a per-binary repository, because there is one
      binary.

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
- [x] ~~Branch protection on `main`: PR with signed-off commits,
      passing CI required.~~ **Done 2026-09-19** — ruleset `main` with
      required checks; commit signatures are verified in CI by
      `firestoned/github-actions/security/verify-signed-commits`. See
      roadmap 16, which also notes the one-time ruleset edit still needed
      to require the new aggregator context.
- [ ] Backport policy for `v1.x` once we hit GA.

## 4.9 Governance and FINOS-readiness

FINOS donation checklist (verify current FINOS docs for exact
requirements when ready):

- [x] ~~**LICENSE**: Apache-2.0~~ **Done** — `LICENSE`, and every source
      file carries an SPDX header (enforced in CI).
- [ ] **NOTICE**: copyright + attributions
- [x] ~~**README.md** with a clear "what this is" + quickstart.~~
      **Done** — `README.md`, with the docs site at `docs/src/` behind it.
- [ ] **CONTRIBUTING.md** with DCO instructions and dev setup.
- [ ] **CODE_OF_CONDUCT.md** (Contributor Covenant v2.1).
- [ ] **GOVERNANCE.md** describing maintainers, decision process,
      maintainer addition criteria.
- [x] ~~**SECURITY.md** with disclosure email and supported versions.~~
      **Done** — `SECURITY.md`, pointing at GitHub private vulnerability
      reporting rather than an email address.
- [ ] **MAINTAINERS.md** listing current maintainers with contact.
- [ ] **DCO** enforced via GitHub app on all commits.
- [ ] **OWNERS** files for sub-areas (optional but useful).
- [ ] Project metadata: name, mission statement, charter draft.
- [ ] Migration plan from `firestoned/banlieue` to
      `finos/banlieue`: redirect, image republish under new path,
      CRD API group rename considerations (probably keep
      `banlieue.io` to avoid breaking users).

## 4.10 ADRs (Architecture Decision Records) — largely done

**This workstream overtook its own plan.** `docs/adr/` exists with 55 ADRs
(0001–0055; 0043–0049 were reserved by roadmap 17 and are now all Accepted),
each following the standard metadata-bullets / Context / Decision /
Consequences template. ADRs are
already the canonical decision record, and ADD makes writing one **step 1**
of any architecturally significant change, not a Phase 4 cleanup task
(`rules/architecture-driven-development.md`).

The illustrative filenames that were listed here never existed; the real
sequence numbers its decisions as they were actually made.

What remains under this heading:

- [x] ~~**Renumber the two duplicate ADR numbers.**~~ **Done 2026-09-19.**
      `0041` and `0042` had each been used twice. The earlier-dated Accepted
      file kept each number; `0041-vmimage-trusted-boot-uki-support` became
      **0051** and `0042-instant-clone-vmfork-not-supported` became **0052**.
      All ~40 inbound references were disambiguated by sense — the
      imagebuilder-Role meaning of "ADR-0041" and the userdata-authorization
      meaning of "ADR-0042" both stayed put — across the imagebuilder and
      operator crates, RBAC manifests, the CALM model, generated CRDs and API
      docs, examples, and roadmap 17.
- [ ] **Reconcile `01-decisions.md`.** Its entries have been annotated with
      the ADRs that superseded them; decide whether to retire it entirely or
      keep it as the annotated pre-ADR record. Do not delete it silently —
      several entries are still the only statement of their decision.
- [ ] Keep the threat model current: a **full pass** after every implemented
      ADR (`rules/threat-modeling.md`), which is ADD's last step.

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
