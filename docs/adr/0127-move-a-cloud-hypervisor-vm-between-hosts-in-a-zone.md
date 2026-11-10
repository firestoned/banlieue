<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0127: Move a running Cloud Hypervisor VM between hosts in one zone

- **Status:** Proposed
- **Date:** 2026-11-10
- **Deciders:** Erick Bourgeois
- **Related:** Phase A of [ADR-0036](0036-live-migration-phased-approach.md) for the Cloud
  Hypervisor class (execution plan: roadmap 14). Amends
  [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md) (a host provider opens a
  listening socket, for one move at a time) and [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md)
  (vTPM state is moved, never duplicated). Uses [ADR-0124](0124-unschedulable-providers.md)'s
  cordon and [ADR-0045](0045-vtpm-endorsement-key-certificate.md)'s EK certificate. faubourg ADR-0017 is the
  requesting side. Depends on [ADR-0125](0125-a-placed-vm-keeps-a-placement-that-still-fits.md)
  (a placed VM keeps its placement), without which the next scheduling pass would undo a move.
  ADR-0116 to ADR-0122 are reserved by roadmap 20.

## Context

An operator taking a Cloud Hypervisor host out for maintenance, or rebalancing a zone, wants
the VMs on it to keep running somewhere else: same guest, same memory, same disk, same
addresses. vSphere calls this vMotion. banlieue cannot do it on any backend:
`MigrationAction::Recreate` (destroy and rebuild) is the only response to a placement change
(`crates/banlieue-controller/src/reconciler/migration.rs`), and ADR-0124's cordon deliberately
moves nothing.

**What Cloud Hypervisor upstream offers** (checked 2026-11-10 against `main` at `0f47e60b`
and v54.0):

