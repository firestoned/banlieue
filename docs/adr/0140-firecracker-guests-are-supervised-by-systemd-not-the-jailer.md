<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0140: Firecracker guests are supervised by systemd as the jailer, and start from a config file with no API socket

- **Status:** Proposed
- **Date:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0137](0137-firecracker-provider-class.md) (Decision 3 left
  both questions here), [ADR-0063](0063-cloud-hypervisor-host-supervision.md)
  (template units, a per-guest uid, an unprivileged provider),
  [ADR-0105](0105-cloud-hypervisor-guests-slice.md),
  [ADR-0138](0138-a-host-may-run-both-vmms-with-split-budgets.md),
  [ADR-0139](0139-hosts-without-systemd-userdb-or-cgroup-v2.md),
  [ADR-0141](0141-shared-host-provider-layer.md)

## Context

ADR-0137 puts each Firecracker guest in a root-owned template unit run as a
per-guest uid, and left two questions to measurement (roadmap 20, phase 0):

1. Does the VMM need Firecracker's own **jailer**, or is systemd, which
   already sets the uid, the sandbox and the cgroup, enough?
2. Does the VMM start from a **config file**, or from API calls, and is a
   runtime API socket kept?

Both were measured on a RHEL 8.10 host (kernel 4.18, systemd 239, cgroup v1),
Firecracker v1.18.0, in transient units in `banlieue-firecracker.slice`, with
the guest and kernel of the spike (a 512 MiB Ubuntu guest, kernel 6.1.155).
Everything below was observed unless it says otherwise.

### Jailer and systemd, side by side

