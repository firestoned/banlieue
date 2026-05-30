<!-- Copyright (c) 2026 Erick Bourgeois, banlieue -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# banlieue

> A Kubernetes-native, **provider-agnostic** virtualization API.
> One CRD. Many backends. Swap them without touching the user's manifest.

[![Build](https://github.com/firestoned/banlieue/actions/workflows/build.yaml/badge.svg?branch=main)](https://github.com/firestoned/banlieue/actions/workflows/build.yaml)
[![Documentation](https://github.com/firestoned/banlieue/actions/workflows/docs.yaml/badge.svg?branch=main)](https://github.com/firestoned/banlieue/actions/workflows/docs.yaml)
[![CodeQL](https://github.com/firestoned/banlieue/actions/workflows/codeql.yaml/badge.svg?branch=main)](https://github.com/firestoned/banlieue/actions/workflows/codeql.yaml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/firestoned/banlieue/badge)](https://scorecard.dev/viewer/?uri=github.com/firestoned/banlieue)

[![License](https://img.shields.io/github/license/firestoned/banlieue?color=blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![Docs site](https://img.shields.io/badge/docs-firestoned.github.io%2Fbanlieue-informational?logo=materialformkdocs)](https://firestoned.github.io/banlieue/)
[![Status](https://img.shields.io/badge/status-In%20Development-orange)](https://firestoned.github.io/banlieue/reference/roadmap/)
[![Issues](https://img.shields.io/github/issues/firestoned/banlieue)](https://github.com/firestoned/banlieue/issues)
[![Last commit](https://img.shields.io/github/last-commit/firestoned/banlieue/main)](https://github.com/firestoned/banlieue/commits/main)
[![PRs welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/firestoned/banlieue/pulls)

---

**banlieue** lets you declare a virtual machine the same way you declare a
`Deployment` or `Service` — one Kubernetes Custom Resource — and have it
scheduled onto whatever hypervisor or VM platform you run: **vSphere** today,
**Proxmox** and **libvirt** next, or any backend a third party writes. The
user's manifest never changes when the backend does.

```yaml
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: db-prod-01
spec:
  classRef:                 # references a VMClass (the hardware "shape")
    name: db-prod-large
  imageRef:                 # references a VMImage (the OS)
    name: ubuntu-22.04-cloudinit
  placement:                # optional — where it may land
    providerSelector:
      matchLabels: { dc: dc1, env: prod }
  desiredPowerState: PoweredOn
```

That single CR is scheduled onto a `Provider`, which resolves it to a concrete,
backend-specific infrastructure CR (e.g. a `VSphereMachine`) that a provider
controller turns into a real VM — all over the Kubernetes API, with no new
transport or auth to operate.

## Why banlieue?

The VM control plane is fragmented: every team running more than one hypervisor
writes the same glue twice. banlieue makes the VM a first-class, declarative,
backend-agnostic Kubernetes object.

- **One declarative API** for VMs, regardless of backend.
- **Swap or mix providers** without rewriting workloads — vSphere here, Proxmox
  there, libvirt for dev — in the same cluster.
- **Zero new transports.** The contract between the main controller and the
  providers is the Kubernetes API itself: no gRPC, no REST, no custom auth.
- **A battle-tested status model.** Provider infrastructure CRDs satisfy the
  [Cluster API v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/),
  so they double as reusable CAPI infrastructure providers.

### What banlieue is **not**

- Not a hypervisor.
- Not a "VMs-as-containers" shim (that's [KubeVirt](https://kubevirt.io/)).
- Not a CAPI replacement — a `VirtualMachine` is **not** a `clusterv1.Machine`.
  banlieue coexists with Cluster API but does not depend on it.
- Not a closed system — providers are a documented contract; anyone can write one.

## Architecture

The main controller **never speaks directly to a provider.** Both sides watch
the Kubernetes API server — that is the bus. The controller schedules a
`VirtualMachine` and creates a provider-specific infrastructure CR; the provider
controller reconciles that CR against its backend and reports status back; the
main controller mirrors that status onto the `VirtualMachine`.

> **📐 Architecture diagram + reconcile flow:**
> [`docs/src/concepts/architecture.md`](docs/src/concepts/architecture.md)
> (rendered: [Architecture](https://firestoned.github.io/banlieue/concepts/architecture/)).
> It's the single source of truth for the component diagram — GitHub renders the
> Mermaid inline when you open the file.

| Resource | Group | What it is |
| --- | --- | --- |
| `VirtualMachine` | `banlieue.io/v1alpha1` | The user-facing request for a running VM. |
| `VMClass` | `banlieue.io/v1alpha1` | A reusable hardware "shape" + capability requirements. |
| `VMImage` | `banlieue.io/v1alpha1` | A backend-agnostic, multi-source OS image catalog entry. |
| `Provider` | `banlieue.io/v1alpha1` | One registered backend (a vCenter, a Proxmox cluster, …). |
| `VSphereMachine` / `VSphereCluster` | `infrastructure.banlieue.io/v1alpha1` | Concrete, CAPI-contract infra CRs the vSphere provider reconciles. |

Full schema reference: **[API Reference](https://firestoned.github.io/banlieue/reference/api/)** ·
Design rationale: **[Concepts](https://firestoned.github.io/banlieue/concepts/)** &
**[Why banlieue?](https://firestoned.github.io/banlieue/reasoning/)**

## Repository layout

```
banlieue/
├── crates/
│   ├── banlieue/                # the single binary: dispatches subcommands
│   ├── banlieue-api/            # authoritative CRD type system (code-first)
│   ├── banlieue-controller/     # main controller lib: schedule + status mirror
│   ├── banlieue-provider-sdk/   # shared provider building blocks
│   └── banlieue-provider-vsphere/  # the vSphere provider lib
├── deploy/                      # CRDs (generated) + kustomize manifests
├── docs/                        # MkDocs site + CALM architecture model
├── examples/                    # sample CRs
└── docs/adr/                    # Architecture Decision Records
```

`crates/banlieue-api` is the **source of truth**: CRD YAML in `deploy/crds/` and
the API reference are generated from the Rust types (`make crds`). Never
hand-edit generated YAML.

## Development

banlieue follows **ADD — Architecture Driven Development**: significant changes
start with an [ADR](docs/adr/) and a [CALM](https://github.com/finos/architecture-as-code)
architecture update, *then* test-driven implementation (`ADR → CALM → TDD`).
See [`.claude/rules/architecture-driven-development.md`](.claude/rules/architecture-driven-development.md).

```sh
# Generate CRDs + API reference from the Rust types
make crds

# Quality gate (run after any Rust change)
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all

# Bring up a local kind cluster with the CRDs applied
make kind-up

# Build the documentation site (also validates the CALM model)
make docs
```

## Project status

banlieue is **early**. The `banlieue-api` type system + CRDs are in place and the
controller, provider SDK, and vSphere provider are landing. The CRD surface is
`v1alpha1` and **will break before `v1`** — don't run production workloads
against it yet.

## License

Apache License 2.0 — see [LICENSE](LICENSE).
