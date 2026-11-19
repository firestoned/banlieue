# 23: Azure provider

> **Goal.** A `VirtualMachine` (and a `VirtualMachinePool` member) can run
> as an Azure virtual machine, through a new InfraMachine CRD and
> `Provider`s of class `azure`, booting a `VMImage` that roadmap 22 published
> into an Azure Compute Gallery. Trusted Launch gives the guest a vTPM, so
> sealed pools (ADR-0088) can run here too.
>
> **Stop condition.** A `VirtualMachine` with `spec.userData`, scheduled onto
> an `azure` `Provider`, becomes an Azure VM in the zone banlieue chose,
> booted from a gallery image version banlieue published, on a subnet the
> admin mapped; the guest consumes its user-data, reports its private
> address and `GuestReady`. Deleting it removes the VM, its NIC and its
> disks, and nothing banlieue did not create. A `GuestReady`,
> `tpmEnabled` pool warms on Trusted Launch VMs across two zones and
> refills, and its numbers sit in the provider comparison. The provider
> authenticates by workload identity federation with no secret in the
> cluster.
>
> **Status (2026-11-19): not started.** Nothing in the tree yet. Depends on
> roadmap 22 phases 0 to D.
>
> **Build it shared-capable from the first commit** (ADR-0133): the
> InfraMachine's `providerRef` is a `ProviderReference`, and the provider
> serves tenant namespaces through `ProviderScope`.

Baseline when written: `dca5f45` (2026-11-19). This roadmap takes **ADR-0151
and ADR-0152** from the block roadmaps 22 to 25 reserve (0145 to 0156).

## How Azure maps onto banlieue

| banlieue | Azure |
| --- | --- |
| `Provider` | one subscription, one region, one admin-named resource group that banlieue creates into (explicit, never discovered) |
| failure domain | an availability zone of the region (`1`, `2`, `3`). Logical zone numbers are **per subscription**: zone 1 in one subscription need not be zone 1 in another, so two `Provider`s on different subscriptions must not assume their zones line up |
| `VMClass` | a VM size (`Standard_D4s_v5`), mapped per `Provider` (ADR-0147) |
| storage class | a managed-disk SKU (`Premium_LRS`, `PremiumV2_LRS`, `StandardSSD_ZRS`) |
| network class | a subnet resource ID and an NSG, per zone |
| `VMImage` | an image version in an Azure Compute Gallery (roadmap 22, ADR-0149), or a hand-published one by resource ID (`Template`) |
| `spec.userData` | `osProfile.customData` (cloud-init reads it; 64 KiB limit checked at admission) |
| `tpmEnabled` | Trusted Launch: `securityType: TrustedLaunch`, Secure Boot and vTPM on; the gallery image definition must declare Trusted Launch support |
| `desiredPowerState: PoweredOff` | deallocate (stops compute billing), not power-off |
| address | the NIC's private IP; no public IP in v1 |

## Decisions (one ADR each)

### ADR-0151: the Azure InfraMachine and a first-party ARM client

- **Kind name.** CAPZ already owns `AzureMachine` (in
  `infrastructure.cluster.x-k8s.io`). A second `AzureMachine` in
  `infrastructure.banlieue.io` is legal but makes `kubectl get azuremachine`
  ambiguous on a cluster that runs both. The ADR picks a name that cannot
  collide; the CRD satisfies the CAPI v1beta2 InfraMachine contract either
  way (`providerID` as `azure:///subscriptions/.../virtualMachines/<name>`,
  `addresses`, `failureDomain`, `initialization.provisioned`).
- **Client.** No maintained official Rust crate covers the Compute and
  Network management planes (confirmed in roadmap 22 phase 0). banlieue
  writes a small typed ARM client crate, as it did for Proxmox and libvirt:
  token from the federated credential, typed requests for the handful of
  resources it touches (VM, NIC, disk, gallery image version, usages,
  resource SKUs), `api-version` pinned per resource type.
- **Long-running operations.** A PUT or DELETE returns an operation URL. The
  provider records it on the CR's status, returns, and observes it on the
  next reconcile, as the vSphere provider does with tasks; it never blocks a
  worker on an operation.
- **Idempotency and adoption.** Resource names are derived from the owning
  object's UID, so a retried PUT is the same resource, and every resource
  carries roadmap 22's ownership tag. The NIC and disks are created with
  `deleteOption: Delete`, so deleting the VM takes them with it; the
  provider still checks and removes a leftover by tag.
