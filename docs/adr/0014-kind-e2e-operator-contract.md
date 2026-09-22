<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0014 — kind-based e2e: test the operator contract, not backend connectivity

- **Status:** Accepted
- **Date:** 2026-07-31
- **Amended:** 2026-09-20 (suite topology — one test binary split into several,
  one CI job each)
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0003 (per-instance topology — the contract under test);
  ADR-0012 (operator role); ADR-0013 (bootstrap CLI); ADR-0006 (release
  pipeline); `rules/github-workflows.md` (workflows delegate to Makefile);
  `rules/testing.md` (integration tests live in `tests/`).

## Context

`banlieue-operator` ships with 78 unit tests, all of which assert on objects
built in memory. None of them prove the thing that actually matters: that
applying a `Provider` to a **real API server** produces a working set of
objects, that server-side apply is accepted, that owner references garbage-
collect what they should, and that the finalizer removes what they cannot.

Those are exactly the failures unit tests cannot reach. A Deployment whose
`spec.selector` does not match its pod template, a `resourceNames` rule the
apiserver rejects, an `ownerReference` with a bad `apiVersion`, an SSA patch
that silently drops a field because the CRD schema disagrees with the Rust
type — every one of these passes `cargo test` and fails on first contact with
Kubernetes.

The obstacle is that a provider workload's *purpose* is to reach a backend, and
CI has no vCenter, no Proxmox, and no libvirt host.

Options considered:

1. **No e2e.** Rely on unit tests and manual `make kind-deploy-operator`.
   Cheapest, and leaves the entire apiserver-contract surface untested.
2. **Full-stack e2e with a simulated backend.** Run `vcsim` in the cluster and
   assert a Provider reaches `Ready` with populated `failureDomains`. Highest
   fidelity; couples every operator test run to the vSphere provider's
   correctness and to vcsim's fidelity, and tests two components at once so a
   failure does not localise.
3. **Operator-contract e2e.** Assert everything the operator is responsible for
   — object creation, shape, ownership, status, GC, pause — and deliberately
   assert **nothing** about whether the spawned provider can reach a backend.

## Decision

Adopt **option 3**, with option 2 recorded as a separate follow-up suite.

This works because of ADR-0012's boundary: **the operator never talks to a
backend.** It reads CRs and writes workloads through the Kubernetes API. Its
entire contract is therefore observable in a bare kind cluster with no backend
of any kind. Nothing is being skipped or stubbed — the operator's real,
complete responsibility is exercised against a real apiserver.

### The spawned pod is expected to be unhealthy, and that is not a failure

A `Provider` in CI points at an endpoint that does not resolve. Its spawned
provider pod will start, fail to reach the backend, and report NotReady — and
`status.workload.readyReplicas` will stay `0`.

The e2e **must not** wait for that pod to become Ready, and must not treat its
unhealthiness as a failure. Asserting on it would be asserting on the vSphere
provider's behaviour and on CI's DNS, neither of which this suite is about.
What is asserted is that the Deployment *exists, is correctly shaped, and is
owned* — the operator's actual output.

This is the single most important thing to understand before editing the suite:
a future contributor "fixing" the e2e by waiting for pod readiness will produce
a permanently red, permanently unfixable job.

### What is asserted

Split across two suites, because the install path and the reconcile contract
fail independently.

**`e2e_bootstrap_install.rs`** — asserts the *installer's* output, in a cluster
where nothing else created those objects:

| Area | Assertion |
| --- | --- |
| CRDs | Every CRD exists **and** reaches `Established` — merely existing is not enough, since an unestablished CRD rejects its own CRs |
| Control plane | Controller and operator Deployments, ClusterRoles and ClusterRoleBindings exist, with binding subjects following `--namespace` |
| Shared provider role | `banlieue-provider-<backend>` exists for every compiled-in backend — the operator binds it but cannot create it |
| Seeding | One ProviderClass per backend, with a non-`latest` image tag |

The *reconcile* contract — everything a `Provider` CR makes the operator do —
is asserted across four more binaries:

