<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0064 — Artifact delivery to a host-resident provider: OCI registry, by digest

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-09-26 (Decision 5 added: `BackingFile` sources, the
  first slice, served before the registry path exists); 2026-09-26
  (Decisions 1 to 4 implemented: gzip rather than zstd, a first-party
  client, the host pins its repository, the controller holds the deletion
  finalizer; see *Implementation notes*)
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
- the layer is zstd-compressed (built as gzip; see *Implementation notes*);
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

### 5. `BackingFile`: an image already in the host cache

*Added 2026-09-26.* Before any of the above is built, a `VMImage` source
with `providerClass: cloud-hypervisor` and `kind: BackingFile` names a file
an admin has already put in a storage class's cache,
`<storage class dir>/images/<name>`. The provider publishes its
`perProvider[]` row as ready, `resolvedRef` the file name, when at least one
storage class on the host holds it; otherwise `ImageNotFound`, requeued so
a copy made later is picked up. The reference's last path component is the
name, as on libvirt, and it must be a plain file name. The row names storage
classes, never the directory (ADR-0062 Decision 4). (Until Decisions 1 to 3
landed, a `Url` source was reported `UnsupportedSourceKind`.)

This is the libvirt provider's `BackingFile` semantics on the same cache
layout Decision 4 defines, so the registry path, when it lands, writes into
the directory this already reads from. Implemented in
`crates/banlieue-provider-cloud-hypervisor/src/vmimage.rs`.

## Implementation notes (2026-09-26)

Decisions 1 to 4 as built, and where they differ from the text above.

- **gzip, not zstd.** The layer is gzip (`application/vnd.banlieue.disk.v1+gzip`)
  through `flate2`'s default `miniz_oxide` backend, which is pure Rust. The
  widely used `zstd` crate binds the C library through `zstd-sys`, which
  roadmap 09 rules out ("no new native dependency"). gzip is OCI's default
  layer compression, and a raw disk is mostly zeros: a 16 MiB test disk
  pushes as a 16 KiB layer.
- **First-party client, `crates/banlieue-oci`.** The consequence above left
  the choice open; an existing crate such as `oci-client` was the
  alternative. The subset used is small: push two blobs and a manifest,
  pull a manifest and a blob by digest, and answer Basic and Bearer
  challenges. Built first-party, the only new dependency is `flate2` with
  its pure-Rust backends (`miniz_oxide`, `zlib-rs`, and small checksum
  crates); `cargo deny check` passes. The client is protocol only (no
  `kube`), like `banlieue-libvirt` and `banlieue-cloud-hypervisor`. A pull
  streams through gzip into a sparse writer (zero blocks become holes),
  verifies the layer digest, and renames into place; a mismatch leaves
  nothing. The layer carries its uncompressed length
  (`io.banlieue.disk.size`, covered by the manifest digest), and a pull
  that would decompress to more, or ends at less, is refused. There is no implicit Docker Hub: a reference must name its
  registry.
- **Where the reference is recorded.** `status.buildArtifact.ociArtifact`
  (`phase` `Pushing|Ready|Failed`, `reference`, `message`), so a push in
  progress or failed is visible rather than only its result.
- **How the Job reports.** The push Job has no ServiceAccount token. Its
  only output is the digest-pinned reference, written as its termination
  message; the imagebuilder reads it from the pod and accepts it only if it
  names the configured repository by digest. Each build is tagged by its
  `OSArtifact` uid, so a rebuild never finds the previous build's tag. The
  Job is owned by the `OSArtifact` (ADR-0027) and has no
  `ttlSecondsAfterFinished`, so the message outlives the reconcile that
  reads it. The imagebuilder's build-namespace Role gains `jobs`
  get/create/patch/delete and `pods` list. Configuration:
  `--registry-repository`, `--registry-credentials-secret` (a
  `kubernetes.io/basic-auth` Secret), `--registry-plain-http`,
  `--push-image`.
- **The host pins its repository.** The host config gains an optional
  `[registry]` section: `repository`, `credentials_dir` (the same
  `username`/`password` files), `plain_http`. The host pulls a reference
  only if it is a digest in that one repository. `VMImage` status is written
  in the cluster; the host owner, not the cluster, chooses where images come
  from. The import re-checks its own command line against the config too.
- **Every storage class.** The image is pulled once and reflinked (or
  copied) into every storage class's cache, and the row is ready only when
  all of them hold it, since a machine may choose any class. The cache file
  is `sha256-<hex>.raw`.
- **The unit.** `banlieue-ch-import@<vmimage uid>.service` (a template
  instance since ADR-0063's 2026-09-27 amendment), the provider's
  own user, hardened like a guest unit and writable only in the image
  caches. The polkit rule widens to `banlieue-ch-(import-)?<uuid>.service`.
  A failed unit is reported `ImportFailed` with systemd's reason, then
  reset so the next reconcile retries.
- **Decision 4, eviction.** A pulled file is *referenced* while this
  host's row of a live `VMImage` resolves to it or one of its machines
  boots from it. After each successful import the host deletes
  unreferenced pulls beyond the newest `[registry] keep_unreferenced`
  (default 1, for a quick rollback). Admin-placed `BackingFile` files are
  never touched. "Never remove an image with live reflinked disks" holds
  by construction: an OS disk is its own file (a reflink or a sparse
  copy), so removing the cache file cannot affect it; what is protected is
  an image a machine still names, which it would need to be recreated.
- **Decision 4, deletion.** The finalizer is `banlieue.io/host-image-cache`,
  held by **banlieue-controller**, not by each host: a per-host finalizer
  needs `patch` on `vmimages`, which also reaches the spec, and a stolen
  host token must not be able to rewrite what every provider builds. The
  controller adds it to images with a `cloud-hypervisor` `Url` source. On
  deletion each host stops any import, removes its cache file unless
  something else there uses it, and reports `reason: Released`; the
  controller drops the finalizer once every existing Cloud Hypervisor
  Provider with a row has. A Provider that no longer exists is not waited
  on; a host that is down holds deletion until it returns, or until an
  admin removes the finalizer by hand.

**Verified live 2026-09-27:** a 20 GiB image pushed, pulled by digest in
62 s (sparse), booted by `make ch-e2e`, and released on `VMImage`
deletion with the controller's finalizer.

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
