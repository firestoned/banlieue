# Vendored upstream spec

`cloud-hypervisor-v53.0.yaml` is copied **verbatim** from
[cloud-hypervisor/cloud-hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor)
at tag `v53.0`, path `vmm/src/api/openapi/cloud-hypervisor.yaml`. Upstream
licenses it under Apache-2.0 and BSD-3-Clause; see that repository's
`LICENSES/` directory.

It is not compiled or generated from. `src/spec_tests.rs` parses it to check
that every field this crate sends exists in the pinned release, and that the
file still matches the digest in `PIN`. To upgrade, replace the file with the
new tag's copy, update `PIN`, and fix what the tests report.
