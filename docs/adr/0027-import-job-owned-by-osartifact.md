# 0027 — Per-zone import Jobs are owned by the `OSArtifact`, not orphaned

## Status

Accepted — 2026-08-22. Implemented the same day: `BuildArtifactStatus.os_artifact_uid`
(`crates/banlieue-api`), `banlieue_provider_sdk::osartifact::owner_references`
(shared helper), and both providers' import-Job manifests setting it.

## Context

`banlieue-imagebuilder`'s `VMImage` reconciler
(`crates/banlieue-imagebuilder/src/reconciler/vmimage.rs::reconcile`) names
the `OSArtifact` it creates deterministically from the `VMImage`'s own name
(`os_artifact_name(&name)`) — stable across rebuilds, not per-generation. When
the live `OSArtifact` is stale or foreign (owner UID mismatch, or its spec no
longer matches the current build request — a source URL or checksum change,
for example), the reconciler deletes it and lets the next pass recreate it
fresh under the **same name** (lines 514–545). kairos-operator's own PVC
naming convention (`<osArtifactName>-artifacts`,
`artifacts_pvc_name`, line 417–419) means the rebuilt artifact's PVC is also
the **same name** as before — it has to actually finish deleting before
kairos-operator can create its replacement.

Each provider's per-zone import Job (`crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs::import_job_name`,
and the equivalent in `banlieue-provider-libvirt`) is *also* named
deterministically — but keyed on `(image, provider, failure_domain)` only,
with no dependency on which `OSArtifact` generation produced the PVC it
mounts. Every such Job:

- mounts the artifacts PVC (read-only) to do its upload/import work,
- sets `ttlSecondsAfterFinished: 86400` for eventual cleanup, and
- carries **no `ownerReference`** to the `OSArtifact` (or anything else) —
  confirmed by reading `create_import_job`'s Job manifest end to end in both
  providers.

Found live: retriggering a build (changing the `VMImage`'s source so the
`OSArtifact` is judged stale) deletes the old `OSArtifact`, which cascades to
its PVC via kairos-operator's own ownership of that PVC — but the *old*
import Job (and its Pod, still holding the PVC's mount) has nothing telling
it to go away. It sits until its 24-hour TTL fires or someone deletes it by
hand, and the PVC it's mounting sits in `Terminating` the entire time,
blocking kairos-operator from creating the replacement PVC the rebuilt
`OSArtifact` needs. The operator-facing symptom is a rebuild that appears
stuck indefinitely, with no error surfaced anywhere in `VMImage.status`.

This is not a hypothetical: it is the direct, reliable failure mode any time
an `OSArtifact` is deleted-and-recreated while a prior import Job for the
same `(image, provider, failure_domain)` still exists — which is exactly what
"retrigger a build that already imported once" does.

## Decision

**The per-zone import Job's `ownerReferences` points at the `OSArtifact` it
was created for — not the `VMImage`, and not left unset.**

The `VMImage` is the wrong owner: it is not deleted or recreated when a
rebuild is triggered, only its spec changes, so an `ownerReference` to it
would never fire garbage collection for this case. The `OSArtifact` is
exactly the object whose lifecycle we need the Job bound to — same
namespace (`ctx.build_namespace`, ADR-0016), and its identity (UID) changes
precisely when a rebuild replaces it.

1. **`BuildArtifactStatus` (`crates/banlieue-api/src/banlieue/vmimage.rs`)
   gains `os_artifact_uid: Option<String>`**, alongside the existing
   `os_artifact_ref` (name). `banlieue-imagebuilder` populates it from the
   live `OSArtifact`'s `metadata.uid` in `compute_build_artifact_status`,
   the same place `os_artifact_ref` is already set. Optional because it is
   only meaningful once the `OSArtifact` exists (mirrors `pvc_ref`, already
   optional for the same reason).

2. **A new shared helper, `banlieue_provider_sdk::osartifact::owner_references`**
   (both providers already depend on `banlieue-provider-sdk`; this is the
   only place either needs to know the `OSArtifact` GVK, so it lives there
   rather than being duplicated in each provider or pulled from
   `banlieue-imagebuilder`, which providers have no dependency on and which
   needs the full `ApiResource` for its `OSArtifact` `DynamicObject` `Api` —
   a different-shaped need). Given `artifact.os_artifact_ref` (name) and
   `artifact.os_artifact_uid.as_deref()`, it returns
   `Some(json!([{ "apiVersion": "build.kairos.io/v1alpha2", "kind":
   "OSArtifact", "name": ..., "uid": ... }]))`, or `None` when the uid is not
   yet known. Both providers' `build_import_job` set
   `metadata.ownerReferences` to this value directly — `None` serializes to
   JSON `null`, the same idiom the existing `tolerations` field in both Job
   builders already relies on for "omit this key." `blockOwnerDeletion`
   stays unset, matching the existing `OSArtifact`→`VMImage` owner
   reference's own rationale (`desired_os_artifact`'s doc comment): setting
   it needs `update` on the owner's `finalizers` subresource, RBAC neither
   controller otherwise needs. When `os_artifact_uid` is absent, the Job is
   created without an owner reference and picks one up on a later reconcile
   once the field is populated — fail-open on missing metadata, not a hard
   error.

