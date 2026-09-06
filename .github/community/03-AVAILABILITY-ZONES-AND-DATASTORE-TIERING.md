# banlieue — Availability Zones, Datastore & Compute Tiering

> **Status:** Design principle to lock (revisits and supersedes any notion of
> per-datastore tiering). Roadmap item: examine, decide, then enforce in the
> capability model, the scheduler, and the failure-domain mapping.
>
> Read alongside `01-DECISIONS.md` (this should land there as a D-NNN once
> agreed) and the provider phase docs (`11`–`13`).

## The question

Why would we tier *individual* datastores at all? The capability model lets an
admin map abstract storage classes (`gold`, `silver`, …) onto concrete
datastores per Provider, which invites a reading where each datastore is its
own performance tier and a VM picks "the gold datastore."

That is **not** the intended model, and this doc records why.

## The principle (intended design)

1. **Uniform tiering, not per-datastore tiering.** All datastores in a given
   scope are the *same* tier; all compute in that scope is the *same* tier. A
   "tier" is a property of the *fleet/zone*, not of an individual datastore or
   host. We do not place a VM on "the faster datastore" — we place it on **a**
   local datastore of its zone, and every datastore in that zone is equivalent.

2. **Spread for availability.** Capacity and instances are spread *evenly*
   across the datastores and compute units. The more independent units we
   spread across, the higher the aggregate availability: a single
   datastore/host failure takes out a bounded fraction of instances, never a
   disproportionate share. Even spreading is the availability mechanism;
   tiering individual units would concentrate load and defeat it.

3. **Local datastores only — always.** A VM's storage MUST be on a datastore
   **local** to the compute it runs on (host-local / cluster-local, per the
   backend's locality model). This is a hard rule, primarily for performance
   (no fabric hop in the data path) but also for isolation (below).

4. **No cross-datastore access — ever.** Neither a VM's **second/data disk**
   nor its **compute** may reach a datastore outside its own local zone.
   Cross-datastore access:
   - couples two otherwise-independent failure units (a remote datastore
     outage now takes down a VM whose compute is "healthy"),
   - adds a network/fabric dependency to the storage data path, and
   - **violates the Availability Zone boundary** — the whole point of an AZ is
     that it fails independently.

## What "Availability Zone" means in banlieue

An **Availability Zone = a unit of local compute + its local datastore(s),
which fail together and nothing else.** In CRD terms this is a
`Provider.status.failureDomains[]` entry: today a `(datacenter, cluster)` pair,
which must be refined so that **datastore locality is part of the failure-domain
identity**, not a free-floating capability.

Consequences:

- A failure domain advertises the storage/network classes that are **local**
  to it. The same abstract class (`gold`) resolves to a *different* local
  datastore in each domain — same tier, different physical unit.
- The scheduler's job is to **spread** `VirtualMachine`s across failure domains
  (it already supports `failureDomainSelector` + anti-affinity `topologyKey`),
  and within a chosen domain, bind **all** of a VM's disks to that domain's
  local datastore(s).

## Mapping to the current model

| Concept | Today | Under this principle |
|---|---|---|
| `Provider.spec.capabilities.storageClasses[]` | name → `{datastore: X}` | name → tier intent; the *concrete* datastore is resolved **per failure domain** (local), not globally |
| `FailureDomain.attributes` | `availableStorageClasses`, `raw{datacenter,cluster}` | also pins the **local datastore(s)**; a class is "available" only if a *local* datastore satisfies it |
| `VMClass.hardware.disks[].storageClass` | abstract class | must resolve to a **single local datastore** for *all* disks of the VM (root + data on the same local zone) |
| Scheduler | filters by class availability + anti-affinity | additionally guarantees **co-location** of every disk on the placement's local datastore; rejects any plan needing a remote datastore |

## Rules to enforce (acceptance criteria)

- A `VirtualMachine` is schedulable onto a failure domain **only if every disk's
  storage class is satisfiable by a datastore local to that domain.**
- All disks of a VM resolve to local datastore(s) of the **same** placement —
  never split across zones.
- The provider MUST refuse (status error, not silent) any request that would
  attach a disk on, or run compute against, a non-local datastore.
- "Tier" is expressed once per fleet/zone class, **uniformly**; the model must
  make per-datastore tiering *unrepresentable* or at least non-idiomatic.

## Anti-patterns (explicitly disallowed)

- A "gold datastore" vs "silver datastore" within the same zone.
- A data disk on datastore B while compute runs in zone A.
- Shared/stretched datastores presented as one capability across zones (a
  stretched datastore is a single failure unit masquerading as many — it
  violates AZ independence even if the backend allows it).
- Scheduling that prefers a "better" datastore over even spread.

## Open questions

- **Locality model per backend.** vSphere: host-local vs cluster-shared
  datastores, datastore clusters (SDRS), vSAN (which is cluster-local by
  design). Proxmox: local-LVM/ZFS vs shared (Ceph/NFS). libvirt: pool locality.
  Each provider needs a concrete definition of "local" and how to detect it.
- **Where does locality live in the CRD?** Extend `FailureDomainAttributes`
  with explicit local-datastore identity vs deriving it. Likely the former.
- **vSAN / Ceph nuance.** These are cluster-local-but-replicated. Are they one
  AZ (the cluster) — yes — and is intra-cluster replication acceptable (it is,
  it's *within* the zone)? Confirm the boundary is the cluster, not the host.
- **Do we ever allow shared storage?** Default: no cross-zone shared storage in
  the data path. If a backend only offers shared storage, the failure domain
  granularity must collapse to match (fewer, larger AZs) rather than pretend.
- **Capacity-aware even spread.** Spreading "evenly" needs a notion of
  per-domain capacity/utilization the scheduler can read.

## Roadmap actions

1. Lock the principle as a decision in `01-DECISIONS.md` (D-NNN: "Availability
   Zones = local compute + local datastore; uniform tiering; no cross-datastore
   access").
2. Per-provider: define "local datastore" and surface local datastore identity
   in `status.failureDomains[].attributes` (Phase 1B vSphere first).
3. Scheduler: enforce all-disks-local co-location and reject remote-datastore
   plans; keep even-spread as the placement objective (not best-tier).
4. Capability model review: ensure `storageClasses` express *tier intent*
   resolved locally per domain, and make per-datastore tiering non-idiomatic.
5. Docs: fold the AZ definition into `docs/src/concepts/` (failure domains) so
   users understand storage classes are uniform-tier + zone-local.

## Why this matters (one line)

Availability comes from **many small, independent, evenly-loaded zones** — and
a zone is only independent if its compute and *all* its storage are local to it.
Per-datastore tiering and cross-datastore access both trade that independence
away for a local optimization that isn't worth it.
