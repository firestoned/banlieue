<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0138: A host may run both VMMs, each as its own Provider with its own budget

- **Status:** Accepted
- **Date:** 2026-11-19
- **Proposed:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Amends:** roadmap 20, "One VMM per host, in v1" (that constraint is withdrawn by this ADR)
- **Related:** [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (one host is one Provider), [ADR-0105](0105-cloud-hypervisor-guests-slice.md)
  and [ADR-0106](0106-vm-systemd-slices-and-overhead-sampler.md) (the guests'
  budgeted slice and the `banlieue.slice` hierarchy),
  [ADR-0107](0107-capacity-aware-scheduling.md) and
  [ADR-0128](0128-every-provider-publishes-measured-capacity.md) (capacity is
  the slice budget), [ADR-0139](0139-hosts-without-systemd-userdb-or-cgroup-v2.md)
  (hosts on cgroup v1), [ADR-0137](0137-firecracker-provider-class.md) (the
  `firecracker` class)

## Context

Roadmap 20 chose "a host is either a Cloud Hypervisor host or a Firecracker
host" for v1, on one argument: two Providers on one host would both report
the host's whole CPU and memory to the capacity-aware scheduler, which would
then double-count it.

That argument was written before ADR-0107 and ADR-0128 made a Provider report
its **slice budget** rather than the host's physical totals. With that in
place the double count disappears if each VMM has its own slice and each
Provider reports its own slice. The restriction is then the cost, not the
protection: bare-metal KVM hosts are scarce (96 hardware threads and 500 GB on
the first one measured), the two VMMs serve different workloads
(Cloud Hypervisor: general-purpose guests, vTPM, sealing; Firecracker:
ephemeral sandboxes), and the first real Firecracker host is also the
Cloud Hypervisor host, split half and half by the owner.

## Decision

### 1. One host, up to two Providers, one per VMM

A host may be both a `cloud-hypervisor` host and a `firecracker` host. Each is
a separate `Provider` (ADR-0060 is unchanged: one Provider is one VMM on one
host, fenced by its own Lease), with its own ServiceAccount, credentials,
polkit rule, unit templates and uid range. Nothing is shared at runtime except
the kernel and the host's own agents.

### 2. Each VMM has its own sibling slice under `banlieue.slice`

| VMM | Slice | Defined by |
| --- | --- | --- |
| Cloud Hypervisor | `banlieue-guests.slice` (unchanged) | ADR-0105 |
| Firecracker | `banlieue-firecracker.slice` | ADR-0137, same flags |

Both are children of `banlieue.slice` (ADR-0106). The Firecracker slice is
**not** named `banlieue-guests-firecracker.slice`: systemd reads a dash in a
slice name as a parent and child, so that name would nest the Firecracker
guests inside the Cloud Hypervisor budget and make the two budgets one.

### 3. The split is declared, disjoint and checked

Explicit over implicit: nothing infers how a host is divided. Each `install`
takes its own `--guest-cpus`, `--guest-memory-max` and the other ADR-0105
flags, for its own slice. `banlieue host <vmm> preflight`, which every
`install` runs first, then reads the **other** VMM's slice if one exists and
refuses when the two budgets overlap or over-commit the host:

- `AllowedCPUs` lists intersect; or
- `MemoryMax` values sum to more than the host's `MemTotal` less hugepages; or
- one slice is unbudgeted on a host where both VMMs are present (an unbounded
  slice makes the other's ceiling meaningless).

A check is a refusal with the offending line quoted, never a silent clamp.
It guards a footgun and is not a security boundary: the host owner can edit
unit files; a provider cannot (the polkit rule does not allow `set-property`
on a slice, ADR-0105).

### 4. Capacity is each Provider's own slice

Each Provider publishes capacity from its own slice (ADR-0128 table row for
its VMM), so the scheduler sees two hosts-worth of half a machine, never one
machine twice. The Firecracker Provider reads `banlieue-firecracker.slice`
exactly as the Cloud Hypervisor Provider reads `banlieue-guests.slice`
(D-Bus, `Get`, drop-ins included, ADR-0107 Decision 2). The failure domain
fields are the same on both and both are reported, so spread (ADR-0098) still
treats them as one domain: a pool spread across the two Providers of one host
gets no extra fault tolerance and is not counted as having any.

### 5. Network and storage do not overlap

Each VMM uses its own private bridge and its own subnet; neither bridge has an
uplink, NAT or DHCP on the host (ADR-0067 Decision 7). Storage is separate
directories, one per VMM, on the same volume. The two Providers never share a
tap name: taps are named from the guest uid, and each VMM has its own uid
range (ADR-0063).

### 6. Withdrawn: "refuses a host that already has the other VMM"

Roadmap 20's preflight refusal is replaced by the checks in Decision 3.

## Consequences

- The host's scarce capacity can be split and used by both VMMs, and the
  scheduler sees the split.
- ADR-0105 pins CPUs with `AllowedCPUs`, which systemd honours only under
  **cgroup v2**. On a cgroup v1 host (the first mixed host is RHEL 8) the
  split uses `--guest-cpu-quota` instead, a share of time and not exclusive
  cores, with SMT siblings shared ([ADR-0139](0139-hosts-without-systemd-userdb-or-cgroup-v2.md)
  adds the flag and refuses the pinning flags where they would not be
  enforced). Preflight compares quotas as it compares cpusets. **This
  decision stands either way; the isolation quality differs.**
- Two Providers on one host is two things to monitor and upgrade. The
  installer treats them independently; upgrading one never restarts the other.
- A noisy Firecracker pool can still hurt the Cloud Hypervisor half through
  shared hardware (memory bandwidth, last-level cache, SMT siblings, disk,
  the kernel). Disjoint cpusets reduce this and do not remove it. For
  untrusted sandboxes the two halves are the same trust domain (banlieue's),
  so this is a performance risk, not an isolation break; a multi-tenant split
  would need a stronger answer (whole sockets).
- Threat model: new DoS boundary pair (two budgets on one host) and one new
  root-owned file (the Firecracker slice). Pass due when implemented.
- Accepted, not implemented. CALM follows now; the threat-model pass follows the implementation.
