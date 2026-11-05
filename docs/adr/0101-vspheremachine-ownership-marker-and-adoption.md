<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0101: VSphereMachine ownership marker, and recovering a clone the provider lost

- **Status:** Accepted
- **Date:** 2026-10-14
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-11-04 (a pass decides from a live read of the machine,
  never from the watch cache; a recorded `vmRef` whose VM is gone is
  recovered by the marker. Found when every OpenBao VM rolled onto a new image
  destroyed its own healthy clone 13 ms after provisioning it)
- **Related:** Amends [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)
  (the create path) and [ADR-0026](0026-vspheremachine-deletion-lifecycle.md)
  (deletion with no `status.vmRef`); follows
  [ADR-0075](0075-proxmoxmachine-inframachine-contract.md) Decision 4 (the
  Proxmox ownership marker); leaves
  [ADR-0089](0089-vspheremachine-custom-attributes.md)'s custom attributes as
  they are

## Context

A `VSphereMachine` learns which VM is its own from one place:
`status.vmRef`, the moref `CloneVM_Task` returned, written by the status
patch at the end of a successful pass. Nothing on the VM itself says which
machine it belongs to.

That leaves a window. Between the clone completing and `status.vmRef`
reaching the API server, the VM exists and nothing records it. The window
holds five vCenter calls (custom attributes, OS disk floor, vTPM,
endorsement certificates, power state) and the status patch itself. If the
provider loses the moref inside it, the next pass clones again under the
same name, vCenter refuses with `DuplicateName` (names are unique per
folder), and the machine fails that way on every pass from then on.

The moref is lost in three ways:

1. **A step after the clone faults.** Fixed without an ADR (bug-179):
   `finish_clone` runs those steps as one unit and `roll_back_clone`
   destroys the clone if any of them fails.
2. **The provider stops inside the window**: a crash, an OOM kill, a node
   drain, a leader-election handover. No code runs to roll back, so a
   rollback cannot cover it.
3. **The machine is deleted inside the window.** Fixed for the success path
   (bug-177: a 404 on the status write destroys the clone). A deletion that
   races a provider *restart* still finds `status.vmRef` empty, and ADR-0026
   finalizes that as "nothing to destroy", leaking the VM.

Cases 2 and 3 can only be closed by finding the VM again without
`status.vmRef`, which means looking it up by name. ADR-0026 rejected exactly
that, for a sound reason: a display name is not an identity. Names are unique
only per folder, two namespaces can both have a `web-1`, and an
administrator can create a VM called anything. A name lookup that destroys
or adopts what it finds would eventually take someone else's VM.

Proxmox faced the same problem and answered it in ADR-0075 Decision 4: the
clone carries the machine's UID as a marker, set atomically by the clone
itself, and a name match counts only when the marker matches too. vSphere can
do the same, because the clone already writes `extraConfig` (the `guestinfo`
keys of ADR-0024) as part of the `CloneSpec`, in the same task that creates
the VM.

## Decision

### 1. The clone carries the machine's UID in `extraConfig`

`CloneVmRequest.extra_config` gains one key, set in the same
`VirtualMachineCloneSpec.config` as the `guestinfo` keys:

```text
banlieue.machine.uid = <VSphereMachine metadata.uid>
```

Because it is written by `CloneVM_Task`, there is no moment at which a
banlieue VM exists without it.

The key is deliberately **not** under `guestinfo.`. Every `guestinfo.*` key
can be read and written from inside the guest (`vmware-rpctool`), and the
guest is the least trusted party in the system. Other `extraConfig` keys are visible only
through the vSphere API, to users with the `VirtualMachine.Config.AdvancedConfig`
privilege. A guest cannot forge, copy or erase this marker.

### 2. A VM is ours only if its marker equals the machine's UID exactly

Name, folder, template, custom attributes (ADR-0089) and power state are
never evidence of ownership. Only an exact, whole-value match of
`banlieue.machine.uid` is. A VM without the key, or with another UID, is not
ours, whatever it is called.

### 3. When `status.vmRef` is unset, look before cloning

