# Guide: VirtualMachine Pools

Keep a set of already-provisioned VMs standing by, so a consumer gets one
immediately instead of waiting out provisioning.

A `VirtualMachinePool` creates nothing but ordinary `VirtualMachine`s. The
scheduler places them, providers realise them, and no provider learns that
pools exist — which is why there is no `VSpherePool` or `LibvirtPool` and
never will be
([ADR-0046](https://github.com/firestoned/banlieue/blob/main/docs/adr/0046-virtualmachinepool.md)).

!!! note "Scope"
    A pool **maintains a warm set**. Handing a member out to a specific
    consumer — binding, expiry, and the guarantee that a member is used once
    and then destroyed — is `VirtualMachineClaim`. See the
    [VirtualMachine Claims guide](virtualmachine-claims.md).

## Why a pool exists at all

For an `installMode: Immediate` image a VM boots from an already-installed
disk, and provisioning is fast enough that a pool is a convenience.

For `installMode: Deferred` it is not a convenience. A disk sealed to a
per-VM vTPM can only be produced by letting the guest install itself on
first boot, with its own TPM attached
([ADR-0040](https://github.com/firestoned/banlieue/blob/main/docs/adr/0040-deferred-install-for-vtpm-encryption.md)),
and Instant Clone is ruled out
([ADR-0052](https://github.com/firestoned/banlieue/blob/main/docs/adr/0052-instant-clone-vmfork-not-supported.md)).
So that VM costs a full unattended install — minutes — and no provisioning
cleverness removes it, because **the slowness is the security property**.

The only way to hand out such a VM quickly is to have installed it already.

## Quick start

`examples/18-virtualmachinepool.yaml` is a complete pool. It pairs with the
Provider, `VMImage` and `VMClass` from `examples/17-virtualmachine-libvirt.yaml`.

```sh
kubectl apply -f examples/17-virtualmachine-libvirt.yaml
kubectl apply -f examples/18-virtualmachinepool.yaml
kubectl get virtualmachinepool,virtualmachine -n banlieue-system -w
```

Within a reconcile you will see members appear with generated names:

```text
NAME           WARM   AVAILABLE   PROVISIONING   CLAIMED   AGE
sandbox-pool   2      0           2              0         5s

NAME                  CLASS   IMAGE                  PROVIDER   POWER       READY
sandbox-pool-9dmtx    small   ubuntu-22.04-libvirt   kvm-host-1             
sandbox-pool-bfttl    small   ubuntu-22.04-libvirt   kvm-host-1             
```

and, once they provision, `AVAILABLE` reaches `WARM`.

## The one field that can fail silently

`spec.readiness` selects which member condition means *claimable*. It is
**required and has no default**, which is unusual in this API and
deliberate.

| Value | Means | Use when |
| --- | --- | --- |
| `InfrastructureReady` | the backend says the VM exists and is running | `installMode: Immediate` |
| `GuestReady` | the **installed guest** booted and announced itself | `installMode: Deferred`, on any backend (see below) |

For a Deferred image, `InfrastructureReady` fires when the install
**starts**, not when it finishes. A pool using it would hand out VMs that
are still installing.

Every cheap "is the guest up" signal has the same flaw, which is why
`GuestReady` is not one: a guest-agent ping, a DHCP lease and an open SSH
port are all satisfied by the installer environment while it is still
overwriting the disk. The distinguishing fact is *which disk booted*
([ADR-0043](https://github.com/firestoned/banlieue/blob/main/docs/adr/0043-guestready-installed-guest-signal.md)).

### What `GuestReady` needs, per backend

`GuestReady` is published only after the **installed** system has reported
(ADR-0043) and, for a `Deferred` machine, after the installer has been
ejected (ADR-0044). For a `tpmEnabled` class it also waits for the vTPM's
endorsement certificate (ADR-0045), so a member that cannot attest is never
claimable. How the installed system reports differs by backend:

| Backend | How the installed system reports | What the image or user-data must carry | Verified live |
| --- | --- | --- | --- |
| **libvirt** | writes `/run/banlieue/phase`; the provider reads it through `qemu-guest-agent` | the libvirt stage of `examples/16-cloud-config-guest-phase.yaml`, and `qemu-guest-agent` installed and enabled. A `tpmEnabled` class also needs `examples/20-cloud-config-guest-ek-certificate.yaml` plus `tpm2-tools` and `openssl`: swtpm keeps no host-side copy of the certificate, so the guest exports it | 2026-10-12: Deferred, `tpmEnabled`, Kairos on Debian 13. A member reached `GuestReady` **158 s** after creation, installer ejected, EK certificate published; the pool e2e (warm, claim, refill, release, pool deletion) passed |
| **Cloud Hypervisor** | `systemd-notify phase=installed` over vsock; the provider listens on the host | the Cloud Hypervisor stage of example 16. No guest agent. The EK certificate is read on the host | 2026-10-09: a 2-member pool `Warm` in **90 s**, refills in 80 s and 100 s. 2026-10-12, two hosts with the vTPM-fixed VMM: `Warm` in 115 s and 110 s, the pool e2e passed on both. See [the timings page](../reference/cloud-hypervisor-pool-timings.md) for the vTPM fault and the VMM it needs |
| **vSphere** | `vmware-rpctool` writes `guestinfo.banlieue.phase` | the vSphere stage of example 16, and open-vm-tools | 2026-09-23: a 2-member `GuestReady` pool `Warm` in **130.3 s** |

Example 16 carries all three stages; each one only runs where its
transport exists, so one layer serves every backend.

The install itself is user-data (or a `cloudConfigs` layer baked into the
installer): Kairos needs `install.auto: true` and `install.reboot: true`,
and a `tpmEnabled` class adds `encrypted_partitions: [COS_PERSISTENT]` so
the disk is sealed to that member's own vTPM.

!!! warning "libvirt: a Deferred image needs a provider newer than v0.4.1"
    Up to v0.4.1 the libvirt provider booted the installer before the disk,
    so after a completed install the guest came back up in the live
    installer, never booted the installed system, and never reported. The
    member stays `GuestNotAnnounced` and reinstalls until the pool reaps it
    at `provisioningTimeoutSeconds`. The provider now boots the disk first
    and falls back to the installer only while the disk is empty
    ([ADR-0044](https://github.com/firestoned/banlieue/blob/main/docs/adr/0044-detach-install-media-after-install.md),
    amended 2026-10-12). Images that install nothing (`installMode:
    Immediate`) were never affected.

!!! tip "Building a libvirt image that can report"
    Kairos's own Ubuntu image ships no `qemu-guest-agent`, and Hadron has no
    package manager. `scripts/build-kairos-debian-iso.sh` builds a Kairos
    installer ISO on Debian with the agent enabled; for a `tpmEnabled` pool
    add the EK tools:

    ```sh
    EXTRA_PACKAGES="tpm2-tools openssl" scripts/build-kairos-debian-iso.sh
    ```

    Put the ISO in the Provider's pool and name it from a `BackingFile`
    source with `template.installMode: deferred`.

Without `qemu-guest-agent` a libvirt provider cannot evaluate the signal at
all, so it leaves `GuestReady` **absent** rather than false, and the pool
reports:

```text
Warm=False   reason=ReadinessSignalAbsent
message: no member has published the "GuestReady" condition;
         spec.readiness selects a signal nothing is setting
```

That is the pool telling you the signal it was asked to wait for is one
nothing sends, not a slow install. Check a libvirt image before adopting it:

```sh
virsh qemu-agent-command <domain> '{"execute":"guest-ping"}'
```

An error rather than `{"return":{}}` means the agent is missing.

A `tpmEnabled` member whose certificate has not appeared yet reports
`GuestReady=False reason=TpmEndorsementPending`, which keeps it out of the
warm set.

This is also why the field has no default. `GuestReady` is the *right*
default for the use case pools were built for, and defaulting to it would
make every pool silently do nothing.

## Sizing

Two numbers, and one of them is usually got wrong:

```text
warmReplicas  >=  peak requests per minute  x  minutes to provision one member
maxReplicas   >=  warmReplicas + peak concurrent holds + maxSurge
```

A pool shallower than the first line hands out nothing during a burst — the
consumer waits exactly as long as it would have with no pool at all.

Without the `+ maxSurge` term in the second, an image rollout has to retire
warm capacity to build its replacements, and `status.available` dips while
it does.

`maxSurge` is a **host-protection** knob, not a throughput knob. Each
provisioning member is a full install's worth of disk and CPU on hosts that
are also running members already handed out. If refill is too slow, raise
`warmReplicas` or shorten the install — raising `maxSurge` mostly moves the
pain onto the hosts.

## What the pool does on its own

### Refills

Members are replaced whenever `available` drops below `warmReplicas` —
whether a member was deleted, expired, or failed. Members carry
`generateName`, never an index: they are deleted from the middle of the set
constantly, so nothing may depend on an ordinal.

### Reaps poisoned members

A member still provisioning past `provisioningTimeoutSeconds` (default
1800s) is deleted and replaced, never repaired. A pool has no way to
diagnose a half-finished unattended install, and a warm set is worthless if
it can contain one.

The default is generous on purpose — a Deferred member is not Ready until a
full install *plus* a reboot have finished. Lowering it reaps healthy
members mid-install, which then get replaced by members that are reaped the
same way.

### Follows the image

`recycleOnImageChange` defaults **on**, unlike most booleans in this API. A
warm member is an already-installed VM: a pool that ignored image rebuilds
would keep handing out the unpatched build indefinitely, and nothing would
say so.

Rollout is surge-style — a stale member is retired only once enough fresh
members are ready to keep `warmReplicas` claimable, unless `maxReplicas`
leaves no room to build the replacement first. Give it headroom and
`available` never dips.

`maxIdleSeconds` bounds how stale a member can get *between* rebuilds.

## Addressing

Omit `spec.addressing` for DHCP or class-level IPAM. Set it to stamp a
static address per member into the template's `networkOverrides`. `pool` is
a list of entries drawn in the order written, each low to high
([ADR-0056](https://github.com/firestoned/banlieue/blob/main/docs/adr/0056-vmpool-address-pool-entries.md)) —
familiar to anyone who has set up MetalLB's `IPAddressPool.spec.addresses`:

```yaml
addressing:
  interface: eth0          # matches a VMClass network interface name
  pool:
    - 192.0.2.10-192.0.2.29   # an inclusive range: 20 addresses
    - 192.0.2.40              # a single address
    - 192.0.2.50/31           # a CIDR block, network + broadcast included
  prefix: 24
  gateway: 192.0.2.1
```

**Size the pool above `maxReplicas`, with spares.** An address is held
until a deleted member's backend VM is really gone, not until its delete is
issued — so a pool churning members needs more addresses than it has
members. When every entry runs out the pool reports:

```text
Capacity=False   reason=AddressRangeExhausted
```

**Keep the range clear of standalone VMs.** The pool avoids addresses held
by its *own* members only. A member stamped with an address that a
standalone `VirtualMachine` already claims is blocked with
`Ready=False reason=DuplicateAddress`
([ADR-0083](https://github.com/firestoned/banlieue/blob/main/docs/adr/0083-block-duplicate-static-address.md)),
is reaped at `provisioningTimeoutSeconds`, and the same address can be drawn
again. Nothing collides, but the pool churns until the overlap is removed.

This inline addressing is interim. CAPI IPAM
([ADR-0033](https://github.com/firestoned/banlieue/blob/main/docs/adr/0033-capi-ipam-pool-integration.md))
is recorded but not implemented; when it lands this field gains a `poolRef`
alternative ([ADR-0053](https://github.com/firestoned/banlieue/blob/main/docs/adr/0053-ipam-claims-for-pool-members.md))
and the inline list stays as the zero-dependency option.

## Reading a pool's status

```sh
kubectl get virtualmachinepool sandbox-pool -n banlieue-system -o yaml
```

| Field | Meaning |
| --- | --- |
| `replicas` | members of any phase, excluding ones already being deleted |
| `available` | Ready and unclaimed — what could be handed out right now |
| `provisioning` | members still coming up |
| `claimed` | members held by a [claim](virtualmachine-claims.md) |
| `imageRevision` | the image build new members are being created from |

Conditions:

| Type | Reason | Meaning |
| --- | --- | --- |
| `Warm` | `Warm` | `available >= warmReplicas` — the pool is doing its job |
| `Warm` | `Filling` | not warm yet, but making progress |
| `Warm` | `ReadinessSignalAbsent` | **stuck** — nothing publishes the condition `spec.readiness` names |
| `Capacity` | `MaxReplicasReached` | wanted to create members, `maxReplicas` forbade it |
| `Capacity` | `AddressRangeExhausted` | wanted to create members, no free address |

`Filling` and `ReadinessSignalAbsent` are the pair worth telling apart:
the first means wait, the second means the pool will never warm.

## Deleting a pool

Deleting a pool deletes its members, and deleting a member deletes its
backend VM. That is the intended behaviour and also a sharp edge:

```sh
kubectl delete virtualmachinepool sandbox-pool -n banlieue-system
```

Members are owned by the pool through `ownerReferences`, so Kubernetes
garbage-collects them, and each member's own finalizer blocks until the
backend VM is really gone. The base image the members were built from is
never touched — it belongs to the `VMImage`.

A **claimed** member is re-parented to its claim at bind time, so deleting a
pool does not kill VMs that are in use — only the warm, unclaimed members go
with it. See [VirtualMachine Claims](virtualmachine-claims.md).

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `Warm=False`, reason `ReadinessSignalAbsent` | `spec.readiness` names a condition nothing publishes. Almost always `GuestReady` before ADR-0043. Switch to `InfrastructureReady` if the image is `Immediate`. |
| `available` stays 0, members exist and are Ready | The members' `Ready` is not the condition `spec.readiness` selects. Check `kubectl get virtualmachine <member> -o jsonpath='{.status.conditions}'`. |
| No members created at all | The pool's template does not schedule. Check a member's `Scheduled` condition; if there are no members, check the pool's own events and that the `VMClass` and `VMImage` it names exist. |
| `Capacity=False`, `MaxReplicasReached` | `maxReplicas` is below what `warmReplicas` plus churn needs. |
| `Capacity=False`, `AddressRangeExhausted` | The address range has no spares above `maxReplicas`. |
| Members churn: created, then deleted, repeatedly | `provisioningTimeoutSeconds` is shorter than the image takes to install. |
| libvirt, Deferred: a member stays `GuestNotAnnounced` and its guest is back in the live installer after installing | A provider at v0.4.1 or earlier boots the installer before the disk. Upgrade; see *What `GuestReady` needs, per backend*. |
| Cloud Hypervisor, `tpmEnabled`: about 1 install in 7 fails with `tpm_recv: error -5` | Stock Cloud Hypervisor's 100 ms TPM receive timeout. See [the timings page](../reference/cloud-hypervisor-pool-timings.md). |
| Pool keeps creating members up to `maxReplicas` | The readiness signal is never satisfied, so `available` never rises and the pool keeps trying. The `ReadinessSignalAbsent` condition above names the cause. |

## Full schema reference

- [`VirtualMachinePool` API reference](../reference/api.md#virtualmachinepool)
- [ADR-0046 — `VirtualMachinePool`: warm, never-reused VMs](https://github.com/firestoned/banlieue/blob/main/docs/adr/0046-virtualmachinepool.md)
- [`VirtualMachine` API reference](../reference/api.md#virtualmachine) — a member is one of these
- [VirtualMachine Claims guide](virtualmachine-claims.md) — how a member leaves the pool
