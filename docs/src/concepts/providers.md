# Provider Model

A **provider** is the unit of pluggability in banlieue. A provider is whatever
turns a uniform `VirtualMachine` request into a real VM on a specific backend
(vSphere, Proxmox, libvirt, …).

This page explains what a provider *is* on the wire, what it implements, and
how it plugs in. For the reasoning behind the design, see
[Abstraction principle](../reasoning/abstraction-principle.md) and
[CRD-only contract](../reasoning/crd-only-contract.md).

## Two things make a provider

A provider is **two artifacts**:

1. **A `Provider` CR** — declares the existence of a backend in this cluster
   and carries its connection settings (endpoints, credential references,
   defaults). One per backend instance.
2. **A provider controller** — a Kubernetes controller that watches an
   infrastructure CRD (e.g. `VSphereMachine`) and drives the corresponding
   backend.

The user only ever sees (1). The controller (2) is plumbing.

```mermaid
flowchart LR
    user[User] -->|providerRef.name| vm[(VirtualMachine)]
    vm --> mc[banlieue-controller]
    mc -->|creates| infra[(VSphereMachine)]
    infra --> pv[banlieue-provider-vsphere]
    pv -->|talks to| backend[vSphere API]
```

## The `Provider` CR

```yaml
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata:
  name: prod-vsphere
spec:
  type: vsphere
  vsphere:
    endpoint: https://vcenter.example.com
    credentialsRef:
      name: vsphere-creds
    datacenter: DC0
    defaultDatastore: ssd-tier-0
```

Notes:

- The user never sees the `vsphere:` block. It's owned by whoever administers
  the cluster's `Provider`s.
- `credentialsRef` points at a `Secret`. Credentials are *not* embedded in the
  CR.
- A cluster can have many `Provider`s, including multiple of the same type
  (`prod-vsphere`, `dr-vsphere`, `lab-vsphere`).

The authoritative type is in
[`crates/banlieue-api/src/banlieue/provider.rs`](https://github.com/firestoned/banlieue/blob/main/crates/banlieue-api/src/banlieue/provider.rs).

## What a provider controller does

A provider controller is a Kubernetes controller. Its responsibilities:

1. **Watch its infrastructure CRD** (`VSphereMachine`,
   `VSphereMachineTemplate`, etc.).
2. **Reconcile to the backend.** Translate the uniform spec into native API
   calls (govmomi for vSphere, proxmoxer for Proxmox, libvirt for libvirt).
3. **Report status uniformly.** Patch `.status` on the infra CR with the CAPI
   v1beta2 condition vocabulary, regardless of how the backend natively
   surfaces errors. See [Infrastructure CRDs & CAPI](infra-crds-capi.md).
4. **Add and clear finalisers** so deletes block until the backend is actually
   torn down.
5. **Use server-side apply** with a provider-specific field manager
   (`banlieue.io/provider-vsphere`, etc.) — so ownership of fields is explicit.

## What a provider controller does **not** do

- It does **not** speak directly to the banlieue main controller. The bus is
  the K8s API. See [CRD-only contract](../reasoning/crd-only-contract.md).
- It does **not** publish a service. There is nothing to expose, nothing to
  authenticate against, nothing to load-balance.
- It does **not** mutate `VirtualMachine`. It only mutates its own infra CR.
  Status mirroring is the main controller's job.
- It does **not** see `Provider` credentials except by resolving the Secret it
  was pointed at.

## Anatomy of a provider crate

The expected layout (one crate per provider, all sharing
`banlieue-provider-sdk`):

```
crates/banlieue-provider-vsphere/
├── Cargo.toml
└── src/
    ├── main.rs              # binary entrypoint
    ├── lib.rs               # library root
    ├── context.rs           # reconciler Context
    ├── error.rs             # typed errors
    └── reconciler/
        ├── mod.rs
        ├── vsphere_machine.rs
        └── vsphere_machine_tests.rs
```

The SDK
([`banlieue-provider-sdk`](https://github.com/firestoned/banlieue/tree/main/crates/banlieue-provider-sdk))
gives you:

- `client::build_client` — Kubernetes client with sensible timeouts.
- `status::set_condition` / `find_condition` / `is_condition_true` —
  CAPI-shaped condition handling.
- `finalizer::ensure_finalizer` / `remove_finalizer` — patch-based finalisers.
- `ssa::server_side_apply` — server-side apply with a per-provider field
  manager.
- `reconciler::{requeue_default, requeue_on_error, requeue_long, no_requeue}`
  — small helpers around `kube::runtime::Action`.

## Writing a third-party provider

banlieue's provider model is open. To ship a new provider:

1. Define your infrastructure CRD(s) in a separate crate, satisfying the
   [CAPI v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/).
   The provider CRD shape used in `crates/banlieue-api/src/infrastructure/`
   (`VSphereMachine`, `VSphereMachineTemplate`) is the reference.
2. Build a controller against your CRD using `banlieue-provider-sdk`.
3. Ship a container image and a Helm chart / Kustomize manifest.

That's it. No registration with the banlieue project. No code changes to
`banlieue-api`. No coordination with the main controller team. The whole point
of the [CRD-only contract](../reasoning/crd-only-contract.md) is that you
*don't have to be us* to add a provider.
