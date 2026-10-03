<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0087: A field manager names a writer, not a provider class

- **Status:** Accepted
- **Date:** 2026-10-03
- **Deciders:** Erick Bourgeois
- **Related:** Restores the invariant
  [ADR-0015](0015-vmimage-status-merge-strategy.md) relied on but never stated
  precisely; [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md) (the
  original `VMImage.status` ownership split);
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md) (the
  host-resident provider, which is what broke the invariant);
  [ADR-0003](0003-provider-deployment-topology.md) (one provider deployment
  per `Provider`).

## Context

ADR-0010 split `VMImage.status` across field managers so that server-side
apply would merge concurrent writes instead of clobbering them.
ADR-0015 found that it did not work, diagnosed why (`perProvider` and
`conditions` were plain arrays, which SSA treats as atomic), and fixed it by
making both lists `x-kubernetes-list-type: map`. Its stated model:

> With this, each provider owns only the entry it applies.

That sentence is true of *managers*, not of *providers*, and in 2026-07 the
distinction did not exist: one provider class meant one running writer, so
`banlieue.io/provider-libvirt` and "the libvirt provider" named the same
thing. ADR-0015's own reproduction applied as `provider-vsphere` and then as
`provider-libvirt`: two classes, two managers, two writers.

What actually matters is not how many processes run, but **how much of the
list each one writes**. The pod-based providers all write the *whole* set:

```rust
let providers = list_libvirt_providers(&ctx).await?;   // every Provider of the class
for provider in &providers { rows.push(reconcile_for_provider(..).await); }
patch_status(&ctx, &name, generation, rows).await?;    // one apply, all rows
```

vSphere and Proxmox are the same shape. Several replicas applying that under
one manager is harmless: each computes the same complete set from the same
cluster state, so the apply is idempotent and no row is ever absent from it.
A class-scoped manager is correct for them, and they keep it.

**ADR-0060 inverted that.** The host-resident Cloud Hypervisor provider is one
writer per host, and each host writes **only its own row**, because a host can
only speak for its own image cache. N writers, N disjoint single-row applies,
one manager name.

A merge-keyed list separates *owners*. It cannot separate two writers that
present the same owner name. Under SSA a field manager owns exactly the field
set it last applied, so when host B applies its own row under the manager that
host A just used, the apiserver sees that manager no longer declaring A's row
and **deletes it**. A's watch fires, A re-applies, deleting B's row. The
`.force()` on every provider apply suppresses the conflict that would otherwise
have surfaced this immediately.

**Measured on the management cluster**, one `VMImage` visible to three
host-resident Cloud Hypervisor providers:

```
resourceVersion 366324 -> 728405 in ~35 min      # ~362,000 writes, ~170/sec
status.conditions[Ready]                          # flapping True/False
status.perProvider                                # ONE row at a time,
                                                  # rotating between the hosts
```

Each host logged `VMImage row published ... ready=true reason=Reconciled` tens
of times per second. This is ADR-0015's failure mode exactly, reached by a
different route, and `bug-116` is the same entry in the log.

The deeper problem is that the invariant was never written down anywhere a
reader could check it against the code. `banlieue-provider-sdk/src/ssa.rs`
asserted the opposite of what ADR-0060 then built:

> each CRD has exactly one controller writing each subresource.

Nothing failed when that became false, because nothing stated it as a rule the
new topology had to satisfy. The class constant was simply there, it compiled,
and the `.force()` hid the consequence.

## Decision

**A field-manager string identifies one running writer. Where a provider class
can run more than one writer, the manager name carries the `Provider`'s
identity.**