- **Throttling.** ARM limits reads and writes per subscription and returns
  `Retry-After`. The client honours it; the provider surfaces sustained
  throttling as a condition on the `Provider`.

### ADR-0152: Trusted Launch, the vTPM and its EK certificate

Trusted Launch gives every VM a vTPM, so `tpmEnabled`, first-boot sealing
(ADR-0088) and the sealing isolation property (roadmap 18 phase B) can hold
here. What the ADR must settle with a live spike:

- whether the guest can read an **EK certificate** from the vTPM's NV
  index and whether it chains to a published Microsoft root, so ADR-0045's
  read path and ADR-0049's `ekTrustBundle` apply unchanged;
- which `VMImage` flags Trusted Launch accepts: `trustedBoot` (UKI,
  ADR-0051) under Azure's Secure Boot keys, `firstBootSealing`,
  `installMode: Deferred` (expected: not supported, there is no install
  ISO path in a cloud);
- that a sealed disk copied to another VM does not unlock (roadmap 18's
  invariant 2, run on Azure).

Confidential VMs (SEV-SNP, TDX) are **not** in this ADR: they change the
attestation story enough to need their own.

## Phase 0 spike (gates the rest)

- [ ] A Kairos image, written as a fixed VHD by roadmap 22's writer,
      published as a gallery image version, boots as a Trusted Launch VM
      and applies `customData` (does Kairos's datasource read Azure's?).
- [ ] The `GuestReady` transport roadmap 22 picks (ADR-0150) works here, and
      its latency is recorded.
- [ ] EK certificate readable in the guest; chain checked.
- [ ] Time from create to `GuestReady` for a plain and for a sealed image,
      so the pool numbers have a baseline.
- [ ] Post-quantum key exchange measured against ARM and the Entra ID token
      endpoint (roadmap 22 phase 0).

## Phases

| Phase | What | Status |
| --- | --- | --- |
| 0 | Spike above; ADR-0151 and ADR-0152 written with its results | ⛔ |
| A | ARM client crate: auth from the federated token, VM, NIC, disk, gallery, usages, resource SKUs, operation polling, `Retry-After`; unit tests against a fake that **rejects** what ARM rejects (a create in a zone the size is not offered in, a delete of a missing resource returning 204, not an error), per `rules/testing.md` rule 1; a live protocol tier (`#[ignore]`, fails loudly without a subscription) | ⛔ |
| B | API: the InfraMachine and its Template, `azure` provider class, admission for what the class refuses (`installMode: Deferred`, ISO images, `customData` over 64 KiB, `tpmEnabled` against an image definition without Trusted Launch), `regen-crds`, examples | ⛔ |
| C | Provider: create, observe, delete, adopt by tag, power state, zone retry on allocation failure, quota and offered sizes per zone on `Provider.status` | ⛔ |
| D | Images: gallery image versions published by roadmap 22's publish Job, consumed by digest; `Template` for hand-published versions | ⛔ |
| E | Controller dispatch and scheduling; `VirtualMachine` status mirrors the infra CR (non-negotiable 6) | ⛔ |
| F | Pools: a `GuestReady` pool warms, is claimed, releases and refills across two zones; sealed pool on Trusted Launch; sealing isolation test | ⛔ |
| G | Docs and threat model: guide (federation, resource group, gallery, first VM, first pool), provider comparison column, threat-model full pass for ADR-0151 and ADR-0152 | ⛔ |

## Out of scope

Creating VNets, subnets, NSGs, resource groups or load balancers; public
IPs; Spot VMs and scale sets; Confidential VMs; availability sets (zones
only); Azure Dedicated Host; moving a VM between zones or regions.

## Read first

Roadmap 22 and its ADRs, then ADR-0011 (why banlieue writes its own
clients), ADR-0043, 0045, 0049, 0088, 0101 and 0133.

## Definition of done

- [ ] Stop condition above, verified live against a real subscription (no
      subscription, tenant or resource ID committed).
- [ ] ADR-0151 and ADR-0152 accepted, each with its threat-model pass.
- [ ] Orphan sweep verified: kill the provider mid-create, restart it, and
      nothing is left behind or duplicated.
- [ ] `cargo deny` clean.
- [ ] Provider comparison and `ROADMAPS.md` updated.
