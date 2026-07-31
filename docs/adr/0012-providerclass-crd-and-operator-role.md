<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0012 — `ProviderClass` CRD and the `banlieue operator` role

- **Status:** Accepted
- **Date:** 2026-07-31
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0003 (per-instance topology — this ADR implements it);
  ADR-0013 (bootstrap CLI); ADR-0004 (single binary, library role crates);
  D-003 (CRD-only bus); least-privilege project principle.

## Context

ADR-0003 settles *what* to create: one Deployment, ServiceAccount, Role,
RoleBinding and Lease per `Provider` CR. Two questions remain.

**Where does the install metadata live?** A `Provider` describes a
*backend instance* — an endpoint, credentials, capabilities. It says nothing
about which container image runs its controller, what resources that pod
gets, or which namespace it lands in. `ProviderSpec.provider_class_ref`
already points at a `ProviderClass` that has never existed; its doc comment
says "a future ProviderClass CRD will provide install metadata (image,
RBAC) without changing this reference." That future is now.

Putting image/resources on `Provider` would repeat the same image tag on
every backend and make an upgrade an N-object edit. It would also blur the
line between "an admin registering a vCenter" and "a platform owner
deciding what banlieue runs" — different people, different privileges.

**Which process runs the lifecycle loop?** Creating Deployments,
ServiceAccounts, Roles and RoleBindings is a privilege-escalation surface:
an actor that can create a RoleBinding can grant privileges. The main
`banlieue controller` currently holds a deliberately narrow ClusterRole
scoped to scheduling. Options:

1. **Extend `banlieue controller`.** One fewer pod; the controller already
   watches `Provider` CRs for scheduling. But its ClusterRole grows to
   include `create` on `deployments`, `serviceaccounts`, `roles`,
   `rolebindings` — permanently, for every install, including ones that
   never use provider lifecycle automation.
2. **A new `banlieue operator` role crate.** A separate library crate,
   subcommand, Deployment, and ServiceAccount. Costs one more pod and one
   more thing to install; keeps the escalation surface in an identity that
   does nothing else.

## Decision

**Add a cluster-scoped `ProviderClass` CRD, and a new `banlieue operator`
role crate that reconciles `Provider` → workload.** Option 2 above.

### `ProviderClass` (`banlieue.io/v1alpha1`, cluster-scoped)

Cluster-scoped because it is a platform-owner concern — one decision about
what banlieue runs, referenced by `Provider` CRs in any tenant namespace.

`spec` carries only install metadata:

- `backend` — which `banlieue provider <backend>` subcommand the spawned
  Deployment runs (`vsphere`, `proxmox`, `libvirt`). Separate from the
  object's name so two classes can pin different images of one backend.
- `image` — `{repository, tag, pullPolicy, pullSecrets}`.
- `workloadNamespace` — where workloads are created; defaults to the
  operator's own namespace.
- `replicas`, `resources`, `nodeSelector`, `tolerations` — pod shape.
- `logging` — `{level, format}` passed to spawned workloads.
- `additionalRules` — extra `PolicyRule`s appended to each per-instance Role.
- `paused` — suspend lifecycle reconciliation for the whole class.

`status` holds `conditions`, `observedGeneration`, and `providers` (the
count of `Provider` CRs referencing this class).

### The `banlieue operator` role

A new library crate `crates/banlieue-operator`, dispatched from the single
binary as `banlieue operator` (ADR-0004 — role crates are libraries; the
`banlieue` crate owns only dispatch). It watches `Provider` and
`ProviderClass`, and server-side-applies the five owned objects per
Provider using field manager **`banlieue.io/operator`**.

The main controller's ClusterRole is **unchanged** by this ADR.

### Status ownership

The operator writes **only** `Provider.status.workload`
(`{deploymentName, namespace, readyReplicas, observedGeneration}`). It does
**not** touch `Provider.status.conditions`.

This is deliberate. `conditions` is a plain list without
`x-kubernetes-list-type: map` (kube-derive/schemars do not emit that
marker), so two field managers writing entries into the same list fight
over the whole array rather than merging per-entry. The provider's own
controller (`banlieue.io/provider-vsphere`) already owns `conditions`;
giving the operator a disjoint field keeps server-side apply conflict-free
without needing `force` to paper over it.

### RBAC: the operator must hold what it grants

Kubernetes forbids creating or binding a Role carrying permissions the
creator lacks, unless the creator holds `escalate` / `bind` on
`rbac.authorization.k8s.io`. Rather than granting those verbs — which
effectively make the operator cluster-admin — the operator's ClusterRole
**includes the union of the permissions it hands to provider Roles**
(`vspheremachines`, `vmimages`, IPAM claims, events, leases, and so on).

The escalation surface is therefore bounded by, and auditable from, the
operator's own ClusterRole: it can only grant what you can read there.
Adding a new backend that needs a new permission means adding it to the
operator ClusterRole too — an intentional speed bump.

Credential access stays narrow: each per-instance Role grants `get` on
**only** the Secret named in that Provider's `connection.credentialsRef`
(plus the optional `caBundle` source), scoped via `resourceNames`. The
operator itself never reads Secret contents.

## Consequences

**Positive**

- Image and pod shape are decided once per class, not once per backend;
  upgrading a fleet is a one-object edit.
- The privilege to mint Deployments and RBAC lives in an identity that does
  nothing else, and the main controller's role is untouched.
- `ProviderSpec.provider_class_ref` stops being a dangling reference.
- Disjoint status ownership means no SSA conflicts between operator and
  provider.
- Per-instance Roles are `resourceNames`-scoped to one Secret.

**Negative / accepted costs**

- One more Deployment, ServiceAccount and ClusterRole to install and
  upgrade. Mitigated by `banlieue bootstrap operator` (ADR-0013).
- The operator ClusterRole is a superset of every provider's permissions,
  so it is a broad role by construction. Accepted as strictly better than
  `escalate`, and it is inspectable in one place.
- A new backend's permissions must be added in two places (the provider
  Role template and the operator ClusterRole). Intentional.
- `ProviderClass` being cluster-scoped means tenants cannot define their own
  classes. Deliberate for v1alpha1; revisit if namespaced classes are asked
  for.

**Follow-ups**

- If `status.conditions` ever gains `x-kubernetes-list-type: map`, the
  operator can publish real conditions instead of a status sub-struct.
- Webhook validation that `spec.backend` names a backend compiled into the
  running binary — currently a reconcile-time error surfaced in status.
