<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0137: A `firecracker` provider class runs ephemeral sandbox microVMs on a KVM host

- **Status:** Accepted
- **Date:** 2026-11-19
- **Proposed:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Related:** roadmap 20 (the plan this ADR makes decisions for),
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md),
  [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md),
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md),
  [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md),
  [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md),
  [ADR-0052](0052-instant-clone-vmfork-not-supported.md),
  [ADR-0097](0097-api-ends-at-virtualmachineclaim.md),
  [ADR-0105](0105-cloud-hypervisor-guests-slice.md),
  [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md),
  [ADR-0138](0138-a-host-may-run-both-vmms-with-split-budgets.md) (how it
  shares a host with Cloud Hypervisor),
  [ADR-0140](0140-firecracker-guests-are-supervised-by-systemd-not-the-jailer.md)
  (supervision), [ADR-0142](0142-microvm-image-and-artifact-contract.md) (the image),
  [ADR-0143](0143-firecracker-host-attestation-ca-and-guest-signer.md) (attestation)

## Context

Roadmap 20 proposes Firecracker as the backend for ephemeral, homogeneous
sandbox pools, where time to ready, density and a small device model matter
more than device coverage. Its phase 0 is a spike that gates the rest. The
spike has now been run on two bare-metal hosts (a RHEL 9.8 host, since torn
down, and a RHEL 8.10 host) and answers most of the questions that gate
the design. What it measured, as facts and not intentions:

| Question | Result |
| --- | --- |
| Does the pinned VMM (v1.18.0, static binary) run on a host kernel outside upstream's supported list (5.10, 6.1)? | Yes: a microVM booted to a login prompt on kernel 4.18 (RHEL 8.10) and on AMD EPYC Rome, neither of which is on upstream's list. Upstream does not test them. |
| Can the VMM start fully configured, with no API call after start? | Yes: `--config-file` with `--no-api` boots the guest. Whether a runtime API socket is kept is open (phase 0). |
| Does a guest get a working network with no in-guest tooling? | Yes: the kernel argument `ip=<addr>::<gw>:<mask>::eth0:off` configures it before init; tap-to-bridge, host and guest ping both ways at 0.3 ms. The upstream CI kernel has IP autoconfig. |
| Does guest-to-host vsock work, and where does it land? | Yes: a guest connection to (CID 2, port P) reaches the Unix socket `<uds_path>_P` of **that** microVM on the host. The requester's identity is the socket the provider listens on for it. `vhost_vsock` is not used. |
| How does the guest leave? | `reboot=k` on the kernel command line turns a guest reboot into a VMM exit (no ACPI, so a guest poweroff only halts). |
| Can the existing sandbox image (Kairos) boot as it is? | Not within a 2 s budget: stock Kairos took 13 s, almost all of it boot orchestration (immucore, a 129 MB initrd), not services. A purpose-built layout (read-only squashfs root, a 2 MB initramfs that mounts a tmpfs overlay, a monolithic kernel) reached userspace in 1.8 s and multi-user.target about 1 s later, with no network and no SPIRE agent (measured on the first host; end to end to `GuestReady` is not yet measured). |
| Is there a vTPM? | No. The VMM has no TPM device. |

## Decision

### 1. A new class, `firecracker`, and its own InfraMachine

`FirecrackerMachine` (with `FirecrackerMachineTemplate`) satisfies the CAPI
v1beta2 InfraMachine contract (non-negotiable 2), beside
`CloudHypervisorMachine`; it is not a `vmm` field on that kind. Each class
keeps its own declared capabilities (non-negotiable 4), so the scheduler can
answer "can this Provider run this" from the Provider alone. The
`providerRef` is a `ProviderReference` and the provider serves tenant
namespaces through `ProviderScope` from the first commit (ADR-0133): it is
not built single-namespace and ported later.

### 2. Host-resident, supervised by systemd, as Cloud Hypervisor is

`banlieue provider firecracker` runs on the host as a systemd service,
outbound only, no listener, bound ServiceAccount token, no Secret reads
(ADR-0060). Each guest is a root-owned **template unit**
`banlieue-firecracker@<uid>.service` with a per-guest uid from a declared
range; the provider chooses only the instance; polkit allows `start`, `stop`,
`reset-failed` and `set-property MemoryMax` on guest instances only (ADR-0063).
The provider re-adopts running guests from the units and never trusts its own
memory. Guests live in `banlieue-firecracker.slice`, a sibling of
`banlieue-guests.slice`, whose budget is declared at install, never inferred,
and which the provider cannot change (ADR-0105, ADR-0138).

### 3. The VMM starts configured

