# Quick Start

!!! warning "Phase 1A — not production-ready"
    banlieue is in active development. The CRD surface is `v1alpha1` and will
    break before `v1`. This quickstart will be expanded as Phase 1A (main
    controller + provider SDK + first vSphere provider) lands. See the
    [roadmap](../reference/roadmap.md).

## Prerequisites

- A Kubernetes cluster (kind / minikube / a real one).
- `kubectl` configured against it.
- Rust toolchain (only for now, while we build from source).

## 1. Generate and apply the CRDs

From a clone of [firestoned/banlieue](https://github.com/firestoned/banlieue):

```bash
# Code-first CRDs — generated from the Rust types in crates/banlieue-api
cargo run -p banlieue-api --bin crdgen --features crdgen -- --out-dir deploy/crds

kubectl apply -f deploy/crds/
```

(Or use the [Makefile](https://github.com/firestoned/banlieue/blob/main/Makefile)
shortcuts: `make crds`, `make kind-up`.)

## 2. Inspect the API

```bash
kubectl explain virtualmachine.spec
kubectl explain provider.spec
kubectl explain vmclass.spec
kubectl explain vmimage.spec
```

## 3. Read an example

The repository's [`examples/`](https://github.com/firestoned/banlieue/tree/main/examples)
directory has YAML examples that exercise the current CRD surface:

- `03-vmclass-db-prod-large.yaml` — sample `VMClass`.
- `05-virtualmachine.yaml` — sample `VirtualMachine`.

These are syntactically valid against the generated CRDs but will only
provision once the matching provider controller is deployed (Phase 1B+).

## 4. Coming next

- Phase 1A: deploy the **banlieue-controller** to a kind cluster and watch
  it reconcile `VirtualMachine`s against a placeholder infra CRD.
- Phase 1B: deploy the **vSphere provider** against a real vCenter (or a
  vcsim mock).
- Phase 1C/1D: Proxmox and libvirt providers.

In the meantime, the most useful reading is [Why banlieue?](../reasoning/index.md)
and the [Architecture](../concepts/architecture.md) page.
