<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0065 — vTPM through swtpm, and `Deferred` install, on Cloud Hypervisor

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-09-25 (phase 0 spike results folded in; the four
  spike-gated points are verified); Decision 1 moves TPM manufacture out of
  the guest uid, into a one-shot unit run as the provider user, so the EK CA
  key is never readable by a guest uid
- **Related:** [ADR-0040](0040-deferred-install-for-vtpm-encryption.md)
  (`Deferred` install), [ADR-0043](0043-guestready-installed-guest-signal.md)
  (`GuestReady`), [ADR-0044](0044-detach-install-media-after-install.md)
  (eject install media), [ADR-0045](0045-vtpm-endorsement-key-certificate.md)
  (EK certificate), [ADR-0048](0048-tpm-enabled-requires-deferred-install.md)
  (`tpmEnabled` needs `Deferred`), [ADR-0049](0049-attestation-trust-anchors.md)
  (trust anchors), [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md),
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md); roadmap 09 phase 6,
  roadmap 17.

## Context

Roadmap 17's sandboxes are TPM-sealed guests installed `Deferred`: each
member installs itself onto an empty disk and seals to its own fresh vTPM,
and is never cloned (ADR-0040, ADR-0048). On libvirt, libvirtd runs swtpm,
QEMU provides a CD-ROM for the installer, and `qemu-guest-agent` carries
`GuestReady` and the EK certificate out of the guest (ADR-0043, ADR-0045).

Cloud Hypervisor has none of those pieces:

- **TPM:** `--tpm` takes only the path of an swtpm socket (`TpmConfig` in
  the v53.0 API). Running swtpm is the provider's job.
- **Install media:** there is no CD-ROM; the installer ISO is a read-only
  virtio disk.
- **Guest agent:** there is no QEMU-style agent socket. The VMM offers
  **vsock**, whose host end is a Unix socket.
- **Reboots:** a guest reboot resets the VM in place with the same devices,
  so anything "removed for the next boot" is still there after the guest's
  own reboot (roadmap 09, Gotchas).

The decisions below that depended on behaviour nobody had yet seen on this
VMM were first marked **spike-gated**. Roadmap 09's phase 0 spike
(2026-09-25, Cloud Hypervisor v53.0, edk2 `ch-97eeb7b09`, swtpm 0.7.1, Kairos
Hadron v0.4.0 core) ran all of them. Each is now marked **verified** with
what was observed.

## Decision

### 1. One swtpm per machine, run by systemd

`banlieue-swtpm-<machine-uid>.service` (ADR-0063) runs:

```text
swtpm socket --tpm2 --tpmstate dir=<machine-dir>/tpm
             --ctrl type=unixio,path=/run/banlieue/ch/<uid>/swtpm.sock
```

as the guest's uid. The VMM gets
`--tpm socket=/run/banlieue/ch/<uid>/swtpm.sock`.

The TPM state is created once, before first start, by `swtpm_setup` running
in a **separate one-shot unit, `banlieue-swtpm-setup-<machine-uid>.service`,
as the provider's `banlieue` user**. systemd runs it, not the reconciler, so
no subprocess enters the reconcile path. The unit then hands the state
directory to the guest's uid. It is not the swtpm unit's `ExecStartPre=`
running as the guest uid, as first drafted: `swtpm_setup` signs the EK
certificate with the host CA's private key, so any uid that manufactures a
TPM can read that key. A guest uid that could read it could mint EK
certificates this host's CA vouches for, which is exactly what the
certificate exists to rule out. The CA key is `banlieue`-only
(`scripts/bootstrap-cloud-hypervisor-host.sh`). The unit runs:

- `--create-ek-cert --create-platform-cert`, signed by the host's
  `swtpm_localca`;
- `--vmid <machine-name>:<machine-uid>`, which becomes the EK
  certificate's subject CN, the same `<name>:<uuid>` shape ADR-0045
  Decision 3 checks on libvirt.

The setup unit is skipped when state exists (`ConditionPathExists=!`), so a
restart never re-creates a TPM.

**Every swtpm path is absolute.** Daemonized swtpm changes directory to `/`.
With a relative `--tpmstate` it cannot find its lock file, fails `CmdInit`,
and the VMM refuses to boot (`Control Cmd CmdInit returned error code :
0x9`). A systemd unit needs absolute paths anyway; this states why it is
not optional.

**verified:** the guest has `/dev/tpm0` and `/dev/tpmrm0` and sees the
VMM's `CHTPM2` ACPI table. The EK certificates carry
`CN=<machine-name>:<machine-uid>` exactly, issued by `CN=swtpm-localca`.
There are two: RSA-2048 at NV `0x01c00002` and **ECC P-384 at
`0x01c00016`**. ADR-0045 names `0x01c0000a` (P-256) for ECC, which is not
where swtpm puts it, so a guest export reads both `0x01c00002` and
`0x01c00016`.

### 2. TPM state has exactly one owner and one lifetime

The state lives in the machine's directory, keyed by machine UID
(ADR-0063 Decision 5). It is never copied, never shared, and deleted by the
machine's finalizer. The finalizer test asserts that the directory is gone,
which is the lesson ADR-0050 learned from `VIR_DOMAIN_UNDEFINE_TPM`.

Advertise `FEATURE_VTPM` only when `swtpm`, `swtpm_setup` and a
`swtpm_localca` configuration are present and usable on the host.

### 3. `Deferred` layout: empty OS disk first, installer second

For `bootSource: InstallMedia`, the provider attaches, in order:

