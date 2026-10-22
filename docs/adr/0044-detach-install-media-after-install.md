<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0044 — Detach install media once the guest is installed

- **Status:** Accepted
- **Date:** 2026-09-23
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-10-12 (libvirt boot order: disk first, installer as
  fallback; see *Amendment 2026-10-12* at the end)
- **Amended:** 2026-10-23 (implemented on vSphere; see *Amendment
  2026-10-23*)
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

## Amendment 2026-10-12: on libvirt, the disk boots before the installer

**Context.** The libvirt domain listed `<boot dev='cdrom'/>` before
`<boot dev='hd'/>` until the eject. The eject waits for `guestInstalled`
(Decision 1), and `guestInstalled` is written only by a system booted from
the installed disk (ADR-0043). With the installer first, that boot never
happens. Found live on 2026-10-12 with a Kairos Debian 13 installer and a
`tpmEnabled` class on a libvirt host: the install completed (the disk held
2.78 GiB), the guest rebooted, and it came back up in the live installer
(`cdroot`, `/run/cos/live_mode`) with no marker. It would have reinstalled
in a loop until `provisioningTimeoutSeconds`. Every earlier libvirt pool run
used images that install nothing, so none of them could see it. vSphere was
not affected: its Deferred path reached `GuestReady` with the CD-ROM first
(2026-09-23).

**Decision.** On libvirt, a machine with install media lists the disk first
and the installer second. The disk is empty on first boot, so the firmware
skips it and boots the installer; after the install the disk is bootable
and wins. This is the order Cloud Hypervisor already uses (ADR-0065
Decision 3). The eject, and the suppression of the ISO from every later
definition, are unchanged.

**Consequences.**

- The "one reboot away from re-entering the installer" risk in Context is
  now smaller on libvirt even before the eject: a guest with a bootable disk
  boots that disk. The eject is still required, for the readership reason
  and because an installed disk that stops booting would fall through to
  the installer.
- A Deferred machine whose disk is *not* empty (none exists today:
  `InstallMedia` always creates an empty disk) would boot that disk instead
  of the installer.


## Amendment 2026-10-23: implemented on vSphere

**Context.** The Consequences above deferred vSphere "for want of a vCenter to
verify against". The cost of that deferral was found live on 2026-10-22: a
`Deferred` VM (an OpenBao node) still had its image's ISO connected to its
CD-ROM days after installing, because a `Deferred` template's CD-ROM has
`startConnected: true`, every clone inherits it, and nothing disconnects it.
When the image was re-imported with `forceUpload`, the import Job could not
delete the old ISO from that zone's datastore (vCenter answers
`500 Internal Server Error` for a file a running VM has open), the zone's
template had already been destroyed, and the `VMImage` stayed `Ready=False`.
So the readership exposure in Context is real on vSphere, and so is a second
cost: an installed VM blocks every later re-import of its image into its
zone.

**Decision.**

1. **Decision 1 holds on vSphere unchanged.** Once `guestInstalled` is true,
   the provider ejects the install media, and only then can `GuestReady` be
   `True`. A machine whose eject has not succeeded reports
   `GuestReady=False`, reason `InstallMediaAttached`, the libvirt reason.
2. **The eject is the one `govc device.cdrom.eject` performs.** Every CD-ROM
   backed by an ISO file is edited (`ReconfigVM_Task`) to a client-device
   backing (`VirtualCdromRemotePassthroughBackingInfo`) with `connected`,
   `startConnected` and `allowGuestControl` all false. The drive stays, as on
   libvirt (Decision 3): only the medium leaves, so nothing references the ISO
   and the guest cannot reconnect it. Device removal is not used: an IDE
   CD-ROM cannot be hot-removed from a running VM.
3. **`VSphereMachineStatus.installMediaDetached`** is the sticky record, as
   on libvirt (Decision 2). It is absent until the guest is installed.
   vSphere does not tell the provider whether the image was `Deferred`
   (`VSphereMachineSpec` carries no install mode), so the first installed
   observation reads the VM's devices and sets `true` whether it ejected an
   ISO or found none. That is the one difference from Decision 5: an
   `Immediate` clone (whose template's CD-ROM was removed at build time,
   ADR-0021) also records `true`, meaning "verified no install media", rather
   than staying absent. It costs one `config` read per machine, once.
4. **A guest that holds the tray locked is overridden** (revised
   2026-10-23, after the first live run; the opposite of libvirt's Decision
   3). vCenter asks the VM `msg.cdromdisconnect.locked` ("Disconnect anyway
   and override the lock?") and the reconfigure task waits on it; left
   alone, vCenter answers No itself after about four minutes and the eject
   fails, on every pass, forever. Found live: **every installed Kairos guest
   seen so far holds the tray locked**, and the VMs could not be logged into
   (no SSH, no console login), so "release it from inside the guest" was not
   available. The eject only happens once `guestInstalled` is true, when the
   guest no longer needs its installer, so the provider answers Yes, to that
   one question only (matched by its message id, the Yes key read from the
   labels). Any other question is left for a human. A failed eject is retried
   at the long requeue, not every pass.
5. **Machines provisioned before this change are covered.** Their
   `installMediaDetached` is absent and `guestInstalled` is already true, so
   their next pass ejects. A provider rollout therefore disconnects the
   install ISO from every installed `Deferred` VM it manages.

**Why Kairos's own `eject-cd` did not prevent this.** `eject-cd: true` makes
the installer write `/usr/lib/systemd/system-shutdown/eject`, a one-line
`/usr/bin/eject -rmF`, run when the live installer shuts down before its
reboot (`agent/pkg/action/install.go`, only when booted from CD). The
debian13 image (`kairos-init v4.4.0-rc1`) ships no `eject` binary (Debian's
`eject` package is not installed), so the hook fails silently, which systemd
allows for shutdown hooks. vCenter's event log for the affected VM shows one
power-on and no disconnect at install time. Even when the hook works, a guest
eject changes only the running VM: it cannot clear `startConnected` or
`allowGuestControl`, so the provider-side eject is needed either way.

**Consequences.**

- The "bound pool member never has install media" invariant now holds on
  vSphere too, and `kubectl get vspheremachine -o json` audits it.
- An installed VM no longer blocks a re-import of its image; the import's
  own pre-flight (ADR-0020, amendment 2026-10-23) covers whatever still does.
- **Verified live 2026-10-23** (vCenter 8.0.3), for the eject itself: on
  rollout the provider ejected the ISO from two installed `Deferred` VMs; both
  CD-ROMs read back with a client-device backing and `connected`,
  `startConnected` and `allowGuestControl` false, `installMediaDetached:
  true`, `GuestReady=True`. Clones without a CD-ROM recorded `true` without a
  reconfigure. Both guests held the tray locked; that run predates the
  automatic answer, and the lock question was answered Yes by hand. **The
  automatic answer (Decision 4) was then verified live the same day**: on a
  freshly installed `Deferred` clone the provider logged that the guest holds
  the tray locked and was overriding it, and the ISO was ejected 3 s later
  (no question left pending); a second installed VM behaved the same.
  The same rollout's re-import of the blocked zone passed the ADR-0020
  pre-flight and rebuilt the zone's template.
