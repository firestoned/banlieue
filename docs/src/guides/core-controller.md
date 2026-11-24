# Guide: Core Controller

This guide installs the **banlieue main controller** on a real Kubernetes
cluster from a released image, `ghcr.io/firestoned/banlieue:$BANLIEUE_VERSION`
(see [Conventions](index.md#conventions-used-in-these-guides)). The
controller watches `VirtualMachine` resources, schedules them onto a `Provider`,
creates the backend-specific infrastructure CR (e.g. `VSphereMachine`), and
mirrors status back. It is the foundation every provider builds on — install it
first, then add a provider with the [vSphere Provider guide](vsphere-provider.md).

!!! note "What you get after this guide"
    The CRDs, the controller Deployment (running, leader-elected), least-privilege
    RBAC, and optional admission hardening. The controller alone will accept
    `VirtualMachine`s and try to schedule them, but nothing provisions until a
    `Provider` and its provider controller exist.

## Prerequisites

- A Kubernetes cluster, **1.30+** (1.30 is required only for the
  ValidatingAdmissionPolicies in step 6; the controller itself runs on 1.27+).
- `kubectl` with **cluster-admin** (you are installing CRDs, ClusterRoles, and
  cluster-scoped admission policies).
- The released manifests. Pin the repo to the tag so the YAML matches the image:

    ```sh
    git clone --branch "$BANLIEUE_VERSION" --depth 1 https://github.com/firestoned/banlieue
    cd banlieue
    ```

## 1. Install the CRDs

The CRDs are generated from the Rust types and committed at the release tag.

```sh
kubectl apply -f deploy/crds/
```

```text
customresourcedefinition.apiextensions.k8s.io/providerclasses.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/providers.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/virtualmachineclaims.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/virtualmachinemoves.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/virtualmachinepools.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/virtualmachines.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/vmclasses.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/vmimages.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/cloudhypervisormachines.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/cloudhypervisormachinetemplates.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/libvirtmachines.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/libvirtmachinetemplates.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/proxmoxmachines.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/proxmoxmachinetemplates.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/vsphereclusters.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/vspheremachines.infrastructure.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/vspheremachinetemplates.infrastructure.banlieue.io created
```

Verify the API is registered:

```sh
kubectl explain virtualmachine.spec
kubectl api-resources --api-group=banlieue.io
```

## 2. Create the namespace

All banlieue workloads run in `banlieue-system` under the Pod Security
**restricted** profile.

```sh
kubectl apply -f deploy/controller/namespace.yaml
```

```yaml
# deploy/controller/namespace.yaml
apiVersion: v1
kind: Namespace
metadata:
  name: banlieue-system
  labels:
    app.kubernetes.io/name: banlieue
    pod-security.kubernetes.io/enforce: restricted
    pod-security.kubernetes.io/audit: restricted
    pod-security.kubernetes.io/warn: restricted
```

## 3. Apply RBAC (least privilege)

The controller needs a ServiceAccount, a ClusterRole, and a binding. The role is
scoped tightly:

- **`banlieue.io`**: full reconcile on `providers`, `virtualmachines`,
  `vmclasses`, `vmimages` (+ their `status` and the `finalizers` subresources it
  writes); read plus `status` and `finalizers` on `virtualmachinepools`, and
  read, `patch`, `delete` plus `status` and `finalizers` on
  `virtualmachineclaims`.
- **`infrastructure.banlieue.io`**: full reconcile on every machine and
  machine-template kind (vSphere, libvirt, Cloud Hypervisor, Proxmox), but
  only **watch + status** on `vsphereclusters` (CAPI/the operator owns their
  lifecycle; the controller never creates or deletes them).
- **`ipam.cluster.x-k8s.io`** `ipaddressclaims` / `ipaddresses`, and read-only
  Cluster API `machines`, for the CAPI integrations.
- **`namespaces`**: read, for the namespace selector (ADR-0130).
- **`events`**: create/patch (state transitions visible in `kubectl describe`).
- **`coordination.k8s.io/leases`**: leader election, in its own namespaced
  Role (`role-leader-election.yaml`).

The ClusterRole has no `secrets` rule (security review 2026-07-31 SEC-008):
providers read their own credentials through their per-instance Roles. The one
Secret read the controller does is a namespaced Role,
`banlieue-controller-userdata` (`deploy/controller/rbac/role.yaml`), granting
`get` on Secrets and ConfigMaps in `banlieue-system` only. It resolves a
`VirtualMachine`'s `spec.userData` reference before inlining the rendered
content into the infrastructure CR (ADR-0025, ADR-0038), and reads a Cluster
API Machine's bootstrap Secret (ADR-0129). The
`virtualmachine-userdata-authorization` admission policy (ADR-0042) is what
keeps that grant from becoming a confused deputy; apply it (step 6).

```sh
kubectl apply -R -f deploy/controller/rbac/
```

This ClusterRole is **not** aggregated into Cluster API's manager. That is a
separate, smaller role, `banlieue-capi-infrastructure`
(`deploy/capi/clusterrole-aggregate.yaml`), which grants CAPI only the
infrastructure kinds (ADR-0096). See [Cluster API](cluster-api.md).

## 4. Apply configuration

Runtime configuration is a ConfigMap consumed via `envFrom`:

```sh
kubectl apply -f deploy/controller/configmap.yaml
```

```yaml
# deploy/controller/configmap.yaml (data)
RUST_LOG: "info,kube=warn,hyper=warn,tower=warn"
RUST_LOG_FORMAT: "json"            # structured logs for in-cluster
BANLIEUE_METRICS_PORT: "8080"
BANLIEUE_HEALTH_PORT: "8081"
# BANLIEUE_NAMESPACE_SELECTOR: "banlieue.io/tenant=true"   # serve tenant namespaces (ADR-0130)
# BANLIEUE_PROVIDER_NAMESPACE: "banlieue-system"           # shared providers' namespace (ADR-0133)
```

Unset, the controller serves only its own namespace; it never defaults to
every namespace. See [Multi-tenancy](multi-tenancy.md) for the two
commented-out keys.

## 5. Deploy the controller

```sh
kubectl apply -f deploy/controller/deployment.yaml
kubectl apply -f deploy/controller/service.yaml
# The manifest's tag is a reference only; run the release you cloned.
kubectl -n banlieue-system set image deployment/banlieue-controller \
  controller=ghcr.io/firestoned/banlieue:"$BANLIEUE_VERSION"
```

The Deployment runs the **single `banlieue` binary** with the `controller`
subcommand ([ADR-0004](https://github.com/firestoned/banlieue/blob/main/docs/adr/0004-single-binary-subcommand-dispatch.md)),
pinned to the released tag and hardened for the restricted PSS profile:

```yaml
# deploy/controller/deployment.yaml (excerpt)
spec:
  template:
    spec:
      serviceAccountName: banlieue-controller
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        seccompProfile: { type: RuntimeDefault }
      containers:
        - name: controller
          image: ghcr.io/firestoned/banlieue:<release>   # pinned, never :latest
          args: ["controller"]                          # role selector
          envFrom:
            - configMapRef: { name: banlieue-controller-config }
          ports:
            - { name: metrics, containerPort: 8080 }
            - { name: health,  containerPort: 8081 }
          livenessProbe:  { httpGet: { path: /livez,  port: health } }
          readinessProbe: { httpGet: { path: /readyz, port: health } }
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities: { drop: ["ALL"] }
```

Wait for it to become ready:

```sh
kubectl -n banlieue-system rollout status deploy/banlieue-controller --timeout=120s
```

## 6. (Recommended) Admission policies

banlieue ships optional [ValidatingAdmissionPolicies](https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/)
that enforce invariants the CRD schema can't express — in the API server, via
CEL, with **no webhook to run or certificates to rotate** (Kubernetes 1.30+):

| Policy | Enforces |
| --- | --- |
| `banlieue-virtualmachine-immutable-refs` | `VirtualMachine.spec.classRef` / `imageRef` are immutable after creation (changing class/image is a delete-and-recreate). |
| `banlieue-provider-immutable-class` | `Provider.spec.providerClassRef.name` is immutable. |
| `banlieue-provider-cabundle-source` | `Provider.spec.connection.caBundle` names exactly one of `inline` / `configMapRef` / `secretRef`. |
| `banlieue-provider-connection` | `Provider.spec.connection.endpoint` is `https://`, has no embedded userinfo, and `insecureSkipTLSVerify` requires an explicit opt-in annotation (CHAIN-001). |
| `banlieue-provider-credentialsref-authorization` | The principal creating a `Provider`, or changing its `spec.connection`, must itself be authorized to `get` or to `reference` the Secret named by `spec.connection.credentialsRef` (CHAIN-001, ADR-0157). A label or capability change is not checked. |
| `banlieue-providerclass-guardrails` | Bounds what a `ProviderClass` writer can hand a provider Deployment via `spec.additionalRules` / `spec.workloadNamespace` (SEC-006). |
| `banlieue-vmimage-import-source` | `VMImage.spec.sources[].importFrom` must be digest-pinned (`@sha256:`) and from an allow-listed registry (CHAIN-003). |
| `banlieue-virtualmachine-userdata-authorization` | The principal creating/updating a `VirtualMachine` must be authorized to `get` the Secret or ConfigMap named by `spec.userData` (ADR-0042). |
| `banlieue-provider-attestation-ektrustbundle` | `Provider.spec.attestation.ekTrustBundle` names exactly one of `inline` / `configMapRef` / `secretRef` (ADR-0049). |
| `banlieue-provider-attestation-hostca` | `Provider.spec.attestation.hostCa` names exactly one of `inline` / `configMapRef` / `secretRef` (ADR-0143). |
| `banlieue-provider-namespace-selector-authorization` | Setting `Provider.spec.namespaceSelector` needs `update` on `providers/namespaceselector` (ADR-0133). |
| `banlieue-machine-provider-ref` | Only the controller sets or changes a machine's `spec.providerRef.namespace` (ADR-0133). Required when shared providers serve tenant namespaces. |
| `banlieue-virtualmachineclaim-subject-authorization` | A claim's `spec.subject.id` must equal the authenticated username unless the requester is a declared broker (ADR-0047). Edit its `issuers` list before applying. |
| `banlieue-vmimage-first-boot-sealing` | `VMImage.spec.firstBootSealing` names only plain, sealable Kairos partitions and is not combined with `trustedBoot` (ADR-0088). |
| `banlieue-cloudhypervisormachine-move-fields` | Only the controller sets a `CloudHypervisorMachine`'s move fields (ADR-0127). Required when VMs are moved. |
| `banlieue-capi-machine-bootstrap-data-authorization` | Who may point a Cluster API `Machine` at a bootstrap Secret (ADR-0129). Matches nothing without Cluster API. |

`deploy/admission/virtualmachine-created-by.yaml` is a
`MutatingAdmissionPolicy` that stamps `banlieue.io/created-by` (ADR-0089) and
needs Kubernetes 1.36+; on an older API server that one file is rejected while
the rest apply. `deploy/admission/README.md` has the per-file notes (parameter
ConfigMaps to edit, the 1.34 to 1.35 variant in `compat/`).

```sh
kubectl apply -f deploy/admission/
```

Each file pairs a `ValidatingAdmissionPolicy` with a binding whose
`validationActions: ["Deny"]` enforces it. To roll out in report-only mode
first, edit the binding to `["Warn", "Audit"]`, observe, then switch to `Deny`.

Confirm a violation is rejected (after you have a VM — see the provider guide):

```sh
kubectl patch virtualmachine db-prod-01 --type=merge -p '{"spec":{"imageRef":{"name":"something-else"}}}'
# Error from server: ... spec.imageRef.name is immutable (was ubuntu-22.04-cloudinit); delete and recreate ...
```

## 7. Verify

```sh
kubectl -n banlieue-system get deploy,pods
kubectl -n banlieue-system logs deploy/banlieue-controller | head

# Leader election lease acquired:
kubectl -n banlieue-system get lease banlieue-controller

# Metrics / health are reachable in-cluster:
kubectl -n banlieue-system port-forward deploy/banlieue-controller 8081:8081 8080:8080 &
curl -s localhost:8081/readyz   # -> leader (or standby on a second replica)
curl -s localhost:8080/metrics | grep banlieue_leader
```

A healthy controller logs leader acquisition and a reconcile loop that idles
until you create resources. What each endpoint reports, and how to scrape the
metrics, is in the [Observability guide](observability.md).

## What's next

The controller is running but has no backend to schedule onto. Add one:

- **[Cloud Hypervisor Provider](cloud-hypervisor-host.md)**, the reference
  backend: prepare a bare-metal KVM host and connect it, then run VMs on it.
- **[vSphere Provider](vsphere-provider.md)**: take a `VirtualMachine` all the
  way to a running VM on vCenter.
- **[libvirt Provider](libvirt-provider.md)**: register a libvirt/KVM host over
  mutual TLS and run VMs on it.
- **[Proxmox VE Provider](proxmox-provider.md)**: register a Proxmox node with a
  least-privilege API token and run VMs as template clones.

To uninstall, delete in reverse order (`deploy/admission/`, `deploy/controller/`,
then `deploy/crds/` — deleting the CRDs removes all banlieue objects).
