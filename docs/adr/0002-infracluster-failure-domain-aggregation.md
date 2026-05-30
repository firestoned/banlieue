<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0002 — InfraCluster CRD with multi-Provider failure-domain aggregation

- **Status:** Accepted
- **Date:** 2026-05-30
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0001 (CAPI-native provisioning); D-003, D-005, D-009; non-negotiable #1 (CRD-only), #4 (explicit over implicit).

## Context

ADR-0001 commits banlieue to acting as a CAPI infrastructure provider.
CAPI's `Cluster.spec.infrastructureRef` must point at an **InfraCluster**
object whose status advertises the failure domains the cluster's machines
are spread across. banlieue ships `VSphereMachine` (InfraMachine) and
`VSphereMachineTemplate`, but **no InfraCluster** — so there is currently
no contract-compliant place to publish failure domains for cluster-side
spread.

The CAPI **v1beta2** InfraCluster contract (verified against the official
contract docs, 2026-05-30) requires:

- CRD label `cluster.x-k8s.io/v1beta2: <crd-version>` (banlieue uses
  value `v1alpha1`, matching `VSphereMachine`).
- `spec.controlPlaneEndpoint` — optional `{ host: string, port: int32 }`.
- `status.initialization.provisioned: *bool` — replaces deprecated
  `status.ready`.
- `status.failureDomains` — a **list** (changed from the v1beta1 map):
  `[]FailureDomain { name: string, controlPlane: *bool, attributes: map[string]string }`.
- `spec.paused` + `cluster.x-k8s.io/paused` annotation handling,
  surfaced via a `Paused` condition.
- `status.conditions`; a `Ready` condition is mirrored to the parent
  `Cluster`'s `InfrastructureReady`.

banlieue has a distinguishing requirement: **one Kubernetes cluster may
span multiple backends.** The motivating case is one cluster across 2
vCenters × 3 compute clusters = 6 failure domains. In banlieue each
vCenter is one `Provider` CR, so the InfraCluster must aggregate failure
domains from *multiple* `Provider`s — unlike CAPV's `VSphereCluster`,
which is bound to a single vCenter.

## Decision

Add `infrastructure.banlieue.io/v1alpha1` **`VSphereCluster`**, banlieue's
reference InfraCluster. Naming parallels `VSphereMachine` (and CAPV);
the `infrastructure.banlieue.io` group disambiguates it from CAPV's
`infrastructure.cluster.x-k8s.io/VSphereCluster`.

### Spec

- `controlPlaneEndpoint: Option<ApiEndpoint>` — user-supplied control-plane
  VIP (`{host, port}`). Optional; with k0smotron the endpoint may instead
  be managed by the control-plane provider. Mirrored to status when set.
- `providerSelector: LabelSelector` **and/or** `providerRefs: [LocalObjectReference]`
  — which `Provider`s (same namespace) to aggregate failure domains from.
  Explicit refs win where both are set. Declaring the set explicitly
  honours non-negotiable #4 (explicit over implicit).
- `controlPlaneFailureDomainSelector: Option<LabelSelector>` — which of the
  aggregated failure domains are eligible to run control-plane nodes
  (matched against the Provider FD `labels`). Default: all eligible
  (`controlPlane: true`). This is how an operator keeps the etcd quorum
  to an odd, bounded set of domains while workers spread wider.
- `paused: bool` — in-band pause (mirrors the `cluster.x-k8s.io/paused`
  annotation).

### Status (CAPI v1beta2)

- `initialization: InitializationStatus` (`provisioned`).
- `controlPlaneEndpoint: Option<ApiEndpoint>` — echoes the resolved endpoint.
- `failureDomains: [ClusterFailureDomain]` — CAPI-shaped list
  (`name`, `controlPlane`, `attributes`), translated from the selected
  Providers' `status.failureDomains[]`. The banlieue FD `name` carries
  through; `attributes` is flattened from the Provider FD's `attributes.raw`
  plus `dc`/`cluster` labels; `controlPlane` is set from
  `controlPlaneFailureDomainSelector`.
