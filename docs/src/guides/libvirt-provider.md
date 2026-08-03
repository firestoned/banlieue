# Guide: libvirt Provider

Register a libvirt/KVM host with banlieue and import a guest image onto it.

Unlike the vSphere provider, this one speaks **libvirt's RPC protocol
directly** — a first-party client in `banlieue-libvirt`, no `libvirt-dev`, no
`virsh` subprocess, no third-party crate (ADR-0011).
Two consequences shape everything below:

- **Mutual TLS is the only transport.** `qemu+ssh://` and `qemu+tcp://` are
  rejected at reconcile time rather than silently retried, because there is no
  ssh client to tunnel over.
- **The x509 client certificate *is* the credential.** libvirtd runs with
  `auth_tls="none"`, so there is no password anywhere in this guide.

!!! note "Scope"
    Today the libvirt provider verifies host capabilities and imports images.
    Machine provisioning (`LibvirtMachine`) is not implemented yet — see
    [Non-goals and sequencing](../reasoning/non-goals.md).

## Prerequisites

- A libvirt host reachable from the cluster, running libvirtd with TLS enabled
  on port 16514.
- The core controller and the operator installed — see
  [Core Controller](core-controller.md) and
  [Provider Lifecycle & Install](provider-lifecycle.md).
- For image import: `banlieue-imagebuilder` and the Kairos operator, per
  [Using banlieue-imagebuilder](using-banlieue-imagebuilder.md).

## 1. Enable TLS on the libvirt host

`scripts/bootstrap-libvirt-tls.sh` provisions the PKI (CA, server certificate,
client certificate), writes `/etc/libvirt/libvirtd.conf`, and restarts the
socket units in the right order.

```sh
scp scripts/bootstrap-libvirt-tls.sh kvm-1.example:/tmp/
ssh kvm-1.example 'sudo /tmp/bootstrap-libvirt-tls.sh'
```

It leaves the client credentials in `~/.config/banlieue/libvirt/` on the
machine you ran it from — **outside any repository**, mode `600`, because they
include a private key.

Confirm the host answers before involving Kubernetes:

```sh
virsh -c "qemu+tls://kvm-1.example/system" pool-list --all
```

If that hangs rather than errors, the usual cause is a firewall dropping 16514.

## 2. Create the credentials Secret and CA ConfigMap

The Secret carries the client identity; the ConfigMap carries the CA that
signed libvirtd's **server** certificate. A CA is required — a private CA is
used in every realistic deployment, so falling back to public trust roots would
only fail later and less clearly.

```sh
kubectl -n banlieue-system create secret generic libvirt-edge-creds \
  --from-file=tls.crt="$HOME/.config/banlieue/libvirt/clientcert.pem" \
  --from-file=tls.key="$HOME/.config/banlieue/libvirt/clientkey.pem"

kubectl -n banlieue-system create configmap libvirt-edge-ca \
  --from-file=ca.crt="$HOME/.config/banlieue/libvirt/cacert.pem"
```

## 3. Register the `Provider`

```yaml title="examples/02-provider-libvirt-edge.yaml (excerpt)"
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata:
  name: libvirt-edge-host-7
  namespace: banlieue-system
spec:
  providerClassRef:
    name: libvirt
  connection:
    endpoint: qemu+tls://kvm-7.edge.example/system
    credentialsRef:
      name: libvirt-edge-creds
    caBundle:
      configMapRef:
        name: libvirt-edge-ca
  capabilities:
    storageClasses:
      - name: gold          # admin asserts: on this host, gold = nvme pool
        target:
          pool: nvme-pool
      - name: standard
        target:
          pool: default
    networkClasses:
      - name: prod
        target:
          network: br-prod
```

`capabilities` is **declared, not discovered**. The provider connects and
*narrows* your declaration to what is really present, publishing the result as
`status.failureDomains[]`. A pool you named that does not exist on the host is
dropped and reported — which is the point of probing at all, since a `Provider`
reporting `Ready` without ever having reached the host is actively misleading.

