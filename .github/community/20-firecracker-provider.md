# 20: Firecracker provider, the sandbox backend

> **Goal.** A `VirtualMachine` (and so a `VirtualMachinePool` member) can land
> on a bare-metal KVM host running [Firecracker](https://firecracker-microvm.github.io/)
> microVMs, through a new `FirecrackerMachine` InfraMachine CRD and
> `Provider`s of class `firecracker`. Firecracker is the backend for
> **ephemeral, homogeneous** workloads (AI agent sandboxes, roadmap 17), where
> density and time to ready matter more than device coverage. Cloud
> Hypervisor stays the backend for anything that needs a general-purpose
> guest, UEFI, a vTPM, or devices.
>
> **Stop condition.** A `VirtualMachine` with `spec.userData`, scheduled onto a
> `firecracker` `Provider`, becomes a running microVM that booted a declared
> kernel and root filesystem, consumed its user-data, reports its address and
> `GuestReady`, and runs inside the host's `banlieue-firecracker.slice` budget
> (ADR-0138; the slice was `banlieue-guests.slice` when this was written).
> Deleting it removes the VMM process, the seed and the tap (there is no jail
> under ADR-0140 and no per-member disk under ADR-0142). Restarting or upgrading the provider does not disturb a running guest.
> A `GuestReady` pool on a `firecracker` Provider warms and refills, and its
> numbers sit next to Cloud Hypervisor's in the provider comparison.
>
> **Status (2026-11-19): in progress; the API and two libraries exist, nothing
> runs a guest.** The phase 0 spike has been run on two bare-metal hosts and its
> measurements are recorded in ADRs 0137, 0138, 0140, 0142 and 0143 (see "What
> the ADRs decided" below); ADR-0139, the installer prerequisite for a host
> without systemd userdb or cgroup v2, is implemented (phase 1, installer only).
> CALM models all of ADR-0137 to ADR-0143 (`docs/architecture/calm/architecture.json`).
> In the tree (corrected 2026-11-19; an earlier version of this paragraph said
> none of it existed): the `FirecrackerMachine` and `FirecrackerMachineTemplate`
> types and CRDs, `Provider.spec.attestation.hostCa`, the `OciArtifact` image
> kind, the `attestationSubject` mirror to the `VirtualMachine` and the claim
> (#214), the microvm artifact format in `banlieue-oci` and an attestation signer
> in `banlieue-provider-firecracker` (#224). Not in the tree: a provider, a host
> installer verb, controller dispatch, or any caller of either library. The
> served parts had their threat-model pass on 2026-11-19. The end-to-end time to
> `GuestReady` is not measured, so the phase 0 gate below is still open.
>
> **Before the provider links an attestation signer:** ADR-0146 makes
> `banlieue-attestation-ca` the one implementation both providers share. The
> `attest` module in `banlieue-provider-firecracker` is a second one, and is to
> be replaced by, or reconciled with, the shared crate first.
>
> **Build it shared-capable from the first commit** (ADR-0133): `FirecrackerMachine.spec.providerRef`
> is a `ProviderReference`, and the provider serves tenant namespaces through `ProviderScope` and
> reads its `Provider` and credentials from its home namespace. Do not build it single-namespace
> and port it later.

Baseline when written: `933f94d` (2026-11-07). ADRs 0001 to 0115 exist or
are reserved (0057 to 0059 by roadmap 15, 0070 to 0073 by roadmap 18, 0076
to 0080 by roadmap 19). **This roadmap reserved ADR-0116 to ADR-0122. Those
numbers were not used:** other ADRs took the range, and the next free number
when the design was written was 0137. The ADRs below carry the decisions the
reservation was for. Text further down that still says ADR-0116 to ADR-0122
is the original plan, kept for the record and annotated where superseded.

### What the ADRs decided

| ADR | Decides | Replaces in this plan |
| --- | --- | --- |
| [0137](../../docs/adr/0137-firecracker-provider-class.md) | The `firecracker` class and its own `FirecrackerMachine` InfraMachine; host-resident and shared-capable from the first commit; the VMM starts configured; a guest is a kernel and a root filesystem; static `ip=` networking; readiness on vsock `_1024` and identity bootstrap on `_1025`, guest-initiated; declared absences refused at admission; hosts outside upstream's tested list allowed and recorded. Carries the spike's measurements. | planned ADR-0116, part of ADR-0117 |
| [0138](../../docs/adr/0138-a-host-may-run-both-vmms-with-split-budgets.md) | A host may run both VMMs, each its own Provider with its own sibling slice (`banlieue-firecracker.slice` beside `banlieue-guests.slice`, both under `banlieue.slice`) and a declared, disjoint, preflight-checked budget; capacity is each Provider's own slice. | withdraws "One VMM per host, in v1" |
| [0139](../../docs/adr/0139-hosts-without-systemd-userdb-or-cgroup-v2.md) | Installer prerequisite, not a Firecracker decision: guest accounts in the files database on a host without systemd userdb, CPU budgets by quota on cgroup v1. Implemented (phase 1, installer only). | nothing; a prerequisite found by the spike's second host |
| [0140](../../docs/adr/0140-firecracker-guests-are-supervised-by-systemd-not-the-jailer.md) | systemd is the jailer (no Firecracker jailer in v1); the VMM starts from a config file with `--no-api`, no API socket; `Restart=no`; stop is `systemctl stop`; the template removes stale sockets and the provider prepares the tap and config; accounting on. The missing chroot is a recorded gap. | planned ADR-0118 and ADR-0120 |
| [0141](../../docs/adr/0141-shared-host-provider-layer.md) | A `banlieue-provider-host` crate extracted first, as a pure move, from the Cloud Hypervisor provider, with the VMM behind a `Vmm` trait; two processes, one layer. | planned ADR-0121 |
| [0142](../../docs/adr/0142-microvm-image-and-artifact-contract.md) | A microvm image is one OCI artifact of kernel, initramfs and root filesystem plus a config blob, pulled by digest into a directory cache; `VMImage` source kind `OciArtifact`; the root is shared read-only with a RAM overlay, so no per-guest disk; the provider composes the kernel command line; the pull fails closed. | planned ADR-0119 |
| [0143](../../docs/adr/0143-firecracker-host-attestation-ca-and-guest-signer.md) | The host's attestation CA, asserted by the administrator on `Provider.spec.attestation.hostCa`; `FirecrackerMachineStatus.attestationSubject`; the per-guest signer on vsock `_1025`. With mediatore ADR-0049 (`x509pop`) it replaces the identity idea this plan left open. | new; none was planned |

Not decided by any ADR yet, and so still open: the `FirecrackerMachine` spec
and status fields beyond `attestationSubject` (the planned ADR-0117), the
snapshot-restore question (phase J, planned ADR-0122), and a typed VMM client
(the planned ADR-0120 client crate, which ADR-0140 makes unnecessary for v1,
phase C).

## Why Firecracker, when Cloud Hypervisor already works

Roadmap 09 brought Cloud Hypervisor in for sandbox attack surface, and it
delivered: a sealed pool member reaches `GuestReady` in 30 s (roadmap 18).
Firecracker is a narrower tool for a narrower job:

1. **Smaller device model.** Block, net, vsock, balloon, entropy and a serial
   console. No PCI passthrough, no virtiofs, no firmware. For a guest whose
   threat model starts with "prompt-injected agent", every device not present
   is one not to defend.
2. **Direct kernel boot, no firmware.** No edk2, no GRUB, no menu wait.
   Roadmap 18 phase C found that firmware and GRUB were the largest share of
   Cloud Hypervisor's time to ready. Firecracker removes both by construction.
3. **Density.** Per-guest VMM overhead is small enough that `warmReplicas` is
   bounded by guest memory, not by the VMM. Phase 0 measures this against the
   same guest on Cloud Hypervisor rather than assuming it.
4. **Licence.** Apache-2.0, as is its jailer.

What it is **not**: a replacement for Cloud Hypervisor. The gaps below are
permanent properties of the VMM, not missing work.

## The gaps, and what each one rules out

Checked against Cloud Hypervisor before committing. Each row is a capability
the `firecracker` class **declares absent** (non-negotiable 4: explicit over
implicit), so a request for it is refused at admission, never silently
degraded.

| Gap | Consequence in banlieue |
| --- | --- |
| **Direct kernel boot only.** No UEFI, no BIOS. | A `VMImage` for this class is a kernel plus a root filesystem (phase E), not a bootable disk. Kairos `cloudImage` and ISO images do not boot as-is: Kairos boot selection (active, passive, recovery) lives in GRUB. Spike result (ADR-0137): stock Kairos does boot but took 13 s, almost all boot orchestration (immucore, a 129 MB initrd), against 1.8 s to userspace for a purpose-built layout; A/B and recovery selection do not exist here. The sandbox image is purpose-built (ADR-0108: guest software is the consumer's image content). `trustedBoot` (UKI, ADR-0051) is meaningless here. |
| **No vTPM.** | The class rejects `tpmEnabled: true`. That rules out first-boot sealing (ADR-0088, ADR-0112), `Deferred` installs sealed to a vTPM (ADR-0040, ADR-0065), and the EK certificate path (ADR-0045, ADR-0049 Decision 10). A Firecracker pool member is unsealed and ephemeral by design. ~~Its disk is a per-member copy deleted at release.~~ **Superseded by ADR-0142 Decision 4:** there is no per-member disk; the root is the image's read-only root filesystem, shared by every guest of the image, with writes in a RAM overlay that dies with the guest. ~~Whether a per-member encryption key is worth adding is an ADR-0118 question.~~ Moot with no per-member disk. ~~Sandbox identity comes from whatever attestation mediatore's broker uses that does not need a TPM.~~ **Decided:** `x509pop` node attestation with a certificate the host signs for the guest's own key (mediatore ADR-0049, banlieue ADR-0143); the join-token idea was built and tested over the same channel and rejected, because creating a join token needs the SPIRE admin identity that mediatore forbids itself (ADR-0137 Decision 6). |
| **No hotplug.** CPU and memory are fixed at boot; only a balloon can take memory back. | A `VMClass` change is a `Recreate`, never an in-place resize. The balloon is not used to overcommit in v1: `MemoryMax` stays guest memory plus overhead, as on Cloud Hypervisor. |
| **Limited devices.** Block, net, vsock, balloon, entropy. | No virtiofs, no VFIO, no GPU. `Provider.status` reports none; a class that needs one cannot be scheduled here. Cloud-init arrives on a second read-only block device (the NoCloud seed, ADR-0054), as it already does on Cloud Hypervisor. |
| **No live migration.** Snapshot and restore only. | Roadmap 14's per-class table records `firecracker` as `Recreate`-only. |
| **Snapshot restore is memory forking.** | It is Firecracker's headline feature for fast clones, and **ADR-0052 rules it out** ("a restored or forked guest does not re-read its identity"; roadmap 18 keeps that stance). v1 is **cold boot only**. Phase J is a gated, possibly never, path that would need its own ADR to scope ADR-0052 for this class (see there). |

**Rule of thumb, recorded in the provider comparison:** Firecracker for
ephemeral, homogeneous, untrusted workloads where cold-boot time and density
matter; Cloud Hypervisor for anything needing a general-purpose guest, UEFI,
a vTPM, sealing, or devices; libvirt for everything else on KVM.

## How the pasted shape maps onto banlieue

The design this roadmap started from is close to what roadmap 09 already
built for Cloud Hypervisor. Where it differs, an existing ADR already decided
the point, and this roadmap follows the ADR:

| Proposed | banlieue's answer | Source |
| --- | --- | --- |
| A host agent that "implements the banlieue provider API" | A **host-resident provider**: `banlieue provider firecracker` runs on the host as a systemd service, talks to the API server outbound only, opens no listener. The provider API *is* the CRDs: there is no RPC to implement. | ADR-0060, non-negotiable 1 |
| `firecracker.slice` with the host's aggregate caps | ~~Firecracker guests go into the **existing** `banlieue-guests.slice`.~~ **Superseded by ADR-0138:** Firecracker guests go into `banlieue-firecracker.slice`, a sibling of `banlieue-guests.slice` under `banlieue.slice`, with the same ADR-0105 flags and its own declared budget. (Not `banlieue-guests-firecracker.slice`: systemd would nest it in the Cloud Hypervisor budget.) ADR-0105 already has `MemoryMax`, `CPUWeight`, `TasksMax` and `AllowedCPUs` (on cgroup v1 a quota, ADR-0139); **`IOWeight` is missing** and is added for both VMMs' slices (phase G). | ADR-0105, ADR-0106, ADR-0138 |
| Each VM as a transient unit (`systemd-run --slice=…`) or a template | **Template units only**, root-owned, `banlieue-firecracker@<uid>.service` (ADR-0137 Decision 2; the plan's `banlieue-fc@` name was not kept). Transient units were removed from the Cloud Hypervisor design because polkit cannot stop a provider from describing a `User=root` transient unit. | ADR-0063 amendment (2026-09-27) |
| The jailer handles chroot, uid/gid, netns **and seccomp** | Seccomp is **Firecracker's own** (built-in filters, on by default); the jailer does chroot, uid/gid, cgroups, netns, device nodes and rlimits. **Decided by ADR-0140: the jailer is not used in v1**; systemd sets the uid, sandbox and cgroup, and the VMM keeps Firecracker's own seccomp. The missing chroot is an accepted, recorded gap. | Firecracker docs; ADR-0140 |
| `--parent-cgroup` at the slice, *or* systemd owns the cgroups | systemd owns the cgroups. Two writers in one cgroup subtree is the failure mode cgroup v2 delegation exists to prevent. The jailer is not used (ADR-0140); measured on cgroup v1, its `--cgroup` without `--parent-cgroup` moved the VMM out of the slice budget, and a test is to refuse that invocation if a jailer is ever added. | ADR-0140 |
| Agent creates tap and disk, starts the unit, then drives the API (`/boot-source`, `/drives`, `/network-interfaces`, `/machine-config`, `/actions InstanceStart`) | Same steps, with one choice open: drive the API after start, or hand the VMM a `--config-file` the provider writes so the process **starts configured** (no window where a crash leaves a half-configured VMM). ~~Recommendation: config file for boot, API for runtime.~~ **Decided by ADR-0140: config file with `--no-api`, no API socket at all in v1** (fastest mode measured, smallest interface, one fewer socket to own and clean); a socket is added only by amending that ADR. | ADR-0140 |
| State reconciles from systemd units plus sockets, so the agent restarts without losing VMs | Already how Cloud Hypervisor works: the provider re-adopts from units and never trusts its memory. Reused, not rewritten (phase B). | ADR-0063 Decision 2 |
| One provider trait with Firecracker and Cloud Hypervisor backends | A **shared host-provider layer** (systemd, taps, host filesystem, neighbour table, token renewal, `Provider.status` reporting) extracted from `banlieue-provider-cloud-hypervisor`, behind a trait that each VMM implements. Two **provider classes** and two InfraMachine CRDs stay separate. **Decided by ADR-0141:** the crate is `banlieue-provider-host`, extracted first as a behaviour-preserving move. | phase B, ADR-0141, non-negotiable 2 |
| AZ grouping sits above both | Already true: failure domains, spread and capacity-aware placement are the controller's (ADR-0030, ADR-0098, ADR-0107), above every provider. A `firecracker` Provider reports the same failure-domain and capacity fields; nothing new is needed above it. | ADR-0098, ADR-0107 |

### Why two classes, not one class with a `vmm` field

A single `host-kvm` class whose machine picks its VMM would put two different
capability sets behind one `ProviderClass`, so the scheduler could no longer
answer "can this Provider run this class" from the Provider alone, and
`CloudHypervisorMachine` would grow fields that mean nothing for half its
instances. Each class keeps its own CAPI-contract InfraMachine (non-negotiable
2) and its own declared capabilities (non-negotiable 4). Sharing happens in
code, below the CRDs.

### One VMM per host, in v1 (superseded by ADR-0138)

> **Withdrawn.** [ADR-0138](../../docs/adr/0138-a-host-may-run-both-vmms-with-split-budgets.md)
> replaces this section. The argument below was written before ADR-0107 and
> ADR-0128 made a Provider report its **slice budget** instead of the host's
> totals; with a sibling slice per VMM the double count disappears. A host may
> be both a Cloud Hypervisor host and a Firecracker host, as two Providers with
> separate ServiceAccounts, units, uid ranges, bridges and slices; each
> `install` checks that the two budgets are disjoint and fits the host
> (`preflight` reads the other VMM's slice; it no longer refuses the other VMM).
> ADR-0060 stands: one Provider is one VMM on one host.

Original text, kept for the record:

~~ADR-0060 makes one `Provider` one host, fenced by a Lease. A host running both
VMMs would be two Providers on one host, both reporting the same CPU and
memory to ADR-0107's capacity-aware scheduler, which then double-counts it.
**v1: a host is either a `cloud-hypervisor` host or a `firecracker` host.**
`banlieue host firecracker preflight` refuses a host that already has the
Cloud Hypervisor provider installed, and vice versa.~~

## Fixed constraints (not up for re-litigation here)

| Constraint | Source |
| --- | --- |
| No RPC between controller and providers. CRDs and the Kubernetes API only. | non-negotiable 1 |
| Infra CRDs satisfy the CAPI v1beta2 InfraMachine contract, with a `*Template` kind. | non-negotiable 2, ADR-0050 Decision 2 |
| Host-resident, outbound-only, bound ServiceAccount token, no Secret reads. | ADR-0060 |
| Root-owned template units; the provider picks only the instance; per-guest uid from a declared range; polkit allows `start`, `stop`, `reset-failed` and `set-property MemoryMax` on guest instances only. | ADR-0063 |
| Guests live in a budgeted slice under `banlieue.slice` (`banlieue-guests.slice` for Cloud Hypervisor, `banlieue-firecracker.slice` for Firecracker); the budget is declared at install, never inferred; the provider cannot change it. | ADR-0105, ADR-0106, ADR-0138 |
| Artifacts reach the host through an OCI registry, content-addressed, pulled by a transient import unit; the host pins its repository. | ADR-0064, ADR-0142 |
| First-party, pure-Rust clients. No FFI, no subprocess in a reconcile path. | ADR-0011, ADR-0061 |
| No fork-from-golden. | ADR-0052 (phase J is the only place this is revisited, and only by a new ADR) |
| banlieue's API ends at `VirtualMachineClaim`; the sandbox itself is mediatore's. | ADR-0097 |
| No Helm. | maintainer |
| No real hostnames or addresses in tracked files. | `rules/no-real-infrastructure.md` |

---

## 0. Spike and decision gate

One bare-metal KVM host, a shell, no code in the tree. Pin one Firecracker
release (VMM and jailer) and record its version and digest.

> **Run, on two hosts (ADR-0137, ADR-0140).** The spike pinned Firecracker
> v1.18.0 (static binary; the ADRs record the version, not a digest). It ran on
> a RHEL 9.8 host, since torn down, and on a RHEL 8.10 host (kernel 4.18,
> systemd 239, cgroup v1, AMD EPYC Rome). Only what an ADR records as measured
> is ticked below; every item the ADRs list as not measured stays open, with a
> note saying what is missing.

- [x] **Host support.** Record the host kernel against Firecracker's
      supported host kernel list. Note whether the host is outside it and
      what that costs. *Measured (ADR-0137): v1.18.0 booted a microVM to a
      login prompt on kernel 4.18 (RHEL 8.10) and on AMD EPYC Rome, neither on
      upstream's list (5.10, 6.1) and untested by upstream. The cost is
      decided: such a host is allowed, and the kernel and CPU are recorded in
      `Provider.status` as a visible fact, not a block (ADR-0137 Decision 8,
      not implemented).*
- [x] **Boot.** A purpose-built guest kernel (uncompressed `vmlinux`,
      virtio-mmio drivers built in; check whether the pinned release has
      virtio-pci and whether it helps) and an ext4 root filesystem. Serial
      console to a file. Record time from `InstanceStart` to userspace.
      *Measured (ADR-0137, first host): a purpose-built layout reached
      userspace in 1.8 s and multi-user.target about 1 s later, with no
      network and no SPIRE agent. The layout was a monolithic kernel of about
      44 MB, a 2 MB initramfs and a read-only squashfs root with a tmpfs
      overlay, not ext4. On the second host the spike's Ubuntu guest booted in
      1.64 to 1.70 s with no NIC and 1.91 to 2.03 s with a NIC and a hardened
      unit (ADR-0140). Not recorded: the virtio-pci question and the serial
      console to a file. Not measured: time to `GuestReady`.*
- [x] **Kairos.** Try extracting the kernel and initrd from a Kairos raw
      image and booting it. Expected to fail or to lose A/B and recovery;
      record which, so the question is closed. *Closed (ADR-0137 Decision 4):
      stock Kairos booted but took 13 s, almost all boot orchestration, so it
      does not fit the 2 s budget; A/B and recovery selection live in GRUB and
      do not exist here. The image is purpose-built (ADR-0142).*
- [x] **Config file vs API.** Boot the same guest both ways
      (`--config-file`, and the PUT sequence then `InstanceStart`). Record
      what each leaves behind when the VMM is killed mid-configuration.
      *Measured (ADR-0140): config file with `--no-api` 1670 ms median, with
      `--api-sock` 1751 ms, API only 1703 ms (no NIC); after a SIGKILL and a
      restart the file boots the guest again, while API mode returns an empty
      VMM that needs every call again. Stale vsock and API sockets block a
      restart after any stop and are removed in `ExecStartPre`. Decided: config
      file, no API socket.*
- [ ] **Jailer vs systemd-as-jailer.** *Partly measured, decided (ADR-0140):
      the jailer against a direct, hardened unit, on the RHEL 8.10 host. The
      chroot-equivalent variant (J2 below, `RootDirectory=` with
      `BindReadOnlyPaths=`) was NOT measured, so this item stays open; ADR-0140
      records the chroot as an accepted gap and leaves the variant to a new
      ADR.* Two units, same guest:
      (J1) root unit running the jailer, which chroots, drops to the guest
      uid and execs the VMM; (J2) no jailer, the unit itself runs as the
      guest uid with `RootDirectory=`/`BindReadOnlyPaths=`,
      `DeviceAllow=/dev/kvm`, `/dev/net/tun`, `PrivateNetwork=` or not, and
      the ADR-0063 hardening set. For each: which of the jailer's guarantees
      hold, which process ever runs as root, and whether the polkit rule
      still only allows instance choice.
- [ ] **cgroups.** *Partly measured (ADR-0140, cgroup v1, in
      `banlieue-firecracker.slice`): the jailer's `--cgroup` without
      `--parent-cgroup` put the VMM outside the slice budget, with
      `--parent-cgroup` it stayed inside, and the cgroups it makes outlive the
      VMM unseen by systemd. Not recorded: a `Delegate=yes` leaf, and
      `systemd-cgtop` output.* Under the slice, show that J1's jailer
      cgroup handling either is off or writes only into a `Delegate=yes`
      leaf, and that `systemd-cgtop banlieue.slice` and the slice's
      `MemoryMax` see the VMM.
- [ ] **Network.** *Partly measured (ADR-0137): tap to bridge, the kernel
      `ip=` argument configures the guest before init, host and guest ping
      both ways at 0.3 ms. Not measured: the host neighbour table mapping the
      guest's MAC, and the per-guest network namespace variant.* Tap on the host bridge (as Cloud Hypervisor), guest gets
      an address, the host neighbour table maps its MAC (phase 5 of roadmap
      09 relies on this). Then the per-guest network namespace variant: what
      it takes (veth pair into the namespace, tap inside it) and what it buys
      over ADR-0109's L2-only sandbox segment. Recommendation going in: no
      per-guest netns in v1.
- [ ] **Seed.** The NoCloud seed (ADR-0054, ADR-0114) as a read-only second
      drive; cloud-init in the purpose-built rootfs finds it.
- [ ] **vsock.** *Partly measured (ADR-0137, ADR-0141): a guest connection to
      (CID 2, port P) arrives on the Unix socket `<uds_path>_P` of that
      microVM, `vhost_vsock` is not used, and port 1025 was observed. Not
      shown: that the existing guest reporter (ADR-0065) works unchanged.*
      Firecracker's host-side vsock is a Unix socket with a
      `CONNECT <port>` handshake, like Cloud Hypervisor's. Show the existing
      guest reporter (ADR-0065's vsock report) works unchanged.
- [ ] **Overhead.** *Not measured as written (ADR-0140): only one 512 MiB
      guest at idle (RSS and PSS 106 to 109 MB); ADR-0140 says the
      `MemoryMax` overhead constant must be set from a measurement under load,
      not from those figures, and no ten-guest or Cloud Hypervisor comparison
      is recorded.* Extend nothing yet: run ten guests and measure per-guest
      VMM memory and CPU by hand, against the same guest on Cloud Hypervisor.
      This sets the `MemoryMax` overhead constant.
- [x] **Stop.** What a graceful stop is (`SendCtrlAltDel` is x86-only and
      needs the guest's cooperation), and how long to wait before `SIGKILL`.
      *Measured and decided (ADR-0140 Decision 4): SIGTERM is not handled by
      Firecracker, it dies by signal in about 70 ms with no guest shutdown;
      that is the whole stop path (`systemctl stop`) for a guest whose disk is
      discarded. `SendCtrlAltDel` was measured only in API mode (VMM exit 0 in
      331 ms) and is not used in v1.*

**Gate.** ~~Write ADR-0116 with the answers.~~ The answers are written as
ADR-0137 (the class and the spike's measurements) and ADR-0140 (jailer, config
file, supervision), not ADR-0116. **The gate itself is still open:** if the time
to `GuestReady` is not materially below Cloud Hypervisor's 30 s for the same
workload, stop here and record why: a second VMM is not free, and density alone
may not pay for it. The ADRs measure about 1.8 s to userspace and about 1 s
more to multi-user.target for the purpose-built layout (first host), with no
network and no SPIRE agent; the end-to-end figure to `GuestReady` is not yet
measured.

## A. ADRs and CALM (before any code)

The reserved numbers 0116 to 0122 were not used; each item is ticked when an
ADR records the decision, under the number it really has. A ticked box means
the ADR is written, not that anything is built.

- [x] **ADR-0137** (was 0116): Firecracker is a first-class, host-resident
      provider class, with the capability gaps declared absent. (Its "one VMM
      per host" half is withdrawn by ADR-0138, below.)
- [x] **ADR-0138** (new): a host may run both VMMs, each its own Provider with
      a sibling slice and a split, checked budget. Withdraws "One VMM per host,
      in v1".
- [x] **ADR-0139** (new, prerequisite): guest accounts in the files database
      and CPU quotas on a host without systemd userdb or cgroup v2. Installer
      phase implemented.
- [ ] **ADR-0117**: `FirecrackerMachine` / `FirecrackerMachineTemplate`
      InfraMachine contract (spec, status, conditions, `hostUid`, addresses,
      `GuestReady`; no TPM fields). *Not written as its own ADR.* ADR-0137
      Decision 1 names the kind and ADR-0143 adds
      `status.attestationSubject`; the rest of the spec and status is open.
- [x] **ADR-0140** (was 0118): host supervision. systemd is the jailer, the
      template is `banlieue-firecracker@.service`, the VMM starts from a config
      file with no API socket, `Restart=no`. Per-member disk encryption is moot
      (no per-member disk, ADR-0142). The import template belongs to ADR-0142.
- [x] **ADR-0142** (was 0119): the kernel-plus-rootfs image: one OCI artifact,
      the `OciArtifact` source kind, a directory cache, a shared read-only
      root, a provider-composed command line, a fail-closed pull.
- [x] **ADR-0120**: `banlieue-firecracker`, the VMM client crate (as
      ADR-0061 is for Cloud Hypervisor). *Config file vs API for boot is
      decided by ADR-0140 (config file, no API socket); with no API socket there
      is no HTTP client to write in v1, so this ADR is not needed unless a
      later amendment adds the socket.* *Closed 2026-11-27 against the tree:* superseded by ADR-0140 Decision 3; no such crate exists.
- [x] **ADR-0141** (was 0121): the shared host-provider layer and its VMM
      trait, crate `banlieue-provider-host`.
- [x] **ADR-0143** (new): the host attestation CA, the admin-asserted
      `hostCa`, `attestationSubject` and the per-guest signer.
- [ ] **ADR-0122**: reserved for phase J; written only if phase J is opened.
      The number was not kept free: take the next free one at the time.
- [x] CALM: nodes for the Firecracker provider, the guest microVM, the host,
      the host installer, the shared layer, the machine CRD, the image artifact
      and cache, the attestation CA and the three slices; relationships to the
      API server, the registry, the host and the existing SPIRE and mediatore
      nodes; controls and two flows, all marked `DESIGNED, NOT IMPLEMENTED`.
      `make calm-validate` clean, `make calm-diagrams` regenerated.

## B. Shared host-provider layer (refactor, no behaviour change)

- [ ] Extract the VMM-independent parts of `banlieue-provider-cloud-hypervisor`
      (`systemd.rs`, `hostfs.rs`, `neigh.rs`, `token.rs`, `report.rs`,
      `sys.rs`, `host_config.rs`, the adopt-from-units loop) into a crate
      named by ADR-0141 (`banlieue-provider-host`), behind a `Vmm` trait the
      VMM implements: render a unit instance's inputs, start, observe, stop,
      adopt. ADR-0141 has the module-by-module split and lists the open
      questions (the exact split of `host_config` and `host`).
- [ ] Cloud Hypervisor implements it first. `make ch-e2e`, `ch-restart-e2e`
      and `ch-sealing-isolation-e2e` stay green: this phase ships nothing new.

## C. `banlieue-firecracker` client crate

> **Superseded for v1 by ADR-0140 Decision 3.** The VMM starts from a config
> file with `--no-api`, so there is no runtime API to call and no typed HTTP
> client to write. What remains is the config-file document type and its
> unit tests. The checklist below is kept as the plan this replaces; it
> applies only if an amendment adds the API socket.

- [x] Pure-Rust HTTP over the API Unix socket, typed requests for
      boot-source, drives, network interfaces, machine config, vsock,
      balloon, entropy, logger, metrics and actions; the config-file
      document if ADR-0120 picks it.
      *Closed 2026-11-27 against the tree:* superseded by ADR-0140 Decision 3 (config file, `--no-api`, no API socket); no `banlieue-firecracker` crate exists or is needed.
- [x] Unit tests against a fake that **rejects** what the real VMM rejects
      (a second `InstanceStart`, configuration after start), per
      `rules/testing.md` rule 1.
      *Closed 2026-11-27 against the tree:* superseded by ADR-0140 Decision 3 (config file, `--no-api`, no API socket); no `banlieue-firecracker` crate exists or is needed.
- [x] Live protocol tier against a real VMM on a host (`#[ignore]`,
      fails loudly when no host is configured).
      *Closed 2026-11-27 against the tree:* superseded by ADR-0140 Decision 3 (config file, `--no-api`, no API socket); no `banlieue-firecracker` crate exists or is needed.

## D. API, CRDs and dispatch

- [x] `FirecrackerMachine` and `FirecrackerMachineTemplate` in
      `banlieue-api`; `regen-crds`; examples; API reference. *Done (#214):
      `crates/banlieue-api/src/infrastructure/firecracker_machine.rs`,
      `deploy/crds/infrastructure.banlieue.io_firecrackermachine{s,templates}.yaml`,
      `examples/30-firecrackermachine.yaml`, `docs/src/reference/api.md`. The spec
      beyond what ADR-0137 and ADR-0143 name is still the open ADR-0117 question.*
- [x] `Provider.spec.attestation.hostCa` (exactly one certificate, admission
      rule on the pattern of `ekTrustBundle`) and
      `FirecrackerMachineStatus.attestationSubject` with its claim mirror
      (ADR-0143); `ImageSource` kind `OciArtifact` (ADR-0142). *Done: `hostCa`
      with `deploy/admission/provider-attestation-hostca.yaml` (one source) and
      `ProviderAttestation::validate` (one certificate); the subject mirrored
      machine to VM to claim (`reconciler/status_mirror.rs`, `claim.rs`);
      `OciArtifact`, which the existing providers report unsupported.*
- [ ] `firecracker` provider class, `deployment: External`, seeded by
      `banlieue bootstrap operator` like `cloud-hypervisor`.
- [ ] Admission: `tpmEnabled`, `trustedBoot`, `firstBootSealing`,
      `installMode: Deferred` and non-kernel image kinds refused for the
      class, each with a message naming the gap.
- [ ] Controller dispatch to the new InfraMachine kind; `VirtualMachine`
      status mirrors it (non-negotiable 6). *The mirror's read side exists
      (`InfraMachineRead` for `FirecrackerMachine`); the controller does not yet
      create, watch or hold a grant on the kind.*

## E. Kernel-plus-rootfs images

- [ ] The `VMImage` shape from ADR-0142 (`OciArtifact`; the plan said ADR-0119),
      `BackingFile` included only if ADR-0142's directory cache admits it
      (the ADR does not say).
- [ ] ~~Imagebuilder path, or a documented external build, that produces and
      pushes the artifact pair by digest.~~ **Replaced by ADR-0142 Decision 7:**
      the image is the consumer's own build; a first-party `banlieue-oci` push
      subcommand (multi-layer push and pull) is run from the consumer's
      pipeline, so banlieue's imagebuilder, `OSArtifact` and PVC are not
      involved. *The library half exists (#224, `crates/banlieue-oci/src/microvm.rs`:
      manifest, push, fail-closed pull); the subcommand and any caller do not.*
- [ ] A reference sandbox kernel config and rootfs recipe under `examples/`
      or `scripts/`, with no real infrastructure in it.

## F. Provider lifecycle

- [ ] Create: ~~per-member disk copy~~ (superseded by ADR-0142 Decision 4: the
      root is shared read-only, nothing to clone), seed, tap, config file, unit
      start, boot.
- [ ] Observe: address from the neighbour table, `GuestReady` over vsock
      (`_1024`), and the attestation signer on `_1025` (ADR-0143). *A signer
      library exists in `banlieue-provider-firecracker` (#224), unlinked; use
      `banlieue-attestation-ca` (ADR-0146) instead, see the status note above.*
- [ ] Delete: stop, then remove seed, tap, config file, run directory (no jail
      under ADR-0140, no per-member disk under ADR-0142).
- [ ] Re-adopt after a provider restart or upgrade; never restart a guest.
- [ ] `Provider.status`: CPU, memory, storage targets, bridges, failure
      domain, and the slice budget if ADR-0105's follow-up has landed.
- [ ] e2e, in the production topology and identity (`rules/testing.md`
      rule 3): `make fc-e2e`, `make fc-restart-e2e`.

## G. Host install

- [ ] `banlieue host firecracker {preflight,status,selftest,install}`
      (ADR-0067 shape): pinned VMM by digest (ADR-0084, ADR-0103; no jailer,
      ADR-0140), templates, polkit rule, userdb range (or files accounts,
      ADR-0139), the attestation CA stage (ADR-0143), and a preflight that
      checks the other VMM's slice for overlap and over-commit instead of
      refusing the other VMM (ADR-0138 Decision 3, replacing the plan's
      refusal).
- [ ] `overhead` finds `banlieue-firecracker@` units (ADR-0106 Decision 4).
- [ ] `--guest-io-weight` → `IOWeight=` on each VMM's slice
      (`banlieue-guests.slice` and `banlieue-firecracker.slice`; amends
      ADR-0105).

## H. Pools

- [ ] A `GuestReady` `VirtualMachinePool` on a `firecracker` Provider warms,
      is claimed, releases and refills (`e2e_pool_claim.rs`).
- [ ] Spread across two Firecracker hosts in different failure domains
      (ADR-0098).
- [ ] Record warm and refill times next to Cloud Hypervisor's.

## I. Docs and threat model

- [ ] Provider comparison gains a `firecracker` column, with the rule of
      thumb above.
- [ ] Guide: host install, image build, a first sandbox pool.
- [ ] Roadmap 14's per-class table and roadmap 17's backend list.
- [ ] **Threat-model full pass** for each ADR as it lands. *Partly done:* ADD
      runs it after implementation, and ADR-0137 to ADR-0143 each say it is
      due then (ADR-0139's installer phase had its pass). The parts in the tree
      (the served CRDs and image kind, `hostCa`, the subject mirror, the CAPI
      role, the two uncalled libraries) had theirs on 2026-11-19; the provider,
      host and image-pull parts get theirs when they land. Items to cover: new
      component and actor, the accepted no-chroot gap (ADR-0140; the jailer's
      root step does not exist), no API socket, the shared read-only root and
      world-readable image cache, the attestation CA key and the vsock signer
      (a new trust-anchor class), the two-budget DoS boundary pair (ADR-0138),
      the unsealed-disk posture recorded against the sealed one, §5 diagram.

## J. Snapshot-restore clones (gated; may never open)

Firecracker's fastest path is restoring a booted snapshot into many guests.
ADR-0052 rules that out, for reasons that apply here too: a restored guest
holds its parent's identity, network state, RNG state and any secret that was
in memory when the snapshot was taken. This phase opens **only** if phases
F and H show cold boot is the bottleneck for a real workload, and then only
with a new ADR (this plan reserved 0122; take the next free number), which
must answer before any code:

- [ ] what may be in memory at snapshot time (nothing per-member, nothing
      secret), and how that is verified;
- [ ] RNG and clock after restore (VMGenID support in the pinned release);
- [ ] identity, address and user-data after restore (MMDS or vsock, since
      cloud-init's first boot has already run);
- [ ] how it scopes ADR-0052 to this class without weakening it elsewhere.

## Definition of done

- [ ] Stop condition above, verified live on a host.
- [ ] ADR-0137, ADR-0138, ADR-0140, ADR-0141, ADR-0142 and ADR-0143 accepted
      and implemented, each with its threat-model pass (ADR-0139's installer
      phase is already implemented; the plan's ADR-0116 to ADR-0121 were not
      used).
- [ ] Cloud Hypervisor's e2e suite green on the shared host-provider layer.
- [ ] `cargo deny` clean; no native dependency added.
- [ ] Provider comparison and `ROADMAPS.md` updated.
