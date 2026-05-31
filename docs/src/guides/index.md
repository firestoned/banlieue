# Guides

Production-oriented, step-by-step guides for installing and operating banlieue
on a real cluster, using the released container image
`ghcr.io/firestoned/banlieue:v0.1.0`. No build-from-source, no simulators.

<div class="grid cards" markdown>

- :material-engine: **[Core Controller](core-controller.md)**

    Install the CRDs, the `banlieue-controller`, RBAC, and the optional
    ValidatingAdmissionPolicies — the foundation every provider builds on.

- :material-server-network: **[vSphere Provider](vsphere-provider.md)**

    From an empty cluster to a scheduled `VirtualMachine` on vCenter: the
    provider Deployment, credentials, `Provider`, `VMClass`, `VMImage`, and a
    `VirtualMachine` — every file and `kubectl apply`.

</div>

!!! info "Looking to hack on banlieue itself?"
    Building from source, running against `kind`/`vcsim`, and the
    `*-run-local` workflow live under **[Developer → Local Development](../developer/local-development.md)**.

## Conventions used in these guides

- Everything is pinned to the released tag **`v0.1.0`**. Manifests live in the
  repository under [`deploy/`](https://github.com/firestoned/banlieue/tree/v0.1.0/deploy)
  at that tag; the guides apply them directly.
- All workloads run in the **`banlieue-system`** namespace under the
  Pod Security **restricted** profile.
- A cluster of **Kubernetes 1.30+** is assumed (required for the
  ValidatingAdmissionPolicies; the controllers themselves work on older
  clusters).

```sh
# Pin the repo to the release so the manifests match the image.
git clone --branch v0.1.0 --depth 1 https://github.com/firestoned/banlieue
cd banlieue
```
