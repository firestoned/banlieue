# 22: Public-cloud providers, the shared foundation and cloud images

> **Goal.** banlieue can place a `VirtualMachine` (and a `VirtualMachinePool`
> member) in a public cloud as well as on a host it runs. Three providers
> build on this roadmap: Azure virtual machines (roadmap 23), Amazon EC2
> instances (roadmap 24) and AWS Lambda MicroVMs (roadmap 25). This roadmap
> holds what they share, decided once: how a cloud `Provider` is shaped, how
> it authenticates without a long-lived key, how a `VMClass` becomes an
> instance size, what capacity means when the hardware is not ours, how
> banlieue tags and finds what it created, and, above all, **how a
> `VMImage` is published into each cloud** so a `VirtualMachine` can boot
> from it.
>
> **Stop condition.** One `VMImage` built by `banlieue-imagebuilder` is
> published, without a hand step, as an AMI in an AWS account, as an image
> version in an Azure Compute Gallery, and (for an image of that kind) as a
> Lambda MicroVM image; each publication appears on the `VMImage`'s
> per-Provider status with the cloud's own identifier and the source digest
> it came from; deleting the `VMImage` removes every published copy that
> banlieue created and nothing else. Both cloud providers authenticate by
> workload identity federation from their own ServiceAccount, with no cloud
> key stored in the cluster.
>
> **Status (2026-11-19): not started.** Nothing in the tree yet.

Baseline when written: `dca5f45` (2026-11-19). ADRs 0001 to 0144 exist or are
reserved. **Roadmaps 22 to 25 together reserve ADR-0158 to ADR-0169**; this
one takes ADR-0158 to ADR-0163. Renumber at landing if anything else claims
them first. *Renumbered 2026-11-19:* the block was ADR-0145 to ADR-0156, until
ADR-0145 (the controller progress audit) and ADR-0157 (credential references)
landed first; every number in roadmaps 22 to 25 moved up by 13.

## Why public clouds, and why in banlieue

banlieue's value is one `VirtualMachine` API and one pool and claim model
over unlike backends. Until now every backend has been something the
operator runs. A cloud backend adds burst capacity, regions the operator
has no hardware in, and, for sandboxes, a backend nobody has to patch.

What banlieue is **not** doing here: replacing Cluster API's own cloud
providers. A Cluster API cluster on AWS or Azure should use CAPA or CAPZ,
which manage the VPC or VNet, load balancers and control plane around the
machines. banlieue's cloud providers create machines inside networks an
admin already has, exactly as its vSphere provider creates VMs on port
groups an admin already has (non-negotiable 4: explicit over implicit).
They still satisfy the InfraMachine contract (non-negotiable 2), so a
`MachineDeployment` can use them, but they ship no InfraCluster that builds
cloud networking.

## Decisions this roadmap makes (one ADR each)

### ADR-0158: shape of a cloud provider class

- **Deployment `Managed`**, in the cluster, like vSphere and Proxmox
  (ADR-0003, ADR-0012): one provider workload per `Provider`, its own
  ServiceAccount and Role. Not host-resident: there is no host.
- **One `Provider` is one account (or subscription) in one region.** Images,
  quotas and networks are regional in both clouds, so a region is the
  natural unit. Two regions are two `Provider`s, scheduled across like two
  vCenters.
- **Failure domains are the region's availability zones**, published on
  `Provider.status` the way vSphere publishes failure domains (ADR-0002,
  ADR-0023, ADR-0030). `ScopedTarget` is keyed by `(datacenter, cluster)`
  today; the ADR decides whether a zone fills one of those slots or the
  type gains a zone key. Placement across zones reuses ADR-0098 unchanged.
- **Ownership marker on every cloud resource**: a tag carrying the owning
  object's UID and the `Provider`'s name, the cloud equivalent of
  ADR-0101's vSphere marker. banlieue deletes only what carries its marker,
  and adopts a tagged resource instead of creating a second one (a create
  that times out on the client may have succeeded on the server).
- **Orphan sweep.** A resource that bills by the second and outlives its
  object is a cost, not just litter. The provider lists its own tagged
  resources on a slow cadence and deletes any whose owner no longer exists,
  after a grace period, logging each deletion. This is a reconcile over a
  listed snapshot, not a poll in the reconcile path.

### ADR-0159: credentials by workload identity federation

The provider's ServiceAccount token is the only credential. AWS trusts it
through an IAM OIDC identity provider and `AssumeRoleWithWebIdentity`;
Azure through a federated identity credential on a user-assigned managed
identity or app registration. No access key, client secret or certificate
is stored in a `Secret`, which removes a rotation problem (ADR-0095) and a
Secret read from the provider's Role.

- Each `Provider` names its role or identity in spec; the operator renders
  a projected token with that cloud's audience into the provider's pod.
