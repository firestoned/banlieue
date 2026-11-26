<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Cloud Hypervisor: automating the host install

[Cloud Hypervisor Provider](cloud-hypervisor.md) walks one host through by
hand. This page is for doing it to many hosts, unattended: from
cloud-init, a systemd unit, configuration management or CI. Everything here
is `banlieue host cloud-hypervisor` (alias `ch`,
[ADR-0067](https://github.com/firestoned/banlieue/blob/main/docs/adr/0067-banlieue-host-install-subcommand.md),
[ADR-0084](https://github.com/firestoned/banlieue/blob/main/docs/adr/0084-host-install-release-sources-and-versions.md)),
the same binary the host then runs as its provider, so there is nothing else
to install or keep in step.

## Why it automates well

| Property | What it means for automation |
| --- | --- |
| **No prompts** | Every input is a flag or an environment variable; nothing is read from a terminal. |
| **Every flag has a `BANLIEUE_HOST_*` variable** | A whole host can be described by an environment file, with no command line to template. |
| **Idempotent** | A second `install` changes nothing that is already right: the EK CA, the host config and the guest records are kept. Run it at every boot, or on every configuration-management pass. |
| **`--dry-run`** | Prints each change `install` would make (`would ...` lines) and changes nothing. Run it as root, like `install`, so it can read every file it compares. |
| **Fails closed, before writing** | `preflight` and every stage refuse with a message naming the problem and exit non-zero; a VMM, `ch-remote` or firmware download that does not match its sha256 installs nothing. |
| **Stages** | `--only <stage>` runs one of `preflight`, `vmm`, `host`, `attestation`, `tpm`, `polkit`, `provider`, `selftest`, and refuses when a stage it builds on has not run. |
| **Read-only verbs** | `preflight`, `status`, `selftest` and `overhead` change nothing, so they can run as checks anywhere. |

What it deliberately does **not** do, so your automation must: install OS
packages (`systemd`, `libnss-systemd`, `dbus`, `polkitd`, `swtpm`,
`swtpm-tools`, `ca-certificates`; see
[Requirements](cloud-hypervisor.md#requirements)), create a network bridge
([Step 1](cloud-hypervisor.md#step-1-a-bridge-for-guests)), and fetch the
host's cluster credential (below).

## The inputs

Every flag of `install` has an environment variable, except the three that
change what a run does rather than what the host is (`--only`, `--force`,
`--dry-run`). `banlieue host cloud-hypervisor install --help` prints them
all; the ones a fleet usually sets:

| Variable | Flag | Recorded in the host config |
| --- | --- | --- |
| `BANLIEUE_HOST_PROVIDER_NAME` | `--provider-name` (default: the host's short name) | yes |
| `BANLIEUE_HOST_PROVIDER_NAMESPACE` | `--provider-namespace` (default `banlieue-system`) | yes |
| `BANLIEUE_HOST_STORAGE_CLASSES` | `--storage-class NAME=PATH`, comma-separated | yes |
| `BANLIEUE_HOST_NETWORK_CLASSES` | `--network-class NAME=BRIDGE`, comma-separated | yes |
| `BANLIEUE_HOST_ISOLATE_PORTS` | `--isolate-ports NAME`, comma-separated | yes |
| `BANLIEUE_HOST_GUEST_UID_BASE`, `BANLIEUE_HOST_GUEST_UID_COUNT` | `--guest-uid-base`, `--guest-uid-count` | yes |
| `BANLIEUE_HOST_REGISTRY_REPOSITORY`, `BANLIEUE_HOST_REGISTRY_PLAIN_HTTP`, `BANLIEUE_HOST_REGISTRY_KEEP_UNREFERENCED` | `--registry-*` | yes |
| `BANLIEUE_HOST_GUEST_ACCOUNTS` | `--guest-accounts userdb\|files` | yes |
| `BANLIEUE_HOST_ACCEPT_REDUCED_HARDENING` | `--accept-reduced-hardening` | yes |
| `BANLIEUE_HOST_GUEST_CPUS`, `BANLIEUE_HOST_GUEST_CPU_WEIGHT`, `BANLIEUE_HOST_GUEST_CPU_QUOTA`, `BANLIEUE_HOST_GUEST_MEMORY_MAX`, `BANLIEUE_HOST_GUEST_TASKS_MAX` | `--guest-*`, the guests' slice budget | **no: give them on every run** |
| `BANLIEUE_HOST_VMM_VERSION`, `BANLIEUE_HOST_FIRMWARE_TAG` | `--vmm-version`, `--firmware-tag` (default: the pinned release) | only `[vmm]`, which follows each run |
| `BANLIEUE_HOST_VMM_URL`, `BANLIEUE_HOST_CH_REMOTE_URL`, `BANLIEUE_HOST_FIRMWARE_URL`, and the matching `*_SHA256` | a mirror for each release file | no |
| `BANLIEUE_HOST_ARTIFACTS_DIR` | `--artifacts-dir`: the three release files from a local directory | no |
| `BANLIEUE_HOST_PROVIDER_BINARY` | `--provider-binary`: install this file as `/usr/local/bin/banlieue` | no |
| `BANLIEUE_HOST_ALLOW_VIRTUALIZED_HOST` | `--allow-virtualized-host` (labs only) | no |

The attestation flags (`BANLIEUE_HOST_ATTESTATION*`) are covered in
[Attesting with x509pop](cloud-hypervisor.md#attesting-with-x509pop-adr-0146).

Two rules decide what a repeated run does:

1. **The host config wins over defaults, and refuses contradiction.** On a
   host already prepared, every setting the host config
   (`/etc/banlieue/cloud-hypervisor.toml`) records comes from it when the
   flag is not given, and a flag that says something else is **refused**,
   naming the flag, the file and `--force`. So a fleet run either passes
   exactly the values the host was installed with, or leaves them out.
2. **The slice budget is not recorded.** Pass the `--guest-*` flags on
   every run, or the slice loses its limits.

!!! danger "Keep one-off flags out of anything that runs repeatedly"
    `--force` regenerates the host config **and rotates the EK CA**: every
    EK certificate the host issued stops verifying. `--rotate-attestation-ca`,
    `--attestation-ca-csr`, `--attestation-ca-import` and
    `--attestation-ca-openbao-token-file` are single operations too. None of
    them belongs in an environment file applied at every boot. To change a
    recorded setting, edit the host config by hand (or remove it) and re-run;
    never reach for `--force` to get past a refusal.

## The run, in order

The same five steps whatever drives them:

```sh
# 1. Check. Exits non-zero, naming each problem, and changes nothing.
sudo banlieue host cloud-hypervisor preflight

# 2. Preview. "would ..." lines are the changes; none means none.
sudo banlieue host cloud-hypervisor install --dry-run

# 3. Apply. Idempotent.
sudo banlieue host cloud-hypervisor install

# 4. Prove. Boots nothing, leaves nothing.
sudo banlieue host cloud-hypervisor selftest

# 5. Report.
sudo banlieue host cloud-hypervisor status
```

With the settings in the environment, as below, the commands carry no
flags at all, which is what makes them easy to put in a unit or a playbook.
`selftest` takes the same release flags as `install`: when you install a
release other than the pinned one, keep `BANLIEUE_HOST_VMM_VERSION` and
`BANLIEUE_HOST_FIRMWARE_TAG` in the environment so it checks what was
installed.

## An environment file and a oneshot unit

One file per host describes it. Placeholders throughout; keep the real file
out of any repository (it names hosts):

```sh
# /etc/default/banlieue-host-install    (root:root, 0600)
BANLIEUE_HOST_PROVIDER_NAME=bar
BANLIEUE_HOST_NETWORK_CLASSES=default=br0
BANLIEUE_HOST_STORAGE_CLASSES=default=/srv/banlieue/ch
BANLIEUE_HOST_GUEST_CPUS=16-95
BANLIEUE_HOST_GUEST_MEMORY_MAX=200G
# Pin the VMM release you chose, or a re-run puts the host back on the
# release pinned in this banlieue binary.
#BANLIEUE_HOST_VMM_VERSION=v54.0
```

A oneshot unit then re-applies it at every boot, which is how an
image-based host gets back what its ephemeral paths lose, and is harmless on
any other host:

```ini
# /etc/systemd/system/banlieue-host-install.service
[Unit]
Description=Prepare this host for banlieue's Cloud Hypervisor provider
Wants=network-online.target
After=network-online.target
# The bridges must exist before preflight checks them; add the unit that
# creates yours, for example systemd-networkd.service.

[Service]
Type=oneshot
EnvironmentFile=/etc/default/banlieue-host-install
ExecStart=/usr/local/bin/banlieue host cloud-hypervisor install
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
```

`install` starts and restarts `banlieue-provider-cloud-hypervisor.service`
itself (below), so nothing needs to order the provider after this unit.
On Kairos, use the boot stage in
[Image-based hosts](cloud-hypervisor.md#image-based-hosts-kairos)
instead, with storage on a persistent path.

## cloud-init

For a host provisioned by cloud-init, the same run as `runcmd` (packages
first, through cloud-init's own `packages:`; the bridge from your
`network-config`). The binary is assumed to be on the image or fetched
by an earlier step of yours:

```yaml
#cloud-config
packages: [systemd, libnss-systemd, dbus, polkitd, swtpm, swtpm-tools, ca-certificates]
write_files:
  - path: /etc/default/banlieue-host-install
    permissions: "0600"
    content: |
      BANLIEUE_HOST_PROVIDER_NAME=bar
      BANLIEUE_HOST_NETWORK_CLASSES=default=br0
      BANLIEUE_HOST_STORAGE_CLASSES=default=/srv/banlieue/ch
runcmd:
  - [sh, -c, "set -a; . /etc/default/banlieue-host-install; set +a; /usr/local/bin/banlieue host cloud-hypervisor install"]
```

Add the oneshot unit above if the host should re-apply at every boot.

## Configuration management

The dry run is the change detector: a playbook runs it, and applies only
when it prints a `would` line. In Ansible, for example:

```yaml
- name: banlieue host install (preview)
  ansible.builtin.command: /usr/local/bin/banlieue host cloud-hypervisor install --dry-run
  environment: "{{ banlieue_host_env }}"
  become: true
  register: banlieue_preview
  changed_when: false

- name: banlieue host install
  ansible.builtin.command: /usr/local/bin/banlieue host cloud-hypervisor install
  environment: "{{ banlieue_host_env }}"
  become: true
  when: banlieue_preview.stdout_lines | select('match', '^would ') | list | length > 0
```

`banlieue_host_env` is a dictionary of the `BANLIEUE_HOST_*` variables
above, per host. A refusal (a contradicting flag, a missing bridge, a
taken uid) fails the preview task with the installer's message, before
anything is written.

## From a workstation over SSH

`scripts/bootstrap-cloud-hypervisor-host.sh --remote user@host <step>`
copies the binary to the host, translates an environment file
(`BANLIEUE_ENV_FILE`, template from `--print-env-template`) into flags, runs
the step under `sudo` there and removes the copy. It is the right tool for a
handful of hosts from a terminal (it asks for your `sudo` password, so it is
not for CI); see [The whole chain](cloud-hypervisor.md#the-whole-chain). Its
template covers the identity, classes, registry and release settings; pass
the slice flags to the binary directly.

## Connecting the host, unattended

`install` installs the provider unit but enables it only once both
`/usr/local/bin/banlieue` and `/etc/banlieue/credentials/kubeconfig` exist.
The credential comes from the cluster, so it is a step your automation
carries from one side to the other:

```sh
# In the cluster, as an admin, once the host's Provider is applied:
banlieue bootstrap cloud-hypervisor-host --provider bar --output-dir ./creds-bar
#   --namespace (default banlieue-system), --lifetime-minutes (default 1440)

# On the host, as root: both files belong to the banlieue user, mode 0600,
# in the directory install created (banlieue, 0700).
install -m 0600 -o banlieue -g banlieue ./creds-bar/kubeconfig /etc/banlieue/credentials/kubeconfig
install -m 0600 -o banlieue -g banlieue ./creds-bar/token      /etc/banlieue/credentials/token

# Enables and starts the provider now that it can run. Use the host's usual
# environment: the provider stage also renders the guests' slice, so the
# --guest-* settings must be present here too.
set -a; . /etc/default/banlieue-host-install; set +a
banlieue host cloud-hypervisor install --only provider
```

Re-running the oneshot unit (`systemctl restart banlieue-host-install`)
does the same with the environment already in place.

Treat `token` as a secret in transit (your configuration management's vault,
not its plain variables). From then on the provider renews it itself at
half its lifetime. A host that stays down longer than one lifetime comes
back with an expired token: issue a new one the same way. Revoking a host
is deleting its ServiceAccount, which invalidates every token issued for it.
`scripts/ch-host-provider-up.sh` does this whole sequence in one root step
for a test host.

## Upgrades

Roll hosts one at a time; guests are their own units and keep running,
and the new provider adopts them.

```sh
# The provider only: replaces /usr/local/bin/banlieue and the unit files,
# restarts the provider when either changed. Repeat the slice flags.
sudo banlieue host cloud-hypervisor install --only provider --provider-binary ./banlieue --dry-run
sudo banlieue host cloud-hypervisor install --only provider --provider-binary ./banlieue

# The VMM: a newer release, or back to an older one still on disk.
sudo banlieue host cloud-hypervisor install --only vmm --vmm-version v54.0
sudo systemctl restart banlieue-provider-cloud-hypervisor.service
```

A full `install` also re-runs the VMM stage, which links the release it is
given (the pinned one by default): use `--only provider` on a host running a
VMM release other than the pinned one, or keep `BANLIEUE_HOST_VMM_VERSION`
in its environment. Details:
[Choosing the VMM version](cloud-hypervisor.md#choosing-the-vmm-version-and-where-it-comes-from).

## Decommissioning a host

There is **no uninstall verb**. To take a host out:

1. Drain it in the cluster: delete or move the VMs placed on its `Provider`,
   then delete the `Provider`. Deleting the provider's ServiceAccount
   revokes the host's credential at once.
2. On the host, stop the provider and any guests left:
   `systemctl disable --now banlieue-provider-cloud-hypervisor.service`,
   then stop the `banlieue-ch@*`, `banlieue-swtpm@*` units.
3. Remove what `install` wrote, listed in
   [What ends up where](cloud-hypervisor.md#what-ends-up-where): the units
   in `/etc/systemd/system/` (then `systemctl daemon-reload`), the polkit
   rule, the tmpfiles entry, `/etc/banlieue/`, `/var/lib/banlieue/`,
   `/opt/banlieue/`, the links in `/usr/local/bin/`, the storage class
   directories, and the guest identities (`/etc/userdb/` records, or with
   `--guest-accounts files`, the `banlieue-g<uid>` users and groups).

Removing `/var/lib/banlieue/` destroys the host's EK CA and every guest's
vTPM state; do it only once nothing needs to verify an EK certificate this
host issued.
