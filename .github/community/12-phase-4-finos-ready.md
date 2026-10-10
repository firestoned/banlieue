# Phase 4 — FINOS-Ready Polish

> **Goal.** Bring banlieue to a quality bar suitable for donation to
> FINOS: docs, governance, CAPI integration, observability,
> security hardening, release engineering.
>
> **Stop condition.** A new user can install banlieue with
> `banlieue bootstrap operator` (or the kustomize bases under `deploy/`),
> follow a quickstart, get a VM running on any of the three providers. The
> project meets FINOS donation requirements.

## Closed 2026-10-09

The repo side of FINOS readiness is done. The five items still unticked below
are not phase 4 work any more; each now lives where its blocker does, and none
is a reason to keep this roadmap open:

| Item | Where it lives now | Blocked on |
|---|---|---|
| Migration-policy e2e scenario (4.5) | roadmap 14, *Tests* | live migration itself |
| Snapshot-schedule e2e scenario (4.5) | roadmap 10, *Definition of done* | the snapshot CRDs |
| Nightly real-backend CI matrix (4.5) | deferred, no roadmap | self-hosted runners |
| `cargo publish` of `banlieue-api` (4.8) | deferred, no roadmap | a crates.io owner, a token, and a decision that the crate is a supported public API |
| Threat model kept current (4.10) | `rules/threat-modeling.md` | nothing: a standing rule, run as ADD's last step |

The donation itself is a FINOS legal and TSC process
(`docs/src/governance/finos-contribution.md`), not a repo task.

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
│   ├── install.md                     // banlieue bootstrap operator / kustomize
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

**Done 2026-10-08 (ADR-0096)**, verified against real CAPI v1.14.3 on
`kind` by `make kind-e2e-capi` (`crates/banlieue-operator/tests/e2e_capi_contract.rs`).

Concrete tasks:

- [x] ~~Stand up CAPI in a `kind` cluster.~~ `make kind-capi-create` +
      `kind-capi-init` (`clusterctl init`, pinned v1.14.3).
- [x] ~~Apply banlieue's CRDs with the `cluster.x-k8s.io/v1beta2: v1alpha1`
      label.~~ Installed by `clusterctl` from a local repository built from
      the tree.
- [x] ~~Apply the aggregated ClusterRole.~~ **Changed:** a dedicated
      `banlieue-capi-infrastructure` role (`deploy/capi/clusterrole-aggregate.yaml`)
      scoped to `infrastructure.banlieue.io`; the label is gone from the
      controller's own role, which had granted CAPI's manager all of
      `banlieue.io`.
- [x] ~~Create a Cluster + Machine pair pointing at a VSphereMachine
      directly.~~
- [x] ~~Verify CAPI's Machine controller sets
      `status.initialization.infrastructureProvisioned=true`.~~ Plus the
      providerID and addresses copied, and delete cascading to the
      infrastructure objects.

`clusterctl` integration:

- [x] ~~Add `metadata.yaml` per provider crate.~~ **One provider, not one per
      backend** (ADR-0096): `config/clusterctl/metadata.yaml`. Releases attach
      it with `infrastructure-components.yaml`, rendered from
      `banlieue bootstrap operator --dry-run` (`make clusterctl-components`).
- [x] ~~Add `clusterctl.yaml` config entries to the docs.~~
      `docs/src/guides/cluster-api.md`: `clusterctl init --infrastructure banlieue`.
- [x] ~~Document the constraints.~~ Same guide: infrastructure role only;
      bootstrap and control-plane providers come from upstream.

## 4.3 Observability

- [x] ~~**Metrics**: controller-runtime-style metrics in every controller
      via `prometheus-client`.~~ **Done 2026-10-08 (ADR-0091)**: `/metrics` on
      `--metrics-port` in every role (cloud-hypervisor included), recorded by
      one SDK wrapper (`banlieue-provider-sdk::runner::run_controller`) that
      every `Controller::new` call site uses:
  - `banlieue_reconcile_total{controller,result}`,
    `banlieue_reconcile_duration_seconds{controller}`,
    `banlieue_reconcile_errors_total{controller,kind}`
  - `banlieue_leader{role}`, `banlieue_provider_failure_domains{provider,kind}`
  - `banlieue_virtualmachines{phase}` **instead of** the per-object
    `banlieue_vm_state{namespace,name,state}` (unbounded cardinality)
  - `banlieue_snapshot_size_bytes` waits for snapshots (roadmap 10)
- [x] ~~**Tracing**: wire OpenTelemetry exporter; spans for reconcile,
      scheduling, backend API calls.~~ **Done 2026-10-08 (ADR-0092)**: OTLP over
      HTTP/protobuf, opt-in via `OTEL_EXPORTER_OTLP_ENDPOINT`; reconcile,
      scheduler and per-operation backend spans (vSphere, Proxmox, libvirt,
      Cloud Hypervisor) that never record credentials or user-data.
