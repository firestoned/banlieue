<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0106: Every banlieue VM in `banlieue.slice`; `banlieue host cloud-hypervisor overhead` measures them

- **Status:** Accepted
- **Date:** 2026-10-23
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0105](0105-cloud-hypervisor-guests-slice.md)
  (the guests' budgeted slice); amends
  [ADR-0099](0099-cloud-hypervisor-nodes-and-node-join-in-k0s-bootstrap.md)
  (the k0s bootstrap's CH nodes); extends
  [ADR-0067](0067-banlieue-host-install-subcommand.md) (`banlieue host
  cloud-hypervisor` and its read-only verbs)

## Context

ADR-0105 put every banlieue guest's VMM and vTPM in `banlieue-guests.slice`,
budgeted by `install` flags. Two kinds of VM on the same hosts are still
outside any shared cgroup, and nothing reports what VMs cost a host:

1. **Management-cluster nodes.** `scripts/bootstrap-k0s-cluster.sh` runs each
   node as `k0s-vm-<name>.service` plus `k0s-vm-<name>-swtpm.service`
   (ADR-0099), in `system.slice` beside sshd and the database. On a host that
   carries both banlieue guests and management nodes, there is no one cgroup
   for "all the VMs here", and no budget can cover both.
2. **No measurement.** On 2026-10-13 the maintainer measured CH against
   libvirt on a lab host to decide whether to move the management cluster to
   CH. It took a one-off script that walked `system.slice` and
   `machine.slice` by unit name and read every thread's `/proc` counters. Its
   findings (the CH VMM process costs about a tenth of QEMU's; most host-side
   cost is KVM halt polling, not the VMM) were the evidence behind ADR-0099's
   default, and the script lived only in a scratch directory.

systemd names slices hierarchically: `banlieue-guests.slice` is already a
child of `banlieue.slice`, which systemd creates implicitly from any child
(verified on systemd 257: no unit file is needed for the parent, and
`systemctl set-property` on the implicit parent applies and persists).

## Decision

1. **`banlieue.slice` is the host's VM grouping.** Its children are
   ADR-0105's `banlieue-guests.slice` and a new `banlieue-nodes.slice`.
   `banlieue.slice` itself has no unit file and no writer.
2. **Management nodes run in `banlieue-nodes.slice`.**
   `scripts/bootstrap-k0s-cluster.sh` writes the slice unit and sets
   `Slice=banlieue-nodes.slice` on each `k0s-vm-<name>.service` and
   `k0s-vm-<name>-swtpm.service` (the `-net` oneshot is not a VM). A node built
   before this gets a `Slice=` drop-in (`50-slice.conf`) the next time `vms`
   runs, rather than having its units rewritten: they carry the sizing they
   were built with, and a running node is not restarted for it.
3. **Budgets.** Guests keep ADR-0105's declared budget (`--guest-*` flags
   rendered into `banlieue-guests.slice`). The nodes' slice and the parent
   have no flags: the script has no budget to declare, and a budget across
   both kinds of VM belongs to whoever owns the host. The operator sets one
   with `systemctl set-property banlieue.slice …` (or on
   `banlieue-nodes.slice`), a drop-in under `/etc/systemd/system.control/` that
   neither `install` nor the script writes or removes. The provider gets no
   rights on any slice: the polkit rule is unchanged, and its tests now refuse
   `set-property` and `stop` on all three.
4. **`banlieue host cloud-hypervisor overhead [--seconds N]`** is a new
   read-only verb (like `status` and `preflight`, it writes nothing, and it
   needs no root: `/proc/*/task/*/stat` and cgroup files are world-readable).
   It samples every VM it can find twice, `N` seconds apart (default 60), and
   prints per VM, as a percentage of one host core: **guest** (vCPU threads'
   `guest_time`), **vCPU host** (the rest of those threads' time: KVM exits,
   halt polling, in-kernel emulation), **VMM** (the VMM's other threads),
   **swtpm**, their total, and overhead as `(vCPU host + VMM + swtpm) / guest`.
   It finds VMs by cgroup name wherever they run: `banlieue-ch@` and
   `banlieue-swtpm@` units (one VM per guest uid, both units counted),
   `k0s-vm-<name>` units, and libvirt's `machine-qemu…<name>.scope`, so a host
   part-way through a move from libvirt can compare both VMMs. It also
   reports `banlieue.slice`'s own `cpu.stat` over the window.

## Consequences

- `systemd-cgtop banlieue.slice` gives every VM on the host in one tree, and
  one `set-property` can bound guests and management nodes together.
- Two budgets can now apply to a guest: ADR-0105's on `banlieue-guests.slice`
  and the operator's on `banlieue.slice`. The tighter one wins, as cgroups
  always do; the CH host guide says how to size them so a memory cap never
  makes the kernel kill a guest's VMM.
- A node started before the drop-in stays in `system.slice` until its unit
  restarts; `overhead` still finds it there.
- `overhead` tells vCPU threads apart by name (`vcpu<N>` for Cloud
  Hypervisor, `CPU <N>/KVM` for QEMU). A VMM that renames its threads would
  move time between columns; the unit tests pin both conventions, and the
  slice total is kernel-accounted and cannot be renamed. The numbers are a
  diagnostic, never an input to scheduling.
- The numbers measure CPU only. Memory overhead is visible in the slice's
  `memory.current` but is not split per VM.
