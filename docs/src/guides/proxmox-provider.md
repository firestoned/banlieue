# Guide: Proxmox VE Provider

Register a Proxmox VE node or cluster with banlieue and run VMs on it.

The provider talks to Proxmox's REST API through a **first-party client**
(`banlieue-proxmox`, [ADR-0074](https://github.com/firestoned/banlieue/blob/main/docs/adr/0074-banlieue-proxmox-rest-client.md)):
no community crate, no `pvesh` over SSH. Two consequences shape this guide:

- **API tokens are the only credential.** Password and ticket auth are not
  implemented. A token is stateless, revocable on its own, and can be
  *privilege-separated* so it holds only the permissions granted to it.
- **Every mutation is a task.** Clone, start, stop, delete and upload return
  a UPID; the provider waits for each, with a timeout, and treats a non-`OK`
  exit status as a failure.

VMs are realised as `ProxmoxMachine` objects
([ADR-0075](https://github.com/firestoned/banlieue/blob/main/docs/adr/0075-proxmoxmachine-inframachine-contract.md)):
a full clone of a template VM, configured, grown, seeded with cloud-init and
started.

## Prerequisites

- A Proxmox VE 9 node (single node or cluster) reachable from the cluster on
  port 8006.
- The core controller and the operator installed — see
  [Core Controller](core-controller.md) and
  [Provider Lifecycle & Install](provider-lifecycle.md).

## 1. Prepare the node

Run this **on the node**, as root:

```sh
./scripts/bootstrap-proxmox-host.sh all
```

It is idempotent, and does five things:

| Step | Result |
|---|---|
| `role` | The `BanlieueProvider` role with exactly the privileges below, and the `BanlieueSeed` role |
| `seed` | A `dir` storage, `banlieue-seed`, that holds nothing but NoCloud seed ISOs |
| `token` | User `banlieue@pve` and a privilege-separated token; the Secret manifest is written to `./proxmox-provider-token-secret.yaml` |
| `cert` | A `pveproxy` certificate naming every address clients connect by, signed by the node's own CA; the CA is written to `./proxmox-ca.pem` |
| `template` | A Debian cloud image imported as template **VMID 9000** (`scsi0` on virtio-scsi, guest agent on) |

A token's secret cannot be read back later, so re-issuing needs
`FORCE_TOKEN=true`.

### The privileges the provider holds

`VM.{Allocate,Audit,Clone,PowerMgmt}`,
`VM.Config.{CDROM,CPU,Cloudinit,Disk,HWType,Memory,Network,Options}`,
`VM.GuestAgent.Audit`, `Datastore.{AllocateSpace,AllocateTemplate,Audit}`,
`SDN.{Audit,Use}`, `Sys.Audit` — granted on `/vms`, the node, the SDN zone and
the image storages only.

A second role, `BanlieueSeed` — `Datastore.{Allocate,AllocateTemplate,Audit}` —
is granted **on `banlieue-seed` alone**. Deleting a seed ISO needs
`Datastore.Allocate`, and on a shared storage like `local` that privilege would
also delete your backups, templates and other ISOs, and let the token edit the
storage's definition. Confining it to a storage that holds only seeds keeps a
leaked token from touching anything else. The token holds no grant on `local`.

It never holds `VM.Console`, guest-exec (`VM.GuestAgent.{FileRead,FileWrite,Unrestricted}`),
`VM.Migrate`, `VM.Snapshot*`, `VM.Backup`, `Sys.Modify` or
`Permissions.Modify`. A token that leaks can create and destroy VMs on this
node; it cannot open a console, run commands in a guest or change permissions.

## 2. Create the Secret and the CA

```sh
kubectl -n banlieue-system apply -f proxmox-provider-token-secret.yaml
kubectl -n banlieue-system create configmap proxmox-ca --from-file=ca.crt=proxmox-ca.pem
```

The Secret carries two keys: `username` (the **full** `user@realm!tokenid`)
and `tokenValue`. The token id is validated before any request; a mis-pasted
value is reported as such rather than as a bare `401`.

TLS is verified by default. With no `caBundle`, the system roots apply.
`insecureSkipTLSVerify` exists but is gated by the admission policy in
`deploy/admission/provider-connection.yaml`.

## 3. Declare the Provider

See [`examples/22-virtualmachine-proxmox.yaml`](https://github.com/firestoned/banlieue/blob/main/examples/22-virtualmachine-proxmox.yaml),
which is a complete, working set. The parts that matter:

```yaml
capabilities:
  storageClasses:
    - name: standard
      target: { storage: local-lvm }   # must allow `images`
    - name: seed-iso                   # well-known name
      target: { storage: banlieue-seed }  # dedicated; must allow `iso`
  networkClasses:
    - name: mgmt
      target: { bridge: vmbr0 }
```

The provider checks each declaration against every online node and reports
`Ready=False` with the reason if one is wrong — a storage that exists but does
not allow the content it is used for, or a bridge that is missing. One
**failure domain per node** is published.

!!! important "`seed-iso` is required for cloud-init"
    Proxmox's API cannot upload `cicustom` snippets, so `userData` reaches the
    guest as a NoCloud ISO uploaded to the storage named by the `seed-iso`
    class. Any VM with `userData` — or a static address — needs it. The seed is
    deleted with the VM.

## 4. Images are template VMIDs

A `VMImage` source of `kind: Template` names a **template VMID**:

```yaml
sources:
  - providerClass: proxmox
    kind: Template
    ref: "9000"
```

The provider verifies the VMID is a template and publishes it as the image's
`resolvedRef`. `Url` and `BackingFile` sources are reported as unsupported.
Machines are always **full clones**, so a guest's disk never depends on its
template's lifetime.

The template's boot disk must be `scsi0` (what the bootstrap script builds).
A template booting from another key is refused when the OS disk is resized,
rather than growing the wrong disk.

## 5. Run a VM

```sh
kubectl apply -f examples/22-virtualmachine-proxmox.yaml
kubectl -n banlieue-system get virtualmachine,proxmoxmachine
```

`kubectl get proxmoxmachine` shows the node and VMID. The VMID is recorded in
`status.vmid` **before** the clone. The Proxmox VM is named after the machine,
and its **description carries a marker line** `banlieue-machine-uid=<uid>`:
that marker, not the name, is how the provider recognises a VM as its own
after a crash, and how it refuses to adopt, reconfigure or delete a VM it did
not create.

!!! warning "Leave the marker line in the VM description"
    Renaming a banlieue VM in the Proxmox UI is harmless, and so is editing
    the rest of the description. **Deleting the `banlieue-machine-uid=` line
    orphans the VM**: the machine will clone a new one. A VM that merely has
    the same name as a machine is never touched.

Addresses come from the QEMU guest agent (`agent=1` is set on every VM), or
from IPAM when the address is static. A guest without the agent is
provisioned, but reports no dynamic addresses.

## Deleting

Deleting the `VirtualMachine` stops the VM if it is running, destroys it with
its unreferenced disks, deletes the seed ISO, and only then releases the
finalizer. "Already gone" counts as success at every step.

## Troubleshooting

| Symptom | Cause |
|---|---|
| Provider `Ready=False`, message names a storage | The storage does not allow `images` (or `iso` for `seed-iso`), is disabled, or is inactive on a node |
| Provider `Ready=False`, message names a bridge | The bridge is absent on a node |
| `401 Authentication failed!` in the log | Wrong token id or secret, or the token expired |
| `403 Permission check failed` | The token lacks an ACL on that path; re-run the bootstrap `token` step |
| `Permission check failed (/storage/…, Datastore.Allocate)` when a VM is deleted | The `seed-iso` class points at a storage other than `banlieue-seed`, where the token cannot delete; point it at the seed storage |
| TLS error naming a host | The certificate does not list the name banlieue connects by; re-run `cert` with `EXTRA_SAN_DNS` |
| VMImage not ready, "not a template" | The VMID exists but is not a template (`qm template <vmid>`) |
| `UnresolvedClass("seed-iso")` on the VirtualMachine | The Provider does not declare the `seed-iso` storage class |
| A VM with the machine's name exists but is not used | It has no `banlieue-machine-uid=<uid>` line naming this machine, so it is not ours; banlieue cloned a fresh VM beside it |
| Machine stuck cloning | Check the task in the Proxmox UI; the provider gives up on a task that outlasts its timeout and retries |

## Verifying the client against your node

Two tiers run against a real node. `make proxmox-live-test` is read-only.
`make proxmox-lifecycle-test` **creates and destroys VMs**: it clones the
template, seeds and starts it, tears it down, and checks that a foreign VM
sharing the machine's name survives. It needs `PROXMOX_NODE` and
`PROXMOX_TEMPLATE_VMID` as well:

```sh
PROXMOX_ENDPOINT=https://bar.foo.io:8006 \
PROXMOX_TOKEN_ID='banlieue@pve!provider' \
PROXMOX_TOKEN_SECRET=<uuid> \
PROXMOX_CA_FILE=./proxmox-ca.pem \
  make proxmox-live-test
```