- `vm.send-migration` / `vm.receive-migration` move a running VM between two VMMs over TCP
  with mutual TLS or over a UNIX socket, precopy or postcopy, with cancellation. **The disk
  must already be at the same path on both hosts.** Block storage migration is an open
  feature request (cloud-hypervisor#8006), worked on in a downstream fork first.
- **The vTPM does not migrate.** `devices/src/tpm.rs` implements no snapshot or migration
  support and `add_tpm_device` registers nothing migratable. A migration does not refuse a VM
  with a vTPM either: the swtpm process and its state simply stay behind.
- **A paused VM migrates to a paused VM** (the `CompletePaused` handover,
  `vmm/src/lib.rs:1108`). A *running* source always resumes on the destination; a request to
  let the destination stay paused (cloud-hypervisor#8792) was closed unimplemented.
- **A handover can split-brain** (cloud-hypervisor#9208, open): if the destination's final
  acknowledgement is lost, both VMMs run the VM and write the disk.
- Migration compatibility is guaranteed only from v54 (n-2). banlieue's hosts pin v53.0
  (`crates/banlieue-host/src/pins.rs`).

**What swtpm offers:** its control channel exports and imports the `permanent` and `volatile`
state blobs (what QEMU uses to migrate a vTPM), `--migration incoming` defers its storage lock
until blobs arrive, and `--migration-key` encrypts blobs in transit. `swtpm_localca --ek`
certifies an existing EK public key. But **the control channel serves one client at a time**
(`src/swtpm/mainloop.c` listens for a control client only while it has none), and the VMM
holds that connection for the guest's whole life. So only the VMM can move a vTPM's state:
QEMU does exactly that, and Cloud Hypervisor does not yet.

**What banlieue's model gives a move to work with:**

- On Cloud Hypervisor one host is one `Provider` and one failure domain (ADR-0060), with
  host-local storage only (roadmap 03). "Two hosts in one zone" is not represented in banlieue.
  faubourg, which bootstraps these hosts, groups them into `AvailabilityZone`s and labels each
  host's `Provider` with its zone (faubourg ADR-0003).
- A host provider authenticates outbound to the apiserver, opens no listening socket, and may
  read and write only its own `Provider` and the `CloudHypervisorMachine`s naming it
  (ADR-0060). Two host providers share nothing.
- A machine's MAC, tap names, machine directory and EK certificate subject are derived from
  the `CloudHypervisorMachine`'s name and UID
  (`crates/banlieue-provider-cloud-hypervisor/src/plan.rs`). Its guest uid is allocated per
  host. Its TPM state lives in `<state root>/tpm/<guest uid>/`.
- A guest sealed on first boot (ADR-0088, ADR-0112) holds its LUKS passphrase in its vTPM's NV
  storage: the disk is useless without that exact swtpm state.
- XFS with `reflink=1` is the expected Cloud Hypervisor storage (it removes a 7 s firmware
  flush wait on clones, roadmap 18), and the provider already probes and publishes whether a
  storage class reflinks (`raw.storageWithoutReflink`).

**A placed VM stays placed** ([ADR-0125](0125-a-placed-vm-keeps-a-placement-that-still-fits.md)):
the scheduler re-runs for every placed VM on every pass, and before ADR-0125 its spread
(ADR-0098) could re-decide a placed VM onto a lighter host, which `migrationPolicy: Automatic`
answered with `Recreate`. ADR-0125 returns the recorded placement while it still passes every
filter. A move relies on that: once `status.scheduled` names the destination, the next pass
must keep the VM there, not send it back.

## Decision

### Who asks, who moves

1. **faubourg asks, banlieue moves.** Deciding *when* and *where* a host's VMs go (drain for
   maintenance, rebalance a zone) is host management and belongs to faubourg (faubourg
   ADR-0017). Doing the move needs per-machine state that only banlieue's host provider holds
   (guest uid, swtpm state, machine directory, derived identity), so the mechanism is
   banlieue's. faubourg never touches a VM; banlieue never decides to drain a host. A human
   can create the same request faubourg does.

2. **The request is a new namespaced CRD, `VirtualMachineMove`** (`banlieue.io/v1alpha1`),
   in the VM's namespace:

   - `spec.vmRef.name`: the VM to move. Required, immutable.
   - `spec.target` (optional): `providerName`, or a `providerSelector`. When absent, banlieue
     picks a destination with the scheduler, restricted to the VM's zone, excluding its current
     Provider and cordoned ones.
   - `status.phase`: `Pending`, `Preparing`, `Copying`, `HandingOver`, `Succeeded`, `Failed`,
     `Rejected`; `status.source` and `status.destination` (Provider names); `conditions` with
     reasons; byte counters for the copy; `status.downtimeMilliseconds` once done.

   One move per VM at a time; a second request for the same VM is `Rejected`
   (`MoveInProgress`). Deleting a `VirtualMachineMove` before `HandingOver` cancels it; after
   that it runs to `Succeeded` or `Failed` (deleting it then only removes the record). It is
   below the API ceiling (ADR-0097): it acts on one VM, as a claim does.

   A CRD rather than an annotation on the VM: it can be RBAC-granted on its own (faubourg gets
   `create` on moves, not `update` on VMs), it carries progress and a result, and several of
   them make a drain auditable.

3. **A move is admitted only between Providers that can actually do it.** The controller
   checks, and rejects with a named reason (`status.phase: Rejected`) otherwise:

   - same provider class, `cloud-hypervisor` only in this ADR;
   - **same zone**: both Providers carry the label **`banlieue.io/zone`** with the same,
     non-empty value. banlieue defines the key (as it defined `banlieue.io/unschedulable`) and
     faubourg sets it from the host's zone. A zone is the set of hosts the operator declares
     share the L2 segments behind the VM's network classes, so the guest's MAC and addresses
     stay valid after it moves. banlieue cannot verify that; the label is the operator's
     statement of it.
   - both Providers advertise the new feature `move`;
   - identical `raw.vmm` (VMM version string) and identical `raw.cpuModel` on both hosts. Cloud
     Hypervisor exposes the host CPU to the guest; moving between different CPU models is not
     safe without a CPU baseline, which is out of scope;
   - the destination is `Ready`, `Alive`, not cordoned, advertises the VM's storage class,
     network classes and `vtpm` (if `tpmEnabled`), passes the capacity fit (ADR-0107), and
     satisfies the VM's own `placement` (a move never takes a VM somewhere its spec forbids);
   - the source storage class reflinks (Decision 7);
   - the VM's `migrationPolicy` is not `Never` (`Never` means "do not move this VM", which
     covers a move too);
   - the VM is running and not mid-install (`Deferred` installs move only once `GuestReady`);
   - the VM has no vTPM (`tpmEnabled: false`) until Stage 2 (Decision 8, Decision 13); a
     `tpmEnabled` VM, which includes every sealed one, is rejected with `VTPMNotMovable`.

### Control plane: CRDs only

4. **The controller orchestrates both machines; each host provider reads only its own.** The
   destination machine is a **new `CloudHypervisorMachine`** (a host provider cannot own
   another host's machine, and `providerID` embeds the Provider), named `<vm>-<short move
   id>`, created by the controller with `spec.moveIn`. The source machine gets `spec.moveOut`.
   The controller copies each side's published values to the other:

   | Step | Source machine | Destination machine |
   | --- | --- | --- |
   | 1 | controller sets `spec.moveOut` (move id) | controller creates it with `spec.moveIn` (move id) |
   | 2 | provider generates a per-move key, publishes `status.moveOut.certificate` | provider allocates a guest uid, prepares directories, starts an empty VMM (and, from Stage 2, a swtpm with `--migration incoming`), generates a per-move key, publishes `status.moveIn.{certificate, endpoint}` |
   | 3 | controller copies the destination's certificate and endpoint into `spec.moveOut.peer` | controller copies the source's certificate into `spec.moveIn.peer` |
   | 4 | provider copies the disk while the guest runs (Decision 7), then reports `Precopied` | receives |
   | 5 | controller sets `spec.moveOut.cutover`; provider pauses the guest, sends the disk delta and the VMM state (which carries the vTPM state from Stage 2), reports `Sent` and stops its VMM | receives, restores, stays **paused**, reports `Received` |
   | 6 | | controller sets `spec.moveIn.commit` only once the source reports `Sent` and its VMM has exited; provider resumes the guest and reports `Running` |
   | 7 | controller deletes the source machine; its finalizer removes the old disk, snapshot and TPM state | |

   No RBAC changes for host providers beyond the new fields on their own machines.
   The controller gains `create` on `cloudhypervisormachines` (it creates machines today) and
   the new CRD's verbs.

5. **The guest runs on exactly one host, always.** The invariant is enforced in three places:
   the handover is paused to paused, so Cloud Hypervisor itself never resumes either side; the
   destination resumes only on the controller's `commit`; the source resumes only on the
   controller's `abort`; and the controller never sets both. This is why upstream's
   cloud-hypervisor#9208 does not apply to this design: no VMM in it ever resumes on its own.

6. **The identity moves with the guest.** The destination machine carries
   `spec.identity` (the source machine's name and UID, copied by the controller, immutable),
   and the provider derives the MAC, tap names, machine directory and EK subject from it
   instead of from its own metadata. Without it the guest's MAC would change on every move and
   its EK certificate would no longer match ADR-0045's `<name>:<uid>` check. A machine with no
   `spec.identity` derives from itself, as today. The guest uid is the destination host's own
   (allocated there); nothing outside the host depends on it.

### Data plane: the host provider copies

7. **The source host provider copies the disk, mostly while the guest keeps running**, using
   reflink as dirty tracking:

   1. Pause the guest, `fsync` the disk, `FICLONE` it to `os.raw.move-<id>` (a constant-time
      reflink on XFS), resume. The pause lasts milliseconds.
   2. Stream the clone's allocated extents (`SEEK_DATA`/`SEEK_HOLE`) to the destination while
      the guest runs. The seed and any install media are sent in this phase too.
   3. At cutover, pause the guest and `fsync` the disk (upstream does not yet flush on pause,
      cloud-hypervisor PR #9213). Every block the guest wrote since step 1 is now an extent of
      `os.raw` that is **no longer shared** with the clone (`FIEMAP_EXTENT_SHARED` clear), and
      every range it discarded is a hole in `os.raw` that is data in the clone. Send those
      extents and punch those holes. That delta is what the pause pays for.

   A source storage class that does not reflink is rejected in this ADR
   (`StorageWithoutReflink`): a full copy inside the pause is a cold migration with the
   downtime to match, and is better done as a planned recreate.

8. **A vTPM moves inside the VMM's own migration stream, once Cloud Hypervisor carries it.**
   The provider cannot fetch or load swtpm state itself while a VMM is connected (Context), and
   putting a relay between every VMM and its swtpm (Alternatives) costs more than the gap it
   works around. So banlieue **contributes vTPM state migration to Cloud Hypervisor upstream**,
   the way QEMU does it: the source VMM's TPM device fetches the `permanent` and `volatile`
   blobs over its existing control connection while the guest is paused, sends them in the
   migration stream, and the destination VMM's device loads them into a swtpm started with
   `--migration incoming` before the guest resumes. Until that is released and pinned, a
   `tpmEnabled` VM is not movable (Decision 3). The source swtpm is not stopped before commit,
   so an abort resumes the guest with its vTPM untouched; at commit the source machine's
   finalizer deletes the source copy. The state exists on two hosts for the length of the
   handover and runs on one.

   **The EK certificate is re-issued, not moved.** The EK key pair is inside the moved state,
   so the guest's endorsement key does not change. Its certificate chains to the *source*
   host's `swtpm_localca`, which ADR-0065 makes the only trust anchor for that host's guests.
   The controller copies the source machine's published certificate into `spec.moveIn`, and
   the destination provider certifies the same public key, taken from that certificate, with
   its own CA (`swtpm_localca --ek`, same `--vmid`, so the same subject). It needs no access to
   the TPM for that. It writes the certificate to its EK directory and publishes it in
   `status.tpmEndorsementCertificates`, from where it is mirrored to the VM and its claim as
   today. A verifier sees a new certificate for the same key, under the destination host's CA,
   which it needs anyway to verify guests born there.

9. **The VMM state rides the providers' channel, not Cloud Hypervisor's TCP listener.** Each
   VMM migrates over a UNIX socket to its own host provider (`send-migration
   destination_url=unix:…`, `receive-migration receiver_url=unix:…`), which carries it over the
   one connection between the two hosts. That keeps one listener, one TLS stack and one key
   exchange policy per move, and keeps Cloud Hypervisor's own `tls_dir` material (files with
   private keys) out of the picture.

10. **The connection between the hosts.** The destination provider listens on the address in
    its host config's new `[move] listen` (a zone-network address; no default, a host without
    it does not advertise `move`), for one move at a time, and only between steps 2 and 6:

    - TLS 1.3 with **`X25519MLKEM768` as the only key exchange**: guest memory, disk and vTPM
      state cross this link, so their confidentiality must not rest on a key exchange that a
      recorded session can later break. rustls with `aws-lc-rs`, as elsewhere in banlieue.
    - **Mutual authentication against a pinned certificate**, not a CA: each side accepts only
      the exact certificate the other published on its machine and the controller copied
      across. The per-move keys are generated on the host, never leave it, and are discarded
      when the move ends. Their signatures (Ed25519) only need to hold for the move's
      lifetime; confidentiality is the post-quantum requirement, authentication is not
      stored.
    - The listener accepts one connection, closes at the end of the move or after an idle
      timeout, and is never a control-plane endpoint: the providers send each other bytes, and
      every decision still arrives through their own machine's spec.

    Non-negotiable 1 (no RPC between the controller and the providers) is untouched. ADR-0060's
    "opens no listening socket" is amended to "opens no control-plane listener; a data listener
    exists only for the duration of one move, for one pinned peer".

### Status and failure

11. **The VM shows the move.** `Migrating=True` (the condition ADR-0036 Decision 3 reserved)
    from `Preparing` to the end; `Ready=False`, reason `Moving`, only while the guest is paused
    at cutover; `status.scheduled` switches to the destination at commit and only then. The
    `VirtualMachineMove` records `downtimeMilliseconds` (cutover pause to resume).

12. **Failure before commit leaves the VM where it was.** The controller sets
    `spec.moveOut.abort`: the source resumes its guest (its disk and vTPM are untouched), the
    destination machine is deleted with everything it received, and the move is `Failed` with
    a reason. If the *destination* host disappears after the source reported `Sent` but
    **before the controller wrote `commit`**, the source still has its disk, its vTPM and a
    stopped VMM, and the destination, if it is merely unreachable, stays paused forever because
    no `commit` will reach it: the controller aborts and the source provider boots the guest
    again from its disk. The guest restarts (its memory is gone) but loses no data. **Once
    `commit` is written the source copy is never booted again**, whatever happens to the
    destination: the destination may already be running the guest and writing its own disk,
    so the source copy is stale and booting it would be the split brain Decision 5 forbids.
    The move then waits on the destination, and the VM is unavailable while that host is. If
    the *source* host disappears mid-move, the move fails and the VM is exactly as unavailable
    as it would have been without one. **Nothing in this design
    restarts a VM on another host after a host failure**: that is fencing and HA, and stays
    out (faubourg ADR-0004 Decision 4).

### Staging

13. **Built in stages; each is a separate implementation milestone of this ADR.** Stages 2
    and 3 are independent of each other.

    - **Stage 1 (buildable now): VMs without a vTPM.** Decisions 1 to 12. Downtime is the disk
      delta and the *whole* guest memory, sent while paused (a running source always resumes
      on the destination upstream, so memory cannot be precopied live). Roughly RAM size over
      link speed: about 4 s for 4 GiB at 10 GbE, about 35 s at 1 GbE.
    - **Stage 2: VMs with a vTPM, sealed ones included.** Needs Decision 8's upstream change
      in a Cloud Hypervisor release banlieue pins.
    - **Stage 3: live memory.** Contribute the destination-stays-paused option upstream (the
      wire protocol already has `CompletePaused`; cloud-hypervisor#8792). Memory is then
      precopied while the guest runs, and the pause carries only the last dirty pages and the
      disk delta.
    - **Stage 4: no banlieue copy.** Once upstream migrates block storage
      (cloud-hypervisor#8006), the provider stops copying the disk and only relays the stream.

    The CRD shape does not change between stages. Every stage needs identical VMM versions on
    both hosts (Decision 3). Moving the hosts onto an upstream release (which Stages 2 to 4
    need) first needs the vTPM response fix (cloud-hypervisor#9053) upstream.

## Consequences

- **A move is a vMotion-shaped operation on local storage**: no shared datastore, no
  migration network beyond a zone address per host, no fencing. The price is a disk copy per
  move and, until Stage 3, a pause proportional to RAM. Until Stage 2, sealed guests (most
  Cloud Hypervisor pool members) do not move at all.
- **The zone becomes a trust unit.** A guest's memory, disk and (from Stage 2) sealed vTPM state can now
  reach any host in its zone, not only the one it was born on. ADR-0065's "the TPM state is
  never copied" becomes "moved, and never running twice". Host root could always read its own
  guests; now a compromised host in a zone can receive another host's guests if the
  controller sends them there. The threat model gains a host-to-host boundary (TB-10) and an
  accepted risk for this.
- **Verifiers of EK certificates must trust every host CA in the zone**, not only the birth
  host's, since a moved guest's certificate is re-issued by its new host.
- **New host configuration:** `[move] listen` and a firewall opening between zone hosts on that
  port. Without it a host simply does not advertise `move`.
- **New surface to build and test:** the CRD and its controller, `spec.identity`,
  `moveIn`/`moveOut` on `CloudHypervisorMachine`, the provider's copy and VMM relay code, the
  upstream Cloud Hypervisor change (Stage 2), a live tier (two real hosts: guest moved with the
  same MAC and address, abort at every phase leaves one running guest; from Stage 2 a sealed
  guest that still unlocks and an EK certificate re-issued under the destination CA) and an e2e
  through faubourg's drain.
- **Questions for the implementation, not for this decision:** whether the guest needs a
  network announcement beyond Cloud Hypervisor's RARP for every topology; how a drain orders
  pool members (an unclaimed member may be cheaper to replace than to move, which is the pool's
  call); a per-host limit on concurrent outgoing moves (a constant in the provider, sized by
  measurement).

### Alternatives considered

- **Shared storage per zone.** Removes the disk copy, adds a storage backend, a migration
  network and fencing. faubourg ADR-0004 rules it out for v1, and banlieue's AZ model is local
  storage (roadmap 03).
- **Snapshot and restore instead of migration.** `vm.snapshot` writes guest memory, LUKS keys
  included, to a file on both hosts, and pauses for the whole memory twice. Rejected for
  putting guest RAM at rest.
- **faubourg as the zone's Provider (vCenter's model).** banlieue would see one Provider per
  zone and faubourg would move VMs inside it. It would make faubourg a second owner of every
  VM's lifecycle and undo one-host-one-Provider for this class (scheduling spread, per-host
  RBAC, ADR-0060). Not ruled out for later; not needed for moves.
- **A relay between each VMM and its swtpm**, so the provider could fetch and load state blobs
  itself (Stage 2 without an upstream change). A relay inside the provider would cut every
  guest's vTPM connection whenever the provider restarts, which every upgrade does; a separate
  per-guest relay unit would need file-descriptor passing for swtpm's data channel and a third
  process in every guest's TPM path. Rejected for the risk it adds to every vTPM guest to move
  a few of them sooner.
- **Copy with `rsync` over SSH between hosts.** Needs host-to-host SSH trust (a long-lived
  credential on every host), no reflink delta, and a second channel for the VMM state.
- **Wait for upstream.** cloud-hypervisor#8006 has no upstream date, and the vTPM gap has no
  issue. Stage 4 adopts upstream block migration when it lands; Stage 1 does not wait for
  anything, and Stages 2 and 3 are banlieue's own upstream contributions.
