# Non-goals

What banlieue **refuses to be** is as important as what it is. Scope is a
feature. Every "no" on this page is what protects the abstraction from
softening into a leaky one.

## 1. banlieue is not a hypervisor

banlieue does not run VMs. It does not have a kernel module, it does not own a
host, it does not schedule onto cores. It is a *control plane* — a declarative
API plus controllers — that drives existing hypervisors (vSphere, Proxmox,
libvirt, …) through their own native APIs.

If you don't already have a backend, banlieue does nothing useful for you.

## 2. banlieue does not pretend VMs are pods

The unit is a `VirtualMachine`, not a wrapped pod. We don't schedule onto
Kubernetes nodes, we don't reuse the CNI, we don't pretend VM networking is
Service networking. If you want VMs to behave like Kubernetes workloads in
every respect — co-tenant with pods, on the same nodes — use
[Kubevirt](https://kubevirt.io/) for that.

## 3. banlieue does not replace Cluster API

[CAPI](https://cluster-api.sigs.k8s.io/) declares Kubernetes clusters. banlieue
declares VMs. A banlieue `VirtualMachine` is **not** a `clusterv1.Machine`. It
has no `Cluster`, no bootstrap config, no join token.

The two can coexist — and banlieue providers, by design, also satisfy the
CAPI v1beta2 InfraMachine contract, so a single provider can serve both — but
banlieue's user-facing API is independent of CAPI. It can be used in a cluster
that has never heard of CAPI.

## 4. banlieue does not expose backend-specific fields on `VirtualMachine`

This is the strict form of the [abstraction principle](abstraction-principle.md).
If a field only makes sense for vSphere, it does not appear on
`VirtualMachine`. It appears on the provider's own infrastructure CRD (e.g.
`VSphereMachine`). Users who use the default flow never see it. Users who
need the escape hatch reach for it explicitly, with full knowledge that they
are now coupled to a backend.

No `vsphere:` blocks. No `proxmox:` blocks. No `provider == "libvirt" ? do X`
branches in user manifests. Ever.

## 5. banlieue does not invent a new transport between controllers

The main controller and providers do not speak gRPC, REST, NATS, Kafka, or any
other off-cluster channel. The bus is the Kubernetes API; the messages are
CRDs. This is covered at length in
[CRD-only contract](crd-only-contract.md). Anything proposed that breaks this
is rejected — see the project's [`CLAUDE.md`](https://github.com/firestoned/banlieue/blob/main/CLAUDE.md#the-non-negotiables).

## 6. banlieue does not manage credentials *for* the user

Each `Provider` resource owns the credentials it needs to talk to its backend.
banlieue does not synthesise SSH keys, mint kubeconfigs, rotate vSphere
passwords, or wrap a secret manager. It expects credentials to be configured
once per provider, by an operator with cluster-level authority.

In return, the *user* never sees those credentials. Their manifest references
the `Provider` by name and nothing else.

## 7. banlieue does not do guest-OS configuration

What's installed inside the VM is a property of the **image**, plus any
cloud-init / ignition / sysprep the image consumes — not a property of
banlieue. We will reference cloud-init user-data because the standard CRD
shape calls for it, but banlieue does not template SSH keys, run Ansible, or
otherwise reach inside the VM after it boots.

Configuration management is a separate problem with mature tools. We do not
duplicate them.

## 8. banlieue does not implement live migration, snapshots, or backup

Not in v1alpha1. Maybe later, once the *uniform* shape of those operations
across backends is honest. (Spoiler: snapshots in vSphere and snapshots in
Proxmox have very different semantics, and a leaky abstraction here would be
worse than nothing.)

If the underlying backend supports these operations, you can drive them
through the provider's own infrastructure CRD until banlieue has a vetted
uniform contract.

## 9. banlieue does not solve storage modelling

Disks, datastores, storage classes, encryption-at-rest, dedup, tiering — all
of these vary wildly between backends. banlieue's `VirtualMachine` exposes a
minimal disk shape (size, count, persistence) and defers everything else to
the provider. If you need fine-grained storage policy, you express it at the
*provider* layer, by configuring the `Provider` resource or by referencing
backend-specific infrastructure CRDs directly.

## 10. banlieue is not a SaaS

It's a Kubernetes operator. You run it. You upgrade it. You set its RBAC.
There is no banlieue.io to call.

---

## A meta-rule

> If a planned feature can only be implemented by **leaking a backend
> concept into the user-facing API**, that feature does not ship in that
> shape. We either find a uniform contract that serves the use case, or we
> defer.

The cost of saying "yes" here is a leak that lives in user manifests forever.
The cost of saying "no" is one frustrated user who has the escape hatch of
directly reaching for a provider's infrastructure CRD when they truly need
backend-specific behaviour. We will choose the latter every time.

---

That closes the *why* section. From here, jump to:

- [Concepts → Architecture](../concepts/architecture.md) — how it's wired.
- [Guides](../guides/index.md) — install it.
