# Comparisons

banlieue isn't alone in this neighbourhood, and it's worth being honest about
which neighbours it resembles, which it doesn't, and why none of them solve
the same problem.

This page is **not** a "banlieue is better than X" sales pitch. It's a map of
the space, so you can decide whether banlieue is the right thing for your
problem — or whether one of these projects is.

## TL;DR

| Project | Solves | Doesn't solve |
| --- | --- | --- |
| **[Kubevirt](https://kubevirt.io/)** | Running VMs *as Kubernetes pods*, with libvirt under the hood. | Multi-backend abstraction. Kubevirt *is* the backend. |
| **[Cluster API](https://cluster-api.sigs.k8s.io/) (CAPI)** | Lifecycle of Kubernetes *clusters* (control planes, node pools, bootstrap). | Standalone VMs that aren't part of a cluster. |
| **[Crossplane](https://www.crossplane.io/)** | Declarative provisioning of cloud resources via Kubernetes. | A focused, opinionated VM contract. Crossplane is generic; it doesn't claim "the user manifest is portable across backends." |
| **[Terraform](https://www.terraform.io/) / OpenTofu** | Imperative-then-declarative provisioning, from a CLI, across many providers. | A control plane. Terraform reconciles on `apply`; banlieue reconciles continuously. |
| **Direct hypervisor SDKs** (govmomi, proxmox-api, libvirt) | Programmatic access to one backend. | Abstraction. They are the *fragmentation* banlieue exists to hide. |
| **banlieue** | One user-facing `VirtualMachine` CR. N pluggable providers. CRD-only contract. | Running VMs inside pods. Bootstrapping K8s clusters. Generic resource modelling. |

The rest of this page walks through each row.

## Kubevirt

[Kubevirt](https://kubevirt.io/) lets you run VMs as Kubernetes workloads —
each VM is wrapped in a pod, libvirt does the heavy lifting under the hood,
and the VM lives on the same Kubernetes nodes as your other pods.

**Where it overlaps with banlieue:** the user-facing object is a CRD
(`VirtualMachine`), the API is Kubernetes-native, and the lifecycle is
declarative.

**Where it differs fundamentally:**

- Kubevirt **is** the backend. The hypervisor is libvirt, on the Kubernetes
  nodes, period. There's no "swap Kubevirt for vSphere" — that's not a coherent
  sentence.
- Kubevirt VMs are co-tenant with your pods. That's a feature for some workloads
  ("run a Windows VM next to my microservices") and a non-starter for others
  ("my prod database needs to live on the dedicated vSphere cluster behind a
  firewall, not on a worker node").
- The unit of compute is a pod. Capacity, scheduling, and networking flow through
  Kubernetes primitives.

**When Kubevirt is right:** when your VMs *should* be Kubernetes workloads — same
operational model as pods, same lifecycle, running on the cluster's hardware.

**When banlieue is right:** when your VMs live *outside* Kubernetes (on a vSphere
cluster, a Proxmox cluster, libvirt hosts elsewhere) and you want a Kubernetes
*API* over them, not a Kubernetes *runtime* under them.

The two can absolutely coexist in the same cluster — a `VirtualMachine` could be
backed by a Kubevirt provider one day. They are not competitors.

## Cluster API (CAPI)

[CAPI](https://cluster-api.sigs.k8s.io/) is the upstream Kubernetes project for
declarative cluster lifecycle: control planes, machine pools, bootstrap configs,
infrastructure providers. CAPI deeply influenced banlieue's design — the
[CRD-only contract](crd-only-contract.md) is *their* idea, and we re-use their
[v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/)
verbatim.

**Where it overlaps:** the provider model. CAPI infra providers (CAPV, CAPP,
CAPL, etc.) speak to the same hypervisors banlieue cares about. Their
`InfraMachine` CRDs are the contract our provider CRDs satisfy.

**Where it differs:** CAPI's unit of value is a **cluster**. Everything CAPI
does is in service of producing a working Kubernetes control plane and joining
nodes to it. A *standalone* VM — one that has nothing to do with bootstrapping
a Kubernetes node — has no CAPI model. You don't get a `Machine` without a
`Cluster`.

banlieue's `VirtualMachine` is **explicitly not** a `clusterv1.Machine`. It's a
peer-level concept: a VM that exists on its own merits.

That said, banlieue providers — by design — also work as CAPI infra providers.
A `VSphereMachine` written for banlieue satisfies the CAPI v1beta2 contract, so
a CAPI `Cluster` can consume the same provider with no changes. **You can have
banlieue and CAPI in the same cluster, using the same provider, sharing the
same credentials.**

**When CAPI is right:** when the thing you're declaring is a Kubernetes
cluster.

**When banlieue is right:** when the thing you're declaring is a VM that may
or may not ever join a cluster.

## Crossplane

[Crossplane](https://www.crossplane.io/) lets you model cloud resources as
Kubernetes CRDs and have controllers reconcile them against cloud APIs. In
principle, someone could build a Crossplane provider for vSphere and another
for Proxmox and declare VMs with both.

**Where it differs:**

- Crossplane is **generic**. It models *anything* — buckets, databases, IAM
  roles, load balancers. VMs are one resource among many. There is no canonical
  "VirtualMachine" abstraction in Crossplane; each provider exposes its own
  shape (`VSphereVM`, `ProxmoxVM`, etc.). The user's manifest is **not portable
  across them**.
- Crossplane providers each expose their backend's native model. Swapping
  isn't a one-line `providerRef` change — it's writing a new manifest of a
  different kind.
- Crossplane's "composition" feature can paper over some of that, but composition
  is project-specific glue, not an opinionated contract.

banlieue trades Crossplane's generality for **a focused, opinionated VM
contract**. One kind. One spec shape. One status shape. Many backends. The
abstraction is *what makes the manifest portable*; without it, you're back to
fragmentation under a Kubernetes-shaped hat.

**When Crossplane is right:** when you need to model a wide variety of
resources declaratively, and per-resource heterogeneity is acceptable.

**When banlieue is right:** when you want exactly one VM API surface, and
backend portability is the whole point.

## Terraform / OpenTofu

[Terraform](https://www.terraform.io/) (and its fork
[OpenTofu](https://opentofu.org/)) provides declarative provisioning across
hundreds of providers from a CLI.

**Where it overlaps:** declarative, provider-pluggable, supports vSphere /
Proxmox / libvirt.

**Where it differs:**

- Terraform is a **batch tool**: state is captured at `apply` time. Drift is
  detected on re-`plan`, not corrected by a continuous loop.
- Terraform doesn't *abstract* between providers; each provider exposes its
  backend's native resources. `vsphere_virtual_machine` and `proxmox_vm_qemu`
  are different resource kinds with different schemas. The manifest is **not
  portable**.
- Terraform lives outside Kubernetes. The state is in S3 / Consul / a backend.
  It doesn't react to events.

banlieue is a **continuous reconciliation control plane** with **one
user-facing kind**. Those are both Terraform's blind spots, by design.

**When Terraform is right:** for one-shot provisioning across many resource
kinds, when the team has a Terraform workflow already.

**When banlieue is right:** when the model is "the cluster is the source of
truth and the controller continuously converges to it."

The two can complement each other: Terraform might lay down the *provider* (a
vSphere cluster, RBAC, a `Provider` CR), and banlieue takes over the per-VM
lifecycle from there.

## Direct hypervisor SDKs (govmomi, proxmoxer, libvirt-go)

The "you don't need this project" position: just call the hypervisor's API
directly.

This is exactly the world banlieue exists to escape. Every team that takes
this path ends up writing the same glue, and the variation lands on every
caller. See [The problem](problem.md) for the full argument.

These SDKs are not competitors — they are *inputs*. Providers use them under
the hood. The whole point of banlieue is that **users don't.**

## A simpler way to choose

If you're not sure which project fits your problem, ask three questions in
order:

1. **Are you declaring a Kubernetes cluster?** → Use CAPI.
2. **Do you want VMs *as* Kubernetes pods, scheduled by Kubernetes?** → Use
   Kubevirt.
3. **Do you want one Kubernetes-native API that works across vSphere /
   Proxmox / libvirt / your-next-backend, with the same manifest?** → banlieue.

If none of those fits, you might just want Terraform or a direct SDK.

The next page — [Non-goals](non-goals.md) — closes the loop on the project's
scope.
