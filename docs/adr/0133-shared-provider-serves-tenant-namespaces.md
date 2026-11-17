# 0133 — A shared provider serves tenant namespaces by selector and keeps its credentials

- **Status:** Accepted
- **Date:** 2026-11-16
- **Implemented:** 2026-11-16 for the vSphere, libvirt and Proxmox providers (the
  controller lookup, each provider's home-namespace resolution and selector
  scope, the operator's flag rendering, the RBAC and the admission policies).
  Cloud Hypervisor: [ADR-0134](0134-cloud-hypervisor-serves-tenant-namespaces.md).
- **Proposed:** 2026-11-16
- **Deciders:** Erick Bourgeois
- **Supersedes:** [ADR-0131](0131-operator-and-provider-namespace-scope.md)
  Decision 4 (providers get no selector, and a tenant namespace therefore
  needs its own `Provider`). The rest of ADR-0131 stands.
- **Related:** Extends [ADR-0130](0130-namespace-selector-multi-tenancy.md)
  and ADR-0131 (the same selector mechanism, now on the provider);
  builds on [ADR-0025](0025-vspheremachine-userdata-secret-rbac.md) (the
  controller renders userData) and
  [ADR-0047](0047-virtualmachineclaim.md) /
  [ADR-0132](0132-claim-selects-its-pool-by-label.md) (a team's pool, members
  and claims live together in one namespace).

## Context

ADR-0130 and ADR-0131 scoped the controller and the operator to namespaces
chosen by label. They left the *provider* on a single namespace, and the
controller looks for `Provider`s only in the VM's own namespace. A tenant
namespace could therefore only be served by a `Provider`, and a provider
workload, **in that namespace**.

That puts the hypervisor credentials in the tenant namespace: the
credentials Secret is read by a namespaced `Role` there, so anyone with
`get` on Secrets in that namespace can read them. That is not acceptable.
Found live: a `VirtualMachinePool` in a tenant namespace `team-a` (labelled
`banlieue.io/tenant=true`, which the controller and operator serve) stays at
0 available; every member reports `NoProviderMatched`, because the only
vSphere `Provider` is in `banlieue-system`.

ADR-0131 concluded "a provider cannot usefully serve many namespaces" because
a provider that watched other namespaces "would reconcile their objects and
then fail the credential read with a 403". That is true of the code **as
written**, and the reason is narrow (verified):

- The vSphere machine reconciler looks up the `Provider` with
  `Api::<Provider>::namespaced(client, <the machine's namespace>)` and reads
  the credentials Secret and CA bundle from that same namespace
  (`reconciler/vspheremachine.rs`: `provider_api.get`, `read_credentials`,
  `resolve_ca_bundle`).
- `spec.providerRef` is a `LocalObjectReference` — a name, no namespace —
  so "the Provider" can only mean one in the machine's own namespace.

Nothing about *watching* a namespace needs a credential there. If the provider
resolves its `Provider`, credentials and CA bundle from **its own** namespace,
it can serve machines in many namespaces and read no Secret in any of them.

Further facts (verified against the tree):

- **The provider never needs a tenant Secret.** The controller already reads
  the tenant's `secretRef`/`configMapRef` userData, renders it, and writes the
  result inline into the machine's `spec.userData`
  (`reconciler/virtualmachine.rs`, `resolve_rendered_user_data`; ADR-0025).
- **The scope code is shared.** `Scope`, `resolve_scope`, `NamespaceGate` and
  `check_selector` live in `banlieue-provider-sdk` (ADR-0131 Decision 3) and
  every provider already depends on it.
- **A team's pool, members, claims and machines are one namespace's
  business.** Claims find members by listing in their own namespace and bind
  them by re-parenting an `ownerReference`; deleting a claim destroys the VM
  by garbage collection; mediatore's `AgentSandbox` owns its claim the same
  way. An `ownerReference` cannot cross namespaces, so anything that moves a
  VM, a claim, or a machine out of the team namespace replaces each of those
  links with a finalizer and a sweeper.

## Decision

1. **Provider lookup is the VM's namespace first, then the provider
   namespace.** For a `VirtualMachine`, the controller lists `Provider`s in the
   VM's own namespace. If that namespace holds **any** `Provider`, that is the
   whole candidate set. Only if it holds **none** does the controller list the
   `Provider`s in the **provider namespace**. The scheduler runs on whichever
   set results, unchanged. A namespace that runs its own `Provider` has chosen
   its own backend, so a selector that matches nothing there fails with
   `NoProviderMatched` rather than silently placing the VM on the shared
   platform. Fail closed.

2. **The provider namespace is explicit controller configuration.**
   `--provider-namespace` (`BANLIEUE_PROVIDER_NAMESPACE`). Unset means **no
   fallback** — the controller behaves as today, never "search every
   namespace". The resolved value is logged at startup.

3. **The machine stays in the VM's namespace; only the `Provider` reference
   crosses.** The infrastructure machine CR is created beside its VM, pool and
   claim exactly as today: same name, same namespace, same `ownerReference`,
   same garbage collection, same `.owns` watch, same status mirror, no new
   label or finalizer, no name collision between tenants. What changes is the
   reference. `spec.providerRef` gains an optional `namespace`; absent means
   the machine's own namespace (today's behaviour, so nothing placed is
   rewritten). The controller sets it when the chosen `Provider` came from the
   provider namespace.

