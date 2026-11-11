<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0128: Every provider publishes capacity, including measured free memory and disk

- **Status:** Accepted
- **Date:** 2026-11-11
- **Proposed:** 2026-11-11
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0107](0107-capacity-aware-scheduling.md) (typed
  `capacity`, memory strict, CPU by ratio) to the vSphere, libvirt and Proxmox
  providers and to disk; keeps [ADR-0125](0125-a-placed-vm-keeps-a-placement-that-still-fits.md)
  (a placed VM keeps a placement that still fits); assumes
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)'s one
  host, one Provider for the host classes; answers part of roadmap 03's
  "capacity-aware even spread" question

## Context

ADR-0107 gave `FailureDomainAttributes` a typed `capacity` (`cpus`,
`memoryMiB`, `memoryOverheadMiBPerVM`) and made the scheduler fit each VM
against it. Two things are still missing:

1. **Only the Cloud Hypervisor provider publishes it.** The libvirt, Proxmox
   and vSphere providers set `capacity: None`, so the scheduler filters none
   of their failure domains on size. A libvirt host keeps taking VMs until
   QEMU fails to allocate or the kernel OOM-kills a guest.
2. **The figures are totals, and there is no disk.** The scheduler computes
   "used" from the classes of the VMs *banlieue* placed there. It cannot see:
   - memory taken by anything else on the host: VMs banlieue did not create,
     management-cluster nodes, page cache that will not give way, a
     hypervisor's own services;
   - disk at all. The Cloud Hypervisor provider measures free space per
     storage class, but only as a display string in `attributes.raw`
     (`storageFreeGiB`). Nothing checks it. A filesystem that fills under
     running guests pauses them; this has happened on a maintainer's host
     shared by guests and other workloads.

Every backend can already answer these questions, mostly through calls the
providers make today:

| Backend | Failure domain | CPUs and memory | Free memory | Storage |
| --- | --- | --- | --- | --- |
| Cloud Hypervisor | the host | `/proc/cpuinfo`, `/proc/meminfo` `MemTotal` less hugepages, slice budget (ADR-0105) | `/proc/meminfo` `MemAvailable` | `statvfs` per storage-class directory (free already measured) |
| libvirt | the host | `virNodeGetInfo` | `virNodeGetMemoryStats` (free + buffers + cached) | `virStoragePoolGetInfo` (capacity, available) per pool a storage class targets |
| Proxmox | one per node | `GET /nodes`: `maxcpu`, `maxmem` (already parsed) | the same response: `maxmem` less `mem` | `GET /nodes/{node}/storage`: `total`, `avail`, `shared` (already parsed) |
| vSphere | one per (datacenter, cluster) | cluster `summary`: `numCpuThreads`, `effectiveMemory`; the target resource pool's `runtime.memory.maxUsage` when smaller | `effectiveMemory` less the sum of the cluster's hosts' `summary.quickStats.overallMemoryUsage`, or the pool's `runtime.memory.unreservedForVm` | `Datastore.summary`: `capacity`, `freeSpace` (`freeSpace` already read by `import.rs`) |

## Decision

1. **`FailureDomainCapacity` gains measured figures and storage.** All new
   fields are optional, like ADR-0107's: a provider publishes what it can
   measure, and the scheduler checks only what is present.

   ```yaml
   capacity:
     cpus: 32                      # ADR-0107: guests may use (total)
     memoryMiB: 61440              # ADR-0107: guests may use (total)
     memoryOverheadMiBPerVM: 512   # ADR-0107
     memoryAvailableMiB: 40960     # new: measured free for a new guest
     storage:                      # new: one entry per available storage class
       - storageClass: fast
         totalGiB: 718
         freeGiB: 512
         shared: false             # true: the same space backs other failure domains
   ```

   - `memoryAvailableMiB`: memory a new guest could take now, measured on the
     backend (table above), never larger than `memoryMiB`.
   - `storage[]`: for each class in `availableStorageClasses`, the backing
     store's size and free space as the writer the provider runs as sees it
     (for a filesystem, `f_bavail`, not `f_bfree`). `shared` is true when the
     same space backs more than one failure domain: a Proxmox `shared`
     storage, or a vSphere datastore mounted by more than one cluster.
   - There is **no measured CPU figure.** Instantaneous CPU use is too noisy
     to place on, and CPU contention only slows guests. "Free CPU" stays what
     ADR-0107 makes it: `cpus x cpuOvercommitRatio`, less the vCPUs placed.

   The Cloud Hypervisor provider's `raw.storageFreeGiB` string goes, replaced
   by `storage[]`. Its other `raw` figures stay for display.

