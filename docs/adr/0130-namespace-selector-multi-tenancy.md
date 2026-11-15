# 0130 — Select the namespaces banlieue serves by label

- **Status:** Accepted
- **Date:** 2026-11-14
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0046](0046-virtualmachinepool.md)
  (`VirtualMachinePool`) and [ADR-0047](0047-virtualmachineclaim.md)
  (`VirtualMachineClaim`). `AgentSandbox` is a mediatore CRD since
  [ADR-0097](0097-api-ends-at-virtualmachineclaim.md); it is **not** served
  by this repo and needs the same selector in mediatore (see Consequences).

## Context

Several teams should each work in their own namespace, with their own pools,
claims and policies, locked down to that namespace. Today a controller either
watches one namespace (`--namespace`, `BANLIEUE_NAMESPACE`) or every
namespace. There is nothing between "one" and "all", so onboarding a team
means either editing the controller's arguments or serving namespaces nobody
agreed to serve.

A label selector is the Kubernetes-native answer: a platform admin opts a
namespace in by labelling it, and a team cannot opt itself in, because
labelling a `Namespace` is a cluster-scoped right.

### What a watch can and cannot filter

The API server filters a watch by the *watched object's own* labels and
fields. It cannot filter a `VirtualMachinePool` by its **namespace's**
labels. So the selector cannot be pushed into `watcher::Config`; the
controller has to know the set of selected namespaces and apply it itself.

## Decision

1. **Which namespaces are served is resolved in a fixed precedence, and
   the default is never cluster-wide:**

   1. `--namespace-selector` (`BANLIEUE_NAMESPACE_SELECTOR`), a standard
      label selector (`team in (a,b)`, `banlieue.io/tenant=true`), wins when
      set.
   2. else `--namespace` (`BANLIEUE_NAMESPACE`): exactly that namespace.
   3. else **the namespace the controller itself runs in** (the in-cluster
      service-account namespace, or the kubeconfig context's namespace when
      run locally).

   When more than one is set the higher one wins and the lower is ignored;
   the controller logs the resolved scope at startup, naming which rule
   applied. If no namespace can be determined at step 3, startup fails
   rather than widening. There is no "all namespaces" switch: serving every
   namespace requires a selector that matches them (for example
   `banlieue.io/tenant`, an exists-selector), so it is always an explicit,
   visible act of labelling.

2. **Single-namespace modes (rules 2 and 3) use `Api::namespaced`**, exactly
   as `--namespace` does today: the watch itself is narrowed, and the
   controller needs only a namespaced `Role`, not cluster-wide read.

3. **Selector mode uses a `Namespace` reflector filtered by the selector
   server-side** as the single source of "which namespaces are served". A
   `Namespace` is the object carrying the labels, so this filter *is*
   pushed to the API server.

4. **In selector mode, controllers use `Api::all` and gate on the reflector.** `reconcile`
   returns early, doing nothing and writing nothing, for an object whose
   namespace is not in the set. An object being deleted is the exception
   (decision 6): its finalizer path runs in full, including the writes a
   deletion needs, so un-labelling cannot strand it. A `Namespace` add, remove or relabel event
   requeues every object in that namespace, so labelling a namespace
   takes effect at once and un-labelling stops reconciliation at once,
   without a restart.

5. **Scope: `VirtualMachinePool`, `VirtualMachineClaim`, `VirtualMachine`
   and the infra machine CRs they own.** Cluster-scoped kinds (`Provider` is
   namespaced and stays platform-owned; `VMClass`, `VMImage`) are not
   gated: they are platform resources shared by every tenant.

6. **Un-labelling is non-destructive.** It stops reconciliation; it does
   not delete tenant objects, and finalizers still run, so a namespace
   being un-served cannot strand a deletion.

7. **The selector is not the isolation boundary.** In selector mode the
   controller still needs cluster-wide `list`/`watch`. What isolates tenants from each other is, in
   order of strength: RBAC (a `Role` per tenant namespace, never a
   `ClusterRole`), `ValidatingAdmissionPolicy` restricting which
   classes, images and Providers a namespace's pools may reference, and
   `ResourceQuota`. Those are a follow-up ADR; this one decides only *which
   namespaces are served*.

## Consequences

- The default narrows from every namespace to the controller's own. A
  deployment that relied on cluster-wide serving must now set a selector
  that matches its namespaces; the startup log names the resolved scope so
  the change is visible. The default `ClusterRole` for the controller can
  become a namespaced `Role` unless selector mode is enabled.
- The operator's `--watch-namespace` and the provider crates' namespace
  flags have the same cluster-wide default and should adopt this resolution
  in a follow-up; this ADR changes `banlieue controller` only.
- Teams are onboarded by labelling a namespace; no controller restart.
- A not-yet-synced `Namespace` reflector must not read as "serve nothing"
  or "serve everything": readiness stays false until it has synced.
- Memory cost is one extra watch on `Namespace`, filtered, so only selected
  namespaces are cached.
- An object in a namespace that loses its label keeps its last status. That
  is deliberate (decision 6) and must be documented, or it reads as a stuck
  resource.
- mediatore needs the same flag for `AgentSandbox`, and should reuse the
  selector semantics so one label covers both.
- Needs a threat-model pass: a new trust boundary (tenant namespace), a new
  actor (tenant), and an accepted-risk entry for the cluster-wide read the
  controller keeps.
