# 24: Amazon EC2 provider

> **Goal.** A `VirtualMachine` (and a `VirtualMachinePool` member) can run
> as an EC2 instance, through a new InfraMachine CRD and `Provider`s of
> class `ec2`, booting an AMI that roadmap 22 published from a `VMImage`.
> NitroTPM gives the guest a TPM 2.0, so sealed pools (ADR-0088) can run
> here too.
>
> **Stop condition.** A `VirtualMachine` with `spec.userData`, scheduled onto
> an `ec2` `Provider`, becomes an instance in the zone banlieue chose,
> booted from an AMI banlieue published, in a subnet and security groups
> the admin mapped, with IMDSv2 required; the guest consumes its
> user-data, reports its private address and `GuestReady`. Deleting it
> terminates the instance and removes its volumes, and nothing banlieue did
> not create. A `GuestReady`, `tpmEnabled` pool warms across two zones and
> refills, and its numbers sit in the provider comparison. The provider
> authenticates by `AssumeRoleWithWebIdentity` with no access key in the
> cluster.
>
> **Status (2026-11-27): phase 0 and A done, ADRs Accepted (partially implemented), phases B, C and
> E in progress.** The spike ran in
> one account (2026-11-24 to 2026-11-26); its results are in ADR-0174 (the
> `EC2Machine` and a first-party AWS client), ADR-0167 (NitroTPM and the EK
> public key), ADR-0175 (the EC2 guest layer) and ADR-0162's EC2 amendment
> (publishing through VM Import). Phase B (the API) is in the tree except the
> cross-object checks that need scheduling. Phase C (the provider's
> reconcilers, `banlieue provider ec2`) is written and unit tested, not yet run
> live. Phase E (controller dispatch, scheduling, the operator's workload) is
> written and unit tested, so a `VirtualMachine` now schedules onto an `ec2`
> `Provider` and becomes an `EC2Machine`; none of it has run against an
> account.
>
> **Build it shared-capable from the first commit** (ADR-0133): the
> InfraMachine's `providerRef` is a `ProviderReference`, and the provider
> serves tenant namespaces through `ProviderScope`.

Baseline when written: `dca5f45` (2026-11-19). This roadmap reserved
**ADR-0166 and ADR-0167** from the block roadmaps 22 to 25 reserve (0158 to
0169). ADR-0166 was used for another decision first, so the InfraMachine
decision is **ADR-0174**; ADR-0167 is as reserved, and the guest layer is
**ADR-0175**.

**Sequencing.** Azure (roadmap 23) was built first after all, and EC2
follows its shape: a first-party client crate (ADR-0174, as ADR-0164 for
ARM), a guest helper delivered as a bundle (ADR-0175, as ADR-0171), and a
publish Job (ADR-0162). Publishing goes through VM Import, not the EBS
direct APIs, because governed accounts commonly withhold the `ebs:`
actions (ADR-0162 Decision 9).

## How EC2 maps onto banlieue

| banlieue | EC2 |
| --- | --- |
| `Provider` | one account, one region (the role to assume names the account) |
| failure domain | an availability zone. Zone **names** are mapped per account; the provider publishes zone IDs (`use1-az1`) beside names so two accounts can be compared |
| `VMClass` | an instance type (`m7i.xlarge`, `m8g.xlarge`), mapped per `Provider` (ADR-0160). The type's architecture must match the `VMImage`'s (`Amd64`, `Arm64`): checked at admission and in scheduling |
| storage class | an EBS volume type and its IOPS and throughput (`gp3`, `io2`) |
| network class | a subnet ID and security group IDs, per zone |
| `VMImage` | an AMI published by roadmap 22 (a pre-laid disk as a stream-optimized VMDK, staged in S3, `ImportSnapshot`, `RegisterImage`; ADR-0162 Decision 9), or a hand-published AMI by ID (`Template`) |
| `spec.userData` | instance user data (16 KiB limit checked at admission) |
| `tpmEnabled` | NitroTPM: the AMI is registered with `BootMode=uefi` and `TpmSupport=v2.0`, on an instance type that supports it |
| `desiredPowerState: PoweredOff` | `StopInstances` (compute stops billing, volumes do not) |
| address | the primary private IP; no public IP or Elastic IP in v1 |

## Decisions (one ADR each)

### ADR-0174: the EC2 InfraMachine (reserved as ADR-0166)

Written 2026-11-26 as
[ADR-0174](../../docs/adr/0174-ec2-inframachine-and-a-first-party-aws-client.md),
Accepted (not implemented). What follows is the plan it answered.

- **Kind name.** CAPA owns `AWSMachine`; `EC2Machine` does not collide and
  says what it is. The CRD satisfies the CAPI v1beta2 InfraMachine
  contract (`providerID` as `aws:///<zone>/<instance-id>`, the form the
  AWS cloud controller manager expects, `addresses`, `failureDomain`,
  `initialization.provisioned`).
- **Idempotent create.** `RunInstances` takes a `ClientToken` derived from
  the owning object's UID, so a retried create returns the same instance;
  `TagSpecifications` tag the instance and its volumes in the same call, so
  nothing exists untagged even for an instant.
- **Hardening on every instance**, not configurable down: IMDSv2 required
  (`HttpTokens=required`), hop limit 1, no instance profile unless a later
  ADR adds one, EBS encryption on, `DeleteOnTermination` on every volume
  banlieue creates.
- **Observation.** State comes from `DescribeInstances` filtered by
  banlieue's tag; the provider requeues a booting instance on the same 2 s
  then back-off cadence ADR-0043 Decision 8 uses, and never blocks a worker.
- **Rate limits.** EC2 throttles per account and API. The client uses the
  SDK's adaptive retry (or the first-party client's equivalent), and
  sustained throttling is a `Provider` condition.

