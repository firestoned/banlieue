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
> **Status (2026-11-19): not started.** Nothing in the tree yet. Depends on
> roadmap 22 phases 0 to D.
>
> **Build it shared-capable from the first commit** (ADR-0133): the
> InfraMachine's `providerRef` is a `ProviderReference`, and the provider
> serves tenant namespaces through `ProviderScope`.

Baseline when written: `dca5f45` (2026-11-19). This roadmap takes **ADR-0166
and ADR-0167** from the block roadmaps 22 to 25 reserve (0158 to 0169).

**Sequencing.** Roadmap 23 (Azure) comes first in the numbering, but EC2 is
the cheaper first proof of roadmap 22: an official Rust SDK may cover it
(roadmap 22 phase 0), `RunInstances` is idempotent by client token, and
the EBS direct APIs make image publishing a plain upload. Building EC2
first and Azure second is the recommended order; the numbers stay as they
are.

## How EC2 maps onto banlieue

| banlieue | EC2 |
| --- | --- |
| `Provider` | one account, one region (the role to assume names the account) |
| failure domain | an availability zone. Zone **names** are mapped per account; the provider publishes zone IDs (`use1-az1`) beside names so two accounts can be compared |
| `VMClass` | an instance type (`m7i.xlarge`, `m8g.xlarge`), mapped per `Provider` (ADR-0160). The type's architecture must match the `VMImage`'s (`Amd64`, `Arm64`): checked at admission and in scheduling |
| storage class | an EBS volume type and its IOPS and throughput (`gp3`, `io2`) |
| network class | a subnet ID and security group IDs, per zone |
| `VMImage` | an AMI published by roadmap 22 (EBS direct APIs, `RegisterImage`), or a hand-published AMI by ID (`Template`) |
| `spec.userData` | instance user data (16 KiB limit checked at admission) |
| `tpmEnabled` | NitroTPM: the AMI is registered with `BootMode=uefi` and `TpmSupport=v2.0`, on an instance type that supports it |
| `desiredPowerState: PoweredOff` | `StopInstances` (compute stops billing, volumes do not) |
| address | the primary private IP; no public IP or Elastic IP in v1 |

## Decisions (one ADR each)

### ADR-0166: the EC2 InfraMachine

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

- [ ] A Kairos raw image, uploaded through the EBS direct APIs and
      registered as a UEFI, `TpmSupport=v2.0` AMI, boots and applies user
      data (does Kairos's datasource read EC2's IMDSv2?), on `x86_64` and
      on Graviton.
- [ ] The `GuestReady` transport roadmap 22 picks (ADR-0163) works here
      (`GetConsoleOutput` with `Latest` is the first candidate), and its
      latency is recorded.
- [ ] `GetInstanceTpmEkPub` returns a key that matches what the guest's TPM
      reports.
- [ ] Time from `RunInstances` to `GuestReady`, plain and sealed.
- [ ] Post-quantum key exchange measured against STS, EC2 and EBS
      (roadmap 22 phase 0).

## Phases

| Phase | What | Status |
| --- | --- | --- |
| 0 | Spike above; ADR-0166 and ADR-0167 written with its results | ⛔ |
| A | Client: the SDK or first-party SigV4 client roadmap 22 phase 0 chose, behind a trait; a fake that **rejects** what EC2 rejects (an instance type not offered in the zone, a second create with the same client token but different parameters, `IdempotentParameterMismatch`), per `rules/testing.md` rule 1; a live protocol tier (`#[ignore]`, fails loudly without an account) | ⛔ |
| B | API: `EC2Machine` and `EC2MachineTemplate`, `ec2` provider class, admission for what the class refuses (`installMode: Deferred`, ISO images, user data over 16 KiB, an architecture mismatch, `tpmEnabled` on an AMI without TPM support), `regen-crds`, examples | ⛔ |
| C | Provider: create, observe, stop, start, terminate, adopt by tag, zone retry on `InsufficientInstanceCapacity`, quota and offered types per zone on `Provider.status` | ⛔ |
| D | Images: AMIs published by roadmap 22's publish Job, consumed by digest; `Template` for hand-published AMIs | ⛔ |
| E | Controller dispatch and scheduling; `VirtualMachine` status mirrors the infra CR (non-negotiable 6) | ⛔ |
| F | Pools: a `GuestReady` pool warms, is claimed, releases and refills across two zones; sealed pool on NitroTPM; sealing isolation test | ⛔ |
| G | Docs and threat model: guide (OIDC provider and role, subnet and security group mapping, first instance, first pool), provider comparison column, threat-model full pass for ADR-0166 and ADR-0167 | ⛔ |

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
- [ ] ADR-0166 and ADR-0167 accepted, each with its threat-model pass.
- [ ] Orphan sweep verified: kill the provider mid-create, restart it, and
      nothing is left behind or duplicated.
- [ ] `cargo deny` clean.
- [ ] Provider comparison and `ROADMAPS.md` updated.
