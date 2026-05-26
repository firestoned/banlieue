# Roadmap

banlieue is built in phases. The maintainer keeps the detailed phase plans in
a private directory outside this repo (see
[`CLAUDE.md`](https://github.com/firestoned/banlieue/blob/main/CLAUDE.md));
this page is a public-facing summary.

## Phase 0 — `banlieue-api` ✅ (done)

The type system. Two API groups:

- `banlieue.io/v1alpha1` — `Provider`, `VMClass`, `VMImage`, `VirtualMachine`.
- `infrastructure.banlieue.io/v1alpha1` — `VSphereMachine`,
  `VSphereMachineTemplate`.

The `crdgen` binary produces the CRD YAMLs in `deploy/crds/`.

## Phase 1A — Main controller + Provider SDK 🚧 (in progress)

- [`banlieue-controller`](https://github.com/firestoned/banlieue/tree/main/crates/banlieue-controller) —
  watches `VirtualMachine`, resolves refs, creates infra CRs, mirrors status.
- [`banlieue-provider-sdk`](https://github.com/firestoned/banlieue/tree/main/crates/banlieue-provider-sdk) —
  shared library: client, status, finalizers, server-side apply, reconciler
  helpers.
- Docker / Chainguard images for the controller.
- `make kind-up` end-to-end local dev story.

## Phase 1B — vSphere provider

- `banlieue-provider-vsphere` — the reference provider.
- Talks to vCenter via `govmomi`-equivalent in Rust (or a Rust binding).
- Satisfies the
  [CAPI v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/).

## Phase 1C — Proxmox provider

- `banlieue-provider-proxmox` — talks to a Proxmox VE cluster.

## Phase 1D — libvirt provider

- `banlieue-provider-libvirt` — talks to one or more libvirt/KVM hosts.

## Phase 1E — Documentation site

The doc site you are reading. Includes:

- This `Why banlieue?` section.
- Concepts: architecture, providers, infrastructure CRDs.
- A real Quick Start once Phase 1A lands.
- An auto-generated CRD API reference.

## Phase 2+ — Beyond v1alpha1

The CRD surface is `v1alpha1` and **will break** before `v1`. Subjects we
expect to revisit before stabilising:

- Disk / storage modelling (uniform shape across vSphere / Proxmox / libvirt).
- Network attachment modelling.
- Image source modelling (HTTP URL vs registry vs datastore template).
- Multi-cluster / federation story.
- Live migration / snapshots / backup (only if a uniform contract is honest;
  see [Non-goals](../reasoning/non-goals.md#8-banlieue-does-not-implement-live-migration-snapshots-or-backup)).

## Non-goals (recap)

The [Non-goals page](../reasoning/non-goals.md) is the authoritative list of
what banlieue refuses to be. The roadmap above respects all of those.
