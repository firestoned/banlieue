<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Cloud Hypervisor host: systemd, polkit and identities

The Cloud Hypervisor provider runs **on the KVM host**, and every guest is a
systemd unit on that host (ADR-0060, ADR-0063). This page explains every
piece of host configuration involved: what it is, why it is there, and what
breaks without it. Nothing on this page is optional; anything that turned out
not to be needed has been removed (see
[Deliberately not set](#deliberately-not-set)).

The files live in the repository as templates, rendered and installed by
`scripts/bootstrap-cloud-hypervisor-host.sh`
([bootstrap guide](cloud-hypervisor-host.md)):

| File in `deploy/provider-cloud-hypervisor/host/` | Installed as | What it is |
| --- | --- | --- |
| `banlieue-provider-cloud-hypervisor.service` | `/etc/systemd/system/` | The provider itself |
| `60-banlieue-cloud-hypervisor.rules` | `/etc/polkit-1/rules.d/` | What the provider may ask systemd to do |
| `banlieue-cloud-hypervisor.tmpfiles.conf` | `/etc/tmpfiles.d/banlieue-cloud-hypervisor.conf` | The run root, recreated every boot |
| `banlieue-ch@.service` | `/etc/systemd/system/` | One VMM per guest, instance = guest uid |
| `banlieue-swtpm@.service` | `/etc/systemd/system/` | One swtpm per `tpmEnabled` guest |
| `banlieue-swtpm-setup@.service` | `/etc/systemd/system/` | Manufactures a guest's vTPM, once |
| `banlieue-ch-import@.service` | `/etc/systemd/system/` | Pulls one registry image, instance = VMImage UID |

The last four are **templates, installed root-owned**. The provider only
starts, stops and resets their instances; what an instance runs, as whom,
and in which sandbox is in the file (ADR-0063, amended 2026-09-27). Earlier
versions created *transient* units, which the provider described itself;
because polkit sees only a unit's name, that let the provider's user start a
unit as root. `systemd_tests.rs` reads these files and checks their
settings.

Two more things the bootstrap sets up are not files in `deploy/`: the
[guest identities](#guest-identities-etcuserdb) in `/etc/userdb`, and the
[directory layout](#directories-and-permissions).

## The provider unit

`banlieue-provider-cloud-hypervisor.service` runs
`banlieue provider cloud-hypervisor --config /etc/banlieue/cloud-hypervisor.toml`
as the unprivileged `banlieue` user.

| Setting | Why |
| --- | --- |
| `Wants=`/`After=network-online.target` | It dials the Kubernetes API at start. |
| `After=dbus.service` | It talks to systemd over D-Bus to start guests. |
| `After=systemd-tmpfiles-setup.service` | The run root must exist (see [tmpfiles](#the-run-root-tmpfiles)). |
| `ConditionPathExists=` binary and kubeconfig | A half-installed host stays stopped instead of crash-looping. The kubeconfig arrives separately, from `banlieue bootstrap cloud-hypervisor-host`. |
| `User=`/`Group=banlieue` | Never root. |
| `AmbientCapabilities=` / `CapabilityBoundingSet=CAP_NET_ADMIN CAP_CHOWN CAP_FOWNER` | The only three privileges it holds, and each is used: **`CAP_NET_ADMIN`** creates each guest's tap, sets its owner and persistence, adds it to the bridge and brings it up. **`CAP_CHOWN`** hands machine directories, OS disks and seeds to the guest's uid. **`CAP_FOWNER`** re-modes files the guest owns — its directories each pass, and the VMM's API socket, which Cloud Hypervisor always creates `0700` (see [the API socket](#the-vmms-api-socket)). |
| `NoNewPrivileges=yes` | Nothing it runs can gain privileges. |
| `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes` | The whole filesystem is read-only to it except `ReadWritePaths`; home directories are invisible. |
| `ReadWritePaths=` run root + each storage class directory + `/var/lib/banlieue/{ek,tpm,units}` + `/etc/banlieue/credentials` | Where it creates and removes per-guest run and machine directories (and, after each import, evicts superseded cache files), each vTPM's state and EK certificate directories, the template units' environment files, and where it **replaces its own token** at half-life (ADR-0060 Decision 5). It reads its config and the firmware; it writes nothing else. |
| `KillMode=process` | Stopping, restarting or upgrading the provider must **not** stop guests. They are separate units owned by systemd (ADR-0063 Decision 1); the provider re-adopts them when it comes back. |
| `Restart=on-failure`, `RestartSec=5s` | A crash is retried without a tight loop. |

## The guest unit

For each machine the provider starts `banlieue-ch@<guest uid>.service`, an
instance of `banlieue-ch@.service`, after setting its `MemoryMax` at runtime
(guest memory + 512 MiB). The instance is the guest's host uid, so the
template derives everything from it: `User=%i`, `Group=%i`, and the run
directory `/run/banlieue/ch/<uid>/`. On a host, `systemctl cat
banlieue-ch@.service` shows the template and `systemctl list-units
'banlieue-ch@*'` the live guests. The VMM starts with only its API socket;
the provider then creates and boots the VM through that socket.

| Setting | Why | What happens without it |
| --- | --- | --- |
| `CollectMode=inactive` | A **failed** guest stays loaded, so the provider can read why (`Result`, exit status) and report it as `Ready=False, VmmExited` before clearing it. | With `inactive-or-failed` systemd discarded a failed unit at once; the first live run became a silent 3-second restart loop with status stuck at "waiting for its API". |
| `ExecStart=cloud-hypervisor --api-socket path=…` | The VMM, idle until the provider configures it. | — |
| `User=<guest uid>` | Each guest is its own user, from the host's guest range. | Guests would share an identity and could reach each other's files. |
| `Group=<guest uid>` | Each guest also has its **own** group (gid = uid). Never the provider's group. | With the provider's group, every guest could open every other guest's disk and API socket through the group bits. |
| `SupplementaryGroups=kvm` | `/dev/kvm` is `root:kvm 0660`. | The VMM cannot create a VM. |
| `MemoryMax=` guest memory + 512 MiB | Caps the VMM's memory in its cgroup; the overhead covers the VMM itself. | A runaway VMM can take the host's memory. |
| `TasksMax=1024` | Caps processes and threads. | A runaway VMM can fork the host out of PIDs. |
| `NoNewPrivileges=yes` | Nothing in the VMM can gain privileges. | — |
| `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes` | The filesystem is read-only except its own two directories; home directories are invisible; `/tmp` is private. | A compromised VMM could write anywhere its uid can. |
| `ReadWritePaths=` its run directory and the storage roots | A template cannot name the machine directory (`<storage class>/<machine UID>/`), so the storage roots are listed; each machine directory is `2770 guest:banlieue` under a `0711` root, so the guest can write only its own. | — |
| `DevicePolicy=closed` + `DeviceAllow=/dev/kvm rw`, `/dev/net/tun rw` | Only the two devices it needs, plus the standard pseudo-devices (`/dev/null`, `/dev/urandom`, …) that `closed` always allows. | Every device node its uid can open would be open to it. |

The VMM also sandboxes itself: seccomp filters per thread (on by default) and
**Landlock** (`landlock_enable`, set by the provider). Landlock denies reading
a tap's flags from sysfs, which the VMM does when it opens the tap, so the
provider adds one read-only Landlock rule per tap, on
`/sys/devices/virtual/net/<tap>` — nothing wider. Without it the network
device silently drops out of the VM and the guest boots with no NIC.

## The vTPM units

A `tpmEnabled` machine (ADR-0065) gets two more units, instances named by
the guest's uid:

| Unit | Runs as | Does |
| --- | --- | --- |
| `banlieue-swtpm-setup@<uid>.service` | the provider's user (`banlieue`) | Once, before the guest ever starts: `swtpm_setup` creates the TPM in `/var/lib/banlieue/tpm/<uid>/`, signs its EK and platform certificates with the host's `swtpm_localca` (`--vmid <machine-name>:<machine UID>` becomes the EK's CN), and writes the EK certificates to `/var/lib/banlieue/ek/<machine UID>/`. The two machine-specific values come from `/var/lib/banlieue/units/swtpm-setup-<uid>.env`, which the provider writes (`0600`, single safe words only). Writable: the state, the EK directories and the CA directory (its serial number). |
| `banlieue-swtpm@<uid>.service` | the guest's uid and private group | `swtpm socket --tpm2 --tpmstate dir=/var/lib/banlieue/tpm/<uid> --ctrl type=unixio,path=/run/banlieue/ch/<uid>/swtpm.sock`, started before the VMM, which is given that socket. Writable: its state and run directories. |

`/var/lib/banlieue` is `0751` so a guest can reach its own `tpm/<uid>/`
(`tpm/` is `0711`); everything else there is `0700`. Guest uids are reused
after a machine is deleted, so the provider clears `tpm/<uid>/` just before
a fresh manufacture, and only then.

Why manufacture is not the guest's: signing reads the CA's private key,
and any uid that can read it can mint EK certificates this host vouches
for. Why the EK certificates live under `/var/lib/banlieue/ek/`: the guest's
uid owns its machine directory and could swap a certificate there for its
own. That directory is also how the provider knows a TPM was already made,
so deleting its own state never gets a guest a second manufacture.

Between the two units the provider hands the state to the guest's uid,
file by file through handles opened without following links. Power-off and
delete stop swtpm after the VMM; delete removes the state and the EK
directory and checks both are gone.

The host offers `vtpm` only when the host config has a `[tpm]` section and
`swtpm`, `swtpm_setup`, the setup configuration and the CA certificate all
exist, and the `Provider` declares `vtpm` in its features. The CA
certificate is published on `Provider.status.ekCaCertificates`.

## The image import unit

For a `Url` source (ADR-0064) the provider starts
`banlieue-ch-import@<vmimage uid>.service`, which runs this binary's
`provider cloud-hypervisor import` and exits. The reference and cache file
name come from `/var/lib/banlieue/units/import-<vmimage uid>.env`. The pull runs in its own unit
so a multi-GB download is neither inside the reconcile loop nor lost when
the provider restarts.

| Setting | Why |
| --- | --- |
| `User=`, `Group=` the provider's own | It writes the provider's image cache; no guest identity is involved. |
| `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `NoNewPrivileges=yes` | As for guests. |
| `ReadWritePaths=` each storage class's `images/` | The only place it writes: the cache files, through a temporary name and a rename. |
| `DevicePolicy=closed`, no `DeviceAllow` | It needs no device. |
| `CollectMode=inactive` | A failed import stays loaded until the provider has reported why (`ImportFailed`) and cleared it; a successful one is collected. |

It reads the host config and, if the registry needs them, the `username`
and `password` files in `[registry] credentials_dir`
(`/etc/banlieue/registry`, `0750 root:banlieue`). It re-checks its command
line against the config: the reference must be a digest in the configured
repository and the file its cache name.

## polkit: what the provider may ask systemd

Starting a system unit that runs as another user is a privileged D-Bus call.
`60-banlieue-cloud-hypervisor.rules` allows exactly this, for the `banlieue`
user only:

| Allowed | Why |
| --- | --- |
| instances `banlieue-ch@<uid>`, `banlieue-swtpm@<uid>`, `banlieue-swtpm-setup@<uid>` **with `<uid>` in the guest range**, and `banlieue-ch-import@<uuid>` (`.service`) | Only its own guests, their vTPMs and image imports. The range check is what keeps `banlieue-ch@0` (root) out; the bootstrap renders the range from `[guests]`. |
| verb `start` | `StartUnit`, to start an instance. |
| verb `stop` | `StopUnit`, on power-off and delete. |
| verb `reset-failed` | `ResetFailedUnit`, to clear a failed unit after reporting it. |
| verb `set-property`, VMM instances only | `SetUnitProperties` for the runtime `MemoryMax`; systemd accepts only resource limits on a unit with a file. |

Two systemd behaviours make this sound, both checked on a host: a transient
unit cannot take a name that has a unit file, so an allowed name cannot be
reused with the provider's own `User=`; and `set-property` refuses `User`,
`ExecStart`, `Environment` and `ReadWritePaths`. `node
scripts/test-cloud-hypervisor-polkit.js` (`make ch-polkit-test`) runs the
rule against 22 cases.

Listing units needs no authorization. A feature that needs more widens the
rule in the same change that uses it.

## The credentials directory

`/etc/banlieue/credentials/` is `0700 banlieue`, created by the bootstrap
and filled by `banlieue bootstrap cloud-hypervisor-host`:

| File | Mode | What |
| --- | --- | --- |
| `kubeconfig` | `0600` | The cluster's server and CA, and a user with `tokenFile: /etc/banlieue/credentials/token`. No secret in it; it never changes. |
| `token` | `0600` | A bound ServiceAccount token. The provider requests a new one at half its lifetime and renames it over this file; the kube client re-reads it within a minute. |

The provider's Role lets it create tokens for **its own** ServiceAccount
only. A host down for longer than one token lifetime must be issued a new
credential. Revoking a host is deleting its ServiceAccount: every token
bound to it stops working at once, and the operator recreates the account.

## The run root (tmpfiles)

`/run` is a tmpfs, emptied at every boot. `banlieue-cloud-hypervisor.conf`
recreates:

| Path | Mode | Why |
| --- | --- | --- |
| `/run/banlieue` | `0755 root:root` | Parent. |
| `/run/banlieue/ch` | `0711 banlieue:banlieue` | The provider creates per-guest run directories here. `0711` lets a guest reach its own directory without being able to list anyone else's. |

## Guest identities (`/etc/userdb`)

systemd refuses `User=`/`Group=` for a numeric id that the user database
cannot resolve: the unit fails before `exec` with status **217/USER**. The
bootstrap therefore registers the whole guest range (default 1024 uids from
2000000) as systemd *userdb* drop-ins, which `nss-systemd` serves to
`getent`, systemd and everything else. For each uid:

```text
/etc/userdb/banlieue-g2000000.user   {"userName":"banlieue-g2000000","uid":2000000,"gid":2000000,
                                      "realName":"banlieue guest","homeDirectory":"/",
                                      "shell":"/usr/sbin/nologin","locked":true}
/etc/userdb/banlieue-g2000000.group  {"groupName":"banlieue-g2000000","gid":2000000}
/etc/userdb/2000000.user  -> banlieue-g2000000.user     (lookup by uid)
/etc/userdb/2000000.group -> banlieue-g2000000.group    (lookup by gid)
```

Accounts are locked, have no shell and no home: they exist only to be a
unit's identity. This needs `systemd` in the `passwd:` and `group:` lines of
`/etc/nsswitch.conf` (Debian's default); `preflight` checks, and `selftest`
confirms the first uid resolves:

```sh
getent passwd 2000000     # banlieue-g2000000:x:2000000:2000000:banlieue guest:/:/usr/sbin/nologin
```

## Directories and permissions

```text
<storage class dir>/            banlieue:banlieue   0711   guests traverse, cannot list
<storage class dir>/images/     banlieue:banlieue   0750   image cache, provider only
<storage class dir>/<machine UID>/  guest:banlieue  2770   disk, seed, serial log
/run/banlieue/ch/<guest uid>/       guest:banlieue  2770   API, swtpm and vsock sockets
/var/lib/banlieue/                  banlieue        0751   traverse only
/var/lib/banlieue/tpm/              banlieue        0711   guests traverse, cannot list
/var/lib/banlieue/tpm/<guest uid>/  guest:banlieue  2770   swtpm state
/var/lib/banlieue/ek/<machine UID>/ banlieue        0700   host-minted EK certificates
/var/lib/banlieue/units/            banlieue        0700   template environment files
/var/lib/banlieue/swtpm-localca/    banlieue        0700   the host's EK CA
```

`2770` is owner (the guest) and group (the provider), plus **setgid**: what
the VMM creates inside inherits the `banlieue` group. The provider needs write
on these directories to tear a machine down, and guests are never in the
`banlieue` group, so no guest can enter another's.

### The VMM's API socket

Cloud Hypervisor sets its own umask to `0077`, whatever the unit says, so its
API socket is always `0700` and the provider — the group, not the owner —
could not connect. Before every connect the provider opens the socket path
with `O_PATH | O_NOFOLLOW`, checks on that open file that it is a socket,
owned by the guest's uid, in the `banlieue` group, and grants nothing to
others, and only then sets it to `0660` through that same file handle. The
guest owns the directory and could plant a symlink there; checking and
changing the opened file, never the path, means `CAP_FOWNER` cannot be turned
against another file on the host.

The same holds for everything else the provider changes inside a guest's
directories. The OS disk and the seed are created with `O_CREAT|O_EXCL`
(which never follows a symlink), then grown, handed to the guest and
re-moded through that open file; directories are opened
`O_NOFOLLOW|O_DIRECTORY` before being re-owned; an existing seed is read
without following links. `CAP_CHOWN` and `CAP_FOWNER` therefore only ever
apply to files the provider itself just created or opened.

## Deliberately not set

Removed after an audit on 2026-09-26, because nothing uses them:

| Was | Why it went |
| --- | --- |
| provider `ReadWritePaths=` the state directory (`/var/lib/banlieue`) | Holds the vTPM CA; the provider does not write it until vTPM support lands. |
| provider `ReadWritePaths=` the kubeconfig | Replaced by the credentials directory once token renewal landed: the kubeconfig names a token *file* beside it and never changes, so the provider writes only the token, atomically, in its own `0700` directory. |
| guest `UMask=0007` | Cloud Hypervisor overrides the umask to `0077`; the setting had no effect. |
| polkit verbs `restart`, `kill` | The provider never issues them. |
| Transient units | Replaced by root-owned templates (ADR-0063, amended 2026-09-27): polkit cannot see what a transient unit runs. |

Each comes back in the change that needs it.

## Inspecting a host

```sh
systemctl status banlieue-provider-cloud-hypervisor
systemctl list-units 'banlieue-ch@*' 'banlieue-swtpm*'  # running guests and vTPMs
systemctl cat banlieue-ch@.service                      # the template
systemctl show banlieue-ch@<uid>.service -p User -p Group -p MemoryMax -p Result -p ExecMainStatus
journalctl -u banlieue-ch@<uid>.service                 # the VMM's own log (root)
systemd-analyze security banlieue-provider-cloud-hypervisor.service
getent passwd 2000000
cat /etc/polkit-1/rules.d/60-banlieue-cloud-hypervisor.rules
```
