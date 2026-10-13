<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0099: Cloud Hypervisor nodes and a node join in the k0s bootstrap

- **Status:** Accepted
- **Date:** 2026-10-13
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0017](0017-vsphere-bootstrap-backend.md) (the
  bootstrap script and its backends) and
  [ADR-0085](0085-management-cluster-kine-bridge-kairos-debian.md)
  (bridged Kairos-on-Debian nodes); reuses the unit hardening of
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md) without banlieue;
  [ADR-0086](0086-management-cluster-control-plane-vip.md) (the VIP a
  joining node dials, which a joining controller also serves)

## Context

`scripts/bootstrap-k0s-cluster.sh` builds the management cluster without
banlieue, because banlieue needs a cluster before it can create a VM. Its
libvirt backend makes every node a libvirt domain. Two needs fall outside
that:

- **Running a node on Cloud Hypervisor.** CH is the VMM banlieue's
  host-resident provider drives (ADR-0060), and an operator may want the
  management cluster on it too: to compare its host CPU cost with QEMU's on
  the same host, or to drop QEMU from the host altogether, by replacing
  the cluster's libvirt nodes, controllers included, one at a time. A management node
  cannot be a banlieue `VirtualMachine`: the provider that would start it
  needs the cluster's API, and after a host reboot that API is down until the
  node is up. libvirt solves this with `autostart`, independent of
  Kubernetes; a CH node needs the same property.
- **Growing a cluster that already runs**, with workers or with
  controllers: a controller is added before an old one is retired, so the
  control plane never drops below its size. `config` regenerates the whole
  k0sctl configuration from the script's settings and `apply` pushes it to
  every node, controllers included. A long-lived cluster drifts from that
  file: a VIP added after the fact (ADR-0086), API server flags added by
  hand, components disabled later. Re-running `config` and `apply` to add
  one worker rewrites `/etc/k0s/k0s.yaml` on every controller and restarts
  them with the older settings. A new controller generated from the same
  file would be just as wrong: it must match the controllers already
  running, which agree with each other (one datastore, one VRRP group, one
  API server configuration) and not with the file.

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

### 4. `join`: add nodes to a running cluster without rewriting its controllers

The `join` step joins each node named in `JOIN_NODES` in its `NODE_ROLES`
role, natively: it stages the pinned, checksum-verified k0s binary on the
node, asks a running controller (`JOIN_CONTROLLER`, else the first in the
node table) for a token of that role valid for 15 minutes, carries it over
SSH straight into a `0600` file on the node, and runs `k0s install`. It
never reads or rewrites the k0sctl configuration, and on the existing
controllers it only reads.

- **A worker** runs `k0s install worker` with its token.
- **A controller** (`controller` or `controller+worker`) is a copy of the
  running controller it asked, not of the script's settings. `join` copies
  that controller's `/etc/k0s` to the node, through one pipe between two
  SSH sessions, minus join tokens and the containerd configuration k0s
  generates per worker: `k0s.yaml` with its datastore, VIP and API server
  settings, and the files it names, such as a datastore CA bundle or an
  authentication configuration. It reuses that controller's install flags
  (from its `ExecStart`), replacing only the worker flags, which follow the
  node's role and `NO_TAINTS` as at bootstrap. The token then brings the
  cluster CA and service-account keys over k0s's join API. `join` waits for
  the node's `ControlNode`, then, with a kubelet, for the node to be
  `Ready`. A `--single` controller is refused: it cannot have peers.

A node already running the role's service is skipped, so the step is
idempotent. A node running k0s in a different role is refused with the
reset command to run first, since converting it destroys its k0s state. A
controller is also refused while a Node of its name exists: kubelet labels
a Node only when it registers it, so a reset worker's Node would carry no
control-plane role.

What `join` cannot do is admit the node to systems outside the cluster:
with kine, the datastore must already accept the new controller's address
(for PostgreSQL, a `pg_hba.conf` line), and the LAN's DHCP server must
reserve its MAC.

Retiring the old node of a swap stays a manual, documented procedure
(drain, `k0s reset`, delete the Node, remove the VM): it is rare, it is
destructive, and the right moment for each step is the operator's call.
The script only has to forget the node. Nodes are named by their position
in `NODE_ROLES`, so a retired node keeps its entry and is listed in
`RETIRED_NODES`, which `vms` does not recreate and which no step waits for.

## Consequences

**Positive**

- A management node can run on the same VMM as banlieue's workload VMs, and
  QEMU and CH nodes can be compared side by side in one cluster.
- A CH node survives a host reboot without the cluster, like a libvirt
  domain.
- Workers and controllers can be added to a long-lived cluster safely,
  whatever has changed on its controllers since bootstrap, so a cluster's
  libvirt nodes can be replaced by CH ones one at a time without
  rebuilding it.

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
  key on both ends. A token is a join credential for its 15 minutes and is
  left on the node in a root-only file, as k0sctl and the vSphere backend
  leave theirs. A **controller** token hands its holder the cluster CA key,
  and the copied `/etc/k0s` carries the datastore password, so a controller
  join is a cluster-admin action and its SSH paths must be trusted.
- A joining controller copies all of `/etc/k0s`, so stray files there
  (backups an operator left beside `k0s.yaml`) are copied too.
- A derived MAC is stable but not reserved: the LAN's DHCP server must
  still reserve it, as for libvirt nodes (ADR-0085).
