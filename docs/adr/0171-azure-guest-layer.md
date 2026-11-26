<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0171: The Azure guest layer and its Rust helper

- **Status:** Accepted
- **Date:** 2026-11-26
- **Deciders:** Erick Bourgeois
- **Related:** Implements [ADR-0163](0163-guestready-on-a-cloud.md) Decision 3
  (what an Azure image must carry) for every image banlieue builds. Extends
  [ADR-0037](0037-vmimage-layered-cloud-config.md) (layered cloud-config) and
  copies [ADR-0088](0088-first-boot-sealing-for-immediate-images.md)'s
  pattern (a banlieue-generated layer merged last).
  [ADR-0162](0162-publishing-a-vmimage-into-a-cloud.md) (the image it
  publishes), [ADR-0010](0010-vmimage-build-pipeline-imagebuilder.md) (the
  build pipeline, as amended 2026-11-26),
  [ADR-0170](0170-post-quantum-tls-with-aws-lc-rs.md) (no `ring`; the helper
  has no TLS at all). Roadmap 23 phase D.

## Context

ADR-0163 Decision 3 lists what a Kairos image needs before it can run on
Azure: read `customData` from the local provisioning disk, leave the
accelerated-networking VF unmanaged, start no QEMU guest agent on Hyper-V,
log to `/dev/kmsg`, and report ready to WireServer only from an installed
boot, after user data has been applied. The roadmap 23 spike met them with a
hand-written cloud-config (`curl`, `grep`, `sed`, `base64` in shell) baked
into one image.

That does not scale to "any Kairos image works on Azure". Every site would
copy the same layer into its own cloud-configs, a site's own base image
would need it, and a sandbox gateway image would be special when it should be
"the same as any other `VMImage`". Two of the five pieces are imperative
(parse XML, speak HTTP with retries) and the standing rule for in-guest
helpers is Rust, never shell with `curl`, `openssl` or Python.

The image builder already owns the merged cloud-config of every `Url` build
(ADR-0037) and already appends a layer of its own last (ADR-0088), so the
declarative pieces have an obvious home. The hard part is the binary: the
layer is cloud-config, which AuroraBoot copies into the image's `COS_OEM`;
a binary must reach the image's root filesystem **at build time**, because a
boot may fetch nothing (the local-only rule), and it must be pinned by digest
so what runs in every Azure VM is what was reviewed.

What kairos-operator v0.3.0 (`build.kairos.io/v1alpha2`) offers for putting
files into an image, read from its `internal/controller/job.go`:

| Mechanism | What it does | For a `cloudImage` (raw) build |
| --- | --- | --- |
| `spec.artifacts.bundles` | each entry is an OCI image, `auroraboot unpack <image> /rootfs` in an init container after the source image is unpacked and before the disk is made | **applies**: the raw build reads `container_image=dir:/rootfs` |
| `spec.artifacts.overlayRootfsVolume` | `--overlay-rootfs` | **ignored**: only `build-iso` passes it |
| `spec.image.ociSpec` (a Dockerfile in a Secret) | buildah builds a new image, `FROM` plus whatever the file says | applies, but replaces `image.ref` with a buildah build |
| `spec.importers` | user containers that run first | run **before** the source unpack, on volumes the user names |
| `cloudConfigRef` with the binary in `files:` (base64) | written by yip at boot | applies |

## Decision

### 1. Every build of a `VMImage` with an `azure` source carries the layer

When any `spec.sources[]` entry has `providerClass: azure`, whatever its
kind, `banlieue-imagebuilder` adds the helper (Decision 2) and the layer
(Decision 3) to the image's build. There is no new API field: an image for
Azure needs them, so asking would only allow a broken image.

The build is shared: a `VMImage` with a `libvirt` `Url` source and an `azure`
source builds once, and the disk carries the layer on both. Every step is
gated on Azure's DMI chassis asset tag (`7783-7084-3265-9085-8269-3286-77`),
in the layer and again in the helper, so off Azure nothing runs and nothing
changes.

An image **without** an `azure` source builds exactly as before: the merged
cloud-config is byte for byte what it was and the `OSArtifact` has no
`bundles` key (both unit tested).

### 2. The helper is delivered as a digest-pinned kairos-operator bundle

