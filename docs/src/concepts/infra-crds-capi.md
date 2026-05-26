# Infrastructure CRDs & CAPI

banlieue's provider infrastructure CRDs satisfy the
[Cluster API (CAPI) v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/).

This page explains *what that contract is*, *why banlieue uses it*, and *what
you get for free* by piggybacking on it.

## What is the CAPI InfraMachine contract?

CAPI is the upstream Kubernetes project for declarative cluster lifecycle.
Every CAPI cluster uses an **infrastructure provider** (CAPV for vSphere,
CAPP for Proxmox, CAPL for libvirt, etc.) that owns the actual machines. To
keep providers interoperable, CAPI defines a versioned **contract** — a set
of expectations about what an infrastructure CRD's spec and status must look
like.

The v1beta2 contract specifies, among other things:

- A `spec` shape that can be templated (machine + machine template).
- A `status.ready` boolean.
- A `status.conditions[]` array with `metav1.Condition` shape.
- A `status.addresses[]` list of `MachineAddress` entries (IP, hostname).
- `status.failureReason` / `status.failureMessage` for terminal errors.
- `spec.providerID` discoverable after provisioning.
- Owner-reference and finaliser conventions.

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
