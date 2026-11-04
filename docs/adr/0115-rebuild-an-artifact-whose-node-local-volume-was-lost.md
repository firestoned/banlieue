<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0115: Rebuild an artifact whose node-local volume was lost

- **Status:** Accepted
- **Date:** 2026-11-04
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md)
  (the imagebuilder) and its rebuild gate (`artifact_gate`, SEC-005);
  [ADR-0027](0027-import-job-owned-by-osartifact.md) (import Jobs are owned by
  the `OSArtifact`, so a rebuild also replaces them); touches
  [ADR-0016](0016-imagebuild-namespace-isolation.md) only in what the
  imagebuilder may read

## Context

A build's output, the ISO or disk image, lives on the `OSArtifact`'s artifacts
PVC. On a cluster whose only StorageClass is `local-path` that PVC is a
directory on **one worker's disk**, and its PV is pinned to that worker by
`nodeAffinity`.

When the worker is rebuilt, the node comes back under the **same name** with an
empty disk. Nothing in Kubernetes notices: the PV and PVC are still `Bound`, the
`OSArtifact` still reports `phase: Ready`, and the imagebuilder mirrors that
`Ready` into `VMImage.status.buildArtifact`. Observed on the dev cluster on
2026-11-04 after every node was rebuilt:

- six `OSArtifact`s `Ready`, six PVCs `Bound`, all six ISOs gone;
- every import Job failing with `cannot read /artifacts/<image>-build.iso:
  No such file or directory`, retried every ten minutes (ADR-0020 amendment
  2026-10-23) and never succeeding;
- the image the sandbox pools clone from only looked healthy because its old
  import Jobs had completed and were being adopted. It would have failed the
  same way the next time a Job was created.

The imagebuilder's gate (`artifact_gate`) rebuilds an `OSArtifact` that is
foreign or whose spec no longer matches. It has no notion of the artifact's
**data** having disappeared, and the controller cannot look inside a volume it
does not mount.

## Decision

1. **A volume is lost when its node was replaced after the volume was made.**
   For the artifacts PVC the gate resolves the bound PV and reads the node it is
   pinned to (`nodeAffinity` on `kubernetes.io/hostname`, `In`, one value). The
   volume is **lost** when no `Node` with that hostname label has a
   `creationTimestamp` at or before the PV's own `creationTimestamp`. That
   covers both a node that is gone and a node that came back as a new object:
   either way, whatever the PV held on that disk is not there.
2. **A lost volume is a stale artifact.** `LiveArtifactView` gains
   `volume_lost`, and `artifact_gate` returns `DeleteForRebuild` for it. The
   existing path then does the rest: foreground delete of the `OSArtifact`, wait
   for it and its PVC to go (`AwaitTeardown`), recreate. The import Jobs, owned
   by the `OSArtifact`, are replaced with it.
3. **Only what the cluster can show decides.** A PV with no node affinity (a
   network volume), a PVC not yet bound, or a PV with no readable hostname is
   **not** lost; the check never guesses.
4. **A failed read is not a loss.** If the PV or Node read fails (including
   `403` because the site has not yet applied the new ClusterRole), the
   reconciler logs a warning and behaves exactly as before this ADR. An upgrade
   never starts deleting artifacts because a permission is missing.
5. **New read-only RBAC, cluster-scoped, nothing else.**
   `deploy/imagebuilder/rbac/clusterrole.yaml` gains `get` on
   `persistentvolumes` and `list` on `nodes`. Both objects are cluster-scoped,
   so a namespaced Role cannot grant them. Neither verb writes anything and
   neither returns Secret content.

The rebuild converges: the new PVC's PV is created after every existing node,
so the next reconcile finds it not lost.

## Consequences

- A rebuilt worker costs one rebuild (about eight minutes) per image whose PV
  lived on it, with no operator action. The status message says the artifact
  was replaced because its volume was lost.
- **False positive, accepted.** A worker whose `Node` object is deleted and
  re-registered keeps its disk but gets a new `creationTimestamp`. Its artifacts
  are rebuilt unnecessarily. The cost is bounded, and the opposite error (an
  ISO that is gone while `Ready` is reported) is what this ADR exists to stop.
- **Not covered:** a file deleted from a volume on a node that was **not**
  replaced, and any storage with no node affinity. A probe Job that checks the
  file would cover both at the cost of a pod per check; it was not chosen as the
  default and can be added behind this gate later. Storage that survives node
  loss removes the cause and is still the better fix where the site can
  provide it.
- The imagebuilder's identity now reads two cluster-scoped kinds. The
  threat-model pass for this ADR records that.
- `OSArtifact`s are deleted by this path only when the cluster itself shows the
  disk was replaced, and the `VMImage` is rebuilt into the same name, so a
  consumer sees `Ready` go away and come back rather than a vanished image.