- **Prerequisite the ADR must state plainly:** the cluster's service account
  issuer must be published where the cloud can fetch its OIDC discovery
  document and keys. A cluster whose issuer is not reachable cannot use
  these providers without that step. The guide documents it; banlieue does
  not publish the issuer for you.
- Least privilege per class: the ADR lists the exact actions each provider
  needs (and the image publisher, separately), as a policy document in
  `deploy/`, scoped by tag condition where the cloud supports it.
- Static keys from a `Secret` are a documented fallback only if federation
  proves unworkable somewhere specific; the default and the examples use
  federation.

### ADR-0160: a `VMClass` becomes an instance size by explicit mapping

A cloud does not take "4 vCPU, 16 GiB"; it takes `m7i.xlarge` or
`Standard_D4s_v5`. The admin maps each `VMClass` to an instance size per
`Provider` (a new capability list beside `storageClasses` and
`networkClasses`), and the provider reports which mapped sizes are offered
in which zone. No best-fit guessing: a class with no mapping is not
schedulable on that `Provider`, and the scheduler says so. Storage classes
map to disk SKUs (`gp3`, `io2`; `Premium_LRS`, `PremiumV2_LRS`); network
classes map to subnet and security group (or NSG) identifiers per zone.

### ADR-0161: capacity on a cloud is quota, not measured hardware

ADR-0128 has every provider publish measured capacity. A cloud has no host
to measure. The provider publishes **quota headroom** (vCPU quota per
instance family in the region, from Service Quotas or the Compute usage
API) and the scheduler treats it as ADR-0107 treats capacity. The ADR must
also cover the failure quota cannot predict: the cloud has no capacity for
that size in that zone right now (`InsufficientInstanceCapacity`,
`ZonalAllocationFailed`). That is a placement failure on one zone, retried
in another zone of the same `Provider` with back-off, and surfaced as a
condition, never a silent spin.

### ADR-0162: publishing a `VMImage` into a cloud

This is the centre of the roadmap. Today `banlieue-imagebuilder` produces a
raw disk (ADR-0010) and an OCI artifact for host-resident providers
(ADR-0064); each provider imports from there. A cloud image is a different
kind of thing: a registered, regional, billable object in the cloud's own
catalogue. The ADR decides:

1. **Who publishes.** A publish Job in the imagebuild namespace (ADR-0016)
   with its own federated identity, separate from the provider's, because
   publishing (create snapshot, register image) needs rights the runtime
   provider should not hold. The provider only reads that a published image
   exists and boots it.
2. **What is published, per cloud.**

   | Cloud | Artifact uploaded | Registered as | First-party path |
   | --- | --- | --- | --- |
   | EC2 | raw disk | EBS snapshot, then AMI (`RegisterImage`, `BootMode=uefi`, `TpmSupport=v2.0` when the class needs a vTPM) | EBS direct APIs (`StartSnapshot`, `PutSnapshotBlock`, `CompleteSnapshot`): no S3 bucket, no `vmimport` service role. `ImportSnapshot` from S3 is the fallback the ADR weighs |
   | Azure | fixed VHD (raw plus a 512-byte footer; banlieue writes it, no `qemu-img`) | managed disk by direct upload, then an image version in an Azure Compute Gallery image definition (`SecurityType=TrustedLaunch` when the class needs a vTPM) | direct upload to a managed disk through a short-lived write SAS; page blob as the fallback |
   | Lambda MicroVMs | build context (Dockerfile plus content) as a zip in S3 | MicroVM image from a managed base image | the service builds it; banlieue uploads and calls the create API (roadmap 25) |

3. **How the `VMImage` asks for it.** Explicitly (non-negotiable 4): either a
   per-Provider publish target on the `VMImage`, or a publish policy on the
   cloud `Provider` that names which images it carries. Status gains one
   entry per Provider with the cloud identifier (AMI ID, gallery image
   version ID, MicroVM image ARN), the region, and **the source digest it
   was published from**, so a rebuilt image is a new publication, never an
   overwrite in place. `ImageSourceKind::Template` keeps working for an
   image an admin published by hand (an existing AMI or gallery version),
   for brownfield.
