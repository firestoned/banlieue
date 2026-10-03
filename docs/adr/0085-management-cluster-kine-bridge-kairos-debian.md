<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0085: Management cluster on kine/PostgreSQL, bridged nodes, Kairos on Debian

- **Status:** Accepted
- **Date:** 2026-10-02
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0017](0017-vsphere-bootstrap-backend.md) (the
  bootstrap script and its backends), [ADR-0013](0013-banlieue-bootstrap-cli.md)
  (`banlieue bootstrap` installs onto the cluster this builds),
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (host-resident providers dial the cluster's API server)

## Context

`scripts/bootstrap-k0s-cluster.sh` builds the management cluster: the k0s
cluster banlieue runs on, built without banlieue, because banlieue needs a
cluster before it can create a VM. Its libvirt backend made three
assumptions that a long-lived management cluster does not want:

- **etcd on the controllers.** Every controller carries an etcd member, so
  cluster state lives inside the VMs and is backed up by backing up VMs.
- **libvirt's NAT network.** Nodes sit on `192.168.122.0/24` behind the
  hypervisor. A Cloud Hypervisor provider on another machine (ADR-0060), or
  anything resolving names through a DNS server the cluster runs, cannot
  reach them without tailscale or port forwarding.
- **Kairos means Hadron.** Kairos v4 publishes installer ISOs only for
  Hadron, its own distribution. A Kairos node on a familiar base, Debian
  here, is something one builds: `kairos-init` turns a stock distro image
  into a Kairos image and AuroraBoot turns that into an ISO.

## Decision

### 1. kine on an external SQL database, opt-in

`K0S_STORAGE_TYPE=kine` with `KINE_DATASOURCE` writes k0s's
`spec.storage: {type: kine, kine: {dataSource: …}}`. `etcd` stays the
default. The controllers then hold no cluster state: the database does, and
its backups are the cluster's. The data source carries a password, so it
comes from the operator's untracked env file, is refused if it contains a
character that would break the YAML string, and the generated k0sctl
config is written `0600`.

Running the database on the hypervisor host itself, outside every VM, keeps
cluster state independent of the VMs' lifecycle: a node can be rebuilt
without touching it.

### 2. Nodes on a host bridge, opt-in

`LIBVIRT_BRIDGE=<bridge>` attaches each VM to that bridge instead of a
libvirt network, so nodes take addresses from the LAN. libvirt then has no
DHCP lease to read, so the script asks the guest agent and then the host's
neighbour table. `VM_MACS` fixes each node's MAC address so the LAN's DHCP
server can reserve its address: a k0s node must not change address.

### 3. A Kairos-on-Debian installer ISO, built locally

`scripts/build-kairos-debian-iso.sh` builds one with the documented
`kairos-init` and AuroraBoot steps, pinned by version, and adds
`qemu-guest-agent` for decision 2. It carries no k0s: k0sctl installs
exactly `K0S_VERSION`, as before. The bootstrap script takes it through the
existing `BASE_IMAGE_PATH` with its digest pinned in `IMAGE_SHA256`.

## Consequences

**Positive**

- Cluster state survives the loss of every node, and is backed up with
  ordinary database tooling.
- Host-resident providers and DNS clients reach the cluster on the LAN.
- Nodes run Kairos's immutable layout on a base the operator knows.

**Negative / accepted costs**

- The database is a single point of failure for the control plane. It is
  one process on one host; its availability and backups are the operator's.
- kine is not etcd. k0s supports it, and it is a well-trodden path for k3s,
  but watch latency and compaction differ from etcd's.
- A bridge puts the nodes on the LAN, reachable by everything on it. The
  API server authenticates every request; the nodes' other ports are as
  exposed as any LAN host's.
- The ISO is built from upstream images that change: pin `kairos-init`,
  AuroraBoot and the Debian tag, and pin the ISO's digest when using it.
- `kube-apiserver`'s `externalAddress` is the first controller's address;
  with no load balancer in front, clients that use it lose the API if that
  node is down. k0s's control-plane load balancing (a virtual IP) is the
  follow-up.
