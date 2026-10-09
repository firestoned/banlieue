<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Network Policies

banlieue ships opt-in NetworkPolicy templates in `deploy/network-policies/`
(ADR-0094). Without them, every banlieue pod can reach anything on the pod
network and beyond, and anything in the cluster can reach its ports. With
them, each component can talk only to what it needs.

They are not applied by the default install. A wrong egress rule breaks
banlieue quietly (a provider that cannot reach its backend just stops making
progress), so applying them is a deliberate step.

## What the policies allow

`00-default-deny.yaml` denies all ingress and egress for every pod labelled
`app.kubernetes.io/name=banlieue`. One allow policy per component then opens
what that component needs, selected by `app.kubernetes.io/name=banlieue` plus
`app.kubernetes.io/component`:

| Component | Runs as | Egress | Ingress |
| --- | --- | --- | --- |
| `controller` | Deployment | DNS, API server | `metrics`, `health` |
| `operator` | Deployment | DNS, API server | `metrics`, `health` |
| `imagebuilder` | Deployment | DNS, API server | `metrics`, `health` |
| `provider-vsphere` | Deployment (hand-installed or operator-spawned) | DNS, API server, vCenter TCP 443 | `metrics`, `health` |
| `provider-proxmox` | operator-spawned Deployment | DNS, API server, Proxmox VE API TCP 8006 | `metrics`, `health` |
| `provider-libvirt` | operator-spawned Deployment | DNS, API server, libvirtd TLS TCP 16514 | `metrics`, `health` |
| `vsphere-import` | Job in the image-build namespace | DNS, API server, TCP 443 (vCenter and the ESXi hosts behind each datastore) | none |
| `libvirt-import` | Job in the image-build namespace | DNS, API server, TCP 16514 | none |
| `registry-push` | Job in the image-build namespace | DNS, registry TCP 443 | none |

The rules behind those words:

- **DNS**: UDP and TCP 53, only to pods labelled `k8s-app=kube-dns` in the
  `kube-system` namespace.
- **API server**: TCP 443 and 6443 to any address. The API server is not a
  pod, so it cannot be matched by a pod selector, and its address differs per
  cluster. Port 6443 is there because many CNIs evaluate policy after the
  `kubernetes` Service has been translated to the API server's real port.
- **Backends**: only the backend's port, to any address.
- **`metrics`** (8080): only from pods in namespaces labelled
  `banlieue.io/monitoring=true`.
- **`health`** (8081): from anywhere. Kubelet probes do not come from a pod,
  so they cannot be matched by a selector.

The registry push Job runs without a service account token and never calls
the API server, so it gets no API server rule.

Because policies select by label rather than by name, the same file covers a
provider you installed from `deploy/provider-vsphere/` and one the operator
spawned for a `Provider`: both carry `app.kubernetes.io/component=provider-<backend>`.

## Applying them

The manifests carry no namespace, so you apply the directory once per
namespace that runs banlieue pods:

```sh
# Controller, operator, imagebuilder and any hand-installed provider.
kubectl apply -n banlieue-system -f deploy/network-policies/

# Image-build namespace: the import and registry push Jobs.
kubectl apply -n banlieue-imagebuild -f deploy/network-policies/
```

Operator-spawned providers run in `ProviderClass.spec.workloadNamespace`, or
in the `Provider`'s own namespace when that field is unset. Apply the same
directory there too:

```sh
kubectl get providerclass -o custom-columns=NAME:.metadata.name,NS:.spec.workloadNamespace
kubectl apply -n <workload-namespace> -f deploy/network-policies/
```

Applying every file in every namespace is harmless: a policy whose component
does not run in that namespace selects no pods.

Then let your monitoring namespace scrape metrics:

```sh
kubectl label namespace <monitoring-namespace> banlieue.io/monitoring=true
```

The default deny selects banlieue pods only. Other pods in the same
namespace, such as kairos-operator's `OSArtifact` build pods in
`banlieue-imagebuild`, are not affected by these policies.

## Narrowing the destinations

Every rule except DNS restricts the port and leaves the destination open,
because banlieue cannot know your addresses. To narrow one, add a `to:` with
your CIDRs to your copy of the file. Each rule's comment shows the spot. For
example, pinning the API server and a libvirt host network:

```yaml
  egress:
    - to:
        - ipBlock:
            cidr: 192.0.2.10/32        # API server
      ports:
        - protocol: TCP
          port: 443
        - protocol: TCP
          port: 6443
    - to:
        - ipBlock:
            cidr: 198.51.100.0/24      # libvirt hosts
      ports:
        - protocol: TCP
          port: 16514
```

Find the API server address with
`kubectl get endpointslices -n default -l kubernetes.io/service-name=kubernetes`.

On vSphere, TCP 443 appears in both the API server rule and the vCenter rule,
so 443 is open to any address until you narrow both. Some CNIs do not match
the API server by `ipBlock` at all (it is host-network traffic); check your
CNI's documentation before narrowing that rule.

Other site edits:

- **A registry on another port** (for example `registry.internal:5000`, or
  plain HTTP) needs that port added to `registry-push.yaml`.
- **OpenTelemetry tracing** (ADR-0092): when you set
  `OTEL_EXPORTER_OTLP_ENDPOINT`, each exporting component needs one more
  egress rule for the collector's address and port. The shipped policies do
  not guess it, so without that rule spans are dropped.

## Out of scope

- **Cloud Hypervisor provider.** It runs on the hypervisor host under
  systemd, not in a pod, so a NetworkPolicy cannot select it. Restrict it
  with the host's own firewall.
- **kairos-operator build pods.** They are created and labelled by
  kairos-operator, not banlieue, and pull from whatever registries the image
  source names.

## How the shape is kept honest

`crates/banlieue-operator/tests/network_policies.rs` parses every manifest in
`deploy/network-policies/` and fails if:

- the default deny is missing;
- a component with a Deployment under `deploy/`, a provider the operator
  spawns, or a banlieue Job has no policy;
- a selector uses label keys or values other than the ones the operator
  stamps;
- any egress rule allows a port outside UDP/TCP 53, TCP 443, 6443, 8006 and
  16514, or a component gets a port it does not need;
- any ingress rule allows something other than `metrics` and `health`.

It checks the files in the repository, not your edited copies.
