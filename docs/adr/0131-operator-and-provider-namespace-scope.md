# 0131 — Operator and provider namespace scope: never cluster-wide by default

- **Status:** Accepted
- **Date:** 2026-11-15
- **Proposed:** 2026-11-15
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-11-16 (Decision 4's "no selector on a provider" is
  superseded by [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md),
  Proposed; the premise that a provider cannot serve many namespaces held only
  while it resolved its `Provider` and credentials in the machine's namespace)
- **Related:** Extends [ADR-0130](0130-namespace-selector-multi-tenancy.md)
  (which fixed the controller only and named this as its follow-up).

## Context

ADR-0130 made `banlieue controller` serve a chosen set of namespaces and
never "all" by default. Three other binaries still default to every
namespace:

| Binary | Flag | Unset means | Where |
| --- | --- | --- | --- |
| `banlieue operator` | `--watch-namespace` / `BANLIEUE_WATCH_NAMESPACE` | `Api::all` for `Provider` | `banlieue-operator/src/app.rs` |
| `banlieue provider libvirt` / `vsphere` / `proxmox` | `--namespace` / `BANLIEUE_NAMESPACE` | `Api::all` for `Provider` and the machine CR | each `banlieue-provider-*/src/app.rs` |
| `banlieue provider cloud-hypervisor` | provider config | already one namespace | (no change) |

The two cases are not the same problem, and the facts below decide the
design.

**Providers live in tenant namespaces.** The controller lists `Provider`s in
the VM's own namespace (`reconciler/virtualmachine.rs`), and a machine CR
always lives beside the `VirtualMachine` that owns it. A multi-tenant
install therefore has `Provider`s, and the operator-rendered provider
workloads, in each tenant namespace. The operator is the component that has
to see all of them.

**Operator-rendered providers are already single-namespace.** The operator
passes `--namespace <provider namespace>` to every workload it renders
(`workload.rs`, `build_args`), and `banlieue bootstrap provider` passes
`--namespace <install namespace>` (`bootstrap.rs`). The cluster-wide default
is reachable only by running a provider binary by hand with no `--namespace`.

**A provider cannot usefully serve many namespaces.** Its credential and
CA-bundle reads go through a namespaced `Role` in one namespace; the shared
`ClusterRole` no longer grants cluster-wide Secret access (security review
2026-07-31, CHAIN-002). A provider watching other namespaces would
reconcile their objects and then fail the credential read with a 403. So a
selector on a provider would widen a watch whose results it is forbidden to
use.

**`--namespace` does not mean the same thing everywhere.** On the
controller and providers it is the *scope*. On the operator it is the
namespace the operator *runs in* (default `banlieue-system`), used for its
Lease and imagebuilder namespace; the scope flag there is
`--watch-namespace`. Reusing ADR-0130's flag name on the operator would
silently change what an existing flag means.

## Decision

1. **The operator resolves its `Provider` scope with ADR-0130's precedence,
   and never defaults to every namespace:**

   1. `--watch-namespace-selector` (`BANLIEUE_WATCH_NAMESPACE_SELECTOR`), a
      label selector, wins when set.
   2. else `--watch-namespace` (`BANLIEUE_WATCH_NAMESPACE`): exactly that
      namespace.
   3. else the operator's own namespace (`--namespace`, default
      `banlieue-system`).

   The new flag is named `--watch-namespace-selector`, not
   `--namespace-selector`, so the operator's existing `--namespace` keeps its
   meaning. The resolved scope is logged at startup, naming the rule that
   applied. An invalid selector fails startup, validated by the same probe
   LIST as ADR-0130.

2. **Operator selector mode reuses ADR-0130's mechanism, not a copy of it:**
   a selector-filtered `Namespace` reflector, `Api::all` for `Provider`, and a
   gate that skips an object outside the served set. A `Namespace`
   add, remove or relabel re-triggers `Provider` reconciles, and only when
   the served set really changes. An object being deleted bypasses the gate,
   so the operator's finalizer path still tears down the workload it
   rendered. Un-labelling stops reconciliation; it does not delete the
   `Provider` or its workload (ADR-0130 decision 6).

