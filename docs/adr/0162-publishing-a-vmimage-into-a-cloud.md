<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0162: Publishing a VMImage into a cloud

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-11-26 (Decision 4: the `VMImage`'s cloud source entry is
  the declaration, as for vSphere, instead of `spec.publish.providers[]`;
  Decision 8 added: the published set is reconciled to the declared one;
  Decision 3's long-running version create is resumed by adoption);
  2026-11-26 again (Decision 2's EC2 row and Decision 9: EC2 publishes a
  pre-laid disk through VM Import, not the EBS direct APIs, after roadmap
  24's spike)
- **Related:** Roadmap 22 (the centre of it), roadmap 23 (Azure), roadmap 24
  (EC2). [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md) (the image
  builder produces a raw disk), [ADR-0016](0016-imagebuild-namespace-isolation.md)
  (imagebuild namespace), [ADR-0028](0028-vmimage-template-deletion-lifecycle.md)
  (template deletion lifecycle), [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md),
  [ADR-0088](0088-first-boot-sealing-for-immediate-images.md) (first-boot
  sealing), [ADR-0103](0103-digest-pinned-installs.md) (digest pinning),
  [ADR-0158](0158-shape-of-a-cloud-provider-class.md) (ownership tags),
  [ADR-0159](0159-cloud-credentials-by-workload-identity-federation.md)
  (the publisher's own identity), [ADR-0160](0160-a-vmclass-becomes-an-instance-size-by-explicit-mapping.md)
  (disk controllers, disk-size floor).

## Context

A host-resident provider imports banlieue's raw disk and boots it. A cloud
boots only images registered in its own catalogue: an Azure Compute Gallery
image version, an AMI. Registering one is a separate, slower, billable act
with rights the runtime provider should not hold.

The roadmap 23 phase 0 spike published a banlieue-built Kairos image into an
Azure Compute Gallery eight times by hand (2026-11-23 to 2026-11-24) and
booted it as a Trusted Launch VM. What it found:

1. **The raw disk needs no conversion beyond a footer.** auroraboot's raw
   disk (GPT, EFI system partition) became a fixed VHD by appending the
   512-byte footer; its size was already a whole number of MiB.
2. **Direct upload works with plain Put Page.** A managed disk opened for
   upload takes 4 MiB `Put Page` requests through a write SAS valid for one
   hour; **52 to 53% of a 2.5 GB image was zeros and could be skipped**.
   Throughput was bound by the uplink (2.5 MiB/s one stream, 3.1 MiB/s eight
   streams), not the protocol.
3. **Two places must declare NVMe.** The image definition
   (`DiskControllerTypes=SCSI,NVMe`) and **the source snapshot or disk**
   (`supportedCapabilities.diskControllerTypes`); with only the first, the
   version create fails (`does not have a matching diskControllerType`).
   Image definition features are immutable once created.
4. **Registering is slow and variable.** One image version took 1351 s, the
   same-size next one 425 s, later ones about 430 to 490 s.
5. **Lifecycle defaults are changing.** The CLI warns that new versions will
   default to a six-month end-of-life and to blocking deletion before it.
6. **Temporaries cost more than the image.** The upload disk billed as a
   32 GiB tier; the snapshot and version billed by used size. Both
   temporaries could be deleted as soon as the version succeeded: the
   version keeps its own copy.
7. **A Kairos raw image needs a disk larger than itself** on first boot
   (ADR-0160 finding 3).

## Decision

### 1. A publish Job, with its own identity

Publishing runs as a Job in the imagebuild namespace (ADR-0016) under its own
federated identity (ADR-0159 Decision 3), separate from any provider's. The
runtime provider can read that a published image exists and boot it; it
cannot create, register or delete images.

### 2. What is published, per cloud

| Cloud | Uploaded | Registered as | Path |
| --- | --- | --- | --- |
| Azure | fixed VHD: the raw disk plus a 512-byte footer, written by banlieue (no `qemu-img`) | managed disk (direct upload, write SAS), snapshot, then an image version in a Compute Gallery image definition | Put Page in 4 MiB chunks, **all-zero chunks skipped**, several in parallel |
| EC2 | *amended:* the **pre-laid** disk (ADR-0113, ADR-0175) as a stream-optimized VMDK written by banlieue | an encrypted EBS snapshot by VM Import (`ImportSnapshot` from an S3 staging object), then an AMI with `BootMode=uefi`, and `TpmSupport=v2.0` when needed | Decision 9 |

The VHD footer writer and roadmap 15's VMDK writer share one disk-format
module.

### 3. Azure specifics, from the spike

- The image definition is created once per `(VMImage name, security type)`
  with `HyperVGeneration=V2` and features `SecurityType=TrustedLaunch` (when
  the image declares `tpmEnabled` or `firstBootSealing`) and **always
  `DiskControllerTypes=SCSI,NVMe`** (finding 3).
- The upload disk is created with
  `supportedCapabilities.diskControllerTypes = "SCSI, NVMe"` so its snapshot
  inherits it (finding 3).
- Every version is created with an explicit end-of-life (none, or a date the
  `VMImage` sets) and **`blockDeletionBeforeEndOfLife: false`** (finding 5).
- The upload disk and snapshot are deleted as soon as the version succeeds
  (finding 6).
- The version create is a long-running operation: the Job records the
  operation URL, exits, and is resumed; it never assumes a duration
  (finding 4). *Amended 2026-11-26:* every step is named deterministically
  (upload disk, snapshot, version), so a Job that is restarted, or a new Job
  for the same digest, adopts the step in flight by reading its
  provisioning state instead of starting a second one. The Job polls the
  version create with a deadline far beyond the observed 7 to 22 minutes and
  fails loudly at it; the provider's retry adopts what is still running.

### 4. How a `VMImage` asks for it: explicitly

*Amended 2026-11-26.* A `VMImage` declares a cloud by a source entry for its
class, exactly as it declares vSphere (ADR-0020): `providerClass: azure`,
`kind: Url` builds the image (or reuses the raw disk already built for it)
and publishes it into **every `azure` `Provider` that serves the
`VMImage`**; `kind: Template` names a version an admin published by hand.
The source entry is the declaration; nothing is published to a class the
`VMImage` does not name (non-negotiable 4). The earlier
`spec.publish.providers[]` list is dropped: it would make the cloud the one
backend whose images are declared differently from vSphere's, and the
per-`Provider` choice it offered is already made by which `Provider`s serve
the `VMImage`'s namespace (ADR-0133). The build must produce a raw disk
(`cloudImage`): a `VMImage` whose shared build is an ISO (a vSphere `Url`
source listed first) reports the cloud row not ready with a reason, and is
not published. Status gains one entry per `Provider`: the cloud's
identifier (image version ID, AMI ID), the region, **the source digest it was
published from**, and the published image's **minimum OS disk size**
(finding 7, consumed by ADR-0160 Decision 4). A rebuilt image is a new
version, never an overwrite. `ImageSourceKind::Template` keeps working for an
image an admin published by hand.

### 5. Lifetime

*See also Decision 8.*

A published image banlieue created carries the ADR-0158 ownership tags and is
deleted when its `VMImage` is deleted or superseded **and** no machine still
references it. A hand-published `Template` is never touched (ADR-0028).

### 6. Integrity

The Job verifies the source digest before upload and records the SHA-256 of
what it uploaded on the version as a tag (`banlieue-source-sha256`). A
provider boots only a published image whose recorded digest matches the
`VMImage` it was asked for.

### 7. Image flags per cloud

`firstBootSealing` (ADR-0088) is valid only on a target with a vTPM (Azure
Trusted Launch, EC2 NitroTPM). `installMode: Deferred` and ISO sources are
refused for cloud `Provider`s at admission: there is no install-media path.

### 8. The published set follows the declared set (added 2026-11-26)

banlieue's `VMImage`s are the declared state; the gallery is reconciled to
it. For each `Provider` serving a `VMImage` with a cloud `Url` source:

- a version for the current source digest that does not exist (never
  published, or deleted in the cloud by hand) is published;
- a version banlieue published (owner tags, ADR-0158) for a digest the
  `VMImage` no longer builds, and that no machine references, is deleted;
- when the `VMImage` is deleted, every version it owns is deleted once no
  machine references it, and its image definition with the last version;
- a version, definition or gallery banlieue did not tag is never touched,
  and the gallery itself is the admin's (declared on the `Provider`, never
  created or deleted by banlieue).

The check reads the version by name on the provider's slow cadence; it is a
reconcile over what was read, never a poll in the create path.

### 9. EC2 specifics, from the roadmap 24 spike (added 2026-11-26)

The spike (2026-11-24 to 2026-11-26) published through both the EBS direct
APIs and VM Import, and booted both. VM Import is the path:

- **Why not EBS direct.** It writes a snapshot block by block
  (`StartSnapshot`, `PutSnapshotBlock`, `CompleteSnapshot`) and worked, but
  its `ebs:` actions are a separate grant that governed accounts commonly
  withhold; the environment the spike targets does. VM Import needs only S3
  and EC2 Query actions, which the same accounts grant. The EBS direct path
  is not implemented.
- **What is uploaded.** The pre-laid disk (ADR-0113), never a raw image
  that boots into recovery: that first boot took 413 s to ready on EC2,
  the pre-laid one 49.9 s (ADR-0175 finding 4). Its persistent partition is
  laid out small (8 GiB in all in the spike) and grows on first boot. The
  Job writes it as a **stream-optimized VMDK** in `banlieue-diskformat`
  (the module roadmap 15 planned, `flate2` for the grains, already in the
  tree): all-zero grains are omitted and the rest compressed, so the
  upload carries roughly the data, not the disk.
- **Staging.** The object goes to an **admin-owned S3 bucket** named on the
  `Provider` (block public access on, expiry of one day and abort of
  incomplete multipart uploads as a lifecycle backstop) by multipart upload,
  64 MiB parts in parallel, under a key derived from the source digest. The
  object is deleted as soon as the import completes.
- **Import.** `ImportSnapshot` with `Encrypted=true` (the account's default
  EBS key, or a KMS key the `Provider` names) and the **admin's VM Import
  service role** (trusted by `vmie.amazonaws.com` with
  `sts:ExternalId=vmimport`, reading only the staging bucket). The
  publisher's identity holds `iam:PassRole` for that one role, to that one
  service, and no S3 read: VM Import reads the object, the publisher only
  writes it. The task is tagged at creation; the resulting snapshot is
  tagged by `CreateTags` as soon as it exists (in the spike, the task's
  tags did not reach it).
- **Register.** `RegisterImage` with `BootMode=uefi`, `EnaSupport=true`,
  `TpmSupport=v2.0` when the `VMImage` declares `tpmEnabled` or
  `firstBootSealing` (ADR-0167), one gp3 root volume of the template's size
  with `DeleteOnTermination`, tagged at creation.
- **Long-running and adoptable.** Upload took about 400 s and the import
  about 680 s for the 8 GiB template; neither is assumed. Every step is
  named or tagged from the source digest, so a restarted Job adopts the
  upload (by `ListParts`), the import task (by tag) or the snapshot (by
  tag) in flight, as Decision 3's Azure amendment does.
- **Deletion.** Deregister the AMI, then delete its snapshot, both only
  when tagged as banlieue's and unreferenced (Decision 8).

## Consequences

- One `VMImage` can run on hosts and in clouds; the cloud copy is traceable to
  the digest it came from.
- A publish takes minutes (upload bound by the publisher's bandwidth, plus a
  variable registration), so a pool rollout to a cloud waits on it; status
  shows where it is.
- Publishing from a site with a slow uplink is slow; running the Job in the
  cloud's region (where the cluster allows) is the remedy, not a design
  change.
- `BuildArtifactKind` is unchanged: the Job converts the raw disk on the fly.
- **Follow-up: publish pre-laid images.** A Kairos raw image spends its
  first boot in recovery mode laying out partitions and resetting into
  active (about 170 s of a 189 s create to ready, ADR-0163 finding 5); a
  reboot takes 18 s. Publishing the image already laid out, as ADR-0113 does
  for host providers, removes that from every create. Measure before
  deciding; it changes what is uploaded, not this ADR's shape.

## Alternatives considered

- **Upload through a page blob in a storage account.** Works, but needs a
  storage account and its access; direct upload to a managed disk needs
  neither.
- **`azcopy` in the Job.** A large third-party binary to do what Put Page
  does in a few hundred lines; the spike used Put Page directly.
- **The runtime provider publishes on first use.** Rejected: it would need
  image-registration rights, and the first VM would wait for the publish.
