# Guide: Core Controller

This guide installs the **banlieue main controller** on a real Kubernetes
cluster from the released image `ghcr.io/firestoned/banlieue:v0.1.0`. The
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
    git clone --branch v0.1.0 --depth 1 https://github.com/firestoned/banlieue
    cd banlieue
    ```

## 1. Install the CRDs

The CRDs are generated from the Rust types and committed at the release tag.

```sh
kubectl apply -f deploy/crds/
```

```text
customresourcedefinition.apiextensions.k8s.io/providers.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/virtualmachines.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/vmclasses.banlieue.io created
customresourcedefinition.apiextensions.k8s.io/vmimages.banlieue.io created
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

- **`banlieue.io`** — full reconcile on `providers`, `virtualmachines`,
  `vmclasses`, `vmimages` (+ their `status` and the `finalizers` subresources it
  writes).
- **`infrastructure.banlieue.io`** — full reconcile on `vspheremachines` /
  `vspheremachinetemplates`, but only **watch + status** on `vsphereclusters`
  (CAPI/the operator owns their lifecycle — the controller never creates or
  deletes them).
- **`secrets`** — read-only (provider credentials, cloud-init user-data).
- **`events`** — create/patch (state transitions visible in `kubectl describe`).
- **`coordination.k8s.io/leases`** — leader election.

```sh
kubectl apply -R -f deploy/controller/rbac/
```

The ClusterRole carries the `cluster.x-k8s.io/aggregate-to-manager: "true"`
label, so a future Cluster API manager can drive the same infrastructure CRDs
without a bespoke binding.

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
```

## 5. Deploy the controller

```sh
kubectl apply -f deploy/controller/deployment.yaml
kubectl apply -f deploy/controller/service.yaml
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
          image: ghcr.io/firestoned/banlieue:v0.1.0   # pinned — never :latest
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
kubectl -n banlieue-system port-forward deploy/banlieue-controller 8081:8081 &
curl -s localhost:8081/readyz   # -> ok
```

A healthy controller logs leader acquisition and a reconcile loop that idles
until you create resources.

## What's next

The controller is running but has no backend to schedule onto. Add one:

- **[vSphere Provider guide](vsphere-provider.md)** — install the provider and
  take a `VirtualMachine` all the way to a scheduled `VSphereMachine` on vCenter.

To uninstall, delete in reverse order (`deploy/admission/`, `deploy/controller/`,
then `deploy/crds/` — deleting the CRDs removes all banlieue objects).
