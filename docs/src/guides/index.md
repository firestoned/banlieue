# Guides

Production-oriented, step-by-step guides for installing and operating banlieue
on a real cluster, using the released container image
`ghcr.io/firestoned/banlieue:v0.1.0`. No build-from-source, no simulators.

<div class="grid cards" markdown>

- :material-map: **[End-to-End Setup](end-to-end-setup.md)**

    The whole chain in one diagram: bootstrapping the management cluster,
    installing banlieue, registering a backend, building an image, and
    provisioning a VM — with links out to every guide below.

- :material-engine: **[Core Controller](core-controller.md)**

    Install the CRDs, the `banlieue-controller`, RBAC, and the optional
    ValidatingAdmissionPolicies — the foundation every provider builds on.

- :material-server-network: **[vSphere Provider](vsphere-provider.md)**

    From an empty cluster to a scheduled `VirtualMachine` on vCenter: the
    provider Deployment, credentials, `Provider`, `VMClass`, `VMImage`, and a
    `VirtualMachine` — every file and `kubectl apply`.

- :material-server: **[libvirt Provider](libvirt-provider.md)**

    Register a libvirt/KVM host over mutual TLS and import a guest image onto
    it — a first-party RPC client, no `libvirt-dev` and no `virsh`.

- :material-cloud-download: **[Setting up the Kairos Operator](kairos-operator-setup.md)**

    Install the third-party [Kairos operator](https://kairos.io) banlieue's
    image-build pipeline depends on, and confirm it with a smoke-test build.

- :material-image-sync: **[Using banlieue-imagebuilder](using-banlieue-imagebuilder.md)**

    Turn an OCI/Kairos image into a `VMImage` raw disk automatically (ADR-0010)
    — install `banlieue-imagebuilder`, watch the build, and see exactly what's
    implemented today versus tracked as a follow-up.

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
