# Least-touch workflow

"Least touch on the user's workflow" is the practical face of the
[abstraction principle](abstraction-principle.md). It is the test we apply to
every design decision: *does this change what the user has to type, learn, or
remember?* If yes, the design is wrong.

This page walks through the three concrete scenarios where the principle is
most visible: **swapping** a provider, **mixing** providers in one cluster, and
**onboarding** a new backend.

## What "the user's workflow" actually is

When we say "the user's workflow," we mean the artifacts they touch and the
verbs they run:

```yaml
# The user's mental model — and ideally the entirety of their manifest:
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: db-prod-01
spec:
  class: db-prod-large
  image: ubuntu-22-04
  providerRef:
    name: prod
```

The verbs are `kubectl apply`, `kubectl get`, `kubectl describe`,
`kubectl delete`. The principle says: those four verbs, against that one CR,
should be everything the user ever needs — even as the infrastructure
underneath changes.

## Scenario 1 — Swapping a provider

A workload is running on vSphere. We want to move it to Proxmox.

**What the user has to do:**

```diff
-  providerRef:
-    name: prod-vsphere
+  providerRef:
+    name: prod-proxmox
```

One line. No retitling. No new SDK. No retraining. The class
(`db-prod-large`) and image (`ubuntu-22-04`) reference *names*, and those
names mean the same thing — by contract — on every provider that resolves them.

**What banlieue does behind the scenes:**

1. The main controller sees `providerRef` changed.
2. It cleans up the old infrastructure CR (`VSphereMachine`).
3. It creates a new one (`ProxmoxMachine`) carrying the same uniform spec.
4. The Proxmox provider watches, sees the new CR, and provisions.
5. Status propagates back; the user's `VirtualMachine` is `Ready=true` again.

**What the user did NOT have to do:**

- Learn Proxmox's API.
- Translate disk specs from vSphere semantics to Proxmox semantics.
- Re-do credentials.
- Rewrite their automation, their alerting, or their dashboards (status fields
  are the same).

That is what "least touch" means in practice.

## Scenario 2 — Mixing providers in one cluster

A platform team wants:

- Prod workloads on vSphere.
- Dev workloads on libvirt (cheap, local, throwaway).
- Edge workloads on Proxmox at a regional site.

All in the **same** Kubernetes cluster, addressed by the **same** kind:
`VirtualMachine`.

```yaml
---
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: db-prod-01
spec:
  class: db-prod-large
  image: ubuntu-22-04
  providerRef:
    name: prod-vsphere
---
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: dev-01
spec:
  class: dev-small
  image: ubuntu-22-04
  providerRef:
    name: dev-libvirt
---
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: edge-store-paris-01
spec:
  class: edge-medium
  image: ubuntu-22-04
  providerRef:
    name: edge-paris-proxmox
```

Three different backends. **One CR kind.** One set of conditions. One
`kubectl get vm` to see them all. One `kubectl describe vm` to debug any of
them.

What this enables, that wasn't easily possible before:

- **Policy-driven placement.** A higher-level controller (yours, written
  later) can decide *which* provider a `VirtualMachine` lands on — based on
  cost, geography, capacity, compliance — by patching `providerRef`. The user
  doesn't even pick.
- **Per-environment backends.** Dev is libvirt because it's cheap. Prod is
  vSphere because it's hardened. CI is Proxmox because it's fast to recycle.
  The workloads themselves don't know.
- **Graceful migrations.** Spin up a new backend, route a few `VirtualMachine`s
  onto it, watch them work, then move more. No big-bang cutover.

## Scenario 3 — Onboarding a new backend

A team running OpenStack wants to participate. Today, banlieue doesn't have an
OpenStack provider. Tomorrow they write one.

**What that team has to build:**

- A controller (a Rust crate, using `banlieue-provider-sdk`) that:
  - Watches `OpenStackMachine` infrastructure CRs.
  - Translates the uniform spec into OpenStack API calls.
  - Reports status back on the CR's `.status`.
- An infrastructure CRD (`OpenStackMachine`, `OpenStackMachineTemplate`) that
  satisfies the [CAPI v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/).
- A container image and a deployment manifest.

**What every existing user has to do to consume it:**

```diff
-  providerRef:
-    name: prod-vsphere
+  providerRef:
+    name: prod-openstack
```

That's it. Their `VirtualMachine` manifests don't change shape. Their
automation doesn't change. Their dashboards don't change. The OpenStack
provider plugs into the existing `VirtualMachine` API like a driver plugs
into an OS.

This is the *exponent* in banlieue's value: a single user-facing API surface,
multiplied by N pluggable backends. The work of adding a backend is bounded
and one-time. The benefit is global to every user.

## The smell test

When a feature is proposed, we apply the least-touch test:

| Question | If the answer is "yes," reconsider the design |
| --- | --- |
| Does this require the user to add a new field that only makes sense for one provider? | yes |
| Does this require the user to know which provider they're on to read status? | yes |
| Does this require the user to install a new tool? | yes |
| Does this require the user to learn new YAML conventions? | yes |
| Does this require the user to authenticate to anything other than the K8s cluster? | yes |

If any of those becomes "yes," the proposed design has leaked the backend into
the user's surface. The feature lives **behind** the API or it doesn't ship.

## In one line

> **The user's workflow is `kubectl apply` against a `VirtualMachine`. Nothing
> below that line should ever require them to know what's below that line.**

The next page — [CRD-only contract](crd-only-contract.md) — explains the
mechanism that lets banlieue enforce this without inventing yet another
transport between controllers.
