<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Cloud Hypervisor vTPM pool timings

> **Measured 2026-10-10 and 2026-10-11**, banlieue v0.4.1, one host: a run of
> fourteen members on stock Cloud Hypervisor v53.0, then ten on v53.0 with
> the vTPM fix below. A point-in-time measurement, not a living table: rerun
> it with `make ch-pool-timings` when the provider's pacing, the image or the
> VMM version changes. The broader per-provider numbers are in
> [Provider comparison](provider-comparison.md).

This page answers one question for roadmap 17's sandbox pools on Cloud
Hypervisor: **how long does each stage of a `GuestReady` pool take, from the
pool's creation to a claimed sandbox being destroyed?** Every member is a
`tpmEnabled` machine that installs itself from an installer ISO
(`installMode: Deferred`), seals its persistent partition to its own vTPM, and
reports over vsock (ADR-0065). Nothing is cloned; every member is a full
encrypted install.

## Summary

| What | Result |
| --- | --- |
| A member becomes claimable (`GuestReady`), from `VirtualMachine` creation | **108 s** median (97–111 s), 10 of 11 healthy members; one outlier at 190 s |
| Three members installing at once, all healthy, to `Warm` | **107.5 s** (the refill after three simultaneous claims) |
| Refill after one claim, back to `Warm` | **103 s** and **122 s** |
| Refill after three claims at once, back to `Warm` | **107.5 s** when all three installs succeeded |
| Claim created to `Bound` | **0–1 s** server-side (`creationTimestamp` to `boundAt`), all 8 claims |
| Claim deleted to claim, VM and host unit all gone | **6.4 s** and **6.7 s** for 4 claims at once |
| Pool deleted to pool and members gone | **5.1 s** |
| Six members installing at once, to `Warm` | **141 s** (installs slow under contention: 104 s each, against 77 s at three) |
| Members lost to the vTPM fault below | **3 of 14** on stock v53.0, each costing the pool's full `provisioningTimeoutSeconds` (900 s); **0 of 10** with the fix |

vSphere's 2-member `GuestReady` pool warmed in 130.3 s (roadmap 17); this one
brings three concurrent members to `Warm` in 107.5 s with no split-image work (roadmap 18).

## Environment

- **Host:** two 8-core Intel Xeon E5-2630 v3 (32 threads), 62 GiB, one SATA
  SSD, ext4; Debian 13. The same host as the Cloud Hypervisor column in
  [Provider comparison](provider-comparison.md). All fourteen members landed
  on it: with two Cloud Hypervisor Providers serving the class, the scheduler
  breaks the tie alphabetically rather than spreading (roadmap 17 phase G).
- **Hypervisor:** Cloud Hypervisor v53.0, swtpm 0.7.1, one `banlieue-ch@`
  unit and one `banlieue-swtpm@` unit per guest (ADR-0063, ADR-0065).
- **banlieue:** v0.4.1 provider on the host; v0.4.1 controller and operator
  on a separate three-node management cluster.
- **Class:** 2 vCPU, 4 GiB, 20 GiB thin OS disk, `tpmEnabled: true`, one NIC
  on the host's NAT bridge.
- **Image:** Kairos Hadron v0.4.0 core installer ISO, `BackingFile` source,
  `installMode: deferred`. User-data: one `users:` entry, `install.auto` to
  the empty disk with `encrypted_partitions: [COS_PERSISTENT]`, `reboot: true`,
  and a `boot` stage that runs `systemd-notify phase=installed` over
  `vsock-stream:2:1024` once the installed system is active.
- **Pool:** `warmReplicas: 3`, `readiness: GuestReady`,
  `provisioningTimeoutSeconds: 900`.

## Per-stage timings

Times are from the `VirtualMachine`'s `creationTimestamp`, over the 11
members that reached `GuestReady`. Median, then min–max.