On a pass with no `status.vmRef`, the create path first reads the direct
children of the folder the clone would target (resolved exactly as
`clone_vm` resolves it: `spec.folder` under the datacenter's `vmFolder`,
else `vmFolder` itself). It is one property read of `name` and
`config.extraConfig` over that folder, **not** recursive, because
`DuplicateName` is per folder and nothing outside the target folder can block
the clone. It happens only on passes without `status.vmRef`, so a
provisioned machine never pays for it.

| Found in the target folder | Action |
| --- | --- |
| No VM of the machine's name | Clone, as today. |
| A VM of that name **with our marker** | A clone from an interrupted pass. **Destroy it and clone afresh**, the same outcome as bug-179's rollback. |
| A VM of that name **without our marker** | Not ours. Never touched. `Ready=False`, reason `VMNameConflict`, naming the moref; requeue with the normal error backoff, never a hot clone loop. |

The interrupted clone is destroyed, not resumed. How far the lost pass got
is unknown, and not every step is safe to repeat (`add_tpm_device` is not
written to tolerate a VM that already has a vTPM). Destroying it and cloning again costs one clone task, and it is
correct whatever state the VM was left in, including fully built and powered
on. The VM was never reported provisioned, so nothing depends on it.

### 4. Deletion with no `status.vmRef` uses the same lookup

ADR-0026's finalizer treats a missing `status.vmRef` as "nothing to destroy".
It now runs the Decision 3 lookup first and destroys a VM **only** if it
carries our marker. A same-named VM without it is left alone and does not
block the finalizer. If the Provider itself is already gone, there is no
vCenter to ask: with nothing recorded the finalizer is removed with a
warning rather than blocking deletion forever (a namespace teardown deletes
the Provider too). The path **with** `status.vmRef` is unchanged:
destroy by moref, as ADR-0026 decided, so VMs cloned before this ADR (which
have a moref but no marker) are still deleted correctly.

### 5. Status stays a cache

`status.vmRef` remains the fast path and is still written only by a
successful pass. The marker is the source of truth for ownership, consulted
only when the cache is empty. Nothing about the success path, the
`already_provisioned` early return or `refresh_power_state` changes.

## Consequences

- Cases 2 and 3 of Context are closed: a provider that stops inside the
  window, or a machine deleted while it is restarting, no longer strands a
  VM or wedges the machine on `DuplicateName`.
- The marker is load-bearing. Removing `banlieue.machine.uid` from a VM in
  vCenter orphans it: banlieue will neither adopt nor delete it, the same
  rule as Proxmox's description line. Renaming or moving the VM is harmless
  once `status.vmRef` is set.
- **No new vCenter privilege.** Writing `extraConfig` at clone time is
  already required for `guestinfo`; reading `config.extraConfig` needs only
  `System.Read`, which every lookup already uses.
- A user with `VirtualMachine.Config.AdvancedConfig` could set someone
  else's UID on a VM of the right name in the right folder, and banlieue would
  destroy it on that machine's next interrupted pass or deletion. That user
  can already destroy VMs directly; the vCenter boundary (threat model TB-4)
  does not move, but the implementing change records this in §6.
- Machines with the same name in two namespaces, placed in one folder,
  collide on the VM name. Today that is an endless `DuplicateName`; after
  this ADR it is a clear `VMNameConflict` on the second machine. Making VM
  names unique across namespaces is a separate decision, not taken here.
- Machines stuck on `DuplicateName` today hold an **unmarked** clone, so
  after upgrade they report `VMNameConflict`. The operator deletes that VM
  in vCenter once; the troubleshooting guide documents it.
- `FakeClient` must model what this relies on, or the tests prove nothing
  (`rules/testing.md`, rule 1): VMs per folder, `extraConfig` on each,
  `DuplicateName` per folder (already modelled since bug-179), and a foreign
  same-named VM. Then one live check against a real vCenter: that a
  non-`guestinfo` `extraConfig` key survives the clone and is not readable
  through `vmware-rpctool` from the guest.
