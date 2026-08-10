# 0020 — vSphere per-zone image import via ISO template creation

## Status

Proposed — 2026-08-08. Extends [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md)
(the `banlieue-imagebuilder` build/import split), [ADR-0015](0015-vmimage-status-merge-strategy.md)
(per-provider status ownership), and [ADR-0016](0016-imagebuild-namespace-isolation.md)
(imagebuild namespace). Builds on [ADR-0008](0008-byoc-vsphere-http-client.md)
(BYOC vim client) and mirrors [ADR-0011](0011-libvirt-provider-own-client.md)'s
per-target import-Job pattern.

## Context

ADR-0010 established that a `Url`-kind `ImageSource` is built once, provider-
agnostically, into a shared artifact (`VMImageStatus.rawDiskArtifact`) by
`banlieue-imagebuilder` driving a kairos-operator `OSArtifact`, and then
*imported per zone* by the owning provider. `ZoneImageStatus` already exists on
`ImagePerProviderStatus.zones` for exactly this. Iter-1 landed the build half;
the vSphere import half was a stub — `Url`-source `perProvider` entries report
`PerZoneImportNotImplemented` and never go ready, which is the observed "all
error or empty `status.perProvider`" for `rhel98-kairos-url`.

The maintainer already has a proven, hand-run flow for turning an OCI Kairos
image into a per-cluster vCenter template, in two scripts:

1. **build-iso** — `auroraboot build-iso` from the OCI image, baking a default
   cloud-config (`--cloud-config`), producing a bootable install ISO; the ISO is
   uploaded to one datastore and copied to the others.
2. **create-template** — per compute cluster: create an empty EFI VM
   (`pvscsi`, `vmxnet3`, matching guestId), attach the ISO as a CD-ROM, then
   `MarkAsTemplate`. No Content Library is used (it is not enabled in the target
   environment).

Two facts make this cleanly decomposable onto the existing pipeline:

- **kairos-operator already does the whole build side.** `OSArtifact` with
  `artifacts.iso: true` runs `auroraboot --debug build-iso`; when
  `artifacts.cloudConfigRef` (a `SecretKeySelector`) is set it mounts the Secret
  and passes `--cloud-config /cloud-config.yaml` — i.e. it bakes a default
  cloud-config into the ISO exactly as the build-iso script does. The output
  lands in the artifacts PVC, as the raw-disk build already does. So the ISO
  build needs **no** new banlieue code — only a different `artifacts` selection
  and a `cloudConfigRef`.
- **The import side is the libvirt pattern applied to vCenter.** ADR-0011's
  `banlieue-provider-libvirt` already fans a build artifact out to targets with
  one import Job per target, the Job mounting the shared artifacts PVC and
  running the banlieue binary's own import subcommand. The vSphere provider can
  reuse that shape: one Job per failure domain, uploading the ISO from the PVC
  to that zone's datastore and creating the template via the BYOC vim client.

This ADR records how those pieces compose, and the one API change they require:
the shared build artifact must become **typed** so the same status field can
carry either a raw cloud image (libvirt) or an ISO (vSphere), named to match
kairos-operator's own artifact vocabulary.

## Decision

### 1. Typed build artifact (replaces `rawDiskArtifact`)

Generalise the raw-disk-specific status into a **typed build artifact**, using
kairos-operator's own artifact kinds so the name is not banlieue-invented:

- `VMImageStatus.rawDiskArtifact` → `VMImageStatus.buildArtifact`.
- `RawDiskArtifactStatus` → `BuildArtifactStatus`, gaining
  `kind: BuildArtifactKind`.
- `RawDiskArtifactPhase` → `BuildArtifactPhase` (unchanged 4-state subset:
  `Pending | Building | Ready | Failed`).
- `BuildArtifactStatus.diskFile` → `file` (the artifact filename in the PVC:
  `<osArtifactRef>.raw` for a cloud image, `<osArtifactRef>.iso` for an ISO).

`BuildArtifactKind` mirrors kairos-operator `OSArtifactKind`:

| `BuildArtifactKind` | kairos `OSArtifactKind` | `artifacts.*` requested | consuming provider class |
| --- | --- | --- | --- |
| `cloudImage` | `cloudImage` | `cloudImage: true` (raw) | libvirt |
| `iso` | `iso` | `iso: true` (`build-iso`) | vsphere |

One `VMImage` produces one build artifact; its `kind` is chosen by
`banlieue-imagebuilder` from the `Url` source's `providerClass` (vsphere → `iso`,
libvirt → `cloudImage`). This is pre-1.0 `v1alpha1`; the rename is a code-first
CRD change (regen), and the two in-tree consumers (`banlieue-imagebuilder`
writer, `banlieue-provider-libvirt` reader) migrate in the same change.

### 2. Image-level default cloud-config on `VMImage`

