<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0086: Management cluster control plane behind a keepalived VIP, no konnectivity

- **Status:** Accepted
- **Date:** 2026-10-03
- **Deciders:** Erick Bourgeois
- **Related:** Resolves the follow-up recorded in
  [ADR-0085](0085-management-cluster-kine-bridge-kairos-debian.md)
  (`externalAddress` is the first controller's address);
  [ADR-0017](0017-vsphere-bootstrap-backend.md) (the bootstrap script and
  its backends, and the vSphere default of disabling konnectivity);
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (host-resident providers dial the cluster's API server)

## Context

`scripts/bootstrap-k0s-cluster.sh` builds a management cluster of three
`controller+worker` nodes by default. Its libvirt backend set
`spec.api.externalAddress` to the first controller's address. A live
management cluster built that way showed what that one address costs:

- **Everything in the cluster pinned to one controller.** With
  `externalAddress` set, k0s replaces the API server's own endpoint
  reconciler with one that publishes `externalAddress` as the only endpoint
  of the `kubernetes` Service. kube-proxy's kubeconfig, every pod's
  in-cluster client, and the join URL for new workers all named that one
  node. Two healthy controllers were unreachable from inside the cluster.
- **Konnectivity worked a third of the time.** Every konnectivity agent
  dialled `externalAddress:8132` and so reached only that controller's
  konnectivity server; the other two API servers held no agent connection.
  Any request an API server proxies to a node or pod (aggregated APIs such
  as `metrics.k8s.io`, admission webhooks, `kubectl logs/exec/port-forward`)
  failed with `No agent available` whenever it landed on them. The
  `metrics.k8s.io` APIService flapped between Available and
  `FailedDiscoveryCheck` every few seconds. An admission webhook would have
  failed two calls in three.
- **The first controller was a single point of failure** for every client
  outside the cluster too: kubectl and the host-resident Cloud Hypervisor
  providers (ADR-0060) all dialled it.

k0s has two built-in answers. Node-local load balancing (NLLB) puts an
Envoy on every worker's loopback, but it is incompatible with
`externalAddress`, does nothing for clients outside the cluster, and the
default topology's workers are mostly controllers. Control plane load
balancing (CPLB) runs keepalived inside k0s: a VRRP virtual IP that floats
between controllers, plus a userspace reverse proxy on the VIP holder that
balances port 6443 over every API server listed in the `kubernetes`
endpoints. It is supported on `controller+worker` nodes with the userspace
proxy (not with keepalived virtual servers). When CPLB is enabled, k0s
disables its own endpoint reconciler (`cmd/controller/controller.go`), so
each API server advertises itself again and the `kubernetes` endpoints list
every controller.

CPLB balances only the API port. Konnectivity agents dialling the VIP on
8132 would reach only the VIP holder, the same pinning as before.

## Decision

### 1. A keepalived VIP in front of the API server, with `externalAddress` set to it

`API_VIP=<address>/<prefix>` enables `spec.network.controlPlaneLoadBalancing`
(type `Keepalived`, one VRRP instance, the userspace reverse proxy) and sets
`spec.api.externalAddress` to the VIP's address. The VIP joins the
certificate SANs and becomes the default kubeconfig server.

`externalAddress` is the VIP, not unset, because k0s and k0sctl derive the
address kube-proxy, joining workers and k0sctl itself use from it. With
CPLB on, setting it no longer collapses the `kubernetes` endpoints to one
address: each API server publishes itself, and the reverse proxy balances
over all of them. The CPLB proxy's backend list *is* those endpoints, which
is why this matters.

The VRRP password (`API_VIP_AUTH_PASS`) is generated once and kept `0600` in
the work directory, because keepalived requires it to match on every
controller and a rerun must not change it. It prevents accidental
collisions between clusters on one LAN; VRRP's simple password is not
authentication. `API_VIP_ROUTER_ID` (default 51) must be unique on the
broadcast domain.

### 2. Multi-controller clusters require a VIP

The script refuses to configure a cluster with more than one controller
and no `API_VIP`, unless `NO_API_VIP=true` says that single entry point is
intended. The VIP is a LAN address the operator owns, so it has no
default: any default would be a guess, and a collision takes the LAN
address of something else.

### 3. Konnectivity disabled on both backends

`K0S_DISABLE_KONNECTIVITY` (default `true`) now applies to the libvirt
backend too, as `--disable-components=konnectivity-server` in each
controller's k0sctl `installFlags`. Both backends put every node on one
routable network, so each API server reaches kubelets and pods directly. The
management cluster's controllers are also workers, so they hold routes to
every pod CIDR. Konnectivity stays available (`false`) for a network where
the controllers cannot reach the nodes.

### 4. The VIP is reachable over the tailnet

With `TAILSCALE_AUTHKEY` set, each controller advertises `<VIP>/32` as a
Tailscale subnet route. Three advertisers make three HA subnet routers. A
client off the LAN reaches the VIP through whichever is up. Routes need
approval in the tailnet's admin console (or an `autoApprovers` policy); the
script prints the instruction.

## Consequences

**Positive**

- Losing any one controller no longer takes the API away from kubectl,
  host-resident providers, in-cluster clients, or new workers.
- Webhooks, aggregated APIs and `kubectl logs/exec` work on every API server.
  This is a precondition for SPIRE's controller manager, whose validating
  webhook would otherwise fail most calls.
- Both backends now share one konnectivity posture.

**Negative / accepted costs**

- **The VIP is an address the operator must reserve**, outside DHCP and any
  MetalLB pool. k0s does no IP address management, and a collision is
  silent until something else answers on it.
- **VRRP's password is not authentication.** Anything on the LAN can send
  VRRP adverts with a higher priority and take the VIP. That is the same
  exposure as any LAN host claiming an address by ARP, which ADR-0085's
  bridged nodes already accepted, and the API server still authenticates
  every request: a hijacked VIP can deny service or present a certificate
  clients reject, not impersonate the API.
- **The userspace proxy is one more hop** in k0s's own process on the VIP
  holder. Keepalived virtual servers (IPVS) would be faster but are not
  supported on `controller+worker` nodes.
- **No konnectivity means the API servers need direct reach** to every
  kubelet (10250) and pod. On the flat networks both backends build that is
  already true; a split network sets `K0S_DISABLE_KONNECTIVITY=false` and
  accepts konnectivity's pinning, or adds NLLB.
- **Converting an existing cluster leaves one step behind.** Rerunning
  `config` and `apply` rewrites `/etc/k0s/k0s.yaml` and, through k0sctl's
  reinstall phase, each controller's install flags. Disabling konnectivity
  does not remove what it deployed, though, and neither does removing
  `/var/lib/k0s/manifests/konnectivity`: the agent DaemonSet, its
  ServiceAccount and the `system:konnectivity-server` ClusterRoleBinding stay
  until they are deleted by hand (see the end-to-end setup guide).
