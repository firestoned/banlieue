# Infrastructure CRDs & CAPI

banlieue's provider infrastructure CRDs satisfy the
[Cluster API (CAPI) v1beta2 infrastructure contracts](https://cluster-api.sigs.k8s.io/developer/providers/contracts/)
— the **InfraMachine** contract (`VSphereMachine`) and the **InfraCluster**
contract (`VSphereCluster`).

This page explains *what those contracts are*, *why banlieue uses them*, and
*what you get for free* by piggybacking on them.

## What is the CAPI InfraMachine contract?

CAPI is the upstream Kubernetes project for declarative cluster lifecycle.
Every CAPI cluster uses an **infrastructure provider** (CAPV for vSphere,
CAPP for Proxmox, CAPL for libvirt, etc.) that owns the actual machines. To
keep providers interoperable, CAPI defines a versioned **contract** — a set
of expectations about what an infrastructure CRD's spec and status must look
like.

The v1beta2 InfraMachine contract specifies, among other things:

- A `spec` shape that can be templated (machine + machine template).
- `status.initialization.provisioned` — a boolean that replaces the deprecated
  v1beta1 `status.ready`.
- A `status.conditions[]` array with `metav1.Condition` shape. Terminal failures
  are expressed as conditions, **not** the deprecated `status.failureReason` /
  `status.failureMessage` fields.
- A `status.addresses[]` list of `MachineAddress` entries (IP, hostname).
- `spec.providerID` discoverable after provisioning, and `spec.failureDomain` for
  placement.
- Owner-reference and finaliser conventions.

CAPI discovers which CRDs implement a contract via a CRD-level label,
`cluster.x-k8s.io/v1beta2: v1alpha1`. Since `kube-derive` cannot emit CRD labels,
banlieue stamps it onto every `infrastructure.banlieue.io` CRD during generation
(`crdgen`); see
[ADR-0005](https://github.com/firestoned/banlieue/blob/main/docs/adr/0005-capi-contract-label-codegen.md).

It is, in practical terms, the result of years of CAPI providers converging
on what an "infrastructure object" actually needs to expose.

## Why banlieue adopts it

We had three options for what shape provider CRDs should take:

1. **Invent our own contract.** Tempting (we'd get exactly the fields we
   want), but it would not interoperate with CAPI providers, and we'd be
   re-litigating decisions the CAPI community already made carefully.
2. **Copy CAPI's shape, but separately.** Reduces interop and doubles the
   surface to maintain.
3. **Adopt the CAPI v1beta2 contract verbatim.** Lose nothing, gain
   compatibility, inherit a battle-tested status model.

We picked (3). Banlieue's infrastructure CRDs (`VSphereMachine`,
`VSphereMachineTemplate`, future `ProxmoxMachine`, `LibvirtMachine`) satisfy
the InfraMachine contract.

The user-facing `VirtualMachine` is **not** a `clusterv1.Machine` — it's a
peer-level resource — but the infrastructure CRDs banlieue creates *behind
the scenes* are CAPI-shaped.

## What you get for free

### 1. Battle-tested status semantics

Every condition type, every `MachineAddress` field, every failure semantic was
arrived at through years of operational experience across cloud providers.
Banlieue doesn't have to invent any of it, and users can rely on familiar
status semantics if they already work with CAPI.

### 2. Providers can serve **both** banlieue and CAPI

A `VSphereMachine` written for banlieue *also* satisfies the CAPI v1beta2
InfraMachine contract. That means:

- A CAPI `Cluster` can directly consume the same provider with no changes.
- A team already running CAPI for cluster lifecycle can adopt banlieue for
  standalone VMs **using the same provider deployment**, the same
  credentials, and the same backend connectivity.
- Provider authors can target two upstream consumers with one codebase.

### 3. CAPI tooling works on banlieue's infra CRDs

`clusterctl`-style tooling that inspects infrastructure CRDs (e.g. to see why
a machine is unhealthy) works on banlieue's CRDs out of the box.

### 4. We inherit a versioning story

CAPI's contract is versioned (`v1beta1` → `v1beta2` → eventually `v1`). When
the contract moves, we move with it — but we don't have to invent a versioning
discipline from scratch.

## InfraCluster: cluster-side failure-domain spread

The InfraMachine contract covers a single machine. CAPI has a second contract,
**InfraCluster**, for the cluster-level object a CAPI `Cluster` points its
`spec.infrastructureRef` at. banlieue implements it as
`infrastructure.banlieue.io/v1alpha1` **`VSphereCluster`**.

Its job is to advertise the **failure domains** a cluster's machines may be
spread across, in the CAPI v1beta2 shape (`status.failureDomains` is a *list* of
`{ name, controlPlane, attributes }`). CAPI's control-plane and MachineSet
controllers then balance the requested `replicas` across those domains — so
"spread a control plane across all six (datacenter, cluster) pairs" is just
`replicas: 6`. banlieue ships **no** cluster or "tier" CRD of its own; cluster
lifecycle is CAPI's job (with a control-plane provider such as
[k0smotron](https://docs.k0smotron.io/) for k0s). See
[ADR-0001](https://github.com/firestoned/banlieue/blob/main/docs/adr/0001-capi-native-cluster-provisioning.md).

What makes banlieue's `VSphereCluster` distinct from CAPV's same-named object:
it **aggregates failure domains from one or more `Provider`s**, so a single
Kubernetes cluster can span multiple vCenters (e.g. 2 vCenters × 3 compute
clusters = 6 failure domains). The banlieue controller builds the list by
reading each selected `Provider.status.failureDomains[]` — it talks to **no
backend**, preserving the CRD-only contract. Capacity-awareness is the
provider's concern (it omits a full cluster from its status) and, within a
chosen cluster, vSphere DRS picks the host. See
[ADR-0002](https://github.com/firestoned/banlieue/blob/main/docs/adr/0002-infracluster-failure-domain-aggregation.md).

## Where the user touches this

Ideally: never. The `VirtualMachine` CR is the user's surface. The
infrastructure CR (`VSphereMachine`) is created and managed by the banlieue
controller; the user only sees it if they go looking.

`kubectl describe virtualmachine db-prod-01` shows uniform conditions. If a
user *wants* to see the underlying infra object's status (for example to
debug a vSphere-specific failure), the `VirtualMachine.status` points at it
via `infrastructureRef`.

## Where to read the contract

- [Cluster API v1beta2 contracts (cluster-api.sigs.k8s.io)](https://cluster-api.sigs.k8s.io/developer/providers/contracts/)
- [InfraMachine spec specifically](https://cluster-api.sigs.k8s.io/developer/providers/contracts/infra-machine)

Banlieue's interpretation of the contract is encoded in
[`crates/banlieue-api/src/infrastructure/`](https://github.com/firestoned/banlieue/tree/main/crates/banlieue-api/src/infrastructure)
(Phase 0 ships `VSphereMachine` + `VSphereMachineTemplate`; further providers
follow in Phases 1C and 1D).
