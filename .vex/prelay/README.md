<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# `.vex/prelay/`: OpenVEX statements for the pre-lay image

Curated [OpenVEX](https://openvex.dev/) statements for
`ghcr.io/firestoned/banlieue-prelay`, the image banlieue-imagebuilder's
pre-lay Job runs ([ADR-0113](../../docs/adr/0113-pre-laid-images-for-fast-sealed-clones.md),
*Amendment: a Grype gate for the pre-lay image*).

They are kept apart from `.vex/*.json` on purpose. Those statements argue
about one static Rust binary (`pkg:oci/banlieue`) and are merged and attested
to the banlieue image; the pre-lay image is a Debian 13 userland (QEMU, OVMF,
e2fsprogs, gdisk, util-linux) and none of those arguments transfer to it. The
`build-vex` job globs `.vex/*.json` only, so nothing here reaches the banlieue
image's VEX.

`make grype-prelay` passes every `*.json` here to Grype as `--vex`. The gate
fails only on a finding at or above `high` that Debian has **already fixed**
(`--only-fixed`), so a statement is needed only when a fixed finding cannot be
taken (rare: rebuilding picks up the fix) or does not apply to how the Job
runs the package. Write the argument in terms of that Job: a privileged
container, no API token, QEMU seccomp-sandboxed with no network and no TPM,
booting the image being built.

Use the shape in [`../README.md`](../README.md), with
`"products": [{ "@id": "pkg:oci/banlieue-prelay" }]`, and validate with
`make vex-validate`.
