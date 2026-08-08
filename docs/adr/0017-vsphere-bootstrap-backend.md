<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0017 — vSphere backend for the k0s bootstrap script

- **Status:** Accepted
- **Date:** 2026-08-03
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0013 (`banlieue bootstrap` installs the platform *onto* a
  cluster); ADR-0010 (VMImage build pipeline / kairos-operator, the workload
  the imagebuild node is reserved for); ADR-0002 (InfraCluster failure-domain
  aggregation — the same failure-domain reasoning drives the node topology
  here).

## Context

`scripts/bootstrap-k0s-cluster.sh` stands up the **management cluster** — the
k0s cluster that runs the banlieue controllers and kairos-operator. It is the
substrate `banlieue bootstrap` (ADR-0013) later installs onto; it is *not* a
workload provisioned through banlieue itself.

Until now the script had exactly one provisioning backend: **libvirt/KVM**. It
creates Kairos "Hadron" VMs with `virt-install`, drives an unattended
install-from-ISO, then installs k0s with `k0sctl`. That is the right tool for a
laptop or a single KVM host, but it cannot stand up a management cluster in an
on-prem **VMware vSphere** estate, which is where banlieue now needs to run so
it can exercise the vSphere provider against a real vCenter.

The forces:

1. **Reuse, don't fork.** The maintainer explicitly wants *one* bootstrap
   script serving both libvirt and vSphere, not a parallel copy. The
   k0s-specific half — `k0sctl` config generation, `apply`, kubeconfig fetch,
   imagebuild-node labelling — is already backend-agnostic. Only VM
   create / IP-discovery / destroy are libvirt-specific.
2. **Templates, not ISOs.** The vSphere estate already publishes maintained,
   cluster-specific Kairos VM **templates** (e.g. an `rhelXX-kairos-*-vX.Y.Z`
   family, one copy per compute cluster). Cloning a template is faster and
   more idempotent than the Hadron empty-disk + installer-ISO dance, and it
   keeps OS provenance in the platform team's hands. The vSphere path
   therefore drops the ISO-install flow entirely: clone → set config → power
   on → wait for SSH.
3. **Failure domains for free.** A production vSphere estate is partitioned
   into several compute clusters, each with its own resource pool, SDRS
   datastore cluster, and DVS port group / subnet. Spreading the k0s nodes
   evenly across those clusters makes each vSphere cluster a **failure
   domain**: a control-plane node in each means etcd quorum survives the loss
   of any single vSphere cluster. This mirrors the InfraCluster failure-domain
   model in ADR-0002.
4. **No real infrastructure in a public repo.** vCenter hostnames, datacenter
   names, resource-pool naming, subnets, DNS servers, and node IPs are all
   real infrastructure identifiers (`rules/no-real-infrastructure.md`). None of
   them may be committed. The script must therefore obtain every environment
   specific value **at runtime** — from the standard `GOVC_*` environment, from
   `govc find` discovery, and from an operator-supplied, untracked config —
   never from a baked-in default.

## Decision

Add a pluggable **`BACKEND`** selector to `scripts/bootstrap-k0s-cluster.sh`
with two values: `libvirt` (the default, unchanged behaviour) and `vsphere`.

**Backend seam.** Factor the three backend-specific concerns behind a small
dispatch layer, leaving the k0s half untouched:

- `backend_check_deps` — libvirt: `virt-install`/`virsh`/…; vSphere: `govc`/`jq`.
- `backend_create_node <name>` — libvirt: existing empty-disk + ISO-install;
  vSphere: `govc vm.clone` from the cluster-specific template, NIC pinned to
  PCI slot 192 (`ens192`), static networking via both `guestinfo.network.*`
  and a systemd-networkd cloud-config stage, cloud-init via
  `guestinfo.userdata` (base64), CPU/memory/disk reconfigure, power on.
- `node_ip <name>` — libvirt: DHCP lease via `virsh domifaddr`; vSphere: the
  node's declared static IP.
- `backend_destroy_node <name>` — libvirt: `virsh undefine`; vSphere:
  `govc vm.destroy`.