3. **The 24-hour `ttlSecondsAfterFinished` is unchanged** — it remains the
   backstop for the case this ADR does *not* fix: a `Ready` artifact whose
   import Job completed successfully and is simply never rebuilt. Owner-ref
   GC and Job TTL now cover two different lifecycles (the artifact's, and
   the Job's own completion) instead of one mechanism being asked to do
   both.

### Sequence after this change

1. `VMImage` spec changes → `banlieue-imagebuilder` judges the live
   `OSArtifact` stale → deletes it.
2. Kubernetes garbage collection sees the old `OSArtifact`'s UID is gone and
   cascades: the provider's old import Job (owned by that UID) is deleted.
3. The old Job's Pod terminates, releasing its mount on the old PVC.
4. kairos-operator (owning the PVC itself, independently of banlieue) can now
   actually finish deleting it.
5. `banlieue-imagebuilder` recreates the `OSArtifact` (same name, **new**
   UID) → kairos-operator creates the replacement PVC (same name, now free).
6. The provider's reconciler finds no import Job for this
   `(image, provider, failure_domain)` (the old one is gone, not merely
   stale) and creates a fresh one, owned by the new `OSArtifact` UID.

No step here waits on the 24-hour TTL; the whole cycle proceeds at
reconcile-loop speed, gated only by how long the actual delete/create calls
take.

### Not covered by this ADR

- **`force.reimport`'s existing explicit-delete path**
  (`crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs:545-561`) is
  unaffected — it already deletes-then-recreates the Job itself when the
  `OSArtifact` (and PVC) haven't changed at all, which is a different
  scenario (rerun the same import, not rebuild the artifact) and has no PVC
  contention to fix.
- **Jobs already orphaned before this ships** — existing stale Jobs from
  before the owner reference existed have no owner and still rely on their
  24h TTL (or manual `kubectl delete job`) to clear. This ADR only prevents
  the problem going forward.
- **libvirt's own re-trigger behavior** beyond adding the same owner
  reference — libvirt's importer, unlike vSphere's, has no existing
  `force.reimport`/explicit-delete path at all; adding one, if wanted, is
  separate work.

## Consequences

- Retriggering a `VMImage` build whose `OSArtifact` needs to be rebuilt no
  longer leaves the artifacts PVC stuck `Terminating` behind a Job with
  nothing telling it to go away — the stale Job disappears as an automatic
  side effect of the `OSArtifact` delete already being on the reconcile's
  critical path, so no proactive/eager deletion logic needs to be written in
  either provider.
- `crates/banlieue-api` — a source-of-truth CRD schema change
  (`BuildArtifactStatus.os_artifact_uid`) — requires regenerating
  `deploy/crds/banlieue.io_vmimages.yaml` (`regen-crds` skill) before this
  can ship.
- A completed import Job whose `OSArtifact` is still `Ready` and untouched
  keeps existing exactly as it does today (owner reference doesn't fire;
  TTL still applies) — this ADR changes behavior only on rebuild, not on the
  steady-state `Ready` path.
