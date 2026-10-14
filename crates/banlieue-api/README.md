<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# banlieue-api

The Rust types behind every [banlieue](https://github.com/firestoned/banlieue)
custom resource: `VirtualMachine`, `VirtualMachinePool`, `VirtualMachineClaim`,
`Provider`, `ProviderClass`, `VMClass`, `VMImage` (group `banlieue.io`) and the
per-backend infrastructure kinds such as `VSphereMachine` and
`LibvirtMachine` (group `infrastructure.banlieue.io`). banlieue's CRDs are
generated from this crate, so these types are the schema the API server
enforces.

```toml
[dependencies]
banlieue-api = "0.4"
# Choose the Kubernetes version you target; this crate does not.
k8s-openapi = { version = "0.28", features = ["latest"] }
kube = { version = "4.2", features = ["client", "runtime"] }
```

```rust,ignore
use banlieue_api::banlieue::VirtualMachine;
use kube::{Api, Client};

let client = Client::try_default().await?;
let vms: Api<VirtualMachine> = Api::namespaced(client, "default");
for vm in vms.list(&Default::default()).await? {
    println!("{}", vm.metadata.name.unwrap_or_default());
}
```

## Versions

The crate is published with every banlieue release and carries the release's
version: `banlieue-api 0.4.3` holds the types of the `v0.4.3` CRDs. The API
groups are `v1alpha1`, so any minor version may change them.

## Features

| Feature | What it adds |
| --- | --- |
| `yaml` | `banlieue_api::yaml`, YAML output as banlieue writes it |
| `crdgen` | The `crdgen` and `crddoc` binaries that write banlieue's CRDs and API reference. Enables k8s-openapi's `latest` version, so a library must not turn it on |

Licensed under Apache-2.0.
