<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0067 — `banlieue host`: preparing a Cloud Hypervisor host from the binary

- **Status:** Accepted
- **Date:** 2026-09-27
- **Proposed:** 2026-09-27
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-10-02 by [ADR-0084](0084-host-install-release-sources-and-versions.md)
  (Decisions 1 and 8: the command is `banlieue host cloud-hypervisor <verb>`,
  alias `ch`; Decision 2: no `packages` stage; Decision 4: the VMM and
  firmware come from any URL at any version the client's gate accepts;
  Decisions 7 and 8: no package installation on any distribution)
- **Related:** [ADR-0004](0004-single-binary-subcommand-dispatch.md) (one
  binary, subcommands), [ADR-0011](0011-libvirt-provider-own-client.md) (no
  subprocess in a reconcile path), [ADR-0013](0013-banlieue-bootstrap-cli.md)
  (`banlieue bootstrap`, `--dry-run`), [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (host-resident provider, its credential),
  [ADR-0061](0061-banlieue-cloud-hypervisor-vmm-client.md) (the VMM pin),
  [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md) (the host
  config file), [ADR-0063](0063-cloud-hypervisor-host-supervision.md) (users,
  template units, polkit), [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md)
  (the registry section), [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md)
  (the EK CA); roadmap 09 phase 10.

## Context

A Cloud Hypervisor host is prepared today by
`scripts/bootstrap-cloud-hypervisor-host.sh`: about 780 lines of bash, an
environment file, the template units under
`deploy/provider-cloud-hypervisor/host/`, and then the `banlieue` binary.
That is three artifacts to deliver to a hypervisor and three ways to get the
combination wrong, where `k0s install` needs one.

The script and the provider must agree on a dozen facts, and today they
agree by hand, in two languages: the host config schema (ADR-0062 D4), the
guest uid range and its NSS records (ADR-0063 D3), the directory layout and
modes that are the access control (ADR-0063 D5), the template unit names the
polkit rule allows (ADR-0063, amended), the EK CA path (ADR-0065), and the
pinned VMM release the client is written against (ADR-0061). Nothing checks
the last one except a test added on 2026-09-27 that reads the script. Drift
is silent until a guest fails to start.

The script also cannot use the provider's own code to check its work: its
`selftest` re-implements, in shell, checks the provider makes in Rust.

## Decision

### 1. A `host` subcommand, with read-only and mutating verbs kept apart

```sh
banlieue host preflight                 # changes nothing
banlieue host status                    # changes nothing
banlieue host selftest                  # changes nothing, boots nothing
banlieue host install                   # every stage, in order
banlieue host install --only tpm        # one stage; its prerequisites must hold
banlieue host install --dry-run         # prints what it would do, does nothing
```

`install` is the only verb that changes the host. `preflight` and `status`
are not "careful": they are written against an interface (`Probe`) that
has no mutating methods, so they cannot change the host by construction.
`selftest` must manufacture a vTPM to check its EK certificate, so it runs
`swtpm_setup` as the provider's user in a scratch directory under the state
root, which it removes. The one thing it changes is the EK CA's serial
counter, which every manufacture advances and which a CA must never rewind;
tests assert the host is otherwise unchanged.

### 2. The stages and their order

| Stage | Needs | Changes |
| --- | --- | --- |
| `preflight` | — | nothing (it runs first in every `install`; with `--install-packages`, just after `packages`, since what it checks — the kvm group, NSS's systemd module — can come from them) |
| `packages` | — | dpkg state, only with `--install-packages` |
| `vmm` | — | `/opt/banlieue/cloud-hypervisor/<version>/`, `/opt/banlieue/firmware/<tag>/`, two symlinks |
| `host` | — | the `banlieue` user, guest uid records, state, storage and run directories, the host config |
| `tpm` | `packages`, `host` | the per-host EK CA and swtpm configuration |
| `polkit` | `packages`, `host` | one polkit rule |
| `provider` | `host` | the four template units and the provider unit; enabled only when it can run |
| `selftest` | `vmm`, `host`, `tpm`, `polkit` | nothing |

`--only <stage>` checks the stage's prerequisites and refuses, naming what
is missing, instead of half applying. `install` with no `--only` runs the
stages in the order above and stops at the first failure.

### 3. The installer is its own crate, which no reconciler can reach

A new library crate, `banlieue-host`, holds the stages. It depends on
`banlieue-provider-cloud-hypervisor` (the host config type, unit and path
constants) and `banlieue-cloud-hypervisor` (the pin), never the reverse. The
`banlieue` binary includes it behind a `host` Cargo feature, on by default
so the binary a host downloads can prepare that host.

ADR-0011 forbids subprocesses **in a reconcile path**. This installer runs
`apt-get`, `useradd`, `systemctl`, `systemd-tmpfiles`, `runuser` and
`swtpm_setup` once, as root, at the operator's request: it is not a
reconcile path, and the crate boundary is what keeps it from becoming one.
A test asserts that the provider crate does not depend on `banlieue-host`.

### 4. One pinned release, one constant

The VMM version, the sha256 of `cloud-hypervisor-static` and
`ch-remote-static`, and the firmware tag and sha256 are constants in
`banlieue-host`. A unit test asserts that the version equals the one
`banlieue-cloud-hypervisor/spec/PIN` names, the same release the client's
version gate uses. There is no flag to install a different version: the pin
is part of the product, and changing it is a code change with its test.

Downloads are HTTPS only (rustls, already in the workspace), written to a
temporary name beside the destination, hashed, and renamed into place only
when the digest matches. On a mismatch nothing is installed, the previous
symlinks are left as they were, and the command fails. `--artifacts-dir
<dir>` takes the same files from a local directory instead of the network,
verified the same way, for air-gapped hosts and for the container test.

### 5. What it writes, and how

- Every file is written to a temporary name and renamed into place, then
  owned and moded. A crash leaves the old file or the new one.
- The host config is rendered from the same `HostConfig` type the provider
  loads, and parsed back before it is installed: the installer cannot write
  a file the provider would refuse.
- The template units and the polkit rule are the files in
  `deploy/provider-cloud-hypervisor/host/`, compiled into the binary, with
  their `@NAME@` placeholders filled. A placeholder left unfilled is an
  error. They stay in the repository as files, reviewed as files.
- Directories the OS or another package owns (`/etc/systemd/system`,
  polkit's `rules.d`, which Debian ships `root:polkitd`, `/usr/local/bin`,
  `/etc/tmpfiles.d`) are created when missing and otherwise never re-moded
  or re-owned. Found by a dry run on a host the script had prepared.
- Every ownership and mode change acts on a handle opened `O_NOFOLLOW`:
  several directories the installer touches as root belong to the
  provider's user, who must not be able to plant a symlink that turns a
  re-run against another file.
- An existing host config and an existing EK CA are kept unless `--force`.
  Rotating the CA invalidates every EK certificate this host has issued
  (ADR-0065 Decision 6); `--force` says so before it acts.
- **Idempotence is equality.** A second `install` changes nothing: same
  files, same bytes, same modes, same owners, same units.
- `systemctl daemon-reload`, `enable` and `start` run only when systemd is
  the running init; otherwise the files are installed and the command says
  what it skipped. That is what lets the stages run in a container.

### 6. Configuration

Every setting is a flag with a `BANLIEUE_HOST_*` environment variable, so a
cloud-init payload needs no file of its own:

| Flag | Default |
| --- | --- |
| `--provider-name` | the host's short name |
| `--provider-namespace` | `banlieue-system` |
| `--storage-class name=path` (repeatable) | `default` on the candidate mount with the most free space |
| `--network-class name=bridge` (repeatable) | `default=virbr0` if it exists |
| `--guest-uid-base`, `--guest-uid-count` | 2000000, 1024 |
| `--registry-repository`, `--registry-plain-http`, `--registry-keep-unreferenced` | none, false, 1 |
| `--install-packages` | off: `packages` verifies and lists what is missing |
| `--allow-virtualized-host` | off |
| `--artifacts-dir` | none: download |
| `--provider-binary` | install this file as the provider binary |
| `--force` | off |

Paths (`/etc/banlieue`, `/var/lib/banlieue`, `/run/banlieue/ch`,
`/opt/banlieue`, `/usr/local/bin`) are the ones the script used and the
guides document. They are not flags: they are the contract with the
template units and the provider, and moving one means moving all.

### 7. What does not move into the binary

- **The bridge.** Neither the script nor the binary creates one. A bridge
  mistake over SSH loses the host; the guide's timed-rollback recipe stays
  in a human's hands. `preflight` refuses a network class whose bridge does
  not exist.
- **SSH.** The binary gets no SSH client (ADR-0011's shape D). The script
  shrinks to a `--remote user@host` wrapper that copies the binary to the
  host and runs `sudo banlieue host <verb>` there.
- **Installing packages by default.** Only with `--install-packages`, and
  only through `apt-get`; on any other distribution `packages` verifies and
  prints what is missing.

### 8. The questions roadmap 09 left open

- **`uninstall` and `drain`: not in this ADR.** Taking a host out of
  service means moving or deleting its guests first, which is roadmap 14's
  `migrationPolicy` question, and then removing what `install` put there.
  Both belong to a later decision; `status` shows what is installed so an
  operator can see what a manual removal must cover.
- **Distributions: Debian family.** `packages` installs through `apt-get`
  or not at all. Every other stage is distribution-neutral (systemd, userdb,
  polkit, swtpm are the requirements), so a host of another family works if
  its packages are installed by hand.
- **Naming: `bootstrap` for a cluster, `host` for a machine.** `banlieue
  bootstrap` installs into a cluster (ADR-0013) and keeps its name,
  including `banlieue bootstrap cloud-hypervisor-host`, which issues a
  host's credential and runs against the cluster, not the host. `banlieue
  host install` prints that command when the credential is missing.

## Consequences

**Positive**

- A host is prepared by one verified binary and one command, the same
  binary it then runs as the provider.
- The script's hand-kept agreements become shared Rust constants and types:
  the host config round-trips through the provider's parser, the pin is one
  constant with a test, and `selftest` manufactures a vTPM the way the
  provider does.
- Every stage is unit-tested against fakes, and the whole install runs as
  root in a Debian 13 container (`make ch-host-install-test`).

**Negative / accepted costs**

- The `banlieue` binary now contains code that is meant to run as root.
  Mitigated: it is a separate crate, reachable only from the `host`
  subcommand, and a build without the `host` feature does not contain it.
- The VMM pin can only change with a release of banlieue. That is the
  intent, and air-gapped hosts use `--artifacts-dir` with the same pins.
- The script remains, as a thin SSH wrapper.

**Follow-ups**

- `banlieue host uninstall` and host drain, with roadmap 14.
- A second package backend, if a non-Debian host family is ever a target.