- [x] ~~**Structured logs**: JSON output mode behind a CLI flag.~~
      **Done** — `--log-format json|text` (`BANLIEUE_LOG_FORMAT`) on every
      binary, via `banlieue-provider-sdk::bootstrap::init_tracing`.
- [x] ~~**Healthchecks**: ensure they reflect leader-election state.~~
      **Done 2026-10-08 (ADR-0093)**: path-aware `/livez` and `/readyz`
      (404 elsewhere, 400 on malformed). Readiness means "serving or able to
      take over", so a healthy standby stays Ready (a NotReady standby would
      stall rollouts and PDBs); leadership is reported in the `/readyz` body
      and the `banlieue_leader` gauge. A health server that cannot bind is
      fatal.
- [x] ~~Sample Grafana dashboards in `deploy/dashboards/`.~~ **Done**:
      `deploy/dashboards/banlieue-overview.json`, `banlieue_*` metrics only.

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
- [x] ~~**NetworkPolicy** templates restricting controller pods to
      egress only to required endpoints.~~ **Done 2026-10-08 (ADR-0094)**:
      opt-in `deploy/network-policies/`: default deny for every banlieue pod,
      then one allow policy per component (selected by
      `app.kubernetes.io/component`, so operator-spawned providers in any
      namespace are covered) and per import/push Job. Egress is DNS, the API
      server and the component's own backend port; ingress is `metrics` (from
      `banlieue.io/monitoring=true` namespaces) and `health`. Destination
      CIDRs stay a documented site edit. `crates/banlieue-operator/tests/network_policies.rs`
      pins the shape against the labels the operator stamps.
- [x] ~~**Secret rotation**: providers re-read credentials on Secret
      change events.~~ **Decided 2026-10-08, ADR-0095: no Secret watch.**
      Watching needs `list`/`watch` on every Secret in the namespace, where
      each provider can today `get` only its own. Credentials are read per
      reconcile and no client outlives one, so a rotation lands within the
      ~30 s requeue. That bound is now a contract: per-provider tests pin
      the production client factories as stateless, and the vSphere, Proxmox
      and libvirt guides document the rotate-then-revoke procedure.
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
  - [ ] Migration policy: Automatic + Manual paths. **Moved to roadmap 14**
        (2026-10-09); only the `Recreate` placeholder exists.
  - [ ] Snapshot schedule: cron firings + retention. **Moved to roadmap 10**
        (2026-10-09); no CRDs yet.
  - [x] Provider lifecycle: install/upgrade/uninstall ProviderClass —
        `e2e_provider_{class,workload,pause}.rs`, `e2e_workload_namespace.rs`,
        `e2e_bootstrap_install.rs` (roadmap 11).
  - [x] ~~CAPI integration: Machine + VSphereMachine pair~~:
        `e2e_capi_contract.rs` (`make kind-e2e-capi`), §4.2 above.
- [ ] CI matrix per backend; nightly runs against real vCenter/Proxmox
      where possible (self-hosted runners). **Still open** — `e2e.yaml`
      fans the `kind` suites out one job each, but every backend-touching
      suite is `#[ignore]`d and local-only; there are no self-hosted
      runners. **Deferred at close (2026-10-09)** until runners exist.

## 4.6 ~~Helm chart~~: out of scope

**Removed 2026-10-08. banlieue does not ship a Helm chart, now or later.**
The install paths are `banlieue bootstrap operator` (ADR-0013), which
installs the operator and lets `ProviderClass` resources bring up each
backend, and the kustomize bases under `deploy/`. Neither needs a templating
layer, and CRD upgrades stay a plain `kubectl apply` of `deploy/crds/`.

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

- [x] ~~Conventional commit history; use `git-cliff` or similar to
      auto-generate CHANGELOG.~~ **Done 2026-10-08**: `cliff.toml`;
      `make changelog` (root `CHANGELOG.md`, published on the docs site) and
      `make release-notes`, which the release workflow appends to each GitHub
      Release body. git-cliff is installed in CI by `make git-cliff-install`,
      pinned and SHA-512 verified.
- [ ] Semantic version tags `vX.Y.Z` trigger GH Actions:
      - cargo publish (only crates we want to publish; probably
        skip the provider binaries and only publish `banlieue-api`).
        **Still open**, the one remaining sub-item: it needs a crates.io
        owner and a publish token, and a decision on whether `banlieue-api`
        is a supported public crate. **Deferred at close (2026-10-09).**
      - ~~container image build/sign/push~~ **Done** (`build.yaml`, ADR-0006).
      - ~~CRD YAMLs attached to GH release~~ **Done** (`deploy-manifests.tar.gz`).
      - ~~changelog entry~~ **Done 2026-10-08** (`make release-notes` in the
        release body).
