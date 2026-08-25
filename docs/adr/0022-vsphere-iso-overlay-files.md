# 0022 — vSphere ISO overlay files via OSArtifact volumes

## Status

Accepted — 2026-08-15, amended 2026-08-17 (Decision #3, the Secret-symlink
workaround) and 2026-08-18 (Decision #4, configurable importer image).
Extends [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md) (the
`banlieue-imagebuilder` build pipeline) and
[ADR-0020](0020-vsphere-per-zone-iso-import.md) (vSphere ISO import).

## Context

Live-testing ADR-0021 against the real vCenter surfaced a VM that never
completed its unattended install: vCenter's own event log showed `Virtual
device ide0:0 will start disconnected`, and `xorriso` confirmed the built ISO
had a malformed multi-session structure (`Chain of ISO session headers broken
at #2`) — a genuine defect in that one build, unrelated to anything in the
vSphere provider. A fresh `OSArtifact` rebuild resolved it.

Investigating this surfaced the maintainer's own proven, hand-run ISO-build
pipeline (`~/dev/vm-build/bin/build-kairos-iso.sh`), which passes
`--set "iso.overlay_iso=/iso-overlay"` to `auroraboot build-iso` — overlaying
a custom `/boot/grub2/grub.cfg` (and potentially other files) onto the built
ISO. `banlieue-imagebuilder`'s `OSArtifact` requests have no equivalent: they
request `artifacts.iso: true` and (optionally) `cloudConfigRef`, nothing else.
kairos-operator's installed `OSArtifact` CRD, however, already has a
first-class mechanism for this: `spec.artifacts.overlayISOVolume` names an
entry in `spec.volumes` (a standard `[]corev1.Volume`) that gets passed to
`auroraboot build-iso --overlay-iso <mount path>` — the exact flag the manual
pipeline already uses.

This ADR closes that gap: let a `VMImage` declare overlay files (backed by a
Secret) that `banlieue-imagebuilder` wires into the `OSArtifact` it already
builds, using the CRD field kairos-operator already exposes. No new
kairos-operator capability is needed, and no new `banlieue-imagebuilder`
RBAC either — `spec.volumes[].secret.secretName` + `items[].key` need only the
Secret's *name* and the *key names the user declares*, never its content;
`banlieue-imagebuilder`'s "never touches Secrets" posture (ADR-0021 Decision
#3) is unaffected.

## Decision

### 1. New `VMImageSpec.isoOverlay` field

```rust
/// Additional files overlaid onto a built ISO (vSphere `Url` sources only),
/// backed by a single Secret. Maps to kairos-operator's OSArtifact
/// `spec.volumes[]` + `spec.artifacts.overlayISOVolume`, mirroring
/// auroraboot's `--overlay-iso` flag (ADR-0022).
pub struct IsoOverlaySource {
    /// Secret holding the overlay file contents. Only its name is read by
    /// banlieue-imagebuilder — never its data (ADR-0021 Decision #3).
    pub secret_ref: LocalObjectReference,
    /// Explicit key -> ISO-relative-path mapping. At least one entry.
    pub files: Vec<IsoOverlayFile>,
}

pub struct IsoOverlayFile {
    /// Key within `secret_ref` holding this file's content.
    pub key: String,
    /// Destination path, relative to the ISO root (e.g. `boot/grub2/grub.cfg`).
    pub path: String,
}
```

One Secret, not one per file: every real use case (a custom `grub.cfg`, maybe
a couple more files later) fits one Secret with multiple keys, and
`spec.artifacts.overlayISOVolume` only ever names a *single* volume — multiple
source Secrets would need `banlieue-imagebuilder` to materialize a merged
derived Secret, which (per the `autoManageInstall` decision) is exactly the
Secret-reading complexity this ADR avoids.

### 2. `banlieue-imagebuilder` wiring

`desired_os_artifact` (`crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`)
gains an `iso_overlay: Option<&IsoOverlaySource>` parameter. When set, it adds:

```json
{
  "spec": {
    "volumes": [
      {
        "name": "iso-overlay-source",
        "secret": {
          "secretName": "<isoOverlay.secretRef.name>",
          "items": [{"key": "<file.key>", "path": "<file.path>"}, ...]
        }
      },
      {
        "name": "iso-overlay",
        "emptyDir": {}
      }
    ],
    "importers": [{
      "name": "iso-overlay-materialize",
      "image": "busybox:1.36",
      "command": ["sh", "-c", "find /overlay-src -mindepth 1 -maxdepth 1 -not -name '.*' -exec cp -rL -t /overlay-dst/ {} +"],
      "volumeMounts": [
        {"name": "iso-overlay-source", "mountPath": "/overlay-src", "readOnly": true},
        {"name": "iso-overlay", "mountPath": "/overlay-dst"}
      ]
    }],
    "artifacts": {
      "overlayISOVolume": "iso-overlay"
    }
  }
}
```

`"iso-overlay"` (the `emptyDir`, not the Secret volume) is what
`overlayISOVolume` names — see **Decision #3** below for why the Secret is
never mounted directly into the build container. Threaded through
unconditionally when set, mirroring how `cloudConfigRef` already passes
through regardless of artifact kind — `overlayISOVolume` is presumably inert
for a `cloudImage` build, matching the precedent for not gating
`cloudConfigRef` on kind either.

### 3. Dereference the Secret before `auroraboot` sees it (added 2026-08-17)

Live-testing this ADR against the real vCenter pipeline hit
`Failed creating ISO image: exit status 5` during `auroraboot build-iso`,
every time `overlayISOVolume` pointed directly at the Secret volume from
Decision #1/#2 as originally written.

Root-caused via a local, out-of-cluster reproduction
(`docker run quay.io/kairos/auroraboot:v0.24.0 build-iso --overlay-iso ...`,
outside Kubernetes entirely): kubelet mounts every Secret/ConfigMap volume
using an "atomic writer" layout where each top-level path component is a
*symlink* into a hidden, timestamped directory —

```
iso-overlay-source/
├── ..data -> ..2026_08_17_20_28_56.3347250082
├── ..2026_08_17_20_28_56.3347250082/boot/grub2/grub.cfg
└── boot -> ..data/boot                              # symlink, not a real dir
```

— and `auroraboot`'s overlay-copy step, when merging that tree onto the
already-populated ISO root (which by then has a *real* `boot/` directory from
the earlier EFI/kernel staging step), collides the symlink with the real
directory. The copy step itself reports success (`Finished syncing`) with no
error, but the ISO's `/boot` ends up broken, and `xorriso` fails later,
opaquely, with `exit status 5`. Confirmed both ways: an identical overlay
tree with plain files (no symlinks) builds a valid ISO every time; the exact
kubelet symlink layout reproduces the crash every time. Filed upstream as
[kairos-io/kairos#4324](https://github.com/kairos-io/kairos/issues/4324).

**Decision:** never point `overlayISOVolume` at the Secret volume directly.
Instead:
- Mount the Secret read-only into a `spec.importers[]` init container (an
  existing kairos-operator mechanism — "init containers that run before the
  build phase on the builder Pod"), alongside a fresh `emptyDir`.
- That container runs `find <src> -mindepth 1 -maxdepth 1 -not -name '.*'
  -exec cp -rL -t <dst>/ {} +`: `-L` dereferences the symlinks into plain
  files, `-not -name '.*'` skips kubelet's `..data`/`..<timestamp>`
  bookkeeping entries so only the caller-declared overlay files are copied.
- `overlayISOVolume` names the `emptyDir`, which by build time holds plain,
  symlink-free content — no different, from `auroraboot`'s point of view,
  than the maintainer's original hand-built overlay directory.

This is a `banlieue-imagebuilder`-side workaround, not a fix — the underlying
`auroraboot` bug is unresolved upstream. It is intentionally kept even if
`auroraboot` fixes the symlink handling later: it costs one small `busybox`
init container per build and removes an entire class of overlay bugs (any
future Secret/ConfigMap-shaped overlay input) rather than depending on
`auroraboot`'s copy semantics indefinitely.

### 4. Configurable importer image and pull secrets (added 2026-08-18)

Decision #3 hardcoded `busybox:1.36` from the public registry as the
materializer's image, with no pull secret. Some clusters cannot reach public
registries at all — every image, including `busybox`, is pulled from an
internal mirror requiring its own credentials.

**Decision:** `banlieue-imagebuilder` gains two CLI flags (`Cli` in
`crates/banlieue-imagebuilder/src/app.rs`), parsed into a new
`ImporterImage` (`crates/banlieue-imagebuilder/src/importer_image.rs`) and
threaded through `Context` into `desired_os_artifact`:

- `--build-importer-image` / `BANLIEUE_BUILD_IMPORTER_IMAGE` (default:
  `busybox:1.36`) — full reference (`repo[:tag][@sha256:digest]`) for the
  `spec.importers[]` container. A digest pin works the same way as
  `ProviderImage.digest` (`providerclass.rs`): whatever is set here is what
  actually gets pulled.
- `--build-importer-image-pull-secret` (repeatable, CLI-only — same
  precedent as `--build-node-selector`/`--build-toleration`, which also have
  no env-var form since a single env var does not map cleanly onto a
  repeated flag) — Secret names used to pull it.

These are cluster-wide, install-time settings on the `banlieue-imagebuilder`
binary — not a per-`VMImage` field. This follows the same shape as
`ProviderClass.spec.image` (also cluster-scoped, also install-time): which
registry a cluster's nodes can reach is an operator decision made once, not
a per-build-request one, and `banlieue-imagebuilder` has no per-backend
"class" CRD of its own to hang a per-resource override off.

The pull secrets are applied as the `OSArtifact`'s pod-wide
`spec.imagePullSecrets`, unconditionally when configured — **not** gated on
`iso_overlay` being set. Kubernetes pull secrets have no per-container form;
a Pod either has access to a registry's credentials or it doesn't. A cluster
that needs credentials to pull `busybox` from its mirror needs the same
credentials to pull the kairos build image itself, so scoping this to "only
when the overlay importer runs" would leave the common case (mirror-only
cluster, no overlay in use) with no way to authenticate the main build pull
at all.

## Consequences

- **Closes the ISO-customization gap without new kairos-operator work.** The
  CRD field already existed; `banlieue-imagebuilder` just wasn't using it.
- **No new RBAC.** `banlieue-imagebuilder` never reads the referenced
  Secret's content — only its declared name and key list, exactly as already
  established for `cloudConfigRef`.
- **One extra `busybox` init container per overlay-enabled build.** Adds a
  small, fixed cost (Decision #3) to work around the upstream `auroraboot`
  symlink bug; negligible next to the OCI build and ISO-generation steps it
  runs alongside.
- **Importer image/pull-secrets are cluster-wide config, not per-`VMImage`.**
  An operator on an air-gapped or mirror-only cluster sets
  `--build-importer-image`/`--build-importer-image-pull-secret` once on the
  `banlieue-imagebuilder` Deployment; no `VMImage` author needs to know about
  it (Decision #4).
- **Does not by itself explain or fix the multi-session ISO defect** found
  live this session — that was resolved by a fresh `OSArtifact` rebuild and
  remains a separate, currently unexplained one-off (or possibly systemic)
  issue with a given `auroraboot`/kairos-operator build. This ADR gives
  operators an escape hatch to customize boot behavior (e.g. a hand-verified
  `grub.cfg`) independent of whatever caused that.
- **`grubConfig` on the same CRD is deliberately not used.** Its behavior
  isn't documented in kairos-operator's source (no doc comment found), so the
  well-documented, general `overlayISOVolume` mechanism is used instead.

## Follow-ups

- Track [kairos-io/kairos#4324](https://github.com/kairos-io/kairos/issues/4324)
  upstream. If `auroraboot` fixes `--overlay-iso` to tolerate a Secret/ConfigMap
  symlink layout, the Decision #3 importer becomes unnecessary — but keep it
  anyway (see Decision #3's closing note) unless the extra init container
  becomes a real cost.
- If a future need arises for overlay files from more than one Secret,
  revisit the single-Secret constraint — likely via a derived-Secret merge in
  `banlieue-imagebuilder`, which would need the RBAC relaxation this ADR
  avoids.
- Consider exposing `overlayRootfsVolume` (the `--overlay-rootfs` equivalent)
  the same way, if a concrete need for rootfs-level overlays (as opposed to
  ISO-filesystem-level) shows up.
