# Relationship to Cluster API (CAPI / CAPM)

banlieue and [Cluster API](https://cluster-api.sigs.k8s.io/) are different
products solving different problems, but they share a contract: banlieue's
provider infrastructure CRDs satisfy the **CAPI v1beta2 `InfraMachine`
contract**. That single decision is what lets the same provider binary serve
both projects, and it deserves an explanation of its own.

If you only want to know *what* the contract is, read
[Concepts → Infrastructure CRDs & CAPI](../concepts/infra-crds-capi.md).
This page is about *why* — the reasoning, the boundary, and the limits.

## Different products

| | **banlieue** | **Cluster API (CAPI)** |
| --- | --- | --- |
| What you create | `VirtualMachine` | `Cluster` + `MachineDeployment` + `Machine` |
| What you get | one VM, declarative | a Kubernetes cluster |
| Bootstrap | none — the VM is the deliverable | kubeadm, RKE2, k3s, … |
| Scope | a single VM lifecycle | the lifecycle of an entire K8s cluster |
| Tooling assumed | `kubectl` | `clusterctl`, `Cluster`/`Machine` controllers |
| Conformance | none yet (project is young) | CNCF, large vendor matrix |

banlieue is intentionally **not** a CAPI distribution. A `VirtualMachine`
is not a `clusterv1.Machine`; it is a peer-level resource that happens to
*own* a CAPI-shaped infrastructure CR underneath.

## What banlieue takes from CAPI

Only one thing — the v1beta2
[`InfraMachine` contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/infra-machine).
Concretely, every banlieue provider's infrastructure CRD exposes a status
shape that an upstream CAPI consumer can read without modification:

| Field | Source | Meaning |
| --- | --- | --- |
| `status.ready` | CAPI | infrastructure provisioning is complete |
| `status.initialization.provisioned` | CAPI v1beta2 | infrastructure has been provisioned at least once |
| `status.addresses[]` | CAPI | `MachineAddress` entries (IP, hostname, type) |
| `status.failureReason` / `status.failureMessage` | CAPI (deprecated in v1beta2) | terminal-failure surface |
| `status.conditions[]` | CAPI | `metav1.Condition`-shaped, with `Ready` always present |
| `spec.providerID` | CAPI | backend-stable identifier surfaced after provisioning |
| Owner-reference & finaliser conventions | CAPI | how cleanup is gated |

The reconciliation accessor that fronts these fields lives at
[`crates/banlieue-controller/src/reconciler/status_mirror.rs`](https://github.com/firestoned/banlieue/blob/main/crates/banlieue-controller/src/reconciler/status_mirror.rs)
(`InfraMachineRead` trait) so that providers added in later phases can
expose CAPI-shaped status without touching the main reconciler.

## What banlieue does **not** take from CAPI

These omissions are deliberate. Each one would either expand scope or
constrain users in ways the project explicitly rejects.

- **No `Cluster`.** banlieue never creates a Kubernetes cluster. A
  `VirtualMachine` is the deliverable, full stop.
- **No `Machine` / `MachineSet` / `MachineDeployment`.** These types
  exist to describe machines *as members of a cluster*. banlieue does
  not model cluster membership at all.
- **No bootstrap providers.** CAPI's `Bootstrap` contract (kubeadm,
  RKE2, …) is about preparing a node to join a cluster. banlieue's image
  + cloud-init story is unconstrained — bring whatever image you want,
  banlieue will not run kubeadm for you.
- **No control-plane providers.** Same reasoning: there is no control
  plane to manage because there is no cluster.
- **No `clusterctl`.** banlieue uses plain `kubectl` / GitOps. Provider
  installation is whatever the provider's Helm chart or manifest says.

The CAPI components banlieue avoids are excellent at what they do —
they are simply not what banlieue is for.

## Why "compatible" instead of "built on"

A reasonable question: if banlieue is going to wire its providers to a
CAPI contract anyway, why not adopt CAPI wholesale? Three reasons:

1. **The user surface would change.** Users would create `Cluster` and
   `Machine` resources to provision a single VM. That is a 1990s vendor
   joke, not a virtualization API. banlieue's whole point is `kind:
   VirtualMachine`.
2. **The lifecycle assumptions don't match.** CAPI is opinionated about
   how machines come and go (immutability, rolling replace via
   `MachineDeployment`, surge counts). VM lifecycles are not always
   that — sometimes you want to *swap a backend* and keep the VM, which
   has no analogue in CAPI's machine model.
3. **The dependency would be enormous.** CAPI brings in its own CRDs,
   webhooks, RBAC, controller, conformance suite. banlieue would inherit
   all of that to deliver one VM. The contract-only adoption gives us
   the interop without the install footprint.

The cost of compatibility — adopting one external contract for one
internal type — is small. The cost of full adoption would be the
project's identity.

## What "the same provider serves both" actually means

Because banlieue's infrastructure CRDs are CAPI-shaped, a provider
controller that already targets banlieue (e.g. the planned
`banlieue-provider-vsphere`) can be pointed at CAPI `Cluster` objects
**with no source changes**. The same goes in the other direction: a team
already running CAPV (`cluster-api-provider-vsphere`) for cluster
lifecycle can deploy banlieue alongside it and reuse the same vSphere
credentials and connectivity.

This is encoded in the architecture model as the
`capi-v1beta2-infra-machine-contract` control (see
[Architecture → Controls](../architecture/index.md#controls-modelled-in-calm)).
A CI check on the CRD shape — comparing the generated CRDs against the
contract's required fields — is a Phase 2 deliverable.

## Why pin to v1beta2

CAPI's contract is versioned (`v1beta1` → `v1beta2` → eventually `v1`).
banlieue tracks v1beta2 because:

- It is the current contract upstream as of 2026.
- The `initialization.provisioned` field clarifies a long-running
  ambiguity in v1beta1 about whether `status.ready` means *first time
  ready* or *currently ready*. v1beta2 splits the two.
- `MachineAddress.type` is a discriminated string in v1beta2; v1beta1
  consumers can still read it.

When CAPI moves to `v1`, banlieue moves with it — but only after the
controller's status-mirror accessor (`InfraMachineRead`) has been
extended to translate any new fields. The decoupling is intentional.

## Further reading

- [Concepts → Infrastructure CRDs & CAPI](../concepts/infra-crds-capi.md) — the *what*
- [Architecture → System Diagram](../architecture/system.md) — where the infra CR sits in the model
- [CAPI v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/infra-machine)
- [CAPI provider matrix](https://cluster-api.sigs.k8s.io/reference/providers)
