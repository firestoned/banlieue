# The abstraction principle

banlieue is, before anything else, an opinion about **where the variation
between virtualization backends should live**.

The opinion is: **not in the user's hands.**

## The principle, stated once

> The user-facing API should describe *what* they want — not *how*, not *where*,
> not *with what credentials*, not *against which SDK version*.
>
> All variation between backends — vSphere, Proxmox, libvirt, the next one —
> belongs **behind the API**, in provider implementations that the user does
> not see and does not need to learn.

This is the **least-touch principle**. The user's workflow should be touched as
little as possible — ideally not at all — when the underlying provider changes,
when a new provider is added, or when one workload is moved from one backend to
another.

## Why this principle, and not a thinner one

A thinner version of this idea is: *"give users one API, internally translate to
each backend."* Lots of tools have tried that — and most have failed for the
same reason: they leak.

They leak when:

- The user has to specify provider-specific options inline ("for vSphere, set
  this field; for Proxmox, set that one").
- Status fields differ between backends, so the user has to know which one
  they're talking to in order to read their VM's state.
- Authentication is the user's problem ("here's the vSphere cred, here's the
  Proxmox token, you wire it up").
- Errors come back in the backend's native language.

Every leak is a place where the user is forced to *care* about a specific
backend, and every place the user is forced to care is a place where you've
just re-coupled them. The leaks aren't bugs — they're the result of an
insufficiently rigorous abstraction.

banlieue's principle is stricter:

- **Specs are uniform.** Provider-specific knobs do not appear on
  `VirtualMachine`. If a backend can't satisfy a uniform spec, it's the
  provider's job to fail with a clear status — not the user's job to bend.
- **Status is uniform.** Every `VirtualMachine` exposes the same set of
  conditions, the same lifecycle phases, the same readiness semantics. The
  provider translates *its* native states into *banlieue's* contract.
- **Credentials are providers' problem.** The user references a `Provider` by
  name. The provider holds the credential. The user never sees a vSphere
  password or a Proxmox API token in a manifest.
- **Errors are uniform.** Backend-specific errors are translated into
  banlieue-native conditions ("`ProvisioningFailed`", "`ImageNotAvailable`",
  "`ProviderUnreachable`") that the user can act on without knowing which
  backend produced them.

## What this gets us

When the abstraction is honest — when the user-facing surface really is
backend-agnostic — three things become *easy* that used to be *hard*:

1. **Swapping a provider.** A workload pinned to `providerRef: prod-vsphere`
   can be repointed to `providerRef: prod-proxmox` without touching anything
   else.
2. **Mixing providers.** A single cluster can route some VMs to vSphere, some
   to libvirt, some to Proxmox — driven by class/label/policy, not by
   parallel implementations of the user's manifest.
3. **Adding a new backend.** Someone writes a provider once. Every existing
   `VirtualMachine` is potentially deployable there. No user changes anything.

These are not features. They are *consequences* of the principle. If banlieue
ever ships a feature that requires the user to touch their manifest when a
provider changes — that feature is broken on arrival.

## The cost of the principle

It is not free. The cost is paid by **providers**, not users:

- Each provider must do the translation from banlieue-uniform semantics to its
  backend's native semantics. That work is non-trivial.
- Each provider must report status in banlieue's contract, even when its native
  status model differs.
- Providers cannot expose escape hatches in the user-facing API. If they need
  backend-specific configuration, it goes in their *own* infrastructure CRD
  (e.g. `VSphereMachine`) — referenced by the user's `VirtualMachine` only by
  shape, never by content.

This is a deliberate trade. We accept higher *provider* complexity to deliver
near-zero *user* complexity. The leverage is the right way around: there is
exactly one user-facing API to keep clean, and N providers that absorb the
mess. Hiding mess from many users behind a small number of providers is the
whole point.

## How banlieue enforces the principle

Three concrete mechanisms keep the principle honest:

### 1. `VirtualMachine` has no provider-specific fields

The CRD is reviewed against the principle. If a field smells backend-specific —
even subtly — it goes on the provider's infrastructure CRD instead. The
[`crates/banlieue-api`](https://github.com/firestoned/banlieue/tree/main/crates/banlieue-api)
type system is the source of truth, and it is small on purpose.

### 2. Providers implement a fixed contract

Provider infrastructure CRDs (`VSphereMachine`, future `ProxmoxMachine`, future
`LibvirtMachine`) satisfy the
[Cluster API v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/).
That contract is **not banlieue-invented**; it's a public, versioned, multi-vendor
spec for *what an infrastructure object looks like* that the CAPI community has
already vetted. By piggybacking on it, banlieue gets a battle-tested status
model and providers get a path to also be reusable as CAPI infra providers.

### 3. Communication between controller and providers is CRDs only

No RPC. The main banlieue controller patches infrastructure CRs; providers
watch them. Providers patch their own status; the main controller watches it.
The Kubernetes API is the bus. This is covered in detail in
[CRD-only contract](crd-only-contract.md), and it's why providers cannot leak a
sneaky out-of-band channel into the user's flow.

## In one line

> **Users describe VMs. Providers run hypervisors. banlieue is the thin layer
> that refuses to let the second leak into the first.**

The next page — [Least-touch workflow](least-touch.md) — makes this concrete by
walking through swapping, mixing, and onboarding scenarios.
