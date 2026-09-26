<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0064 — Artifact delivery to a host-resident provider: OCI registry, by digest

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Erick Bourgeois
- **Revisits:** the alternative [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md)
  deferred: "revisit if a future provider needs to consume the artifact from
  outside the build namespace/cluster". This is that provider.
- **Related:** [ADR-0015](0015-vmimage-status-merge-strategy.md)
  (`perProvider[]` ownership), [ADR-0020](0020-vsphere-per-zone-iso-import.md)
  (typed build artifact), [ADR-0028](0028-vmimage-template-deletion-lifecycle.md)
  (image deletion), [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md),
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md); roadmap 09 phase 4.

## Context

`banlieue-imagebuilder` leaves a `cloudImage` raw disk (or an ISO, for
`Deferred`) on a PVC in the imagebuild namespace (ADR-0010, ADR-0020).
Every consumer so far runs in the cluster and mounts that PVC in an import
Job. A Cloud Hypervisor provider runs on a host outside the cluster and
cannot mount a PVC.

Constraints that stay: bulk bytes never flow through a reconcile loop
(ADR-0011); a checksum mismatch fails closed (SEC-004); the host credential
reads no Secrets (ADR-0060 Decision 5).

Options:

1. **Push the artifact to an OCI registry; the host pulls it by digest.**
2. **A short-lived artifact server in the imagebuild namespace.** ADR-0010
   weighed and deferred this: one more service to run, authenticate, secure
   with TLS and expose outside the cluster.
3. **Let the host mount the storage** (NFS, RWX export). Not available on
   every cluster, and it exposes cluster storage to hypervisor hosts.
4. **Stream the bytes through the API server.** Violates the bulk-bytes
   rule and loads the control plane with multi-GB transfers.

## Decision

### 1. Option 1: OCI registry, content-addressed

Registries already exist wherever banlieue runs, hosts can already reach
them, and the protocol brings authentication, TLS, resumable transfer and
content addressing. The host pulls by **digest**, so integrity comes from
the protocol and needs no extra checksum.

### 2. The push is a Job in the imagebuild namespace

When a `VMImage` has a `cloud-hypervisor` source and its build artifact is
`Ready`, `banlieue-imagebuilder` creates a Job that mounts the artifacts PVC
read-only and pushes the file as a single-layer OCI artifact:

- `artifactType`: `application/vnd.banlieue.disk.raw.v1` or
  `application/vnd.banlieue.disk.iso.v1`;
- the layer is zstd-compressed;
- the manifest annotations name the `VMImage` and its generation.

The Job runs `banlieue imagebuilder push`, a first-party pure-Rust OCI
client, not `oras` or `skopeo`. The registry and push credentials are
imagebuilder configuration, a Secret in the imagebuild namespace, not
per-provider. The Job writes nothing to status; the reconciler reads the
Job's outcome, as the libvirt import does.

The resulting reference, `registry/repository@sha256:…`, is recorded on
`VMImage.status.buildArtifact`. It is provider-neutral, one per artifact,
and owned by the imagebuilder, per ADR-0015.

Pushed only when a host-resident source exists, so installs without Cloud
Hypervisor need no registry.

### 3. The pull runs as its own transient unit on the host

The provider starts `banlieue-ch-import-<vmimage-uid>.service` (ADR-0063),
which runs `banlieue provider cloud-hypervisor import`. That process:

- pulls the blob **by digest** only;
- decompresses while streaming;
- writes to `<storage-target>/images/<digest>.raw` through a temporary name
  and an atomic rename;
- exits.

The reconciler never touches the bytes. It reads the unit's result and
writes the provider's own `VMImage.status.perProvider[]` row (ADR-0015):
phase, digest and the storage classes it holds the image in. A digest
mismatch fails the unit, and nothing is renamed into place.

Registry pull credentials are in the host config (ADR-0062 Decision 4),
written at bootstrap. They are **not** read from a cluster Secret, which
keeps ADR-0060's "no Secret reads" intact.

### 4. The host image cache

- The cache is keyed by digest, so identical images deduplicate across
  `VMImage`s.
- A machine's OS disk is a reflink (`FICLONE`) of the cached image where
  the filesystem supports it, and a sparse copy otherwise. It is then grown
  (ADR-0062 Decision 3).
- Eviction keeps a configured number of unreferenced images, and never
  removes an image with live reflinked disks.
- `VMImage` deletion: the finalizer (ADR-0028) also waits for every host
  that listed the image to report it evicted.

## Consequences

**Positive**

- No new service to run or secure, and no inbound path to the cluster or to
  the host.
- Integrity comes from digest addressing, not from a checksum banlieue has
  to carry around.
- The same mechanism serves any future out-of-cluster provider.
- Registry mirrors and pull-through caches work for large fleets without
  banlieue changes.

**Negative / accepted costs**

- **A registry becomes a dependency** for Cloud Hypervisor installs.
  Accepted: it is scoped to installs that use a host-resident class.
- Images now sit in a registry as well as on a PVC, so registry retention is
  the operator's to configure. The image's `VMImage` annotations make that
  traceable.
- A first-party OCI client is new code, or a new dependency if an existing
  pure-Rust crate passes `cargo deny` and the maintenance check. That
  choice is made at implementation.
- Push credentials live in the imagebuild namespace, and pull credentials on
  each host.

**Follow-ups**

- Signature verification (cosign/sigstore) on pull, as its own ADR. Digest
  pinning gives integrity, not provenance.
- Threat model: the registry as a new trust boundary (TB-6), and credentials
  on the host.
- NetworkPolicy for the push Job's egress to the registry.
