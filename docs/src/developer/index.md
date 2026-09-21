# Developer

Working **on** banlieue rather than deploying a release: building from source,
running the controller and providers out-of-cluster, and exercising the vSphere
provider against `kind` + the `vcsim` simulator with no real vCenter.

- **[Local Development](local-development.md)** — toolchain, `make` targets,
  build-from-source, `kind`/`vcsim` loop, and shell completion.
- **[OAuth / OIDC Clients](oauth-clients.md)** — registering a GitHub,
  Google or Auth0 client so a dev cluster can authenticate you as a real
  person, which is what makes the claim subject policy testable. Starts with
  the one decision that matters: GitHub needs Dex, Google and Auth0 do not.

If instead you want to *install a release* on a cluster, start with the
**[Guides](../guides/index.md)** (production manifests, `ghcr.io` images).

## Methodology

banlieue follows **ADD — Architecture Driven Development**: an architecturally
significant change starts with an [ADR](https://github.com/firestoned/banlieue/tree/main/docs/adr)
and a [CALM](https://github.com/finos/architecture-as-code) architecture update,
*then* test-driven implementation (`ADR → CALM → TDD`). After any Rust change,
run the quality gate:

```sh
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
```
