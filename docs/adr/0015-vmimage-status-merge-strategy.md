# 15. VMImage.status merge strategy and ownership of aggregate readiness

Date: 2026-07-31

## Status

Accepted

## Context

ADR-0010 split `VMImage.status` across field managers: `banlieue-imagebuilder`
owns `rawDiskArtifact`, each provider owns its own `perProvider[]` entry. The
split was designed to keep server-side apply conflict-free.

It does not work, and this was reproduced against a real apiserver
(`crates/banlieue-provider-libvirt/tests/e2e_vmimage_ssa.rs`). Applying as
`banlieue.io/provider-vsphere` and then as `banlieue.io/provider-libvirt`
yields:

```
perProvider rows  : ["kvm-1"]                                  # vSphere's row: gone
conditions        : [("Ready","False","libvirt importing")]    # vSphere's condition: gone
rawDiskArtifact   : Some(Ready)                                # survived
```

**Why.** `perProvider` and `conditions` are plain arrays carrying no
`x-kubernetes-list-type`. Server-side apply therefore treats them as **atomic**:
the array is a single leaf field owned by one manager. Each provider applies the
whole list containing only its own rows, with `force()`, so it takes ownership
and discards the other's. `rawDiskArtifact` survives only because it is a
distinct key and is `skip_serializing_if = "Option::is_none"`, so a provider's
`None` is omitted rather than nulled.

The split is right; the schema does not implement it.

This is not a corner case. `examples/04-vmimage-ubuntu.yaml` ships sources for
`vsphere`, `proxmox` *and* `libvirt` — the documented way to describe one
abstract image available on several backends. Two providers reconciling it flip
the object back and forth on every reconcile.

The hazard was already known for `Provider`: `provider.rs` documents that
`conditions` is atomic and gives the operator a disjoint field (`workload`) to
avoid contention (ADR-0012). That workaround cannot apply here, because the
providers genuinely share one list.

A second, independent problem sits on top of it. Every provider computes the
aggregate `Ready` condition from **only its own rows** — `aggregate_ready` sees
the rows that provider just built, not the whole list. Even with merging fixed,
both providers would still write `conditions[type=Ready]` from partial data.
Fixing the merge without fixing ownership would replace a visible flip-flop with
a subtler wrong answer.

What actually consumes this status matters for the blast radius:

- `banlieue-controller` reads `perProvider[].ready` and `.resolvedRef` for
  scheduling and infra-CR construction. It never reads `conditions`.
- The `Ready` **printcolumn** (`kubectl get vmimage`) reads
  `conditions[?(@.type=='Ready')].status`.

So `conditions` on a `VMImage` is a human-facing summary today, not a
machine-readable contract. Nothing schedules on it.

## Decision

**1. Make the lists merge per entry.**

- `status.perProvider` → `x-kubernetes-list-type: map`, keyed on
  `["providerName", "providerNamespace"]`. Both keys, because Providers are
  namespaced and two namespaces may legitimately hold the same name.
- `status.conditions` → `x-kubernetes-list-type: map`, keyed on `["type"]`.
  This is the standard Kubernetes convention for condition lists and is what
  `metav1.Condition` assumes.

With this, each provider owns only the entry it applies. A manager removing its
own entry removes only that entry.

**2. Providers stop writing `VMImage.status.conditions`.**

A provider knows about itself. It cannot compute "ready everywhere" without
reading rows it does not own, and a status field whose value depends on data
outside the writer's ownership is a field with the wrong owner.

Providers continue to own their `perProvider[]` entry, which is where all
per-provider detail (readiness, zones, reason, message) already lives.

**3. `banlieue-controller` owns the aggregate `Ready` condition.**

It is the only component with a legitimate whole-image view: it already watches
and reads every `VMImage` for scheduling, and already holds
`vmimages/status: update,patch` in its ClusterRole. It writes under its own
field manager (`banlieue.io/controller`), disjoint from every provider's.

`Ready` means **ready on every provider that has reported**, matching the
existing documented semantics ("`Ready` is True iff every per-provider entry is
ready") and the printcolumn's meaning. `Unknown` while no provider has reported.

## Consequences

- The flip-flop is gone, and `kubectl get vmimage` shows a `Ready` derived from
  all providers rather than from whichever wrote last.
- `perProvider` merging is now a schema guarantee, not a convention. A provider
  that stops reconciling leaves its entry behind until it removes it — correct,
  and the same lifecycle every merge-keyed list has.
- The controller gains a `VMImage` watch. It is a cheap one: the reconcile is
  pure aggregation with no backend calls.
- **`listMapKeys` must be required fields with no defaults**, or the apiserver
  rejects the schema. `providerName` and `providerNamespace` already are.
- Changing a list's `x-kubernetes-list-type` on an established CRD is not
  free — the apiserver re-derives ownership on next apply. At v1alpha1 with no
  installed base this is a non-issue; noted so a future version bump does not
  repeat it casually.
- **The same annotation belongs on every other `conditions` list in the API.**
  They are single-writer today, so the bug is latent rather than live, and
  `Provider` additionally has the ADR-0012 disjoint-field workaround. Applying
  the annotation everywhere is tracked as follow-up rather than bundled here,
  to keep this change reviewable.

## Alternatives considered

**Have each provider recompute `Ready` from the full merged list.** Once
`perProvider` merges, each provider could read the whole list and derive the
same value, converging. Rejected: two managers still own one key, so they churn
`lastTransitionTime` whenever they observe different intermediate states, and
correctness depends on every provider agreeing on the aggregation rule forever.
Single ownership is simpler and cannot drift.

**Give each provider a distinct condition type** (`VsphereReady`,
`LibvirtReady`). Merges cleanly, but `kubectl wait --for=condition=Ready` stops
working and the printcolumn has nothing to show. It also pushes aggregation onto
every consumer.

**Drop the aggregate condition entirely** and let the printcolumn render
`perProvider[*].ready`. Honest, and the smallest change, but it renders as
`true,false` and removes a standard condition consumers reasonably expect.
