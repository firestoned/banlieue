<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0063 — Host supervision: systemd units over D-Bus

- **Status:** Accepted
- **Date:** 2026-09-27
- **Proposed:** 2026-09-25
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-09-26 (Decision 4: taps by tun/bridge ioctls, not
  `rtnetlink`, and one digit longer for the NIC index; Decision 5: machine
  and run directories are `2770` guest-uid:banlieue under `0711` roots;
  Decision 3: guest uids are registered with NSS as userdb records, each
  with a private group — all found while implementing, the last two on the
  first live run); 2026-09-27 (Decision 1: **root-owned template units, not transient
  units** — a security fix; see *Amendment 2026-09-27*); 2026-09-30
  (Decision 3, **proposed**: guest memory is bounded by one slice for all
  guests, not a `MemoryMax` per guest — a per-guest limit throttles the
  guest's own disk writes; see *Amendment 2026-09-30*)
- **Related:** [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (host-resident provider), [ADR-0061](0061-banlieue-cloud-hypervisor-vmm-client.md)
  (the socket this ADR places), [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md)
  (machine identity and deletion order),
  [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md) (the swtpm
  unit); roadmap 09 phase 3.

## Context

Something on the host must start one `cloud-hypervisor` process per guest,
and one `swtpm` per `tpmEnabled` guest. It must also keep them running,
limit their resources, create their tap devices, and tear everything down
on delete. Roadmap 09's stop condition adds a hard requirement: **restarting
or upgrading the provider must not disturb a running guest.**

Options:

1. **Child processes of the provider.** They die with the provider, and an
   upgrade restarts every guest. Fails the stop condition outright.
2. **`systemd-run`.** A subprocess in the reconcile path (ADR-0011), and its
   output is text to parse.
3. **Unit files written to `/etc/systemd/system`.** The provider writes to
   system configuration, a crash can leave stale files, and units then
   survive a host reboot on their own, disagreeing with the cluster's
   desired state.
4. **Transient units created with `StartTransientUnit` over D-Bus.** systemd
   owns the processes; the provider asks for them by name, with typed
   properties, over an API. They vanish on host reboot, and the reconciler
   recreates what the cluster says should run.

## Amendment 2026-09-27: root-owned template units, not transient units

**Why.** polkit's `manage-units` action tells a rule only the unit name and
the verb. With transient units, the provider *describes* the unit it
starts, so whoever holds the provider's user could call
`StartTransientUnit` with an allowed name (`banlieue-ch-<uuid>.service`)
and `User=root`, `ExecStart=` anything: the `banlieue` user was root on the
host. Found by review during roadmap 09, before any release; verified
closed on a host (below).

**Decision.** The provider starts instances of four **template units**
that the bootstrap installs root-owned in `/etc/systemd/system`
(`deploy/provider-cloud-hypervisor/host/`):

| Template | Instance | Runs as |
| --- | --- | --- |
| `banlieue-ch@.service` | guest host uid | `User=%i`, `Group=%i`, `kvm` |
| `banlieue-swtpm@.service` | guest host uid | `User=%i`, `Group=%i` |
| `banlieue-swtpm-setup@.service` | guest host uid | `banlieue` (reads the EK CA key) |
| `banlieue-ch-import@.service` | `VMImage` UID | `banlieue` |

What runs, as whom, and in which sandbox is in the file; the provider
chooses only the instance. Per-guest paths are therefore keyed by the
guest's host uid, which the template derives from `%i`: the run directory
is `<run root>/<uid>/`, TPM state `<state root>/tpm/<uid>/`. The machine
directory stays `<storage class>/<machine UID>/`, reached through
`ReadWritePaths=` on the storage roots and confined by its `2770` mode. The
two templates that run as the provider's own user read their
machine-specific arguments (`--vmid`, the image reference) from an
`EnvironmentFile=` the provider writes in `<state root>/units/` (`0700`);
values are refused unless they are single safe words. That grants nothing
the provider's user does not already have.

The polkit rule allows `start`, `stop` and `reset-failed` on
`banlieue-(ch|swtpm|swtpm-setup)@<uid>.service` **only for a uid inside the
guest range** (rendered from the host's `[guests]`), and on
`banlieue-ch-import@<uuid>.service`, plus `set-property` on VMM instances
for their runtime `MemoryMax`. Two systemd behaviours make this sound,
both checked on a host: `StartTransientUnit` refuses a name that has a unit
file ("already loaded or has a fragment file"), so an allowed name cannot
be reused for a transient unit; and `SetUnitProperties` on a unit with a
file accepts only resource-control properties (`User`, `ExecStart`,
`Environment`, `ReadWritePaths` are refused).

Option 3 was rejected for files *the provider* writes. These are written by
the bootstrap, as root, once; the provider cannot write
`/etc/systemd/system`.

**Verified 2026-09-27, as the `banlieue` user on a bootstrapped host:** a
transient unit under the old name with `User=root`, a transient unit under
a template instance name, `banlieue-ch@0.service`, `set-property User=root`,
`set-property` on a swtpm instance and stopping `cron.service` are all
refused; `set-property MemoryMax` on a guest instance is allowed. The
policy is also exercised by `scripts/test-cloud-hypervisor-polkit.js` (22
cases), and `make ch-e2e` and `make ch-vtpm-e2e` pass on the templates.

**Consequences.** Adding a unit kind means a new template in the
bootstrap, not only code. Host-reboot behaviour is unchanged: instances
are not enabled, so nothing starts on boot until the provider starts it.
`BindsTo=` between the VMM and swtpm (Decision 1) was never implemented;
the provider orders them (swtpm, then its socket, then the VMM) and stops
the VMM first.

## Amendment 2026-09-30: one memory bound for all guests, not one per guest

*Proposed 2026-09-30; accepted when implemented.*

**Why.** Decision 3 gave every VMM unit a `MemoryMax` of guest memory plus
a 512 MiB overhead. A cgroup memory limit appears to do more than cap
memory: the kernel sizes that cgroup's dirty page-cache allowance from its
own budget instead of the host's. (Inferred from the measurements below,
which fit it; not traced in the kernel.) The VMM writes guest disks through the host
page cache (its default, and the fast path on these hosts), so with a
per-guest limit a guest writing a few hundred megabytes is throttled by its
own unit long before memory is anywhere near the limit.

Measured on one host (2026-09-29/30), one Debian 13 guest, 2 vCPU, 4 GiB,
the VMM started exactly as the provider starts it, eight rounds of the
`make provider-bench` disk workload per boot, two boots per row:

| Memory limit on the VMM's cgroup | 4 KiB direct writes, collapsed rounds | Typical |
| --- | --- | --- |
| None | 0 of 16 | 50–82 MB/s |
| `MemoryMax` = guest + 512 MiB (Decision 3) | 8 of 14 (5–37 MB/s) | 49–79 MB/s |
| `MemoryMax` = guest + 1.5 GiB / + 2 GiB (one boot each) | 2 / 1 of 8 | 65–84 MB/s |
| `MemoryHigh` = guest + 512 MiB, `MemoryMax` = guest + 2 GiB (one boot) | 4 of 8 | 69–84 MB/s |
| None on the guest; `MemoryMax` = 48 GiB on a parent slice | **0 of 16** | 62–83 MB/s |

In every limited run where they were captured, the cgroup recorded no
`high` or `max` events, and the guest peaked at 1.85 GB against a 4.5 GiB
limit: the limit was never reached, and its presence alone produced the
stalls. Sequential 1 MiB writes collapsed the same way (250–350 MB/s in
those rounds, 500–900 otherwise). Two result lines were lost on the serial
console in the 512 MiB boots, hence 14. Opening the disk `O_DIRECT` instead (`direct=on`) avoids the page
cache and was worse everywhere (4 KiB writes 3–6 MB/s, reads about 10 MB/s).
This is what `make provider-bench` recorded as 8.2 MB/s for Cloud
Hypervisor on 2026-09-27.

**Decision.**

1. The guest templates (`banlieue-ch@.service`, `banlieue-swtpm@.service`)
   run in **`banlieue-guests.slice`**, a root-owned slice unit that
   `banlieue host install` writes beside them (`Slice=` in the template, so
   the provider cannot choose another).
2. The slice carries the one hard bound: `MemoryMax` = the host's **guest
   memory budget**, a host setting `banlieue host install` renders into the
   slice. It is explicit: printed by `banlieue host status`, and set by
   the operator or, if they leave it unset, computed once at install from
   the host's memory minus a named reserve for the host itself, and written
   down, never recomputed behind the operator's back.
3. **No per-guest `MemoryMax`.** The provider stops calling
   `SetUnitProperties`, and the polkit rule loses the `set-property` grant
   the *Amendment 2026-09-27* added for it. The provider's authority on the
   host shrinks to `start`, `stop` and `reset-failed`.
4. Unchanged: `TasksMax` per guest, hugepage accounting, and guest memory
   itself, which the VMM fixes at `vm.create` (`--memory size`) and the
   guest cannot grow.

**What is given up.** Decision 3's per-guest bound limited a VMM process
that leaked or was compromised to its own guest's memory plus 512 MiB.
Under the slice, such a process can take memory from the other guests on
the host until the slice limit, and can no longer take it from the host
itself or its services. The guest's RAM is unaffected: it is set by the
VMM, not by the cgroup. A compromised VMM already runs as an unprivileged
uid under seccomp and Landlock (Decisions 3 and 5), which is what keeps it
from the other guests' files and processes; memory contention between
guests on one host becomes a noisy-neighbour concern rather than a
hard-isolated one. The shared budget is exactly what removes the stalls,
so this trade is the fix, not a side effect of it.

**Rejected.** More headroom per guest only moves the cliff (+2 GiB still
collapsed) and reserves memory that is mostly idle. `MemoryHigh` per guest
collapsed as often (4 of 8). `direct=on` removes the page cache and was
worse. Host-wide `vm.dirty_*` tuning was not tried: it would change every
workload on the host to fix one.

**Follow-ups, in the implementing change.** The slice unit and the budget
setting in `banlieue-host`; `Slice=` in both guest templates; the
per-guest `MemoryMax` removed from `plan.rs` and from the polkit rule and
its test cases; the CALM host node; the threat model's cross-guest rows and
the polkit row; `docs/src/guides/cloud-hypervisor-host-systemd.md`; and a
`make provider-bench` rerun to replace the Cloud Hypervisor column in
`docs/src/reference/provider-comparison.md`.

## Decision

### 1. Option 4, with `zbus`

> **Amended 2026-09-27: superseded by template units** — see *Amendment
> 2026-09-27* below. The text of this decision is kept as the record of
> what was built first.

One transient unit per guest, **`banlieue-ch-<machine-uid>.service`**,
created with `org.freedesktop.systemd1.Manager.StartTransientUnit` through
`zbus` (pure Rust, no libsystemd). For `tpmEnabled`, a sibling
**`banlieue-swtpm-<machine-uid>.service`**. The VMM unit carries
`BindsTo=` and `After=` on it, so the TPM is up before the VMM and a dead
swtpm takes the VMM down with it rather than leaving a guest with no TPM.

The unit set lives only in systemd's runtime state, so a host reboot clears
it. After a reboot the provider starts the machines whose
`desiredPowerState` is `On`, from the CRs. Kubernetes, not the host, is the
source of truth.

### 2. The provider re-adopts and never trusts its memory

On start, and on every resync, the provider lists units matching
`banlieue-ch-*` and `banlieue-swtpm-*` (`ListUnitsByPatterns`) and matches
them to machines by UID.

- A unit whose machine exists is **adopted**: no restart, no reconfigure.
- A unit whose machine is confirmed gone (a `404` from the API server, not
  a cache miss) is torn down through the normal deletion path
  (ADR-0062 Decision 8).
- A machine with no unit is started if it should be running.

It keeps no list of its own of what is running.

### 3. Each guest runs as its own unprivileged uid

Both units run as a per-guest uid from a range declared in the host config
(ADR-0062 Decision 4), recorded on `status.hostUid` and re-derived from the
unit's `User=` on adoption. They get `SupplementaryGroups=kvm` and no
capabilities.

*Amended 2026-09-26, from the first live run.* systemd refuses `User=` and
`Group=` for a numeric id that NSS cannot resolve: the unit fails before
exec with status 217/USER. The bootstrap therefore registers the whole
range as systemd userdb drop-ins in `/etc/userdb` (`nss-systemd`), one user
`banlieue-g<uid>` and one private group with the same number per guest. The
unit's primary group is that private group (`Group=<uid>`), never the
provider's: with a shared group every guest could open every other guest's
files. The default range shrinks to 1024, a realistic ceiling per host, so
the registration stays small.

Unit hardening, each property a named constant with a test:
`NoNewPrivileges=yes`, `ProtectSystem=strict`, `ProtectHome=yes`,
`PrivateTmp=yes`, `ReadWritePaths=` limited to the machine's own directory
under its storage target and its run directory,
`DeviceAllow=/dev/kvm rw` and `DeviceAllow=/dev/net/tun rw`. (`UMask=0007`
was listed here; it was removed on 2026-09-26 because the VMM overrides its
umask — see Decision 5.) Every setting, and why it is needed, is in
`docs/src/guides/cloud-hypervisor-host-systemd.md`; the unit is rendered to
`deploy/provider-cloud-hypervisor/host/banlieue-ch-guest.example.service`,
which a test keeps equal to what the provider sends.

cgroup limits come from the spec: `MemoryMax` is guest memory plus a named
overhead constant, `TasksMax` is bounded, and hugepages are accounted
separately. (*Amended 2026-09-30, proposed:* the per-guest `MemoryMax` is
replaced by one bound on `banlieue-guests.slice`, because a per-guest limit
throttles the guest's own disk writes; see *Amendment 2026-09-30*.) The VMM runs with `--seccomp true` and `--landlock`.

### 4. Taps are created by the provider, with the tun and bridge ioctls

The provider creates each tap persistent (`TUNSETIFF`, `TUNSETOWNER` to the
guest uid, `TUNSETPERSIST`), enslaves it to the bridge the network class
resolves to (`SIOCBRADDIF`) and brings it up (`SIOCSIFFLAGS`). The VMM opens
the tap by name and never holds `CAP_NET_ADMIN`.

*Amended 2026-09-26.* This originally said `rtnetlink`. Netlink cannot set
a tap's owner or make it persistent — those are `/dev/net/tun` ioctls — so
the provider needed the ioctls regardless, and the remaining two operations
are one ioctl each. Doing all four the same way drops a dependency tree
(`rtnetlink`, `netlink-*`) for about forty lines of `libc`, confined to
`crates/banlieue-provider-cloud-hypervisor/src/sys.rs`, the crate's only
`unsafe`, and exercised in a user+network namespace by `tests/live_sys.rs`.

*Also amended 2026-09-26, from the first live run.* The provider creates a
tap only when it is absent and never attaches to an existing one
(`sys::ensure_tap`): the VMM holds a single-queue tap's only queue, so a
per-pass re-attach fails with `EBUSY` once a guest is running. Bridge
membership and link state are still repaired every pass, without attaching.

With `--landlock` on, Cloud Hypervisor v53 cannot open a tap at all: it
reads the tap's flags from sysfs and Landlock denies that, so the network
device silently fails and drops out of the VM's configuration (the guest
boots with no NIC). `vm.create` therefore adds one read-only Landlock rule
per tap, on `/sys/devices/virtual/net/<tap>` — the resolved path, since
Landlock does not match through the `/sys/class/net` symlink — and nothing
wider (`banlieue-cloud-hypervisor` `types.rs`). Landlock stays on.

Tap names are `bch<10 hex digits from the UID><NIC index>`, 14 characters,
under Linux's 15-character interface name limit. (The original 13 had no
room for a second NIC.)

### 5. Sockets and files: the layout is the access control

```text
/run/banlieue/ch/                banlieue:banlieue 0711
/run/banlieue/ch/<uid>/          guest-uid:banlieue 2770
    api.sock                     guest-uid:banlieue 0660  (VMM API, ADR-0061)
    swtpm.sock                   guest-uid:guest-uid 0600 (ADR-0065)
    vsock.sock*                  (ADR-0065)
<storage-target>/                banlieue:banlieue 0711
<storage-target>/images/         banlieue:banlieue 0750   (image cache)
<storage-target>/<uid>/          guest-uid:banlieue 2770
    os.raw  seed.iso  data-N.raw  install.iso (Deferred only)
    tpm/                         swtpm state (ADR-0065)
    serial.log                   0600
```

Only the guest's own uid and the `banlieue` group (the provider) can reach
the API socket. Another guest's uid cannot traverse the directory.

*Amended 2026-09-26.* The directories were `0750` and `0700`
guest-uid:guest-uid. Neither lets the provider tear a machine down: removing
a file needs write on its directory, and the provider is not the guest's
uid. `2770` with the `banlieue` group gives the provider exactly that and
still shuts out every other guest, since a guest's groups are its own
private group and `kvm`, never `banlieue` (Decision 3). The setgid bit puts
what the VMM creates — the API socket, the serial log — in the `banlieue`
group, and the provider opens the socket to that group (below), so it
reaches it without sharing a group with the guest. The roots are `0711`: a guest
traverses to its own directory and cannot list the others.

*Also found on the first live run:* Cloud Hypervisor v53 sets its own umask
to `0077`, overriding `UMask=`, so its API socket is always `0700` and the
provider cannot connect as the group. The provider, which holds
`CAP_FOWNER`, sets it to `0660` itself — but only after checking, on the
inode it opened with `O_PATH | O_NOFOLLOW`, that it is a socket owned by the
guest uid, in the `banlieue` group, granting nothing to others, and it
changes that same inode through `/proc/self/fd`. The guest owns the run
directory and could plant a symlink there; with `CAP_FOWNER` a path-based
chmod would let it retarget the change at any file on the host.
(`hostfs::grant_api_socket`.) The same rule governs every file the
provider changes inside a guest's directory (security audit, 2026-09-26):
disks and seeds are created `O_CREAT|O_EXCL`, then grown, owned and
re-moded through that open handle (`fchown`, `fchmod`, `set_len`), never
by path; directories are opened `O_NOFOLLOW|O_DIRECTORY` before being
re-owned; and an existing seed is read `O_NOFOLLOW`. The serial log is likewise `0600` to the
guest; the provider does not read it and removes it through its directory
write. The
client checks ownership and mode before every connect (ADR-0061
Decision 6).

### 6. The provider's own privileges

The provider runs as a dedicated `banlieue` system user, not root, with:

- `AmbientCapabilities=CAP_NET_ADMIN CAP_CHOWN CAP_FOWNER` — taps, handing
  files to guest uids, and re-moding guest-owned files (the API socket,
  Decision 5);
- a **polkit rule** that lets the `banlieue` user start, stop and
  reset-failed only instances of its own root-owned templates, for uids in
  the guest range (*amended 2026-09-27*, see the amendment above; listing
  needs no authorization). Originally: units named
  `banlieue-ch-<uuid>.service`. *Amended 2026-09-26:* this said `banlieue-swtpm-` too;
  the rule now allows only what the provider uses, and each feature widens
  it in the change that needs it;
- read-only access to the host config and kubeconfig, and write access only
  to the run root and the storage targets.

The unit, the rule and the tmpfiles entry are files in
`deploy/provider-cloud-hypervisor/host/`, explained setting by setting in
`docs/src/guides/cloud-hypervisor-host-systemd.md`.

Anything else a bug or a stolen credential might try — another unit, a
file outside those roots — needs privileges the process does not have.

### 7. Stopping a guest

`vm.power-button`, a bounded wait for the guest to shut down, then
`vm.shutdown` and `vmm.shutdown`, then stop the unit. The swtpm unit stops
through `BindsTo=` or explicitly. Each step is verified: the unit is
inactive and the process has gone.

## Consequences

**Positive**

- Guests outlive provider restarts and upgrades. That meets roadmap 09's
  stop condition by construction.
- No subprocess anywhere in the reconcile path, and no files in
  `/etc/systemd`.
- Every guest is isolated by uid, cgroup, seccomp and Landlock. A VMM
  escape lands as an unprivileged uid, not as the provider.
- The provider itself is not root, and its polkit grant covers only its
  own units.

**Negative / accepted costs**

- A new host-level dependency: systemd with D-Bus and polkit. Accepted: the
  target hosts are systemd distributions, and the bootstrap script checks.
- `zbus` is a new dependency (`rtnetlink` was, before the Decision 4
  amendment). It must pass `cargo deny`
  and the maintenance check before landing.
- A uid range per host is one more host config value that has to be
  managed.
- Transient units do not restart on host boot until the provider has
  started. Accepted: the cluster decides what runs.

**Follow-ups**

- The spike checks the whole hardened property set against a real Kairos
  boot, since some of it may be too strict for the VMM.
- The threat model: the host trust boundary (ADR-0060), the polkit rule,
  and uid-per-guest isolation.
- Bootstrap script: the `banlieue` user, the polkit rule, the uid range and
  the provider's own systemd unit.
