<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0164: The Azure InfraMachine and a first-party ARM client

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 23 (Azure provider). Follows
  [ADR-0011](0011-libvirt-provider-own-client.md) (banlieue writes its own
  backend clients), [ADR-0050](0050-libvirtmachine-domain-lifecycle.md) and
  [ADR-0075](0075-proxmoxmachine-inframachine-contract.md) (InfraMachine
  lifecycles and the CAPI v1beta2 contract), [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md)
  (shared-capable from the first commit). Uses
  [ADR-0158](0158-shape-of-a-cloud-provider-class.md) to
  [ADR-0163](0163-guestready-on-a-cloud.md).

## Context

Roadmap 23 adds Azure virtual machines as a backend. Two choices have to be
made before code: what the InfraMachine kind is called, and how banlieue talks
to Azure Resource Manager (ARM).

**The name.** Cluster API's Azure provider (CAPZ) already owns
`AzureMachine` in `infrastructure.cluster.x-k8s.io`. A second `AzureMachine`
in `infrastructure.banlieue.io` is legal but makes `kubectl get azuremachine`
ambiguous on a cluster that runs both, which is exactly a cluster that uses
CAPZ for Kubernetes nodes and banlieue for everything else.

**The client.** The phase 0 spike (2026-11-22 to 2026-11-24) drove ARM by raw
REST (`az rest --method put`) to see the exact request and every failure.
What a client must handle, from that:

1. A VM create is a `PUT` of one JSON document, and a usable one needs:
   `storageProfile.diskControllerType: NVMe` (for NVMe-only sizes),
   `osDisk.diskSizeGB` (ADR-0160), `deleteOption: Delete` on the OS disk and
   the NIC, `securityProfile` for Trusted Launch,
   `diagnosticsProfile.bootDiagnostics.enabled: true` (ADR-0163),
   `osProfile.customData` and `linuxConfiguration.provisionVMAgent: false`.
   The NIC is created first, by its own `PUT`.
2. Creates are long-running (the `PUT` returns `Creating`); deletes of a VM
   took about 12 s with NIC and disk; a gallery image version took 7 to 22
   minutes.
3. **Deletes are throttled early** (`SubscriptionRequestsThrottled`, retry
   after one second, on the fourth consecutive delete).
4. **A deploy-time rejection leaves the NIC** created before it (ADR-0158).
5. Failure reasons arrive in two places: the deployment or `PUT` error
   (`InvalidParameter`, `Conflict`, `NotAvailableForSubscription`) and, later,
   the instance view (`ProvisioningState/failed/OSProvisioningTimedOut`).
6. No maintained Rust crate covers the Compute and Network planes:
   `azure_mgmt_compute` is the retired generated SDK, and the new official
   `azure_resourcemanager_compute` and `_network` crates are at 0.0.1.
7. ARM negotiates no post-quantum key exchange today (ADR-0159).

## Decision

### 1. Kind `AzureVirtualMachine` (and `AzureVirtualMachineTemplate`)

In `infrastructure.banlieue.io/v1alpha1`, short name `azvm`. It cannot be
confused with CAPZ's `AzureMachine`, and it reads as what it creates. It
satisfies the CAPI v1beta2 InfraMachine contract: `spec.providerID`
(`azure:///subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.Compute/virtualMachines/<name>`,
the form CAPZ and the Azure cloud provider use), `status.addresses` (the NIC's
private IP; no public IP in v1), `spec.failureDomain` (the zone),
`status.initialization.provisioned`. Its `providerRef` is a
`ProviderReference` and the provider serves tenant namespaces through
`ProviderScope` (ADR-0133).

### 2. A first-party crate, `banlieue-azure`

A small typed ARM client, as `banlieue-proxmox` and `banlieue-libvirt` are:

- one `reqwest` client, rustls, **X25519MLKEM768 offered first**;
- a token source from the federated credential (ADR-0159), refreshed before
  expiry;
- typed requests only for what banlieue touches: virtual machines (and their
  instance view), network interfaces, disks, snapshots, gallery image
  definitions and versions, compute usages, resource SKUs;
- **`api-version` pinned per resource type**, changed deliberately;
- long-running operations returned as a handle (`Azure-AsyncOperation` or
  `Location`) that the caller stores and polls on its next reconcile; the
  client never blocks a worker on one;
- **`Retry-After` honoured on 429 for every verb, deletes included**
  (finding 3); sustained throttling reported to the caller so the provider
  can set a condition on the `Provider`;
- a `DELETE` returning 204 or 404 is success;
- errors typed by ARM's `error.code`, so the provider can tell
  "not available in this zone" from "bad request" from "throttled".

### 3. The provider's lifecycle

Create: NIC (`PUT`, idempotent by UID-derived name), then VM (`PUT`), both
tagged (ADR-0158); record the operation handle; observe. Observe: instance
view for power and provisioning state, NIC for the address; `GuestReady` from
provisioning state (ADR-0163). Delete: VM with `deleteOption`s, then confirm
by tag that NIC and disks are gone. Power: `desiredPowerState: PoweredOff`
is **deallocate** (stops compute billing), not power-off. Adopt: before any
create, look up by tag and name.

### 4. A fake that rejects what ARM rejects

Unit tests run against an in-process fake ARM that **rejects** what the spike
saw ARM reject (rules from `rules/testing.md`): a size restricted in a zone;
an NVMe-only size against a SCSI-only image definition; a version from a
snapshot without `diskControllerTypes`; tag names with `/`; a fourth delete
in a burst (429 with `Retry-After`); a NIC left behind by a rejected VM
create. A live tier (`#[ignore]`) runs against a real subscription from the
environment and fails loudly without one.

## Consequences

- One more first-party client to maintain, small and typed to the resources
  banlieue uses; ARM's REST surface for these types is stable and versioned.
- Moving to the official crates later is a client-internal change, once they
  cover Compute and Network.
- The kind name differs from CAPZ's on purpose; documentation calls it out.

## Alternatives considered

- **`AzureMachine` in banlieue's group.** Rejected: ambiguous `kubectl get`
  beside CAPZ.
- **The 0.0.1 official crates.** Too early to depend on (finding 6).
- **Shelling out to `az`.** A Python runtime in the provider image, and no
  typed errors.