```sh
kubectl apply -f examples/02-provider-libvirt-edge.yaml
kubectl -n banlieue-system get provider libvirt-edge-host-7
```

Applying the `Provider` is all that is needed: the operator creates the
Deployment, ServiceAccount, Role, RoleBinding and ClusterRoleBinding for it
(ADR-0012).

A libvirt host is a single failure boundary, so exactly one failure domain is
published per `Provider` — unlike vSphere, where a datacenter/cluster hierarchy
yields several.

## 4. Import an image

With a `VMImage` whose source has `providerClass: libvirt` and
`kind: Url`, the pipeline runs in two halves
(ADR-0010):

1. `banlieue-imagebuilder` turns the OCI reference into a raw disk on a PVC and
   publishes `status.rawDiskArtifact`.
2. This provider waits for `phase: Ready`, then creates **one import Job per
   storage pool** the `Provider` advertises.

```sh
kubectl get vmimage kairos-ubuntu-2404 \
  -o jsonpath='{.status.perProvider[*].zones[*]}' | jq
```

```sh
# Watch the transfer.
kubectl -n banlieue-system get jobs -l banlieue.io/vmimage=kairos-ubuntu-2404
kubectl -n banlieue-system logs -l banlieue.io/vmimage=kairos-ubuntu-2404 -f
```

### Why a Job

Importing a guest image moves gigabytes. A reconcile loop blocked for minutes
on I/O stops reconciling everything else, holds memory proportional to the
image, and leaves a half-written volume behind if the pod restarts.

The Job runs the **`banlieue` binary itself** — `banlieue provider libvirt
import` — not a third-party `virsh`/`qemu-img` image, so the data path stays
inside banlieue's own supply chain and the same `banlieue-libvirt` code is
exercised in both roles.

Three properties are worth knowing when reading a failure:

- **Job names are deterministic** (`import-<image>-<provider>-<pool>`). A
  re-reconcile *adopts* a running import rather than starting a second copy of
  a multi-gigabyte transfer.
- **`backoffLimit` is 1.** A partial upload is resumable only by starting over,
  so retrying forever would hammer the host for no benefit.
- **The import is idempotent.** A volume already present in the pool is left
  alone, so the one retry can finish the work rather than trip over its
  predecessor's.

### Running an import by hand

The Job's flags are stable, so a failed import can be reproduced directly:

```sh
banlieue provider libvirt import \
  --vmimage kairos-ubuntu-2404 \
  --provider libvirt-edge-host-7 \
  --provider-namespace banlieue-system \
  --pool default \
  --source /artifacts/kairos-ubuntu-2404.raw
```

It reads the `Provider` for the endpoint and TLS material rather than taking
them on the command line — passing a private key as a process argument would
expose it through `/proc` to anything sharing the namespace.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `scheme "qemu+ssh" is not supported` | This provider is TLS-only (ADR-0011). Use `qemu+tls://host/system`. |
| `Ready=False`, reason `CapabilitiesIncomplete` | A declared pool or network is not on the host. The message names which. |
| `Ready=False`, reason `ConnectFailed` | TLS or reachability. Check 16514, then reproduce with `virsh -c qemu+tls://…`. |
| `Ready=False`, reason `CredentialsUnavailable` | The Secret or CA ConfigMap is missing a key. `tls.crt` / `tls.key` / `ca.crt`. |
| Zone stuck at `BuildPending` | `status.rawDiskArtifact` is not `Ready` yet — the problem is upstream, in `banlieue-imagebuilder`. |
| Import Job `403` on `jobs` | The `ProviderClass` is missing `additionalRules`. See `examples/09-providerclass-libvirt.yaml`. |
| Import Job `Pending`, ServiceAccount not found | The Job's namespace and the provider's differ. The provider falls back to that namespace's `default` SA, which needs the grant. |

## Full schema reference

- [`Provider` API reference](../reference/api.md#provider)
- [`VMImage` API reference](../reference/api.md#vmimage)