**k0s install: native on vSphere, k0sctl on libvirt.** The libvirt path keeps
`k0sctl` (uploads the binary over SSH to `/usr/local/bin/k0s`). The vSphere path
does **not** use k0sctl: the estate's Kairos image persists `/opt/k0s`,
`/var/lib/k0s`, and `/etc/k0s` as `COS_PERSISTENT` bind mounts and expects the
k0s binary at `/opt/k0s/<ver>-amd64` symlinked to `/usr/local/bin/k0s` —
a layout k0sctl cannot produce (it always installs the binary directly to
`/usr/local/bin/k0s`). So the vSphere backend installs natively, mirroring the
maintainer's Ansible flow: each node downloads the binary from
`K0S_BINARY_BASEURL` (an internal Artifactory mirror on-prem) into `/opt/k0s`,
verifies its sha256, and symlinks it; the first controller runs
`k0s install controller --enable-worker --no-taints -c /etc/k0s/k0s.yaml`, and
the remaining nodes join with `k0s token create` (controller / worker). The
generated `k0s.yaml` uses the estate's CNI (`calico`) and internal image mirror
(`K0S_IMAGE_REPOSITORY`), with SANs covering `API_SAN` + every node FQDN/IP.

**Topology is a declared node table, not a count.** The libvirt path keeps its
`VM_COUNT` + uniform-config model. The vSphere path is driven by an explicit,
**untracked** node table (one row per node: hostname, vSphere cluster id,
static IP, k0s role), because in a real estate every node differs in placement
and address. Default vSphere topology: **two nodes in each of three compute
clusters** (six nodes), three of them `controller+worker` (one per cluster) so
etcd quorum spans three failure domains, three `worker`. Every pure worker is
labelled `banlieue.io/imagebuild=true` and tainted
`dedicated=imagebuild:NoSchedule` (one imagebuild node per failure domain);
`IMAGEBUILD_NODE` can pin a specific subset. This generalises the libvirt path's
single reserved worker.

**Placement is discovered, never hardcoded.** For each cluster id the script
resolves resource pool, SDRS datastore, DVS port group, and the
cluster-specific template path via `govc find` / `govc datastore.info`
(the same technique the maintainer's Ansible provisioner uses), so no estate
naming convention is committed. Gateways default to the first three octets of
the node IP + `.1`; DNS servers, search domain, and the API-server SAN come
from the untracked config.

**Secrets and identifiers stay out of the tree.** vCenter credentials come from
the ambient `GOVC_*` environment. The node table and network parameters live in
an operator-supplied env file outside the repo (loaded with `--env-file` or
`source`). Corporate HTTP proxies are explicitly unset before every on-prem
`govc`/`kubectl` call.

## Consequences

**Positive**

- One script, two substrates; the k0s half is exercised identically by both,
  so fixes there benefit both backends.
- The management cluster gains real HA: losing a whole vSphere compute cluster
  costs one control-plane node, not the cluster.
- Nothing environment-specific enters git history; a fresh checkout on a
  different estate works by supplying a different env file, changing no code.
- The vSphere path is markedly simpler than libvirt's (no ISO fetch, no
  install-wait, no media eject) because it clones a ready template.

**Negative / trade-offs**

- The script grows a second code path and a discovery layer; mitigated by the
  narrow backend seam and by keeping the libvirt default byte-for-byte
  compatible.
- vSphere discovery depends on the estate's naming conventions being
  `govc find`-able; if an estate diverges, the operator must set the resolved
  values explicitly in the env file (every discovered value is overridable).
- Cross-subnet clusters assume the three node subnets are mutually routable
  (etcd / konnectivity / kube traffic crosses them). This is an operator
  precondition, asserted in the script’s docs, not something the script can
  guarantee.

**Follow-ups**

- This script provisions the *management* cluster. Provisioning *workload* VMs
  on vSphere remains the job of the vSphere provider's `VSphereMachine`
  reconciler (still unimplemented; capability introspection + VMImage only for
  now) — deliberately out of scope here.