Add `VMImageSpec.cloudConfig`, a baked-in default cloud-config for the built
artifact, resolved by `banlieue-imagebuilder` and passed to the `OSArtifact` as
`artifacts.cloudConfigRef`. It mirrors the existing `CABundleSource`
(`crates/banlieue-api/src/common.rs`) — the same inline / `configMapRef` /
`secretRef` `KeySelector` shape with an "exactly one of" `validate()` — but is
**implemented `secretRef`-first**:

```rust
pub struct CloudConfigSource {
    /// Cloud-config from a Secret key (`KeySelector`). Implemented now.
    pub secret_ref: Option<KeySelector>,
    // Future (this ADR reserves the shape, does not implement):
    //   inline: Option<String>              -> materialise a derived Secret
    //   config_map_ref: Option<KeySelector> -> materialise a derived Secret
}
```

Because kairos-operator's `cloudConfigRef` is **Secret-only** (its own
`SecretKeySelector`), `banlieue-imagebuilder` maps this `KeySelector` onto it.
The eventual inline/configMap variants resolve by materialising an
imagebuilder-owned derived Secret in the imagebuild namespace and pointing
`cloudConfigRef` at it; for now `secretRef` passes straight through. It is
image-level (not per-source) because the cloud-config is baked into the single
shared build, not chosen per backend.

### 3. Content Library toggle on `Provider.spec` (default off)

`ProviderSpec` is flat and class-generic (no nested `vsphere` struct), so the
toggle is a top-level optional `Provider.spec.useContentLibrary: Option<bool>`
(vsphere-only semantics, documented; ignored by other classes), defaulting to
`false`. Default (false) is the datastore-upload + empty-VM + `MarkAsTemplate`
path, matching the environment where Content Library is not enabled. When true
(future), the provider imports the ISO/OVF into a named Content Library instead.
This ADR implements only the default path; the field exists so enabling CL later
is a spec change, not a schema migration.

### 4. vSphere per-zone import (the "push")

`banlieue-imagebuilder` orchestrates the build and hands off via status; the
**vSphere provider does the push**, one failure domain at a time, reusing the
ADR-0011 import-Job shape:

1. Gate: act only when `VMImageStatus.buildArtifact.phase == Ready` and
   `kind == iso`.
2. For each `Provider.status.failureDomains[]`, ensure one import Job
   (`imageImport` subcommand of the banlieue binary), in the imagebuild
   namespace, mounting the shared artifacts PVC read-only. Idempotent, keyed by
   `(vmimage, failureDomain)`; verifies the ISO against
   `buildArtifact.checksum` and fails closed on mismatch (SEC-004, per ADR-0010).
3. The Job, via the BYOC vim client (ADR-0008), performs the create-template
   flow against that zone's datacenter/cluster/datastore/network:
   datastore-upload the ISO → create an empty EFI VM (`pvscsi`, `vmxnet3`,
   image `guestId`) → attach the ISO as CD-ROM → `MarkAsTemplate`.
4. The provider writes `ImagePerProviderStatus.zones[]` (`ZoneImageStatus`,
   already defined): `ready: true` + `resolvedRef` (the template's
   `[datastore] folder/name`) per zone, owning only its own `perProvider` row
   (ADR-0015). `banlieue-controller` still owns the top-level `Ready` condition.

No RPC is introduced — the handoff is PVC + status only. The provider never
builds; the imagebuilder never touches vCenter.

## Consequences

- **`Url`-source vSphere images become usable.** Per-zone templates are created
  from a URL image with a baked default cloud-config, closing the iter-1 stub.
- **One status field, two artifact kinds.** `buildArtifact.kind` lets libvirt
  (raw) and vSphere (ISO) share the pipeline without parallel fields. The rename
  is a breaking `v1alpha1` status change; regen CRDs, update the imagebuilder
  writer + libvirt reader + all tests/examples in the same change.
- **Provider-side vCenter work is not unit-testable.** `FakeClient` covers Job
  planning, gating, checksum, and status shaping; the actual datastore-upload /
  `CreateVM` / `MarkAsTemplate` is verified live against the real vCenter
  (recorded in project memory), consistent with how introspection (ADR-0019) is
  validated.
- **Content Library is deferred but not designed out.** Default-off toggle now;
  the CL import path is a follow-up behind the same field.
- **Security posture unchanged.** Import Jobs live in the isolated imagebuild
  namespace (ADR-0016), mount the PVC read-only, and verify the checksum before
  writing anything to a backend.
- **cloud-config secret-first.** Inline/configMap convenience is reserved in the
  API shape but not implemented; users supply a Secret today.

## Follow-ups

- CALM: model the `imageImport` Job (imagebuild ns) → vCenter datastore/cluster
  relationship and the ISO artifact flow; `make calm-validate` + `calm-diagrams`.
- Implement the Content Library import path behind `useContentLibrary: true`.
- Implement `cloudConfig` inline/`configMapRef` variants (derived Secret).
- Provider-class-driven artifact selection when a single `VMImage` is consumed
  by both a libvirt and a vSphere `Url` source (two build artifacts, or a
  primary + derived) — out of scope here; current scope is one artifact kind
  per `VMImage`.