1. an empty raw OS disk of `osDiskSizeGiB`;
2. the installer ISO, read-only, `image_type=raw`, device id `install`;
3. the NoCloud seed, read-only;
4. any data disks.

With no persistent UEFI variables, the firmware skips the unbootable empty
disk and boots the installer. After the install, the first disk is
bootable and wins.

**verified:** the firmware logs the empty disk as `Not Found` and boots
the ISO next. An unattended Kairos install with `encrypted_partitions:
[COS_PERSISTENT]` seals the LUKS2 passphrase into the vTPM's NV storage
("Using local TPM NV passphrase … Partition unlocked and verified"). The
reboot, in place in the same VMM process, lands on the installed disk
(`active_mode`), not the installer. The same state unlocks after a full
VMM and swtpm restart.

### 4. Ejecting the installer is two operations, both required

Once `guestInstalled` is true, and before `GuestReady` (ADR-0044 Decision 1):

1. **Live:** `vm.remove-device` with id `install` (ADR-0061), so the device
   is gone from the running VMM and cannot reappear on a guest-initiated
   reboot.
2. **Future starts:** `status.installMediaDetached = true`. The provider
   rebuilds the VMM configuration from spec and status on every start and
   leaves the installer out once this is true.

Only when both are done is `installMediaDetached` set and `GuestReady`
allowed.

**verified:** `vm.remove-device` with id `install` removes the disk from
the VMM configuration, and the running guest loses it at once. A
guest-initiated reboot afterwards comes back without it, still
`active_mode` and still unlocked. The planned fallback, a full VMM restart
before `GuestReady`, is not needed.

After the unplug the guest's disk names shift on the next boot (the seed
moves from `vdc` to `vdb`). Nothing may address guest disks by
`/dev/vdX`: the seed is found by label and the OS disk is always first.

The NoCloud seed stays attached, as ADR-0044 Decision 4 decided for
libvirt, with the same accepted risk.

### 5. `GuestReady` and the EK certificate travel over vsock

Every machine gets a vsock device, `--vsock cid=<n>,socket=/run/banlieue/ch/<uid>/vsock.sock`.

**The provider listens; the guest connects and reports.** In Cloud
Hypervisor's hybrid vsock, a guest connecting to host port `P` reaches the
Unix socket `vsock.sock_P`. The provider listens on one fixed port per
machine. That listener is a local Unix socket in the guest's own run
directory, not a network listener.

The installed system's `boot` stage, guarded on `active_mode` or
`passive_mode` exactly as ADR-0043 Decision 2, connects and sends a small
line protocol:

```text
phase=installed
ek-pem-begin
-----BEGIN CERTIFICATE----- ...
ek-pem-end
```

The same stage writes the same facts to `/run/banlieue/` as on libvirt, so
one image serves both backends. It is re-sent on every boot (ADR-0043
Decision 3).

The provider **sends nothing back**. Everything received is untrusted input:

- size-capped and strictly parsed;
- the certificate's CN is checked against `<machine-name>:<machine-uid>`
  before it is published (ADR-0045 Decision 3);
- a mismatch sets `Ready=False`, reason `TpmEndorsementMismatch`;
- publishing the certificate gates `GuestReady` for `tpmEnabled` machines
  (ADR-0045 Decision 5).

This is the same read-only direction ADR-0043 chose: the host learns facts
from the guest, and never executes anything in it.

When roadmap 17 phase C's in-guest agent exists, it uses this channel. It
does not get a second one.

### 6. The trust anchor is per host

EK certificates are signed by the host's `swtpm_localca`. The provider
publishes that CA certificate on `Provider.status`, which is the input
roadmap 17 phase F's `Provider.spec.attestation.ekTrustBundle` needs
(ADR-0049). A verifier trusts a Cloud Hypervisor guest's EK only through
the host that created it.

### 7. No UEFI variable store means no Secure Boot on this class

`CLOUDHV.fd` is expected to have no persistent variable store, so keys
cannot be enrolled and Trusted Boot/UKI images are out of scope on
`cloud-hypervisor`. That matches where ADR-0051 left sandboxes (classic
GRUB). Such images are rejected with `ImageClassMismatch`, as ADR-0048's
check does.

**verified:** the running firmware accepts a new non-volatile variable,
and it is gone after a guest reset in the same VMM process. edk2's `NvVars`
fallback is present but does not keep it.

## Consequences

**Positive**

- The same sealing, never-clone and `GuestReady` guarantees as libvirt, and
  the same status fields, so roadmap 17 pools work on this class unchanged.
- One guest image serves both KVM backends: the `boot` stage writes files
  for libvirt and reports over vsock for Cloud Hypervisor.
- TPM state is per machine, has one owner and is deleted with its machine.
- The guest channel is read-only and local, with no `guest-exec`
  equivalent.

**Negative / accepted costs**

- The guest image needs a vsock sender in its `boot` stage (for example
  `socat`, until the phase C agent). That is a new image requirement for
  this class.
- A per-host EK CA means verifiers need one anchor per host. Accepted: that
  is what `ekTrustBundle` is for.
- No Secure Boot or UKI on this class.

**Follow-ups**

- Correct ADR-0045's ECC EK index (`0x01c0000a` → read `0x01c00016` too)
  and example 20's export stage to match, so the libvirt export does not
  miss the P-384 certificate either.
- Guest images for this class need `tpm2-tools`: Kairos Hadron *core* has
  no `tpm2_nvread`, so example 20's stage failed in the spike. Same
  requirement example 20 already states for libvirt.
- Roadmap 17: add phase G for this backend (roadmap 09 phase 7).
- The threat model: the vsock channel as a guest → host input path.
