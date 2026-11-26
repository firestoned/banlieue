<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Firecracker Provider

!!! warning "In progress: nothing runs a guest yet"
    The `firecracker` class is designed and its API is in the tree, but there
    is **no provider, no host installer verb and no controller dispatch**
    yet (roadmap 20). A `FirecrackerMachine` you apply today is accepted by
    the API server and acted on by nothing. Use
    [Cloud Hypervisor](cloud-hypervisor.md) until this page says otherwise.

[Firecracker](https://firecracker-microvm.github.io/) is the backend banlieue
is building for **ephemeral, homogeneous** workloads, such as AI agent
sandboxes in a `VirtualMachinePool`, where density and time to ready matter
more than device coverage. Cloud Hypervisor stays the backend for anything
that needs a general-purpose guest, UEFI, a vTPM or devices.

## What is decided

The design is recorded in ADRs, each Accepted:

| Decision | ADR |
| --- | --- |
| A host-resident `firecracker` provider class and the `FirecrackerMachine` CRD, with its absences declared: no vTPM, no hotplug, no device list, no kernel arguments | [ADR-0137](https://github.com/firestoned/banlieue/blob/main/docs/adr/0137-firecracker-provider-class.md) |
| A host may run both VMMs, each in a sibling slice with its own share of the budget | [ADR-0138](https://github.com/firestoned/banlieue/blob/main/docs/adr/0138-a-host-may-run-both-vmms-with-split-budgets.md) |
| systemd supervises each guest (not Firecracker's jailer), from a config file, with no API socket | [ADR-0140](https://github.com/firestoned/banlieue/blob/main/docs/adr/0140-firecracker-guests-are-supervised-by-systemd-not-the-jailer.md) |
| One host-provider layer shared with Cloud Hypervisor | [ADR-0141](https://github.com/firestoned/banlieue/blob/main/docs/adr/0141-shared-host-provider-layer.md) |
| A guest is a kernel and a read-only root filesystem delivered as one digest-pinned OCI artifact (`OciArtifact` image source) | [ADR-0142](https://github.com/firestoned/banlieue/blob/main/docs/adr/0142-microvm-image-and-artifact-contract.md) |
| A per-host attestation CA (`Provider.spec.attestation.hostCa`) and a per-guest signer over vsock, for `x509pop` | [ADR-0143](https://github.com/firestoned/banlieue/blob/main/docs/adr/0143-firecracker-host-attestation-ca-and-guest-signer.md) |

## What is in the tree

- The `FirecrackerMachine` and `FirecrackerMachineTemplate` CRDs
  (`deploy/crds/`); the shape is in
  [`examples/30-firecrackermachine.yaml`](https://github.com/firestoned/banlieue/blob/main/examples/30-firecrackermachine.yaml)
  and the [API Reference](../reference/api.md).
- `Provider.spec.attestation.hostCa`, the `OciArtifact` image kind, and the
  `attestationSubject` mirror from the machine to its `VirtualMachine` and
  claim.
- The microVM artifact format (`banlieue-oci`) and an attestation signer
  library.

## What is not

The provider itself, a host installer verb, controller
dispatch from a `VirtualMachine`, and the measured time to `GuestReady` that
gates the rest. Progress is tracked in roadmap 20 and on the status board,
[`ROADMAPS.md`](https://github.com/firestoned/banlieue/blob/main/ROADMAPS.md).
