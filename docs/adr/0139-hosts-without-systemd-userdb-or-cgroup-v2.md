<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0139: Hosts without systemd userdb or cgroup v2: guest accounts in the files database, CPU budgets by quota

- **Status:** Accepted
- **Date:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Amends:** [ADR-0063](0063-cloud-hypervisor-host-supervision.md) Decision 3
  (how guest uids are registered), [ADR-0105](0105-cloud-hypervisor-guests-slice.md)
  Decision 2 (the budget flags)
- **Related:** [ADR-0067](0067-banlieue-host-install-subcommand.md),
  [ADR-0084](0084-host-install-release-sources-and-versions.md),
  [ADR-0137](0137-firecracker-provider-class.md),
  [ADR-0138](0138-a-host-may-run-both-vmms-with-split-budgets.md)

## Context

ADR-0063 to ADR-0067 and ADR-0105 were built and checked on systemd 252 and
newer, with cgroup v2. The first host that is not that, a RHEL 8.10 host
(systemd 239, cgroup v1), was prepared for the installer and `preflight` was run
read-only. It fails in four places, none of which is a defect of the host:

1. **Guest accounts cannot resolve.** Decision 3 of ADR-0063 registers each
   guest uid as a systemd *userdb* record in `/etc/userdb`, resolved by
   `nss-systemd`, because a unit with `User=<uid>` fails with status 217 when
   NSS does not know the uid. systemd's userdb arrived in 245; on 239 there is
   no `userdbctl`, no `systemd-userdbd`, and `libnss_systemd` does not read
   `/etc/userdb`. The installer's own post-check ("NSS does not resolve the
   guest records") fails.
