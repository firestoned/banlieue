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

**The providers** (`libvirt`, `vsphere`, `proxmox`) take no selector. They
serve `--namespace`, or the namespace they run in when it is unset, and no
longer default to every namespace. A provider can only read the credentials in
one namespace, so serving more would not work anyway. To serve another
namespace, run a provider there: the operator already does, one per
`Provider`.

## Checking it

```sh
kubectl get namespaces -l banlieue.io/tenant=true       # what is served
kubectl -n banlieue-system logs deploy/banlieue-controller | rg serving
```

A pool created in an unlabelled namespace gets no members and no status
update. Label the namespace and the pool fills within a reconcile.