| Stage | From | Median (min–max), s |
| --- | --- | --- |
| Scheduled; infrastructure CR created | VM created | 0.0 (0.0–1.0) |
| Guest uid allocated on the host | VM created | 0.7 (0.3–1.7) |
| vTPM manufactured (`swtpm_setup` run time) | its unit start | 0.2 (0.1–0.2) |
| swtpm started | VM created | 4.1 (1.2–5.1) |
| VMM unit started | VM created | 7.1 (1.2–8.2) |
| VM created in the VMM; the installer boots | VM created | 10.2 (1.4–11.3) |
| `Ready` (the VM is running) | VM created | 10.0 (1.0–11.0) |
| **Install:** installer boot to the installed system's kernel | VM boot | **77.0 (72.8–82.0)** |
| Installed system's boot to its report | its kernel start | 18.7 (18.4–105.9) |
| Report to installer ejected | the report | 0.2 (0.0–0.2) |
| **`GuestReady`, the member is claimable** | VM created | **108.0 (97.0–190.0)** |

Where the 108 s goes, for a typical member:

```
0 s     VM created, scheduled, infra CR created
1 s     guest uid allocated; vTPM manufactured (0.2 s of work)
4 s     swtpm started
7 s     VMM unit started
10 s    VM created and booted from the installer ISO           -> Ready
        ...installer: partition, install, encrypt COS_PERSISTENT
           to the vTPM, reboot (about 77 s)...
87 s    installed system's kernel starts
106 s   boot stage reports phase=installed over vsock
106 s   provider ejects the installer (0.2 s later)             -> GuestReady
```

**Reading it.**

- **The install is 70 % of the time** and is all the guest's own work:
  writing the image, creating and sealing the LUKS volume, and a reboot. The
  provider cannot shorten it. Roadmap 18's split-image clone is the lever.
- **About 9 s of the first 10 s is pacing, not work.** The provider advances
  one step per reconcile pass (manufacture, then swtpm, then the VMM), and
  passes are about 3 s apart; the steps themselves take well under a second.
  One member went through all of them in 1.4 s, so the pacing is not
  inherent. This is a cheap optimization target.
- **The installed boot takes about 19 s to report.** The report is the last
  thing its `boot` stage does.
- **One healthy member reported 106 s after its installed boot**, not 19 s,
  and so became claimable at 190 s. Its install time was normal; the delay
  was inside the installed guest before the report ran. Not investigated.

## Claims

Eight claims in two admission modes against the same warm pool: four created
by the claim's own subject, an OIDC user, through ADR-0047's
subject-authorization policy; four created by a declared **broker** on behalf
of another subject. Each mode made one claim, waited for the pool to refill,
then three claims at once.

| | Subject's own claim (OIDC) | Broker on behalf of a subject |
| --- | --- | --- |
| `creationTimestamp` to `boundAt` (server-side) | 0, 1, 0, 0 s | 0, 0, 0, 0 s |
| Three claims at once, all `Bound` | 2.5 s observed | 4.3 s observed |
| Refill after one claim | 103.1 s | 121.6 s |
| Refill after three claims | 107.5 s | 1101 s (one install lost to the vTPM fault, replaced after 900 s) |
| Delete to claim gone | 3.6–5.4 s | 3.6–5.6 s |
| Delete of all four to claims, VMs and host units gone | 6.4 s | 6.7 s |

**Reading it.**

- **Binding is effectively instant**: the controller binds a warm member in
  the same second the claim is created, in both modes. Observed times of
  2–8 s are the harness's poll interval (about 3 s), not the system.
- **Admission mode makes no measurable difference** at one-second resolution.
- **A claim never waits for an install** while the pool has a warm member;
  the refill runs behind it. A claim that arrives when the pool is empty
  waits for a full install (about 108 s), or longer if that install hits the
  vTPM fault.
- **Release is a destroy**, and it is complete in about 6.5 s: the claim and
  its `VirtualMachine` go, and the guest's VMM unit is stopped on the host
  (ADR-0047 Decision 6). Four releases at once take no longer than one.

