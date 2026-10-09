<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0094: Opt-in NetworkPolicy templates, selected by component label

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 12 §4.4; [ADR-0003](0003-provider-deployment-topology.md)
  ("per-backend NetworkPolicy becomes expressible");
  [ADR-0012](0012-providerclass-crd-and-operator-role.md) (operator-spawned
  workloads and their labels); [ADR-0016](0016-imagebuild-namespace-isolation.md);
  [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md) (push-Job egress follow-up)

## Context

Nothing under `deploy/` ships a NetworkPolicy. Every banlieue pod can reach
anything on the pod network and anything beyond it, and anything in the
cluster can reach its ports. What each component actually needs is narrow:

| Component | Needs to reach |
| --- | --- |
| controller, operator, imagebuilder | the Kubernetes API server, DNS |
| provider-vsphere | API server, DNS, vCenter HTTPS (443) |
| provider-proxmox | API server, DNS, Proxmox VE API (8006) |
| provider-libvirt | API server, DNS, libvirtd over mutual TLS (16514) |
| image-build and import Jobs | API server, DNS, an OCI registry or datastore (443) |

Inbound, the only ports are `metrics` (8080) and `health` (8081).

Three facts shape any policy:

- **Endpoints are deployment-specific.** banlieue cannot know the addresses
  of a site's vCenter, Proxmox nodes, libvirt hosts or registry.
- **The API server is not a pod.** It cannot be matched by `podSelector`,
  and its address differs per cluster.
- **Operator-spawned provider pods** run in `ProviderClass.spec.workloadNamespace`,
  not necessarily `banlieue-system`. They carry
  `app.kubernetes.io/component=provider-<backend>`, the same label the
  hand-installed vSphere provider has.

## Decision

1. **Ship policies as opt-in manifests** under `deploy/network-policies/`,
   applied per namespace that runs banlieue pods. They are not part of the
   default install, because a wrong egress rule breaks the product silently.

2. **Default deny, then one allow-list policy per component**, selected by
   `app.kubernetes.io/name=banlieue` plus `app.kubernetes.io/component`.
   Because the component label is what selects them, the same policies
   cover hand-installed and operator-spawned providers in any namespace they
   are applied to.

3. **Egress rules restrict ports, and addresses only where banlieue knows
   them:**
   - DNS: UDP/TCP 53 to pods labelled `k8s-app=kube-dns` in `kube-system`.
   - API server: TCP 443 and 6443 to any address. The address is per
     cluster; the comment in each file says to replace this with the
     cluster's API server CIDR.
   - Backends: the provider's backend port only (443, 8006, 16514) to any
     address, again with a comment to narrow it to the site's CIDRs.
   - Nothing else.

4. **Ingress allows only `metrics` and `health`.** Metrics come from pods in
   a namespace labelled `banlieue.io/monitoring=true`, so the scraping
   namespace is an explicit, labelled choice. Health is open, because the
   kubelet's probes do not come from a pod.

5. **A test pins the shape.** A test parses every manifest in
   `deploy/network-policies/` and asserts that:
   - every component that has a Deployment, or is spawned by the operator,
     has a policy;
   - no policy allows egress on a port outside the table above;
   - no policy allows ingress beyond `metrics` and `health`.

## Consequences

- The threat model's unaddressed lateral-movement path gains a control that
  exists in the tree, with "opt-in, and egress addresses are as wide as the
  site leaves them" recorded as the accepted residue.
- Narrowing destination CIDRs is a site edit, documented in each file and in
  the hardening guide. banlieue does not template it: there is no Helm chart
  and none is planned.
- The OpenTelemetry collector (ADR-0092), when enabled, needs an extra
  egress rule. The guide says so; the shipped policies do not guess its
  address.
- Operator-generated per-workload policies (the operator writing a
  NetworkPolicy beside each provider Deployment) would need new RBAC for the
  operator. That stays a follow-up; the label-selected templates already
  cover those pods.
