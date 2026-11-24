<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0160: A VMClass becomes an instance size by explicit mapping

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 22, roadmap 23. [ADR-0030](0030-per-zone-capability-targets.md)
  (per-zone capability targets), [ADR-0158](0158-shape-of-a-cloud-provider-class.md)
  (zones as failure domains), ADR-0161 (capacity), ADR-0162 (image
  definitions declare disk controllers).

## Context

A cloud does not take "2 vCPU, 8 GiB"; it takes `Standard_D2s_v7` or
`m7i.large`. Hundreds of sizes exist per region, priced and offered
differently. The roadmap 23 spike also showed that "offered" is not one fact:

1. In one region a size family was offered in three zones and **restricted
   for this subscription in all three** (`NotAvailableForSubscription`, type
   `Zone`); another family was usable in two of four zones. The resource-SKU
   listing carries both the offered zones and the subscription's
   restrictions; only their difference is usable.
2. **Every 2-vCPU size the subscription could use there was NVMe-only.** A
   VM create from an image whose definition declared only SCSI failed at
   deploy time ("cannot boot with OS image or disk ... disk controller
   types"). Whether a size can boot an image depends on the image too.
3. **The OS disk is created at the image's size unless the VM says
   otherwise.** A Kairos image of 2.3 GiB needs room for its state and
   persistent partitions on first boot; with a disk the image's size, its
   first-boot reset failed ("state partition not found"). Disk size is part
   of what a class must decide.

## Decision

### 1. `spec.capabilities.instanceSizes` on a cloud `Provider`

A new capability list beside `storageClasses` and `networkClasses`:

```yaml
capabilities:
  instanceSizes:
    - vmClass: small          # a VMClass name
      size: Standard_D2s_v7   # the cloud's size name
      perZone:                # optional, ADR-0030 shape with a zone key
        - zone: "2"
          size: Standard_D2as_v6
```

A `VMClass` with no mapping is **not schedulable** on that `Provider`, and
the scheduler says so in its condition. No best-fit guessing from vCPU and
memory: price, CPU generation and disk controller differ between sizes that
look alike.

### 2. The provider reports what is usable, per zone

The provider lists resource SKUs on a slow cadence and publishes, per
failure domain, which mapped sizes are **offered there and not restricted
for this subscription** (finding 1). A mapped size unusable in every zone
raises a condition on the `Provider`.

### 3. Admission checks the size against the image

A size whose disk controller types (finding 2) do not intersect the image
definition's is refused before any create: by the provider at placement
time, recorded as the reason, never left to fail at the cloud.

### 4. Disk size comes from the class, with a floor

`VMClass` disk sizes map to the OS disk size (`diskSizeGB`). The provider
refuses a size below the image's floor: the image's virtual size plus what
its first boot lays out (finding 3; ADR-0162 records the floor on the
published image). Storage classes map to disk SKUs (`Premium_LRS`,
`StandardSSD_LRS`, `gp3`) through the existing `storageClasses` list;
network classes map to a subnet and security group per zone through
`networkClasses` with `perZone` keyed by `zone` (ADR-0158 Decision 3).

## Consequences

- The admin writes one line per class per `Provider`; adding a region means
  re-checking which sizes it offers, which is exactly the work the provider's
  per-zone report makes visible.
- A class can resolve to different sizes in different zones when a family is
  restricted in one of them (finding 1).
- `regen-crds` applies: `ProviderCapabilities` gains `instanceSizes`, and
  `ScopedTarget` its `zone` key (ADR-0158).

## Alternatives considered

- **Best fit from vCPU and memory.** Rejected: picks a cheaper, older or
  NVMe-incompatible size silently.
- **The size on the `VirtualMachine`.** Rejected: ties a workload to one
  cloud, against the point of `VMClass`.
