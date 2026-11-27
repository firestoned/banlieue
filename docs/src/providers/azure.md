<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Azure Provider

!!! warning "New and alpha"
    The `azure` provider class is new (roadmap 23) and **alpha**: its API and
    behaviour may still change between releases. Its ARM client has passed
    a live test against a real subscription (token exchange, SKU and quota
    reads), but the full VM lifecycle through the provider has not yet been
    verified live the way the other classes have. Not yet supported:
    warm pools (`VirtualMachinePool`) on Azure, publishing a `VMImage` into
    Azure by banlieue (you publish the gallery image yourself), first-boot
    sealing and sealed pools, `installMode: Deferred`, public IPs, Spot VMs,
    scale sets and Confidential VMs. Current state:
    [`ROADMAPS.md`](https://github.com/firestoned/banlieue/blob/main/ROADMAPS.md),
    row 23.

The `azure` class runs a `VirtualMachine` as an Azure virtual machine. It
differs from every other banlieue backend in one way that shapes this whole
page: **there is no credential to store.** The provider authenticates with
its own Kubernetes ServiceAccount token, which Entra ID trusts through a
federated identity credential
([ADR-0159](https://github.com/firestoned/banlieue/blob/main/docs/adr/0159-cloud-credentials-by-workload-identity-federation.md)).
No client secret, no key, no `Secret` read anywhere in the cluster.

## How Azure maps onto banlieue

| banlieue | Azure |
| --- | --- |
| `Provider` | One subscription, one region (`location`), and one resource group you created, that banlieue creates VMs into ([ADR-0158](https://github.com/firestoned/banlieue/blob/main/docs/adr/0158-shape-of-a-cloud-provider-class.md)) |
| Failure domain | An availability zone of the region (`1`, `2`, `3`) that the subscription can actually use for at least one mapped size: offered there, and not restricted for this subscription. Named `<provider>-<zone>` (for example `az-prod-1`). Zone numbers are **per subscription**: two `Provider`s on different subscriptions must not assume their zone `1` is the same place |
| `VMClass` | A VM size, mapped explicitly per `Provider` in `capabilities.instanceSizes` ([ADR-0160](https://github.com/firestoned/banlieue/blob/main/docs/adr/0160-a-vmclass-becomes-an-instance-size-by-explicit-mapping.md)); never guessed from vCPUs and memory |
| Storage class | A managed-disk SKU: `target: {sku: Premium_LRS}` |
| Network class | A subnet, by resource ID: `target: {subnetId: ...}`, per zone if needed |
| `VMImage` | An Azure Compute Gallery image version, by resource ID (`kind: Template`) |
| `spec.userData` | `osProfile.customData`, at most 64 KiB |
| `tpmEnabled` | Trusted Launch: Secure Boot and a vTPM ([ADR-0165](https://github.com/firestoned/banlieue/blob/main/docs/adr/0165-trusted-launch-vtpm-identity-on-azure.md)) |
| `desiredPowerState: PoweredOff` | **Deallocate**, which stops compute billing |
| Address | The NIC's private IP; no public IP |
| Infrastructure CR | `AzureVirtualMachine` (short name `azvm`) in `infrastructure.banlieue.io`, deliberately not `AzureMachine`, which Cluster API's Azure provider (CAPZ) already owns ([ADR-0164](https://github.com/firestoned/banlieue/blob/main/docs/adr/0164-azure-inframachine-and-a-first-party-arm-client.md)) |

## 1. Prerequisites in Azure

banlieue creates VMs, NICs and OS disks, and nothing else: it never creates a
resource group, network, security group or image. Have these first
(placeholders throughout; substitute your own):

- **A subscription** and a region with availability zones.
- **A resource group for the VMs** (`banlieue-vms` below). banlieue creates
  into it and never creates or deletes it. It touches nothing in it that
  does not carry its ownership tags, but a group of its own keeps the role
  assignment tight.
- **A virtual network and subnet** the VMs' NICs join, with whatever network
  security group and egress you want. It may live in another resource group.
- **A Compute Gallery image version** to boot, built for Azure (banlieue
  builds it for you from a `Url` source; see
  [What the image must do](#what-the-image-must-do-and-what-banlieue-adds-for-you)). Its image definition
  must have:
    - `HyperVGeneration` **V2**;
    - `SecurityType=TrustedLaunch`, when VMs of a `tpmEnabled` class will
      boot it;
    - **`DiskControllerTypes=SCSI,NVMe`**: the provider creates every VM
      with the NVMe disk controller. Image definition features cannot be
      changed after creation, and the disk or snapshot the version is made
      from must declare the same controller types
      (`supportedCapabilities.diskControllerTypes`), or the version create
      fails ([ADR-0162](https://github.com/firestoned/banlieue/blob/main/docs/adr/0162-publishing-a-vmimage-into-a-cloud.md)).

    ```sh
    az sig image-definition create \
      --resource-group banlieue-images --gallery-name banlieue --gallery-image-definition kairos \
      --publisher example --offer kairos --sku amd64 \
      --os-type Linux --os-state Generalized --hyper-v-generation V2 \
      --features "SecurityType=TrustedLaunch DiskControllerTypes=SCSI,NVMe"
    ```

- **Quota** for the sizes you will map, in the region and in each size's
  family. A new subscription often has only a few vCPUs per region; the
  provider publishes quota headroom as each zone's vCPU capacity
  ([ADR-0161](https://github.com/firestoned/banlieue/blob/main/docs/adr/0161-capacity-on-a-cloud-is-quota.md);
  see [Failure domains and capacity](#failure-domains-and-capacity) for
  how far the scheduler honours it today), and ARM refuses a create over
  quota. Check what a size offers where:

    ```sh
    az vm list-skus --location <region> --size Standard_D2s_v5 --output table   # zones, and restrictions
    az vm list-usage --location <region> --output table                        # vCPU quota per family
    ```

## 2. Publish the cluster's ServiceAccount issuer

Entra ID must be able to fetch your cluster's OIDC discovery document and
signing keys over HTTPS, from the URL in the tokens' `iss` claim. A cluster
whose issuer is `https://kubernetes.default.svc` cannot federate. **banlieue
does not publish the issuer** (ADR-0159 Decision 2): it is a cluster-level
decision that every other federated workload shares.

A generic recipe, for any static HTTPS host (an object-storage container
with public read, a web server):

1. Pick the public URL, for example `https://oidc.example.com/clusters/prod`.
2. Set the API server's flags (on k0s, under `spec.api.extraArgs` in the
   cluster config; on kubeadm, in the API server's manifest):

    ```text
    --service-account-issuer=https://oidc.example.com/clusters/prod
    --service-account-jwks-uri=https://oidc.example.com/clusters/prod/openid/v1/jwks
    ```

    The first `--service-account-issuer` signs new tokens. To change the
    issuer without breaking tokens already issued, keep the old one as a
    second `--service-account-issuer` flag until they have expired.
3. Fetch the two documents from the cluster and upload them under that URL:

    ```sh
    kubectl get --raw /.well-known/openid-configuration > openid-configuration.json
    kubectl get --raw /openid/v1/jwks                   > jwks.json
    # serve as:
    #   https://oidc.example.com/clusters/prod/.well-known/openid-configuration
    #   https://oidc.example.com/clusters/prod/openid/v1/jwks
    ```

    Check that `issuer` and `jwks_uri` in the discovery document are the
    public URLs, and that both fetch anonymously:
    `curl -fsS https://oidc.example.com/clusters/prod/.well-known/openid-configuration`.

**Republish `jwks.json` whenever the cluster's ServiceAccount signing key
rotates**, or federation stops for every workload that uses it.

## 2a. Alternative: OpenBao signs the assertion

!!! warning "New and alpha"
    [ADR-0176](https://github.com/firestoned/banlieue/blob/main/docs/adr/0176-openbao-signs-the-federation-assertion-for-clusters-without-a-public-issuer.md)
    is accepted and its spike has passed against a real Entra tenant. The
    client, the `Provider` field and the operator's rendering are implemented
    and tested; a whole run on a cluster with a real ServiceAccount token as
    the OpenBao login, and a republish after a real key rotation, are not yet
    verified live.

Use this instead of section 2 only when the cluster's own ServiceAccount
issuer **cannot** be published. Entra ID never calls the issuer; it fetches a
discovery document and public keys once. So the signer stays on your internal
network, and only two static documents are public.

1. **OpenBao signs.** In an OpenBao namespace for banlieue, create an RS256
   key on a long rotation period, an OIDC role whose `client_id` is the cloud
   audience, a JWT auth role bound to the provider's ServiceAccount, and a
   policy that reads exactly that one token path:

    ```sh
    bao write identity/oidc/key/azure algorithm=RS256 rotation_period=2160h \
      verification_ttl=2400h allowed_client_ids='api://AzureADTokenExchange'
    bao write identity/oidc/role/azure-az-prod key=azure \
      client_id='api://AzureADTokenExchange' ttl=1h
    bao policy write azure-az-prod - <<'EOF'
    path "identity/oidc/token/azure-az-prod" { capabilities = ["read"] }
    EOF
    bao write auth/<jwt mount>/role/azure-az-prod role_type=jwt user_claim=sub \
      bound_audiences=openbao \
      bound_subject=system:serviceaccount:banlieue-system:banlieue-provider-azure-az-prod \
      token_policies=azure-az-prod token_ttl=15m token_max_ttl=15m
    ```

    One role, one policy and one entity **per Provider**: a Provider cannot
    read another's token, so it cannot assume another's cloud identity.

2. **Set the issuer, and publish two documents.** OpenBao's issuer is **only a
   scheme, a host and an optional port**; it appends its own path:

    ```sh
    bao write identity/oidc/config issuer=https://oidc.example.com
    ```

    The token's `iss` is then `https://oidc.example.com/v1/<namespace>/identity/oidc`
    (without a namespace, `https://oidc.example.com/v1/identity/oidc`). Serve
    byte copies of OpenBao's two unauthenticated responses, as
    `application/json`, at exactly these paths on that host (so the host is
    dedicated, or a proxy preserves `/v1/...`):

    ```text
    /v1/<namespace>/identity/oidc/.well-known/openid-configuration
    /v1/<namespace>/identity/oidc/.well-known/keys
    ```

    Copy them from `https://<openbao>/v1/identity/oidc/.well-known/...` with the
    `X-Vault-Namespace` header set. They hold public keys only. **Republish
    both after every key rotation**, before `verification_ttl` elapses; the
    key list already carries the next key, so a republish just before the
    following rotation never leaves the live key missing.

3. **A federated credential on the identity**, as in section 3, but with this
   issuer and `--subject <entity id>`. In a namespace the subject is
   `<entity uuid>.<namespace id>`: log in once as the Provider's ServiceAccount
   and read `entity_id` from the response. Deleting the entity changes the
   subject and breaks federation until the credential is updated. A new
   credential can take a short while to propagate, during which Entra ID
   answers `AADSTS70025`.

4. **Declare it on the `Provider`.** Nothing is inferred: the block below is
   the one place the settings live, and the operator renders them into the
   provider's pod (a second projected ServiceAccount token whose audience is
   `audience`, and the `BANLIEUE_OPENBAO_*` variables). Leave it out and
   nothing about OpenBao is rendered.

    ```yaml
    spec:
      connection:
        azure:
          openbao:
            address: https://openbao.example.com   # https only, no path
            namespace: banlieue                    # optional
            authMount: k8s-jwt
            loginRole: azure-az-prod
            tokenRole: azure-az-prod
            audience: openbao                      # default; never the cloud audience
    ```

    The complete manifest is
    [`examples/33-provider-azure-openbao.yaml`](https://github.com/firestoned/banlieue/blob/main/examples/33-provider-azure-openbao.yaml);
    the OpenBao role and policy are in
    [`deploy/provider-azure/openbao/`](https://github.com/firestoned/banlieue/blob/main/deploy/provider-azure/openbao/README.md).
    The schema refuses a non-`https` address and any name that could change a
    request path. The provider logs in, reads the token, revokes its login
    token and holds the assertion in memory only; redirects are never
    followed and an error names the step and the HTTP status, never a body or a
    token.

    **The OpenBao identity belongs to the workload**, not to a tenant
    `Provider`: the login token is the pod's. For a shared provider the block is
    read from the `Provider` that owns the workload.

    **Egress.** The provider's pod must reach OpenBao as well as Entra ID and
    ARM. The shipped NetworkPolicy allows TCP 443 to any address; for another
    port, add a rule for OpenBao's address
    ([`deploy/network-policies/provider-azure.yaml`](https://github.com/firestoned/banlieue/blob/main/deploy/network-policies/provider-azure.yaml)).

**The cost:** OpenBao joins the provider's authentication path. If it is
down, new ARM tokens stop once the cached one expires (about an hour); running
VMs are unaffected and reconciliation pauses. The default (section 2) has no
such dependency.

## 3. An identity, and a federated credential that trusts the provider

The provider acts as a **user-assigned managed identity** (or an app
registration). The operator creates the provider's ServiceAccount when you
apply the `Provider`, under a name derived from the class and the `Provider`
name:

```text
system:serviceaccount:<namespace>:banlieue-provider-azure-<provider name>
```

`<namespace>` is the `Provider`'s namespace, unless the `azure`
`ProviderClass` sets `workloadNamespace`. A name longer than 63 characters
is cut with a hash suffix, so for a long `Provider` name read it from the
cluster once the `Provider` exists:
`kubectl -n banlieue-system get sa -l banlieue.io/provider=<provider name>`.

For a `Provider` named `az-prod` in `banlieue-system`:

```sh
az identity create --name banlieue-az-prod --resource-group <identity resource group> --location <region>

CLIENT_ID=$(az identity show --name banlieue-az-prod --resource-group <identity resource group> --query clientId -o tsv)
PRINCIPAL_ID=$(az identity show --name banlieue-az-prod --resource-group <identity resource group> --query principalId -o tsv)

az identity federated-credential create \
  --name banlieue-az-prod \
  --identity-name banlieue-az-prod \
  --resource-group <identity resource group> \
  --issuer https://oidc.example.com/clusters/prod \
  --subject system:serviceaccount:banlieue-system:banlieue-provider-azure-az-prod \
  --audiences api://AzureADTokenExchange
```

The issuer, subject and audience must match exactly: Entra ID refuses a
token of any other ServiceAccount, or of the right one with another
audience. For an app registration instead, add the same three values with
`az ad app federated-credential create --id <application id> --parameters <json file>`.

The operator mounts a projected token into the provider's pod for that
audience, valid for one hour and refreshed by the kubelet, at
`/var/run/secrets/azure/tokens/azure-identity-token` (the provider's
`--federated-token-file` flag, or `AZURE_FEDERATED_TOKEN_FILE`, with that
path as the default). The provider re-reads the file on every exchange,
trades it for an ARM token when it needs one, keeps the ARM token in memory
only, and writes neither token anywhere.

## 4. The Azure role: least privilege

[`deploy/provider-azure/role-definition.json`](https://github.com/firestoned/banlieue/blob/main/deploy/provider-azure/role-definition.json)
is a custom role with exactly the actions the runtime provider calls: read,
write, start, deallocate and delete VMs; read and delete their disks; write
tags (to tag each VM's OS disk, which ARM leaves untagged, ADR-0173); create,
join and delete NICs; join a subnet; read a gallery image version; read
quota, SKUs and long-running operations. It holds **no** snapshot, image or
gallery write, no resource-group create or delete, no role assignment and no
Key Vault access. Its README,
[`deploy/provider-azure/README.md`](https://github.com/firestoned/banlieue/blob/main/deploy/provider-azure/README.md),
is the reference for the assignments; in short:

```sh
# Edit AssignableScopes in the file to your subscription and resource groups first.
az role definition create --role-definition deploy/provider-azure/role-definition.json

# The VMs' resource group: the custom role.
az role assignment create --assignee-object-id "$PRINCIPAL_ID" --assignee-principal-type ServicePrincipal \
  --role "banlieue Azure provider" \
  --scope /subscriptions/<subscription id>/resourceGroups/banlieue-vms
```

Two resources usually live outside that group and need their own, narrower
grants there:

- **The subnet**: `Microsoft.Network/virtualNetworks/subnets/join/action`, on
  the subnet or its virtual network's resource group.
- **The gallery**: `Microsoft.Compute/galleries/images/versions/read`, on the
  gallery.

Quota and SKU reads (`Microsoft.Compute/locations/usages/read`,
`Microsoft.Compute/skus/read`) are calls on subscription-scoped URLs
(`/subscriptions/<id>/providers/Microsoft.Compute/...`). In banlieue's live
test (roadmap 23, phase A) both reads succeeded for an identity whose only
role was assigned at resource-group scope, with no subscription-scoped
assignment. That is what was observed, not a documented Azure guarantee: if
your tenant refuses them (`AuthorizationFailed` naming one of those two
actions), assign a second role holding just those two read actions at the
subscription.

## 5. The `Provider`

```yaml
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata:
  name: az-prod
  namespace: banlieue-system
  labels:
    env: cloud
spec:
  providerClassRef:
    name: azure
  connection:
    endpoint: https://management.azure.com      # a known ARM endpoint (see below)
    azure:
      tenantId: 00000000-0000-0000-0000-000000000000
      clientId: 00000000-0000-0000-0000-000000000000   # the identity's client ID
      subscriptionId: 00000000-0000-0000-0000-000000000000
      location: eastus2
      resourceGroup: banlieue-vms               # must exist
      # authorityHost: https://login.microsoftonline.com/   (default: the endpoint's cloud's)
    # No credentialsRef: admission refuses one on an azure Provider.
  capabilities:
    storageClasses:
      - name: ssd
        target:
          sku: Premium_LRS
    networkClasses:
      - name: lan
        target:
          subnetId: /subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/banlieue-net/providers/Microsoft.Network/virtualNetworks/banlieue-vnet/subnets/vms
          # acceleratedNetworking: "true"        # only with an image that leaves the VF alone
        # perZone:                               # a different subnet in one zone
        #   - zone: "2"
        #     target:
        #       subnetId: /subscriptions/.../subnets/vms-zone2
    instanceSizes:
      - vmClass: azure-small
        size: Standard_D2s_v5
        # perZone:                               # a zone that does not offer the size
        #   - zone: "3"
        #     size: Standard_D2as_v5
    # features: [vtpm]                           # assert it to schedule tpmEnabled classes here
```

The `connection.azure` fields are all required except `authorityHost`.
`failureDomainNameOverrides` is not applied on `azure`: failure domains are
always named `<provider>-<zone>`.
Admission (`deploy/admission/provider-connection.yaml`) refuses an `azure`
`Provider` without `connection.azure`, with a `credentialsRef`, or with an
endpoint that is not `https://`, and refuses `connection.azure` on any other
class.

`endpoint` receives the ARM token and `authorityHost` receives the
provider's ServiceAccount token, so neither is free text. Both are pinned to
the known Azure clouds, each endpoint paired with its own login host:

| Cloud | `endpoint` | `authorityHost` |
| --- | --- | --- |
| Public | `https://management.azure.com` | `https://login.microsoftonline.com` |
| US Government | `https://management.usgovcloudapi.net` | `https://login.microsoftonline.us` |
| China (21Vianet) | `https://management.chinacloudapi.cn` | `https://login.chinacloudapi.cn` |

Each value is matched exactly, with or without one trailing slash; no port,
path, userinfo or other host is accepted. `authorityHost` may be left unset,
which means the endpoint's own cloud's login host. Admission refuses any
other value, and so does the provider itself: a `Provider` that reaches it
with an unknown endpoint, or an authority of another cloud, gets
`Ready=False` with reason `ConnectionInvalid` and no token is sent anywhere.
The token scope follows the cloud (`<endpoint>/.default`).

A `VMClass` with no `instanceSizes` entry is **not schedulable** on this
`Provider`. A `perZone` entry overrides the size, or a network target, in
one zone; network `perZone` entries are keyed by `zone` (`"1"`, `"2"`) on a
cloud `Provider`, not by `datacenter` and `cluster`.

### In a subscription governed by Azure Policy

Enterprise subscriptions often carry Azure Policy assignments with effect
`deny` that refuse a resource banlieue would otherwise create. Three are
handled by the `Provider` itself
([ADR-0172](https://github.com/firestoned/banlieue/blob/main/docs/adr/0172-azure-policy-governed-subscriptions.md)); all three
live under `connection.azure` and apply to every resource the `Provider`
creates, including the [publish Job](#publishing-images)'s:

```yaml
spec:
  connection:
    azure:
      # ... tenantId, clientId, subscriptionId, location, resourceGroup ...
      tags:                                       # required-tag policies
        cost-centre: "1234"
        owner: platform-team
      diskEncryptionSetId: /subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/banlieue-keys/providers/Microsoft.Compute/diskEncryptionSets/banlieue-des
      encryptionAtHost: true                      # encryption-at-host policies
```

| Policy | Setting | What banlieue does |
| --- | --- | --- |
| Required tags | `tags` | Puts them on the NIC, the VM, and the publish Job's image definition, upload disk, snapshot and image version, beside its own ownership tags. The OS disk gets them too, but only once the VM exists (merged through the Tags API, ADR-0173): enough for audit and `modify` policies and cost reports, but a `deny` policy covering `Microsoft.Compute/disks` sees the disk created untagged with the VM and refuses the create (see below). |
| Customer-managed keys | `diskEncryptionSetId` | Names the disk encryption set on the OS disk (`storageProfile.osDisk.managedDisk.diskEncryptionSet`), the upload disk and snapshot (`encryption`), and the image version's replica. |
| Encryption at host | `encryptionAtHost` | Sets `securityProfile.encryptionAtHost` on every VM, beside Trusted Launch when the class asks for it. |

Rules, enforced by admission (`deploy/admission/provider-azure-governance.yaml`)
and again by the provider, which sets `Ready=False` with reason
`ConnectionInvalid` and creates nothing while one is broken:

- **banlieue's own tags always win.** A tag name starting with `banlieue-`
  is refused **in any case**: Azure tag names are case-insensitive, so
  `Banlieue-Owner-Uid` would replace the ownership tag banlieue deletes by.
  Two names that differ only in case are refused for the same reason.
- At most 45 tags (Azure allows 50 per resource; banlieue keeps five), names
  of 1 to 512 characters without `<`, `>`, `%`, `&`, `\`, `?`, `/`, values of
  at most 256.
- The disk encryption set must be a `Microsoft.Compute/diskEncryptionSets`
  resource **in the `Provider`'s subscription and region** (an Azure
  restriction). Its resource group may differ. Both identities, the
  provider's and the publisher's, need `Microsoft.Compute/diskEncryptionSets/read`
  on it: the single-action role in
  [`deploy/provider-azure/`](https://github.com/firestoned/banlieue/tree/main/deploy/provider-azure),
  assigned on the set. The set's own managed identity needs access to the key;
  that grant is the key owner's.
- Encryption at host needs the subscription feature
  `Microsoft.Compute/EncryptionAtHost`, registered once by a subscription
  owner (`az feature register --namespace Microsoft.Compute --name EncryptionAtHost`,
  then `az provider register --namespace Microsoft.Compute`), and every size
  in `instanceSizes` must support it. banlieue does not register it.

The settings apply to resources created after the change. Existing VMs are
not re-tagged, re-keyed or moved to host encryption, and an image version
published with a customer-managed key stays encrypted with it (Azure does not
allow going back).

Not handled, and a conversation with whoever owns the policies:

- **Required tags on managed disks**: the OS disk is created by Azure from
  the VM's create and carries none of its tags, so a required-tag `deny`
  policy that covers `Microsoft.Compute/disks` still refuses the VM. Exempt
  disks in the `Provider`'s resource group from that assignment, or assign
  Azure's `modify` built-in "Inherit a tag from the resource group if missing"
  there (set the tags on the resource group), which tags the disk as it is
  created.

- **`DeployIfNotExists` VM extensions** (monitoring agents, Defender): VMs are
  created with `provisionVMAgent: false` because the image reports readiness
  itself, so an extension cannot be deployed. Exempt the VM resource group,
  or put the agent in the image.
- **Denying public network access on managed disks**: the publish Job uploads
  through a SAS on the managed-disk upload endpoint. Exempt the gallery's
  resource group, or publish images by hand and use a `Template` source.

## 6. The image, the class and a VM

[`examples/31-virtualmachine-azure.yaml`](https://github.com/firestoned/banlieue/blob/main/examples/31-virtualmachine-azure.yaml)
is a complete set, with every placeholder explained: the `Provider` above,
then:

```yaml
apiVersion: banlieue.io/v1alpha1
kind: VMImage
metadata:
  name: kairos-azure
spec:
  osFamily: linux
  osDistribution: kairos
  osVersion: "3"
  architecture: amd64
  sources:
    - providerClass: azure
      kind: Template          # a gallery image version you published
      ref: /subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/banlieue-images/providers/Microsoft.Compute/galleries/banlieue/images/kairos/versions/1.0.0
---
apiVersion: banlieue.io/v1alpha1
kind: VMClass
metadata:
  name: azure-small
spec:
  hardware:
    cpus: 2                   # keep in step with the size: the size decides
    memoryMiB: 8192
    disks:
      - name: os              # the OS disk, created from the image at this size
        sizeGiB: 30
        storageClass: ssd
  network:
    interfaces:
      - name: eth0
        networkClass: lan
        ipam: {}
---
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: az-01
  namespace: banlieue-system
spec:
  classRef:
    name: azure-small
  imageRef:
    name: kairos-azure
  placement:
    providerSelector:
      matchLabels:
        env: cloud
  userData:
    configMapRef:
      name: az-01-cloud-config
  desiredPowerState: PoweredOn
```

Notes on each:

- **`VMImage`**: `ref` is the image version's full resource ID. A
  hand-published `Template` is never modified or deleted by banlieue.
  Admission refuses an image with an `azure` source and
  `spec.template.installMode: deferred`
  (`deploy/admission/vmimage-cloud-install-mode.yaml`): a cloud VM has no
  install media.
- **OS disk size**: a Kairos image lays out its partitions on first boot and
  needs a disk larger than the image itself. With a disk the image's size,
  first boot fails.
- **`tpmEnabled: true`** on a class gives its VMs Trusted Launch (Secure
  Boot and a vTPM). The image definition must declare
  `SecurityType=TrustedLaunch`. The vTPM has **no EK certificate**: Azure
  provides an attestation key (AK) certificate instead, chaining to
  Microsoft's `Azure Virtual TPM Root Certificate Authority 2023` (ADR-0165).

## Publishing images

A `VMImage` can also ask banlieue to build the image and publish it into
Azure, the way a vSphere `Url` source is built and imported
([ADR-0162](https://github.com/firestoned/banlieue/blob/main/docs/adr/0162-publishing-a-vmimage-into-a-cloud.md) Decisions 4
and 8). banlieue's `VMImage`s are the declared state; the gallery follows
them.

```yaml
apiVersion: banlieue.io/v1alpha1
kind: VMImage
metadata:
  name: kairos-azure
spec:
  osFamily: linux
  osDistribution: kairos
  osVersion: "4.4"
  architecture: amd64
  sources:
    - providerClass: azure
      kind: Url               # build it (or reuse the build) and publish it
      ref: unused-for-url-sources
      importFrom: quay.io/kairos/ubuntu:24.04-core-amd64-generic-v4.4.0@sha256:<digest>
```

The published version is named after the SHA-256 of the **built** disk,
which the image builder measures once the build finishes and records as
`status.buildArtifact.checksum` (ADR-0010, amendment 2026-11-26); the
publish Job verifies the file against it before uploading. No `checksum` is
needed on the source: an OCI-built disk's hash cannot be known in advance.
If you do declare one (`sha256:` or `sha512:` of the built disk), the build
fails closed (`ChecksumMismatch`) when the disk does not match it. While the
disk is being measured the build is `Building` (`DigestPending`) and the
Azure row reports `BuildPending`.

### What it needs

1. **A Compute Gallery you own.** banlieue never creates or deletes it. Name
   it on the `Provider`:

   ```yaml
   spec:
     connection:
       azure:
         # ...
         imageGallery:
           resourceGroup: banlieue-images
           name: banlieue
         publisher:
           clientId: 00000000-0000-0000-0000-000000000000   # the publisher identity
   ```

2. **A separate publisher identity** (`publisher.clientId`, same tenant). The
   runtime provider's identity can only read image versions; only the
   publisher can create or delete them (ADR-0162 Decision 1, ADR-0159
   Decision 3). Give it a federated credential with issuer your cluster's
   ServiceAccount issuer (section 2), audience `api://AzureADTokenExchange`
   and subject:

   ```text
   system:serviceaccount:banlieue-imagebuild:banlieue-azure-publisher
   ```

3. **The publisher role**, `deploy/provider-azure/publisher-role-definition.json`,
   assigned to the publisher identity at the gallery's resource group (see
   `deploy/provider-azure/README.md`). The runtime identity keeps
   `Microsoft.Compute/galleries/images/versions/read` on the gallery.

4. **The `banlieue-azure-publisher` ServiceAccount** in `banlieue-imagebuild`.
   `banlieue bootstrap imagebuilder` creates it; for a manifest install, apply
   `deploy/imagebuilder/rbac/serviceaccount-azure-publisher.yaml`. It holds no
   Kubernetes RBAC and mounts no API server token.

### What happens

For every `azure` `Provider` that serves the `VMImage`:

1. The image builder builds the source as a raw `cloudImage`. The build must
   not be an ISO: if a `vsphere` `Url` source is listed first, the shared
   build is an ISO and the Azure row reports `BuildKindUnsupported`. List the
   `azure` source first, or use a separate `VMImage`.
2. The provider names the version after the build's digest
   (`status.buildArtifact.checksum`, the `sha256:...` the image builder
   measured from the built disk) and the
   image definition after the `VMImage` (a `-tl` Trusted Launch definition
   when the image declares `firstBootSealing`), and reads that version.
3. If it is missing, the provider creates a publish Job in
   `banlieue-imagebuild`. The Job runs `banlieue provider azure image-publish`
   as `banlieue-azure-publisher`: it verifies the digest, uploads the disk as
   a fixed VHD, snapshots it, creates the version with the ownership tags and
   `banlieue-source-sha256`, and deletes the temporaries. A version create
   takes minutes; the row reports `Publishing` until it has succeeded.
4. Once the version is `Succeeded` and carries this `VMImage`'s owner tag and
   digest, the row is ready: `resolvedRef` is the version's resource ID,
   `sourceDigest` the digest, and `minOsDiskSizeGiB` the smallest OS disk the
   image boots on. VMs created from the `VMImage` boot that version; the
   controller refuses a row whose `sourceDigest` is not the current build's.

The check repeats on every reconcile: a version deleted in Azure by hand is
published again. A rebuild (a new digest) is a new version, never an
overwrite; the old one is listed in the row's `supersededRefs` and deleted by
a delete-mode Job once no `AzureVirtualMachine` references it.

### Deletion

A `VMImage` with an `azure` `Url` source carries the finalizer
`banlieue.io/vmimage-azure`. On delete, every version it published is deleted
once no machine references it, then the finalizer comes off. A `Provider`
that no longer exists, or no longer declares a gallery and publisher, does
not hold the delete: its versions stay in the gallery with their ownership
tags. A version banlieue did not tag, a `Template` source's version, and the
gallery itself are never touched.

### Reasons

| Reason | Meaning |
| --- | --- |
| `GalleryNotConfigured` | The `Provider` declares no `connection.azure.imageGallery` |
| `PublisherNotConfigured` | The `Provider` declares no `connection.azure.publisher` |
| `BuildPending` / `BuildFailed` | The image builder has not built the source yet, or failed |
| `BuildKindUnsupported` | The shared build is an ISO |
| `ChecksumMissing` | A `Ready` build has no `sha256:` checksum to name and verify the version by: its status was written by an image builder older than ADR-0010's 2026-11-26 amendment, which measures every build; it clears once the upgraded image builder has measured it |
| `LookupFailed` | The version could not be read from Azure (the runtime identity lacks `versions/read` on the gallery, or ARM is unreachable) |
| `Publishing` | A publish Job is running, or was just created or replaced |
| `PublishFailed` | The publish Job failed; it is retried 10 minutes after the failure. The message carries its last error |
| `VersionConflict` | A version by banlieue's name exists, but not with this `VMImage`'s owner tag and digest; it is left alone |
| `Deleting` | The `VMImage` is being deleted |

The Jobs are labelled `app.kubernetes.io/component=azure-publish`:

```sh
kubectl -n banlieue-imagebuild get jobs -l app.kubernetes.io/component=azure-publish
kubectl -n banlieue-imagebuild logs job/<job>
```

An `AADSTS70021` or `AADSTS700213` error in the Job's log means the
publisher's federated credential does not match the subject above.

## What happens when you apply it

The lifecycle ADR-0158, ADR-0161 and ADR-0164 define:

1. The operator starts one provider pod for the `Provider`, with its own
   ServiceAccount, Role and the projected token, and no Secret access.
2. The provider exchanges the token at Entra ID, reads the region's SKUs and
   quota (again every 10 minutes), and publishes one failure domain per
   usable zone, described in
   [Failure domains and capacity](#failure-domains-and-capacity).
3. `banlieue-controller` schedules the `VirtualMachine` onto a zone and
   creates an `AzureVirtualMachine` beside it: size, zone, subnet, disk SKU
   and size, image version, user data, all resolved.
4. The provider adds the finalizer `banlieue.io/azurevirtualmachine`, then
   creates the **NIC** (no public IP), then the **VM**: the OS disk from the
   image, `customData`, the NVMe controller, boot diagnostics always on,
   `provisionVMAgent: false`, and `deleteOption: Delete` on the OS disk and
   the NIC so deleting the VM takes them with it. The admin user is
   `banlieue`, with password login off; Azure requires an SSH key for that,
   so the provider generates a fresh Ed25519 key per create, sends the
   public half and discards the private half. Nothing can log in with it.
   Resource names derive from the machine's UID, so a retried create is the
   same resource.
5. The VM, the NIC and the OS disk carry four tags:

    | Tag | Value |
    | --- | --- |
    | `banlieue-owner-uid` | the owning object's `metadata.uid` |
    | `banlieue-owner-kind` | the kind that created it (`AzureVirtualMachine`) |
    | `banlieue-provider` | `<namespace>.<name>` of the `Provider` |
    | `banlieue-owner-namespace` | the `AzureVirtualMachine`'s namespace |

    ARM takes no tags on the OS disk in the VM create and copies none of the
    VM's onto it, so the disk is named from the UID
    (`banlieue-<uid>-osdisk`) and, once the VM has provisioned, the provider
    merges the tags onto it through the Tags API and records
    `status.osDiskTagged` (ADR-0173). A tag write that fails (for example, a
    role without `Microsoft.Resources/tags/write`) is retried on later
    passes and never holds the machine back; the disk still goes with the
    VM by `deleteOption: Delete`. A VM created by an earlier version keeps
    its ARM-named, untagged disk. banlieue deletes only a resource whose `banlieue-owner-uid` names the
    object it is deleting for, and adopts its own tagged NIC or VM rather
    than creating a second one. A name is never proof of ownership: a NIC
    or VM with the machine's name but another owner's tag, or no tag, is
    left alone and reported as `ForeignResource`. Deleting such a machine
    does not delete that VM: the finalizer is removed as if the machine's
    own VM were gone, with a warning in the provider log, and a NIC of the
    machine's that the foreign VM holds stays with it.
6. Long-running operations are recorded on the `AzureVirtualMachine`'s
   status and observed on later reconciles; no worker blocks on one. ARM's
   `Retry-After` is honoured, deletes included.
7. The NIC's private address appears in status; `providerID` is
   `azure:///subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.Compute/virtualMachines/<name>`.

```sh
kubectl get virtualmachine,azvm -n banlieue-system
kubectl get azvm -n banlieue-system -o wide          # adds the providerID
```

**Deleting** the `VirtualMachine` deletes the VM (its NIC and OS disk go
with it), then lists the resource group and deletes any resource still
tagged with the machine's UID, such as the NIC a refused VM create leaves
behind; a resource already gone is success. An untagged disk named like the
VM is never deleted, only logged. The finalizer is removed once nothing
tagged remains.

**The orphan sweep** runs from the `Provider` reconcile every 10 minutes,
per `Provider`. It lists the `Provider`'s resource group and deletes a VM,
NIC or disk only when all of these hold: it is tagged for this `Provider`
(`banlieue-provider`); its `banlieue-owner-kind` is `AzureVirtualMachine`;
its `banlieue-owner-uid` matches no `AzureVirtualMachine` the provider can
see; and it was created at least 15 minutes ago, so a create in flight is
never swept. A provider that serves only its home namespace (no
`namespaceSelector`) also requires `banlieue-owner-namespace` to be that
namespace, since it cannot see machines anywhere else. A pass deletes at
most 3 resources and stops early if ARM throttles it. If listing the
machines fails, the pass is skipped, because every resource would read as
an orphan. The sweep never touches an untagged resource, one tagged for
another `Provider`, or anything outside the `Provider`'s resource group. It
logs `orphan sweep finished` with counts and exports two counters on the
provider's `/metrics`, labelled with the `Provider` (`<namespace>.<name>`):

| Metric | `result` |
| --- | --- |
| `banlieue_azure_orphan_sweeps_total` | `completed`, `throttled` (ARM stopped the pass), `skipped` (a listing failed, so nothing was judged) |
| `banlieue_azure_orphan_sweep_resources_total` | `found` (orphans past the grace), `deleted`, `failed`, `deferred` (left for the next pass by the 3-per-pass limit) |

A steady `found` with no `deleted` means the deletes are failing; a rising
`skipped` means the provider cannot list its machines or resource group.

**Powering off** (`desiredPowerState: PoweredOff`) **deallocates** the VM:
compute billing stops and the disk and NIC are kept. A stopped but still
allocated VM is not yet `PoweredOff`. `Suspended` is refused
(`Ready=False`, reason `InvalidSpec`).

## Status, conditions and reasons

### Failure domains and capacity

Each failure domain the `Provider` publishes has:

- `name` `<provider>-<zone>`, cut to 63 characters with a hash suffix when
  longer, and the labels `name` (that name) and `zone` beside the
  `Provider`'s own labels;
- `attributes.raw`: `zone`, `location`, `resourceGroup`, `endpoint`, and
  `vmClasses` (the comma-separated `VMClass`es whose mapped size is usable
  in that zone);
- the storage classes whose target in that zone names a `sku`, the network
  classes whose target names a `subnetId`, and the `Provider`'s `features`;
- `capacity.cpus`: the vCPU quota **headroom**, the smallest of the
  regional headroom and the family headroom of every mapped size usable in
  the zone. Regional quota is shared by every zone, so every zone publishes
  the same figure; it is what remains, not a total.

The scheduler treats this figure as quota, not as a host's CPUs: it is never
scaled by `scheduling.cpuOvercommitRatio`, VMs already provisioned are not
subtracted (they are inside the headroom Azure reported), and VMs placed on
the `Provider` but not yet provisioned are subtracted whichever zone they
went to, so two zones repeating the same regional figure cannot together
take twice the quota. The figure is refreshed every 10 minutes, so it can
still lag a quota change or VMs created outside banlieue; ARM is the hard
stop, and a create over quota fails with `QuotaExceeded`.

### `Provider`

| `Ready` | Reason | Meaning |
| --- | --- | --- |
| `True` | `Reconciled` | Every mapped size, storage class and network class is usable in some zone |
| `False` | `CapabilitiesIncomplete` | A mapped size is offered in no zone or restricted in every zone, no `instanceSizes` is declared, or a storage or network class has no `sku` or `subnetId` target in any usable zone. The message lists each. Zero quota headroom does not cause this |
| `False` | `Throttled` | ARM throttled the SKU and quota read; the last published failure domains are kept and the read is retried after ARM's `Retry-After` |
| `False` | `Unauthorized` | Entra ID refused the token exchange, or ARM answered 401 or 403. No failure domains are published |
| `False` | `ConnectFailed` | ARM or Entra ID could not be reached, or answered with another error. No failure domains are published |
| `False` | `ConnectionInvalid` | `spec.connection.azure` is missing, `endpoint` or `authorityHost` is not a known Azure cloud's pair, or ARM refused the request as invalid. No failure domains are published |

### `VMImage`

An `azure` `Template` source is not checked against ARM: the provider makes
no call. When `ref` is shaped like a gallery image version resource ID, the
`Provider`'s row is `ready: true`, with `zones` empty (ready in every zone)
and `resolvedRef` set to that ID. A wrong but well-formed ID surfaces at the
VM create, as `CreateRefused` on the machine. Otherwise the row is not ready,
with reason `InvalidRef`, or `UnsupportedSourceKind` for a source kind other
than `Template` or `Url`. A `Url` source's reasons are listed under
[Publishing images](#reasons).

### `AzureVirtualMachine`

Three conditions:

- **`Ready`**: `True` (`VMReady`) once the VM exists, its provisioning
  succeeded, and it is at its desired power state. Otherwise `False`, with
  the reason of whatever holds it: `Creating`, `OperationInProgress`,
  `Provisioning`, `AwaitingPowerState`, `ProvisioningFailed`,
  `OperationFailed`, `Throttled`, `ArmError`, `InvalidSpec`, or a create
  failure reason below.
- **`GuestReady`**: `True` (`GuestReportedReady`) when ARM's provisioning
  state is `Succeeded`; `False` with `AwaitingGuest` before that, or with
  ARM's own failure reason (such as `OSProvisioningTimedOut`) when
  provisioning failed.
- **`VMCreated`**: `True` (`VMExists`) once the VM exists. `False` after a
  refused or failed create, with `ZoneUnavailable` (the size is not
  available, or has no capacity, in this zone), `QuotaExceeded`,
  `CreateRefused` (anything else ARM refused: image, identity, disk
  controller), `ForeignResource` (a NIC or VM with the machine's name is
  tagged for another owner, or not tagged), or `OperationFailed`.

ARM writes are never waited on: an operation ARM runs asynchronously is
stored in `status.pendingOperation` and polled on the next reconcile, so a
restarted provider resumes it instead of starting a second one. ARM's
`Retry-After` is honoured, deletes included.

### A zone that refuses the VM

A create that Azure refuses for the zone (`ZonalAllocationFailed`,
`AllocationFailed`, `SkuNotAvailable`, the `Overconstrained` codes) is
`VMCreated=False`, reason `ZoneUnavailable`. The provider never moves a
machine; `banlieue-controller` does
([ADR-0173](https://github.com/firestoned/banlieue/blob/main/docs/adr/0173-an-allocation-failure-re-places-the-machine-in-another-zone.md)):

1. It records the failure on the `VirtualMachine`'s
   `status.placementFailures` (`providerName`, `failureDomain`, `reason`,
   `failures`, `lastFailureTime`; at most 8 entries, cleared once the VM
   has provisioned).
2. It schedules again without that zone and without any zone still
   excluded by an earlier failure. A zone is excluded for 5 minutes after
   its first failure, doubling with each further one, at most an hour.
3. If another placement fits, it records it in `status.scheduled`, sets
   `PlacementValid=False` (reason `ZoneUnavailable`) and `Ready=False`
   (reason `ReplacingAfterAllocationFailure`), and deletes the
   `AzureVirtualMachine`. The finalizer removes the NIC and anything the
   failed create left; the next pass creates a new `AzureVirtualMachine`
   in the new zone. This happens whatever the `migrationPolicy`, because no
   VM ever ran; a machine that has provisioned is never moved this way.
4. If nothing else fits (one zone, a selector pinning the VM to that zone,
   or every other zone excluded too), the machine stays and the provider
   retries it in place.

**A refused create waits** before it is tried again in place: 5 minutes
after the first failure, doubling with each consecutive failure
(`status.createFailures`) to at most an hour. A change to the
`AzureVirtualMachine`'s spec (a new generation) retries at once and starts
the count again, and the count resets once the VM exists. This applies to
every create failure (`ZoneUnavailable`, `QuotaExceeded`, `CreateRefused`,
`ForeignResource`, `OperationFailed`); throttling and transient ARM errors
keep their short retry and ARM's `Retry-After`.

## `GuestReady` on Azure, and what the image must do

On Azure, `GuestReady` is the VM's **provisioning state**
([ADR-0163](https://github.com/firestoned/banlieue/blob/main/docs/adr/0163-guestready-on-a-cloud.md)).
A VM from a generalized image stays `Creating` until something in the guest
tells Azure's host-local WireServer it is ready; Azure then moves it to
`Succeeded`, and banlieue sets `GuestReady=True` from that, reading through
ARM only. There is no channel into the guest and no credential in it. If
nothing reports, Azure fails the VM with `OSProvisioningTimedOut` after
about 20 minutes, however healthy it is, and banlieue sets `GuestReady=False`
with that reason.

### What the image must do, and what banlieue adds for you

The provider sets `provisionVMAgent: false` and runs no Azure agent, so the
image itself must (ADR-0163 Decision 3) read `customData` locally, apply it
before reporting, report ready to WireServer only from an installed boot,
leave the accelerated-networking VF unmanaged, start no QEMU guest agent on
Hyper-V and log to `/dev/kmsg`.

**An image banlieue builds gets all of it automatically**
([ADR-0171](https://github.com/firestoned/banlieue/blob/main/docs/adr/0171-azure-guest-layer.md)).
When a `VMImage` has any `azure` source, the image builder adds two things to
its build, whatever the Kairos source image (a stock flavour, your own base
image, a gateway image):

- **`banlieue-azure-guest`**, a 453 KiB static Rust binary, at
  `/usr/bin/banlieue-azure-guest`. It comes from a digest-pinned OCI image
  that kairos-operator unpacks onto the image's root filesystem at build
  time (`spec.artifacts.bundles`); nothing is downloaded at boot.
- **A cloud-config layer**, merged after your `cloudConfigs` and after the
  first-boot sealing layer, so none of yours can remove it:

  | Stage | What it does |
  | --- | --- |
  | `rootfs.before`, `initramfs` | writes `/etc/systemd/network/10-banlieue-azure-vf-unmanaged.network`, so the VF (`mana`, `mlx4_core`, `mlx5_core`) is never configured, in the initramfs or the real root |
  | `initramfs` | masks `qemu-guest-agent.service` for the boot, and installs `banlieue-azure-ready.service` |
  | `boot` | `banlieue-azure-guest customdata`: mounts the provisioning disk read-only, takes `CustomData` out of `ovf-env.xml` (at most 64 KiB, and only a `#cloud-config` document), and writes it to `/oem/91_azure_customdata.yaml`, mode 0600 |
  | after `cos-setup-network.service` | `banlieue-azure-ready.service` runs `banlieue-azure-guest ready` on an active or passive boot only: goal state, then `Health` `Ready` to `168.63.129.16`, retried with back-off for up to 15 minutes |

Every step is gated on Azure's DMI chassis asset tag, so the same image
behaves exactly as before on libvirt, Cloud Hypervisor or vSphere. An image
without an `azure` source is built byte for byte as before.

The order is what makes "ready" mean "configured as asked": a raw image's
first boot is a recovery boot that writes `/oem/91_azure_customdata.yaml`
before AuroraBoot's automatic reset, the reset keeps `COS_OEM`, and the first
active boot applies your user data from its very first stage; the report
waits for Kairos's last boot stage.

The image builder needs the helper image's reference, pinned by digest.
banlieue publishes the image, `ghcr.io/firestoned/banlieue-azure-guest`
(linux/amd64, Cosign-signed, with build provenance and an SBOM; ADR-0171,
*Amendment: the release workflow publishes the helper image*), and every
release states the digest it pushed: in the release notes and in the
`azure-guest-image.txt` asset. Take it from there:

```sh
curl -fsSL https://github.com/firestoned/banlieue/releases/download/v<version>/azure-guest-image.txt
```

Check the signature before you use it; the image is signed keylessly by the
release workflow:

```sh
cosign verify \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github\.com/firestoned/banlieue/\.github/workflows/build\.yaml@' \
  ghcr.io/firestoned/banlieue-azure-guest@sha256:<digest>
```

For a build from `main` (tags `latest`, `main-<date>`, `sha-<short>`), resolve
the tag yourself with `crane digest
ghcr.io/firestoned/banlieue-azure-guest:<tag>`. On a cluster that mirrors
images, copy it by digest (`crane copy <source>@sha256:<digest>
<mirror>/banlieue-azure-guest`) and use the mirror's reference; the digest
does not change. Building your own (`make azure-guest-image`) still works for
an unreleased change.

A kustomize install sets the reference in the ConfigMap:

```yaml
# deploy/imagebuilder/configmap.yaml
BANLIEUE_AZURE_GUEST_IMAGE: "ghcr.io/firestoned/banlieue-azure-guest@sha256:<digest>"
```

A `banlieue bootstrap` install renders its own Deployment, which does not
read that ConfigMap, so pass the reference to the bootstrap instead:

```sh
banlieue bootstrap imagebuilder --image-digest sha256:<banlieue digest> \
  --azure-guest-image ghcr.io/firestoned/banlieue-azure-guest@sha256:<digest>
```

The bootstrap also passes `--push-image` set to the banlieue image it
installs, so the digest and publish Jobs run the same build as the image
builder itself.

Without it, a `VMImage` with an `azure` source fails its build with
`AzureGuestImageUnset` instead of producing an image that would time out on
Azure; a tag instead of a digest stops the image builder at start-up.

#### What your base image must not do

The layer works with any Kairos image that does not undo it. Do not:

- configure the VF from a networkd file that sorts before
  `10-banlieue-azure-vf-unmanaged.network`;
- install another Azure provisioning agent (`waagent`, or cloud-init with the
  Azure datasource): two agents would both consume customData and report;
- ship your own `/usr/bin/banlieue-azure-guest`, or mask
  `banlieue-azure-ready.service`;
- mount `COS_OEM` read-only, or block root's traffic to `168.63.129.16`
  (a host firewall must allow it for uid 0);
- make customData anything but a `#cloud-config` document: anything else is
  refused and logged, never written.

**A hand-published image** (a `Template` source) is not built by banlieue and
gets none of this: it must carry the same pieces itself, or be rebuilt from a
`Url` source.

A first boot (recovery, layout, reset, reboot, user data, readiness) must
finish inside Azure's provisioning window. In the roadmap 23 spike, create to
`Succeeded` took 177 to 330 s, nearly all of it the one-off first boot; an
active boot reported ready in about 18 s.

## Troubleshooting

Boot diagnostics are always on, so the serial log is there when a VM fails:
`az vm boot-diagnostics get-boot-log --resource-group banlieue-vms --name <vm name>`.

| Symptom | Cause |
| --- | --- |
| `GuestReady=False`, reason `OSProvisioningTimedOut`, after about 20 minutes | The image never reported ready: it has no ready step, it could not reach WireServer (no route, often the VF took the default route on an accelerated-networking size), or first boot took longer than the window. Read the boot log. After this failure Azure refuses every update to the VM; delete and recreate it. |
| The VM boots but ignores its user data | The image reads `customData` from a metadata address, or applies it on the next boot (after the report). For a banlieue-built image, the boot log (`banlieue-azure-guest:` lines) says why: no provisioning disk, no customData, or customData refused (not a `#cloud-config` document, over 64 KiB). See [What the image must do](#what-the-image-must-do-and-what-banlieue-adds-for-you). |
| A `VMImage` with an `azure` source fails to build, reason `AzureGuestImageUnset` | The image builder has no `BANLIEUE_AZURE_GUEST_IMAGE`. Set it to the `banlieue-azure-guest` image, pinned by digest (ADR-0171; the release's `azure-guest-image.txt` asset names it): in the ConfigMap for a kustomize install, with `bootstrap imagebuilder --azure-guest-image` for a bootstrap install. |
| A zone missing from the `Provider`'s failure domains, or `NotAvailableForSubscription` | The size is offered in that zone but restricted for this subscription. `az vm list-skus --location <region> --size <size>` shows the restrictions. Map another size for that zone with `instanceSizes[].perZone`, or request access. |
| A `VirtualMachine` stays unscheduled on Azure, reason `insufficient quota: vCPUs ...` | The published quota headroom (`capacity.cpus`) does not fit the VM. Raise the region's or the size family's vCPU quota (`az vm list-usage` shows usage and limit); quota is an action outside banlieue. |
| `VMCreated=False`, reason `QuotaExceeded` | The scheduler admitted the VM but ARM refused it over quota (see [Failure domains and capacity](#failure-domains-and-capacity)). The create is retried after 5 minutes, then 10, 20, 40 and every hour (`status.createFailures` counts); raise the quota or delete the `VirtualMachine`. |
| `VMCreated=False`, reason `ZoneUnavailable` (ARM's `ZonalAllocationFailed`, `SkuNotAvailable`) | Azure has no capacity for, or does not offer, that size in that zone right now. The controller moves the machine to another zone when its placement allows ([A zone that refuses the VM](#a-zone-that-refuses-the-vm)); the `VirtualMachine` shows `PlacementValid=False`, reason `ZoneUnavailable`, and lists the zone in `status.placementFailures`. |
| `VMCreated=False`, reason `ZoneUnavailable`, and the machine stays in that zone | No other placement fits: the Provider has one usable zone for the size, the `VirtualMachine`'s `failureDomainSelector` allows only this zone, or every other zone refused it lately (see `status.placementFailures`). The provider retries in place with the back-off above. Widen the selector, map another size for the zone (`instanceSizes[].perZone`), or wait. |
| `VirtualMachine` `Ready=False`, reason `ReplacingAfterAllocationFailure` | The pass that moved a refused machine; its `AzureVirtualMachine` is being deleted and a new one created in the zone `status.scheduled` names. Transient. |
| `osDiskTagged` stays unset on a running `AzureVirtualMachine` | The provider could not tag the OS disk. The usual cause is a custom role created before `Microsoft.Resources/tags/write` was added: update the role from `deploy/provider-azure/role-definition.json`. A VM created by an earlier banlieue version has an ARM-named disk and stays untagged; its disk is still deleted with it. |
| `VMCreated=False`, reason `CreateRefused` | ARM refused the create for another reason: a wrong image version ID, the identity lacking a role, a disk below the image's size. The message carries ARM's error. |
| A create fails naming disk controller types | The size is NVMe-only (or SCSI-only) and the image definition does not declare `DiskControllerTypes=SCSI,NVMe`. Image definition features are immutable: create a new definition. |
| `Ready=False`, reason `Throttled`, on the `Provider` or a machine | ARM throttles per subscription, deletes early. The provider waits as `Retry-After` says; a throttled `Provider` keeps its last failure domains. Spread bursts (mass deletes) or split across subscriptions. |
| `Provider` `Ready=False`, reason `Unauthorized`, `ConnectFailed` or `ConnectionInvalid` | The token exchange or ARM refused the identity, could not be reached, or `connection.azure` is unusable. The message carries the error; the rows below cover the usual causes. |
| The provider log shows `AADSTS700213` or `AADSTS700212` | Entra ID refused the token exchange: the federated credential's subject does not match the provider's ServiceAccount (700213), or its audience is not `api://AzureADTokenExchange` (700212). Compare the subject with `kubectl get sa`. |
| The token exchange fails to fetch the issuer, or every federated workload stopped at once | The issuer's documents are not reachable from the internet, or the published JWKS is stale after a signing-key rotation. Republish `jwks.json`. |
| `AuthorizationFailed` on a subnet or gallery | The identity has the custom role on the VMs' resource group only. Grant subnet join and gallery read where those live (step 4). |
| `AuthorizationFailed` on `skus/read` or `locations/usages/read` | Your tenant does not allow these subscription-scoped reads through a resource-group assignment. Assign those two actions at the subscription (step 4). |
| Admission refuses the `Provider` | `connection.azure` missing, a `credentialsRef` set, an endpoint that is not `https://`, or an `endpoint` or `authorityHost` outside the cloud table in step 5. Or a governance setting: a `banlieue-` tag, more than 45 tags, or a `diskEncryptionSetId` that is not a disk encryption set in the `Provider`'s subscription ([In a subscription governed by Azure Policy](#in-a-subscription-governed-by-azure-policy)). |
| `VMCreated=False`, reason `CreateRefused`, `RequestDisallowedByPolicy` | An Azure Policy `deny` assignment refused the create. The message names the policy. Required tags, customer-managed keys and encryption at host are `Provider` settings ([In a subscription governed by Azure Policy](#in-a-subscription-governed-by-azure-policy)); anything else is for the policy's owner. |
| `VMCreated=False`, reason `CreateRefused`, naming `EncryptionAtHost` | `encryptionAtHost: true` on a subscription without the `Microsoft.Compute/EncryptionAtHost` feature registered, or a size that does not support it. Register the feature, or map supported sizes. |
| `LinkedAuthorizationFailed` naming `diskEncryptionSets/read` | The identity (provider or publisher) cannot read the disk encryption set. Assign the disk encryption set reader role on the set to both. |

## Security notes

- The ARM token the provider obtains is valid for up to 24 hours, longer
  than the one-hour ServiceAccount token that bought it. The provider holds
  it in memory only; deleting the provider's pod does not revoke a token it
  already holds. Revoke by removing the federated credential or the role
  assignment.
- The provider offers the post-quantum hybrid X25519MLKEM768 first on every
  connection (ADR-0170). Neither Azure Resource Manager nor Entra ID accepts
  it yet (measured 2026-11-26), so those handshakes fall back to a classical
  group; they upgrade with no change to banlieue when Azure does
  (ADR-0159 Decision 5).
- The full analysis is in the [Threat Model](../security/threat-model.md).
