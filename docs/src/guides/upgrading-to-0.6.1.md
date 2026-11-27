<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Upgrading from 0.6.0 to 0.6.1

This guide takes a running banlieue from **v0.6.0** to **v0.6.1**. The
release adds the Azure provider, so most of it is new objects; for a cluster
that does not use Azure, it is the usual CRDs, RBAC, admission and image
bump, plus two behaviour changes worth reading
([recreate](#a-migrationpolicy-recreate-now-recreates) and
[provider Jobs](#provider-jobs-are-granted-only-in-the-build-namespace)).
Coming from 0.5.x, follow [Upgrading from 0.5.x to 0.6.0](upgrading-to-0.6.md)
first.

## At a glance

- **No stored object needs editing.** The API groups stay at `v1alpha1`. No
  CRD, field or enum value was removed or renamed; every new field is
  optional.
- **CRDs go first.** The v0.6.1 controller writes new status fields
  (`VirtualMachine.status.placementFailures`); against v0.6.0 CRDs the write
  succeeds and the field is silently dropped.
- **Two new CRDs**, `AzureVirtualMachine` and `AzureVirtualMachineTemplate`,
  and the `azure` provider class
  ([Azure provider](../providers/azure.md)).
- **Changed CRDs:** `Provider` (`spec.connection.azure`,
  `capabilities.instanceSizes`), `VirtualMachine`
  (`status.placementFailures`), `VMImage` (azure sources, the measured
  build digest).
- **A provider's Job rights moved** from its ClusterRole to a Role in the
  build namespace, behind a new fail-closed admission policy.
- **TLS:** every process now uses one rustls provider, aws-lc-rs, offering
  the post-quantum hybrid X25519MLKEM768 first and classical groups after
  it ([ADR-0170](https://github.com/firestoned/banlieue/blob/main/docs/adr/0170-post-quantum-tls-with-aws-lc-rs.md)).
  Peers that do not speak ML-KEM negotiate as before; no configuration
  changes.

## Behaviour changes

### A `migrationPolicy` recreate now recreates

In v0.6.0, a VM whose placement drifted (a changed selector or class, a
capability or capacity gone) under `migrationPolicy: Automatic` (the
default), or `Manual` with `banlieue.io/migrate: "true"`, had its
infrastructure CR deleted and was **never recreated**: every later pass saw
the same drift and deleted again. In v0.6.1 the controller records the new
placement first, waits while the old infrastructure CR is being deleted, then
creates the new one there (PR #255).

**Who is affected.** Any VM that has drifted and uses the default policy. A
VM caught in the v0.6.0 loop (its infrastructure CR gone, `PlacementValid`
`False`) is **recreated** at its new placement shortly after the controller
starts, with fresh disks. If that is not what you want, set
`spec.migrationPolicy: Never` on it **before** you upgrade the controller.
To list the candidates (a `Manual` VM without the annotation is listed too,
and stays where it is):

```sh
kubectl get virtualmachines -A -o json \
  | jq -r '.items[]
      | select(any(.status.conditions[]?; .type == "PlacementValid" and .status == "False"))
      | "\(.metadata.namespace)/\(.metadata.name) policy=\(.spec.migrationPolicy // "Automatic") migrate=\(.metadata.annotations["banlieue.io/migrate"] // "-")"'
```

### Provider Jobs are granted only in the build namespace

The vSphere and libvirt providers create image-import Jobs; their
ClusterRoles used to grant `jobs` cluster-wide. That grant is gone: the
operator now renders, per Provider, a Role and RoleBinding in the build
namespace (`banlieue-imagebuild` by default) with only the verbs that class
uses. A new admission policy, `deploy/admission/provider-jobs.yaml`
(fail-closed), admits a Job written by a `banlieue-provider-*` identity only
in that namespace, only as `banlieue-import` or `banlieue-azure-publisher`,
and only with a restricted pod. The vSphere import Job gained a restricted
`securityContext` to pass it.

**What to do.** Nothing, if the operator manages your providers: it renders
the Role on its next pass. A **standalone** vSphere provider needs
`deploy/provider-vsphere/rbac/role-jobs.yaml` applied (step 1). A libvirt
`ProviderClass` you seeded yourself keeps the old cluster-wide `jobs` rule
until you re-seed it.

### Image builds report a measured digest

`VMImage.status.buildArtifact.checksum` is now the sha256 of the disk the
image builder produced, measured by a short-lived digest Job, instead of a
copy of the source's declared checksum
([ADR-0010](https://github.com/firestoned/banlieue/blob/main/docs/adr/0010-vmimage-build-pipeline-imagebuilder.md),
amended 2026-11-26). A build stays `Building` with reason `DigestPending`
for the few seconds that takes. A declared sha256 or sha512 is still
verified and fails closed as `ChecksumMismatch`.

The digest Job runs the image named by the image builder's `--push-image`,
which must be a v0.6.1 image (`banlieue imagebuilder digest` does not exist
in v0.6.0). `banlieue bootstrap imagebuilder` passes it since v0.6.1; a
hand-written Deployment must set it, or every build stops at `DigestFailed`.

### Cloud Hypervisor

- Every guest has an SMBIOS system UUID, its machine uid, so
  `/sys/class/dmi/id/product_uuid` exists in the guest (PR #246). It applies
  to VMs created after the host's provider is upgraded.
- `install` gains `--isolate-ports` for sandbox network classes, and
  `preflight` refuses an isolated class without the sandbox segment's host
  rules ([ADR-0144](https://github.com/firestoned/banlieue/blob/main/docs/adr/0144-sandbox-segment-and-egress-gateways-on-a-cloud-hypervisor-host.md),
  [sandbox segment guide](../providers/cloud-hypervisor-sandbox-segment.md)).
  Hosts without an isolated class are unaffected.
- The OpenBao host-CA path sends `max_path_length` as the string `"0"`, as
  OpenBao's ACL requires (PRs #250, #256).

## Order of operations

```sh
export BANLIEUE_VERSION=v0.6.1
git clone --branch "$BANLIEUE_VERSION" --depth 1 https://github.com/firestoned/banlieue
cd banlieue
DIGEST=$(crane digest ghcr.io/firestoned/banlieue:"$BANLIEUE_VERSION")
```

Then, as for every release: CRDs and RBAC, admission policies, the platform
Deployments, the in-cluster providers, the Cloud Hypervisor hosts.

### 1. CRDs and RBAC

**With `banlieue bootstrap operator`**, run the v0.6.1 binary with the flags
you installed with, after reviewing the diff:

```sh
banlieue bootstrap operator --version "$BANLIEUE_VERSION" --image-digest "$DIGEST" --dry-run \
  | kubectl diff --server-side -f -
banlieue bootstrap operator --version "$BANLIEUE_VERSION" --image-digest "$DIGEST"
```

**From the manifests:**

```sh
kubectl apply --server-side -f deploy/crds/
kubectl wait --for=condition=Established --timeout=60s -f deploy/crds/
kubectl apply -R -f deploy/controller/rbac/
kubectl apply -R -f deploy/operator/rbac/
kubectl apply -R -f deploy/imagebuilder/rbac/
```

| File | Change |
| --- | --- |
| `deploy/controller/rbac/clusterrole.yaml` | `azurevirtualmachines`, its `/status`, `/finalizers`, `azurevirtualmachinetemplates` |
| `deploy/operator/rbac/clusterrole.yaml` | the Azure machine kinds; `pods` `list` (it grants the Azure publisher's Role, and RBAC refuses a grant the grantor does not hold) |
| `deploy/imagebuilder/rbac/role.yaml` | comments only (the same Job verbs cover the digest Job) |
| `deploy/imagebuilder/rbac/serviceaccount-azure-publisher.yaml` (new) | `banlieue-azure-publisher`, the publish Job's identity; no RBAC |
| `deploy/provider-vsphere/rbac/clusterrole.yaml`, `deploy/provider-libvirt/rbac/clusterrole.yaml` | `jobs` removed |
| `deploy/provider-vsphere/rbac/role-jobs.yaml` (new) | the standalone vSphere provider's Job verbs, in the build namespace |
| `deploy/provider-azure/rbac/clusterrole.yaml` (new) | the Azure provider |
| `deploy/capi/clusterrole-aggregate.yaml` | the Azure machine kinds, for Cluster API users |

**Verify:**

```sh
kubectl get crd azurevirtualmachines.infrastructure.banlieue.io
kubectl explain virtualmachine.status.placementFailures
```

### 2. Admission policies

Apply after the CRDs, since they name the new fields:

| File | Change |
| --- | --- |
| `provider-jobs.yaml` (new) | the provider Job floor described above; **fail-closed**. Edit `variables.buildNamespace` if yours is not `banlieue-imagebuild` |
| `provider-connection.yaml` | `https://` for azure; an `azure` Provider must set `spec.connection.azure` and must not set `credentialsRef` |
| `provider-azure-governance.yaml` (new) | an `azure` Provider's extra tags and disk encryption set ([ADR-0172](https://github.com/firestoned/banlieue/blob/main/docs/adr/0172-azure-policy-governed-subscriptions.md)) |
| `vmimage-cloud-install-mode.yaml` (new) | no `installMode: deferred` on a VMImage with an azure source |
| `machine-provider-ref.yaml` | covers `AzureVirtualMachine` and its template |

```sh
kubectl apply -f deploy/admission/provider-jobs.yaml
kubectl apply -f deploy/admission/provider-connection.yaml
kubectl apply -f deploy/admission/machine-provider-ref.yaml
# only if you will serve Azure:
kubectl apply -f deploy/admission/provider-azure-governance.yaml
kubectl apply -f deploy/admission/vmimage-cloud-install-mode.yaml
```

### 3. Controller, operator and image builder

Covered by `bootstrap operator` above. From the manifests, set the image of
`deploy/controller/`, `deploy/operator/` and `deploy/imagebuilder/` to
`ghcr.io/firestoned/banlieue:v0.6.1@$DIGEST`, and make sure the image
builder's `--push-image` names the same image (see
[measured digest](#image-builds-report-a-measured-digest)).

To build images for Azure, the image builder also needs the guest helper,
which the release publishes by digest (`azure-guest-image.txt` in the release
assets, and in the release notes):

```sh
banlieue bootstrap imagebuilder --version "$BANLIEUE_VERSION" --image-digest "$DIGEST" \
  --azure-guest-image "$(curl -fsSL https://github.com/firestoned/banlieue/releases/download/$BANLIEUE_VERSION/azure-guest-image.txt)"
```

### 4. In-cluster providers

The seeded `ProviderClass`es move to v0.6.1 with `bootstrap operator`. If you
manage your classes, set `spec.image.tag: v0.6.1` on each, as in
`examples/10-providerclass-vsphere.yaml`.

### 5. Cloud Hypervisor hosts

Upgrade each host's provider as in
[the 0.6.0 guide](upgrading-to-0.6.md#5-cloud-hypervisor-hosts), with
`BANLIEUE_VERSION=v0.6.1`.

### Rolling back

v0.6.0 binaries run against v0.6.1 CRDs: the new fields are optional and
v0.6.0 ignores them. Roll back the Deployments and `ProviderClass` tags,
and keep the CRDs. Remove `provider-jobs.yaml` only if you also restore the
v0.6.0 provider ClusterRoles, since a v0.6.0 vSphere import Job does not
carry the restricted `securityContext` the policy requires. A rollback
brings back the v0.6.0 recreate loop.

## New: the Azure provider

Opt-in. A cluster anywhere (on-prem included) runs VMs in an Azure
subscription through workload identity federation: no Secret holds an Azure
credential. Start with the [Azure provider guide](../providers/azure.md):
federation (the cluster's ServiceAccount issuer must be publicly reachable by
Entra ID), the resource groups and custom roles in `deploy/provider-azure/`,
publishing a VMImage into a Compute Gallery, and a first VM.

## For developers

`banlieue-api` 0.6.1 adds the Azure types and fields above; nothing was
removed or retyped. Pin `banlieue-api = "=0.6.1"`
([Rust API crate](../developer/rust-api-crate.md)).
