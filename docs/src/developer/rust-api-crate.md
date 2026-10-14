# Using banlieue from Rust: the `banlieue-api` crate

Every banlieue custom resource is generated from the Rust types in
`crates/banlieue-api`, and every release publishes that crate to
[crates.io](https://crates.io/crates/banlieue-api) at the release's own
version ([ADR-0100](https://github.com/firestoned/banlieue/blob/main/docs/adr/0100-publish-banlieue-api-to-crates-io.md)).
A program that creates, reads or watches banlieue objects can use the same
types the API server's schema came from, instead of re-deriving them from
the CRD YAML.

## Add it

```toml
[dependencies]
banlieue-api = "=0.4.3"   # the release whose CRDs your cluster runs
k8s-openapi = { version = "0.28", features = ["latest"] }
kube = { version = "4.2", features = ["client", "runtime"] }
```

- **Match the installed release.** `banlieue-api 0.4.3` holds the types of
  the `v0.4.3` CRDs. The API groups are `v1alpha1`, so any minor version may
  change them; pin the exact version, or at least the minor.
- **Choose the Kubernetes version yourself.** Like any library built on
  k8s-openapi, the crate enables no Kubernetes version feature; your binary
  enables exactly one.
- **The crate is types only.** It brings no client, credential or TLS stack.
  Your program talks to the API server with its own identity, so give that
  identity only the RBAC its calls need; the same admission policies apply
  to it as to `kubectl`.

## Example

```rust
use banlieue_api::banlieue::VirtualMachine;
use kube::{Api, Client, api::ListParams};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let vms: Api<VirtualMachine> = Api::namespaced(client, "default");
    for vm in vms.list(&ListParams::default()).await? {
        println!("{}", vm.metadata.name.unwrap_or_default());
    }
    Ok(())
}
```

`banlieue_api::prelude::*` brings in every kind from both API groups.

## Features

| Feature | What it adds |
| --- | --- |
| `yaml` | `banlieue_api::yaml`: YAML output as banlieue writes it |
| `crdgen` | The `crdgen` and `crddoc` binaries that write banlieue's CRDs and the [API reference](../reference/api.md). It enables k8s-openapi's `latest` version, so a library must not turn it on |

## Working on the crate

`cargo build -p banlieue-api` on its own fails in k8s-openapi's build
script, because nothing has chosen a Kubernetes version. Use the workspace
build, the tests (the dev-dependency chooses one), or
`K8S_OPENAPI_ENABLED_VERSION=1.36 cargo build -p banlieue-api`.
`make crate-package` packages the crate and builds the packaged copy alone,
as crates.io will; CI runs it on every pull request.
