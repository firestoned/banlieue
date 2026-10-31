<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0112: First-boot sealing on Cloud Hypervisor

- **Status:** Accepted
- **Date:** 2026-10-28
- **Proposed:** 2026-10-27
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-10-28 (accepted after a live run; Decision 4 gains the
  deadline cadence the run showed was missing; see *Amendment 2026-10-28*)
- **Related:** Answers [ADR-0088](0088-first-boot-sealing-for-immediate-images.md)
  Open question 2 and amends its Decision 4; extends
  [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md) Decision 5
  (the vsock report); clone mechanics from
  [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md) Decision 4

## Context

ADR-0088 lets a `tpmEnabled` VM boot an `Immediate` image that seals its data
partitions to the clone's own vTPM on first boot. It is Accepted, on by
default, and verified on libvirt (a sealed `GuestReady` pool warmed in
125 s). Cloud Hypervisor, banlieue's reference backend, is the one backend
still excluded: the scheduler refuses to place a `firstBootSealing` +
`tpmEnabled` VM there, pending Open question 2, "can Cloud Hypervisor clone
an `Immediate` image with a vTPM?".

ADR-0088 sets two requirements for a backend:

1. **The clone must seal privately.** Sealing writes a LUKS header and a new
   filesystem into the clone's disk; the image it came from must never be
   written, and each clone must have its own vTPM.
2. **`GuestReady` must be read back from the guest** (Decision 5): a guest's
   report of which listed partitions are LUKS containers, parsed strictly,
   with a deadline, because a clone whose sealing failed halts in its
   initramfs and never reports. libvirt reads `/run/banlieue/sealed` through
   `qemu-guest-agent`; vSphere reads `guestinfo.banlieue.sealed`.

On Cloud Hypervisor:

- **Requirement 1 already holds.** An `Immediate` OS disk is the guest's own
  `os.raw` in its machine directory, a reflink of the cached image (or a
  sparse copy where the filesystem cannot reflink, ADR-0064 Decision 4), not
  an overlay on a shared base. Writing it never touches the cache. Each
  `tpmEnabled` guest gets a freshly manufactured swtpm, and its EK
  certificate is read on the host (ADR-0065). The "shared base" ADR-0088
  worried about does not exist on this backend.
- **Requirement 2 has no channel.** A Cloud Hypervisor guest has no guest
  agent and no guestinfo. Its one way to tell the host anything is the
  ADR-0065 vsock report: the guest connects to host port 1024, which reaches
  a socket in its own run directory where the provider listens, and today
  only an exact `phase=installed` line counts. The images banlieue documents
  send it with `NOTIFY_SOCKET=vsock-stream:2:1024 systemd-notify
  phase=installed`.

## Decision

1. **The vsock report gains one line, `sealed=<partitions>`.** The guest
   sends the partitions it found sealed, comma-separated
   (`sealed=COS_PERSISTENT`), on the same socket and under the same limits
   as `phase=installed` (one connection, at most 16 KiB, a read timeout). The
   provider parses it with `banlieue_provider_sdk::sealing::parse_sealed_report`,
   the parser libvirt and vSphere use: plain partition labels only, each
   once. A guest can only report its own state, because the socket is in its
   own run directory. Anything else on the line, or any other line, is
   ignored. The listener keeps the union of what it has heard, and a newly
   reported partition wakes the machine reconciler, as `phase=installed`
   already does.
2. **The imagebuilder's sealing layer also reports over vsock.** Its boot
   stage, which already writes `guestinfo.banlieue.sealed` (vSphere) and
   `/run/banlieue/sealed` (libvirt), also runs
   `NOTIFY_SOCKET=vsock-stream:2:1024 systemd-notify "sealed=$sealed"` when
   `/dev/vsock` exists, so one image reports on every backend. An operator's
   own image (`Asserted`, ADR-0088 Decision 2) must send the same line.
3. **`CloudHypervisorMachine` carries the declaration and the result**, like
   `LibvirtMachine` and `VSphereMachine`: `spec.firstBootSealing` (set by the
   controller from the image, as for the other backends) and
   `status.sealedPartitions`, sticky (a powered-off guest has not become
   unsealed).
4. **`GuestReady` follows ADR-0088 Decision 5 exactly**, through the shared
   `sealing_gate`: `False` with `FirstBootSealingUnverified` while the
   installed guest has reported but not every listed partition is sealed;
   `False` with `FirstBootSealingTimeout` once
   `firstBootSealing.timeoutSeconds` has passed since `Ready=True` without a
   complete report, published even if the guest never reported anything,
   because a halted clone can say nothing; `True` only when every listed
   partition is reported sealed and the existing gates (installer detached,
   EK certificate published) hold.
