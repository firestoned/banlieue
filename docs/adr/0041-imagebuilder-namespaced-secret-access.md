<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0041 — banlieue-imagebuilder reads Secrets through a namespaced Role

## Status

Accepted — 2026-09-09. Amends [ADR-0037](0037-vmimage-layered-cloud-config.md),
whose RBAC consequence was implemented as a cluster-wide grant. Restores the
posture established by [ADR-0003](0003-provider-deployment-topology.md) and the
2026-07-31 security review (CHAIN-002 / SEC-008).

## Context

ADR-0037 gave `banlieue-imagebuilder` the ability to read the Secrets named by
`VMImage.spec.cloudConfigs`, merge their YAML, and server-side-apply a single
merged cloud-config Secret. The RBAC that shipped with it was added to the
component's existing **`ClusterRole`**:

```yaml
- apiGroups: [""]
  resources: ["secrets"]
  verbs: ["get", "list", "watch", "create", "patch"]
```

with no `resourceNames`, bound by a **`ClusterRoleBinding`**. The rule's own
comment states the access is *"scoped to the imagebuild namespace"*. It is not.
A `ClusterRole` reached through a `ClusterRoleBinding` has no namespace scope at
all: this grants read **and write** on every Secret in every namespace of the
cluster.

The code never wanted that. There is exactly one `Api<Secret>` in the crate:

```rust
// crates/banlieue-imagebuilder/src/reconciler/vmimage.rs
let secrets_api: Api<Secret> = Api::namespaced(ctx.client.clone(), &ctx.build_namespace);
```

Every referenced cloud-config Secret and the merged output Secret live in the
build namespace (`--build-namespace`, default `banlieue-imagebuild`). The extra
reach is pure standing privilege — the same defect this project already removed
from the provider `ClusterRole`s (CHAIN-002) and the controller `ClusterRole`
(SEC-008), reintroduced by a later feature.

It is worse than the earlier two instances in one respect: those were read-only
(`get`/`list`/`watch`). This one carries `create`/`patch`, so a compromise of
the imagebuilder identity can *overwrite* a `Provider`'s credentials Secret and
redirect a provider at an attacker-controlled endpoint, not merely read it.

The threat model records this boundary as **TB-2 (pod → Secrets)**, whose
governing rule is: *no banlieue identity holds cluster-wide Secret access, and
each identity reads only the objects it can name.*

## Decision

**`banlieue-imagebuilder`'s Secret access moves out of its `ClusterRole` and
into a namespaced `Role` in the build namespace, bound to the imagebuilder
ServiceAccount by a `RoleBinding` that lives with the Role.**

1. Delete the `secrets` rule from
   `deploy/imagebuilder/rbac/clusterrole.yaml`. The `ClusterRole` keeps only
   what genuinely needs cluster scope: `VMImage` (a cluster-scoped kind),
   `OSArtifact`, Events, and Leases.
2. Add `deploy/imagebuilder/rbac/role.yaml`: a `Role` named
   `banlieue-imagebuilder-cloudconfig` in `banlieue-imagebuild`, granting
   `get`/`list`/`watch` (read the referenced cloud-config Secrets) and
   `create`/`patch` (server-side-apply the merged Secret) on `secrets`.
3. Bind it with a `RoleBinding` **in `banlieue-imagebuild`** whose subject is
   `ServiceAccount banlieue-imagebuilder` **in `banlieue-system`**. The Role and
   RoleBinding live with the objects they grant, not with the subject — the
   same cross-namespace shape `banlieue-operator` already uses for the import
   identity (`build_import_role_binding`).

No `resourceNames`: the referenced Secret names come from arbitrary `VMImage`
authors and are not knowable when the static manifest is written. Namespace
scope is the boundary here, and the build namespace holds only build inputs and
outputs — never provider credentials.

## Consequences

- A compromise of `banlieue-imagebuilder` is now bounded by the build namespace
  instead of the cluster. It can no longer read any `Provider`'s hypervisor
  credentials, nor overwrite them.
- **Operators who override `--build-namespace` must move this `Role` and
  `RoleBinding` with it.** The static manifest names `banlieue-imagebuild`; a
  deployment that changes the flag and not the manifest gets a `403` on the
  first `VMImage` carrying `cloudConfigs`. This is a deliberate, visible failure
  rather than a silent over-grant. Called out in the imagebuilder guide.
- No code change, and no behavioural change on a default install — the code
  already only ever touched the build namespace.
- The `banlieue-imagebuild` namespace's existing node-root warning
  (ADR-0016, SEC-009) now also covers these Secrets. That is not a regression:
  cloud-config Secrets are build inputs living in the build namespace, and
  anyone with pod-create there could already read them.
- **Reviewer's note, recorded because it cost a full audit to catch:** a comment
  claiming namespace scope does not create namespace scope. When reviewing an
  RBAC change, read the binding kind and the presence of `resourceNames` — never
  the prose beside them.