| Suite | Assertion |
| --- | --- |
| **`e2e_provider_workload.rs`** | **Creation:** a `Provider` yields Deployment + ServiceAccount + Role + RoleBinding + ClusterRoleBinding, all under the derived name. **Shape:** Deployment args carry `provider <backend> --provider-name <name>`; selector matches the pod template. **Least privilege:** the Role grants `get` on exactly the credentials Secret via `resourceNames`, and never `list`/`watch` on Secrets. **Ownership:** namespaced objects carry a controlling `ownerReference`; the ClusterRoleBinding carries none. **Status ownership:** `metadata.managedFields` shows the operator owns `status.workload` and **not** `status.conditions`. **Deletion:** deleting the Provider removes all five objects — GC for the owned four, finalizer for the ClusterRoleBinding. Run once per backend (vsphere, libvirt) |
| **`e2e_provider_pause.rs`** | `spec.paused` stops reconciliation — then unpausing produces the workload, so the absence assertion is not vacuous. Covered for both `Provider` and `ProviderClass` |
| **`e2e_workload_namespace.rs`** | With `workloadNamespace` set, the Deployment and ServiceAccount are left **unowned** (a cross-namespace owner is invalid) and the finalizer deletes them; the Role stays with the Secret, owned |
| **`e2e_provider_class.rs`** | Swapping `providerClassRef` prunes the superseded workload, including the ClusterRoleBinding GC cannot reach. An image edit reaches existing workloads inside a budget well below the periodic requeue, proving the `ProviderClass` watch — not the timer — delivered it |

### Amendment (2026-09-20): one binary per contract, one CI job per binary

The reconcile assertions above originally lived in a single
`e2e_provider_lifecycle.rs` — seven tests, 1,291 lines, one `make kind-e2e`
target, one CI job. That shape had three costs:

- **A failure did not localise.** The job reported "e2e failed"; which of seven
  independent contracts broke took reading the log.
- **Everything was serial.** Seven tests, each with a 120s convergence budget
  per wait and two 20s quiesce windows, ran end to end behind one cold Rust
  build and one cluster.
- **A suite could not be re-run alone.** Iterating on the pause contract meant
  re-running the workloadNamespace and class-propagation tests with it.

The assertions are unchanged; only their packaging is. Each contract is now its
own cargo test binary with its own `make kind-e2e-<suite>` target, and
`.github/workflows/e2e.yaml` runs them as a `fail-fast: false` matrix — one job
per suite, each on its own kind cluster named `banlieue-e2e-<suite>`. The
fixtures, polling helpers and builders they share live in
`tests/e2e_common/mod.rs`, so the split costs no duplication.

`make kind-e2e` still runs everything, in sequence, on one cluster — invoked
from the recipe rather than as prerequisites, so `make -j` cannot parallelise
suites that share a cluster.

The two shell-driven bootstrap checks were renamed into the same namespace
(`kind-verify-dry-run` → `kind-e2e-dry-run`, `kind-verify-escape-hatch` →
`kind-e2e-escape-hatch`) so that every CI job is `make kind-e2e-<suite>` with no
exceptions.

### Why the install path gets its own suite

The lifecycle suite runs against a cluster installed either way, and for local
iteration it uses `kubectl apply -R -f deploy/operator/` — the GitOps path.
Those two paths drift silently. Bug-110 (bootstrap never installing the shared
per-backend ClusterRole) was caught only by accident, because a stale copy of
that role lingered in a reused cluster; against a clean one the manifest-based
suite would have stayed green while every real `bootstrap operator` produced
provider pods with zero permissions.

CI therefore runs `make kind-e2e-ci E2E_SUITE=<suite>`, which installs via
**`banlieue bootstrap operator`** before each suite. The manifest-apply path
stays available for local iteration behind `E2E_INSTALL=manifests`, which skips
the `banlieue` CLI build. The three bootstrap-specific suites
(`kind-e2e-bootstrap`, `-dry-run`, `-escape-hatch`) ignore that knob and always
install via bootstrap — asserting the installer's own output is what they are
for.

### Assertions deliberately made over `managedFields`