The provider writes the guest's boot configuration (kernel, drives, network,
vsock, machine) and the unit starts the VMM from it, so a crash can never
leave a half-configured VMM. A runtime API socket, if kept, is for stop,
metrics and balloon only. The jailer question (jailer, or systemd as the
jailer, with seccomp always Firecracker's own) is a phase 0 item and is
decided in its own ADR with the spike's measurements; cgroups stay
systemd's either way (two writers in one subtree is the failure mode).

### 4. A guest is a kernel and a root filesystem, not a bootable disk

A `VMImage` for this class is a kernel (an uncompressed `vmlinux` with its
drivers built in), a small initramfs and a read-only root filesystem, laid
out as above, delivered to the host as one OCI artifact pulled by digest
([ADR-0142](0142-microvm-image-and-artifact-contract.md), on the path of
ADR-0064). Kairos A/B and recovery selection live in GRUB and do not
exist here; immutability comes from the read-only root plus a RAM overlay, so
nothing a guest writes survives it. The sandbox image's own build (what to
keep, drop and rebuild) is the sandbox's repository's decision (ADR-0097,
ADR-0108); banlieue declares only the contract: a kernel, an initramfs, a
root drive, a kernel command line, and a NoCloud seed on a second read-only
drive when user data is given (ADR-0054).

### 5. Static networking, from the kernel command line

The provider assigns the guest's address and gateway on the host's per-VMM
bridge and passes them with the `ip=` argument (all VM networking in banlieue
is static; DHCP is not an option). One tap per guest, named from its uid, on
a private bridge with no uplink, no NAT and no DHCP (ADR-0067 Decision 7,
ADR-0138 Decision 5).

### 6. Readiness and identity travel over vsock, guest-initiated only

Each guest gets a vsock device. The provider listens, **per guest**, on
`<uds_path>_1024` for readiness (ADR-0065) and `<uds_path>_1025` for the guest's
identity bootstrap, which the guest requests and the host never pushes. A guest
cannot reach another guest's sockets and nothing on the network can reach these
at all (vsock is not routed and is not netfilter traffic): the peer of a
connection is, by construction, that guest.

What travels on 1025 is mediatore's decision (ADR-0097), recorded in mediatore
ADR-0049 (Accepted): the guest generates its own key, sends a CSR, and the
provider signs a short-lived certificate for it with the host's own CA, which the
in-guest SPIRE agent presents to `x509pop` node attestation. The private key never
leaves the guest, and the provider never logs a request or a certificate body.
Firecracker has no TPM, so the TPM attestor cannot be used; `join_token` was
built and tested over this same channel and rejected, because creating a join
token needs the SPIRE admin identity that mediatore forbids itself. banlieue's
part is the contract above (a per-guest listener whose peer is that guest), the
host's attestation CA and the signer ([ADR-0143](0143-firecracker-host-attestation-ca-and-guest-signer.md)):
the administrator asserts the CA on the Provider, never the host's own status
(ADR-0049 Decision 10), and the machine's status publishes the subject the signer
will use, so the node entry's parent can be computed before the guest boots.

### 7. Declared absences, refused at admission

The class declares absent, and admission refuses a request for: a vTPM
(`tpmEnabled: true`), so no sealing, no deferred sealed install and no EK
path; hotplug, so a `VMClass` change is a `Recreate`; devices beyond block,
net, vsock, balloon and entropy; live migration (roadmap 14 records
`Recreate` only). **Snapshot restore is not used**: ADR-0052 stands, and v1 is
cold boot only. Reviving snapshot-restore clones needs its own ADR to scope
ADR-0052 for this class.

### 8. Hosts outside upstream's support are allowed, and say so

Upstream supports host kernels 5.10 and 6.1 on listed CPUs. Hosts here run
older enterprise kernels, and the spike shows the VMM works on one. banlieue
does not refuse such a host: it records the host kernel and CPU in
`Provider.status` and treats "outside upstream's tested list" as a fact an
operator can see, not as a block. A defect on such a host is not an upstream
bug until reproduced on a supported one.

## Consequences

- A sandbox pool can use a backend with a much smaller device model and a
  cold boot measured in about a second, beside Cloud Hypervisor.
- A Firecracker pool member is unsealed and ephemeral by design. It has no
  disk of its own: its root is the image's read-only root filesystem, shared by
  every guest of the image, with writes going to a RAM overlay that dies with
  the guest (ADR-0142 Decision 4).
- Because there is no vTPM, **node attestation cannot be the TPM path** for
  this class. mediatore ADR-0049 (Accepted) chooses `x509pop` with a
  certificate the host signs for the guest's own key, and ADR-0143 designs
  banlieue's side. Until `Provider.spec.attestation.hostCa` and
  `FirecrackerMachineStatus.attestationSubject` exist, a `firecracker` pool
  member cannot be bound to a claim.
- Roadmap 20's reserved ADR numbers (0116 to 0122) are stale: the next free
  number was 0137. The jailer, the shared host layer extracted from the Cloud
  Hypervisor provider, and the image/artifact contract each need their own
  ADR, numbered at the time they are written.
- The second spike host (RHEL 8, systemd 239, cgroup v1) has no systemd userdb,
  so the per-guest uid scheme of Decision 2 needs the files-database mode, and
  its CPU budget is a quota, not pinned cores: both are
  [ADR-0139](0139-hosts-without-systemd-userdb-or-cgroup-v2.md), a prerequisite
  for running this class on such a host.
- Not implemented. Open items: runtime API socket; jailer; end to end
  time-to-`GuestReady`; the token/attestor ADR in mediatore; CALM and the
  threat-model pass, due on implementation (ADR-0138 adds one boundary pair).
