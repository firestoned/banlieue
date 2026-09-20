# Guide: libvirt Provider

Register a libvirt/KVM host with banlieue, import a guest image onto it, and
run a VM.

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
    The libvirt provider verifies host capabilities, imports images, and —
    since [ADR-0050](https://github.com/firestoned/banlieue/blob/main/docs/adr/0050-libvirtmachine-domain-lifecycle.md) —
    provisions VMs through the `LibvirtMachine` CRD.

    cloud-init user-data is delivered too, as of
    [ADR-0054](https://github.com/firestoned/banlieue/blob/main/docs/adr/0054-nocloud-seed-iso-first-party.md).

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

1. `banlieue-imagebuilder` turns the OCI reference into a raw cloud image on a
   PVC and publishes `status.buildArtifact` (`kind: cloudImage`).
2. This provider waits for `phase: Ready`, then creates **one import Job per
   storage pool** the `Provider` advertises.

```sh
kubectl get vmimage kairos-ubuntu-2404 \
  -o jsonpath='{.status.perProvider[*].zones[*]}' | jq
```

```sh
# Watch the transfer. Import Jobs run in the build namespace (ADR-0016),
# where the shared artifacts PVC lives — not in the Provider's namespace.
kubectl -n banlieue-imagebuild get jobs -l banlieue.io/vmimage=kairos-ubuntu-2404
kubectl -n banlieue-imagebuild logs -l banlieue.io/vmimage=kairos-ubuntu-2404 -f
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

## 5. Run a VM

Everything above is setup. This is the part that produces a machine.

`examples/17-virtualmachine-libvirt.yaml` is a complete, applyable set: a
`VMImage`, a `VMClass` and a `VirtualMachine`. Apply it and watch both
objects:

```sh
kubectl apply -f examples/17-virtualmachine-libvirt.yaml
kubectl get virtualmachine,libvirtmachine -n banlieue-system -w
```

### What runs where

```text
VirtualMachine  ──(banlieue-controller schedules)──▶  LibvirtMachine
                                                              │
                                        (banlieue-provider-libvirt realises)
                                                              ▼
                                                        libvirt domain
```

The controller never talks to libvirt and the provider never reads a
`VirtualMachine`. They communicate only through the `LibvirtMachine` object
and its status — the CRD-only contract that lets the same provider serve a
CAPI `Machine` unchanged.

### The domain's name

A domain is named `<namespace>-<name>`, so `web-01` in `banlieue-system`
becomes `banlieue-system-web-01`.

The qualification is not decoration. One libvirt host has a single flat
domain namespace, while two Kubernetes namespaces can each hold a `web-01` —
and `DOMAIN_DEFINE_XML` is an upsert, so the second one would silently
redefine the first's domain rather than failing. vCenter has no equivalent
problem because its inventory is a folder tree.

Volume names follow from the domain name for the same reason:
`banlieue-system-web-01-os.qcow2`.

### The two disk shapes

Which one a machine gets follows from its image's `installMode`
([ADR-0040](https://github.com/firestoned/banlieue/blob/main/docs/adr/0040-deferred-install-for-vtpm-encryption.md)), not
from anything set per-VM:

| `installMode` | Shape | What the provider does |
| --- | --- | --- |
| `Immediate` | `backingVolume` | Creates a qcow2 **overlay** over the imported image. Fast. |
| `Deferred` / `Manual` | `installMedia` | Creates an **empty** qcow2 disk and attaches the image as a boot CD-ROM. The guest installs itself. |

`Deferred` is slower — a full unattended install per VM — but it is the only
shape that can produce a disk sealed to that VM's own vTPM. On libvirt it is
also the *simpler* path: no template, no backing chain, nothing to copy.

The overlay's backing format is taken from the backing volume's **name**
(`.qcow2`/`.qcow` → qcow2, otherwise raw), never from its contents. libvirt
refuses to probe a backing file's format because a raw image whose first
bytes resemble a qcow2 header would be reinterpreted as one, and the backing
file it then names can be any path the daemon can read. banlieue does not
reintroduce that.

### EFI needs no configuration

`firmware: efi` on a `VMClass` renders `<os firmware='efi'>`, which makes
libvirt select the loader and varstore template from its own firmware
descriptors. There is deliberately no OVMF path to set: Debian, Fedora and
Arch each put it somewhere different, so a hardcoded path is not portable.

Each domain gets its **own** varstore. Without that, every EFI domain on the
host would share one set of UEFI variables — and for a Secure Boot domain,
that set is the enrolled key database.

### vTPM

`tpmEnabled: true` on a `VMClass` attaches an emulated TPM 2.0 (`tpm-crb`,
backed by swtpm). Two things to know:

- **The Provider must advertise it.** The scheduler rejects a candidate whose
  failure domain does not list `vtpm` in `capabilities.features`. This is
  never auto-discovered
  ([ADR-0039](https://github.com/firestoned/banlieue/blob/main/docs/adr/0039-vsphere-vtpm-support.md)) — set it by hand once
  you have confirmed swtpm works on the host.
- **Never clone a domain that has TPM state.** swtpm keys its state by domain
  UUID, so a fresh domain always gets a fresh TPM — but a *copy* carries the
  original's sealed keys, which is the shared-vTPM problem ADR-0040 exists to
  avoid. banlieue never clones a libvirt domain, and nothing in this guide
  should lead you to.

### cloud-init user-data

`spec.userData` on a `VirtualMachine` reaches a libvirt guest as a **NoCloud
seed**: a small ISO labelled `CIDATA`, attached as a second CD-ROM, holding
`meta-data` and `user-data` at its root. vSphere needs no such disk — it has
`guestinfo` — but libvirt has no hypervisor channel, so the payload has to
arrive as a filesystem.

banlieue builds that image **in-process**. There is no `genisoimage` in the
provider image and no subprocess: the image stays distroless, and a
subprocess would need the payload on disk to hand it a path, writing the
guest's bootstrap credentials into the container's filesystem on every
reconcile.

The seed carries a generated `meta-data` even when you set no user-data,
because cloud-init requires one and ignores a seed without it:

```yaml
instance-id: <the domain's UUID>
local-hostname: <the domain name>
```

`instance-id` comes from the domain's UUID deliberately. cloud-init keys
"have I already run for this instance?" on that value, so a rebuilt VM — a
replaced pool member, say — is a new instance by construction. Reusing an
id would make a fresh VM skip every per-instance module as though it had
merely rebooted.

The volume is `<domain>-cidata.iso` in the machine's pool, and it is deleted
with the domain.

For the full picture — what the seed contains, why it is Joliet, how to
verify a guest consumed it, and the two ways to get a VM that boots but
never networks — see [cloud-init on libvirt](cloud-init-on-libvirt.md).

!!! note "Why the image is Joliet"
    cloud-init looks for `user-data` and `meta-data`. ISO9660 Level 1 names
    are 8.3, uppercase, and exclude the hyphen — `meta-data` cannot be
    written in the primary tree at all. A Joliet supplementary tree carries
    the real names, which is why the documented `genisoimage` command passes
    `-joliet`. You can confirm any seed banlieue produced by mounting it:
    `hdiutil attach seed.iso` (macOS) or `mount -o loop seed.iso /mnt`.

### Deletion

Deleting the `VirtualMachine` blocks at the Kubernetes API until the domain
and its disks are really gone. The order is destroy → undefine → delete
volumes, and the undefine always passes `NVRAM | TPM | MANAGED_SAVE`.

Those flags are not optional. libvirt **fails** an undefine outright on a
UEFI domain with no `--nvram`, and without `--tpm` a deleted VM leaves its
swtpm state behind — which for a sealed sandbox means leaking the very
per-VM secret material the vTPM existed to isolate. The provider re-looks-up
the domain afterwards rather than trusting the undefine, because a teardown
that reports success while leaving a domain defined is how the next VM of the
same name quietly inherits a stale one.

The shared boot image is never deleted: it belongs to the `VMImage`, and
removing it with one machine would break every other VM using it.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `scheme "qemu+ssh" is not supported` | This provider is TLS-only (ADR-0011). Use `qemu+tls://host/system`. |
| `Ready=False`, reason `CapabilitiesIncomplete` | A declared pool or network is not on the host. The message names which. |
| `Ready=False`, reason `ConnectFailed` | TLS or reachability. Check 16514, then reproduce with `virsh -c qemu+tls://…`. |
| `Ready=False`, reason `CredentialsUnavailable` | The Secret or CA ConfigMap is missing a key. `tls.crt` / `tls.key` / `ca.crt`. |
| Zone stuck at `BuildPending` | `status.buildArtifact` is not `Ready` yet — the problem is upstream, in `banlieue-imagebuilder`. |
| Import Job `403` on `jobs` | The `ProviderClass` is missing `additionalRules`. See `examples/09-providerclass-libvirt.yaml`. |
| Import Job `Pending`, ServiceAccount not found | The Job's namespace and the provider's differ. The provider falls back to that namespace's `default` SA, which needs the grant. |
| `VirtualMachine` stuck `Scheduled=False`, reason `ImageNotReady` | The `VMImage` has no ready `libvirt` source. For a `BackingFile`, the volume must already be in the pool. |
| `LibvirtMachine` `Ready=False`: *volume … is not in pool* | The image has not finished importing, or a `BackingFile` names a volume that is not there. `virsh vol-list <pool>`. |
| `LibvirtMachine` `Ready=False`: *storage pool … does not exist* | `spec.pool` came from the Provider's `storageClasses` mapping. The mapping names a pool the host does not have. |
| `VirtualMachine` never gets a `LibvirtMachine` | The chosen Provider's class has no builder in `banlieue-controller`. The `Scheduled` condition names the class. |
| `tpmEnabled` class will not schedule | The Provider's failure domain does not advertise `vtpm` in `capabilities.features`. Never auto-discovered (ADR-0039). |
| Guest boots but ignores its user-data | Check the seed mounted: `virsh domblklist <domain>` should list a second CD-ROM. Mount the volume and confirm `user-data` is at the root. |
| cloud-init skips per-instance modules on a rebuilt VM | `instance-id` was reused. banlieue derives it from the domain UUID, so this means the domain was redefined rather than recreated. |
| Deletion hangs | Expected while the domain is still being torn down. If it persists, the message on `LibvirtMachine` says which step failed — teardown errors are never swallowed. |

## Next: keep VMs standing by

Once a single `VirtualMachine` works here, a
[`VirtualMachinePool`](virtualmachine-pools.md) keeps a set of them
already-provisioned so a consumer does not wait out provisioning. Members
are ordinary `VirtualMachine`s on this same Provider — nothing in this guide
changes.

## Full schema reference

- [`Provider` API reference](../reference/api.md#provider)
- [`VMImage` API reference](../reference/api.md#vmimage)
- [`LibvirtMachine` API reference](../reference/api.md#libvirtmachine)
- [ADR-0050 — `LibvirtMachine`: the InfraMachine contract on libvirt](https://github.com/firestoned/banlieue/blob/main/docs/adr/0050-libvirtmachine-domain-lifecycle.md)