- `conditions: [metav1.Condition]` — `Ready` (→ `InfrastructureReady`),
  `Paused`.
- `observedGeneration`.

### Reconciliation owner

**The main controller reconciles `VSphereCluster`, not the vSphere
provider.** Aggregation only reads `Provider.status.failureDomains[]` —
which the provider's controller already populated by talking to vCenter —
so no backend access is needed. This preserves D-003/#1 (the main
controller never touches a backend) *for free*, and keeps the provider
focused on machines (InfraMachine) and capability introspection.

### Failure-domain → `controlPlane` and the k0s quorum

The spread count from ADR-0001 ("platinum = 6/6") is the CAPI
`MachineDeployment`/control-plane `replicas`, **not** a banlieue field.
For k0s, the control-plane replica count must respect etcd quorum (1/3/5)
— so a 6-domain "platinum" cluster typically means **3 control-plane
nodes** (across 3 control-plane-eligible FDs) and **workers spread across
all 6**. `controlPlaneFailureDomainSelector` expresses the control-plane
subset; worker `MachineDeployment`s spread over the full set.

### Capacity-aware placement (the two traps)

Per ADR-0001: in the CAPI path banlieue does **not** capacity-weight FD
selection (CAPI round-robins). Instead:
- the provider publishes per-FD utilisation into
  `Provider.status.failureDomains[].attributes` and **omits** FDs over a
  capacity threshold; the `VSphereCluster` reconciler therefore simply
  won't advertise them; and
- **DRS** picks the ESXi host within the chosen compute cluster — banlieue
  creates the VM against the cluster's resource pool and lets DRS place it.

### Shared types (added to `common.rs`)

- `ApiEndpoint { host: String, port: i32 }` — CAPI `APIEndpoint`.
- `ClusterFailureDomain { name: String, control_plane: Option<bool>, attributes: BTreeMap<String,String> }`
  — CAPI v1beta2 `clusterv1.FailureDomain` (list element).

## Consequences

**Positive**
- banlieue becomes a complete CAPI infra provider (cluster + machine).
- Multi-vCenter clusters are a first-class capability, beyond CAPV.
- No new backend access path; the contract stays CRD-only.

**Negative / follow-ups**
- New reconciler in the main controller (tracked separately).
- The CRD needs the `cluster.x-k8s.io/v1beta2: v1alpha1` contract label
  (kube-derive cannot emit CRD-level labels). Resolved in **ADR-0005**: emitted
  by `crdgen` post-processing for all `infrastructure.banlieue.io` CRDs — same
  treatment as `VSphereMachine`. (Originally noted here as a kustomize overlay;
  superseded by the code-first approach.)
- New RBAC: main controller needs `get/list/watch` on `providers` and
  `patch` on `vsphereclusters/status`.
- `VSphereClusterTemplate` (for CAPI ClusterClass / topology) is **not**
  added now; deferred until ClusterClass support is required.
- Proxmox/libvirt InfraClusters follow the same pattern in their phases.

## Alternatives considered

- **One generic `BanlieueCluster` InfraCluster across all classes.**
  Rejected for v1alpha1: the InfraMachineTemplate is already per-class
  (`VSphereMachineTemplate`), so a MachineDeployment pins a class anyway;
  a per-class InfraCluster is more CAPI-idiomatic and clearer.
- **One InfraCluster per Provider (CAPV semantics).** Rejected: a
  cluster spanning 2 vCenters would become 2 CAPI clusters, defeating the
  motivating use case.
- **Reconcile `VSphereCluster` in the vSphere provider.** Rejected:
  aggregation needs no vCenter access; doing it in the main controller
  keeps provider scope tight and avoids granting the provider read access
  to every Provider's status.