- ADD order for the implementation: CALM (the provider-to-vCenter
  relationship's description gains the marker and the lookup; no new node),
  then TDD, then docs, then a full threat-model pass.

## Alternatives considered

- **Resume the interrupted clone instead of destroying it.** Saves one
  clone, but every post-clone step would have to become idempotent and
  inspect what is already there (an existing vTPM, a disk already grown, a
  guest already booted), and a bug in any one of those inspections leaves a
  half-configured VM reported as provisioned. Rejected: correctness over one
  clone task.
- **Record `status.vmRef` immediately after the clone, before the remaining
  steps.** Narrows the window but does not close it: the crash can still
  land between the clone task and that first status write. It also collides
  with the `already_provisioned` early return, which treats any recorded
  `vmRef` as finished. The marker is needed either way.
- **Use an ADR-0089 custom attribute as the marker.** Custom attributes are
  set by a separate call after the clone, so an unmarked window remains.
  They are also editable by anyone with `Global.SetCustomField` across the
  whole vCenter, and need a field definition before first use. They stay
  what ADR-0089 made them: information for humans.
- **Put the UID in `guestinfo.banlieue.machine-uid`.** Guest-writable, so a
  compromised guest could rewrite or erase it. Rejected.
- **Set the VM's `instanceUuid` to the machine UID.** vCenter treats
  `instanceUuid` as its own identity, may change it on a conflict, and gives
  it meaning across vCenter features (linked mode, replication). Overloading
  it would be clever and fragile.
- **Name lookup without a marker (what ADR-0026 rejected).** Still rejected,
  for ADR-0026's reasons.

## Amendment 2026-11-04: decide from a live read, and recover a lost `vmRef` by the marker

### What happened

Rolling `openbao01`..`03` onto a new image left each `VirtualMachine`
`Ready=False` with `InfrastructureReady=False / BackendMissing`, while the
guests were up and serving. The provider log for one machine, 2026-11-04 UTC:

| Time | Pass | Event |
| --- | --- | --- |
| 23:49:13.054 | 1 | Finishes the clone (`vm-25779`), vTPM and power-on; logs "provisioned" and writes `status` |
| 23:49:13.067 | 2 | Starts 13 ms later, from a cache snapshot that predates that write: no `vmRef`, not provisioned |
| 23:49:13.232 | 2 | `reclaim_or_refuse` finds a VM of the machine's name carrying its marker, takes it for "a clone from an interrupted pass", and **destroys the healthy VM** |
| 23:49:31.642 | 2 | Clones again (`vm-25780`) and writes `status.vmRef = vm-25780` |
| 23:49:31.656 | 3 | Starts 14 ms later, from a snapshot that has pass 1's status but not pass 2's: `vmRef = vm-25779` |
| 23:49:31.76 | 3 | `power_state(vm-25779)` is `ManagedObjectNotFound`; the failure report **re-applies the whole stale status with `force`**, putting `vmRef = vm-25779` back over pass 2's |

The machine then reports `BackendMissing` for a VM the provider itself
destroyed, and nothing repairs it: the live VM (`vm-25780`) is not recorded
anywhere. It happened four times in four OpenBao creations. The three spire
servers built on the same provider minutes earlier took the same 13 ms gap
and came out fine, because their cache had caught up: the outcome is a race
on watch latency, not a property of the image, the class or the guest.

### Cause

Decision 3 reads "`status.vmRef` is unset" as "no pass has recorded a VM".
That holds only if the object the pass was handed is current. The controller
hands every pass the cached object, and a pass that starts straight after the
previous one (a queued event for the same object runs the instant the
previous pass returns) can be handed one that does not yet contain that
pass's own write. From that snapshot, "no `vmRef`" and "an interrupted pass
left this clone" cannot be told apart, and the provider destroys a VM on the
strength of a read it has no reason to trust. Decision 3 anticipated a lost
*moref*; it did not anticipate a lost *write that had in fact happened*.

The same snapshot is the base of every full-status write (`patch_status_failed`
and the observation patch re-apply the entire current status, by design: a
narrow SSA apply from the same field manager retracts the omitted fields).
A stale base therefore regresses `vmRef` and `provisioned`, which is how pass
3 undid pass 2.

### Decisions

**6. A pass reads the machine live and decides from that.** After the
`serves` check, `reconcile` reads the `VSphereMachine` straight from the API
server (an uncached `GET`, not the cache) and uses *that* object for the rest
of the pass: the deletion branch, `already_provisioned`, the `vmRef` handed to
`ensure_vm`, and the base of every status write. The cached object only
decides *when* to run. A live object with a different `metadata.uid`, or that
is gone, ends the pass without acting (the replacement has its own event).

This closes both halves at the source: Decision 3's lookup now runs only when
the *API server* says there is no `vmRef`, so a destroy is never decided from
a stale cache; and no status write is built from a base older than the pass.
The cost is one `GET` per pass. It is not a new race, because the provider's
controller runs one pass per object at a time and nothing else writes
`status` (the controller owns `spec` and metadata, the provider owns
`status`).

**7. A `vmRef` whose VM is gone is recovered by the marker.** When a
provisioned machine's recorded VM answers `ManagedObjectNotFound`, the
provider, before reporting `BackendMissing`, looks in the machine's target
folder (Decision 3's lookup) for VMs of the machine's name carrying the
machine's UID:

| Marked VMs found | Action |
| --- | --- |
| Exactly one | **Adopt it**: write its moref as `status.vmRef`, and let the next pass observe it. |
| None | Report `BackendMissing`, as today: the VM is really gone. |
| More than one | Report `BackendMissing`, naming the morefs. Never guess. |
| The lookup itself fails | Report `BackendMissing`; a vCenter fault is not a reason to adopt. |

ADR-0034 left `provisioned=true` with a bad reference as detect-and-report
only, because recovering by *name* "risks adopting the wrong VM". That
objection is what Decisions 1 and 2 answer: ownership is the exact marker, not
the name. This is Decision 5's rule (status is a cache; the marker is the
source of truth) applied to a cache that is wrong rather than empty. It
repairs machines already in this state without recreating them.

### Consequences

- The regression is covered at the decision level, not by a helper in
  isolation: a test provisions a machine, then runs a second pass whose
  *cache* is the pre-write snapshot while the *live* object carries the
  `vmRef`, and asserts nothing is destroyed and no second clone happens. A
  companion test keeps the interrupted-pass behaviour of Decision 3 when the
  live object genuinely has no `vmRef`.
- One extra `GET` of the `VSphereMachine` per pass, for every machine, every
  pass, including the 5-minute settled requeue. Negligible beside the vCenter
  calls each pass already makes.
- Adoption can only choose a VM that carries this machine's UID in a
  non-`guestinfo` key, in the machine's own folder, under its own name. The
  residual risk recorded in Consequences above (a user with
  `VirtualMachine.Config.AdvancedConfig` writing someone's UID onto a
  same-named VM) is unchanged, and now also means such a VM can be adopted
  for a machine whose recorded VM is gone.
- Not changed: the destroy-and-reclone of Decision 3 for a genuinely
  interrupted pass; the marker; deletion (Decision 4).
- Not checked: whether the libvirt, Proxmox and Cloud Hypervisor providers
  decide a destructive step from the cached object. Each has its own
  create path and ownership marker (ADR-0075 for Proxmox); they need the same
  question asked, in a follow-up.
- A live check is owed: with the fixed provider, roll one node and confirm a
  single `CloneVM_Task` per machine and `status.vmRef` equal to the VM in
  vCenter (`make` target to be named when it is written).
- ADD order: this amendment, then CALM (the provider-to-vCenter relationship
  gains the live read and the adoption), then tests first, then the change,
  then docs and the threat-model pass.

### Alternatives considered

- **Write a `resourceVersion` precondition into the status apply.** Would
  make a stale write fail instead of land, but whether server-side apply
  enforces `metadata.resourceVersion` in the applied configuration is not
  stated in the documentation this was checked against, and it does nothing
  for the destructive *read* in Decision 3. Rejected as the control; a
  precondition stays available as a second layer once it has been verified
  against a real API server.
- **Record `status.vmRef` right after the clone.** Already rejected above, and
  still: it narrows the window but the destroy decision would still rest on a
  snapshot, and it collides with the `already_provisioned` early return.
- **Wait for the cache to catch up** (sleep, or compare a resourceVersion the
  pass wrote against the one it reads). Polling for a cache is the pattern
  the project forbids, and it makes correctness depend on a latency.
- **Make the destroy in Decision 3 conditional on the VM's age or power
  state.** A VM provisioned 13 ms ago is as young and as powered-on as one
  from an interrupted pass; neither is evidence.
- **Serialise passes across the status write** (hold the next pass until the
  watch shows the write). Same dependence on a latency, and a lock held across
  a watch event.
