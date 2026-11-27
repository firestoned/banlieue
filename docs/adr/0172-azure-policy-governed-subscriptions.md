<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0172: Running in a policy-governed Azure subscription

- **Status:** Accepted
- **Date:** 2026-11-26
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-11-26 (the OS disk receives the extra tags after create, through ADR-0173 Decision 7);
  2026-11-27 (Azure creates the OS disk with the VM's tags, so it carries the extra tags from the create; the merge is removed)
- **Related:** Extends [ADR-0158](0158-shape-of-a-cloud-provider-class.md)
  Decision 4 (ownership tags) and Decision 1 (one `Provider` is one
  subscription in one region),
  [ADR-0159](0159-cloud-credentials-by-workload-identity-federation.md)
  Decision 3 (least-privilege role definitions),
  [ADR-0162](0162-publishing-a-vmimage-into-a-cloud.md) (the publish Job and
  what it creates), [ADR-0164](0164-azure-inframachine-and-a-first-party-arm-client.md)
  (the VM body and the fake that rejects what ARM rejects),
  [ADR-0165](0165-trusted-launch-vtpm-identity-on-azure.md) (the
  `securityProfile` this one shares). Roadmap 23.

## Context

An enterprise Azure subscription is governed by Azure Policy assignments
banlieue does not control. Three common ones, each with effect `deny`, make
every create banlieue sends fail today:

1. **Required tags.** A cost centre, an owner, an environment: a resource
   without them is refused. banlieue sets only its ownership tags
   (ADR-0158 Decision 4), so the VM, the NIC and every resource the publish
   Job creates are refused. `modify` or `append` policies can work around it,
   but only where the admin owns the policy.
2. **Customer-managed keys.** Managed disks (and, by the same built-in
   policies, snapshots and gallery image versions) must name a disk
   encryption set (DES). banlieue never sets one, so the OS disk is created
   with platform-managed keys and refused.
3. **Encryption at host.** VMs must set `securityProfile.encryptionAtHost`,
   which banlieue never does.

All three are facts about where a resource is created, not about which VM
it is. The question is where each one is declared: on the `Provider`
(`spec.connection.azure`), on a storage class target (as the disk SKU is,
ADR-0160), on the `VMClass`, or on the `AzureVirtualMachine` the controller
resolves.

Constraints from Microsoft's documentation, which settle part of it:

- A DES is a regional resource, and "most resources related to your
  customer-managed keys (disk encryption sets, VMs, disks, and snapshots)
  must be in the same subscription and region"
  ([server-side encryption of managed disks](https://learn.microsoft.com/azure/virtual-machines/disk-encryption)).
  A gallery image version's DES must be in the image's subscription, one per
  region ([encrypted image versions](https://learn.microsoft.com/azure/virtual-machines/image-version-encryption)).
  A DES may sit in another resource group (often the security team's).
- Referencing a DES in a disk, snapshot, VM or image version is a linked
  access check: the caller needs `Microsoft.Compute/diskEncryptionSets/read`
  on the DES (the failure mode recorded in
  [OCPBUGS-17919](https://issues.redhat.com/browse/OCPBUGS-17919) for
  OpenShift's Azure machine identity). The DES's own managed identity needs
  access to the key in Key Vault; that grant is the admin's, never banlieue's.
- Encryption at host requires the subscription feature
  `Microsoft.Compute/EncryptionAtHost` to be registered, and a VM size that
  supports it. It cannot be enabled on a running VM (deallocate first).
- Azure tag names are case-insensitive for operations
  ([tag resources](https://learn.microsoft.com/azure/azure-resource-manager/management/tag-resources)):
  `Banlieue-Owner-Uid` and `banlieue-owner-uid` are the same tag. At most 50
  tags per resource, names up to 512 characters without `<>%&\?/`, values
  up to 256.

## Decision

### 1. Extra tags: `spec.connection.azure.tags`, on the `Provider`

A map of tag name to value, applied to every resource banlieue creates by
name for that `Provider`: the NIC, the VM, and the publish Job's image
definition, upload disk, snapshot and image version.

The OS disk needs nothing of its own. Azure creates it implicitly from the
VM's `storageProfile`, and the VM body has no field that tags it, but Azure
creates it with the VM's tags, the extra tags included, in the same write.
(Amended 2026-11-27. This section first said Azure does not copy the VM's
tags to the disk, so a required-tag `deny` policy covering
`Microsoft.Compute/disks` would refuse the VM create, and the 2026-11-26
amendment had ADR-0173 Decision 7 merge the tags on after create. Observed
on 2026-11-27, compute API 2024-07-01: every OS disk of five banlieue VMs
carried exactly its VM's tags, and the activity log shows one
`Microsoft.Compute/disks/write` at create and no tag write. A `deny` policy
therefore judges the disk with the tags. The merge is removed; ADR-0173 as
amended the same day.)

- **Ownership tags always win, and cannot be forged.** A name that starts
  with `banlieue-`, compared case-insensitively, is refused: that prefix is
  banlieue's namespace (the four ownership tags and
  `banlieue-source-sha256`, which records the digest a published image was
  verified against, ADR-0162 Decision 6). Because Azure folds tag-name case,
  an exact-match check is not enough: `Banlieue-Owner-Uid` would replace the
  owner tag on ARM's side and turn a deletion-guard into a forgery.
- **Two names that differ only in case are refused**, for the same reason.
- **At most 45 extra tags.** 50 per resource, minus the four ownership tags
  and `banlieue-source-sha256` on an image version, so every resource fits.
- Names and values follow Azure's rules above.

*Why the `Provider` and not the `VirtualMachine` or `VMClass`*: required-tag
policies are assigned per subscription or resource group, which is exactly
what a `Provider` names (ADR-0158 Decision 1), and the publish Job, which has
no `VirtualMachine`, needs the same tags. Per-VM tags set by a tenant (a cost
centre per team) are a different feature: they would need an allowlist, and
a tenant-written tag on a shared subscription is a new input crossing into
ARM. Not now.

### 2. Customer-managed keys: `spec.connection.azure.diskEncryptionSetId`, on the `Provider`

The resource ID of one DES. When set:

- the VM's OS disk carries `storageProfile.osDisk.managedDisk.diskEncryptionSet.id`;
- the publish Job's upload disk and snapshot carry
  `properties.encryption = {type: EncryptionAtRestWithCustomerKey, diskEncryptionSetId}`;
- the image version's one target region carries
  `encryption.osDiskImage.diskEncryptionSetId`.

It must be an ARM ID of the shape
`/subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.Compute/diskEncryptionSets/<name>`,
**in the `Provider`'s own subscription** (Microsoft's restriction, so it is
refused early instead of failing at the first create).

*Why the `Provider` and not a storage class target (beside `sku`)*: a CMK
policy covers every disk, snapshot and image the `Provider` creates, and the
publish Job's three have no storage class. A per-class key would also let a
class without one slip through and fail as a denied VM create long after
configuration, where a `Provider` setting is one place to get right. A DES is
also regional and subscription-bound, which is what one `Provider` is; it is
not a capability the scheduler matches on, which is what a storage target
holds. If a site needs separate keys per tier, a per-class override can be
added later without changing this field's meaning.

### 3. Encryption at host: `spec.connection.azure.encryptionAtHost`, on the `Provider`

A boolean. When true, the VM body carries `securityProfile.encryptionAtHost:
true`, beside Trusted Launch's settings when the class asks for them
(ADR-0165).

*Why the `Provider` and not the `VMClass`*: the policy is all-or-nothing for
the subscription, so a per-class switch would let a tenant choose a class the
policy denies; and `VMClass` is provider-agnostic (the same class runs on
vSphere or libvirt), where this bit means nothing. The cost is that every
mapped size must support it: the admin chooses supported sizes in
`instanceSizes` (ADR-0160), and an unsupported one is refused by ARM and
reported as `CreateRefused`.

### 4. None of it is on the `AzureVirtualMachine`

The provider reads the three settings from the `Provider` at create time,
as it already reads the subscription and resource group, and the
controller's `build_azure_virtual_machine` does not change. The infra CR
holds the scheduling decisions (zone, size, image, SKU, subnet); these are
facts of the connection. Carrying them on the infra CR would also let anyone
able to write an `AzureVirtualMachine` directly (a CAPI user, ADR-0164
Decision 1) drop the key or the host encryption for their VM, and the
publish Job has no infra CR at all.

### 5. Validated three times

- **Admission** (`deploy/admission/provider-azure-governance.yaml`, CEL,
  opt-in like the rest of `deploy/admission/`): the DES ID shape and
  subscription, the `banlieue-` prefix, reserved characters, lengths and the
  count. Case-folded duplicates are not checkable in portable CEL here and
  are left to the next layer.
- **The provider**, on every `Provider` reconcile (`checked_azure_connection`):
  all of the above plus case-folded duplicates. A violation sets the
  `Provider` `Ready=False` with reason `ConnectionInvalid` and the provider
  builds no ARM client for it, so nothing is created with the wrong tags.
- **The request bodies** in `banlieue-azure` refuse an invalid tag set or
  DES ID before any call, and `FakeArm` refuses what ARM would: a malformed
  DES ID, one in another subscription or region, one that does not exist,
  encryption at host without the subscription feature, and every tag rule.

### 6. RBAC: one more action, on the DES only

Both identities (runtime and publisher) need
`Microsoft.Compute/diskEncryptionSets/read` **on the DES**, and nothing else
new. Rather than widening the two existing roles to the DES's resource
group, a third custom role,
`deploy/provider-azure/disk-encryption-set-reader-role-definition.json`, holds
that one action and is assigned on the DES resource to each identity.
Encryption at host needs no permission beyond `virtualMachines/write`; it
needs the subscription feature, which the admin registers
(`az feature register --namespace Microsoft.Compute --name EncryptionAtHost`,
then `az provider register --namespace Microsoft.Compute`). banlieue does not
automate either: both are subscription-wide changes outside its resource
group.

## Consequences

- A `Provider` can be pointed at a subscription with required-tag,
  CMK and encryption-at-host `deny` policies and create into it.
- Changes apply to resources created afterwards. An existing VM is not
  re-tagged, re-keyed or moved to host encryption: the VM is created once
  (ADR-0164 Decision 3) and changing its DES or host encryption needs a
  deallocation banlieue does not schedule. Re-tagging is a possible later
  increment (a tags-only PATCH is cheap); re-keying is not planned.
- Once an image version is published with a customer-managed key it cannot
  go back to platform keys (Microsoft), and a CMK-encrypted version cannot be
  the source of another. banlieue never uses a version as a source, so only
  the first matters: dropping `diskEncryptionSetId` from a `Provider` leaves
  its existing versions encrypted, which is harmless.
- A disabled, deleted or expired key shuts down every VM whose disk uses it
  (Microsoft). That is the key owner's control, and is recorded in the
  threat model as such: banlieue cannot recover a VM whose key is revoked.
- The upload disk and snapshot with a DES are not yet exercised against live
  ARM; the request shapes follow the Disks and Snapshots REST APIs. The live
  run is an open item on roadmap 23.
- **The OS disk carries the extra tags from the create** (Decision 1, as
  amended 2026-11-27): Azure creates it with the VM's tags, so a required-tag
  `deny` policy covering managed disks judges it with them, and audit and
  `modify` policies and cost reports see them. This rests on Azure's
  observed behaviour, not on a documented contract: the first VM under such
  a policy is the test, and a refused create reports the policy's name in
  its `CreateRefused` message. Should Azure stop copying the tags, creating
  the OS disk as a resource of its own, tagged before the VM, is the fix,
  and stays open on roadmap 23.
- **Out of scope: `DeployIfNotExists` extensions** (monitoring agents,
  Defender). banlieue creates VMs with `provisionVMAgent: false` (ADR-0163):
  the image reports readiness itself, so extension deployment fails. That is a
  policy conversation (an exemption for the VM resource group, or an agent in
  the image), not a code change.
- Out of scope too: a policy that denies public network access on managed
  disks breaks the publish Job's SAS upload (ADR-0162); the admin exempts the
  gallery resource group's temporaries or publishes by hand.