2. **Every provider fills in `capacity`**, from the sources in the table:
   - **Cloud Hypervisor:** adds `memoryAvailableMiB` (capped by the guests'
     slice budget, ADR-0105, less what the slice already uses) and
     `storage[]`.
   - **libvirt:** `cpus` and `memoryMiB` from node info,
     `memoryOverheadMiBPerVM` as a provider constant for a QEMU process,
     `memoryAvailableMiB` from node memory stats, `storage[]` from each
     targeted pool's info (refreshed first, so a pool on a directory reports
     its filesystem's current free space).
   - **Proxmox:** per node, from the fields `GET /nodes` and
     `GET /nodes/{node}/storage` already return. No new API call and no new
     token privilege.
   - **vSphere:** per cluster, from the cluster summary, the target resource
     pool's runtime usage and the datastore summaries of the classes'
     datastores. These are cluster aggregates: DRS picks the host inside the
     cluster, so a VM larger than any single host can still pass the
     scheduler and fail in vSphere. That stays the backend's error to report;
     per-host figures are not published.

3. **Measured figures gate new placements only.** For a VM with no
   placement, or one whose placement failed ADR-0125's checks, the
   scheduler rejects a failure domain when, in addition to ADR-0107's checks:
   - **memory:** `memoryAvailableMiB` less the memory (plus overhead) of
     *in-flight* VMs there is smaller than this VM's memory plus overhead;
   - **storage**, per storage class the VM's disks use: the sum of this VM's
     disk sizes in that class (`diskOverrides` applied), plus in-flight disks
     in that class, is larger than `freeGiB` less the Provider's reserve
     (decision 4).

   An **in-flight** VM is one scheduled to that failure domain (for storage
   marked `shared`, to any failure domain of the same Provider using that
   class) whose infrastructure CR is not yet provisioned: its resources are
   promised but not yet visible in the measurement. Once it is provisioned,
   the measurement carries it and it stops counting, so it is never
   subtracted twice.

   A **placed VM is never moved or flagged by a measured figure.** Its own
   memory and disk are already inside the measurement, so checking it against
   free space would count it against itself. ADR-0125's "capacity still fits"
   keeps meaning ADR-0107's reservation check only.

   Thin disks count at their full size: a guest can fill its disk, and the
   scheduler must not place a disk whose growth would fill the store under
   every guest on it.

4. **A storage reserve per Provider.** `Provider.spec.scheduling` gains
   `storageReservePercent` (0 to 50, default 10): the share of each store's
   `totalGiB` the scheduler leaves free. A full store pauses or corrupts the
   guests already running on it, so the scheduler stops before the end.