The helper, `banlieue-azure-guest`, is a static
`x86_64-unknown-linux-musl` binary built with the workspace's `guest` profile
(opt-level `z`, fat LTO, `strip`, `panic=abort`): 453 KiB. It is published as
an OCI image with one file, `/usr/bin/banlieue-azure-guest`
(`Dockerfile.azure-guest`, `FROM scratch`), and the image builder lists that
image in the `OSArtifact`'s `spec.artifacts.bundles`. kairos-operator unpacks
it onto the build root filesystem after the source image, so it lands in the
recovery image and in every active and passive image installed from it.

- The reference comes from `--azure-guest-image`
  (`BANLIEUE_AZURE_GUEST_IMAGE`) and must be `<repository>@sha256:<64 hex>`;
  the image builder refuses to start with a tag. Unset (the default), a
  `VMImage` with an `azure` source fails its build before any `OSArtifact`
  exists, `buildArtifact.reason: AzureGuestImageUnset`.
- `bundles` is part of the build's identity: a live `OSArtifact` whose
  bundles differ from the desired ones (an `azure` source added or removed, a
  new helper digest) is deleted and rebuilt, like a changed source image.
- `/usr/bin`, not `/usr/local/bin`: on Kairos `/usr/local` is the persistent
  partition and would hide the file.

Alternatives, and why not:

- **The binary base64-encoded in the layer's `files:`.** 453 KiB becomes
  604 KiB of base64 in a cloud-config that yip parses at every stage of every
  boot, inside a Secret capped at 1 MiB together with the site's own layers,
  and the bytes would have to be compiled into the image builder (a musl
  cross-build inside the banlieue image build). Rejected as the heaviest
  option at run time.
- **`overlayRootfsVolume`.** kairos-operator passes it to `build-iso` only; a
  raw build, which is what Azure publishes, ignores it.
- **An `ociSpec` Dockerfile (`FROM <importFrom>` plus `COPY --from`).**
  Works for any source, but turns every Azure build from an unpack into a
  privileged buildah build, changes how `image.ref` is used (and so what
  ADR-0010 verifies), and adds a Dockerfile templating path for one file.
- **An importer writing into the operator's `rootfs` volume.** Importers
  run before the source unpack, and the volume name is the operator's
  internal detail, not its API.
- **Fetching the helper at boot.** Breaks the local-only rule and puts a
  download in the provisioning window.
- **Shell with `curl`.** What the spike did; ruled out by the in-guest Rust
  rule, and `curl` is not in every Kairos flavour.

### 3. The layer

Generated by `crates/banlieue-imagebuilder/src/azure_guest.rs` and merged
after the operator layers and the first-boot sealing layer, so no operator
layer can remove it. Every step carries the asset-tag guard.

| Stage | Step | Why |
| --- | --- | --- |
| `rootfs.before` | write `/etc/systemd/network/10-banlieue-azure-vf-unmanaged.network` (`Driver=mana mlx4_core mlx5_core`, `Unmanaged=yes`), `networkctl reload` | the initramfs's networkd takes DHCP leases before the real root; this is where Kairos writes its catch-all, and `10-` sorts before it |
| `initramfs` | the same file for the real root | the real root's networkd reads its own `/etc` |
| `initramfs` | `systemctl mask --runtime qemu-guest-agent.service` | Kairos's `26_vm.yaml` matches Hyper-V's DMI name and waits 90 s for a port that never appears (older Kairos; fixed upstream since) |
| `initramfs` | write and enable `banlieue-azure-ready.service` | Decision 5 |
| `boot` | `banlieue-azure-guest customdata` | Decision 4 |

The layer sets no top-level key but `stages`: users, `install` and `kcrypt`
stay the site's.

### 4. The helper

One crate, `crates/banlieue-guest-azure`, two subcommands and no flags:
nothing about it is configurable at run time. Dependencies are `libc`
(mount(2)), `base64` (decode only) and `thiserror`; no async runtime, no TLS
stack, no HTTP library.