Disjoint status ownership (ADR-0012) is asserted against the apiserver's own
`metadata.managedFields` record, not against condition *values*. An earlier
version checked that no condition carried an empty reason, which passes
whenever the operator writes conditions that merely look plausible — the same
vacuous-assertion trap as bug-105.

### Shape of the suite

- **Rust, not shell.** A cargo integration test in
  `crates/banlieue-operator/tests/`, using `kube` and the real `banlieue-api`
  types. Shell + `kubectl` + `jq` would re-encode every CRD field name as a
  brittle JSONPath string; the Rust types are the schema, so a field rename
  breaks the test at compile time instead of at 3am in CI.
- **`#[ignore]`d by default**, so `cargo test` stays hermetic — the same
  convention as `banlieue-libvirt`'s `tests/live_libvirtd.rs`.
- **Located in `tests/`**, per `rules/testing.md`. Cargo compiles it as a
  separate crate linking the library externally, so it exercises only the
  public API — appropriate for a black-box test.
- **All orchestration in the Makefile** (`make kind-e2e-<suite>`, `make
  kind-e2e-ci`), per `rules/github-workflows.md`: the workflow installs tools
  and calls a target, and the identical target runs locally. The workflow's job
  matrix is a transcription of the Makefile's `E2E_SUITES` list, not a second
  source of truth for what runs.

### Not modelled in CALM

CALM models banlieue's runtime architecture — controllers, backends, CRs and
the flows between them. CI jobs are not runtime components and no existing
workflow (including ADR-0006's release pipeline) has a CALM node. This ADR
follows that precedent; the architecture model is unchanged.

## Consequences

**Positive**

- The apiserver-contract failures unit tests structurally cannot catch are
  covered: SSA acceptance, schema agreement, RBAC validity, GC semantics.
- The suite needs no backend, no credentials and no secrets, so it runs on
  fork PRs — where the release pipeline's signing jobs deliberately do not.
- A failure localises to the operator, because nothing else is under test.
- `make kind-e2e-<suite>` is the same command locally and in CI.
- A failure names the contract that broke, and the suites run in parallel
  rather than end to end.

**Negative / accepted costs**

- Backend-facing behaviour (login, inventory walk, `failureDomains`) stays
  uncovered by this suite. That is the follow-up vcsim job's job, not a gap
  this ADR pretends to close.
- A kind cluster plus an image build makes this the slowest job in CI; it is
  therefore scoped to paths that can affect it rather than run on every push.
- Splitting into a matrix pays that cluster-and-build cost **once per suite**
  rather than once per run. Accepted: the jobs are parallel, so wall-clock time
  falls even as total runner-minutes rise, and the cargo cache is shared.
- The suite list now exists in two places — `E2E_SUITES` in the Makefile and the
  workflow's matrix. A suite added to one and not the other runs locally but not
  in CI (or names a target that does not exist). Both carry a comment pointing
  at the other.
- The suite asserts on derived object names, so renaming the naming scheme
  requires updating it — intentional, since that scheme is public API.

**Follow-ups**

- ~~A `vcsim`-backed suite asserting `Provider` reaches `Ready` with populated
  `status.failureDomains`.~~ **Not pursued (2026-07-31).** `vim_rs`'s
  `vcsim_compat` feature requires its `xml` feature — the SOAP transport — and
  the workspace pins `vim_rs` with `default-features = false` precisely because
  production vCenter uses the JSON transport (ADR-0009). A vcsim suite would
  therefore compile in SOAP and exercise **the transport production does not
  use**, proving the reconcile logic while leaving the actual client path
  untested, and shipping a CI image that differs from the released one.

  Backend connectivity is instead validated **manually against a production
  vSphere on-prem**, which exercises the real JSON transport. That is a
  deliberate trade: no CI coverage of backend connectivity, in exchange for the
  coverage that does exist being real rather than simulated. Revisit only if
  `vim_rs` gains a JSON-capable simulator, or if vcsim itself starts serving
  the JSON API.
- Extend to `bootstrap provider <backend>` once a second backend ships, to
  prove the static escape hatch and the operator path stay equivalent.