3. **The scope code moves out of `banlieue-controller` into a crate both
   binaries depend on.** `Scope`, `resolve_scope`, `check_selector` and
   `NamespaceGate` (today `banlieue-controller/src/scope.rs`) are lifted into
   `banlieue-provider-sdk`, which the operator and every provider already
   depend on. The controller re-uses it from there. Two copies of "which
   namespaces do we serve" would drift, and the drift would be a tenancy bug.
   The move is behaviour-preserving and lands first, under the existing
   `scope_tests.rs`.

4. **Providers keep `--namespace` and lose the cluster-wide default; they get
   no selector.** When `--namespace` is unset the provider serves the
   namespace it runs in (service-account namespace in-cluster, kubeconfig
   context locally), and startup fails if none can be determined rather than
   widening. This follows from the CHAIN-002 fact above: a selector would
   only widen what the provider cannot read credentials for. Serving a second
   namespace means running a second provider workload there, which is what
   the operator already does per `Provider`.

5. **What stays cluster-wide, and why.**
   - `ProviderClass` and `VMImage` are cluster-scoped platform resources; they
     are not gated.
   - The operator's `ProviderClass` reconciler lists `Provider`s with
     `Api::all` to report which Providers use a class
     (`reconciler/providerclass.rs`). That is a read-only count for a
     cluster-scoped object, so it is left alone and recorded as a risk
     (below), not gated.
   - The operator's `Deployment` watch (`Api::all`, for the workloads it owns)
     is a watch on objects the operator itself created. Narrowing it to the
     operator's own label is a separate, mechanical change and is not made
     here.
   - The imagebuilder and the Cloud Hypervisor provider already use a fixed
     namespace.

6. **RBAC follows the mode.** Operator selector mode needs a read-only
   `list`/`watch` on `Namespace`, added to the operator's `ClusterRole`
   (and to the one `banlieue bootstrap` renders) with no write verb, for
   ADR-0130's reason: a component that could relabel a namespace could
   choose its own scope. The default (single-namespace) mode needs no
   `Namespace` access.

7. **The selector is scope, not isolation** (ADR-0130 decision 7, unchanged).
   Labelling a namespace lets the operator render provider workloads there.
   Who may create a `Provider` in a tenant namespace, and which classes it
   may reference, is the follow-up RBAC and `ValidatingAdmissionPolicy` ADR.
   Until then a tenant able to create a `Provider` in a served namespace can
   cause the operator to run a workload; that is stated in the threat model,
   not hidden by this ADR.

## Consequences

- **The default narrows.** An operator that relied on watching every
  namespace now watches only its own, and a hand-run provider with no
  `--namespace` now serves its own namespace. A multi-tenant install sets
  `BANLIEUE_WATCH_NAMESPACE_SELECTOR`. The startup log names the resolved
  scope so the change is visible. `banlieue` has no release yet, so this is
  recorded as a change, not a migration.
- **One label covers the whole stack.** The same label that ADR-0130 uses for
  the controller can drive the operator's selector, so labelling a namespace
  onboards its pools, claims, machines and `Provider` workloads together.
  Nothing forces the operator and the controller to share a label; they are
  separate flags.
- **A Provider in an unserved namespace is silent.** The operator skips it,
  so it gets no workload and no status from the operator. This is the same
  trade as ADR-0130 and needs the same documentation, or an un-labelled
  namespace reads as a broken Provider. Surfacing it as a condition is
  possible but would mean writing to an object the operator has just decided
  not to serve; not done here.
- **Operator and controller must agree.** If the controller serves a
  namespace the operator does not, a `VirtualMachine` schedules onto a
  `Provider` that never gets a workload. The multi-tenancy guide must say to
  keep the two selectors aligned. A startup check across binaries is not
  possible (they do not talk to each other), so this is documentation plus,
  later, a status condition.
- **Residual cluster-wide reads** stay: the operator's `Namespace` list/watch
  in selector mode, the `ProviderClass` consumer count, and the
  `Deployment` watch. Each is read-only; none grants write access outside a
  served namespace.
- Memory cost in selector mode is one extra filtered `Namespace` watch.
- **Out of scope:** per-tenant RBAC, admission policy and quota (the
  follow-up ADR); mediatore's `AgentSandbox` selector (ADR-0130); a live
  cluster check of label/un-label behaviour for the operator.
- **Needs a threat-model pass** when implemented: the operator's new
  `Namespace` read, the tenant-creates-`Provider` path in decision 7, and
  the cluster-wide residuals above as §8 entries with a *Revisit when*.