1. **A manager must be scoped exactly when its writer owns only part of what
   the manager could own.** Concretely: a writer that applies the *complete*
   set of its class's `perProvider` rows may use the class constant; a writer
   that applies a *subset* must scope the manager to its own identity, as
   `banlieue.io/provider-<class>/<provider-namespace>/<provider-name>`, built
   by one helper in `banlieue-provider-sdk` and never by concatenation at the
   call site.

   Today that means: Cloud Hypervisor scopes (one row per host); vSphere,
   Proxmox and libvirt do not (each applies every row of its class);
   `banlieue.io/controller` and `banlieue.io/imagebuilder` do not (one writer
   each). The test of a new provider is the subset question, not the process
   count.

2. **The helper is the only way to name a provider manager.** Call sites take a
   `&Provider` or its namespace/name, not a `&str`, so "forgot to scope it" is
   a type error rather than a silent cluster-wide write loop.

3. **`.force()` goes away on a scoped single-row `perProvider` write.** It
   existed to take ownership from a *previous* manager; with a per-writer name
   no legitimate conflict can arise on a row keyed by the writer's own
   identity. Keeping it would preserve exactly the behaviour that hid this bug.
   A conflict after this change is a real defect and must surface as one. The
   whole-set writers keep `.force()`, since taking over a row abandoned by a
   removed `Provider` is part of what owning the whole set means.

4. **Field-manager names are bounded.** The apiserver caps a manager at 128
   characters and `managedFields` grows one entry per distinct manager, so the
   helper truncates deterministically (stable hash suffix) rather than letting
   a long namespace/name produce a rejected apply.

5. **The regression test is the two-writers-one-manager case.** ADR-0015's
   e2e proves two *different* managers coexist. The case that regressed is two
   writers sharing *one* manager, so the suite pins both: shared manager
   erases, per-writer managers coexist. Unit tests cannot reach this; it is a
   property of the apiserver's merge, so it belongs in the `#[ignore]`d e2e
   that runs against a real cluster.

## Consequences

**Easier.** A provider's row is owned by that provider and nothing else can
quietly remove it. The number of writers per class stops being a correctness
concern, which is what ADR-0003 and ADR-0060 both assumed was already true.
Dropping `.force()` turns the next ownership mistake into a loud conflict
instead of a silent loop.

**Harder / newly ruled out.**

- `managedFields` on a widely-referenced `VMImage` now carries one entry per
  Cloud Hypervisor host rather than one per class. That is the honest
  representation of who wrote what, but it is more metadata on a hot object,
  and it grows with the fleet.
- Two shapes of ownership now coexist deliberately: whole-set writers on a
  class manager, single-row writers on a scoped one. That is a real asymmetry
  to understand before adding a provider, and decision 1 states the test for
  which side a new provider falls on. The alternative, scoping everything,
  would mean the whole-set providers could no longer reclaim a row from a
  deleted `Provider`.
- Renaming or recreating a `Provider` orphans the fields its old manager
  owned. They are not removed, because nothing claims them any more. Operators
  deleting a `Provider` should expect its rows to persist until the aggregate
  reconciler prunes them, which is a follow-up this ADR does not solve.
- A rolling upgrade has both manager shapes in flight. The old class-scoped
  manager still owns rows written before the upgrade; the first apply under
  the new name adds a second entry rather than taking the first one over, so
  stale rows survive until something prunes them. Accepted deliberately: the
  alternative is keeping `.force()` through the transition, which is the
  mechanism being removed.

**CALM impact: none.** No node, relationship, interface or trust boundary
changes. This tightens *how* existing writers identify themselves on an
existing relationship; the set of writers and what they write is unchanged.
The ADR is recorded in the model's `adrs` list.

**Threat model impact.** Field-manager identity is an integrity control, not
just bookkeeping: before this change any writer holding a Cloud Hypervisor
host's credentials could erase every other host's readiness rows by applying
its own, and the erasure was indistinguishable from normal operation. With
`.force()` removed on that path the erasure now requires deliberately
impersonating another host's manager name, which is a loud conflict rather
than a silent takeover. That is an integrity and availability concern against
a shared object, so §6 and the mitigations table change and a full pass is
due.