5. **The scheduler stops excluding Cloud Hypervisor** for a
   `firstBootSealing` + `tpmEnabled` VM. ADR-0088 Decision 4's Cloud
   Hypervisor bullet becomes: an `Immediate` clone is a private reflink or
   copy, sealed in place, with a fresh swtpm.

**Accept when** (met 2026-10-28, see *Amendment 2026-10-28*) a
`GuestReady` pool of a `firstBootSealing` image, with a `tpmEnabled` class,
warms on a real Cloud Hypervisor host through banlieue (`e2e_pool_claim`, as
ADR-0088's libvirt run), with every member recording
`status.sealedPartitions: [COS_PERSISTENT]`, and a member booted without a
working vTPM reports `FirstBootSealingTimeout`.

## Consequences

- The reference backend runs the default secure image path: sealed per
  clone, with no installer run, as libvirt and vSphere do.
- One image works on all three backends: the sealing layer reports through
  each backend's channel, and each provider reads only its own.
- The vsock report stops being a single-purpose channel. It stays
  untrusted, size- and time-bounded, and line-oriented; a guest can withhold
  `GuestReady` from itself or claim partitions sealed. Claiming without
  sealing is the same exposure as on libvirt and vSphere (the guest is the
  source of the claim) and is recorded there already; nothing a report says
  grants more than `GuestReady`.
- An operator image built for Cloud Hypervisor before this change, with
  `firstBootSealing`, never reports `sealed=` and times out
  (`FirstBootSealingTimeout`), rather than being reported healthy. It needs
  the new line.
- The `phase=installed` report keeps its meaning; images that do not seal
  are unaffected.

## Amendment 2026-10-28: accepted

**The Accept-when criteria are met**, on one Cloud Hypervisor host, with
`banlieue controller` on a throwaway kind cluster and `banlieue provider
cloud-hypervisor` running on the host as the provider's own user, both
built from this change.

- **The image.** Debian 13 through kairos-init v4.4.0, built raw with the
  operator's AuroraBoot v0.24.0 command, as ADR-0088's libvirt run. Its
  cloud-config was rendered by the imagebuilder's own `merge_cloud_configs`
  and `sealing_layer`: a user, the Cloud Hypervisor `phase=installed` stage
  of `examples/17-cloud-config-guest-phase.yaml`, then the sealing layer,
  whose boot stage now ends with the vsock `sealed=` line (Decision 2). The
  raw file went into the storage class's image cache, declared by a
  `BackingFile` `VMImage` with `firstBootSealing: {}`.
- **The pool.** `e2e_pool_claim` (Cloud Hypervisor), `GuestReady`
  readiness, a `tpmEnabled` class: 2 of 2 tests passed, twice. Every
  member's `CloudHypervisorMachine` carried `spec.firstBootSealing` and
  recorded `status.sealedPartitions: [COS_PERSISTENT]` before
  `GuestReady=True`. Two members warmed in 125 s and 130 s; a refill took
  110 s and 105 s. A `Deferred` Kairos install on this backend took 341 s for
  one machine (ADR-0065), so the per-clone install is gone, as on libvirt
  (125 s). Released and collected guests left nothing on the host.
- **No vTPM.** A machine declaring `firstBootSealing` with no vTPM booted,
  printed Kairos's "Encrypt on boot: partition encryption failed ... could
  not find TPM 2.0 device" on its serial console, halted and reported
  nothing. Its `GuestReady` went `False`, `FirstBootSealingTimeout`, with
  the serial-log hint. A `tpmEnabled` control on the same 180 s budget sealed
  in 98 s and went `GuestReady=True`. (The controller never builds such a
  machine, since `firstBootSealing` without `tpmEnabled` is refused; the
  run created the `CloudHypervisorMachine` directly to stand for a clone
  whose vTPM is missing.)

**What the run found.** On the first no-vTPM run the timeout landed 359 s
after `Ready` against a 180 s budget. A running guest with an address is
requeued on the long cadence (300 s), and a report is the only thing that
wakes the reconciler early, which a halted clone never sends. **Decision 4
gains:** while a sealing report is still due (declared, incomplete, not yet
timed out), the reconciler requeues a running guest on the default cadence
(30 s), as libvirt already does (`reconciler.rs`, `sealing_report_due`).
Re-run: 194 s, within one cadence of the budget.
