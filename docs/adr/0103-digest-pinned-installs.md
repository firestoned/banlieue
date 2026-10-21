<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0103: `--image-digest` pins every installed image; releases publish digest-pinned manifests

- **Status:** Accepted
- **Date:** 2026-10-21
- **Deciders:** Erick Bourgeois
- **Related:** Amends [ADR-0013](0013-banlieue-bootstrap-cli.md) (`banlieue
  bootstrap`); extends [ADR-0006](0006-release-and-supply-chain-pipeline.md)
  (release pipeline) and [ADR-0096](0096-clusterctl-infrastructure-provider.md)
  (clusterctl components); closes the threat model's §8 risk that published
  install manifests reference the image by tag

## Context

Every GitHub Release attaches `install.yaml` and the clusterctl
`infrastructure-components.yaml`, both `banlieue bootstrap operator --dry-run`
for the release's tag. They named the image as `ghcr.io/firestoned/banlieue:<tag>`.
A tag is a pointer anyone with push rights to the package can move, so a fresh
install could run bytes other than the ones the release's signatures and
attestations describe. A tag also hides rollouts: a Deployment whose spec names
an unchanged tag does not roll when the tag moves, and keeps running old layers.

`bootstrap` already had `--image-digest`, but it reached only the seeded
`ProviderClass`es (`spec.image.digest`). The controller, operator and
imagebuilder Deployments, which bootstrap writes itself, were built from the tag
alone. So a pinned install was only partly pinned, and the release pipeline did
not pass a digest at all.

The digest exists only after the image is pushed, so the manifests can be
pinned only by a job that runs after the `docker` job.

## Decision

1. **`--image-digest` pins every banlieue image bootstrap writes.** The
   controller, operator, imagebuilder and statically installed provider
   Deployments use `<repository>:<tag>@<digest>` (`install_image` in
   `crates/banlieue-operator/src/bootstrap.rs`), the same form
   `ProviderImage::reference` gives a pinned class. The tag stays beside the
   digest as documentation of intent; the digest is what the kubelet pulls. A
   `--registry` mirror is pinned to the same digest, since a mirror holds the
   same bytes.
2. **The flag is validated when parsed**: `sha256:` and 64 lowercase hex
   digits, via `banlieue_oci::reference::require_digest`, the one definition
   of a valid digest in the workspace. A malformed digest would otherwise pass
   the install and only surface as `ErrImagePull` on every pod.
3. **Releases pin.** The `release-manifests` job needs `docker` and, on a
   release, passes the default image's multi-arch index digest
   (`docker.outputs.digest-chainguard`, the unsuffixed tag) as
   `RELEASE_IMAGE_DIGEST`. `make install-yaml` and `make clusterctl-components`
   hand it to `--image-digest`. `RELEASE_REQUIRE_DIGEST=true` makes
   `make release-digest-check` fail the release rather than ship an unpinned
   manifest.
4. **Main pushes render by tag.** Their image carries `sha-`/`latest` tags,
   not the version tag the manifests name, and nothing from a push is
   published, so pinning there would pair a tag and a digest that disagree.

## Consequences

- A release's `install.yaml` and clusterctl components install exactly the
  image the release signed and attested, and a moved tag changes nothing for a
  fresh install.
- Upgrading by re-applying a newer release's manifests changes the digest, so
  the Deployments roll, which a tag-only upgrade between rebuilt images did not
  guarantee.
- The Distroless variant is not what the manifests pin. Choosing it means
  `--registry`/`--version` with the `-distroless` tag and its own digest, as
  before.
- `banlieue-operator` depends on `banlieue-oci` for the digest check. That
  crate is already linked into the `banlieue` binary through the imagebuilder
  and the Cloud Hypervisor provider, so the binary gains no new code.
- Bootstrap still does not resolve a digest itself (that would need registry
  access at install time); the caller supplies it. `bootstrap` without
  `--image-digest` behaves as before.
