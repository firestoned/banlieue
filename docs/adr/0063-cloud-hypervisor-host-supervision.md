<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0063 — Host supervision: transient systemd units over D-Bus

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Erick Bourgeois
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

## Decision

### 1. Option 4, with `zbus`

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

Unit hardening, each property a named constant with a test:
`NoNewPrivileges=yes`, `ProtectSystem=strict`, `ProtectHome=yes`,
`PrivateTmp=yes`, `ReadWritePaths=` limited to the machine's own directory
under its storage target and its run directory,
`DeviceAllow=/dev/kvm rw` and `DeviceAllow=/dev/net/tun rw`, `UMask=0007`.

cgroup limits come from the spec: `MemoryMax` is guest memory plus a named
overhead constant, `TasksMax` is bounded, and hugepages are accounted
separately. The VMM runs with `--seccomp true` and `--landlock`.

### 4. Taps are created by the provider, over netlink

`rtnetlink` creates the tap, sets its owner to the guest uid, enslaves it to
the bridge the network class resolves to, and brings it up. The VMM opens
the tap by name and never holds `CAP_NET_ADMIN`.

Tap names are `bch<10 hex digits from the UID>`, 13 characters, under
Linux's 15-character interface name limit.

### 5. Sockets and files: the layout is the access control

```text
/run/banlieue/ch/<uid>/          guest-uid:banlieue 0750
    api.sock                     guest-uid:banlieue 0660  (VMM API, ADR-0061)
    swtpm.sock                   guest-uid:guest-uid 0600 (ADR-0065)
    vsock.sock*                  (ADR-0065)
<storage-target>/<uid>/          guest-uid:guest-uid 0700
    os.raw  seed.iso  data-N.raw  install.iso (Deferred only)
    tpm/                         swtpm state (ADR-0065)
    serial.log                   0600
```

Only the guest's own uid and the `banlieue` group (the provider) can reach
the API socket. Another guest's uid cannot traverse the directory. The
client checks ownership and mode before every connect (ADR-0061
Decision 6).

### 6. The provider's own privileges

The provider runs as a dedicated `banlieue` system user, not root, with:

- `AmbientCapabilities=CAP_NET_ADMIN CAP_CHOWN CAP_FOWNER` — taps, and
  handing files to guest uids;
- a **polkit rule** that lets the `banlieue` user start, stop and query only
  units whose names begin with `banlieue-ch-` or `banlieue-swtpm-`;
- read-only access to the host config, and write access only to the run
  root and the storage targets.

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
- `zbus` and `rtnetlink` are new dependencies. They must pass `cargo deny`
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
