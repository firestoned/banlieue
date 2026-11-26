<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0142: A microvm image is one OCI artifact of kernel, initramfs and root filesystem, pulled by digest and shared read-only

- **Status:** Accepted
- **Date:** 2026-11-19
- **Proposed:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0137](0137-firecracker-provider-class.md) (Decision 4 left
  this contract here), [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md)
  (OCI delivery, the host pins its repository, the cache),
  [ADR-0108](0108-sandbox-guest-software-is-consumer-image-content.md)
  (guest software is the consumer's image content, a template carries no per-VM
  identity), [ADR-0054](0054-nocloud-seed-iso-first-party.md),
  [ADR-0015](0015-vmimage-status-merge-strategy.md),
  [ADR-0140](0140-firecracker-guests-are-supervised-by-systemd-not-the-jailer.md),
  [ADR-0141](0141-shared-host-provider-layer.md)

## Context

A Cloud Hypervisor image is **one raw disk**: the imagebuilder produces it, a
Job pushes it to an OCI registry as a single-layer artifact
(`application/vnd.banlieue.disk.raw.v1`), the host pulls it by digest into a
per-storage-class cache, and each machine gets its own reflinked, grown copy
(ADR-0064 Decisions 2 to 4).

A Firecracker guest cannot boot a disk. It boots a **kernel** the VMM loads
directly, usually with an **initramfs**, over a **root filesystem** the kernel
mounts (ADR-0137 Decision 4). Three things follow from the sandbox image that
the spike built (a monolithic kernel of about 44 MB, a 2 MB initramfs, a
read-only squashfs root of about 100 MB, mounted with a tmpfs overlay):

- the three files must travel and be verified **together**: a kernel and a root
  filesystem that were not built as a pair are a boot failure at best;
- the root filesystem is read-only, so no per-guest disk is needed at all;
- the image comes from the consumer's own build (ADR-0108), not from
  banlieue's imagebuilder, so there is no `OSArtifact` and no PVC to push from.

What exists today does not fit: `banlieue-oci` accepts a manifest with
**exactly one gzip layer** and rejects anything else (`manifest.rs`), and
`ImageSourceKind` is `Template`, `BackingFile` or `Url`, none of which names an
OCI artifact.

## Decision

### 1. One OCI artifact per image, three layers, one digest

An image is a single OCI image manifest with
`artifactType: application/vnd.banlieue.microvm.v1`, an
`application/vnd.banlieue.microvm.config.v1+json` config blob, and layers in
this fixed order:

| Layer | Media type | Stored as | Required |
| --- | --- | --- | --- |
| kernel | `application/vnd.banlieue.microvm.kernel.v1+gzip` | the uncompressed `vmlinux`, gzip-compressed | yes |
| initramfs | `application/vnd.banlieue.microvm.initramfs.v1+gzip` | the cpio archive, gzip-compressed | no |
| root filesystem | `application/vnd.banlieue.microvm.rootfs.v1` | the file as built, **not** recompressed (a squashfs is already compressed) | yes |

The manifest digest identifies the image. Because kernel, initramfs and root
filesystem are layers of one manifest, a pull gets a matched set or nothing, and
the pair is the image owner's build output, not something an operator
assembles. Each gzip layer carries its uncompressed length in the annotation
`io.banlieue.layer.size` (covered by the manifest digest), as ADR-0064's disk
layer does with `io.banlieue.disk.size`.