4. **The provider serves namespaces by selector, using the same mechanism as
   the controller and the operator.** A provider gains
   `--watch-namespace-selector` (`BANLIEUE_WATCH_NAMESPACE_SELECTOR`), the
   flag name ADR-0131 gave the operator. `--namespace` keeps its meaning as
   the provider's **home namespace**: where its `Provider`, credentials Secret,
   CA bundle, leader-election Lease and heartbeat live. In selector mode it
   watches its machine kind with `Api::all` behind a selector-filtered
   `Namespace` reflector and gates each reconcile on the served set
   (`banlieue-provider-sdk`'s `Scope` and `NamespaceGate`, not a copy of
   them). The home namespace is always served. With no selector the provider
   serves its home namespace only, as today: never every namespace by default.
   An invalid selector fails startup, validated by the same probe LIST. An
   object being deleted bypasses the gate so finalizers still run; un-labelling
   a namespace stops reconciliation and deletes nothing (ADR-0130 Decision 6).

5. **The provider resolves its `Provider`, credentials and CA bundle from its
   home namespace, whatever namespace the machine is in.** It reads the
   `Provider` named by `--provider-name` in its home namespace, not
   `providerRef.name` in the machine's namespace. It acts on a machine only
   when `providerRef` resolves to that very `Provider` (namespace defaulting
   to the machine's) and ignores any other, which is not an error but another
   provider's work. The name half of that match already exists
   (`ctx.provider_name`, `vspheremachine.rs`); the namespace half is new. Its Secret and ConfigMap reads stay in its home namespace;
   CHAIN-002 is unchanged. This is the one behavioural change inside the
   provider, and it applies to the machine, `VMImage` and `Provider`
   reconcilers' lookups alike.

6. **A tenant must not be able to forge a placement.** A machine whose
   `providerRef` names a platform `Provider` is a placement that has skipped
   scheduling, the class and image checks and any accounting. In a team
   namespace the team can reach it only by creating the machine itself, so:
   - **RBAC:** no tenant identity holds a verb on machine CRs. Default deny.
   - **`ValidatingAdmissionPolicy`:** only the controller's identity may
     create a machine or set or change `spec.providerRef`, and the provider's
     identity is limited to `status` and finalizers. The VAP is the
     enforcement; RBAC is the default; neither relies on the other.
   - **Candidate, not decided here:** the provider also requiring an owning
     `VirtualMachine` for a machine in a foreign namespace, in the manner of
     ADR-0129's adoption confirmation.

7. **RBAC and the ceiling.** The provider's `ClusterRole` gains
   `get/list/watch/patch` on its machine kind (with `status` and
   `finalizers`) cluster-wide, and read-only `list/watch` on `Namespace` in
   selector mode, no write verb. RBAC cannot be scoped by label, so the
   selector gate is software and the `ClusterRole` is the ceiling: a
   compromised provider could patch machines in a namespace it is not
   serving. That is the same ceiling the controller and operator already
   have, and it is recorded in the threat model, not hidden. Secret access
   stays a namespaced `Role` in the home namespace only.

8. **`VSphereCluster` (CAPI) follows the same lookup.**
   `reconciler/vsphere_cluster.rs` lists `Provider`s in the cluster's
   namespace; it uses the same helper, so failure domains aggregate over the
   resulting set.

9. **userData and cloud-config are not copied anywhere.** They stay inline in
   the machine, in the tenant's own namespace, where they already were. The
   assumption that they are not secret still holds as banlieue's stance
   (cloud-init is configuration, not a credential store), but this design no
   longer moves them across a boundary.

10. **Out of scope, stated so they are not assumed:**
    - Machines created by a CAPI `Machine` controller in a tenant namespace
      (ADR-0129). They carry their own `providerRef`; whether one may name a
      platform `Provider` is an authorization question for the same VAP.
    - `VirtualMachineMove` across the tenant/provider boundary (ADR-0127), and
      the Cloud Hypervisor provider, which has a fixed namespace today.
    - Quota and per-tenant limits on a shared `Provider`. The selector is
      scope, not isolation (ADR-0130 Decision 7).
    - Firecracker, which does not exist yet: it is built shared-capable from its
      first commit (roadmap 20).

11. **A `Provider` declares the namespaces it serves: `spec.namespaceSelector`.**
    An optional `LabelSelector` on `Provider`. Absent means the `Provider`'s own
    namespace only (today's behaviour). It is the single source for both sides:
    - **The operator** renders `--watch-namespace-selector` onto the provider
      workload from it, so the provider's scope is never a second flag to keep
      in step by hand.
    - **The controller's fallback** (Decision 1) considers a provider-namespace
      `Provider` only if its `namespaceSelector` matches the labels of the VM's
      namespace. A `Provider` with none is never a fallback candidate. This is
      the per-namespace allow-list: a namespace reaches only the platform
      `Provider`s that declare it.
    - **Who may set it:** only platform identities. The `Provider` admission
      policies gate it, and a tenant cannot create a `Provider` in the provider
      namespace.
    The controller reads the VM namespace's labels with a read-only `get` on
    `Namespace`.
    **An empty selector (`{}`) serves nothing.** Kubernetes reads it as "every
    namespace", but banlieue never defaults to every namespace (ADR-0130,
    ADR-0131), and a blank `--watch-namespace-selector` means *unset*, so `{}`
    cannot be rendered onto the provider without turning into "home only" while
    the controller read it as "all". It is therefore never a fallback candidate
    and renders no flag.
    **Who may set it** is enforced, not assumed: setting or changing the field
    needs `update` on the virtual subresource `providers/namespaceselector`
    (`deploy/admission/provider-namespace-selector-authorization.yaml`, CEL
    `authorizer`), granted by the ClusterRole that file ships. An unchanged
    selector needs no grant.

## Consequences

- **Credentials never leave the provider's namespace.** `team-a` needs its
  label and nothing else: no `Provider`, no Secret, no provider pod, no Role.
- **Every ownership link is unchanged.** Pool to member, claim to member,
  `AgentSandbox` to claim and VM to machine are all same-namespace
  `ownerReference`s, as ADR-0047, ADR-0132 and mediatore assume. There are no
  cross-namespace finalizers or sweepers, no label-based event mapping, and
  no machine-name scheme.
- **The change is smaller on the controller and larger on the provider.** The
  controller gains a fallback lookup and sets one field. Each provider gains a
  scope and re-points three lookups at its home namespace. The vSphere
  provider is first; libvirt and Proxmox follow the same helper.
- **Capacity is counted across tenants.** The scheduler's capacity fit and
  spread count the VMs already placed on a provider. For a shared provider the
  controller therefore lists VMs across every served namespace, not the VM's own
  (`reconciler/provider_lookup.rs`, `placement_peers`). A VM elsewhere that names
  a same-named `Provider` of its own is counted too, which can only over-count,
  the safe direction for a fit check. `ScheduledPlacement` does not record the
  provider's namespace; adding it would make the count exact and is a follow-up.
- **Only the controller's `Provider` event wiring covers a selector scope.** In
  `--namespace` (single-namespace) mode the controller does not watch the
  provider namespace, so a change to a shared `Provider` reaches VMs on the next
  periodic requeue rather than at once. Selector mode, which a multi-tenant
  install uses, watches `Provider`s cluster-wide.
- **A CRD change.** `providerRef` gains an optional `namespace`, and `Provider`
  gains an optional `namespaceSelector`. There is no release yet (`banlieue`
  has no consumers), so this is recorded as a change.
- **One selector, two readers.** The controller's fallback and the provider's
  scope both derive from `Provider.spec.namespaceSelector`, so they cannot
  disagree. A namespace the *controller* does not serve (its own ADR-0130
  selector) is still skipped silently; a status condition for that is the
  existing ADR-0131 follow-up.
- **A shared provider has a larger blast radius.** One `Provider` serves many
  tenants, though only the namespaces it declares. This is the accepted trade
  for keeping the credentials out of the tenants, and goes in the threat model
  §8.
- Tests that must exist first: lookup precedence (local wins whole, empty falls
  back, unset means none); `providerRef.namespace` default and round-trip; the
  provider reading its `Provider` and credentials from its home namespace for a
  machine in another; the provider ignoring a machine that names another
  `Provider`; the gate (served, unserved, deleting); and a live check that a
  tenant identity can neither create a machine nor read the credentials Secret.

## Follow-ups (not in this ADR)

The machine status condition for an unserved namespace; `ScheduledPlacement`
recording the provider's namespace (so capacity peers count exactly); folding
the per-kind admission policies, now one multi-resource policy, into whatever a
fifth kind needs; per-tenant limits on a shared provider (threat model §8); and
proving the whole path on a real cluster, which no live test has done yet.
