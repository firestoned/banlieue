<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0105 — Cloud Hypervisor guests share one budgeted systemd slice

- **Status:** Accepted
- **Date:** 2026-10-22
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0063](0063-cloud-hypervisor-host-supervision.md) (guests are
  systemd template units), [ADR-0067](0067-banlieue-host-install-subcommand.md)
  and [ADR-0084](0084-host-install-release-sources-and-versions.md) (the
  installer, its flags and placeholders), [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (one host is one Provider)

## Context

ADR-0060 to ADR-0067 assume the KVM host is *dedicated*: it runs banlieue's
guests and little else. Each guest already has its own cgroup with
`MemoryMax` and `TasksMax` (ADR-0063), but the guests are siblings of
everything else under `system.slice`, with **no CPU limit at all** and no
ceiling on their *combined* memory. A host that is also a Kubernetes worker,
a backup agent or an endpoint-security agent (the first real host onboarded
is all three) can be starved by a burst of guests, and can starve them.

Per-guest limits cannot express "all guests together get at most N CPUs and M
GiB". systemd expresses that with a parent **slice**.

## Decision

### 1. One slice, `banlieue-guests.slice`

A root-owned unit file, `deploy/provider-cloud-hypervisor/host/banlieue-guests.slice`,
installed to `/etc/systemd/system/` by the `provider` stage. The per-guest
templates `banlieue-ch@.service` and `banlieue-swtpm@.service` gain
`Slice=banlieue-guests.slice`, so every running VMM and vTPM is inside it.
The import and swtpm-setup units are short-lived, run as the provider's own
user, and stay where they are.

The slice's limits are the host's *budget for guests*. They bound the sum;
each guest's own `MemoryMax` still bounds the guest.

### 2. The budget is declared, never inferred

The non-negotiable "explicit over implicit" applies: banlieue does not guess
how much of a shared host it may take. `install` takes four optional flags,
each with a `BANLIEUE_HOST_*` variable:

| Flag | Slice setting | Form |
| --- | --- | --- |
| `--guest-cpus` | `AllowedCPUs=` | a cpuset list, `0-15,32-47` |
| `--guest-cpu-weight` | `CPUWeight=` | `1`–`10000` |
| `--guest-memory-max` | `MemoryMax=` | bytes with an optional `K`/`M`/`G`/`T` suffix, or `N%` |
| `--guest-tasks-max` | `TasksMax=` | a positive integer |

An unset flag emits no line, so the slice exists but that dimension is
unbounded; `install` says so, because an unbudgeted slice on a shared host is
a choice an operator should see being made. Every value is validated against
a strict grammar before it is rendered into a unit, so a flag cannot inject
a directive.

### 3. Installed and owned like the other host files

The slice is rendered from the file in the repository (a `@GUEST_SLICE_LIMITS@`
placeholder carries the lines), written by temp-and-rename, root-owned,
idempotent. Values are checked when the flags are resolved, before anything is written. The polkit rule is unchanged: the provider may start, stop
and reset instances, and **cannot** alter the slice, so a compromised provider
cannot lift the budget.

### 4. What this does not do

- It does not reserve capacity for the *other* workloads: `AllowedCPUs`
  confines guests to a set, but does not keep others out of it. Pairing it
  with the other workloads' own cpuset is the host owner's call.
- It does not change what the provider reports as capacity. `Provider.status`
  still shows the host's physical CPU and memory; making it report the slice
  budget is a follow-up, recorded below.
- It creates no bridge and moves no other workload (ADR-0067 Decision 7).

## Consequences

- A shared host can run guests with a hard, declared ceiling; a runaway
  guest or a pool-refill storm cannot take the host's CPU or memory.
- The unit templates are a host contract: existing hosts need a re-run of
  `install` to gain the slice. Until then guests run in `system.slice` as
  before. Nothing else changes for them.
- Follow-up: the scheduler's view of a host's headroom should use the slice
  budget, not the physical totals. Until it does, a pool can ask for more than
  the slice allows, and guests then fail against `MemoryMax` rather than being
  placed elsewhere.
- Threat model: a new resource boundary (TB-9, denial of service) and a new
  root-owned file; pass recorded in the same change.