- **`customdata`.** Scans `/dev/sr*`, mounts each read-only (`nosuid`,
  `nodev`, `noexec`; UDF, then ISO 9660) until one holds `ovf-env.xml`, reads
  it within 256 KiB and unmounts. A strict small parser takes the single
  `CustomData` element: root must be `Environment`, a DTD, CDATA or entity
  reference is refused, a second `CustomData` is refused. The base64 is
  decoded within 64 KiB and must be a `#cloud-config` document (UTF-8, no
  NUL), or nothing is written. It is written to
  `/oem/91_azure_customdata.yaml`, mode 0600, through a temporary file,
  `fsync` and `rename`; writing the same bytes again changes nothing. The
  admin password in the same document is never read out, and customData is
  never logged.
- **`ready`.** Only on an active or passive boot. `GET
  /machine/?comp=goalstate` on `168.63.129.16:80` (`x-ms-version:
  2012-11-30`), takes `Incarnation`, `ContainerId` and `InstanceId`, each
  checked against a narrow character set before it is put into XML, and
  `POST /machine/?comp=health` with `<State>Ready</State>`. A failure (no
  route yet, refused, non-2xx, a bad goal state) is retried with back-off
  doubling from 1 s to 30 s, until a 2xx or 15 minutes, inside Azure's
  20-minute provisioning window. The address is a constant; tests inject a
  local fake through a function parameter, never a flag.
- Both exit 0 and do nothing off Azure (asset tag), and both log to
  `/dev/kmsg` with a `banlieue-azure-guest:` prefix.

### 5. Ordering: user data applies before the report

- `customdata` runs in `boot` in every mode. A raw image's first boot is a
  recovery boot whose AuroraBoot `01_reset.yaml` resets in the `network`
  stage, after `boot`; so the file is on `COS_OEM` before the reset, the reset
  keeps `COS_OEM`, and the first active boot applies it from its first stage.
  On a later boot it is a no-op.
- `ready` is a oneshot unit ordered `After=cos-setup-network.service`, the
  unit that runs Kairos's `network.before`, `network` and `network.after`
  stages: every stage of the image's own configuration and of customData has
  finished before the report. Its conditions refuse recovery
  (`ConditionPathExists=!/run/cos/recovery_mode`, plus active or passive),
  and the helper checks again.

### 6. What a site's base image must not do

The layer works for any Kairos image that does not undo it:

- configure the VF (`mana`, `mlx4_core`, `mlx5_core`) from a networkd file
  that sorts before `10-banlieue-azure-vf-unmanaged.network`;
- run another Azure provisioning agent (`waagent`, cloud-init's Azure
  datasource) that also reports ready or consumes customData;
- ship its own `/usr/bin/banlieue-azure-guest`, or mask
  `banlieue-azure-ready.service`;
- mount `COS_OEM` read-only, or block root's traffic to `168.63.129.16`
  (mediatore's host table allows it for uid 0 only, ADR-0163);
- use a Kairos without `cos-setup-network.service` (every Kairos v3 and v4
  has it).

## Consequences

- ADR-0163 Decision 3's image requirements are provided by banlieue, not
  documented for sites to copy. A gateway, sandbox or site base image needs
  nothing Azure-specific to boot on Azure.
- An image banlieue did not build (a `Template` source naming a
  hand-published gallery version) is unchanged: it must still carry the
  requirements itself, or be rebuilt from a `Url` source.
- A second image joins the release: `banlieue-azure-guest`, published,
  signed and pinned by digest in the install manifests next to the banlieue
  image. Until the release workflow publishes it, a site sets
  `BANLIEUE_AZURE_GUEST_IMAGE` itself; without it Azure builds refuse to
  start rather than produce an image that times out.
- A new helper digest rebuilds every Azure image, and so publishes a new
  gallery version (ADR-0162 names versions by the built disk's digest).
- customData now persists on `COS_OEM` at rest (0600, root), across reboots
  and resets. That is what applying it on every boot requires; it matters once
  customData carries a join token (threat model §8).
- arm64 is not built yet: the bundle is `linux/amd64`, matching the Azure
  images published today.
- **Not verified live.** A live Azure run must confirm: the bundle unpacks
  onto `/rootfs` (file present and executable in the active image); the
  recovery boot writes `/oem/91_azure_customdata.yaml` before the reset; the
  first active boot applies it and `banlieue-azure-ready.service` starts
  after `cos-setup-network.service` and gets HTTP 200; the VF stays
  unmanaged on an accelerated-networking size; create to `Succeeded` stays
  near the spike's 189 s.
