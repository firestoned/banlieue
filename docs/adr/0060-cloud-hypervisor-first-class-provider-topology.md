<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0060 — Cloud Hypervisor is a first-class provider, host-resident, with `External` provider classes

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Erick Bourgeois
- **Amends:** [ADR-0003](0003-provider-deployment-topology.md) (a `Provider`
  no longer always reconciles to a Deployment),
  [ADR-0012](0012-providerclass-crd-and-operator-role.md) (`ProviderClass`
  gains `deployment`; the operator's object set depends on it).
- **Related:** [ADR-0004](0004-single-binary-subcommand-dispatch.md) (single
  binary), [ADR-0011](0011-libvirt-provider-own-client.md) (libvirt client,
  mTLS-only transport), [ADR-0013](0013-banlieue-bootstrap-cli.md) (bootstrap CLI),
  [ADR-0050](0050-libvirtmachine-domain-lifecycle.md) (InfraMachine on a KVM
  host); roadmap 09 (Cloud Hypervisor provider) and its phase 0 spike;
  roadmap 17 (first consumer).

> **Decision 1 is the maintainer's call (2026-09-25):** Cloud Hypervisor gets
> first-class support as its own provider, not libvirt's `ch` driver. The
> remaining decisions are proposed and await review.

## Context

Roadmap 09 opened with a gate between two shapes for Cloud Hypervisor:

- **A** — a host-resident provider that drives the VMM itself;
- **B** — libvirt's `ch` driver, reached through the existing
  `banlieue-libvirt` client and `LibvirtMachine`.

B was framed as "nearly free, test it first". The phase 0 spike
(2026-09-25) and a closer look at what B buys argue against it:

- **It is not packaged where it would run.** Debian 13's libvirt (11.3.0)
  ships no `ch` connection driver. B there means building libvirt from
  source, so "days, not weeks" does not hold.
- **It keeps libvirtd in the trusted computing base.** Roadmap 09's first
  reason for Cloud Hypervisor at all is attack surface for untrusted
  sandbox guests (roadmap 17). Putting a large C daemon back between the
  provider and a small Rust VMM gives back half of that.
- **It exposes a subset of the VMM.** vTPM, hotplug and snapshot coverage
  in the driver are unverified, and snapshot-to-disk (roadmap 09 phase 7)
  would be off the table unless the driver grows it.
- **It is a second-class citizen by construction.** The scheduler would have
  to learn that a `libvirt` Provider may secretly be a different VMM, and
  every Cloud Hypervisor gotcha would surface through libvirt's XML model
  instead of the VMM's own API.

What makes A non-trivial is that **Cloud Hypervisor has no daemon**. It is
one process per guest serving a REST API on a local Unix socket. Something
on the host must spawn and supervise it, create tap devices and place
disks. Every provider so far is a Deployment in the management cluster
reaching its backend over the network (ADR-0003); there is nothing on a
Cloud Hypervisor host to reach.

Two shapes that would keep ADR-0003 intact are already rejected in roadmap
09 and stay rejected: a first-party host daemon with an mTLS API (invents a
wire protocol and a privileged listener — writing libvirtd again), and SSH
exec from an in-cluster provider (ADR-0011 already ruled out a CLI's stdout
as a wire format).

## Decision

### 1. First-class `cloud-hypervisor` provider class, not libvirt's `ch` driver

Cloud Hypervisor is its own backend: provider class `cloud-hypervisor`, its
own InfraMachine CRD (`CloudHypervisorMachine`, ADR-0062), its own client
crate for the VMM API (ADR-0061), and its own provider crate. The libvirt
provider does not accept `ch://` or `ch+tls://` URIs, and a `libvirt`
Provider is always QEMU.

### 2. The provider is host-resident

`banlieue provider cloud-hypervisor` (ADR-0004 subcommand, behind a Cargo
feature) runs **on the KVM host** as a systemd service. **One `Provider` is
one host.** It talks to:

- the Kubernetes API server, **outbound only**, with its own scoped
  identity (Decision 5);
- the local VMM sockets and systemd (ADR-0063), and netlink for taps.

It opens no listening socket. The host pulls; nothing pushes to it.
Non-negotiable 1 (no RPC between controller and providers) holds as
written: the controller and this provider still meet only in CRDs.

### 3. `ProviderClass.spec.deployment: Managed | External`

A new optional field, default `Managed`:

- **`Managed`** — today's behaviour. The operator server-side-applies the
  full ADR-0003 object set, Deployment included.
- **`External`** — the operator applies the **identity and RBAC** for each
  `Provider` of the class (ServiceAccount, Role, RoleBinding,
  ClusterRoleBinding, Lease) but **no Deployment**. The process runs
  elsewhere and authenticates as that ServiceAccount.

`image`, `replicas`, `resources`, `nodeSelector` and `tolerations` are
meaningless for `External` and are rejected there by the admission policy
(ADR-0007), rather than silently ignored. Naming, labels, owner references,
the prune selector and the ClusterRoleBinding finalizer from ADR-0003 are
unchanged, so `External` adds no second cleanup path.

`Provider.status.workload` reports `{mode: External}` with no Deployment
name and no replica count. Whether the host is actually running is not the
operator's to say: the provider's own `Ready` condition and its Lease
renewals are the liveness signal.

### 4. `connection.credentialsRef` becomes optional

A host-resident provider has no remote endpoint to authenticate to, and
must read **no Secrets at all** (Decision 5). `ProviderConnection` changes:

- `credentialsRef` becomes optional. When it is absent the operator grants
  no Secret `get` whatsoever.
- `endpoint` stays required and, for `cloud-hypervisor`, names the host
  (for example `bar.foo.io`). It is informational — shown in status and
  events — never dialled.
- The `cloud-hypervisor` provider sets `Ready=False`,
  reason `CredentialsNotAllowed`, on a `Provider` that sets
  `credentialsRef`, and does nothing else with it.

### 5. The host credential: a bounded, self-renewing ServiceAccount token

The provider authenticates with a **bound ServiceAccount token** from the
TokenRequest API, never a legacy long-lived token Secret:

- `banlieue bootstrap` (ADR-0013) gains a host subcommand that, run by an
  admin, requests the first token for that Provider's ServiceAccount and
  writes a kubeconfig on the host, mode 0600, owned by the provider's
  system user.
- The provider **renews its own token** before expiry through TokenRequest
  on its own ServiceAccount (`serviceaccounts/token`, `create`, scoped by
  `resourceNames` to that one ServiceAccount) and rewrites the kubeconfig
  atomically.
- Token lifetime is bounded (default 24h, renewed at half-life). A host
  that stays down longer than one lifetime must be re-bootstrapped. That is
  the intended failure mode: a stolen, stale credential dies on its own.

The per-instance Role for an `External` provider is the smallest the job
allows: a server-side-filtered watch on its own `Provider` and its own
`CloudHypervisorMachine`s, status patches on those, its own `VMImage`
status row, its own Lease, events, and TokenRequest on itself. **No Secret
reads.** User-data arrives already resolved in the machine spec
(ADR-0025, ADR-0038).

### 6. Leader election stays

The Lease is kept even though one host runs one process. Its job changes
from "one of N replicas" to **fencing a misconfiguration**: two hosts
bootstrapped against the same `Provider` would otherwise both create
guests for the same machines. The second host fails to acquire the Lease
and reports it.

### 7. Upgrades are host package management

The provider binary on the host is upgraded like any other host package
(the bootstrap script installs and pins it). Running guests are systemd
units, not children of the provider (ADR-0063), so a provider restart or
upgrade does not restart them. The provider refuses to start against a
`ProviderClass` whose `spec.backend` it was not built with.

## Consequences

**Positive**

- Cloud Hypervisor is used as itself: its full API, its own gotchas
  handled in one place, and no libvirtd in the sandbox TCB.
- No new wire protocol and no listener on the hypervisor. The only new
  path is outbound HTTPS to the API server, which a host already needs to
  pull images.
- `External` is general. Any future daemonless backend reuses it rather
  than reopening ADR-0003.
- The credential on the host is short-lived, self-renewing and
  single-object scoped. It reads no Secrets.

**Negative / accepted costs**

- **A cluster credential lives on a hypervisor**, and a guest escape now
  lands next to it. Mitigated by Decision 5's narrow RBAC and bounded
  lifetime, and recorded for the threat-model pass (new trust boundary:
  host-resident provider ↔ API server).
- Upgrades are no longer `kubectl apply`. Host packaging, the systemd unit
  and the bootstrap script become part of the product.
- Build artifacts must reach a host outside the cluster; ADR-0064 decides
  how.
- `ProviderConnection.credentialsRef` going optional touches every provider:
  vSphere and libvirt must now reject its absence at reconcile time.
- `ProviderClass` validation grows: `External` classes reject pod-shape
  fields.

**Follow-ups**

- ADR-0061 (VMM client), ADR-0062 (`CloudHypervisorMachine`), ADR-0063
  (host supervision), ADR-0064 (artifact delivery), ADR-0065 (vTPM and
  `Deferred`), ADR-0066 (optional snapshot-to-disk).
- CALM: add the host-resident provider node, its outbound-only relationship
  to the API server, and the hypervisor-host trust boundary.
- Threat model: the credential-on-hypervisor boundary above.
- Roadmap 09: the gate is decided; the `ch` driver checks and the
  "If the gate picks B" section are superseded.
