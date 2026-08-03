<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0003 — Provider deployment topology (per-instance)

- **Status:** Accepted
- **Date:** 2026-05-30 (proposed) / 2026-07-31 (accepted)
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0012 (ProviderClass CRD + `banlieue operator` role) implements
  this decision; ADR-0013 (bootstrap CLI); ADR-0004 (single binary, role crates);
  open decision O-003 ("multi-tenancy boundaries within a single Provider");
  D-007 (Provider model); least-privilege project principle.

> **Amended 2026-07-31.** The 2026-05-30 draft proposed a *hybrid* topology
> (`deploymentStrategy: Shared | PerInstance`, defaulting to `Shared`). That
> proposal is **not** adopted. The decision is **per-instance only**, with the
> strategy knob explicitly deferred. Rationale below under *Why not the hybrid*.

## Context

Before this decision, one provider Deployment (e.g.
`banlieue-provider-vsphere`) watched *every* `Provider` of its class
cluster-wide, filtering by `spec.providerClassRef.name` inside the
reconciler. Provider Deployments were hand-applied YAML
(`deploy/provider-vsphere/`); creating a `Provider` CR spawned nothing.
banlieue was therefore not an operator in the usual sense — the control
loop that should turn desired state into running workloads did not exist.

A requirement surfaced during design: **no work-queue starvation across
backends** — a hung or slow reconcile against vCenter A must not stall
vCenter B. Two further drivers are credential isolation and per-backend
network policy (both least-privilege wins).

Three topologies were considered:

1. **Per-class (the pre-decision status quo).** One Deployment per class
   handles all instances. Simplest, lowest overhead; shared blast radius;
   one pod loads every backend's credentials.
2. **Per-instance.** One Deployment per `Provider` CR. Maximum isolation
   (blast radius, credentials, network); pays pod + Lease + watch cost
   per backend (100 vCenters ⇒ 100 pods).
3. **Hybrid with a strategy knob.** `ProviderClass` is the template
   (image, RBAC, resources); `deploymentStrategy: Shared | PerInstance`
   selects per-class or per-instance instantiation. Default `Shared` for
   small installs; `PerInstance` for isolation / multi-tenancy.

## Decision

Adopt **option 2 — per-instance, unconditionally.**

Each `Provider` CR reconciles to its own Deployment, ServiceAccount,
namespaced Role + RoleBinding, ClusterRoleBinding, and leader-election
Lease. Each provider pod runs a **server-side filtered watch** scoped to its
own Provider, so its informer cache holds only its own objects — strictly
better than filtering in the reconciler, which pays the full cluster-wide
cache cost regardless.

Naming is derived and stable:

| Object              | Name                                        |
| ------------------- | ------------------------------------------- |
| Deployment          | `banlieue-provider-<class>-<provider-name>` |
| ServiceAccount      | `banlieue-provider-<class>-<provider-name>` |
| Role / RoleBinding  | `banlieue-provider-<class>-<provider-name>` |
| ClusterRoleBinding  | `banlieue-provider-<class>-<provider-name>` |
| Lease               | `banlieue-provider-<class>-<provider-name>` |

Names longer than 63 characters are truncated with a stable FNV-1a suffix
rather than emitted invalid. The hash must be stable across releases, so
`DefaultHasher` is not usable — it is explicitly not stable across Rust
versions, and a name that changed with the compiler would orphan the
previous Deployment on every upgrade.

### Two bindings, and why deletion needs a finalizer

Permissions split by the scope of what they grant:

- A **namespaced Role + RoleBinding** covers the sensitive, per-instance
  grants: `get` on exactly the credentials Secret this Provider names (and
  its optional CA bundle), its own Lease, and event creation.
- A **ClusterRoleBinding** to the backend's shared ClusterRole covers what
  is cluster-scoped or cross-namespace and cannot be expressed in a Role —
  most importantly `VMImage`, which is a cluster-scoped CRD.

`resourceNames` is what makes the first half worth having, and it only
works for verbs that name a single object — Kubernetes rejects it for
`list`, `watch`, `create` and `deletecollection`. The provider reads both
its credentials Secret and its CA bundle with a `get` by name, so a
`resourceNames`-scoped Role is sufficient and no blanket `secrets: list`
is needed.