- [x] ~~Branch protection on `main`: PR with signed-off commits,
      passing CI required.~~ **Done 2026-09-19** — ruleset `main` with
      required checks; commit signatures are verified in CI by
      `firestoned/github-actions/security/verify-signed-commits`. See
      roadmap 16, which also notes the one-time ruleset edit still needed
      to require the new aggregator context.
- [x] ~~Backport policy for `v1.x` once we hit GA.~~ **Done**: written
      down ahead of GA in `GOVERNANCE.md` ("Releases and support"): from 1.0
      the latest minor gets fixes and the previous minor gets security fixes
      for three months; fixes land on `main`, then a `release-X.Y` branch.

## 4.9 Governance and FINOS-readiness

FINOS donation checklist (verify current FINOS docs for exact
requirements when ready):

- [x] ~~**LICENSE**: Apache-2.0~~ **Done** — `LICENSE`, and every source
      file carries an SPDX header (enforced in CI).
- [x] ~~**NOTICE**: copyright + attributions~~ **Done**: `NOTICE`.
- [x] ~~**README.md** with a clear "what this is" + quickstart.~~
      **Done** — `README.md`, with the docs site at `docs/src/` behind it.
- [x] ~~**CONTRIBUTING.md** with DCO instructions and dev setup.~~
      **Done**: also on the docs site as Developer → Contributing.
- [x] ~~**CODE_OF_CONDUCT.md** (Contributor Covenant v2.1).~~ **Done**.
- [x] ~~**GOVERNANCE.md** describing maintainers, decision process,
      maintainer addition criteria.~~ **Done**.
- [x] ~~**SECURITY.md** with disclosure email and supported versions.~~
      **Done** — `SECURITY.md`, pointing at GitHub private vulnerability
      reporting rather than an email address.
- [x] ~~**MAINTAINERS.md** listing current maintainers with contact.~~
      **Done** (contact by GitHub handle).
- [x] ~~**DCO** enforced via GitHub app on all commits.~~ **Done, in CI
      instead of an app**: the `✍️ DCO Sign-off` job runs `make dco-check`
      on every pull request and is part of `✅ Required Checks`. The sign-off
      must match the author; GitHub App bots match by name (Dependabot signs
      off as `support@github.com`). `make dco-check-test` covers the rules.
- [x] ~~**OWNERS** files for sub-areas (optional but useful).~~ **Done as
      `.github/CODEOWNERS`** (one owner today; paths split as maintainers
      join, per `GOVERNANCE.md`).
- [x] ~~Project metadata: name, mission statement, charter draft.~~
      **Done**: `docs/src/governance/charter.md` (draft for FINOS review).
- [x] ~~Migration plan from `firestoned/banlieue` to
      `finos/banlieue`.~~ **Done**:
      `docs/src/governance/finos-contribution.md`: repository transfer (not a
      fork), image path from the first release after the move, **keep the
      `banlieue.io` API groups**, docs redirect, keyless-signing identity
      change. The donation itself is a FINOS legal/TSC process, not a repo
      task.

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
- [x] ~~**Reconcile `01-decisions.md`.**~~ **Done 2026-10-08: kept and
      frozen** as the annotated pre-ADR record. It takes no new entries; a new
      decision is an ADR, and an entry is only edited to point at the ADR that
      supersedes it.
- [x] Keep the threat model current: a **full pass** after every implemented
      ADR (`rules/threat-modeling.md`), which is ADD's last step. *A standing
      rule, not a task; it outlives this roadmap.*

## Tasks summary

Tackle in roughly this order, but parallelize:

1. Documentation skeleton (4.1) — start early; write as you go.
2. CAPI integration test (4.2) — validates a core design assumption.
3. Observability (4.3) — operators need this for early adoption.
4. Security hardening (4.4) — required for FINOS.
5. E2E (4.5) — gates everything else.
6. ~~Helm chart (4.6)~~: out of scope, see 4.6.
7. Container images and release engineering (4.7, 4.8).
8. Governance and FINOS submission (4.9).
9. ADRs (4.10).

## Definition of done

- A new operator can install banlieue with `banlieue bootstrap operator`
  and provision a VM in under 30 minutes following the docs.
- E2E test matrix passes on every PR.
- Container images are signed; SBOMs published.
- All FINOS donation checklist items satisfied.
- Project is ready for `finos/banlieue` migration.

## Gotchas

- **Doc drift**: API reference must be generated, not
  hand-maintained. Same for sample manifests in the docs — pull from
  `examples/` via include.
- **CAPI version compatibility**: pin to a specific CAPI version in
  integration tests. v1beta2 stabilization is ongoing.
- **DCO enforcement**: easy to enable, easy to break a contributor
  who didn't sign off. Document loudly in CONTRIBUTING.md.
- **FINOS process**: donation is a multi-step legal and TSC review.
  Engage FINOS staff early to scope it; don't try to surprise-drop
  the donation.
