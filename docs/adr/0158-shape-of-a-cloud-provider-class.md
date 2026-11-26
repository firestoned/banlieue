<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0158: The shape of a cloud provider class

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Amended:** 2026-11-25 (Decision 4 adds `banlieue-owner-namespace`;
  Decision 5 bounds a home-only provider's sweep to its home namespace)
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 22 (public-cloud foundation) and roadmap 23 (Azure),
  whose phase 0 spike this records; roadmap 24 (EC2) confirms the AWS half.
  Builds on [ADR-0003](0003-provider-deployment-topology.md) (managed
  provider workloads), [ADR-0101](0101-vspheremachine-ownership-marker-and-adoption.md)
  (ownership marker and adoption), [ADR-0098](0098-spread-placement-across-failure-domains.md)
  (spreading across failure domains), [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md)
  (shared providers). Companion ADRs: ADR-0159 (credentials), ADR-0160
  (instance sizes), ADR-0161 (capacity), ADR-0162 (publishing images),
  ADR-0163 (`GuestReady` on a cloud).

## Context

Every backend banlieue drives today is one the operator runs: a vCenter, a
Proxmox cluster, a libvirt or Cloud Hypervisor host. Roadmap 22 adds backends
the operator rents: Azure virtual machines first, then EC2. They share a set of
questions that should be answered once, before either provider's code exists:
what one `Provider` stands for, what a failure domain is, how banlieue marks and
finds what it created, and what happens to a resource that outlives the object
that asked for it.

The answers were not guessed. A phase 0 spike on a real Azure subscription
(2026-11-22 to 2026-11-24) published a banlieue-built image, booted Trusted
Launch VMs in two zones of one region, failed and deleted them repeatedly, and
recorded what the platform did. The findings that bear on this ADR:

1. **A failed create leaves litter.** A VM create rejected by the platform
   (the size could not boot the image) had already created its NIC. The VM's
   own `deleteOption: Delete` on NIC and disk cannot help when the VM never
   existed. Something has to find the NIC by what banlieue wrote on it.
2. **Deleting the VM with `deleteOption: Delete` on its NIC and OS disk
   removes all three** in about 12 s, every time it was tried.
3. **Tag names cannot contain `/`.** `banlieue.io/owner-uid` is rejected
   (`InvalidTagNameCharacters`); only names under reserved prefixes may. The
   Kubernetes label convention cannot be copied.
4. **Zones are per subscription and not uniform.** In one region the
   subscription saw four zones, not three; some sizes were offered in a zone
   yet restricted for this subscription there (`NotAvailableForSubscription`,
   type `Zone`); one size family was unusable in every zone it was offered in.
5. **Deletes are throttled hard on a new subscription.** The fourth delete
   in a row returned `SubscriptionRequestsThrottled` with a one-second retry
   hint. List views lag behind deletes for some seconds.
6. **The regional quota is the real ceiling.** A new subscription had four
   vCPUs per region and per family.

## Decision

### 1. One `Provider` is one account (or subscription) in one region

Images, quotas, networks and zones are all regional in both clouds, so a region
is the unit. Two regions are two `Provider`s, scheduled across like two
vCenters. Two subscriptions are two `Provider`s even in the same region, and
**their zones are not assumed to line up**: zone `1` in one subscription need
not be zone `1` in another.

### 2. Deployment `Managed`, in the cluster

A cloud provider runs as the vSphere and Proxmox providers do (ADR-0003): one
workload per `Provider`, its own ServiceAccount and Role, in the provider's
namespace, serving tenant namespaces through `ProviderScope` (ADR-0133). There
is no host, so nothing is host-resident.

### 3. Failure domains are the region's zones, as the subscription sees them

The provider publishes one failure domain per zone **that offers at least one of
the `Provider`'s mapped sizes, after the subscription's restrictions**, never a
fixed list of three. Each carries the zone name the cloud uses (`1`, `2`,
`us-east-1a`) in `labels.zone`. `ScopedTarget` gains an optional `zone` key
beside `datacenter` and `cluster`, so per-zone storage and network targets
(ADR-0030) use the same shape
([ADR-0030](0030-per-zone-capability-targets.md)). Placement across zones
reuses ADR-0098 unchanged.

### 4. An ownership tag on every resource banlieue creates

Every cloud resource the provider or the publish Job creates (VM, NIC, disk,
snapshot, image version, published image) carries these tags:

| Tag | Value |
| --- | --- |
| `banlieue-owner-uid` | the owning object's `metadata.uid` |
| `banlieue-owner-kind` | the kind that created it: the cloud InfraMachine kind (named by ADR-0164), `VMImage`, ... |
| `banlieue-provider` | `<namespace>.<name>` of the `Provider` |
| `banlieue-owner-namespace` | the owning object's namespace, for a namespaced owner (amended 2026-11-25) |

Names use `-`, never `/` (finding 3), and the same names are used on both
clouds. **banlieue deletes only a resource whose `banlieue-owner-uid` equals
the object it is deleting for, and adopts a tagged resource instead of
creating a second one** (ADR-0101's rule, applied to clouds): a create whose
response was lost may have succeeded.

Resource **names** are derived from the owning object's UID as well, so a
retried create is the same resource, not a second one.

### 5. The orphan sweep

A resource that bills by the second and outlives its object is a cost, not
litter. Each provider, on a slow cadence (default 10 minutes), lists the
resources tagged with its own `banlieue-provider` value and deletes any whose
`banlieue-owner-uid` names no live object of `banlieue-owner-kind`, **after a
grace period** (default 15 minutes since the resource's creation, so a create
in flight is never swept). Each deletion is logged with the tag values and
counted in a metric. It is a reconcile over a listed snapshot, never a poll in
the create path. Deletes honour the cloud's retry hint and are paced (finding
5); a throttled sweep resumes on the next pass.

The sweep never touches an untagged resource, a resource tagged for another
`Provider`, or anything outside the resource group (or account boundary) the
`Provider` names.

**A sweep judges only what its listing can see** (amended 2026-11-25). A
provider that serves tenant namespaces (ADR-0133) lists machines
cluster-wide and may judge any namespace's resources. A provider that serves
only its home namespace lists that namespace alone, so it judges only
resources whose `banlieue-owner-namespace` is its home namespace, and never
one without the tag. Without this, a `Provider` switched from serving tenant
namespaces back to home-only would read every tenant machine's UID as
missing and delete live tenant VMs.

### 6. Deletion

A VM is created with its NIC and OS disk set to delete with it. Deleting a
machine deletes the VM, then confirms by tag that its NIC and disks are gone,
deleting any that remain. A delete of a resource that is already gone is
success, never an error.

## Consequences

- A cloud `Provider` is cheap to add (one per region) and its failure domains
  are honest about what this subscription can actually use there.
- The orphan sweep makes a crash mid-create recoverable without a person: the
  spike's orphaned NIC is exactly the case.
- Tag-based ownership requires list permission on the resource group or
  account, scoped by tag where the cloud allows it (ADR-0159 lists the
  actions).
- `ScopedTarget` gains a field; it is optional, so existing objects are
  unchanged. `regen-crds` applies.
- **Throttling is a first-class condition**, not an error to retry blindly:
  sustained throttling is surfaced on the `Provider`.
- The EC2 half (tags, zones as `us-east-1a`, idempotency through
  `RunInstances` client tokens) follows the same rules; roadmap 24's spike
  confirms it before code.

## Alternatives considered

- **One `Provider` per subscription spanning regions.** Rejected: images,
  quota and networks are regional, so every spec field would need a region
  key, and a regional outage would mark a whole `Provider` unhealthy.
- **Kubernetes-style tag names (`banlieue.io/owner-uid`).** Not possible on
  Azure (finding 3).
- **No sweep, rely on `deleteOption`.** Insufficient: a create that fails
  before the VM exists leaves resources `deleteOption` never sees (finding 1).
