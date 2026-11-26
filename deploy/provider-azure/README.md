<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Azure provider: Kubernetes RBAC and Azure role

| File | What it is |
| --- | --- |
| `rbac/clusterrole.yaml` | The provider's shared ClusterRole. `banlieue bootstrap operator` installs it; the operator binds it to each azure `Provider`'s ServiceAccount. It grants **no** Secret or ConfigMap access ([ADR-0159](../../docs/adr/0159-cloud-credentials-by-workload-identity-federation.md) Decision 1). |
| `role-definition.json` | An Azure custom role with exactly the actions the runtime provider calls ([ADR-0159](../../docs/adr/0159-cloud-credentials-by-workload-identity-federation.md) Decision 3, [ADR-0164](../../docs/adr/0164-azure-inframachine-and-a-first-party-arm-client.md)). |
| `publisher-role-definition.json` | The image publisher's own Azure custom role, for the publish Job ([ADR-0162](../../docs/adr/0162-publishing-a-vmimage-into-a-cloud.md) Decision 1). Assigned to a **separate** identity at the gallery's resource group. |
| `disk-encryption-set-reader-role-definition.json` | Only when the `Provider` names a disk encryption set (customer-managed keys, [ADR-0172](../../docs/adr/0172-azure-policy-governed-subscriptions.md) Decision 6): `Microsoft.Compute/diskEncryptionSets/read`, assigned **on the set** to both identities. |

## Credential

The provider's only cloud credential is a projected ServiceAccount token
(audience `api://AzureADTokenExchange`, one hour, refreshed by the kubelet),
mounted at `/var/run/secrets/azure/tokens/azure-identity-token` and named by
`AZURE_FEDERATED_TOKEN_FILE` (the provider's `--federated-token-file` flag,
which defaults to that path). The provider re-reads the file on every
exchange. It is exchanged with Entra ID for an ARM token as
the identity in `Provider.spec.connection.azure` (`tenantId`, `clientId`). An
azure `Provider` sets no `credentialsRef`; admission refuses one
(`deploy/admission/provider-connection.yaml`).

The Entra federated credential's subject is the provider pod's
ServiceAccount:

```text
system:serviceaccount:<provider namespace>:banlieue-provider-<providerClassRef.name>-<provider name>
```

for example `system:serviceaccount:banlieue-system:banlieue-provider-azure-az-cc`
for a `Provider` named `az-cc` of class `azure` in `banlieue-system`. The name
is cut to 63 characters with a hash suffix when longer; read it from the
cluster (`kubectl -n <ns> get sa -l banlieue.io/provider=<name>`) rather than
deriving it by hand. A class with `workloadNamespace` set puts the
ServiceAccount there instead. A statically installed provider
(`banlieue bootstrap provider azure`) runs as `banlieue-provider-azure` in the
install namespace.

## Assigning the role

Edit `AssignableScopes` to your subscription and resource group (the file
carries a placeholder), create the role, and assign it to the identity at the
Provider's resource group (`spec.connection.azure.resourceGroup`):

```sh
az role definition create --role-definition deploy/provider-azure/role-definition.json
az role assignment create --assignee <clientId> \
  --role "banlieue Azure provider" \
  --scope /subscriptions/<subscriptionId>/resourceGroups/<resourceGroup>
```

Two resources usually live **outside** that resource group, and need their own,
narrower assignments there:

- **The subnet** (`capabilities.networkClasses[].target.subnetId`): the
  identity needs `Microsoft.Network/virtualNetworks/subnets/join/action` on
  the subnet or its virtual network's resource group.
- **The image gallery** (a `VMImage` `Template` source): the identity needs
  `Microsoft.Compute/galleries/images/versions/read` on the gallery.

Add those scopes to `AssignableScopes`, or create a second, single-action role
for each.