## The vTPM fault, and its fix

On stock Cloud Hypervisor v53.0, three of fourteen members never reported.
In the guests that were inspected, the kernel logged `tpm_recv: error -5`
about 0.2–0.4 s after the Kairos installer first opened `/dev/tpmrm0`, then
`/dev/tpmrm0` was "State not recoverable" and the installer exited at the
encryption step. swtpm logged nothing.

**Cause: a 100 ms read in Cloud Hypervisor, not banlieue.** The VMM passes
each TPM command to swtpm over a stream socket and reads the response with a
single `recvfrom`, on a socket whose receive timeout is 100 ms
(`tpm/src/emulator.rs`, `prepare_data_fd` and `deliver_request`). A command
swtpm takes longer than that to answer (key generation, which LUKS sealing
starts with, on a busy host) is reported to the guest as failed: the CRB
device sets `CTRL_STS.tpmSts`, the TPM's fatal-error bit, which the kernel's
`crb_recv` turns into `-EIO` and which nothing ever clears. A single read can
also return part of a response. The code is unchanged on upstream `main` as
of 2026-10-11.

**Proof.** Throttling one guest's swtpm to 3 % CPU with systemd `CPUQuota`
makes the fault deterministic on stock v53.0: the guest kernel's own TPM
probe at boot fails with `error -5`, and `/dev/tpmrm0` never appears. With
the fix, the same throttle produces slow answers instead of errors: the
member installs, seals `COS_PERSISTENT` to its vTPM and is claimable in about
102 s, as fast as an unthrottled one.

**Fix.** Read the 10-byte response header, then exactly the size it
declares, looping over partial reads; keep the 100 ms socket timeout as a
poll interval, retried until a 120 s deadline (Linux allows TPM2 commands up
to 300 s). A declared size smaller than a header or larger than the buffer is
an error. Unit tests cover a response split across a delay longer than the
socket timeout, the deadline, an impossible size, and swtpm closing
mid-response. With it, ten installs, six of them concurrent, had no fault.

**Status.** The fix is a Cloud Hypervisor change, reported upstream as
[cloud-hypervisor#9053](https://github.com/cloud-hypervisor/cloud-hypervisor/issues/9053) (2026-10-11). A
host takes a patched VMM through
`banlieue host cloud-hypervisor install --vmm-url … --vmm-sha256 … --vmm-version …`
(ADR-0084). Until a host has it, a shorter `provisioningTimeoutSeconds`
(about 3× the 108 s median) bounds the cost of each fault. Tracked in
roadmap 17 phase G.

## How it was measured

`make ch-pool-timings` (`scripts/ch-pool-timings.py`; its module docs list
the settings) creates the pool and claims through the Kubernetes API and polled the pool, its `VirtualMachine`s, their
`CloudHypervisorMachine`s and the claims about every 3 s, recording the
first time each transition was seen. Every stage above uses the more precise
source available rather than the poll:

- **Cluster stages** use the objects' own timestamps (`creationTimestamp`,
  condition `lastTransitionTime`, the claim's `boundAt`), to the second.
- **Host stages** use the provider's and systemd's journal on the host
  (`guest uid allocated`, `manufacturing the vTPM`, the
  `banlieue-swtpm-setup@` unit's start and finish, `starting swtpm`,
  `starting VMM unit`, `creating VM`, `ejecting the installer`, teardown),
  to the microsecond.
- **Guest stages** come from each installed guest's own journal, read over
  SSH while it was alive: the installed boot's first entry and the report.
  The installer's boot is not kept in the guest's journal, so the install is
  measured from the host's `creating VM` to the installed kernel's start,
  and includes the installer's reboot.

Host and guest clocks were NTP-synchronised; the guest's report and the
host's `GuestReady` agree to within 0.6 s throughout, which bounds the skew.