### ADR-0167: NitroTPM and the endorsement key

Written 2026-11-26 as
[ADR-0167](../../docs/adr/0167-nitrotpm-and-the-ek-public-key.md), Accepted (not implemented):
the EK public key from `GetInstanceTpmEkPub` is published as
`status.tpmEndorsementPublicKeys`; `ekTrustBundle` is refused on EC2.

NitroTPM gives `tpmEnabled` a real TPM 2.0, so first-boot sealing
(ADR-0088) can apply. The difference from every other backend: EC2 exposes
the EK as a **public key** (`GetInstanceTpmEkPub`), host-side, over an
authenticated AWS API, rather than as a certificate chained to a vendor
root. The ADR decides:

- whether an EK public key obtained from the cloud API, bound to the
  instance ID, satisfies what ADR-0045 publishes and ADR-0049 Decision 10
  verifies, or whether `ekTrustBundle` gains a second trust mode ("trust
  the backend's API for this key") that must be declared, never inferred;
- whether it is published on the InfraMachine status the way vSphere's
  host-side read is (ADR-0045);
- that a sealed volume attached to another instance does not unlock
  (roadmap 18's invariant 2, run on EC2).

## Phase 0 spike (gates the rest)

- [x] A Kairos image registered as a UEFI, `TpmSupport=v2.0` AMI boots and
      applies user data, on `x86_64`: through the EBS direct APIs first,
      then through VM Import (the path chosen, ADR-0162 Decision 9). Kairos's
      datasource does not read IMDSv2; a baked reader does (ADR-0175).
      **Graviton not run** (needs an arm64 guest build): open.
- [x] `GuestReady` transport: a level-3 `/dev/kmsg` line read with
      `GetConsoleOutput` `Latest`, refreshed about every 30 s (ADR-0174,
      ADR-0175).
- [x] `GetInstanceTpmEkPub` (RSA-2048 and ECC P-384) matches the guest's TPM
      byte for byte, stable across stop and start (ADR-0167).
- [x] Time from `RunInstances` to `GuestReady`: 144 to 156 s for the first
      unsealed images; sealed, 413 s from a raw image that reset out of
      recovery, **49.9 s** from a pre-laid 8 GiB template that grows on first
      boot, 40 s for a restart. First boots spread 29 to 64 s of guest
      uptime across three AMIs (likely EBS lazy loading; Fast Snapshot
      Restore untested).
- [x] Post-quantum key exchange: STS, EC2, EBS and S3 negotiate
      X25519MLKEM768 from rustls with aws-lc-rs (ADR-0170).
- [ ] Not yet run: `InsufficientInstanceCapacity` and zone retry; two
      instances from one AMI (distinct EKs, sealing isolation).

## Phases

| Phase | What | Status |
| --- | --- | --- |
| 0 | Spike above; ADR-0174, ADR-0167, ADR-0175 and ADR-0162's amendment written with its results (Accepted, not implemented) | 🔶 |
| A | Client: the first-party `banlieue-aws` crate (ADR-0174: SigV4, STS web identity, EC2 Query, S3 multipart, VM Import), behind a trait; a fake that **rejects** what EC2 rejects (an instance type not offered in the zone, a second create with the same client token but different parameters, `IdempotentParameterMismatch`), per `rules/testing.md` rule 1; a live protocol tier (`#[ignore]`, fails loudly without an account) | ✅ 2026-11-27: `banlieue-aws` (errors, SigV4 against AWS's vectors, Query, tags, retry, hardened `RunInstances`, XML types, console, `Ec2Api`, a refusing `FakeEc2`, STS web identity at 900 s, `Ec2Client`); live tier green against a real account, including a ServiceAccount token exchanged at STS and an instance run, observed, EK read, IMDS off, terminated. S3 multipart and VM Import (the publisher's calls) come with phase D |
| B | API: `EC2Machine` and `EC2MachineTemplate`, `ec2` provider class, admission for what the class refuses (`installMode: Deferred`, ISO images, user data over 16 KiB, an architecture mismatch, `tpmEnabled` on an AMI without TPM support), `regen-crds`, examples. **Landed (2026-11-27):** `EC2Machine` (`ec2m`) and `EC2MachineTemplate` (`ec2mt`) with `providerID` `aws:///<zone>/<instance-id>` and its parser, `status.zoneId`, `imdsDisabled` and `tpmEndorsementPublicKeys` (ADR-0167 Decision 1); `Provider.spec.connection.ec2` (region, role ARN, optional STS VPC endpoint, KMS key, extra tags, publisher role, staging bucket and VM Import role); identifier shapes in `crates/banlieue-api/src/aws.rs`, enforced both as CRD patterns and in Rust; user data over 16,384 bytes refused by the `EC2Machine` schema and `validate()`; admission: ec2 in `provider-connection.yaml` (endpoint and STS pinned to the region's AWS names), the new `provider-ec2-governance.yaml` (tags), `vmimage-cloud-install-mode.yaml` (Deferred; no ISO source kind exists), `machine-provider-ref.yaml`, `provider-jobs.yaml` (`banlieue-ec2-publisher`) and `ekTrustBundle` refused on ec2 (ADR-0167 Decision 2), each evaluated offline as CEL; CRDs and API reference regenerated; `examples/33-virtualmachine-ec2.yaml`. **Closed in phase E (2026-11-27):** `trustedBoot` refused on an image with an `ec2` source at admission (`vmimage-cloud-install-mode.yaml`) and again in scheduling (`TrustedBootUnsupported`); the class's instance type against the image's architecture (`ArchitectureMismatch`, from the provider's `amd64VmClasses`/`arm64VmClasses`); `tpmEnabled` on a type without NitroTPM (`NitroTpmUnsupported`); `tpmEndorsementPublicKeys` mirrored to the `VirtualMachine` and a bound claim. Also closed in phase E: a hand-published `Template` AMI's own architecture and `TpmSupport=v2.0`/`BootMode=uefi`, read with `DescribeImages` and recorded on the image's row (`ArchitectureMismatch` on the AMI's architecture, `AmiTpmUnsupported`). **Open:** a published AMI's attributes, which the publisher's per-Provider record (phase D) will write | 🔶 |
| C | Provider: create, observe, stop, start, terminate, adopt by tag, zone retry on `InsufficientInstanceCapacity`, quota and offered types per zone on `Provider.status`. **Landed (2026-11-27), unit tested against `FakeEc2`, not run live:** `crates/banlieue-provider-ec2` and `banlieue provider ec2`. `EC2Machine`: finalizer, adoption by the ownership tag, `RunInstances` with the UID-and-zone client token (never awaited; a fresh ID not yet visible is not gone), state, addresses, zone ID and `providerID`, `GuestReady` from this instance's console line, IMDS off after it, both EK public keys (not found right at `running` retried), stop and start, terminate on delete (any instance tagged with the UID, already gone is success), `ZoneUnavailable` for no capacity or a type not offered (ADR-0173) with the exponential create back-off, throttling requeued. `Provider`: one failure domain per zone offering a mapped type (name and zone ID, `vmClasses`, `tpmVmClasses`), storage and network classes, the orphan sweep (home-namespace-only for a home-only provider) and its metrics. Credentials per ADR-0174 Decision 6 (`WebIdentitySource`, 900 s sessions, STS VPC endpoint honoured), extra tags merged under banlieue's own. `deploy/provider-ec2/rbac/` and `deploy/network-policies/provider-ec2.yaml`. `FakeEc2` now refuses an EK read on a `pending` instance and can answer not found right after `running`. **Open:** vCPU quota as `capacity` (Service Quotas is not in `banlieue-aws` yet), the leftover-volume check after terminate (`DescribeVolumes`/`DeleteVolume` not in the client; `DeleteOnTermination` covers the root volume), a `Provider` condition for sustained throttling, and the live lifecycle test. The operator's `ec2` workload landed with phase E. Phase E also publishes `amd64VmClasses` and `arm64VmClasses` per failure domain | 🔶 |
| D | Images: the publish Job for EC2 (ADR-0162 Decision 9: stream-optimized VMDK writer in `banlieue-diskformat`, S3 multipart staging, `ImportSnapshot`, `RegisterImage`, adoption by tag), AMIs consumed by digest; `Template` for hand-published AMIs; the guest layer and `banlieue-guest-ec2` helper (ADR-0175). **`Template` landed early, with phase E (2026-11-27):** the provider's `VMImage` reconciler reads a `Template` AMI with `DescribeImages` (new in `banlieue-aws`: `Ec2Api::describe_image`, `FakeEc2::add_image`, a live test) and writes a regional row per `ec2` `Provider`: ready with `status.perProvider[].ec2` (architecture, root device name, boot mode, TPM support, ENA) and `minOsDiskSizeGiB` from the root snapshot, or not ready as `InvalidRef`, `AmiNotFound`, `AmiNotAvailable`, `AmiNotEbsBacked`, `AmiArchitectureUnsupported`, `AmiRootDeviceInvalid` or `AmiDescribeFailed`; throttling and EC2 or network failures keep the row as it was (`AmiUnverified` when there is none). `Url` and other kinds are `UnsupportedSourceKind`; its ClusterRole gained `vmimages` read and `vmimages/status` write. The controller already boots a `Url` source from the ready row's `resolvedRef` for the current build's digest, so the publish Job only has to write that row | 🔶 |
| E | Controller dispatch and scheduling; `VirtualMachine` status mirrors the infra CR (non-negotiable 6) | 🔶 2026-11-27, unit tested, not run live: `InfraKind::Ec2` and `build_ec2_machine` (zone from the failure domain; instance type from `instanceSizes`; AMI from the `ec2` `Template` source, or a `Url` source's published row; root volume size from the class, `volumeType`/`iops`/`throughput` from the storage class target; `subnetId` and comma-separated `securityGroupIds` from the network class target; `kmsKeyId` from the connection; `tpmEnabled` from the class; user data as rendered, refused over 16,384 bytes; deferred install, `trustedBoot`, `efi-secure`, data disks, a second NIC and per-VM CPU, memory or address overrides refused); the controller watches, applies, mirrors and finalizes `EC2Machine`s (`deploy/controller/rbac/clusterrole.yaml`); the status mirror carries provisioned, addresses, conditions (`GuestReady` included), power state and `tpmEndorsementPublicKeys` (new on `VirtualMachine.status` and `VirtualMachineClaim.status`), and `ZoneUnavailable` reaches ADR-0173's re-placement through the same accessor. Scheduling: `ec2` is a cloud class (`InstanceSizeUnmapped`), and step 3d refuses, each as a named reason when it is the only cause, `TrustedBootUnsupported`, `InstanceTypeNotOffered` (`vmClasses`), `ArchitectureMismatch` (`amd64VmClasses`/`arm64VmClasses`, now published by the provider, against the AMI's recorded architecture, else the `VMImage`'s), `AmiTpmUnsupported` (a `tpmEnabled` class on an AMI recorded without `TpmSupport=v2.0` and `BootMode=uefi`) and `NitroTpmUnsupported` (`tpmVmClasses`, replacing the `vtpm` feature on ec2). The `EC2Machine`'s `rootDeviceName` is the AMI's recorded one (`/dev/xvda` when none is recorded). Operator: `ec2` renders a `Managed` workload with a projected ServiceAccount token (audience `sts.amazonaws.com`, 600 s) at `/var/run/secrets/aws/tokens/aws-identity-token` and `AWS_WEB_IDENTITY_TOKEN_FILE`, no Secret; `ec2` joins `COMPILED_BACKENDS` and `SHARED_PROVIDER_BACKENDS`. **Open:** the live run, including `describe_image_reads_a_real_ami` against a real AMI | 🔶 |
| F | Pools: a `GuestReady` pool warms, is claimed, releases and refills across two zones; sealed pool on NitroTPM; sealing isolation test | ⛔ |
| G | Docs and threat model: guide (OIDC provider and role, subnet and security group mapping, first instance, first pool), provider comparison column, threat-model full pass for ADR-0174, ADR-0167 and ADR-0175 | ⛔ |

## Out of scope

Creating VPCs, subnets, security groups or load balancers; public and
Elastic IPs; Spot instances and Auto Scaling groups; instance profiles;
Dedicated Hosts; Nitro Enclaves; `.metal` instance types as hosts for
banlieue's own host-resident providers (a different idea: a later roadmap
could run Cloud Hypervisor or Firecracker on a bare-metal instance).

## Read first

Roadmap 22 and its ADRs, then ADR-0043, 0045, 0049, 0088, 0101 and 0133.

## Definition of done

- [ ] Stop condition above, verified live against a real account (no
      account ID, ARN or resource ID committed).
- [ ] ADR-0174, ADR-0167 and ADR-0175 accepted, each with its threat-model
      pass.
- [ ] Orphan sweep verified: kill the provider mid-create, restart it, and
      nothing is left behind or duplicated.
- [ ] `cargo deny` clean.
- [ ] Provider comparison and `ROADMAPS.md` updated.
