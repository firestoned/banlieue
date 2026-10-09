<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Cluster API: banlieue as a clusterctl infrastructure provider

banlieue's infrastructure CRDs (`VSphereMachine`, `ProxmoxMachine`,
`LibvirtMachine`, `CloudHypervisorMachine`, their templates, and
`VSphereCluster`) satisfy the Cluster API **v1beta2** InfraMachine and
InfraCluster contracts. Each CRD carries the `cluster.x-k8s.io/v1beta2:
v1alpha1` label, which is how Cluster API finds the version to use
(ADR-0005). This guide installs banlieue next to Cluster API with
`clusterctl`, so a CAPI `Machine` can reference a banlieue infrastructure
object. The decision is ADR-0096 (`docs/adr/` in the repository).

## What banlieue provides, and what it does not

- **The infrastructure role only.** banlieue is one clusterctl provider,
  `banlieue`, of type `InfrastructureProvider`. It is not one provider per
  backend: every backend is compiled into the same binary, and a `Provider`
  resource picks the backend at run time (see
  [Provider Lifecycle](provider-lifecycle.md)).
- **Bootstrap and control plane come from upstream.** banlieue ships no
  bootstrap or control-plane provider. Use the ones you already use, for
  example kubeadm (the `clusterctl init` default) or k0smotron.
- **The same install as `banlieue bootstrap operator`.** The release asset
  `infrastructure-components.yaml` is that command's `--dry-run` output for
  the release's image tag, in the `banlieue-system` namespace. It needs no
  clusterctl variables.

## Prerequisites

- A management cluster running Kubernetes v1.33 or newer.
- `clusterctl` from a Cluster API release that serves the v1beta2 contract
  (v1.11 or newer). banlieue's contract e2e runs against Cluster API
  v1.14.3.

## 1. Register the provider with clusterctl

`banlieue` is not in clusterctl's built-in provider list, so declare it in a
clusterctl configuration file. The URL names the release's components file;
clusterctl reads `metadata.yaml` from the same release.

```yaml
# ~/.config/cluster-api/clusterctl.yaml
providers:
  - name: banlieue
    type: InfrastructureProvider
    url: https://github.com/firestoned/banlieue/releases/v0.4.0/infrastructure-components.yaml
```

To always resolve the newest release, use `releases/latest/` in place of
`releases/v0.4.0/`.

## 2. Initialise the management cluster

```sh
clusterctl init --infrastructure banlieue
```

On a cluster without Cluster API this also installs cert-manager, CAPI core,
and the kubeadm bootstrap and control-plane providers. To pin versions, name
them:

```sh
clusterctl init \
  --core cluster-api:v1.14.3 \
  --bootstrap kubeadm:v1.14.3 \
  --control-plane kubeadm:v1.14.3 \
  --infrastructure banlieue:v0.4.0
```

`clusterctl` reads the target namespace, `banlieue-system`, from the
components file. Then register a backend exactly as without Cluster API:
apply a `Provider` (and its credentials Secret) as described in the
[vSphere](vsphere-provider.md), [libvirt](libvirt-provider.md) or
[Proxmox](proxmox-provider.md) guides.

## 3. Reference banlieue objects from Cluster API

A `Cluster` and a `Machine` name banlieue kinds through `infrastructureRef`.
In v1beta2 the reference carries `apiGroup`, `kind` and `name`, and Cluster
API reads the CRD's contract label to choose the version:

```yaml
apiVersion: cluster.x-k8s.io/v1beta2
kind: Cluster
metadata:
  name: workload
  namespace: default
spec:
  infrastructureRef:
    apiGroup: infrastructure.banlieue.io
    kind: VSphereCluster
    name: workload
  # controlPlaneRef: your control-plane provider's object (kubeadm, k0smotron, ...)
---
apiVersion: infrastructure.banlieue.io/v1alpha1
kind: VSphereCluster
metadata:
  name: workload
  namespace: default
spec:
  controlPlaneEndpoint:
    host: 192.0.2.10
    port: 6443
  providerRefs:
    - name: vcenter-a
```

Machines are usually stamped from a `VSphereMachineTemplate` by a
`MachineDeployment` or a control-plane provider; the template's
`spec.template.spec` is a `VSphereMachine` spec.

What each side writes, per the contract:

| Field | Written by |
| --- | --- |
| `VSphereMachine.spec.providerID` | the banlieue provider, once the VM exists |
| `VSphereMachine.status.initialization.provisioned` | the banlieue provider |
| `VSphereMachine.status.addresses` | the banlieue provider |
| `Machine.status.initialization.infrastructureProvisioned`, `Machine.spec.providerID`, `Machine.status.addresses` | Cluster API, copied from the above |
| owner reference and `cluster.x-k8s.io/cluster-name` label on the `VSphereMachine` | Cluster API |
| deleting the `VSphereMachine` when its `Machine` is deleted | Cluster API |

## RBAC: the scoped aggregate role

Cluster API's manager is authorised on banlieue objects by one ClusterRole,
`banlieue-capi-infrastructure` (`deploy/capi/clusterrole-aggregate.yaml`).
It carries `cluster.x-k8s.io/aggregate-to-manager: "true"`, so CAPI's
aggregated manager role picks its rules up with no extra binding. It grants:

- `get`, `list`, `watch`, `create`, `update`, `patch`, `delete`
- on `infrastructure.banlieue.io` only: every machine, machine-template and
  cluster kind, and the `/status` subresource of those that have one.

It grants nothing in `banlieue.io` (no `VirtualMachine`, `Provider`, ...),
and no Secrets, IPAM claims, Events or Leases. `banlieue bootstrap operator`
and therefore `clusterctl init` install it; a GitOps install applies the file
directly:

```sh
kubectl apply -f deploy/capi/clusterrole-aggregate.yaml
```

### Upgrading from v0.4.0 or earlier

Earlier releases put the aggregation label on the whole `banlieue-controller`
ClusterRole, which handed Cluster API's manager create and delete on every
`banlieue.io` kind. Re-apply the RBAC (`banlieue bootstrap operator`, or
`kubectl apply` of `deploy/controller/rbac/` and `deploy/capi/`): the label
disappears with the re-applied controller role, and the new role restores
exactly the permissions Cluster API needs.

## How the contract is tested

`make kind-e2e-capi` runs the contract against real Cluster API controllers,
with no hypervisor. It creates a fresh kind cluster, runs `clusterctl init`
with Cluster API pinned in the Makefile (`CAPI_VERSION`), installs banlieue
from a local clusterctl repository rendered from the working tree, then plays
the provider: it sets `provisioned`, `providerID` and RFC 5737 addresses on a
`VSphereMachine`, asserts the CAPI `Machine` reflects them, and asserts that
deleting the `Machine` (then the `Cluster`) deletes the banlieue object. The
cluster is deleted afterwards. A backend provisioning a real VM for a CAPI
`Machine` stays a live test against a real host.

To build the release assets locally:

```sh
make clusterctl-components   # target/clusterctl/release/{infrastructure-components,metadata}.yaml
```
