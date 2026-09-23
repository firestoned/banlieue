# 0043 — `GuestReady`: the installed guest announces itself

- **Status:** Accepted
- **Date:** 2026-09-20
- **Proposed:** 2026-09-20
- **Amended:** 2026-09-20 (Decision 8 — the first formulation would have
  polled every `Immediate` VM forever; see the decision for the correction)
- **Deciders:** Erick Bourgeois
- **Notes:** Implemented for libvirt, verified live. The read path **is now
  verified against a real `qemu-guest-agent`** (2026-09-21): `live_guest.rs`
  drives open/read/close and both halves of the tri-state against a booted
  Debian 13 guest on a real host. It no longer waits for an image that ships
  the agent — it installs one at boot through the NoCloud seed (ADR-0054),
  which is what unblocked it, since neither the Kairos build nor Debian's
  `genericcloud` carries it. That first green run cost two real bugs, both
  fixed: an overlay declared `raw` over a `.qcow2` backing image so no guest
  ever booted, and `probe_guest` reported `AgentUnreachable` for a healthy
  guest whose marker simply did not exist yet — inverting Decision 8's
  requeue cadence for the whole install window. **Still open (libvirt):** a
  Kairos image carrying the phase layer, to prove the marker is written at
  the right moment (that its `/run/cos/active_mode` guard keeps it out of
  the live installer).

  **vSphere transport implemented and verified live, 2026-09-22**, per
  Decision 6: `VSphereClient::guest_info` reads a `guestinfo.*` key back out
  of `config.extraConfig`; `VSphereMachineStatus.guestInstalled` mirrors
  libvirt's own sticky field; `GuestReady` is published from
  `refresh_power_state` only while the VM is running (a stopped VM's guest
  cannot be evaluated, mirroring Decision 10's absent-not-false rule with
  `GuestProbe::NotEvaluated` in place of libvirt's `AgentUnreachable`).

  The first live run against a real vCenter cost three real bugs, all
  fixed and now regression-tested in `client/vim_tests.rs` and
  `tests/live_vcenter.rs`:

  1. **Every `live_vcenter.rs` test panicked on teardown**
     ("can call blocking only when running on the multi-threaded runtime")
     — plain `#[tokio::test]` defaults to a single-threaded runtime, and
     `vim_rs`'s client teardown needs the multi-threaded one. Fixed with
     `#[tokio::test(flavor = "multi_thread")]` on all three.
  2. **`guest_info`'s first cut broke every subsequent reconcile of an
     already-provisioned `VSphereMachine`**, not just guest probing:
     fetching the whole `config` property and decoding it as the typed
     `VirtualMachineConfigInfo` failed ("JSON property decode failed").
     Root cause: `vim_rs::types::vim_any::VimAny`'s `Deserialize` only
     implements the polymorphic `{"_typeName": ..., ...}` map shape, and
     errors on a bare-string `OptionValue.value` — which a real vCenter's
     JSON API sends for some `extraConfig` entries. One non-decoding entry
     sinks the whole struct, since miniserde has no per-field fallback.
     (This JSON transport also has no PropertyCollector-style dotted
     property paths — `fetch_property_raw(.., "config.extraConfig")`
     faults with `InvalidType` — so narrowing the *fetch* wasn't an
     option either.) Fixed by parsing the raw JSON bytes by hand
     (`extra_config_value_from_raw_json`), bypassing `vim_rs`'s typed
     decode for this property entirely.
  3. **A key known to be set still read back `None`** after that fix:
     the real wrapper is `{"_typeName": "string", "_value": "..."}` —
     underscore-prefixed `_value`, not the bare `value` first assumed by
     analogy with `OptionValue`'s own outer field name. Confirmed by
     dumping the raw JSON for `guestinfo.network.hostname` (set
     unconditionally by `build_guestinfo` on every vSphere clone) and
     fixed by checking `_value` first.

  None of the three are architectural — all fixed within this ADR's
  existing decision, no amendment needed.

  **End-to-end confirmed live, 2026-09-23**, against a real `Deferred`,
  `tpmEnabled` VM (`debian-tpm-dev-v0.4.0`, rebuilt with the vSphere
  `vmware-rpctool info-set` stage from
  `examples/16-cloud-config-guest-phase.yaml` uncommented): the guest itself
  ran `vmware-rpctool info-set guestinfo.banlieue.phase installed` on boot,
  `guest_info` read it back, and `VSphereMachine.status` showed
  `guestInstalled: true` with `GuestReady=True reason=GuestAnnounced` —
  correctly mirrored onto the parent `VirtualMachine`. This is the closing
  proof Decision 6 needed: not just that the channel decodes, but that a
  real guest driving it produces the right condition.

  That run surfaced one more bug, entirely a deployment gap rather than a
  code or decision defect: the live cluster's `VSphereMachine` CRD had
  never been regenerated after `VSphereMachineStatus.guestInstalled` was
  added, so every status patch 500'd with `.status.guestInstalled: field
  not declared in schema` — the binary was correct and deployed, but the
  CRD wasn't. Fixed by `make crds` + `kubectl apply` of the regenerated
  CRD (one additive optional field, dry-run verified first).

  *Accepted* now records completed, live-verified delivery on both
  backends.
- **Related:** Closes the gap [ADR-0040](0040-deferred-install-for-vtpm-encryption.md)
  Decision 4 recorded and deferred. Makes
  [ADR-0046](0046-virtualmachinepool.md)'s `readiness: GuestReady`
  satisfiable, and is therefore a prerequisite for
  [ADR-0055](0055-agentsandbox.md) being trustworthy against `Deferred`
  images.

## Context

`InfrastructureReady` fires when the backend says the VM exists and is
running — `CloneVM_Task` plus power-on on vSphere, domain defined and
started on libvirt. For an `Immediate` image that is the right answer: the
disk was already installed, so a running VM is a usable VM.

For a `Deferred` image it is the **moment the install starts**. ADR-0040
chose deferred install because a disk sealed to a per-VM vTPM can only be
produced by letting the guest install itself on first boot, and recorded
this gap rather than solving it. A pool that trusts `InfrastructureReady`
against such an image hands out machines that are still copying files.

Today `VirtualMachinePool.spec.readiness` accepts `GuestReady` and nothing
publishes it, so such a pool sits at zero and says
`Warm=False reason=ReadinessSignalAbsent`. That is honest, and it is not a
feature — it is this ADR's absence, reported.

### Why the cheap signals are all wrong

Every convenient "is the guest up" signal answers a different question:

| Signal | What it actually proves |
| --- | --- |
| VMware Tools heartbeat | *a* guest is running — the **live installer runs Tools too** |
| `qemu-guest-agent` ping | same: the installer environment can run the agent |
| DHCP lease appears | something asked for an address; the installer does |
| SSH port open | something is listening |

All four are satisfied by the installer environment that is *in the middle
of overwriting the disk*. A pool built on any of them hands out VMs at
exactly the wrong moment, and does so while reporting success.

The distinguishing fact is not liveness. It is **which disk the guest
booted from**.

## Decision

1. **The installed system announces itself; the host reads the
   announcement out of band.** Only a system booted from the installed disk
   writes the marker, so reading it answers "which disk booted", not "is
   something alive".

2. **The announcement is guarded on immucore's boot-mode sentinels.** Kairos
   runs its `boot` stages in the live installer and in recovery too, so the
   cloud-config stage is conditioned on `/run/cos/active_mode` or
   `/run/cos/passive_mode` — the only two that mean "booted from the
   installed disk".

   Without the guard the installer would announce "installed" while
   installing, which is worse than no signal: it would look like the feature
   working.

3. **It is re-asserted on every boot, not written once.** The value lives in
   runtime state (guestinfo, `/run`) which survives a guest reboot but not a
   power cycle. A once-only write would silently stop being true.

4. **New shared condition type `GuestReady`. `Ready` does not change.**
   Making `Ready` depend on the guest signal would redefine `Ready` for every
   existing `Immediate`-mode VM whose image has no phase stage — they would
   all regress to not-ready, for a signal their images were never built to
   send. `GuestReady` is additive and opt-in via the pool's `readiness`.

5. **`status.guestInstalled: Option<bool>` on the infra machine, sticky once
   true.** The runtime marker does not survive a power cycle, and a VM that
   was powered off has not become uninstalled. Without stickiness a warm
   member would flap out of the pool every time it was stopped.

6. **One condition, per-backend transport.** The condition and the status
   field are backend-neutral; how the marker crosses the boundary is not:
   - **vSphere** — `guestinfo.banlieue.phase=installed` via `vmware-rpctool`,
     read from `config.extraConfig`. A host-readable channel already exists.
   - **libvirt** — a marker file (`/run/banlieue/phase`) read through
     `qemu-guest-agent`. libvirt has no guestinfo equivalent, so the file
     *is* the channel.

   Nothing above the provider learns which was used, exactly as with
   user-data delivery (ADR-0054).

   **Both transports are now implemented and confirmed against a real
   backend.** libvirt's read path is verified against a real
   `qemu-guest-agent`; the vSphere read (`VSphereClient::guest_info` over
   `config.extraConfig`) is verified against a real vCenter (see the Notes
   above) — three real bugs found and fixed on the first live run, none of
   them in this ADR's decision itself.

7. **libvirt support requires speaking a second RPC program.**
   `virDomainQemuAgentCommand` lives in libvirt's qemu-specific program
   (`0x2000_8087`), not the remote program (`0x2000_8086`) `banlieue-libvirt`
   has spoken so far. The framing is program-agnostic, so this is a new
   program constant and one procedure rather than a new transport.

   Reading the marker uses `guest-file-open` / `guest-file-read` /
   `guest-file-close` rather than `guest-exec`: three deterministic calls
   instead of a fire-and-poll with a pid to chase, and it needs no
   executable in the guest.

8. **Requeue at the default interval while the answer is expected to change
   — which is not the same as "while `guestInstalled != true`".**

   The obvious rule is wrong. `guestInstalled` is never true for an
   `Immediate` image, which has no phase stage and usually no guest agent,
   so "poll fast until installed" would poll every 30s forever, per VM, for
   a signal that is never coming.

   So the probe returns three states, not two, and the extra one carries
   the distinction: an **unreachable agent** means nothing will ever
   announce (back off), while an **agent that answers without a marker**
   means something may be installing right now (poll fast). The states cost
   nothing to produce — a libvirt-level error versus an `Ok` reply carrying
   a JSON error already distinguishes them.

   Fast polling matters because the intervals are 30s and 300s: keying off
   addresses instead, as a first cut did, drops a Deferred member to the
   long interval as soon as its *installer* picks up a DHCP lease, and then
   adds up to five minutes between the guest announcing and the pool
   noticing.

9. **This is a liveness signal and must never be read as an integrity
   one.** It says a fresh, unclaimed guest booted from its installed disk.
   It attests nothing: a compromised guest can write the same marker.
   Integrity is ADR-0049's problem (attestation anchors, TPM quote over the
   claim nonce), and conflating the two would make a pool look verified when
   it is merely running.

10. **`GuestReady` is published only when the provider can actually
    evaluate it.** An unreachable guest agent means the image cannot send
    the signal at all — the same position vSphere is in until its transport
    lands — so the condition is left **absent**, not `False`. Once a member
    has announced it is sticky along with `guestInstalled`, so a power cycle
    does not drop it.

    `pool.rs::readiness_signal_absent` decides by condition *type*. A
    blanket `False` makes a pool set to `readiness: GuestReady` report
    `Warm=False reason=Filling` — "wait a bit" — forever, for an image that
    can never announce. That is exactly the failure ADR-0046 Decision 3
    exists to prevent, and the first implementation of *this* ADR
    reintroduced it: the libvirt provider published `False` unconditionally,
    so the diagnostic never fired.

    Caught by running a `GuestReady` pool on a real cluster and reading what
    it said, not in review. Amended the same day.

## Consequences

- `readiness: GuestReady` becomes satisfiable, so pools of `Deferred`,
  TPM-sealed VMs are achievable — the case roadmap 17 exists for.
- **An image without the phase stage never reports `GuestReady`.** That is
  correct and already handled: the pool says
  `Warm=False reason=ReadinessSignalAbsent` rather than sitting silent.
  It does mean adopting `GuestReady` requires rebuilding the image with the
  cloud-config layer, not just editing the pool.
- `banlieue-libvirt` gains a second RPC program. Small, but it ends the
  assumption that one connection speaks one program, which the session and
  framing code must now reflect.
- The guest agent becomes load-bearing on libvirt: a `Deferred` image
  without `qemu-guest-agent` installed cannot satisfy `GuestReady`. That is
  a documented image requirement, not a silent failure — the condition stays
  false and the pool says why.
- **A pool of `Deferred` VMs on vSphere can now use `GuestReady`, fully
  verified end-to-end** against a real vCenter and a real guest: not just
  that `guest_info` reads back a known-set key without erroring, but that
  a real `Deferred`, `tpmEnabled` VM running `vmware-rpctool info-set
  guestinfo.banlieue.phase installed` at boot produces `guestInstalled:
  true` and `GuestReady=True reason=GuestAnnounced`, correctly mirrored
  onto `VirtualMachine`. This is what closes the case roadmap 17 exists
  for on vSphere. libvirt's own moment-of-write proof (a Kairos image
  carrying the phase layer) remains open — that gap is specific to that
  backend's image, not to this ADR's decision.
- `examples/16-cloud-config-guest-phase.yaml`'s vSphere stanza is
  uncommented and confirmed live.
- `VirtualMachine` gains a mirrored `GuestReady` condition, so a consumer
  reads one object rather than reaching into the infra CR.
- An `Immediate` image that *does* ship `qemu-guest-agent` but no phase
  stage polls at the fast interval for as long as it runs: its agent
  answers, so it looks like a guest that might yet announce. Accepted —
  the cost is one agent round trip per 30s per such VM, and the alternative
  is guessing at `installMode`, which the provider cannot see.
