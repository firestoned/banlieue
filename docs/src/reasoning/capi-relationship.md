# Relationship to Cluster API (CAPI / CAPM)

banlieue and [Cluster API](https://cluster-api.sigs.k8s.io/) are different
products, but they meet at a contract: banlieue's provider infrastructure CRDs
satisfy the **CAPI v1beta2 infrastructure contracts** — both the `InfraMachine`
contract (`VSphereMachine`) and the `InfraCluster` contract (`VSphereCluster`).
That makes banlieue usable as a CAPI **infrastructure provider**: the same
provider binary serves banlieue's own `VirtualMachine` *and* CAPI-managed
clusters.

If you only want to know *what* the contract is, read
[Concepts → Infrastructure CRDs & CAPI](../concepts/infra-crds-capi.md).
This page is about *why* — the boundary, and where the two products meet.

## Two surfaces, one provider

banlieue has **two** consumption paths that share the same provider controllers
and backend credentials:

| | **Standalone `VirtualMachine`** | **CAPI cluster (infra provider)** |
| --- | --- | --- |
| What you create | `VirtualMachine` | CAPI `Cluster` + control-plane + `MachineDeployment` |
| What you get | one VM, declarative | a Kubernetes cluster |
| Who owns lifecycle | banlieue's controller | CAPI core + a control-plane provider (e.g. [k0smotron](https://docs.k0smotron.io/) for k0s) |
| Tooling | `kubectl` / GitOps | `clusterctl` / CAPI controllers |
| banlieue's role | the whole thing | the **infrastructure** layer (InfraMachine + InfraCluster) |

The first surface is banlieue's identity: a `VirtualMachine` is **not** a
`clusterv1.Machine`; it is a peer-level resource that happens to *own* a
CAPI-shaped infrastructure CR underneath. The second surface is what the CAPI
contract buys us — banlieue does not have to *become* CAPI to be *used by* it.

## What banlieue implements from CAPI

Two contracts, nothing more.

**InfraMachine** (`VSphereMachine`) — the per-machine contract. Every banlieue
provider's machine CRD exposes a status an upstream CAPI consumer can read
without modification:

| Field | Meaning |
| --- | --- |
| `status.initialization.provisioned` | infrastructure has been provisioned (v1beta2; replaces the deprecated `status.ready`) |
| `status.addresses[]` | `MachineAddress` entries (IP, hostname, type) |
| `status.conditions[]` | `metav1.Condition`-shaped, with `Ready` always present |
| `spec.providerID` | backend-stable identifier surfaced after provisioning |
| `spec.failureDomain` | which failure domain CAPI placed this machine in |
| Owner-reference & finaliser conventions | how cleanup is gated |

banlieue follows the v1beta2 convention of expressing terminal failures as
**conditions** — it does *not* use the deprecated `status.failureReason` /
`status.failureMessage` fields.

**InfraCluster** (`VSphereCluster`) — the per-cluster contract. It advertises
where a cluster's machines may be placed:

| Field | Meaning |
| --- | --- |
| `status.initialization.provisioned` | the InfraCluster is ready |
| `status.controlPlaneEndpoint` | the API-server endpoint (operator-supplied VIP, or set by the control-plane provider) |
| `status.failureDomains[]` | the v1beta2 list (`{name, controlPlane, attributes}`) CAPI spreads machines across |

The reconciliation accessor that fronts the machine fields lives at
[`crates/banlieue-controller/src/reconciler/status_mirror.rs`](https://github.com/firestoned/banlieue/blob/main/crates/banlieue-controller/src/reconciler/status_mirror.rs)
(`InfraMachineRead` trait); the InfraCluster aggregation lives at
[`crates/banlieue-controller/src/reconciler/vsphere_cluster.rs`](https://github.com/firestoned/banlieue/blob/main/crates/banlieue-controller/src/reconciler/vsphere_cluster.rs).

## How a cluster gets built on banlieue

banlieue ships **no** cluster, replica, or upgrade controller of its own — that
is precisely the work CAPI already does well. To provision a Kubernetes cluster
you bring CAPI core plus a control-plane/bootstrap provider (k0smotron for k0s)
and point a CAPI `Cluster` at banlieue's `VSphereCluster`:

1. The `VSphereCluster` aggregates the failure domains of one or more
   `Provider`s (vCenters) into `status.failureDomains` — so a single cluster can
   span multiple vCenters.
2. CAPI's control-plane / `MachineDeployment` controllers spread the requested
   `replicas` across those failure domains and mint a `VSphereMachine` per
   placement from a `VSphereMachineTemplate`.
3. banlieue's vSphere provider realises each `VSphereMachine` on the backend;
   the control-plane provider joins the nodes into the cluster.

So **"spread a control plane across all six (datacenter, cluster) pairs" is just
`replicas: 6`** over a `VSphereCluster` that advertises six failure domains.
There is no banlieue-native "tier" or "cluster" CRD — see
[ADR-0001](https://github.com/firestoned/banlieue/blob/main/docs/adr/0001-capi-native-cluster-provisioning.md)
and
[ADR-0002](https://github.com/firestoned/banlieue/blob/main/docs/adr/0002-infracluster-failure-domain-aggregation.md).

## What banlieue does **not** do

These are deliberate. banlieue *participates* in CAPI as an infrastructure
provider; it does not *reimplement* CAPI.

- **No cluster lifecycle controller.** banlieue never reconciles a `Cluster`,
  `Machine`, `MachineSet`, or `MachineDeployment`. CAPI core owns those; banlieue
  only owns the infrastructure CRs they reference.
- **No bootstrap or control-plane provider.** Preparing and joining nodes
  (kubeadm, k0smotron, RKE2, …) is the control-plane provider's job. banlieue's
  image + cloud-init story is unconstrained — it will not run a bootstrapper for
  you.
- **No native tier / cluster abstraction.** Replica counts and failure-domain
  spread are expressed with CAPI's own objects, not a banlieue CRD.
- **`clusterctl` is not required for the `VirtualMachine` surface.** Standalone
  VMs are plain `kubectl` / GitOps; you only reach for CAPI tooling when you are
  building clusters.

## Why the `VirtualMachine` surface is not "just CAPI"

A reasonable question: if banlieue wires its providers to CAPI contracts anyway,
why keep a separate `VirtualMachine` type at all? Three reasons:

1. **The user surface would change.** Creating `Cluster` + `Machine` resources
   to provision a single VM is a vendor joke, not a virtualization API.
   banlieue's whole point is `kind: VirtualMachine`.
2. **The lifecycle assumptions don't match.** CAPI is opinionated about how
   machines come and go (immutability, rolling replace, surge counts). VM
   lifecycles are not always that — sometimes you want to *swap a backend* and
   keep the VM, which has no analogue in CAPI's machine model.
3. **The dependency would be mandatory.** Forcing CAPI core + webhooks + a
   control-plane provider just to run one VM is a heavy install. Implementing the
   *contract* gives interop without making CAPI a hard dependency of the VM path.

The cost of compatibility — implementing two external contracts — is small. The
benefit is that the same provider serves both a single VM and an entire CAPI
cluster.

## Why pin to v1beta2

CAPI's contract is versioned (`v1beta1` → `v1beta2` → eventually `v1`). banlieue
tracks v1beta2 because:

- It is the current contract upstream as of 2026.
- `status.initialization.provisioned` clears up a long-running v1beta1 ambiguity
  about whether `status.ready` means *first ready* or *currently ready*.
- `status.failureDomains` is a **list** in v1beta2 (`{name, controlPlane,
  attributes}`), which is the shape `VSphereCluster` produces.

CAPI discovers which CRD versions conform via the CRD-level label
`cluster.x-k8s.io/v1beta2: v1alpha1`, emitted onto every
`infrastructure.banlieue.io` CRD by `crdgen` (see
[ADR-0005](https://github.com/firestoned/banlieue/blob/main/docs/adr/0005-capi-contract-label-codegen.md)).
When CAPI moves to `v1`, banlieue moves with it — but only after the
status-mirror accessor is extended to translate any new fields.

## What "the same provider serves both" actually means

Because banlieue's infrastructure CRDs are CAPI-shaped, a provider controller
that targets banlieue (e.g. `banlieue-provider-vsphere`) can be driven by CAPI
`Cluster` / `Machine` objects **with no source changes** — and a team already
running CAPV (`cluster-api-provider-vsphere`) can deploy banlieue alongside it
and reuse the same vSphere credentials and connectivity.

This is encoded in the architecture model as the
`capi-v1beta2-infra-machine-contract` and `capi-v1beta2-infra-cluster-contract`
controls (see
[Architecture → Controls](../architecture/index.md#controls-modelled-in-calm)).

## Further reading

- [Concepts → Infrastructure CRDs & CAPI](../concepts/infra-crds-capi.md) — the *what*
- [Architecture → System Diagram](../architecture/system.md) — where the infra CRs sit in the model
- [CAPI v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/infra-machine)
- [CAPI v1beta2 InfraCluster contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/infra-cluster)
- [CAPI provider matrix](https://cluster-api.sigs.k8s.io/reference/providers)