The config blob (schema version 1) carries: `architecture`
(`amd64` or `arm64`), `cmdline` (the image's own kernel arguments, Decision 5)
and `minMemoryMiB` (the least guest memory the image boots in, so the machine
spec is refused rather than failing in the guest). Nothing else; a field is
added by amending this ADR.

The root filesystem's format is not interpreted by the provider. The contract
is that the kernel can mount it as-is, read-only, from `/dev/vda`.

### 2. The `VMImage` names it with a new source kind, `OciArtifact`

`ImageSource` gains `kind: OciArtifact`, whose `ref` is
`<repository>@sha256:<manifest digest>`, for `providerClass: firecracker`. It is
a new, explicit kind (non-negotiable 4) and not `Url`: an OCI reference is not a
URL, and `Url` is defined by `importFrom` plus a checksum, where here the digest
is the integrity.

The trust rule of ADR-0064 is unchanged and is what makes a cluster-written
reference safe: the **host pins its repository**, pulls only a digest in that
repository, and re-checks its own command line. The cluster says which image; the
host owner says where images may come from.

This is a change to the shared `VMImage` CRD (code-first, regenerated CRDs,
examples, API docs). The existing kinds are unchanged.

### 3. The host cache holds a directory per image

The cache entry is a directory keyed by the **manifest** digest, in the storage
class's image area, not a single file:

```
images/sha256-<manifest hex>/
    kernel          (decompressed, verified)
    initramfs       (decompressed, verified; absent if the image has none)
    rootfs          (verified)
    boot.json       (the config blob, verbatim)
```

The pull writes into a temporary directory and `rename`s it into place only
after every layer's digest and size has been verified, so nothing partial is
ever visible (ADR-0064 Decision 3). The pull is its own transient unit, as for
Cloud Hypervisor; the reconciler never touches bytes. A digest or size mismatch
fails the unit and nothing is renamed.

Permissions are different from Cloud Hypervisor's. A Cloud Hypervisor image is
cloned per guest and the cache is private to the provider. A Firecracker guest
reads the shared files **as its own uid**, so the image area is traversable
(`0711`) and each image's directory and files world-readable (`0755`, `0644`).
That is acceptable because an image holds no secret by contract: a template
carries no per-VM identity and secrets arrive over vsock or the seed (ADR-0108,
ADR-0137 Decision 6). It is recorded as an accepted risk: any local account that
knows an image's digest can read it.

### 4. No per-guest disk: the root drive is the shared image, read-only

The guest's root drive is the cached `rootfs`, attached read-only
(`is_root_device`, `is_read_only`), and **the same file backs every guest of the
image**. Writes go to the guest's RAM overlay (ADR-0137, Decision 4). So:

- there is nothing to clone, grow or delete per guest, unlike Cloud Hypervisor
  (ADR-0064 Decision 4); the host page cache holds one copy of the root for all
  of them;
- a guest's memory must cover its overlay as well as its working set, which is
  why the image declares `minMemoryMiB`;
- a guest that needs storage that survives it is not a workload of this class.

The only other drive is the NoCloud seed, attached second and read-only, and
only when the machine has user data (ADR-0054), as ADR-0137 states.

**Eviction** follows ADR-0064 Decision 4, with one difference that matters here:
a Cloud Hypervisor guest owns its disk, while a Firecracker guest depends on the
cache file for its whole life. An image is *referenced* while this host's row of
a live `VMImage` resolves to it **or any guest unit on the host maps it**, and a
referenced image is never evicted. (On Linux an unlinked file stays readable to
a running VMM; eviction still avoids it, so disk space is not silently held and
a restart is never refused.)

### 5. The kernel command line is composed by the provider

The command line is `platform arguments`, then the image's `cmdline`, and
nothing from the cluster:

- the **platform arguments** are the provider's and cannot be overridden:
  `console=ttyS0`, `reboot=k`, `panic=1`, `pci=off`, `root=/dev/vda`, `ro`, and
  `ip=` (the static address, ADR-0137 Decision 5);
- the image's `cmdline` is the image owner's: logging, systemd and the like. An
  image whose `cmdline` sets any platform argument is **refused at pull**, with
  the offending argument named;
- the machine spec has no field for kernel arguments (explicit over implicit):
  the owner of the image, not a pool user, decides them.

### 6. The pull fails closed

The pull unit refuses the image when any of these hold: the manifest is not
exactly this artifact type; a layer's media type is unknown or out of order; a
required layer is missing; the config blob is not schema version 1;
`architecture` is not the host's; a gzip layer decompresses to more or less than
its annotated length; a layer exceeds the host's per-layer ceiling (a host-config
value, set at implementation); a digest does not match. A refused image reports
its reason in the host's `perProvider[]` row (`ImportFailed`, ADR-0064), and the
unit is reset so the next reconcile retries, as for disks.

### 7. One first-party tool writes and reads the format

The format has one implementation: `banlieue-oci` grows multi-layer push and
pull (today it requires exactly one gzip layer), used by both the host's pull and
a first-party push subcommand that takes the three files, the `cmdline`, the
architecture and the minimum memory, and pushes to a repository. The sandbox
repository's image build already produces exactly the three files
(`vmlinux`, `initramfs.cpio`, `rootfs.squashfs`); its pipeline runs the push, so
banlieue's imagebuilder (and its `OSArtifact` and PVC) is not involved, and
`VMImage.status.buildArtifact` is not used for this class. The host's
`perProvider[]` row reports the resolved manifest digest.

### 8. Not in v1

No signature verification beyond the digest pin and the host's pinned
repository: a signature scheme (for example cosign) is a later ADR. No
per-image kernel selection at machine level. No images with more than one root.

## Consequences

- A Firecracker image is deployed by one reference and one digest; a mismatched
  kernel and root filesystem cannot be scheduled.
- Storage and clone time per guest are zero: this is a large part of the density
  and cold-start advantage ADR-0137 wants, bought with a read-only root and a RAM
  overlay.
- ADR-0137 said a pool member's disk is "a per-member copy deleted at release".
  That is wrong for this design and is corrected there.
- `banlieue-oci` and the host's import need real work (multi-layer; a directory
  cache entry). ADR-0141's open question, whether `import` generalises over
  artifact kinds, is answered: the **client** is shared, the **import unit** is
  per class.
- A new enum variant in a shared CRD: regenerate CRDs, examples and the API
  reference; the existing providers ignore the new kind.
- Threat model: a new artifact format parsed on the host (bounded by the size
  annotations, the per-layer ceiling and the digest), a world-readable image
  cache (accepted, above), and an image owner who controls the guest's kernel
  arguments but not the platform ones. Pass due when implemented.
- Not implemented. Open: the per-layer ceiling's default; the exact config
  schema's field names; whether an `arm64` image is built before an `arm64`
  host exists.
