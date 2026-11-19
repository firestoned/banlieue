<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0141: The host-resident provider layer is extracted into a shared crate

- **Status:** Proposed
- **Date:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Related:** roadmap 20 (phase B),
  [ADR-0137](0137-firecracker-provider-class.md) (the `firecracker` class that
  needs it), [ADR-0138](0138-a-host-may-run-both-vmms-with-split-budgets.md)
  (two Providers on one host),
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md),
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md),
  [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md),
  [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md),
  [ADR-0107](0107-capacity-aware-scheduling.md),
  [ADR-0128](0128-every-provider-publishes-measured-capacity.md)

## Context

`banlieue-provider-cloud-hypervisor` is a host-resident provider: it runs on a
KVM host under systemd, creates taps, supervises one unit per guest, reads the
slice budget, learns guest addresses from the neighbour table, receives the
guest's phase over vsock and renews its own credential. ADR-0137 adds a second
host-resident provider, for Firecracker. Most of what that provider does on the
host is the same work.

Reading the crate (about 12,000 lines with tests) shows where the line between
"host-resident provider" and "Cloud Hypervisor" actually falls:

| Module | What it is | Names the VMM? |
| --- | --- | --- |
| `sys` | persistent tap creation and ownership, bridge membership, reflink clone, uid/gid | no |
| `systemd` | D-Bus client, unit state, start and stop, budget arithmetic over slices | only unit-name prefixes and the slice names |
| `neigh` | guest addresses from the IPv4 neighbour table (ADR-0062 Decision 5) | no |
| `token` | the provider renews its own cluster credential (ADR-0060 Decision 5) | no |
| `report` | the guest's phase over vsock (ADR-0065 Decision 5) | no, if both VMMs use the hybrid vsock socket model (see Decision 4) |
| `import` | one registry image into every storage class's cache (ADR-0064) | the artifact kind only |
| `host_config` | the host-local configuration | some sections (`vmm`, `tpm`) |
| `host` | the `HostOps` trait the reconciler drives | mixed: directories, disk, seed, taps, units and the slice budget are generic; TPM and install media are Cloud Hypervisor's |
| `provider` | `Provider.status`: facts, capacity, slice budget | the slice name |
| `plan`, `machine`, `reconciler`, `vmimage` | one machine's plan, convergence and Kubernetes glue | yes: `CloudHypervisorMachine` and the VMM client |
| `hostfs` | the machine's files | the TPM and installer parts |

The code that is expensive to get wrong is the generic part: `unsafe` ioctls
for taps, privilege boundaries (who owns what, which uid), the supervision
contract with systemd, and credential renewal. Copying it into a second crate
means two copies of security-sensitive code, each needing its own audit and each
able to drift.

## Decision

### 1. A new crate, `banlieue-provider-host`, holds the generic layer

A workspace crate (Linux only, like `banlieue-host`) that contains, moved and
not rewritten: `sys`, `systemd`, `neigh`, `token`, the generic part of `report`,
`import` and `host_config`, the generic operations of `host`, and the
capacity/status computation of `provider`. The `unsafe` audit
(`#![deny(unsafe_code)]` everywhere but `sys`, per-block `SAFETY` comments,
`#![deny(clippy::undocumented_unsafe_blocks)]`) moves with `sys` and is not
loosened.

### 2. The VMM is behind a trait, per class

The layer does not know a VMM. Each provider supplies a `Vmm` implementation
with: the unit template and its instance naming (the prefix replaces today's
hard-coded `banlieue-ch-`), the slice it runs in, how a machine plan becomes a
boot configuration, the runtime control calls it uses, and the capabilities it
declares absent (ADR-0137 Decision 7). Cloud Hypervisor's implementation keeps
`machine`, `plan`, `vmimage`, the TPM and install-media code, and its VMM
client. The Firecracker provider adds its own (ADR-0137).

`HostOps` is split along the table: the generic operations move to the layer;
the TPM and install-media operations stay in a Cloud Hypervisor extension
trait.

### 3. Two classes, two binaries, one layer

`banlieue provider cloud-hypervisor` and `banlieue provider firecracker` stay
separate processes, each serving its own `Provider` with its own
ServiceAccount, credential, polkit rule, units and slice (ADR-0138). A host
that runs both has two providers sharing code, not one process serving two. A
fault, restart or upgrade of one never touches the other.

### 4. Both VMMs use the hybrid vsock model

Both Cloud Hypervisor and Firecracker expose a guest's vsock as a Unix socket
on the host: a guest-initiated connection to port P arrives on the socket
`<path>_P`. The generic `report` listener is built on that, taking the path
and the port from the `Vmm` implementation. Both are verified: the Cloud
Hypervisor provider's `report` module states and implements it (a guest
connecting to host port `REPORT_PORT` reaches `<vsock socket>_<port>`), and the
Firecracker spike observed a guest connection to port 1025 arriving on
`<uds_path>_1025`.

### 5. The extraction is behaviour-preserving, and comes first

Extraction is its own change, before any Firecracker code: a pure move with the
tests moving beside the code, the Cloud Hypervisor provider unchanged in
behaviour, and its whole test suite passing as before (rule: a fake that is
more permissive than the real thing hides bugs, `rules/testing.md`; the fakes
move too and are not weakened). The Firecracker provider is then written on the
layer, never as a copy of the Cloud Hypervisor crate.

### 6. Not in the provider SDK

`banlieue-provider-sdk` stays VMM- and platform-neutral: the vSphere, libvirt
and Proxmox providers use it and have no tap, no systemd and no neighbour
table. Host-resident Linux code does not belong there.

## Consequences

- One audited copy of the tap, supervision and credential code serves both
  VMMs, and a fix lands once.
- A large mechanical change to a crate that works. The risk is regression in
  Cloud Hypervisor, mitigated by Decision 5: the move changes no behaviour and
  its suite must stay green, and the live tiers (`rules/testing.md`) are re-run
  against a real host before Firecracker code starts.
- `banlieue-provider-host` is Linux-only, so it is checked with the Linux-target
  clippy on macOS like the other host crates.
- Unit-name patterns (`banlieue-ch@`, `banlieue-swtpm@`) become properties of
  the `Vmm` implementation. Existing Cloud Hypervisor hosts keep their unit
  names; nothing on an installed host changes.
- Threat model: no new boundary and no new binary beyond ADR-0137's; the `unsafe`
  blocks move crates, so the audit statement in the threat model cites the new
  path. Pass due when implemented.
- Not implemented. Open: the crate name; the exact split of `host_config` and
  `host`; whether `import` generalises over artifact kinds here or per class
  (ADR-0137 Decision 4 needs a kernel-plus-root artifact, not a disk).
