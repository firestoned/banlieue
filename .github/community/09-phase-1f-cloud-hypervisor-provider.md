# 09: Phase 1F, Cloud Hypervisor provider

> **Goal.** A `VirtualMachine` can land on a bare-metal KVM host running
> [Cloud Hypervisor](https://www.cloudhypervisor.org/) guests, through a new
> `CloudHypervisorMachine` InfraMachine CRD and `Provider`s of class
> `cloud-hypervisor`. The user's manifest is the one they already use for
> vSphere and libvirt.
>
> **Stop condition.** A `VirtualMachine` with `spec.userData`, scheduled onto
> a `cloud-hypervisor` `Provider`, becomes a running guest that consumed its
> user-data from a NoCloud seed and reports its addresses. Deleting it removes
> the VMM process, the disks, the seed, the tap device and any swtpm state.
> Restarting or upgrading the provider does not disturb a running guest.
> `Provider.status` reports the host's real CPU, memory, storage targets and
> bridges.
>
> **Status: gate decided, native spike nearly done** (2026-09-25; only the
> QEMU timing comparison is open). Cloud
> Hypervisor is a **first-class, host-resident provider** (shape A below),
> not libvirt's `ch` driver: [ADR-0060](../../docs/adr/0060-cloud-hypervisor-first-class-provider-topology.md).
> Read [The defining problem](#the-defining-problem-there-is-no-daemon)
> before anything else.

## Why a third KVM path when libvirt already works

The libvirt provider (roadmap 07, ADR-0050, ADR-0054) drives QEMU. Cloud
Hypervisor is a different VMM on the same KVM, and it earns a provider for
reasons that are specific to roadmap
[17](17-ephemeral-vm-pools.md):

1. **Attack surface.** A sandbox exists because the workload inside it is not
   trusted. Cloud Hypervisor is a Rust VMM with virtio devices only, no legacy
   emulation (no IDE, no e1000, no BIOS), and seccomp plus Landlock on by
   default. That is a materially smaller thing to defend than QEMU for a guest
   whose threat model starts with "prompt-injected agent".
2. **Density and boot time.** Lower per-guest overhead and a shorter path to
   the kernel change how large `warmReplicas` can be on a given host.
3. **Snapshot and restore** as a way to park an installed warm member on disk
   instead of in RAM. Narrow, but real. See [phase 7](#7-pools-roadmap-70).
4. **It passes the licence screen.** Apache-2.0 / BSD-3-Clause, Linux
   Foundation governed. swtpm is BSD-3-Clause, edk2 is BSD-2-Clause-Patent.

What it is **not**: a replacement for the libvirt provider. No non-virtio
guests, no BIOS boot, no broad OS matrix. General-purpose VMs on KVM stay on
libvirt.

## The defining problem: there is no daemon

Every existing provider is a Deployment in the management cluster (ADR-0003,
per instance) that reaches its backend over the network: vCenter over HTTPS,
libvirtd over mutual TLS (ADR-0011). Cloud Hypervisor has neither. It is one
process per guest, serving a REST API on a **local Unix socket**, and that API
cannot start the process that serves it. Something on the host has to spawn
and supervise `cloud-hypervisor` and `swtpm`, create tap devices, and place
disk files.

| | Shape | For | Against |
|---|---|---|---|
| **A** | **Host-resident provider.** `banlieue provider cloud-hypervisor` runs on the KVM host as a systemd service with a scoped kubeconfig. One `Provider` is one host. | No new wire protocol and no listener on the hypervisor. The host pulls, nothing pushes. Non-negotiable 1 holds literally. Per-instance (ADR-0003) by construction. | Amends ADR-0003 and ADR-0012: the operator must support a `ProviderClass` it does not deploy. A cluster credential lives on a hypervisor. Upgrades become host package management. Artifacts must leave the cluster (phase 4). |
| ~~B~~ | **libvirt's `ch` driver** (rejected, ADR-0060), reached with the `banlieue-libvirt` client that already exists (`ch+tls://host/system`). | Days, not weeks. `LibvirtMachine`, the XML builder, the import Job and the NoCloud seed are all reused. No topology change at all. | The driver exposes a subset of Cloud Hypervisor. vTPM, hotplug and snapshot coverage are unverified, as is packaging on RHEL-family hosts. It puts libvirtd back in the trusted computing base, which is half of what reason 1 above was for. |
| C | A first-party host daemon with an mTLS API. | Leaves ADR-0003 alone. | Invents a wire protocol and a privileged network listener. This is writing libvirtd again. Rejected. |
| D | SSH exec from an in-cluster provider. | None worth the cost. | ADR-0011 already rejected this shape: a CLI's stdout becomes a wire format. Rejected. |

**Decision (2026-09-25): A.** Cloud Hypervisor gets first-class support as
its own provider. B is rejected: it is not packaged on Debian 13, it keeps
libvirtd in the sandbox TCB that reason 1 exists to shrink, and it exposes
only a subset of the VMM. Recorded in
[ADR-0060](../../docs/adr/0060-cloud-hypervisor-first-class-provider-topology.md).
The phases below are written for A.

## Fixed constraints (not up for re-litigation here)

| Constraint | Source |
|---|---|
| No RPC between controller and providers. CRDs and the Kubernetes API only. | non-negotiable 1 |
| Infra CRDs satisfy the CAPI v1beta2 InfraMachine contract, with a `*Template` kind alongside. | non-negotiable 2, ADR-0050 Decision 2 |
| First-party, pure-Rust clients. No FFI, no subprocess in a reconcile path, no new system packages in the image. | ADR-0011, ADR-0050, ADR-0054 |
| Bulk bytes never flow through a reconcile loop. | ADR-0011 |
| TPM-sealed guests require `installMode: Deferred`. Never clone a disk that has been installed with a TPM. | ADR-0040 |
| No fork-from-golden. A restored or forked guest does not re-read its identity. | ADR-0052 |
| No nested virtualization. This provider targets bare-metal KVM hosts only. | environment |
| No real hostnames or addresses in tracked files. | `rules/no-real-infrastructure.md` |

---

## 0. Spike and decision gate

One bare-metal KVM host, a shell, no code in the tree.

**Native checks (inform A):**

- [x] Boot a Kairos `cloudImage` raw disk under `CLOUDHV.fd` (edk2) with a tap
      on a bridge and a serial console to a file. *2026-09-25, Cloud
      Hypervisor v53.0, edk2 `ch-97eeb7b09`, Debian 13 host.* The firmware runs
      the image's own GRUB; DHCP on the bridge; the host neighbour table maps
      the guest MAC to its address, so phase 5's netlink approach holds. Needs
      the disk grown first, see Gotchas.
- [x] Deliver user-data with a seed ISO built by the existing
      `banlieue-provider-libvirt::cloudinit` module, attached as a read-only
      virtio-blk device. Confirm the guest mounts `CIDATA`. *2026-09-25: works
      after a writer fix.* The seed's all-'0' volume dates are legal
      ECMA-119, but kairos-agent v2.26.0 finds a `CIDATA` disk through
      go-diskfs v1.7.0, which rejects them; on libvirt the seed is a CD-ROM
      and never took that path. With real dates the guest applies hostname,
      SSH key and `boot` stages. See the 2026-09-25 CHANGELOG entry.
- [x] Attach swtpm through `--tpm socket=...`. Confirm `/dev/tpmrm0` in the
      guest. *2026-09-25, swtpm 0.7.1:* `/dev/tpm0` and `/dev/tpmrm0` present;
      EK certificates `CN=<name>:<uid>` (RSA-2048 and ECC P-384) from
      `swtpm_localca`. swtpm paths must be absolute (see Gotchas).
- [x] **Deferred install.** Empty disk first, Kairos install ISO second, both
      virtio-blk. Confirm the firmware falls through the unbootable empty disk
      to the ISO, the install seals to the TPM, and the reboot lands on the
      installed disk and not back in the installer. *2026-09-25, Kairos Hadron
      v0.4.0 core ISO:* all three hold. `COS_PERSISTENT` is LUKS2 with its
      passphrase in the vTPM's NV storage; `vm.remove-device` drops the
      installer from a running guest and it stays gone across a guest reboot;
      a full VMM and swtpm restart unlocks again. Details in ADR-0065.
- [x] Confirm what the firmware does about UEFI variables. If, as expected,
      there is no persistent variable store, Secure Boot key enrolment is not
      possible and Trusted Boot/UKI images are out of scope on this class.
      That matches where ADR-0051 already left sandboxes (classic GRUB).
      *2026-09-25: as expected.* A new NV variable is accepted at runtime and
      gone after a guest reset; edk2's `NvVars` fallback does not keep it.
- [x] Record boot-to-login time and resident memory next to the same image
      under libvirt/QEMU on the same host. *QEMU half done 2026-09-26:* same
      installed Kairos disk, 2 vCPU / 4 GiB, virtio-blk raw on the host page
      cache, DHCP on a private bridge, QEMU 10.0 `q35` + OVMF vs Cloud
      Hypervisor v53 + `CLOUDHV.fd`, 5 alternating runs each, medians.
      Boot to login 40.1 s vs 41.4 s (both include the 10 s GRUB countdown).
      VMM RSS at login **1395 vs 1855 MiB** (-25%). CPU and memory
      bandwidth within noise; sequential direct I/O equal; 4 KiB direct
      I/O **55 vs 38 MB/s write, 91 vs 52 MB/s read**. Not measured:
      network throughput, parallel-start density, direct kernel boot, a
      tuned QEMU (`io_uring`, iothreads, `microvm`). *Cloud Hypervisor half done
      (2026-09-25, 2 vCPU / 4 GiB):* first boot of a fresh `cloudImage`
      including layout expand, auto-reset install and in-place reboot, 146 s
      to login; recovery-only boot, about 30 s (10 s is the GRUB countdown).
      VMM RSS 1.33 GiB after boot, high-water 4.0 GiB. QEMU half open.

**`ch` driver checks (inform B):** *superseded 2026-09-25, B rejected in
ADR-0060. Kept for the record.*

- [x] Is the driver packaged for the target host OS at all? **No on Debian
      13** (2026-09-25): libvirt 11.3.0 there ships no `ch` connection driver
      package, so B means building libvirt from source. RHEL-family packaging
      not yet checked.
- ~~Does `virtchd` accept the remote TLS transport the existing client uses?~~
  Superseded.
- ~~Which of these does it support today: `<tpm>` with an emulator backend,
  a cdrom-less ISO disk, CPU and memory hotplug, managed save?~~ Superseded.

**Gate.** Decided: A, in ADR-0060. The native checks above still run, since
they inform ADR-0063 (supervision) and ADR-0065 (vTPM and `Deferred`).

## 1. ADRs, then CALM

Numbers are next-free as of 2026-09-20, after the three roadmap
[15](15-vsphere-disk-image-import.md) reserves (`0057` to `0059`). `0043` to
`0049` stay with roadmap 17, which also holds `0055`.

| ADR | Decides |
|---|---|
| 0060 | **Provider topology for daemonless backends.** *Drafted as [Proposed](../../docs/adr/0060-cloud-hypervisor-first-class-provider-topology.md), 2026-09-25.* Outcome of the gate. For A: `ProviderClass.spec.deployment: Managed \| External`, where `External` makes `banlieue-operator` create the ServiceAccount, Role, RoleBinding and credential but no Deployment. Amends ADR-0003 and ADR-0012. Also: how an out-of-cluster process keeps its credential fresh. |
| 0061 | *[Proposed](../../docs/adr/0061-banlieue-cloud-hypervisor-vmm-client.md), 2026-09-25.* **`banlieue-cloud-hypervisor`**, a first-party client for the VMM's REST API over a Unix socket. Hand-written types for the endpoints actually used, pinned to a named upstream release of `cloud-hypervisor.yaml`. |
| 0062 | *[Proposed](../../docs/adr/0062-cloudhypervisormachine-inframachine-contract.md), 2026-09-25.* **`CloudHypervisorMachine`** and its `Template`: the InfraMachine contract on this backend. |
| 0063 | *[Proposed](../../docs/adr/0063-cloud-hypervisor-host-supervision.md), 2026-09-25.* **Host process supervision.** Transient systemd units created over D-Bus, not child processes and not `systemd-run`. |
| 0064 | *[Proposed](../../docs/adr/0064-artifact-delivery-to-host-resident-providers.md), 2026-09-25.* **Artifact delivery to a provider outside the cluster.** Revisits the alternative ADR-0010 deferred ("revisit if a future provider needs to consume the artifact from outside the build namespace/cluster"). This is that provider. |
| 0065 | *[Proposed](../../docs/adr/0065-cloud-hypervisor-vtpm-and-deferred-install.md), 2026-09-25.* **vTPM through swtpm, and `Deferred` install on Cloud Hypervisor.** |
| 0066 | **Snapshot-to-disk for warm pool members.** Optional. Only if phase 7 goes ahead. |
| 0067 | **`banlieue host`: host install as a subcommand.** Phase 10. Extends ADR-0004's dispatch with a verb you run as root on a hypervisor, and states what stays in shell (the bridge, `--remote`). Next free number: `0066` is reserved above, so this takes `0067`. |

CALM: a new node class (host-resident provider), its relationship to the API
server (outbound only), to the registry or artifact endpoint, and to the local
VMM sockets. The trust boundary around the hypervisor host is new and must be
drawn.

## 2. API: `CloudHypervisorMachine`

`crates/banlieue-api/src/infrastructure/cloud_hypervisor_machine.rs`, shaped
after `libvirt_machine.rs`.

```rust
pub struct CloudHypervisorMachineSpec {
    pub provider_id: Option<String>,   // cloudhypervisor://<provider-name>/<machine-uuid>
    pub failure_domain: Option<String>,
    pub provider_ref: LocalObjectReference,

    pub cpus: ChCpuSpec,               // boot, max (hotplug headroom)
    pub memory: ChMemorySpec,          // size_mi_b, hotplug_size_mi_b, hugepages
    pub disks: Vec<ChDiskSpec>,        // ORDER IS BOOT ORDER, see Gotchas
    pub nics: Vec<ChNicSpec>,          // bridge, mac, ipam
    pub boot_source: ChBootSource,     // Image (clone) | InstallMedia (Deferred)
    pub tpm_enabled: bool,
    pub user_data: Option<String>,     // already resolved (ADR-0025, ADR-0038)
    pub desired_power_state: PowerState,
}
```

- `providerID` uses the provider name, not a hostname, for the reasons
  ADR-0050 Decision 1 gives.
- Firmware path, state directory and the swtpm binary are **host** facts. They
  belong on the `Provider`, not on every machine.
- `banlieue-controller/src/reconciler/infra.rs` gains a third arm next to
  `build_vsphere_machine` and `build_libvirt_machine`, plus
  `PROVIDER_CLASS_CLOUD_HYPERVISOR`. `make crds`.

## 3. Client and host runtime

### `crates/banlieue-cloud-hypervisor`

`hyper` over `tokio::net::UnixStream`. Endpoints for the stop condition:
`vmm.ping`, `vm.create`, `vm.boot`, `vm.info`, `vm.power-button`,
`vm.shutdown`, `vm.delete`, `vmm.shutdown`. Later phases add `vm.resize`,
`vm.add-disk`, `vm.remove-device`, `vm.snapshot`, `vm.restore`.

- Pure `encode`/`decode` halves tested against JSON captured from a real VMM,
  no socket needed. Same convention as `banlieue-libvirt/procs.rs`.
- The pinned upstream spec is vendored with its release tag. A CI check fails
  when the pin and the vendored file disagree, so an upgrade is a deliberate
  diff and not a surprise.
- `vmm.ping` returns the VMM version. Refuse to manage a VMM older than the
  pin, with a clear condition on the `Provider`.

### Supervision (ADR-0063)

- One transient unit per guest, `banlieue-ch-<machine-uid>.service`, created
  with `StartTransientUnit` over D-Bus (`zbus`, pure Rust). A sibling
  `banlieue-swtpm-<machine-uid>.service` when `tpm_enabled`, ordered before it.
- Guests are children of systemd, **not** of the provider. A provider restart,
  crash or upgrade leaves them running. On start the provider lists units by
  prefix and re-adopts them. It never trusts its own memory of what is
  running.
- Each unit runs as an unprivileged per-guest user with `kvm` as a
  supplementary group, a private state directory, and cgroup limits taken from
  the machine spec. Leave the VMM's seccomp and Landlock on.
- Tap devices are created by the provider with the tun and bridge ioctls
  (ADR-0063 Decision 4, amended 2026-09-26: netlink cannot set a tap's owner
  or persistence), owned by the guest's uid, enslaved to the bridge, and
  passed to the VMM by name. The VMM itself never holds `CAP_NET_ADMIN`.

### Provider reconciler: capability introspection

Failure domain is the host, as for libvirt. Report CPU count and model,
memory and hugepages, each declared storage target (a directory: exists,
writable, free space, reflink-capable or not), each declared bridge (exists,
up), `/dev/kvm` access, VMM and firmware versions, swtpm present. Advertise
`FEATURE_VTPM` only when swtpm is. `nestedVirtualization` is reported false
regardless of what the CPU can do, per the environment constraint.

**Exit:** a hand-written `CloudHypervisorMachine` boots a guest from a disk
already on the host, and `kubectl delete` removes unit, tap and state.
*Met 2026-09-26, and more:* a `VirtualMachine` (not a hand-written machine)
on a k0s cluster was scheduled onto a bare-metal host running the provider
under systemd; the Kairos guest booted with its seed applied (hostname, SSH
key, a boot-stage marker), got a DHCP address in 69 s, went through
Kairos's install-and-reboot into `active_boot`, and reported `Ready=True`.
Deleting it removed unit, tap, disk, seed and both directories in 6 s. The
run found five bugs no offline test could: unregistered guest uids
(217/USER), a group shared by all guests, the VMM's forced `0077` umask,
Landlock blocking the tap's sysfs read, and a per-pass tap re-attach —
all fixed, each with a test, and recorded in ADR-0063.

## 4. Images and disks

**Getting the artifact to the host (ADR-0064).** The `cloudImage` raw file
sits in a PVC the host cannot mount. Two candidates:

1. **OCI registry.** A post-build step (kairos-operator's `spec.exporters`
   mounts the artifacts PVC at `/artifacts`) pushes the raw disk as an OCI
   artifact. The host pulls the blob by digest. Authentication, content
   addressing and digest verification come with the protocol, and hosts can
   already reach a registry.
2. **Short-lived artifact server** in the imagebuild namespace. ADR-0010
   weighed this and deferred it as an extra service to run and secure.

Lean to 1. Whichever wins, the fetch runs as its **own transient unit**
(`banlieue imageImport --method ch-host`), never inside the reconcile loop,
and reports through the provider's own `VMImage.status.perProvider[]` row
(ADR-0015). Checksum mismatch fails closed (SEC-004).

**On the host.**

- Image cache per storage target, named by digest. Eviction respects a keep
  count and never removes an image a machine was cloned from while reflinks to
  it exist.
- Per-machine disk: reflink copy (`FICLONE`) where the filesystem supports it,
  sparse full copy where it does not. Grow with `ftruncate` before first boot.
- Raw only. No qcow2.
- `VMImage` deletion: finalizer removes the cached image from every host that
  reported it, mirroring ADR-0028.

## 5. User-data and addressing

- Move `banlieue-provider-libvirt::cloudinit` into a shared crate. ADR-0054
  anticipated exactly this ("moving it to a shared crate is a mechanical change
  and should be done then"). `instance-id` derives from the machine UID.
- The seed is a file next to the machine's disk, attached read-only, owned by
  the machine and removed by its finalizer.
- Addresses: a static address from IPAM is known before boot (ADR-0024,
  ADR-0033). For DHCP, read the host's neighbour table for the guest's MAC over
  netlink, the equivalent of libvirt's ARP source.

**Exit:** the roadmap's stop condition.

## 6. vTPM and `Deferred` install

- swtpm state lives in the machine's state directory, keyed by machine UID,
  and is deleted by the finalizer. ADR-0050 learned this the hard way with
  `VIR_DOMAIN_UNDEFINE_TPM`. Here it is a directory the provider owns, so make
  the finalizer test assert it is gone.
- `Deferred`: empty disk first, install ISO second. After the install
  completes the ISO is dropped from the next boot's configuration. That is the
  ADR-0044 behaviour, and on this backend it is nearly free because the
  provider rebuilds the VMM configuration on every start.
- `GuestReady` (ADR-0043) needs a guest channel. There is no guest agent
  socket of the QEMU kind. Use **vsock** and align with roadmap 17 phase C's
  in-guest agent rather than inventing a second mechanism.
- EK certificates come from the host's `swtpm_localca`. The trust anchor is
  per host, which is what roadmap 17 phase F's
  `Provider.spec.attestation.ekTrustBundle` is for.

## 7. Pools (roadmap 17)

Add a phase G to roadmap 17 for this backend. `VirtualMachinePool` and
`VirtualMachineClaim` are backend-neutral and need no change.

**Snapshot and restore, precisely.** Forking many guests from one golden
snapshot is **rejected**, for ADR-0052's reasons almost word for word: the
restored guest resumes mid-runtime with the parent's machine-id, entropy pool
and network state, does not re-read its seed, and would share a vTPM.

The defensible use is narrower: a member that installed **itself**, with its
**own** vTPM, snapshots **itself** to disk once `GuestReady`, and is restored
on claim. That converts a warm member's RAM cost into disk. It needs VMM
state and swtpm state captured as one consistent pair. It is an optimisation
with a real consistency hazard, so it is ADR-0066 and optional.

## 8. Day 2

- CPU and memory hotplug through `vm.resize`, bounded by the `max` values
  fixed at create.
- Host drain: cordon the `Provider`, let `migrationPolicy` decide. Until
  roadmap [14](14-live-migration.md) phase A covers this class, that means
  `Recreate`, the same position libvirt is in today.
- Live migration between hosts exists upstream but needs the disk reachable on
  both sides. Out of scope here. Record it in roadmap 14's per-class table.

## 9. Docs and threat model

- [ ] `guides/cloud-hypervisor-provider.md`: host preparation (KVM, bridge,
      firmware, swtpm, the systemd unit for the provider, the credential).
      *Host half done 2026-09-26:* `guides/cloud-hypervisor-host.md` covers
      KVM, a remote-safe bridge on Debian, the pinned VMM and firmware, swtpm
      and the EK CA, the provider unit, and a hand smoke-boot. The credential
      section waits for `banlieue bootstrap` (ADR-0060 Decision 5).
- [x] Extend `scripts/` with a host bootstrap, in the spirit of
      `bootstrap-libvirt-tls.sh`. *2026-09-25:*
      `scripts/bootstrap-cloud-hypervisor-host.sh`, local or `--remote
      user@host`. It installs the pinned VMM and firmware (sha256, fail
      closed), the `banlieue` user, the storage and run layout, the host
      config file (ADR-0062 D4), a per-host EK CA readable by `banlieue`
      only, the polkit rule (ADR-0063 D6) and the provider unit (installed,
      enabled only once the binary and kubeconfig exist). It never touches
      a bridge. Tested as root in a Debian 13 container; `preflight` on a
      bare-metal host.
- [ ] Threat model pass. New: a cluster credential on a hypervisor. Bound it:
      server-side filtered watch on its own `Provider` and machines, status
      patch on those, its own `VMImage` row, its Lease, events, and **no
      Secret reads at all**, since user-data arrives already resolved in the
      machine spec. Also new: a guest escape now lands next to that
      credential, which is the strongest argument for keeping the RBAC that
      small.
- [ ] Update [`ROADMAPS.md`](../../ROADMAPS.md) in the same commit as each
      state change.

## 10. `banlieue host`: the install as a subcommand

> **Goal.** The init story for a hypervisor host is the one `k0s install` has:
> fetch one verified binary, run one subcommand, and the host is ready. Today
> it is `scripts/bootstrap-cloud-hypervisor-host.sh` plus an env file plus the
> `banlieue` binary — three artifacts to deliver and three ways to get it wrong.
>
> **Status: planned, not started.** Needs ADR-0067 first: this is a CLI contract
> and it makes the same binary something you run as root on a hypervisor.

### Why it should not stay in shell

1. **The init path gets one artifact instead of three.** A cloud-init `runcmd`
   or a CAPI bootstrap payload can `curl` the binary it already needs, check one
   sha256, and run `banlieue host install`. No script to fetch, no env file to
   template, no shell to quote.
2. **The script and the provider already have to agree, by hand, in two
   languages.** Every row below is a constant the installer writes and the
   provider later reads or derives. Drift here is silent until a guest fails to
   start — and this repository has already been bitten once by prose that
   disagreed with the YAML beside it (ADR-0041).

   | Agreement | Written by | Read by |
   | --- | --- | --- |
   | `/etc/banlieue/cloud-hypervisor.toml` schema | `host` stage | provider, ADR-0062 D4 |
   | Guest uid range | `host` stage | `status.hostUid`, ADR-0062 D4 |
   | Storage and run directory layout | `host` stage | `ReadWritePaths=`, ADR-0063 D3 |
   | Unit naming `banlieue-ch-*` / `banlieue-swtpm-*` | polkit rule | `ListUnitsByPatterns` re-adoption, ADR-0063 D2 |
   | EK CA path, `banlieue`-only | `tpm` stage | swtpm unit, ADR-0065 |
   | Pinned VMM version | `vmm` stage | vendored REST spec `PIN`, ADR-0061 |

3. **`selftest` can use the real code.** In Rust it opens the actual VMM client
   (ADR-0061) and the actual `zbus` supervisor (ADR-0063) instead of
   re-implementing their checks in shell, so a passing selftest means the
   provider's own code paths work on this host.
4. **The strongest reason: one version constant.** The `vmm` stage pins
   `cloud-hypervisor` by sha256; ADR-0061 pins the vendored `cloud-hypervisor.yaml`
   to a named release. Those are the same upstream release and today nothing
   enforces it. In one binary it is one constant and a unit test.

### What deliberately does not move

| Stays | Why |
| --- | --- |
| **The bridge.** The script never touches one, and neither should the binary. | A bridge mistake over SSH locks you out of the host. The guide's `systemd-run --on-active=5min` rollback is the right shape and it belongs in a human's hands, not in an unattended installer. |
| **`--remote user@host`.** | SSH as a one-shot install convenience is fine; SSH as a *control path* is shape D, rejected by ADR-0011. The binary gets no SSH client. The script survives as a thin wrapper that copies the binary and runs it. |
| **Installing packages by default.** | `apt-get` is Debian-family only. `packages` **verifies** by default and prints what is missing; `--install-packages` opts into installing. That also makes the binary safe to run on a host you do not own. |

### Surface

Read-only verbs and the mutating verb are separate words, so "look at this host"
can never change it:

```sh
banlieue host preflight            # changes nothing
banlieue host status               # changes nothing
banlieue host selftest            # changes nothing, boots nothing
banlieue host install [all]        # the only mutating verb
banlieue host install --only vmm --install-packages
banlieue host install --dry-run    # prints the diff, touches nothing
```

`--dry-run` mirrors `banlieue bootstrap --dry-run` (ADR-0013), which is already
the project's answer to "show me what you would apply".

### Stages, with their prerequisites

`--only <stage>` fails fast when a prerequisite is missing rather than half
applying. The order is the DAG, not a preference.

| Stage | Needs | Mutates |
| --- | --- | --- |
| `preflight` | — | nothing |
| `packages` | — | dpkg state (only with `--install-packages`) |
| `vmm` | — | `/opt/banlieue/cloud-hypervisor/<version>/`, `/opt/banlieue/firmware/<tag>/`, symlinks |
| `host` | — | `banlieue` user, storage and run dirs, `/etc/banlieue/cloud-hypervisor.toml` |
| `tpm` | `packages`, `host` | per-host EK CA, `banlieue`-only |
| `polkit` | `packages`, `host` | one polkit rule (ADR-0063 D6) |
| `provider` | `host` | the provider unit, installed and left disabled |
| `selftest` | `vmm`, `host`, `tpm`, `polkit` | nothing |

### Invariants, each one a test

1. **Fail closed on a pin mismatch.** Download to a temp path, verify, and on
   mismatch install nothing, leave the previous symlink intact, exit non-zero.
   *Test: serve a corrupt artifact, assert the symlink and `/opt` tree are
   byte-identical afterwards.*
2. **Idempotence is equality, not absence of error.** *Test: run, snapshot the
   filesystem and the unit list, run again, assert the snapshots are equal.*
3. **No partial stage.** Every file lands by write-to-temp then `rename`, so a
   crash leaves either the old state or the new one.
4. **Read-only verbs write nothing.** *Test: snapshot, run `preflight`,
   `status`, `selftest`, assert equality.*
5. **`provider` never enables the unit** unless both the binary and a kubeconfig
   exist. Already the script's behaviour; encode it.
6. **One pinned release.** *Test: the VMM download constant equals the release
   the vendored REST spec is pinned to (ADR-0061).*
7. **A reconciler cannot reach the installer.** Structural, not documentary: the
   `host` module lives behind a Cargo feature in the `banlieue` binary crate
   only, so `banlieue-provider-cloud-hypervisor` cannot call it even by mistake.

### On subprocesses, before a reviewer flags it

ADR-0011 forbids "no subprocess **in a reconcile path**". `apt-get`,
`swtpm_localca` and `openssl` in a one-shot root install are not a reconcile
path, and invariant 7 is what keeps that true structurally rather than by
convention. Nothing here weakens the rule that the *provider* speaks only
first-party Rust over documented APIs.

### Config

One file, two consumers: `banlieue host install` writes and validates
`/etc/banlieue/cloud-hypervisor.toml`, the provider reads it (ADR-0062 D4).
Values come from flags, `--set key=value`, or `BANLIEUE_HOST_*` environment
variables so a cloud-init payload needs no file of its own. The env-file the
script uses today becomes one of several inputs, not the interface.

### Tests

- **Unit:** every stage behind `HostFs`, `PackageManager`, `Systemd` and
  `Downloader` traits, faked — the same shape the client, supervisor and
  netlink already use.
- **Live protocol, new counterparty:** the tier table's second row is about a
  real daemon accepting what we produce. Here the daemons are systemd, polkit
  and dpkg. `make ch-host-install-test` runs the stages as root in a Debian 13
  container, mirroring `make ch-live-test`; that is how the shell version was
  validated and it stays valid.
- No cluster and no libvirt needed for either.

### Open questions for ADR-0067

- **`uninstall` and `drain`.** A harvested host that goes back to an incumbent
  workload needs both, and the day-2 list already has host drain against
  `migrationPolicy`. Is teardown part of this subcommand or of the provider?
- **Distro scope.** Verify-only on non-Debian, or a second package backend?
- **Naming.** `banlieue bootstrap` installs into a *cluster* (ADR-0013);
  `banlieue host install` installs onto a *host*. Two words for one idea is a
  papercut, but renaming a shipped verb is worse. Decide and write it down.

## If the gate picks B

*Superseded 2026-09-25: the gate picked A (ADR-0060). Kept so the rejected
path's cost stays visible.*

Phases 3, 4 and most of 5 collapse into the libvirt provider: accept the
`ch+tls` scheme in `banlieue-libvirt`, add a domain XML variant, add a
capability probe for the driver's feature subset, and teach the scheduler
that a libvirt `Provider` may be of VMM kind `cloud-hypervisor`. No new CRD,
no ADR-0060, no ADR-0064. Phase 7's snapshot idea is off the table unless the
driver exposes it. Keep this roadmap, mark phases 3 to 5 superseded, and
revisit A when a consumer needs what the driver cannot do.

## Tasks

- [ ] Spike native checks (gate decision written down: ADR-0060, A).
- [ ] ADR-0060 to ADR-0065 accepted. CALM updated. *All six drafted as
      Proposed 2026-09-25; ADR-0065 has three spike-gated decisions.*
      *2026-09-26: CALM updated* (provider, per-guest VMM and KVM host
      nodes; API, VMM and deployed-in relationships with controls; a
      create flow), `make calm-validate` clean. Acceptance is the
      maintainer's call.
- [x] `ProviderClass.spec.deployment` and the operator's `External` path.
      *2026-09-26:* ADR-0060 Decisions 3–5 — External class (identity and
      RBAC, no Deployment; Role scoped to its own Provider by name),
      optional `credentialsRef` (refused for cloud-hypervisor), token
      self-renewal via a token file, `banlieue bootstrap
      cloud-hypervisor-host` for the first credential. See ADR-0060
      "Implementation notes".
- [x] `CloudHypervisorMachine`, `CloudHypervisorMachineTemplate`, controller
      `infra.rs` arm. `make crds`. *2026-09-26:* API per ADR-0062 (no host
      paths, no disk list, required OS disk size); the controller builds it
      for `cloud-hypervisor` Providers, mirrors its status, waits on it at
      deletion, and has RBAC for it. Data disks are refused for now.
- [x] `crates/banlieue-cloud-hypervisor` with the pinned spec check.
      *2026-09-26:* ADR-0061. v53.0 spec vendored with a sha256 `PIN`;
      fixtures captured from a real VMM; `make ch-live-test` runs it against
      one.
- [x] `crates/banlieue-provider-cloud-hypervisor`: `reconciler/provider.rs`,
      `reconciler/machine.rs`, `reconciler/vmimage.rs`, `supervisor.rs`
      (D-Bus), `net.rs` (netlink), `import.rs`. *2026-09-26:* landed flat as
      `provider.rs`, `reconciler.rs` + `machine.rs`, `vmimage.rs`
      (`BackingFile` and registry `Url` sources), `systemd.rs`, `sys.rs`
      (ioctls, not netlink) and `import.rs` (the import unit's subcommand).
      Host effects sit behind `host::HostOps` with a strict `fake.rs`.
- [x] `banlieue provider cloud-hypervisor` subcommand behind a Cargo feature
      (ADR-0004). *2026-09-26:* feature `cloud-hypervisor`, on by default;
      not a `banlieue bootstrap` backend (host-installed).
- [x] Shared NoCloud seed crate, libvirt provider migrated onto it.
      *2026-09-26:* `banlieue-provider-sdk::cloudinit`; libvirt re-exports.
- [x] Artifact delivery per ADR-0064. *2026-09-26/27, Decisions 1–5:*
      `crates/banlieue-oci` (first-party, pure Rust, gzip layer);
      `banlieue imagebuilder push` run by a token-less push Job,
      `status.buildArtifact.ociArtifact`; host `[registry]` pinning one
      repository; `banlieue-ch-import-<uid>.service` pulls by digest into
      every storage class (`sha256-<hex>.raw`, sparse); eviction beyond
      `keep_unreferenced`; the controller's `banlieue.io/host-image-cache`
      finalizer releases a deleted image once every host reports
      `Released`. Verified live end to end 2026-09-27: push, pull by digest,
      VM boot from the pulled image, release on delete.
- [x] Deletion finalizer: unit, swtpm unit, tap, disks, seed, state directory.
      *2026-09-26:* all but the swtpm unit, verified live. *2026-09-27:*
      swtpm and its manufacture unit stopped, EK directory removed and
      verified (unit tests; live run pending).
- [x] swtpm and `Deferred`. *2026-09-27, ADR-0065 Decisions 1–7:*
      one-shot manufacture as the provider user, EK certificates read
      host-side from `<state_root>/ek/<uid>/`, state handed to the guest by
      handle, swtpm before the VMM; `vtpm` advertised only when usable;
      `Provider.status.ekCaCertificates`. `Deferred`: empty sparse OS disk,
      installer second, ejected with `vm.remove-device` when the installed
      system reports `phase=installed` over vsock, `GuestReady` after.
      Unit-tested; `make ch-vtpm-e2e` passed live 2026-09-27, and `make
      ch-deferred-e2e` the same day: Kairos Hadron v0.4.0 installed,
      `COS_PERSISTENT` sealed to the vTPM, reported with `systemd-notify`
      over `vsock-stream:2:1024` (no extra package), installer ejected,
      `GuestReady` in 341 s, clean delete. The first live run found the
      VMM could not read the installer in the cache: each machine now gets
      its own read-only copy, deleted after the eject (ADR-0065 amended).
- [x] Host units as root-owned templates. *2026-09-27:* transient units let
      the provider's user start a unit as root (polkit sees only names);
      `banlieue-ch@`, `banlieue-swtpm@`, `banlieue-swtpm-setup@`,
      `banlieue-ch-import@` templates, polkit restricted to guest-range
      instances (ADR-0063 amended). Verified on a host; `make
      ch-polkit-test`.
- [x] Provider comparison. *2026-09-27:* living doc
      `docs/src/reference/provider-comparison.md` and `make provider-bench`;
      Cloud Hypervisor measured (Debian and Kairos), libvirt measured the
      same day on a second host. The vSphere and Proxmox columns are tasks
      in roadmaps 05 and 06, where the doc stays open until they land.
- [x] Host bootstrap script, guide, example manifests, threat model.
      *2026-09-26:* `scripts/bootstrap-cloud-hypervisor-host.sh` and
      `ch-host-provider-up.sh`; two guides (bootstrap; systemd, polkit,
      identities); example 21; full threat-model pass through ADR-0065
      (TB-8, TB-9, §7.11, five §8 entries).
- [ ] ADR-0067, then `banlieue host {preflight,status,selftest,install}`
      behind a Cargo feature; the shell script shrinks to a `--remote` wrapper
      (phase 10).
- [ ] `make ch-host-install-test`: the stages as root in a Debian 13 container.
- [ ] Test asserting the VMM pin equals ADR-0061's vendored-spec release.

## Tests

- [x] Client, supervisor and netlink behind traits, mocked in unit tests.
      *2026-09-26:* `HostOps` over all of them, faked strictly (it rejects
      what systemd and the VMM reject); the syscalls and systemd are also
      live-tested without root (`tests/live_sys.rs` in a user+net
      namespace, `tests/live_systemd.rs` on the session bus).
- [ ] VMM configuration JSON asserted against the vendored spec.
- [ ] Restart test: kill the provider mid-provision and mid-delete, assert it
      re-adopts and converges.
- [x] Leak test: create then delete leaves no unit, tap, file or directory.
      *2026-09-26:* `tests/e2e_machine.rs` (`make ch-e2e`), run on the host:
      VM → `Ready` with an address → sshd answers → unit, tap and
      directories present → delete → all of them gone. Green in 82 s.
- [x] Live lifecycle test, as ADR-0050 has for libvirt. *2026-09-26:* the
      same `e2e_machine.rs`; plus `live_sys.rs` and `live_systemd.rs` for the
      syscall and D-Bus halves without root. CI on a hosted runner with
      `/dev/kvm` is still unexplored. GitHub's hosted Linux
      runners expose `/dev/kvm`, which would put a real guest boot in CI
      without a self-hosted runner. Confirm before relying on it.

## Definition of done

- [ ] The stop condition holds live on a real host.
- [x] A `tpmEnabled` class installs `Deferred`, seals to its own vTPM, and
      leaves no swtpm state behind on delete. *2026-09-27, `make
      ch-deferred-e2e` on a host (the machine created directly).*
- [ ] Provider upgrade with guests running: zero guest restarts.
- [ ] `cargo deny` clean. No new native dependency in the binary.
- [ ] Roadmap 17 amended with phase G. Roadmap 14's per-class table updated.

## Gotchas

- **Disk order is boot order.** With no persistent UEFI variables the
  firmware takes the first bootable disk. OS disk first, install media second.
  Reverse them and a `Deferred` guest reinstalls itself on every reboot.
- **A guest reboot is not a new process.** The VMM resets in place with the
  same configuration. Anything "removed for next boot" must be removed from
  the live VMM too, or it is still there after the guest's own reboot.
- **The API socket is the guest's root.** Anyone who can write to it owns the
  VM. Mode 0600, owned by the provider, inside a directory the guest's uid
  cannot traverse.
- **The VMM configuration schema moves between releases.** Hence the pin.
- **Always attach virtio-rng.** A first boot that generates SSH host keys and
  a machine-id with a starved entropy pool is slow in a way that looks like a
  hang.
- **Hugepages are reserved, not allocated on demand.** Report them as their
  own capacity figure or the scheduler will overcommit a host that looks half
  empty.
- **Reflinks tie image eviction to machine lifetime.** See phase 4.
- **Always pass `image_type=raw`.** Found in the phase 0 spike: v53 otherwise
  auto-detects, warns that auto-detection is deprecated, and **disables
  sector-0 writes** on the disk, which breaks anything that rewrites the
  partition table.
- **`nested` defaults to on.** `vm.info` reports `cpus.nested: true` unless
  `nested=off` is passed. The environment constraint needs it off.
- **Growing the disk before first boot is required.** The Kairos
  `cloudImage` is exactly its payload size; its reset config adds a ~9 GiB
  `COS_STATE` and, with no free space, fails and leaves the guest in
  recovery with no user-data applied.
- **swtpm paths must be absolute.** Daemonized swtpm changes directory to
  `/`; a relative `--tpmstate` fails `CmdInit` (error `0x9`) and the VMM
  will not boot.
- **The serial log file is truncated on every guest reboot.** The VMM
  reopens it, so it only ever holds the current boot.
- **Guest disk names shift once the installer is unplugged** (the seed moves
  from `vdc` to `vdb` on the next boot). Address guest disks by label, never
  by `/dev/vdX`.
- **The `GuestReady` marker lands a few seconds after the guest answers
  SSH.** Kairos's `boot` stage runs late; "reachable" is not "announced".
- **No CD-ROM means the seed is found by label only.** Kairos probes
  `/dev/sr*` first, then every block device's filesystem label. On this
  backend only the second path exists, so the seed must be readable by the
  guest's label reader, not only mountable by the kernel.
