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
> **Status (2026-11-24): phase 0 spike done but for one measurement; ADRs
> Accepted; phase A started.** A live spike on a real subscription (2026-11-22 to 2026-11-24)
> published a banlieue-built Kairos image into a Compute Gallery and booted
> it as Trusted Launch VMs in two zones: create to `Succeeded` 177 to 330 s,
> an active boot 18.8 s once the image was fixed. ADR-0164 and ADR-0165 are
> written (Accepted) with its results, as are roadmap 22's ADR-0158 to
> ADR-0163. The vTPM has **no EK certificate**, only an Azure-issued AK
> certificate (ADR-0165). Still open in phase 0: create-to-ready for a
> sealed image. Phase A is nearly done: `crates/banlieue-azure` holds the ARM
> client, its federated token source, and a live tier whose read tests pass
> against a real subscription with the production identity path. Depends
> on roadmap 22 phases 0 to D.
>
> **Build it shared-capable from the first commit** (ADR-0133): the
> InfraMachine's `providerRef` is a `ProviderReference`, and the provider
> serves tenant namespaces through `ProviderScope`.

Baseline when written: `dca5f45` (2026-11-19). This roadmap takes **ADR-0164
and ADR-0165** from the block roadmaps 22 to 25 reserve (0158 to 0169).

## How Azure maps onto banlieue

| banlieue | Azure |
| --- | --- |
| `Provider` | one subscription, one region, one admin-named resource group that banlieue creates into (explicit, never discovered) |
| failure domain | an availability zone of the region (`1`, `2`, `3`). Logical zone numbers are **per subscription**: zone 1 in one subscription need not be zone 1 in another, so two `Provider`s on different subscriptions must not assume their zones line up |
| `VMClass` | a VM size (`Standard_D4s_v5`), mapped per `Provider` (ADR-0160) |
| storage class | a managed-disk SKU (`Premium_LRS`, `PremiumV2_LRS`, `StandardSSD_ZRS`) |
| network class | a subnet resource ID and an NSG, per zone |
| `VMImage` | an image version in an Azure Compute Gallery (roadmap 22, ADR-0162), or a hand-published one by resource ID (`Template`) |
| `spec.userData` | `osProfile.customData` (cloud-init reads it; 64 KiB limit checked at admission) |
| `tpmEnabled` | Trusted Launch: `securityType: TrustedLaunch`, Secure Boot and vTPM on; the gallery image definition must declare Trusted Launch support |
| `desiredPowerState: PoweredOff` | deallocate (stops compute billing), not power-off |
| address | the NIC's private IP; no public IP in v1 |

## Decisions (one ADR each)

### ADR-0164: the Azure InfraMachine and a first-party ARM client

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

### ADR-0165: Trusted Launch, the vTPM and its EK certificate

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

- [x] A Kairos image, written as a fixed VHD, published as a gallery image
      version, boots as a Trusted Launch VM and applies `customData`.
      Kairos's datasource does **not** read Azure's (its providers do not
      recognise the UDF `ovf-env.xml` disk); a baked step reads it locally
      (ADR-0163 Decision 3). The VHD was written by `qemu-img` for the spike;
      roadmap 22's first-party writer replaces it.
- [x] The `GuestReady` transport works and its latency is recorded: the
      guest reports Ready to WireServer and banlieue reads the provisioning
      state (ADR-0163); ready at 18.1 s of uptime on an active boot.
- [x] EK certificate readable in the guest; chain checked. **Answered
      differently:** Trusted Launch defines no EK certificate; NV
      `0x01C101D0` holds an Azure AK certificate whose chain verifies to
      `Azure Virtual TPM Root Certificate Authority 2023` (ADR-0165).
- [ ] Time from create to `GuestReady` for a plain and for a sealed image.
      Plain: 177 to 330 s (first boot includes recovery, layout and reset;
      an active boot is 18.8 s). **Sealed: not yet measured.**
- [x] Post-quantum key exchange measured against ARM and the Entra ID token
      endpoint: neither negotiates X25519MLKEM768 (2026-11-23, two networks);
      the guest's SPIRE link through the tailnet did.

