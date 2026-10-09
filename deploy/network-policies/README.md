<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# NetworkPolicy templates

Opt-in NetworkPolicies for banlieue pods (ADR-0094). They are not part of the
default install: a wrong egress rule breaks the product without an obvious
error, so you apply them on purpose, after reading this.

## What is here

| File | Selects (`app.kubernetes.io/component`) | Egress | Ingress |
| --- | --- | --- | --- |
| `00-default-deny.yaml` | every `app.kubernetes.io/name=banlieue` pod | none | none |
| `controller.yaml` | `controller` | DNS, API server | `metrics`, `health` |
| `operator.yaml` | `operator` | DNS, API server | `metrics`, `health` |
| `imagebuilder.yaml` | `imagebuilder` | DNS, API server | `metrics`, `health` |
| `provider-vsphere.yaml` | `provider-vsphere` | DNS, API server, TCP 443 | `metrics`, `health` |
| `provider-proxmox.yaml` | `provider-proxmox` | DNS, API server, TCP 8006 | `metrics`, `health` |
| `provider-libvirt.yaml` | `provider-libvirt` | DNS, API server, TCP 16514 | `metrics`, `health` |
| `vsphere-import.yaml` | `vsphere-import` (Job) | DNS, API server, TCP 443 | none |
| `libvirt-import.yaml` | `libvirt-import` (Job) | DNS, API server, TCP 16514 | none |
| `registry-push.yaml` | `registry-push` (Job) | DNS, TCP 443 | none |

- **DNS** is UDP and TCP 53 to pods labelled `k8s-app=kube-dns` in
  `kube-system`.
- **API server** is TCP 443 and 6443 to any address.
- **`metrics`** (8080) is reachable only from namespaces labelled
  `banlieue.io/monitoring=true`. **`health`** (8081) is open, because kubelet
  probes do not come from a pod.

Policies select pods by label, not by name, so the same files cover a
hand-installed provider and one the operator spawned.

## Apply

The files carry no namespace. Apply the whole directory to every namespace
that runs banlieue pods:

```sh
# Controller, operator, imagebuilder, hand-installed providers.
kubectl apply -n banlieue-system -f deploy/network-policies/

# Image-build and import Jobs.
kubectl apply -n banlieue-imagebuild -f deploy/network-policies/

# Each ProviderClass.spec.workloadNamespace in use, if it is not one of the above.
kubectl apply -n <workload-namespace> -f deploy/network-policies/

# Let your Prometheus namespace scrape metrics.
kubectl label namespace <monitoring-namespace> banlieue.io/monitoring=true
```

Applying all files everywhere is safe: a policy whose component does not run
in a namespace selects nothing there.

## Narrow the addresses

banlieue cannot know your API server, vCenter, Proxmox, libvirt or registry
addresses, so every rule except DNS restricts the port and leaves the
destination open. Edit your copy to add a `to:` with your CIDRs. Each rule's
comment shows where:

```yaml
    - to:
        - ipBlock:
            cidr: 192.0.2.10/32     # your API server
      ports:
        - protocol: TCP
          port: 6443
```

On vSphere, port 443 is both the API server rule and the vCenter rule; 443
stays open to any address until you narrow both.

## Not covered

- **Cloud Hypervisor** runs on the host under systemd, not in a pod.
- **kairos-operator `OSArtifact` build pods** in `banlieue-imagebuild` are
  created by kairos-operator with its own labels. The default deny does not
  select them.
- **OpenTelemetry**: if you enable trace export (ADR-0092), add an egress rule
  for your collector's address and port to each component that exports.
- **A registry on another port** (for example `registry.internal:5000`) needs
  that port added to `registry-push.yaml`.

`crates/banlieue-operator/tests/network_policies.rs` pins the shape of these
files. More detail: `docs/src/security/network-policies.md`.
