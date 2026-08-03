<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0013 — `banlieue bootstrap` — self-contained cluster install

- **Status:** Accepted
- **Date:** 2026-07-31
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0012 (`ProviderClass` + operator role); ADR-0003
  (per-instance topology); ADR-0004 (single binary, dispatch-only `banlieue`
  crate); ADR-0006 (release pipeline — the binary is the released artifact).

## Context

Installing banlieue means applying a namespace, seven CRDs, two
ServiceAccounts, two ClusterRoles, two ClusterRoleBindings, two ConfigMaps
and two Deployments — spread across `deploy/`. Today that is a `kubectl
apply -f` sequence a reader has to assemble from the tree, and the CRD
YAMLs must have been regenerated from the Rust types first (`make crds`)
or the schemas silently drift from the binary being installed.

ADR-0006 makes the single `banlieue` binary the core released artifact.
That binary already contains the authoritative CRD schemas — they are
derived from its own Rust types. It is therefore the one thing that cannot
be out of sync with itself, which makes it the right installer.

`~/dev/bindy` solves the same problem with `bindy bootstrap
operator|scout|mc`: build every object from Rust types, server-side apply
them in dependency order, with `--namespace`, `--version`, `--registry` and
`--dry-run`. That shape is proven; adopt it.

The open question was scope. With the operator now spawning provider
workloads from `Provider` CRs (ADR-0003/0012), is a per-provider install
command still meaningful?

## Decision

Ship `banlieue bootstrap` with a full-install command plus per-role escape
hatches:

```text
banlieue bootstrap operator      [--namespace --version --registry --dry-run]
banlieue bootstrap provider <backend> [...]
banlieue bootstrap imagebuilder  [...]
```

- **`bootstrap operator`** — the normal path. Applies, in order: Namespace →
  CRDs → controller SA/ClusterRole/ClusterRoleBinding/ConfigMap/Deployment →
  operator SA/ClusterRole/ClusterRoleBinding/ConfigMap/Deployment → one
  `ProviderClass` per backend compiled into this binary. After it, installing
  a backend is `kubectl apply` of a `Provider` CR. `--skip-provider-classes`
  opts out of the last step.
- **`bootstrap provider <backend>`** — installs a standalone, statically
  configured provider workload (SA, Role, RoleBinding, Deployment) with no
  operator involvement. Kept for air-gapped and tightly-controlled installs
  where minting workloads from a controller is not acceptable, and as the
  fallback if the operator is down.
- **`bootstrap imagebuilder`** — the equivalent for the ADR-0010 image
  build pipeline, which has no CR-driven lifecycle of its own.

`<backend>` is constrained to the backends compiled into the running binary,
so a slim build (`--no-default-features --features vsphere`) cannot offer to
install a backend it does not contain.

### Where the code lives

In `crates/banlieue-operator`, as a `bootstrap` module — **not** in the
`banlieue` binary crate, which ADR-0004 restricts to dispatch.

This colocation is the point: the operator's reconciler and the bootstrap
CLI build the *same* Deployment, ServiceAccount and RBAC objects. They share
one set of builder functions, so a workload created by
`bootstrap provider vsphere` is shaped identically to one the operator spawns
from a `Provider` CR. Splitting them into separate crates would create two
definitions of the same workload and guarantee eventual drift.

### Object sources

- **CRDs** are built from the Rust types at runtime via the same
  `crdgen_support::prepared()` path `crdgen` uses — the description promotion
  and CAPI contract label included. `prepared()` therefore moves out from
  behind the `crdgen` Cargo feature (only the `serde_yaml` dependency stays
  gated). No CRD YAML is embedded.
- **Static ClusterRoles** are `include_str!`-embedded from `deploy/*/rbac/`
  and parsed into typed objects. The shipped manifests stay the single
  source of truth, so a GitOps install and a `bootstrap` install grant
  identical permissions and the YAML remains reviewable in tree.
- **Per-instance Roles** (the ones scoped to a single credentials Secret)
  are built in Rust — they are computed per `Provider`, so there is no
  static manifest to embed.

### Behaviour

- Every write is server-side apply with field manager
  `banlieue.io/bootstrap`, making re-runs idempotent and distinguishing
  bootstrap-owned fields from those the operator later manages.
- `--dry-run` prints the exact YAML that would be applied and **never
  contacts a cluster** — usable without a kubeconfig, and pipeable into
  `kubectl apply -f -` or a GitOps repo.
- `--version` defaults to the binary's own crate version, so bootstrap
  installs the image matching the binary that ran it. `--registry`
  overrides the registry host for air-gapped mirrors.

## Consequences

**Positive**

- Installing banlieue is one command, and the CRDs applied are by
  construction the ones the running binary implements.
- `--dry-run` makes the same code path serve GitOps users, who get generated
  manifests instead of an imperative apply.
- One definition of each workload, shared between the CLI and the operator's
  reconciler.
- Air-gapped installs are a first-class path, not a documented workaround.

**Negative / accepted costs**

- The binary grows: `k8s-openapi` RBAC/apps types and the embedded RBAC YAML
  are linked into every build, including provider-only slim builds.
- `include_str!` on `deploy/*/rbac/*.yaml` makes those files build inputs —
  moving or renaming one breaks the build. Acceptable: it is exactly the
  coupling that prevents drift, and it fails loudly at compile time.
- Two install paths (bootstrap and raw `kubectl apply -f deploy/`) must stay
  equivalent. Mitigated by both reading the same embedded manifests.
- `bootstrap provider` can create a workload the operator does not know
  about. It is deliberately not owned by a `Provider` CR, so the operator
  will not adopt or delete it; operators must not run both paths for the
  same backend.

**Follow-ups**

- A `banlieue bootstrap uninstall` counterpart is not in scope; deletion is
  currently `kubectl delete` of the namespace plus CRDs.
- Consider emitting a kustomization from `--dry-run` output if GitOps users
  ask for structure beyond a flat YAML stream.
