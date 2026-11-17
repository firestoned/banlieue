# 0134 — A shared Cloud Hypervisor host serves tenant namespaces and keeps its credentials

- **Status:** Accepted
- **Date:** 2026-11-16
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md)
  (the vSphere implementation; this is the host-resident variant);
  builds on [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (the provider runs on the KVM host with a bound ServiceAccount token and a
  per-instance namespaced Role),
  [ADR-0126](0126-provider-status-publishes-its-serviceaccount.md),
  [ADR-0127](0127-move-a-cloud-hypervisor-vm-between-hosts-in-a-zone.md) (moves) and
  [ADR-0129](0129-capi-inframachines-receive-the-machines-bootstrap-data.md) (the CAPI hold).

## Context

ADR-0133 lets one provider serve tenant namespaces chosen by a label selector,
with its `Provider` and credentials in a platform (home) namespace. The vSphere
provider is a Deployment: the operator renders `--watch-namespace-selector` on
its pod and its ClusterRole already covers machines cluster-wide. The Cloud
Hypervisor provider differs in three ways (verified in the tree):

- It runs **on the KVM host**, not in the cluster (ADR-0060). The operator
  renders no arguments for it; everything about the host comes from a
  host-local config file.
- Its token is a **bound ServiceAccount token** whose permissions are a
  per-instance namespaced `Role` (`workload.rs`, the External class's
  `additionalRules`) plus a tiny shared ClusterRole that grants only `vmimages`
  (`deploy/provider-cloud-hypervisor/rbac/clusterrole.yaml`). It has no access
  to a machine in any namespace but its own.
- It matched a machine only when `providerRef.name` was this host **and** the
  machine was in the Provider's namespace
  (`reconciler.rs`, `is_ours`). `providerRef` now carries a namespace
  (`ProviderReference`), so that rule must become name AND namespace.

## Decision

1. **Token reach: a second, opt-in ClusterRole.**
   `deploy/provider-cloud-hypervisor/rbac/clusterrole-shared.yaml` defines
   `banlieue-provider-cloud-hypervisor-shared`: `get/list/watch/update/patch` on
   `cloudhypervisormachines`, `get/update/patch` on `.../status`, `update` on
   `.../finalizers`, and `list/watch` on core `namespaces`. **No Secrets and no
   ConfigMaps.** `banlieue bootstrap operator` installs it (embedded in
   `bootstrap.rs`). The operator binds it with a second ClusterRoleBinding to
   the provider's ServiceAccount **only for a `Provider` with a non-empty
   `spec.namespaceSelector`** (`workload.rs`,
   `build_shared_cluster_role_binding`); dropping the selector prunes the
   binding (`reconciler/provider.rs`). A host that is not shared keeps today's
   namespaced-only token.

2. **How the host learns its selector.** The provider reads its own
   `Provider.spec.namespaceSelector` from its HOME namespace at startup (it
   already reads that `Provider` to learn its class), renders it with
   `banlieue_provider_sdk::scope::selector_to_string` and builds
   `ProviderScope::resolve(Some(home), home, selector)`
   (`app.rs`, `provider_scope`). An absent or empty selector means home only,
   never every namespace. It is **read once at startup**: changing the selector
   needs a provider restart. The operator does not (and cannot) render a flag
   for an External provider, so there is no second place to keep in step.

3. **Serving.** A machine is matched with the SDK's `serves_machine`
   (`providerRef` must resolve to this `Provider` in its home namespace, by name
   AND namespace; `reconciler.rs`, `is_ours`). Each reconcile is gated by
   `SharedServing::skip_unserved`, which never skips a deleting machine. With a
   selector the machine kind is watched through `ProviderScope::api` (cluster
   wide) with `reconcile_all_on(scope_trigger(..))`, so a namespace gaining or
   losing the label re-reconciles; without one it is the home namespace only.

4. **Every read of the `Provider` stays in the HOME namespace.** The host's
   `Provider`, its status patch, the `Provider` watch, the leader Lease, the
   `VMImage` per-provider rows and the ServiceAccount token request already use
   `HostConfig.provider.namespace`, which is the home namespace; none was
   changed. The one machine-side lookup that used to assume the Provider's
   namespace is replaced: see Decision 5.

5. **Two machine lists span the served namespaces.** Guest uids are one pool per
   host, and a pulled image is referenced by machines wherever they are. The
   uid ledger (`used_host_uids`) and the image-eviction check
   (`vmimage::live_references`) therefore list this host's machines through the
   scope's `Api`, not the home namespace. Counting only home would hand two
   tenants' guests the same uid, and evict an image from under a tenant
   machine. They filter with `is_ours`, so a machine in an unserved namespace
   that still names this host still holds its uid and image (the safe
   direction).

6. **Moves and the CAPI hold need no change.** A move is data on the machine
   (`moveIn`/`moveOut`, ADR-0127) written by the controller on machines in the
   VM's namespace; the provider acts on the machine it was given and looks up no
   second machine by namespace. The CAPI hold (ADR-0129) reads the machine's own
   `ownerReferences` and status. Neither assumes the machine is in the
   Provider's namespace.

### Rejected

- **Operator-generated per-served-namespace RoleBindings.** It keeps the token
  namespaced, but needs an operator reconcile loop over the served set and
  `create` on Roles/RoleBindings, which is the escalation path ADR-0012
  refuses. Revisit if the widened stolen-host-token reach (below; threat model
  A-11 and TB-8) is judged too wide.
- **Granting the machine verbs in the one shared ClusterRole.** Every host,
  shared or not, would carry cluster-wide machine access. The opt-in role makes
  the reach follow the declared selector.
- **A `--watch-namespace-selector` flag on the host process.** A second source
  for the selector, to keep in step with `Provider.spec.namespaceSelector` by
  hand, on a process the operator does not start.

## Consequences

- **The stolen-host-token ceiling widens for a shared host.** The token lives on
  the KVM host. For a Provider with a selector, a stolen token can read and
  patch `CloudHypervisorMachine`s in every namespace, not only those the
  selector matches (RBAC cannot be scoped by label; the selector gate is
  software). It still holds no Secret and no ConfigMap anywhere. A host that is
  not shared is unchanged. To be recorded in threat model A-11 / TB-8 and §8 by
  the lead.
- **No credential in any tenant namespace.** The provider reads only its home
  `Provider`; the controller already writes the rendered userData into the
  machine (ADR-0025).
- **Changing the selector needs a provider restart** (read once). The operator
  re-binds or prunes the ClusterRoleBinding at once; the process catches up at
  its next start.
- **Not executed here.** The Cloud Hypervisor crate does not compile on macOS;
  its new tests were type-checked and linted with
  `--target x86_64-unknown-linux-gnu` but run only on a Linux host.
- Tests that exist: `is_ours` by name AND namespace (bare ref in a tenant
  namespace is not ours; ref naming home is; another namespace or host is not);
  the uid ledger spanning namespaces; `provider_scope` (absent/empty selector is
  home only, a selector is rendered with home unchanged); the operator binds the
  shared role only for a selector and only for a backend that ships one.
