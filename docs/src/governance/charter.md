<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Project Charter (draft)

!!! note "Draft for FINOS contribution"
    This is banlieue's draft charter for its contribution to FINOS. It states
    what the project is for and how it is bounded. On contribution it is
    reviewed with FINOS staff and the Technical Oversight Committee, and may
    be replaced by FINOS's standard project charter.

## Name

**banlieue**: a Kubernetes-native, provider-neutral virtualization API.

## Mission

Give platform teams one declarative Kubernetes API for virtual machines on
whatever hypervisor they already run, so that "a VM" means the same thing on
vSphere, Proxmox VE, libvirt and Cloud Hypervisor, and the same machines can
back Cluster API clusters.

## Scope

**In scope:**

- The `banlieue.io` API: `VirtualMachine`, `VMClass`, `VMImage`, `Provider`,
  `ProviderClass`, `VirtualMachinePool`, `VirtualMachineClaim`.
- Provider implementations under `infrastructure.banlieue.io` that satisfy
  the Cluster API v1beta2 InfraMachine contract.
- The controller, operator, providers, image builder and host tooling that
  reconcile them, and the SDK for writing new providers.
- Security properties the API promises: least-privilege identities,
  verifiable releases, and the sandbox and attestation chain for pooled VMs.

**Out of scope:**

- Being a hypervisor, a cloud, or a replacement for Cluster API.
- RPC between the controller and providers. They communicate through
  Kubernetes resources only.
- Packaging formats beyond plain manifests and the `banlieue` CLI. There is
  no Helm chart.

## Principles

These are fixed; the [ADRs](https://github.com/firestoned/banlieue/tree/main/docs/adr/)
record why.

1. **CRDs only between the controller and providers.**
2. **Provider CRDs satisfy the CAPI v1beta2 InfraMachine contract.**
3. **`VirtualMachine` is independent of CAPI.** It can coexist with CAPI, but
   does not depend on it.
4. **Explicit over implicit.** Capabilities, image sources and credentials
   are declared; discovery is a status concern.
5. **Idempotent reconciliation.** Status is patched, never replaced.
6. **Status mirrors infrastructure.** A machine is provisioned only when its
   backend says so.

## How the project works

- **Governance:** [Governance](index.md) and [Maintainers](maintainers.md).
- **Contributions:** under the Apache License 2.0, with a DCO sign-off on
  every commit ([Contributing](../developer/contributing.md)).
- **Decisions:** Architecture Decision Records, written before the code
  (Architecture Driven Development).
- **Conduct:** the [Code of Conduct](code-of-conduct.md).
- **Security:** private vulnerability reporting, a published
  [threat model](../security/threat-model.md), and signed releases with SBOMs
  and SLSA provenance.

## Intellectual property

- Code is licensed under the Apache License 2.0. Documentation is under the
  same licence unless a file says otherwise.
- Contributors keep their copyright and certify their right to contribute
  through the DCO.
- The `banlieue` name and logo are held by the maintainer until contribution,
  then transferred to FINOS under its trademark policy.
