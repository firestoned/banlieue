<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0099: Cloud Hypervisor nodes and a worker-only join in the k0s bootstrap

- **Status:** Accepted
- **Date:** 2026-10-13
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0017](0017-vsphere-bootstrap-backend.md) (the
  bootstrap script and its backends) and
  [ADR-0085](0085-management-cluster-kine-bridge-kairos-debian.md)
  (bridged Kairos-on-Debian nodes); reuses the unit hardening of
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md) without banlieue;
  [ADR-0086](0086-management-cluster-control-plane-vip.md) (the VIP a
  joining worker dials)

## Context

`scripts/bootstrap-k0s-cluster.sh` builds the management cluster without
banlieue, because banlieue needs a cluster before it can create a VM. Its
libvirt backend makes every node a libvirt domain. Two needs fall outside
that:

- **Running a node on Cloud Hypervisor.** CH is the VMM banlieue's
  host-resident provider drives (ADR-0060), and an operator may want the
  management cluster on it too: to compare its host CPU cost with QEMU's on
  the same host, or to drop QEMU from the host altogether. A management node
  cannot be a banlieue `VirtualMachine`: the provider that would start it
  needs the cluster's API, and after a host reboot that API is down until the
  node is up. libvirt solves this with `autostart`, independent of
  Kubernetes; a CH node needs the same property.
- **Growing a cluster that already runs.** `config` regenerates the whole
  k0sctl configuration from the script's settings and `apply` pushes it to
  every node, controllers included. A long-lived cluster drifts from that
  file: a VIP added after the fact (ADR-0086), API server flags added by
  hand, components disabled later. Re-running `config` and `apply` to add
  one worker rewrites `/etc/k0s/k0s.yaml` on every controller and restarts
  them with the older settings.

## Decision

### 1. The VMM is chosen per node, within the libvirt backend

`NODE_VMMS` lists one VMM per node, beside `NODE_ROLES` and `VM_MACS`:
`libvirt` (the default for every node) or `cloud-hypervisor`. One cluster
can mix them. `BACKEND=libvirt` is the "local KVM host" backend: it already
owns the image fetch, the seed, the bridge and the node table, and none of
that depends on which VMM runs the guest.

A `cloud-hypervisor` node must be created on the host itself (it is not
reached through `LIBVIRT_URI`), on a bridge (`CH_BRIDGE`, defaulting to
`LIBVIRT_BRIDGE`), with an explicit firmware (`CH_FIRMWARE`, the
`CLOUDHV.fd` the operator installed). Each is refused with a message rather
than guessed.

### 2. A Cloud Hypervisor node is three systemd units, not a banlieue object

For node `<name>` the script writes, as root:

| Unit | Runs as | Does |
| --- | --- | --- |
| `k0s-vm-<name>-net.service` | root, oneshot | creates tap `kv-<name>` on the bridge, **owned by the VM user**, and deletes it on stop |
| `k0s-vm-<name>-swtpm.service` | VM user | the node's vTPM, matching the `tpm-crb` libvirt gives a UEFI domain (`CH_TPM=true`) |
| `k0s-vm-<name>.service` | VM user | `cloud-hypervisor` with the node's disk, seed, tap, vTPM and firmware |

The VM user (`CH_VM_USER`, default `k0s-vm`) is a system account with no
login and no capabilities, in the `kvm` group. The tap is created for it,
so the VMM opens it without `CAP_NET_ADMIN`. The VMM unit carries the
sandboxing of banlieue's `banlieue-ch@.service` (`NoNewPrivileges`,
`ProtectSystem=strict`, `DevicePolicy=closed` with only `/dev/kvm` and
`/dev/net/tun`), and may write only the node's own directory and runtime
directory. `WantedBy=multi-user.target` starts it at boot, after the bridge
exists: the same guarantee as `virsh autostart`.

Install follows the libvirt Kairos path: the installer ISO is attached
through a runtime drop-in under `/run/systemd/system`, Kairos installs and
powers the guest off, CH exits, the script removes the drop-in and the ISO
copy, and enables the unit. A `cloud` image boots directly with no install
phase. Stopping the unit presses the ACPI power button and waits for the
guest, so a host shutdown is a guest shutdown, not a power cut.

### 3. The node's address comes from the host's neighbour table

There is no guest agent channel. Every CH node has a fixed MAC (from
`VM_MACS`, or derived from the cluster and node name), and its seed adds a
boot-time step that pings the host's address on the bridge, so the host
always holds a resolved neighbour entry for the guest. The script reads the
address from that entry, the way banlieue's provider does (ADR-0062
Decision 5). A bridge with no host address is refused for CH nodes.

### 4. `join`: add workers to a running cluster without touching controllers

The new `join` step joins the nodes named in `JOIN_NODES` as k0s workers,
natively: it stages the pinned, checksum-verified k0s binary on the node,
asks a running controller for a worker token valid for 15 minutes, carries
it over SSH straight into a `0600` file on the node, and runs `k0s install
worker`. It writes nothing to any controller and never reads or rewrites
the k0sctl configuration. A node whose `k0sworker` is already active is
skipped, so the step is idempotent. Controllers are refused: a controller's
configuration (storage, VIP, SANs) is exactly what the script cannot know
about a drifted cluster.

## Consequences

**Positive**

- A management node can run on the same VMM as banlieue's workload VMs, and
  QEMU and CH nodes can be compared side by side in one cluster.
- A CH node survives a host reboot without the cluster, like a libvirt
  domain.
- Workers can be added to a long-lived cluster safely, whatever has changed
  on its controllers since bootstrap.

**Negative / accepted costs**

- CH nodes are invisible to `virsh` and to Cockpit's Virtual Machines page,
  which drives libvirt's QEMU driver only. They are managed with
  `systemctl`, and their serial console is a socket under `/run`.
- Creating a CH node needs root on the host, through `sudo`, to write units,
  create the VM user and own its files. The libvirt path needs only
  membership of the `libvirt` group.
- The VMM is whatever `CH_BINARY` names. A vTPM needs a VMM carrying the
  fix tracked in cloud-hypervisor#9053, as banlieue's provider does.
- `join` trusts the controller it asks for a token, and the operator's SSH
  key on both ends; the token is a cluster join credential for its 15
  minutes and is left on the node in a root-only file, as k0sctl and the
  vSphere backend leave theirs.
- A derived MAC is stable but not reserved: the LAN's DHCP server must
  still reserve it, as for libvirt nodes (ADR-0085).