Namespaced objects (Deployment, ServiceAccount, Role, RoleBinding) carry an
`ownerReference` to their `Provider` (`controller: true`,
`blockOwnerDeletion: true`) and are garbage-collected with it. The
**ClusterRoleBinding cannot be**: a cluster-scoped dependent with a
namespaced owner is treated by the garbage collector as having a missing
owner, which deletes it immediately. The operator therefore holds a
finalizer (`banlieue.io/provider-workload`) on each Provider and deletes
the ClusterRoleBinding explicitly before releasing it.

Routing stays at the **infra-CRD** layer (the provider watches
`VSphereMachine`, never `VirtualMachine`); the main controller's scheduler
stamps the `banlieue.io/provider` label when it emits the infra CR.
Explicit provider pinning, if added, is a scheduling **constraint**, not a
scheduler bypass (preserves D-009).

Credentials follow the isolation: a per-instance Role grants `get` on
**only** the Secret named in that Provider's `spec.connection.credentialsRef`
(and the optional `caBundle` ConfigMap/Secret) — a resource-name-scoped
Role, not a blanket `secrets: get` across the namespace.

### The prune selector is upgrade-sensitive

Cleanup selects owned objects by label, pinning **both**
`banlieue.io/provider` and `banlieue.io/provider-namespace`. Both are required
because the first is not unique cluster-wide: two Providers can share a name in
different namespaces, and a selector matching only the name would let one
tenant's prune delete another tenant's workload.

The consequence is that **adding a label to this selector is a breaking
change**. Objects created by an earlier build carry the old label set, so a
newer operator's selector will not match them — they become unreachable by
prune *and* by finalizer cleanup, and leak.

That is safe today only because `banlieue-operator` has never been released:
there is no deployed version to upgrade from, so no such objects can exist. It
stops being safe the moment there is one.

**Before the first release that changes this selector**, one of:

- keep the selector fixed, treating the current label pair as API surface; or
- ship a migration that re-labels existing objects before the new selector
  takes effect; or
- match on the stable `banlieue.io/provider` label alone and filter by
  namespace in the controller — viable for namespaced objects, which carry
  their own namespace, but *not* for the cluster-scoped ClusterRoleBinding,
  which has no namespace other than the label itself.

`owned_by_selector_pins_both_name_and_namespace` in `naming_tests.rs` pins the
current shape so any change to it is deliberate rather than incidental.

## Why not the hybrid

The hybrid was the 2026-05-30 proposal; it is rejected for four reasons.

- **The knob is not a choice between two balanced options.** Every driver
  the original ADR named — starvation, credential isolation, per-backend
  network policy — is solved by `PerInstance` and by none of them by
  `Shared`. `Shared`'s only advantage is pod count.
- **`Shared` is the status quo, not a capability.** Shipping a hybrid that
  defaults to `Shared` would mean that, by default, creating a `Provider`
  CR still spawns nothing — the operator behaviour this work exists to
  deliver would be the opt-in path.
- **It is two code paths in the lifecycle reconciler,** diverging in
  Deployment naming, Lease naming, ServiceAccount identity, watch scoping,
  and the RBAC template. Both would need building, testing, and documenting
  before either had a user.
- **The knob is cheap to add later and hard to remove.**
  `deploymentStrategy` is an optional field with a default; adding it in a
  later release is a backward-compatible CRD change from this starting
  point. Committing to it now locks in dual-path maintenance ahead of the
  problem it solves.

## Consequences

**Positive**

- Creating a `Provider` CR provisions a running provider — banlieue behaves
  as an operator.
- One hung backend cannot stall reconciliation for any other backend.
- A compromised provider pod holds exactly one backend's credentials.
- Per-backend NetworkPolicy becomes expressible (one pod, one egress target).
- Provider deletion is garbage-collected via owner references.
- Filtered watches keep each pod's cache proportional to its own backend.

**Negative / accepted costs**

- Pod, Lease, ServiceAccount, and watch cost scale linearly with backend
  count. At ~128Mi requested per pod, 100 backends is ~12.8Gi of requests.
  Accepted: the projected near-term backend count per class is small, and
  the `Shared` alternative trades an availability bug for that saving.
- More API objects per Provider, so more to inspect when debugging.
  Mitigated by derived naming and consistent labels.
- The operator needs privileges to create Deployments, ServiceAccounts,
  Roles, and RoleBindings. Isolated into its own role and ServiceAccount by
  ADR-0012 rather than added to the main controller.

**Follow-ups**

- Revisit `deploymentStrategy: Shared` only if a real deployment reports
  pod-count pressure. Record the reporting install in that ADR.
- O-003 (multi-tenancy boundaries within a single Provider) remains open;
  per-instance isolation narrows but does not close it.