`Microsoft.Compute/locations/usages/read` and `Microsoft.Compute/skus/read`
(quota and SKU availability, ADR-0161) are calls on subscription-scoped URLs
(`/subscriptions/<id>/providers/Microsoft.Compute/...`). In banlieue's live
test (roadmap 23, phase A) both reads succeeded for an identity whose only
role was assigned at resource-group scope; no subscription-scoped assignment
was needed. That is an observation, not a documented Azure guarantee. If your
tenant refuses them (`AuthorizationFailed` naming one of those actions, and
the `Provider` reporting `Ready=False`, reason `Unauthorized`), the fallback
is a second, read-only role holding just those two actions, assigned at the
subscription.

## The image publisher

A `VMImage` with an `azure` `Url` source is published into the Compute Gallery
the `Provider` names in `spec.connection.azure.imageGallery` by a Job in the
imagebuild namespace (`banlieue-imagebuild`), not by the provider
([ADR-0162](../../docs/adr/0162-publishing-a-vmimage-into-a-cloud.md)
Decisions 1 and 8). The Job runs as the ServiceAccount
`banlieue-azure-publisher` (`deploy/imagebuilder/rbac/serviceaccount-azure-publisher.yaml`),
which holds no Kubernetes RBAC and mounts no API server token. Its projected
token is exchanged with Entra ID for the identity in
`spec.connection.azure.publisher.clientId`, whose federated credential
trusts:

```text
system:serviceaccount:banlieue-imagebuild:banlieue-azure-publisher
```

Give that identity `publisher-role-definition.json` at the gallery's resource
group, and nothing else:

```sh
az role definition create --role-definition deploy/provider-azure/publisher-role-definition.json
az role assignment create --assignee <publisher clientId> \
  --role "banlieue Azure image publisher" \
  --scope /subscriptions/<subscriptionId>/resourceGroups/<gallery resourceGroup>
```

The upload disk and snapshot are created in the gallery's resource group and
deleted once the version exists. The runtime provider keeps only
`Microsoft.Compute/galleries/images/versions/read` on the gallery, to read
what was published. `Microsoft.Compute/locations/capsOperations/read` is the
status read of a gallery's long-running operations.

## A policy-governed subscription (ADR-0172)

**Customer-managed keys.** When `spec.connection.azure.diskEncryptionSetId`
names a disk encryption set, every OS disk, upload disk, snapshot and image
version banlieue creates names it, and ARM checks that the caller may read
the set (a linked access check). The set is often in the security team's
resource group, so neither main role is widened to it: a third, single-action
role is assigned on the set itself, to both identities.

```sh
az role definition create --role-definition deploy/provider-azure/disk-encryption-set-reader-role-definition.json
for id in <clientId> <publisher clientId>; do
  az role assignment create --assignee "$id" \
    --role "banlieue Azure disk encryption set reader" \
    --scope <diskEncryptionSetId>
done
```

Edit `AssignableScopes` to the set's resource group first. The set must be in
the `Provider`'s subscription and region (Azure's restriction; admission and
the provider refuse another subscription). Granting the set's own managed
identity access to the key in Key Vault is the key owner's step; banlieue
never touches the key.

**Encryption at host.** `spec.connection.azure.encryptionAtHost: true` needs
no extra action: it is part of `virtualMachines/write`. It needs the
subscription feature, registered once by a subscription owner (banlieue does
not do it):

```sh
az feature register --namespace Microsoft.Compute --name EncryptionAtHost
az feature show --namespace Microsoft.Compute --name EncryptionAtHost --query properties.state
az provider register --namespace Microsoft.Compute
```

**Required tags.** `spec.connection.azure.tags` need no extra action: tags
are part of each resource's own `write`.

## What this role does not hold

No snapshot, disk import, image or gallery write: publishing a `VMImage` into
Azure is the image publisher's job, under a **separate** identity and role
([ADR-0162](../../docs/adr/0162-publishing-a-vmimage-into-a-cloud.md)). No
resource-group create or delete, no role assignment, no Key Vault access,
and no access to a disk encryption set beyond reading it (the separate
reader role above).
