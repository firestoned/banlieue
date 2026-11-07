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
> `GuestReady`, and runs inside the host's `banlieue-guests.slice` budget.
> Deleting it removes the VMM process, the jail, the disks, the seed and the
> tap. Restarting or upgrading the provider does not disturb a running guest.
> A `GuestReady` pool on a `firecracker` Provider warms and refills, and its
> numbers sit next to Cloud Hypervisor's in the provider comparison.
>
> **Status (2026-11-07): not started.** Nothing in the tree yet.

Baseline when written: `933f94d` (2026-11-07). ADRs 0001 to 0115 exist or
are reserved (0057 to 0059 by roadmap 15, 0070 to 0073 by roadmap 18, 0076
to 0080 by roadmap 19). **This roadmap reserves ADR-0116 to ADR-0122.**
Renumber at landing if anything else claims them first.

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
| **Direct kernel boot only.** No UEFI, no BIOS. | A `VMImage` for this class is a kernel plus a root filesystem (phase E), not a bootable disk. Kairos `cloudImage` and ISO images do not boot as-is: Kairos boot selection (active, passive, recovery) lives in GRUB. Extracting a kernel and initrd from a Kairos image is a spike item, expected to fail; the sandbox image is purpose-built (ADR-0108: guest software is the consumer's image content). `trustedBoot` (UKI, ADR-0051) is meaningless here. |
| **No vTPM.** | The class rejects `tpmEnabled: true`. That rules out first-boot sealing (ADR-0088, ADR-0112), `Deferred` installs sealed to a vTPM (ADR-0040, ADR-0065), and the EK certificate path (ADR-0045, ADR-0049 Decision 10). A Firecracker pool member is unsealed and ephemeral by design: its disk is a per-member copy deleted at release. Whether a host-side, per-member, discarded-at-delete encryption key is worth adding is an ADR-0118 question. Sandbox identity comes from whatever attestation mediatore's broker uses that does not need a TPM; that is mediatore's decision (ADR-0097), but phase 0 confirms the guest can reach it. |
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
| `firecracker.slice` with the host's aggregate caps | Firecracker guests go into the **existing** `banlieue-guests.slice`. The budget is "guests on this host", whatever the VMM. ADR-0105 already has `MemoryMax`, `CPUWeight`, `TasksMax` and `AllowedCPUs`; **`IOWeight` is missing** and is added for both VMMs (phase G). | ADR-0105, ADR-0106 |
| Each VM as a transient unit (`systemd-run --slice=…`) or a template | **Template units only**, root-owned, `banlieue-fc@<uid>.service`. Transient units were removed from the Cloud Hypervisor design because polkit cannot stop a provider from describing a `User=root` transient unit. | ADR-0063 amendment (2026-09-27) |
| The jailer handles chroot, uid/gid, netns **and seccomp** | Seccomp is **Firecracker's own** (built-in filters, on by default); the jailer does chroot, uid/gid, cgroups, netns, device nodes and rlimits. Whether to run the jailer at all is the phase 0 gate below. | Firecracker docs; phase 0 |
| `--parent-cgroup` at the slice, *or* systemd owns the cgroups | systemd owns the cgroups. Two writers in one cgroup subtree is the failure mode cgroup v2 delegation exists to prevent. If the jailer is used, it gets no cgroup role (or a `Delegate=yes` leaf, decided in ADR-0118). | phase 0, ADR-0118 |
| Agent creates tap and disk, starts the unit, then drives the API (`/boot-source`, `/drives`, `/network-interfaces`, `/machine-config`, `/actions InstanceStart`) | Same steps, with one choice open: drive the API after start, or hand the VMM a `--config-file` the provider writes so the process **starts configured** (no window where a crash leaves a half-configured VMM). Recommendation: config file for boot, API for runtime (stop, metrics, balloon). Decided in ADR-0120. | phase 0, ADR-0120 |
| State reconciles from systemd units plus sockets, so the agent restarts without losing VMs | Already how Cloud Hypervisor works: the provider re-adopts from units and never trusts its memory. Reused, not rewritten (phase B). | ADR-0063 Decision 2 |
| One provider trait with Firecracker and Cloud Hypervisor backends | A **shared host-provider layer** (systemd, taps, host filesystem, neighbour table, token renewal, `Provider.status` reporting) extracted from `banlieue-provider-cloud-hypervisor`, behind a trait that each VMM implements. Two **provider classes** and two InfraMachine CRDs stay separate. | phase B, ADR-0121, non-negotiable 2 |
| AZ grouping sits above both | Already true: failure domains, spread and capacity-aware placement are the controller's (ADR-0030, ADR-0098, ADR-0107), above every provider. A `firecracker` Provider reports the same failure-domain and capacity fields; nothing new is needed above it. | ADR-0098, ADR-0107 |

### Why two classes, not one class with a `vmm` field

A single `host-kvm` class whose machine picks its VMM would put two different
capability sets behind one `ProviderClass`, so the scheduler could no longer
answer "can this Provider run this class" from the Provider alone, and
`CloudHypervisorMachine` would grow fields that mean nothing for half its
instances. Each class keeps its own CAPI-contract InfraMachine (non-negotiable
2) and its own declared capabilities (non-negotiable 4). Sharing happens in
code, below the CRDs.

### One VMM per host, in v1

ADR-0060 makes one `Provider` one host, fenced by a Lease. A host running both
VMMs would be two Providers on one host, both reporting the same CPU and
memory to ADR-0107's capacity-aware scheduler, which then double-counts it.
**v1: a host is either a `cloud-hypervisor` host or a `firecracker` host.**
`banlieue host firecracker preflight` refuses a host that already has the
Cloud Hypervisor provider installed, and vice versa. Mixed hosts (one process
serving two Providers with one capacity report, or a shared budget split
declared at install) are a later ADR if a real host needs it.

## Fixed constraints (not up for re-litigation here)

| Constraint | Source |
| --- | --- |
| No RPC between controller and providers. CRDs and the Kubernetes API only. | non-negotiable 1 |
| Infra CRDs satisfy the CAPI v1beta2 InfraMachine contract, with a `*Template` kind. | non-negotiable 2, ADR-0050 Decision 2 |
| Host-resident, outbound-only, bound ServiceAccount token, no Secret reads. | ADR-0060 |
| Root-owned template units; the provider picks only the instance; per-guest uid from a declared range; polkit allows `start`, `stop`, `reset-failed` and `set-property MemoryMax` on guest instances only. | ADR-0063 |
| Guests live in `banlieue-guests.slice`; its budget is declared at install, never inferred; the provider cannot change it. | ADR-0105, ADR-0106 |
| Artifacts reach the host through an OCI registry, content-addressed, pulled by a transient import unit. | ADR-0064 |
| First-party, pure-Rust clients. No FFI, no subprocess in a reconcile path. | ADR-0011, ADR-0061 |
| No fork-from-golden. | ADR-0052 (phase J is the only place this is revisited, and only by a new ADR) |
| banlieue's API ends at `VirtualMachineClaim`; the sandbox itself is mediatore's. | ADR-0097 |
| No Helm. | maintainer |
| No real hostnames or addresses in tracked files. | `rules/no-real-infrastructure.md` |

---

## 0. Spike and decision gate

One bare-metal KVM host, a shell, no code in the tree. Pin one Firecracker
release (VMM and jailer) and record its version and digest.

- [ ] **Host support.** Record the host kernel against Firecracker's
      supported host kernel list. Note whether the host is outside it and
      what that costs.
- [ ] **Boot.** A purpose-built guest kernel (uncompressed `vmlinux`,
      virtio-mmio drivers built in; check whether the pinned release has
      virtio-pci and whether it helps) and an ext4 root filesystem. Serial
      console to a file. Record time from `InstanceStart` to userspace.
- [ ] **Kairos.** Try extracting the kernel and initrd from a Kairos raw
      image and booting it. Expected to fail or to lose A/B and recovery;
      record which, so the question is closed.
- [ ] **Config file vs API.** Boot the same guest both ways
      (`--config-file`, and the PUT sequence then `InstanceStart`). Record
      what each leaves behind when the VMM is killed mid-configuration.
- [ ] **Jailer vs systemd-as-jailer.** Two units, same guest:
      (J1) root unit running the jailer, which chroots, drops to the guest
      uid and execs the VMM; (J2) no jailer, the unit itself runs as the
      guest uid with `RootDirectory=`/`BindReadOnlyPaths=`,
      `DeviceAllow=/dev/kvm`, `/dev/net/tun`, `PrivateNetwork=` or not, and
      the ADR-0063 hardening set. For each: which of the jailer's guarantees
      hold, which process ever runs as root, and whether the polkit rule
      still only allows instance choice.
- [ ] **cgroups.** Under `banlieue-guests.slice`, show that J1's jailer
      cgroup handling either is off or writes only into a `Delegate=yes`
      leaf, and that `systemd-cgtop banlieue.slice` and the slice's
      `MemoryMax` see the VMM.
- [ ] **Network.** Tap on the host bridge (as Cloud Hypervisor), guest gets
      an address, the host neighbour table maps its MAC (phase 5 of roadmap
      09 relies on this). Then the per-guest network namespace variant: what
      it takes (veth pair into the namespace, tap inside it) and what it buys
      over ADR-0109's L2-only sandbox segment. Recommendation going in: no
      per-guest netns in v1.
- [ ] **Seed.** The NoCloud seed (ADR-0054, ADR-0114) as a read-only second
      drive; cloud-init in the purpose-built rootfs finds it.
- [ ] **vsock.** Firecracker's host-side vsock is a Unix socket with a
      `CONNECT <port>` handshake, like Cloud Hypervisor's. Show the existing
      guest reporter (ADR-0065's vsock report) works unchanged.
- [ ] **Overhead.** Extend nothing yet: run ten guests and measure per-guest
      VMM memory and CPU by hand, against the same guest on Cloud Hypervisor.
      This sets the `MemoryMax` overhead constant.
- [ ] **Stop.** What a graceful stop is (`SendCtrlAltDel` is x86-only and
      needs the guest's cooperation), and how long to wait before `SIGKILL`.

**Gate.** Write ADR-0116 with the answers. If the time to `GuestReady` is not
materially below Cloud Hypervisor's 30 s for the same workload, stop here and
record why: a second VMM is not free, and density alone may not pay for it.

## A. ADRs and CALM (before any code)

- [ ] **ADR-0116**: Firecracker is a first-class, host-resident provider
      class; one VMM per host in v1; the capability gaps above declared
      absent.
- [ ] **ADR-0117**: `FirecrackerMachine` / `FirecrackerMachineTemplate`
      InfraMachine contract (spec, status, conditions, `hostUid`, addresses,
      `GuestReady`; no TPM fields).
- [ ] **ADR-0118**: host supervision. `banlieue-fc@.service` (and any
      import template), jailer or not, cgroup ownership, per-guest uid range,
      polkit rule, file layout as access control, per-member disk encryption
      or not.
- [ ] **ADR-0119**: the kernel-plus-rootfs `VMImage` shape: how a kernel,
      optional initrd, root filesystem and boot arguments are declared,
      built, pushed (ADR-0064) and verified.
- [ ] **ADR-0120**: `banlieue-firecracker`, the VMM client crate (as
      ADR-0061 is for Cloud Hypervisor); config file vs API for boot.
- [ ] **ADR-0121**: the shared host-provider layer and its VMM trait.
- [ ] **ADR-0122**: reserved for phase J; written only if phase J is opened.
- [ ] CALM: new node for the Firecracker provider and VMM, relationships to
      the API server, the registry and the host, controls from ADR-0118.
      `make calm-validate`, `make calm-diagrams`.

## B. Shared host-provider layer (refactor, no behaviour change)

- [ ] Extract the VMM-independent parts of `banlieue-provider-cloud-hypervisor`
      (`systemd.rs`, `hostfs.rs`, `neigh.rs`, `token.rs`, `report.rs`,
      `sys.rs`, `host_config.rs`, the adopt-from-units loop) into a crate
      named by ADR-0121, behind a trait the VMM implements: render a unit
      instance's inputs, start, observe, stop, adopt.
- [ ] Cloud Hypervisor implements it first. `make ch-e2e`, `ch-restart-e2e`
      and `ch-sealing-isolation-e2e` stay green: this phase ships nothing new.

## C. `banlieue-firecracker` client crate

- [ ] Pure-Rust HTTP over the API Unix socket, typed requests for
      boot-source, drives, network interfaces, machine config, vsock,
      balloon, entropy, logger, metrics and actions; the config-file
      document if ADR-0120 picks it.
- [ ] Unit tests against a fake that **rejects** what the real VMM rejects
      (a second `InstanceStart`, configuration after start), per
      `rules/testing.md` rule 1.
- [ ] Live protocol tier against a real VMM on a host (`#[ignore]`,
      fails loudly when no host is configured).

## D. API, CRDs and dispatch

- [ ] `FirecrackerMachine` and `FirecrackerMachineTemplate` in
      `banlieue-api`; `regen-crds`; examples; API reference.
- [ ] `firecracker` provider class, `deployment: External`, seeded by
      `banlieue bootstrap operator` like `cloud-hypervisor`.
- [ ] Admission: `tpmEnabled`, `trustedBoot`, `firstBootSealing`,
      `installMode: Deferred` and non-kernel image kinds refused for the
      class, each with a message naming the gap.
- [ ] Controller dispatch to the new InfraMachine kind; `VirtualMachine`
      status mirrors it (non-negotiable 6).

## E. Kernel-plus-rootfs images

- [ ] The `VMImage` shape from ADR-0119, `BackingFile` included (an artifact
      already in the host cache, ADR-0064 Decision 5).
- [ ] Imagebuilder path, or a documented external build, that produces and
      pushes the artifact pair by digest.
- [ ] A reference sandbox kernel config and rootfs recipe under `examples/`
      or `scripts/`, with no real infrastructure in it.

## F. Provider lifecycle

- [ ] Create: per-member disk copy, seed, tap, unit start, boot.
- [ ] Observe: address from the neighbour table, `GuestReady` over vsock.
- [ ] Delete: stop, then remove jail, disks, seed, tap, run directory.
- [ ] Re-adopt after a provider restart or upgrade; never restart a guest.
- [ ] `Provider.status`: CPU, memory, storage targets, bridges, failure
      domain, and the slice budget if ADR-0105's follow-up has landed.
- [ ] e2e, in the production topology and identity (`rules/testing.md`
      rule 3): `make fc-e2e`, `make fc-restart-e2e`.

## G. Host install

- [ ] `banlieue host firecracker {preflight,status,selftest,install}`
      (ADR-0067 shape): pinned VMM and jailer by digest (ADR-0084,
      ADR-0103), templates, polkit rule, userdb range, refusing a host that
      has the other VMM's provider.
- [ ] `overhead` finds `banlieue-fc@` units (ADR-0106 Decision 4).
- [ ] `--guest-io-weight` → `IOWeight=` on `banlieue-guests.slice`, for both
      VMMs (amends ADR-0105).

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
- [ ] **Threat-model full pass** for each ADR as it lands: new component and
      actor, the jailer's root step (if kept), the API socket, the
      unsealed-disk posture recorded against the sealed one, §5 diagram.

## J. Snapshot-restore clones (gated; may never open)

Firecracker's fastest path is restoring a booted snapshot into many guests.
ADR-0052 rules that out, for reasons that apply here too: a restored guest
holds its parent's identity, network state, RNG state and any secret that was
in memory when the snapshot was taken. This phase opens **only** if phases
F and H show cold boot is the bottleneck for a real workload, and then only
with ADR-0122, which must answer before any code:

- [ ] what may be in memory at snapshot time (nothing per-member, nothing
      secret), and how that is verified;
- [ ] RNG and clock after restore (VMGenID support in the pinned release);
- [ ] identity, address and user-data after restore (MMDS or vsock, since
      cloud-init's first boot has already run);
- [ ] how it scopes ADR-0052 to this class without weakening it elsewhere.

## Definition of done

- [ ] Stop condition above, verified live on a host.
- [ ] ADR-0116 to ADR-0121 accepted, each with its threat-model pass.
- [ ] Cloud Hypervisor's e2e suite green on the shared host-provider layer.
- [ ] `cargo deny` clean; no native dependency added.
- [ ] Provider comparison and `ROADMAPS.md` updated.