2. **`nsswitch.conf` is policed for a `systemd` source** that would not help on
   such a host, and is often managed by `authselect` (editing it by hand
   breaks the host's `authselect check`).
3. **CPU pinning is silently a no-op.** On systemd 239 with cgroup v1,
   `AllowedCPUs=` is accepted, stored and shown by `systemctl show`, but no
   cpuset cgroup is ever created, so nothing is enforced. That breaks "the
   budget is declared, never inferred" (ADR-0105) in the worst way: the
   operator believes a ceiling exists.
4. **One unit directive is silently weaker.** `PrivateIPC=` (systemd 248) is
   unknown to 239: in a unit file it logs `Unknown lvalue` once and the unit
   starts normally, **sharing the host's IPC namespace**.

Measured on the host (a throwaway slice and transient units, systemd 239-82,
cgroup v1, 2026-11-18), not assumed:

| Setting | systemd reports | Kernel enforces |
| --- | --- | --- |
| `AllowedCPUs=0-3` | accepted, shown | **no** (no `cpuset` cgroup exists) |
| `CPUWeight=50` | accepted | **yes**, mapped to `cpu.shares=512` (weight 100 is 1024) |
| `CPUQuota=4800%` | `48s` per second | **yes**, `cpu.cfs_quota_us=4800000` over 100000 |
| `MemoryMax=250G` | accepted | **yes**, `memory.limit_in_bytes=268435456000` |
| `TasksMax=1000` | accepted | **yes**, `pids.max=1000` |

Every other directive in the guest, swtpm and import templates was accepted
by 239 and checked for its effect as an unprivileged user: `ProtectSystem=strict`
and `ReadOnlyPaths=/dev` (writes to `/etc` and `/dev/shm` fail),
`ProtectHome=yes`, `NoNewPrivileges=yes` (`NoNewPrivs: 1`), `PrivateTmp=yes`,
`DevicePolicy=closed` with `DeviceAllow` (`/dev/kvm` opens, `/dev/tpm0` is
denied). The only exception is `PrivateIPC=`.

Enterprise Linux 8 hosts are common and are the first real KVM hosts available
for the Firecracker work (ADR-0137, ADR-0138), so this cannot wait for them to
be replaced.

## Decision

### 1. The installer chooses how guest accounts are registered, and says so

`install` takes `--guest-accounts userdb|files` (`BANLIEUE_HOST_GUEST_ACCOUNTS`).
Unset, it is **derived from a fact, not a guess**: `userdb` when the host's
systemd is 245 or newer *and* `nsswitch.conf` has a `systemd` source for both
databases; otherwise `files`. It prints which it chose and why, and an explicit
flag always wins (and is checked against the facts: `userdb` on a host that
cannot do it is refused).

- **`userdb`:** unchanged (ADR-0063 Decision 3).
- **`files`:** one user `banlieue-g<uid>` and one private group with the same
  number per guest, created with the host's own `groupadd` and `useradd`
  (no home directory, no login shell, no password, a system account), never by
  editing `/etc/passwd` or `/etc/group` by hand: the tools take the locks,
  keep the shadow files consistent and respect the host's account policy
  hooks. The records are idempotent (an existing record in the range with the
  right name is kept; any other account in the range is the conflict the
  preflight already refuses).

Both modes keep every property of ADR-0063: a private group per guest, no
shared group, the same names, the same uids. The preflight check on
`nsswitch.conf` becomes mode-specific: `userdb` needs a `systemd` source;
`files` needs only `files` or `compat` on `passwd` and `group` (which every
host has). The installer **never edits `nsswitch.conf`** in either mode.

The default range of 1024 uids (ADR-0063) is what `files` mode creates, so it
is 2048 lines on a host whose directory service enumerates accounts. `files`
mode therefore prints the count and the operator can lower
`--guest-uid-count` before anything is written.

### 2. The budget flags are refused where they would not be enforced

The installer detects the cgroup mode and the systemd version. A flag whose
setting the host would silently ignore is **refused with a message**, never
written:

| Flag | Setting | Enforced when |
| --- | --- | --- |
| `--guest-cpus` | `AllowedCPUs=` | unified hierarchy (cgroup v2) only |
| `--guest-cpu-weight` | `CPUWeight=` | v1 (as `cpu.shares`) and v2 |
| `--guest-memory-max` | `MemoryMax=` | v1 and v2 |
| `--guest-tasks-max` | `TasksMax=` | v1 and v2 |
| `--guest-cpu-quota` (new) | `CPUQuota=` | v1 and v2 |

### 3. A CPU quota flag, for hosts that cannot pin

`--guest-cpu-quota` (`BANLIEUE_HOST_GUEST_CPU_QUOTA`) renders `CPUQuota=` in the
slice. Form: a positive percentage, `4800%`, meaning 48 CPUs' worth of time
(validated against the same strict grammar as the other flags, before anything
is rendered). It works on both cgroup modes, and so is also available on v2
hosts. It is a **share of time, not a set of cores**: the guests are not kept
off other workloads' cores and share SMT siblings with them. The installer
says that, when it is used instead of `--guest-cpus`. The two may be set
together on a v2 host (a set, and a ceiling inside it).

ADR-0138's preflight compares `--guest-cpu-quota` the way it compares
cpusets: quotas on one host must sum to no more than the host's CPUs
(100% per hardware thread).

### 4. Hardening gaps are named, and not silent

Every unit directive that has a minimum systemd version is a named constant
with that version, beside its test (as every hardening property already is).
Today the only one above 239 is `PrivateIPC=` (248). On a host older than a
directive's version the installer lists the directives that **will not be
enforced** and refuses to continue unless the operator passes
`--accept-reduced-hardening` with the same list (`PrivateIPC`), so the
acceptance is a recorded, reviewable statement of what the host lacks.

A re-run is not a new acceptance. The installer keeps the host config it wrote
(ADR-0067), and the accepted list lives in it: with no flag on a re-run, the kept
acceptance stands, but only for directives the host **still** lacks, so a systemd
upgrade quietly retires an entry and a directive the host newly lacks is not covered
(the preflight refuses and asks for the flag). A flag given on a re-run replaces the
kept list; it is not a contradiction of it, because this section of the config is
managed, unlike storage and network classes, and `install --force` ignores the kept
list. A host config from before this ADR has no `[hardening]` section and means
nothing is accepted.

What the gap is, measured: with `ReadOnlyPaths=/dev`, `/dev/shm` stays
read-only on 239, so the file route between two escaped VMMs that ADR-0063
closed is still closed. What stays shared is SysV IPC and POSIX message
queues. Whether a partial compensation is worth having on such a host
(`InaccessiblePaths=/dev/mqueue`, or a `SystemCallFilter` that denies the
SysV IPC calls, which the VMM must be shown not to need) is an open question
below. The accepted list is written into the host config, read
back by the provider, and published in `Provider.status`, so a scheduler or an
operator can see which providers run with a weaker guest sandbox.

### 5. What does not change

Unit templates, the polkit rule, the uid range's meaning, the slice name,
the guest's private group and the provider's lack of any right over the slice
are exactly as in ADR-0063 and ADR-0105. `files` mode and `userdb` mode are
indistinguishable to a guest and to the provider. Nothing here edits
`nsswitch.conf`, PAM or `authselect` state.

## Consequences

- RHEL 8 and similar hosts can run both providers, with a quota-based CPU
  budget and memory ceilings that work, and a hardening gap that is visible.
- The `files` mode adds up to 2048 lines to the host's account databases, as
  system accounts (no login). Removing them is `userdel` and `groupdel` over
  the range; the installer's undo step does that, and only for names with the
  `banlieue-g` prefix inside the declared range.
- CPU isolation on cgroup v1 is a quota, not exclusive cores. For the
  Firecracker and Cloud Hypervisor split (ADR-0138) it is a fair share, not
  a guarantee against cache or SMT contention. Hosts where that matters
  should be cgroup v2 hosts.
- A second code path exists for account registration; both are covered by the
  same fake-host tests and one real run per mode.
- Threat model: new write to the host's identity databases (TB host
  integrity), a new flag that weakens a named control by consent (the
  acceptance is recorded, never implied). Pass due when implemented.
- Not implemented. Open: whether to compensate for `PrivateIPC=` on old
  hosts (see Decision 4); a minimum-version table for any directive added to
  the templates later (the current set is fully measured, above); and whether the installer should refuse `files` mode on a host that uses a
  directory service that cannot enumerate system accounts.
