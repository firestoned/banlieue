# Guide: giving each team its own namespace

`banlieue controller` serves only the namespaces you choose
([ADR-0130](https://github.com/firestoned/banlieue/blob/main/docs/adr/0130-namespace-selector-multi-tenancy.md)).
A team gets a namespace, you label it, and the controller starts reconciling
that team's `VirtualMachinePool`, `VirtualMachineClaim` and `VirtualMachine`
objects. Nothing is served until it is chosen.

!!! warning "This is scope, not isolation"
    The selector decides **which namespaces are served**. It does not stop a
    team naming another team's `VMClass`, `VMImage` or `Provider` selector, and
    it applies no quota. Isolation today is the namespaced `Role` you give each
    team; admission policy and quota per tenant are a follow-up decision. See
    the accepted risks in the [threat model](../security/threat-model.md).

## How the scope is chosen

In order, the first that applies wins:

| # | Setting | Serves |
| --- | --- | --- |
| 1 | `--namespace-selector` / `BANLIEUE_NAMESPACE_SELECTOR` | every namespace whose labels match |
| 2 | `--namespace` / `BANLIEUE_NAMESPACE` | exactly that namespace |
| 3 | neither | the namespace the controller runs in |

The controller **never defaults to every namespace.** If it cannot find any
of the three it refuses to start. The startup log names the rule that applied:

```text
serving scope="namespaces matching selector \"banlieue.io/tenant=true\""
```

A selector the API server cannot parse also stops startup, with the server's
error.

## Onboard a team

1. Set the selector on the controller. In `deploy/controller/configmap.yaml`:

    ```yaml
    BANLIEUE_NAMESPACE_SELECTOR: "banlieue.io/tenant=true"
    ```

2. Create the team's namespace and label it. The label is what opts it in:

    ```sh
    kubectl create namespace team-a
    kubectl label namespace team-a banlieue.io/tenant=true
    ```

3. Give the team a namespaced `Role` and `RoleBinding` for its own objects.
   Use a `Role`, never a `ClusterRole`.

4. If the team's `VirtualMachine`s name a `spec.userData` Secret or
   ConfigMap, let the controller read them there. The controller resolves
   `userData` in the VM's own namespace, and the shipped
   `deploy/controller/rbac/role.yaml` grants that only in `banlieue-system`.
   Copy that `Role` and its `RoleBinding` into the team's namespace (change
   `metadata.namespace`, keep the subject `banlieue-controller` in
   `banlieue-system`). Without it, such a VM reports a `userData` resolution
   failure; a VM with inline `userData`, or none, needs nothing here. The
   `virtualmachine-userdata-authorization` admission policy (ADR-0042) still
   decides which Secrets a team may name.

The controller notices the label at once: no restart, and everything already
in the namespace is reconciled.

!!! important "Who may label"
    Do not give tenants `update` or `patch` on `namespaces`. That permission
    is what lets a namespace opt itself in. Select on a label only a platform
    admin can set.

If the controller's own namespace holds the `Provider`s, it must carry the
label too, or the controller will not see them.

## Take a team out

```sh
kubectl label namespace team-a banlieue.io/tenant-
```

The controller stops reconciling that namespace. **Nothing is deleted**, and
status is left as last written, so a `VirtualMachine` there can still show
`Ready` while it is no longer being watched. Deleting an object still works:
its finalizer runs even in an unserved namespace.

## Permissions the controller needs

Selector mode lists and watches `Namespace` objects. The shipped
`deploy/controller/rbac/clusterrole.yaml` grants `get`, `list` and `watch` on
`namespaces` and nothing else; the controller cannot relabel a namespace. If
that grant is missing the controller fails at startup and never reports ready.

## The operator and the providers

The controller is one of three binaries that read a namespace scope
([ADR-0131](https://github.com/firestoned/banlieue/blob/main/docs/adr/0131-operator-and-provider-namespace-scope.md)).
A team's `Provider`s live in the team's namespace, so the operator has to
serve that namespace too, or its `Provider`s get no workload.

**The operator** takes the same three-step choice, with its own flag names
(its `--namespace` means where it *runs*, so it is not reused as the scope):

| # | Setting | Serves |
| --- | --- | --- |
| 1 | `--watch-namespace-selector` / `BANLIEUE_WATCH_NAMESPACE_SELECTOR` | the `Provider`s in every namespace whose labels match |
| 2 | `--watch-namespace` / `BANLIEUE_WATCH_NAMESPACE` | exactly that namespace |
| 3 | neither | the namespace the operator runs in (`--namespace`) |

It no longer watches every namespace by default. To serve tenants, give it
the **same label** as the controller:

```yaml
BANLIEUE_WATCH_NAMESPACE_SELECTOR: "banlieue.io/tenant=true"
```

If you install with `banlieue bootstrap operator`, pass it at install time
instead of editing the Deployment afterwards:

```sh
banlieue bootstrap operator --watch-namespace-selector banlieue.io/tenant=true
```

Keep the two selectors aligned. If the controller serves a namespace the
operator does not, a `VirtualMachine` there can schedule onto a `Provider`
that never gets a workload. Un-labelling a namespace stops the operator
reconciling its `Provider`s but deletes nothing; a `Provider` being deleted
still has its workload torn down. The operator's `ClusterRole` grants
read-only `get`, `list` and `watch` on `namespaces` for this.

**The providers** keep their credentials in one namespace, their **home**
namespace (`--namespace`, or the namespace they run in). That is where their
`Provider`, credentials Secret and CA bundle live, and they never read a
Secret anywhere else. Every provider can also *serve* other namespaces
(`vsphere`, `libvirt`, `proxmox` and `cloud-hypervisor`), which is how one
provider serves many teams without any team ever holding the hypervisor
credentials. See the next section.

## One shared provider for every team

Putting a `Provider` and its credentials in each team's namespace would hand
the team's Secret readers the vCenter login. Instead keep one `Provider` in the
platform namespace and let it declare the namespaces it serves
([ADR-0133](https://github.com/firestoned/banlieue/blob/main/docs/adr/0133-shared-provider-serves-tenant-namespaces.md)).
Everything else stays in the team's namespace: the pool, its VMs, the claims,
the `AgentSandbox`es and the infrastructure machine CRs. Only the **reference**
to the provider crosses a namespace.

1. Declare who the `Provider` serves. In the platform namespace, on the
   `Provider` that holds the credentials:

    ```yaml
    apiVersion: banlieue.io/v1alpha1
    kind: Provider
    metadata:
      name: vcenter
      namespace: banlieue-system
    spec:
      providerClassRef:
        name: vsphere
      namespaceSelector:
        matchLabels:
          banlieue.io/tenant: "true"
      connection:
        endpoint: https://vcenter.example.com/sdk
        credentialsRef:
          name: vcenter-creds
    ```

    The operator renders `--watch-namespace-selector` onto the provider from
    this field, so the provider and the controller read the same selector and
    cannot disagree. An absent selector serves the home namespace only, and an
    **empty** one (`{}`) serves nothing: banlieue never defaults to every
    namespace.

2. Tell the controller where the shared providers are:

    ```yaml
    BANLIEUE_PROVIDER_NAMESPACE: "banlieue-system"
    ```

3. Label the team's namespace, as above. A team namespace with no `Provider` of
   its own is now scheduled onto the shared one.

The lookup is: the VM's own namespace first, then the provider namespace. If a
team namespace holds **any** `Provider`, those are the only candidates, so a
selector that matches nothing there fails with `NoProviderMatched` instead of
landing on the shared platform. Only a `Provider` that declares the VM's
namespace is ever a fallback.

!!! important "Apply the admission policies"
    A machine that names a provider in another namespace is a placement that
    skipped scheduling. Apply both policies in
    [`deploy/admission/`](https://github.com/firestoned/banlieue/tree/main/deploy/admission):

    * `machine-provider-ref.yaml` lets only the controller set
      `providerRef.namespace`, so a team cannot write a machine that names the
      platform's provider.
    * `provider-namespace-selector-authorization.yaml` makes setting
      `namespaceSelector` need `update` on `providers/namespaceselector`. Bind
      the `banlieue-provider-namespaceselector-admin` ClusterRole it ships to
      platform admins only.

    Give teams **no verb on machine CRs** (`vspheremachines`, `libvirtmachines`,
    `proxmoxmachines`, `cloudhypervisormachines`). They need to see
    their pool, VMs and claims, not the machines behind them.

!!! note "Cloud Hypervisor hosts are opt-in and read the selector at startup"
    A Cloud Hypervisor provider runs on the KVM host with a bound
    ServiceAccount token, not as a pod
    ([ADR-0134](https://github.com/firestoned/banlieue/blob/main/docs/adr/0134-cloud-hypervisor-serves-tenant-namespaces.md)).
    It reads its `namespaceSelector` from its own `Provider` **once, at
    startup**, so changing the selector needs a provider restart. A host with
    no selector keeps a token that reaches machines in its own namespace only.
    With a selector, the operator binds a second ClusterRole,
    `banlieue-provider-cloud-hypervisor-shared`, which lets the host's token
    read and patch machines in the namespaces it serves (RBAC cannot be scoped
    by label, so it can in every namespace). It holds no Secret or ConfigMap.
    That is a wider stolen-host-token reach than a single-namespace host has,
    so enable it only on hosts that must serve tenants.

!!! note "What the shared provider can and cannot reach"
    Its `ClusterRole` can read and patch `VSphereMachine`s in any namespace and
    list and watch `Namespace`s. RBAC cannot be scoped by label, so the
    selector is the gate and the `ClusterRole` is the ceiling. It has **no**
    Secret or ConfigMap access outside its home namespace, so a team that can
    read Secrets in its own namespace cannot read the credentials. A team's
    `userData` is rendered by the controller into the machine in the team's own
    namespace; the provider never reads it from a Secret.

## Checking it

```sh
kubectl get namespaces -l banlieue.io/tenant=true       # what is served
kubectl -n banlieue-system logs deploy/banlieue-controller | rg serving
```

A pool created in an unlabelled namespace gets no members and no status
update. Label the namespace and the pool fills within a reconcile.
