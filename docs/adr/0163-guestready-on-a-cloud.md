<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0163: GuestReady on a cloud

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 22 phase E, roadmap 23. Extends
  [ADR-0043](0043-guestready-installed-guest-signal.md) (one `GuestReady`
  condition, a per-backend transport) with a transport for Azure; EC2's is
  decided by roadmap 24's spike. [ADR-0054](0054-nocloud-seed-iso-first-party.md)
  (user-data delivery is per backend), [ADR-0158](0158-shape-of-a-cloud-provider-class.md),
  [ADR-0162](0162-publishing-a-vmimage-into-a-cloud.md).

## Context

ADR-0043 gives `GuestReady` one condition and a transport per backend:
guestinfo on vSphere, a file read through the guest agent on libvirt, vsock
on Cloud Hypervisor. A cloud VM has no host channel banlieue controls.
Roadmap 22 listed three candidates: the serial console read by API, a cloud
guest agent, or a guest that calls out.

The roadmap 23 spike found a fourth, already built into Azure, and proved it
on real VMs:

1. **Azure requires the guest to report ready.** A VM from a *generalized*
   image stays `Creating` until something in the guest fetches its goal state
   from the host-local WireServer (`168.63.129.16`, `GET
   /machine?comp=goalstate`, `x-ms-version: 2012-11-30`) and posts a
   `Health` document with `<State>Ready</State>` (`POST
   /machine?comp=health`). Without it, the VM fails with
   `OSProvisioningTimedOut` after about 20 minutes, however healthy it is.
   Kairos ships neither the Azure agent nor cloud-init, so it never reports.
2. **A Kairos image can report it itself.** A baked cloud-config step in
   `network.after`, gated on the Azure DMI chassis asset tag and on not being
   in recovery mode, made the post with `curl` and got HTTP 200; Azure moved
   the VM to `ProvisioningState/succeeded` within seconds. Measured create to
   `Succeeded`: 177 s, 307 s and 330 s across three clean boots in two zones,
   before the boot-time fixes in finding 5. A reboot reported again (HTTP
   200, same incarnation).
3. **The report needs a route, and the route was racy.** On sizes with
   accelerated networking, the VF and the synthetic NIC both took a DHCP
   lease in the initramfs; when the default route landed on the VF, cleaning
   the VF removed it and WireServer was unreachable. Two of four boots
   failed this way before the image was fixed.
4. **User data was applied after the report.** customData read in
   `network.after` applied on the *next* boot, after Ready had been sent, so
   "Ready" did not yet mean "configured as asked".
5. **Two minutes of every boot were waiting, not working.** `systemd-networkd-
   wait-online` timed out on the VF, and a Kairos step started a QEMU agent
   on Hyper-V (its DMI match on "Virtual Machine" fired) and waited 90 s.
   With the VF left unmanaged from the initramfs and the agent masked on
   Azure, an active boot took 18.8 s (from 2 min 17.5 s) and reported ready
   at 18.1 s of uptime; create to `Succeeded` was 189 s, nearly all of it the
   one-off first boot (recovery, layout, reset, reboot).
6. **Console logging was unreliable**: a step holding `/dev/ttyS0` open went
   silent once `agetty` started (vhangup). `/dev/kmsg` survived.

## Decision

### 1. On Azure, `GuestReady` is the VM's provisioning state

The provider reads `properties.provisioningState` and the instance view's
`ProvisioningState/succeeded` through ARM. `Succeeded` sets
`status.guestInstalled = true` (sticky, ADR-0043 Decision 5) and
`GuestReady=True`; `OSProvisioningTimedOut` sets `GuestReady=False` with that
reason. banlieue's side only **reads**, through ARM: no channel into the
guest, no credential in the guest (ADR-0043 and ADR-0045's read-only rule
holds).

### 2. The guest reports only when it is actually ready

The report is made by the image, not by an Azure agent, and only when:

- the system booted from its installed disk (active or passive mode, never
  recovery: ADR-0043 Decision 2);
- **user data has been applied** (finding 4): customData is read from the
  provisioning disk early enough that its stages run in this boot, before the
  report;
- for a sandbox image, the image's own readiness (for mediatore, its agent
  attested), whichever the image declares.

The report retries until it gets a 2xx, waiting for a route to WireServer
first (finding 3).

### 3. What an Azure image must carry

The provider refuses nothing here; these are requirements on the image,
documented in the guide and checked by the image build:

- **read customData locally** from the provisioning disk (UDF,
  `ovf-env.xml`, `<CustomData>` base64), never from a metadata address;
- **never configure the accelerated-networking VF** (drivers `mana`,
  `mlx4_core`, `mlx5_core`): excluded in the initramfs network config, where
  the catch-all DHCP config is written (findings 3, 5);
- **no QEMU guest agent start on Hyper-V** (finding 5);
- **log to `/dev/kmsg`**, not the serial device (finding 6);
- **report ready** as in Decision 2.

### 4. The provider always enables boot diagnostics

Every VM is created with managed boot diagnostics on. Without it a VM that
fails to report is undiagnosable: after `OSProvisioningTimedOut` the VM
refuses every update, including enabling diagnostics.

### 5. The timeout is the cloud's, and bounds first boot

A first boot (recovery, layout, reset, reboot, active boot, user data, image
readiness) must finish inside Azure's provisioning window (about 20 minutes).
A sealed first boot (ADR-0088) is measured against it before sealed pools are
offered on Azure.

## Consequences

- `GuestReady` works on Azure with no agent, no polling of the guest, and no
  credential in it, and the cloud itself enforces a deadline.
- Images that do not report cannot run on Azure from a generalized
  definition. That is a feature: an image that does not know it is on Azure
  would also not read its user data.
- The image-side requirements (Decision 3) belong in the image, not in
  banlieue; mediatore's image (and any consumer image) needs them, and
  Kairos's `26_vm.yaml` match should be fixed upstream.
- EC2 has no equivalent handshake; roadmap 24 chooses its transport.

## Alternatives considered

- **Serial console read by API.** Works (the spike read it constantly) but is
  a log, not a signal: parsing it is fragile and its refresh is slow.
- **The Azure guest agent.** Brings a large agent and extension handling into
  a sandbox image for one status bit.
- **Specialized image plus `userData` from IMDS, no handshake.** No deadline,
  but nothing tells Azure (or banlieue) the guest is ready, and `userData` is
  readable through ARM and by any process in the guest.
