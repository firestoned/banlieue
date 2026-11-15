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

## Checking it

```sh
kubectl get namespaces -l banlieue.io/tenant=true       # what is served
kubectl -n banlieue-system logs deploy/banlieue-controller | rg serving
```

A pool created in an unlabelled namespace gets no members and no status
update. Label the namespace and the pool fills within a reconcile.