5. **Status stays quiet.** A Provider status change requeues every
   VirtualMachine (the controller's Provider watch, ADR-0035), and measured
   figures move all the time. Providers therefore:
   - round `memoryAvailableMiB` down to a whole GiB (in MiB) and `freeGiB`
     down to a whole GiB;
   - patch status only when the rounded values change.

   Measured figures refresh at the Provider reconcile cadence (30 s by
   default). Staleness beyond that is already covered: a Provider whose
   heartbeat is stale is not placed on (ADR-0102).

6. **Spread is unchanged.** ADR-0098's least-loaded-by-count, then
   rendezvous-hash ordering runs over the failure domains that fit. Spreading
   by utilization is roadmap 03's remaining open question, not this ADR.

## Alternatives considered

- **Schedule on measured free figures alone, drop the reservation model.**
  Guest memory is allocated lazily: a VM that has not touched its RAM looks
  free, so the host would be overcommitted the moment its guests warm up.
  The reservation check (ADR-0107) and the measured check answer different
  questions, and a placement needs both.
- **Have the controller publish "allocated" totals on the Provider.** The
  Provider status belongs to the provider's field manager; a second writer
  invites conflicts. The controller can expose them as metrics later.
- **Count disk usage from placed VMs' classes, like memory.** Thin
  provisioning, reflink clones and image backing chains make the declared
  size a poor predictor of what a placed disk occupies. The store's own
  free-space figure is exact; only in-flight disks need adding.
- **Publish raw byte counts.** Exact, but every write on the store changes
  them, and each change requeues every VM (decision 5).

## Consequences

- The scheduler filters every backend on memory, CPU and disk, where each
  provider can measure them. A host's other tenants and a filling store now
  push placements elsewhere, or leave a VM at `NoFailureDomainMatched`
  naming the shortfall, instead of failing in the backend.
- `banlieue-api` changes: two new fields on `FailureDomainCapacity`, a new
  `StorageCapacity` type, one new `ProviderScheduling` field. All optional,
  so the CRDs stay backwards compatible; regenerate `deploy/crds/`.
- The libvirt client gains three read-only procedures (node info, node
  memory stats, storage pool info and refresh); its offline fake must report
  them, and the live libvirtd test checks a real daemon answers them.
- The vSphere client reads cluster, resource pool and host summary
  properties it does not read today. They need only read access to
  inventory the provider already walks.
- A concurrent placement window remains, narrower than ADR-0107's: two VMs
  scheduled in the same pass, before either is recorded as scheduled, can
  both fit against the same free space. The controller places VMs one at a
  time per leader, which keeps the window to one reconcile.
- Two storage classes backed by one filesystem (or one libvirt pool) are
  checked separately and can together overbook it. The host guides should
  say: one storage class per store.
- The capacity figures describe the host to anyone who can read Provider
  status. They are no more than the CH provider already publishes in `raw`,
  but the post-implementation threat-model pass should record them, and a
  compromised provider's ability to attract or repel placements by lying
  about them (already true under ADR-0107).
- Follow-ups, each needing its own decision: spread by utilization rather
  than count (roadmap 03), a storage overcommit ratio for thin disks if the
  full-size rule proves too strict, and controller metrics for allocated
  versus measured capacity.

## Implementation notes (2026-11-11)

Choices the Decision left open, as implemented:

- **Rounding** lives in `banlieue_provider_sdk::capacity` (`available_memory_mib`,
  `bytes_to_gib`), shared by every provider.
- **QEMU overhead.** libvirt and Proxmox publish `memoryOverheadMiBPerVM` =
  256 MiB (`QEMU_MEMORY_OVERHEAD_MIB`), an estimate, recorded as a §8 risk in the
  threat model. vSphere publishes none: vCenter accounts for its own.
- **libvirt free memory** is `free + buffers + cached` from
  `virNodeGetMemoryStats`; the three procedures were verified against a real
  libvirtd (`live_libvirtd.rs::capacity_reads_against_real_libvirtd`).
- **vSphere** caps `memoryMiB` at the cluster's root resource pool's `maxUsage`,
  takes the smaller of (effective memory less the hosts' usage) and the pool's
  `unreservedForVm`, and for a `datastoreCluster` class publishes the member
  with the most free space. A datastore reached by two clusters of the
  Provider is `shared`. Every read is best effort: one that fails leaves its
  figure out.
- **Cloud Hypervisor** headroom is the smaller of the two slices' `MemoryMax`
  less `MemoryCurrent`, read over D-Bus.
