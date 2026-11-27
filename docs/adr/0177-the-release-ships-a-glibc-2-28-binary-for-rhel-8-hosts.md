<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0177: The release ships a glibc 2.28 binary for RHEL 8 hosts

- **Status:** Accepted
- **Date:** 2026-11-27
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0006](0006-release-and-supply-chain-pipeline.md) (signed,
  attested release assets) with one more binary. Serves
  [ADR-0067](0067-banlieue-host-install-subcommand.md) (the host installer
  copies the running binary onto the host as the provider). Supersedes the
  `make build-el8` stopgap of 2026-11-26 as the way to obtain the binary; the
  target stays as the offline route.

## Context

A Cloud Hypervisor host runs the `banlieue` binary itself: the installer copies
it to `/usr/local/bin` and a systemd unit starts it as the provider. The
release binary is built on `ubuntu-24.04`, so it links against glibc 2.39.
RHEL 8 ships glibc 2.28. On such a host the binary does not start:
`GLIBC_2.39 not found`.

v0.6.0 met this on a real RHEL 8.10 host. The workaround was a binary built by
hand in an AlmaLinux 8 container, which is now `make build-el8`
(`scripts/build-el8.sh`). It works, but it is a developer's laptop
producing a binary with no signature, no provenance and no checksum in the
release, for a host that runs it as root next to guest VMs. A host operator
cannot verify it came from the tagged source.

A RHEL 8 host usually cannot build the binary itself: its `rustc` package
trails the workspace MSRV, and it has no route to crates.io. So the choice is
not "build on the host or download"; it is "download something we signed, or
download something nobody can check".

## Decision

1. **The release publishes a third binary tarball,
   `banlieue-linux-amd64-el8.tar.gz`**, next to `banlieue-linux-amd64` and
   `banlieue-linux-arm64`. It contains the same single `banlieue` binary, built
   from the same tag and lockfile, linked against glibc 2.28 or older.

2. **It is built by the same recipe as `make build-el8`**, in a new `build-el8`
   job of `build.yaml` that calls only that Makefile target (the repository's
   workflow rule). The job runs on a native `ubuntu-24.04` runner, so the
   AlmaLinux 8 container runs without emulation. The recipe: an AlmaLinux 8
   image carrying gcc and a rustup toolchain at the workspace MSRV, crates
   vendored on the runner with `cargo vendor --locked` and built `--offline
   --locked`, and the build-path remapping the release build uses.

3. **The base image is pinned by digest** in the job (`EL8_IMAGE=
   docker.io/library/almalinux:8@sha256:...`, the index digest). The
   unpinned `almalinux:8` tag stays the default of the script for a developer
   running it locally.

4. **The glibc ceiling is enforced by the build, not by review.** After the
   build, the script reads the binary's dynamic symbols and fails if any needs a
   glibc newer than 2.28 (compared as versions, so 2.9 is below 2.28), then runs
   `banlieue --version` inside the el8 image. A binary that cannot run on RHEL 8
   is never uploaded.

5. **It is signed and attested exactly like the other binaries.** It joins the
   `sign-artifacts` matrix: cosign keyless signature and bundle, GitHub
   build-provenance attestation, and inclusion in the SLSA subjects and
   `checksums.sha256` (the subjects job already matches
   `banlieue-linux-*-signed`).

6. **It gates the release.** `sign-artifacts` needs `build-el8`, so a failed el8
   build stops the release rather than shipping a release that omits an asset
   the documentation tells RHEL 8 operators to download. The job runs on pushes
   to `main` and on releases, not on pull requests, so a recipe break is found on
   the merge that causes it, not on release day.

7. **x86_64 only, and no separate SBOM.** There is no RHEL 8 arm64 host to
   serve. The el8 binary is built from the same source and lockfile as
   `banlieue-linux-amd64`, whose CycloneDX SBOM already describes its
   dependencies; a second SBOM would repeat it under colliding file names.

## Alternatives considered

- **Build every release binary on el8.** One binary, no new asset. Rejected:
  it moves the tested baseline of every platform onto an older compiler and
  libc for the sake of one host generation, and the arm64 build would need the
  same containerised treatment.
- **A static musl binary.** Runs anywhere. Rejected: the host binary manages
  system accounts and drives systemd, polkit and swtpm, and has only ever been
  built and tested on glibc. The Azure guest helper is musl-static because it
  has none of those needs (ADR-0171); this binary does. A different libc is a
  different product to test.
- **`cargo-zigbuild` against a glibc 2.28 target.** Fewer moving parts than a
  container. Rejected: it links against headers and a libc that are not the ones
  RHEL 8 ships, so "builds" would not mean "runs"; the real el8 userland is the
  only check worth having.
- **Keep `make build-el8` as the only route.** Rejected for the reason in
  Context: the operator cannot verify the bytes.

## Consequences

- A release gains one asset of about 90 MB and one parallel job. The build runs
  alongside the two existing binary builds, so release time grows only if it is
  slower than them.
- RHEL 8 operators download, verify (cosign bundle, provenance, checksum) and
  pass the binary to `banlieue host cloud-hypervisor install --provider-binary`.
  The documentation names the asset instead of the build command.
- **The el8 binary is smoke-tested, not integration-tested.** The unit and
  live tiers run against the glibc 2.39 build. The el8 binary is the same
  source compiled by gcc 8 against glibc 2.28, and is checked only for its
  glibc ceiling and `--version`. A defect specific to the older toolchain would
  reach a RHEL 8 host before it reaches a test. Recorded as an accepted risk in
  the threat model, with a revisit when the host fleet has an el8 e2e.
- **New trust inputs on the build path**, each recorded in the threat model's
  supply-chain section: the AlmaLinux 8 base image (digest-pinned, and not
  tracked by Dependabot since it lives in a workflow variable, so it is
  re-pinned on the same quarterly cadence as the stable toolchain ref), the
  AlmaLinux 8 package repositories (dnf verifies package signatures), and the
  rustup installer and toolchain fetched from `static.rust-lang.org` when the
  toolchain image is built.
- The toolchain image is rebuilt on every run (a clean runner has no cache).
  That costs minutes, and buys a build that does not depend on a cached,
  unattested image.
- A future RHEL 9-class host needs nothing: the main binary runs there.