4. **Lifetime.** A publication banlieue created carries the ownership tag
   and is deleted when its `VMImage` is deleted or rebuilt and no machine
   still references it; a hand-published `Template` image is never touched
   (ADR-0028's template deletion lifecycle, applied to clouds).
5. **Integrity.** The publish Job verifies the source digest before upload
   and records what it registered; a provider boots only an image whose
   recorded digest matches the `VMImage` it was asked for.
6. **Pre-laid and sealed images.** ADR-0113's pre-lay runs before publish,
   so a cloud image is already pre-laid. First-boot sealing (ADR-0088) is
   valid only where the instance gets a vTPM (Azure Trusted Launch, EC2
   NitroTPM); the ADR states which `VMImage` flags each target accepts.

`BuildArtifactKind` grows a VHD (or the publish Job converts on the fly);
whichever, roadmap 15's first-party VMDK writer and this VHD writer should
share one disk-format module rather than grow two.

### ADR-0163: `GuestReady` on a cloud

ADR-0043 gives `GuestReady` one condition and a per-backend transport
(guestinfo on vSphere, the guest agent on libvirt, vsock on Cloud
Hypervisor). A cloud instance has no host channel banlieue controls. The
candidate transports, for the ADR to choose between with measurements:

- the **serial console**, read by API (EC2 `GetConsoleOutput` with
  `Latest`; Azure boot diagnostics with managed storage). Read-only, no
  agent, no credential in the guest; the question is latency and rate
  limits;
- a **cloud guest agent** (SSM on EC2, the Azure guest agent): needs an
  agent and, on EC2, an instance role. Weighed against the read-only rule
  ADR-0043 and ADR-0045 hold to;
- a guest that **calls out** to an endpoint: rejected unless the others fail,
  because it puts a credential in the guest.

## Questions for phase 0 (no code until answered)

- **SDK and TLS stack.** The workspace's rustls runs on `ring`. The AWS SDK
  for Rust defaults to `aws-lc-rs`, a native dependency, and post-quantum
  key exchange (X25519MLKEM768) needs `aws-lc-rs` anyway. Decide once for
  the workspace: move the process provider to `aws-lc-rs` (and accept its C
  build), or call AWS through a first-party SigV4 client on the existing
  `reqwest`. For Azure there is no maintained official Rust crate for the
  Compute management plane, so a first-party ARM client (as
  `banlieue-proxmox` and `banlieue-libvirt` are first-party) is the
  expected answer; confirm it.
- **Post-quantum per link.** Measure which cloud endpoints (STS, EC2, EBS
  direct, ARM, Entra ID token endpoint, Lambda) negotiate X25519MLKEM768
  today, and record the ones that do not.
- **Cost guard for live tests.** Every live and e2e test creates billable
  resources. The harness tags them, deletes them in a drop guard and on
  the next run, and fails loudly if a previous run left anything. Account,
  subscription and region come from the environment, never the repo
  (`rules/no-real-infrastructure.md`).

## Phases

| Phase | What | Status |
| --- | --- | --- |
| 0 | The phase 0 questions above, answered in ADR-0158 and ADR-0159 | ⛔ |
| A | API: cloud `Provider` connection (account or subscription, region, identity), instance-size mapping, zone failure domains, quota on status; `VMImage` publish targets and per-Provider publication status; `regen-crds`, examples | ⛔ |
| B | Operator renders the projected token with the cloud audience; federation guide; least-privilege policy documents in `deploy/` for each provider and for the publisher | ⛔ |
| C | Shared cloud crate (name decided in ADR-0158): ownership tags, adopt-or-create, orphan sweep, quota-to-capacity, zone retry on capacity errors | ⛔ |
| D | Image publish Job: EC2 (EBS direct, AMI), Azure (VHD writer, direct upload, gallery version), digest recorded, deletion lifecycle; Lambda MicroVM publish lands in roadmap 25 | ⛔ |
| E | `GuestReady` transport chosen and implemented (ADR-0163), used by roadmaps 23 and 24 | ⛔ |
| F | Docs: provider comparison gains cloud columns; threat-model full pass for ADR-0158 to ADR-0163 | ⛔ |

## Threat model, in advance

New actors and boundaries the passes will have to cover: the cloud control
plane as a trust boundary (TB-4 class, but across the internet); the
federated token (who can mint it, its audience, its lifetime); the publish
identity's write rights in the cloud account; a published image readable by
anyone in the account; instance metadata services reachable from the guest
(IMDSv2 required on EC2, hop limit 1); cost exhaustion as a denial of
service. Not recorded here as findings: these are the sections to walk.

## Out of scope

Creating VPCs, VNets, subnets, load balancers or DNS (use CAPA, CAPZ or the
admin's own tooling); spot or preemptible instances (a later ADR: they need
an eviction path through `VirtualMachine` status); GCP and other clouds
(the foundation should make each a provider roadmap, not a new foundation);
cross-cloud migration (roadmap 14 rules out cross-class migration).

## Read first

ADR-0002, 0003, 0010, 0012, 0016, 0028, 0043, 0064, 0095, 0098, 0101, 0107,
0113 and 0128; then roadmap 15 (the disk-format writer this roadmap should
share).

## Definition of done

- [ ] Stop condition above, verified live against a real AWS account and a
      real Azure subscription.
- [ ] ADR-0158 to ADR-0163 accepted, each with its threat-model pass.
- [ ] No cloud key in any `Secret` in the shipped manifests or examples.
- [ ] `cargo deny` clean with whatever the phase 0 SDK decision added.
- [ ] `ROADMAPS.md` updated.