| | systemd as the jailer (direct) | Firecracker's jailer v1.18.0 |
| --- | --- | --- |
| Runs as | `User=%i`, nothing needs root | a root unit; it needs `CAP_CHOWN`, `CAP_DAC_OVERRIDE`, `CAP_MKNOD`, `CAP_SETUID`, `CAP_SETGID` and `CAP_SYS_ADMIN` (bisected one at a time; eight others were tried and are not needed) |
| VMM after start | uid 2000255, `NoNewPrivs 1`, `CapEff 0`, `Seccomp 2` (Firecracker's own) | the same: `CapEff 0`, `NoNewPrivs 1`, `Seccomp 2`, no supplementary groups |
| Filesystem the VMM sees | the host's, wherever file modes allow; `/etc/shadow`, other guests' directories, `/dev/mem` and `/dev/sda` were denied | a chroot with one mount: nothing of the host |
| Processes it sees | all 1167 in `/proc` (no pid namespace) | the host's too, unless `--new-pid-ns` is passed |
| Per-guest preparation | a tap owned by the uid and a config file owned by the uid | a per-id chroot, root-prepared, holding the kernel, root filesystem and config, **never cleaned up**; a stale one fails the next start (`mknod ... File exists`), after a SIGKILL as well |
| cgroup v1 and the slice budget | the unit's cgroups sit inside the slice; the slice's quota and `MemoryMax` apply | `--cgroup` **without** `--parent-cgroup` moved the VMM to `/firecracker/<id>`, outside the slice's budget (110 MB charged there while the slice showed 7.9 MB). With `--parent-cgroup` pointing into the slice it stayed inside. The cgroups it creates are not removed on exit, and systemd does not know them |
| Supervision | plain `Type=simple`; stop takes about 70 ms | default mode execs into the VMM (`MainPID` is the VMM, stop about 80 ms); `--new-pid-ns` forks, the VMM is pid 1 of its namespace and ignores SIGTERM (stop took 90 s, the timeout); `--daemonize` under `Type=simple` gets the VMM killed |
| Boot, with a NIC, 5 runs (min / median / max, ms) | hardened unit 1911 / 1981 / 2029 | plain root unit 1783 / 1789 / 1816; hardened unit with the six capabilities 1922 / 1940 / 2006 |
| Memory, 512 MiB guest, idle 10 s after login | RSS and PSS 106 to 109 MB | RSS and PSS 108 to 112 MB (about 4 to 5 MB more page cache in the cgroup) |

### Config file and API socket

| | config file, `--no-api` | config file and `--api-sock` | API only |
| --- | --- | --- | --- |
| Boot, no NIC, min / median / max (ms) | 1640 / 1670 / 1704 | 1670 / 1751 / 1831 | 1674 / 1703 / 1764 |
| After a SIGKILL and a restart | the guest boots again from the file | the same | an empty VMM (`Not started`, default 1 vCPU and 128 MiB): it needs every call again |
| Runtime control | signals only | pause and resume, balloon statistics and resize, `SendCtrlAltDel` (the guest restarts and the VMM exits 0 in 331 ms) | the same |

SIGTERM is not handled by Firecracker: it dies by signal within about 70 ms with
no guest shutdown and no flush. systemd counts that as a clean stop. **After any
kill or stop, including SIGTERM, a stale vsock socket and the API socket remain
and block a restart** (`AddrInUse`, `FailedToBindSocket`); removing the socket
files in `ExecStartPre` fixed it.

## Decision

### 1. systemd is the jailer: no Firecracker jailer in v1

The guest unit runs the VMM directly as `User=%i` with the same hardening set
as the Cloud Hypervisor guest template (ADR-0063 Decision 3; on a host older
than a directive's version, ADR-0139 governs). The jailer is not used.

Why, in the order that decided it:

- **It fits the privilege model.** The provider is unprivileged and may only
  start, stop and reset-failed instances of root-owned templates for uids in a
  declared range. The direct unit needs nothing more. The jailer needs a root
  unit with `CAP_SYS_ADMIN` and five other capabilities, and a per-guest chroot
  that root code must prepare **and remove**, which is exactly the privileged
  per-guest work ADR-0063 keeps out of the provider.
- **It keeps the budget.** On cgroup v1 the jailer's own cgroup handling takes
  the VMM out of the slice unless `--parent-cgroup` is also given, and the
  cgroups it creates outlive the VMM and are invisible to systemd. A budget
  that is exceeded by default is what ADR-0105 exists to prevent.
- **The VMM ends up equally confined where it matters.** Under both, the VMM
  runs as the guest's uid with no capabilities, `NoNewPrivs` and Firecracker's
  own seccomp filter.
- **No difference in cost worth buying.** The two are within the measurement
  noise of the hardening itself (below).

### 2. The chroot is a gap, recorded and not hidden

The jailer's one real advantage is the chroot. Under the direct unit the VMM
can read the host filesystem wherever file modes allow, and it sees every
process in `/proc`. What was observed denied: `/etc/shadow`, other guests'
storage and run directories, `/dev/mem`, `/dev/sda`. `/etc`, `/dev` and `/app`
are read-only; `/tmp` is private. This is an accepted risk, to be recorded in
the threat model with a *Revisit when* in the pass that implementation
triggers: a systemd-native equivalent
(`RootDirectory=` with `BindPaths` for the three device nodes) has **not been
measured**, and a host that needs the chroot may use it, or the jailer behind a
root template that also owns cleanup and passes `--parent-cgroup`, only by a
new ADR. Whatever is added, a jailer invocation with `--cgroup` and no
`--parent-cgroup` is refused by a test.

### 3. The VMM starts from a config file, with no API socket (`--no-api`)

The provider writes the guest's boot configuration and the unit starts the VMM
from it, so a restart boots the guest from the same file and a crash can never
leave a half-configured VMM (API mode restarts to an empty VMM that systemd
cannot repair on its own). There is **no API socket in v1**:

- every runtime call it offers is something ADR-0137 does not use in v1: no
  balloon (not used to overcommit), no pause or resume, no graceful guest
  shutdown for a guest whose disk is discarded;
- it is the fastest mode measured (1670 ms median against 1751 ms), and the
  smallest interface to defend, consistent with ADR-0137's reason for
  Firecracker at all;
- it removes one socket to create, own and clean.

If a later decision needs balloon statistics or a graceful stop, the socket is
added by an amendment to this ADR, with the measurements above as the baseline.

### 4. Stopping is `systemctl stop`; restarts are the reconciler's

`systemctl stop` (SIGTERM) is the whole stop path: it takes about 70 ms and
needs no guest cooperation. The unit has **`Restart=no`**: a crashed guest is
replaced by the reconciler, never resurrected by systemd. A silent restart
would boot the same guest again without a fresh identity bootstrap
(ADR-0137 Decision 6: the join token is single use), and a sandbox pool member
is ephemeral by definition.

### 5. The template cleans its own sockets, and the provider prepares the inputs

Every stop or kill leaves vsock sockets behind, so the template removes the
guest's socket files in `ExecStartPre` (as the uid, inside its own run
directory, which is its only writable path). The provider, which already has
`CAP_NET_ADMIN`, `CAP_CHOWN` and `CAP_FOWNER` for the Cloud Hypervisor
guests (ADR-0063 Decision 4), prepares what the unit needs before it starts:
the persistent tap owned by the guest's uid joined to the bridge, and the
config file owned by the uid (a file the provider writes as root with the
default umask is 0640 root and the VMM cannot read it). No new privilege is
added to the provider.

### 6. Accounting is on, so the overhead can be measured

The unit sets `CPUAccounting=yes` and `MemoryAccounting=yes`. Without the first
the unit has no cpu cgroup (it ran in the slice's), and ADR-0106's per-VM
overhead sampler would read the slice for every guest. `MemoryMax` is guest
memory plus a named overhead constant, as for Cloud Hypervisor; **that
constant is set from a measurement under load, not from the idle figures
above** (106 to 109 MB idle with a 512 MiB guest is a resident size, not an
overhead).

## Consequences

- The Firecracker provider is the Cloud Hypervisor provider's twin in
  supervision: one template, one uid, no new root-owned per-guest state, and
  nothing for the provider to clean beyond its own tap and files.
- Cold boot on this host measured 1.64 to 1.70 s with no NIC and 1.91 to 2.03 s
  with a NIC and a hardened unit, for the spike's Ubuntu guest. That is not the
  sandbox image and not time to `GuestReady`; neither is measured yet.
- The hardened unit was slower than the plain jailer unit by about 150 to
  190 ms in these runs; the cause (which directive, or the uid) was **not
  isolated**. It is a measurement to do before the 2 s target is promised.
- Not measured, and so not decided: the number of seccomp filters (the
  kernel on the test host does not report it); `--daemonize` with
  `Type=forking`; the jailer's cgroup and netns paths with a reduced
  capability set; a guest-initiated `reboot -f` in config mode.
- The accepted chroot gap (Decision 2) widens what an escaped VMM can read
  compared with the jailer: file contents under world-readable modes and the
  process list. Nothing secret is on a Firecracker command line: the identity
  token travels over vsock (ADR-0137 Decision 6).
- Threat model: a new accepted risk (no chroot or pid namespace for the VMM)
  and one fewer privileged component than the jailer path would add. Pass due
  when implemented.
- Not implemented.
