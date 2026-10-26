<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0107: Capacity-aware scheduling: memory fits strictly, CPU fits a ratio

- **Status:** Accepted
- **Date:** 2026-10-24
- **Deciders:** Erick Bourgeois
- **Related:** Closes [ADR-0105](0105-cloud-hypervisor-guests-slice.md)'s
  follow-up (the scheduler should see the guests' slice budget); extends
  [ADR-0098](0098-spread-placement-across-failure-domains.md) (placement) and
  the `Provider`'s per-failure-domain attributes
  ([ADR-0030](0030-per-zone-capability-targets.md)); assumes
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)'s one host
  is one Provider

## Context

The scheduler filters failure domains by selectors, image readiness,
storage and network classes, features, firmware and vTPM, then spreads by VM
count (ADR-0098). It never compares a VM's size with what a failure domain can
hold. A host keeps taking VMs until something below banlieue fails:

- **Memory.** Each Cloud Hypervisor guest's VMM runs with `MemoryMax` = guest
  memory + 512 MiB (ADR-0063), and ADR-0105 caps all guests together with the
  guests' slice `MemoryMax`. Past either, the kernel OOM-kills a VMM: a guest
  crashes instead of the VM being placed elsewhere. ADR-0105 recorded this as
  a follow-up, and the CH host guide has to warn about it.
- **CPU.** Over-placing vCPUs only slows guests; hosts are routinely
  overcommitted on purpose. A strict CPU check would waste most of a host.

The Cloud Hypervisor provider already publishes `cpus` and `memoryMiB` as
free-form strings in `failureDomains[].attributes.raw`, read from `/proc`,
ignoring the slice budget. No other backend publishes capacity.

## Decision

1. **A typed capacity on the failure domain.**
   `FailureDomainAttributes.capacity` (`banlieue-api`), all fields optional:
   - `cpus`: logical CPUs guests may use here;
   - `memoryMiB`: memory guests may use here, in MiB;
   - `memoryOverheadMiBPerVM`: memory each VM costs beyond its class's
     `memoryMiB` (the VMM itself), in MiB.

   A backend fills in what it knows. The `raw` strings stay, for display.
2. **What the Cloud Hypervisor provider publishes.**
   - `cpus` = the host's logical CPUs, or the number of CPUs in the guests'
     slice `AllowedCPUs` (ADR-0105) when that is set and smaller.
   - `memoryMiB` = the host's `MemTotal` less reserved hugepages, or the
     smallest `MemoryMax` among `banlieue-guests.slice` and `banlieue.slice`
     when set and smaller. The slice values are read from systemd over D-Bus
     (the unit properties, which include the operator's `set-property`
     drop-ins and are available while the slice is inactive), never parsed
     from unit files.
   - `memoryOverheadMiBPerVM` = 512, the margin its own VMM `MemoryMax` adds.
3. **The scheduler fits a VM before spreading.** For each candidate
   `(provider, failure domain)` that publishes capacity:
   - **used** = the sum over VMs already scheduled there (`status.scheduled`
     names it, the VM is not the one being placed), using each VM's class:
     vCPUs, and memory plus `memoryOverheadMiBPerVM`. A VM whose class cannot
     be found counts at zero and is logged, rather than blocking placement.
   - **Memory is strict:** reject when `used.memory + class.memoryMiB +
     overhead > capacity.memoryMiB`.
   - **CPU fits a ratio:** reject when `used.cpus + class.cpus >
     capacity.cpus × ratio`. The ratio is `Provider.spec.scheduling.cpuOvercommitRatio`
     (1 to 64, default 4). `1` means no overcommit.
   - A failure domain with no `capacity`, or a capacity missing that figure,
     is **not** filtered on it: nothing that schedules today stops scheduling.
     Rejections join the existing bounded reject reasons, so a VM that fits
     nowhere reports `NoFailureDomainMatched` naming the shortfall.
4. **Spread is unchanged.** ADR-0098's least-loaded, then rendezvous-hash
   choice runs over the failure domains that fit.

## Consequences

- On a Cloud Hypervisor host, a VM that would push guests past the declared
  memory budget is placed elsewhere or waits, instead of a running guest being
  OOM-killed. The CH host guide's "the scheduler does not read slice limits"
  warning goes.
- Capacity is accounted per Provider. Two Providers pointing at one host would
  each count only their own VMs; ADR-0060's one host, one Provider is now load
  bearing, and the threat model records the assumption.
- Memory used by anything that is not a banlieue VM (the host itself,
  management-cluster nodes in `banlieue-nodes.slice`) is not deducted unless a
  slice budget excludes it. On a shared host, declare a guests' budget
  (ADR-0105) so capacity means what guests may have.
- The scheduler now needs every VM's class, so a scheduling pass lists
  `VMClass`es once (they are cluster-scoped and few).
- vSphere, libvirt and Proxmox publish no capacity yet, so they keep today's
  behaviour; each can opt in by filling `capacity`.
- This is a placement-time check over status a provider published, not a
  reservation: two VMs scheduled concurrently can both fit against the same
  free memory. The controller reconciles VMs one at a time per leader, which
  narrows that window but does not close it.