## Phases

| Phase | What | Status |
| --- | --- | --- |
| 0 | Spike above; ADR-0164 and ADR-0165 written with its results | 🔶 Spike done but the sealed-image timing; ADR-0164 and ADR-0165 Accepted (2026-11-24) |
| A | ARM client crate: auth from the federated token, VM, NIC, disk, gallery, usages, resource SKUs, operation polling, `Retry-After`; unit tests against a fake that **rejects** what ARM rejects (a create in a zone the size is not offered in, a delete of a missing resource returning 204, not an error), per `rules/testing.md` rule 1; a live protocol tier (`#[ignore]`, fails loudly without a subscription) | 🔶 `crates/banlieue-azure` started (2026-11-24): the pure core, 52 unit tests, each from what ARM did in the spike: ownership tags with Azure's name rules, typed ARM errors (throttle, size not available, allocation, quota, auth), `Retry-After`, long-running operation handles that refuse a URL off the ARM endpoint, the VM and NIC bodies that booted, the federated token exchange. Then (70 tests) reading ARM's responses, tested against real captured ones: usable zones per size (offered minus the subscription's restrictions), quota headroom, provisioning and power state (`GuestReady`), the NIC's address, tagged orphans past a grace period. Then (86 tests) the `ArmApi` trait and `FakeArm`, which refuses what ARM refused in the spike (restricted zone, NVMe/SCSI mismatch leaving the NIC behind, disk below the image, `/` in a tag, quota, missing NIC, NIC in use, a failed VM, a delete burst) and allows deleting what is gone. Then (118 tests, 2026-11-24) `ArmClient`, the `reqwest` implementation of `ArmApi`, and `FederatedToken`, tested against a local server answering with the captured responses: pinned `api-version` per resource type, path segments checked before any request, every URL that carries the token checked to sit under the endpoint (operation URLs, `nextLink`, and a handle read back from a CR's status), no redirects, capped response size, 204 and a resource's 404 on delete as success but a missing resource group as an error, `Location` operations polled to a final status, a refused token dropped from the cache, `Content-Length: 0` on POST (reqwest sends none and ARM answers 411); the token file re-read on each exchange (the kubelet rotates it). **Live tier** (`tests/live_arm.rs`, `#[ignore]`): the federated exchange, resource listing with `createdTime`, a missing VM as not found, SKUs with usable zones and regional quota all pass against a real subscription through a kind cluster's ServiceAccount token, with the identity holding Reader on one resource group only; **the subscription-level SKU and usage reads needed no subscription-scoped role**. Open: the lifecycle live test (create, boot, delete with the NIC and disk; written, needs the provider's resource group role granted to the test identity) and the gallery image definition read that phase B's admission needs |
| B | API: the InfraMachine and its Template, `azure` provider class, admission for what the class refuses (`installMode: Deferred`, ISO images, `customData` over 64 KiB, `tpmEnabled` against an image definition without Trusted Launch), `regen-crds`, examples | ⛔ |
| C | Provider: create, observe, delete, adopt by tag, power state, zone retry on allocation failure, quota and offered sizes per zone on `Provider.status` | ⛔ |
| D | Images: gallery image versions published by roadmap 22's publish Job, consumed by digest; `Template` for hand-published versions | ⛔ |
| E | Controller dispatch and scheduling; `VirtualMachine` status mirrors the infra CR (non-negotiable 6) | ⛔ |
| F | Pools: a `GuestReady` pool warms, is claimed, releases and refills across two zones; sealed pool on Trusted Launch; sealing isolation test | ⛔ |
| G | Docs and threat model: guide (federation, resource group, gallery, first VM, first pool), provider comparison column, threat-model full pass for ADR-0164 and ADR-0165 | ⛔ |

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
- [ ] ADR-0164 and ADR-0165 accepted, each with its threat-model pass.
- [ ] Orphan sweep verified: kill the provider mid-create, restart it, and
      nothing is left behind or duplicated.
- [ ] `cargo deny` clean.
- [ ] Provider comparison and `ROADMAPS.md` updated.
