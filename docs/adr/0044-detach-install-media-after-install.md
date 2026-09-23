<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0044 — Detach install media once the guest is installed

- **Status:** Accepted
- **Date:** 2026-09-23
- **Deciders:** Erick Bourgeois
- **Related:** Gates on [ADR-0043](0043-guestready-installed-guest-signal.md);
  constrains [ADR-0040](0040-deferred-install-for-vtpm-encryption.md)'s
  deferred-install path; consumed by
  [ADR-0046](0046-virtualmachinepool.md)'s readiness gate; phase A4 of
  [roadmap 17](../../.github/community/17-ephemeral-vm-pools.md)

## Context

Under `installMode: Deferred` (ADR-0040) the install ISO stays attached to the
machine for its entire life. That is correct during the install — the guest
boots from it — and wrong immediately afterwards, for two reasons.

**The ISO is a shared artifact holding build-time material.** It carries the
baked cloud-config overlay (`VMImage.spec.cloudConfigs`, `isoOverlay`), and
§6/TB-5 of the threat model already states that anything embedded in it "should
be treated as readable by every hypervisor operator". A *guest* that keeps it
mounted extends that readership to the workload inside the VM. For roadmap
17's sandboxes the workload is, by assumption, a prompt-injected agent.

**A guest that can still see its installer can re-run it.** The install media
is bootable by construction. A domain that keeps it attached — and, on libvirt,
keeps `<boot dev='cdrom'/>` in its `<os>` block — is one reboot away from
re-entering the installer, which on a `Deferred`/TPM machine means re-sealing
a fresh disk and destroying whatever the previous tenant's workload left. That
is a availability and integrity problem, not merely an untidy one.

### Why not simply never attach it after first boot

Nothing in banlieue observes "first boot". The provider sees domain state, not
guest progress. ADR-0043 built exactly the missing signal — `guestInstalled`,
a sticky marker written only by a system booted from the *installed* disk — and
that is the only trustworthy edge at which the ISO becomes redundant.

### Why this must gate `GuestReady`, not follow it

A `VirtualMachinePool` binds a member the moment it reports `GuestReady`
(ADR-0046). If the detach happened *after* `GuestReady=True`, there would be a
window — one reconcile wide, and wider if the detach fails — in which a claim
can bind a member that still has its installer attached. The pool has no way
to tell the difference, because `GuestReady` is the only readiness input it
has. Ordering the detach *before* the condition is what makes "a bound member
never has install media" true by construction rather than by timing.

## Decision

**1. When `guestInstalled` flips true, eject the install media, then publish
`GuestReady`.** Never the other order. A member that has announced itself but
whose detach has not yet succeeded stays un-Ready and therefore unbindable.

**2. Record the outcome as `LibvirtMachineStatus.installMediaDetached`.** A
sticky boolean, like `guestInstalled`: an ejected ISO does not come back, and
a stopped domain has not become re-armed. It is also what makes the invariant
auditable from outside — `kubectl get libvirtmachine -o json` answers "could
this member have been bound with media attached?" without reading logs.

**3. On libvirt, eject via `virDomainUpdateDeviceFlags`** (wire procedure
`REMOTE_PROC_DOMAIN_UPDATE_DEVICE_FLAGS` = **174**, args
`{dom, xml, flags}` per `src/remote/remote_protocol.x`). Ejecting a cdrom is
an *update* of the existing device to one with no `<source>`, not a
`detach-device`: the drive stays, the medium leaves. Removing the device
outright would renumber the remaining disk targets, which is how you
accidentally move the seed ISO onto the installer's `<target dev=…>`.

Flags are `VIR_DOMAIN_AFFECT_LIVE | VIR_DOMAIN_AFFECT_CONFIG`
(`1 | 2`, verified in `libvirt-domain.h`) so the medium is gone from both the
running domain and its persistent definition. Live-only would let the ISO
return at the guest's next reboot, which — per the Context above — is exactly
the failure being prevented. `VIR_DOMAIN_DEVICE_MODIFY_FORCE` (`1 << 2`) is
**not** used by default: a guest holding a lock on the tray is information,
not an obstacle to bulldoze, and forcing it hides a guest that is still
reading the installer.

**4. The NoCloud `cidata` seed stays attached.** Scope decided deliberately:
this ADR ejects the *installer* only. Ejecting the seed as well would close a
real exposure — a sandbox workload can mount `CIDATA` and read its own
rendered user-data — but cloud-init re-reads its datasource on every boot, so
removing it risks regressing per-boot modules in a way that needs its own live
verification. Recorded as an accepted risk in the threat model rather than
silently bundled into this change.

**5. `Immediate`-mode machines are unaffected.** They have no install media
attached in the first place, so the step is a no-op and must not report
failure. `installMediaDetached` stays absent rather than being set false,
distinguishing "nothing to do" from "not done yet".

## Consequences

**A pool can no longer bind a member with install media attached**, and that
is now a structural property rather than a race the timing happens to win.
This is the property roadmap 17's stop condition asserts ("has no install
media attached"), and it becomes checkable.

**A failed eject is a stuck member, by design.** If `virDomainUpdateDeviceFlags`
errors, `GuestReady` is never published, the member never becomes available,
and `provisioningTimeoutSeconds` (ADR-0046) eventually reaps it as poisoned.
That is the correct outcome — the alternative is handing out a sandbox whose
installer is readable — but it does mean an eject that fails systematically
presents as a pool that never warms. The condition message must therefore name
the eject specifically, not merely say "not ready".

**One more RPC procedure in `banlieue-libvirt`.** Follows the ADR-0050
convention: pure `encode_*` half unit-tested without a daemon, procedure
number quoted with its source, and proven against a real libvirtd before being
trusted. The number was verified twice on paper — against libvirt's own
`remote_protocol.x` for the argument struct, and against an independently
generated constants table whose values for `DOMAIN_UNDEFINE_FLAGS` (231),
`DOMAIN_GET_STATE` (212), `DOMAIN_INTERFACE_ADDRESSES` (353) and
`DOMAIN_DEFINE_XML_FLAGS` (350) all match constants this crate had already
proved live.

**Verified live 2026-09-23.**
`tests/live_libvirtd.rs::update_device_flags_is_understood_by_real_libvirtd`
defines a throwaway domain that actually carries a cdrom and then ejects it
**`CONFIG`-only**, which a stopped domain can service. libvirtd returned
success — so the eject is not merely decodable, it *applies*: the daemon
located the device by its `<target dev=…>` and rewrote the persistent
definition. The same call with `LIVE|CONFIG` returns "domain is not running",
which is the expected semantic refusal for a stopped domain and still
exercises the whole decode path.

That distinction is deliberate. An earlier version of this test used only
`LIVE|CONFIG` against a diskless domain, so libvirt refused on the LIVE flag
before doing any work: it proved the wire format and nothing about whether a
cdrom would ever be found. A test that cannot fail for the reason you care
about is not evidence for it.

**vSphere is not implemented here.** The equivalent
(`build_remove_cdroms_reconfigure_spec`, already present for the `Immediate`
template path) is deferred for want of a vCenter to verify against — the same
position ADR-0043's vSphere transport is in. `VSphereMachineStatus` does not
gain the field until it can be tested, so the absent value keeps meaning
"nothing to do" on that backend too.

**A guest can still read its own user-data from the seed.** Decision 4's
deliberate residue, and the reason this ADR does not claim to make a sandbox's
bootstrap material unreadable from inside the sandbox.
