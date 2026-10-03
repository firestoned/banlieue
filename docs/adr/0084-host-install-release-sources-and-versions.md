<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0084: `banlieue host cloud-hypervisor`: the VMM from any URL, at any supported version, and no packages

- **Status:** Accepted
- **Date:** 2026-10-02
- **Deciders:** Erick Bourgeois
- **Amends:** [ADR-0067](0067-banlieue-host-install-subcommand.md)
  Decisions 1 and 8 (the command is `banlieue host <verb>`), 2 (the
  `packages` stage), 4 (one pinned release, no flag), 7 and 8 (Debian
  family only)
- **Related:** [ADR-0061](0061-banlieue-cloud-hypervisor-vmm-client.md)
  Decision 5 (the client's version gate), [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md)
  Decision 4 (the host config); roadmap 09

## Context

ADR-0067 made `banlieue host install` prepare a Cloud Hypervisor host from
the binary itself. Some of its decisions do not survive contact with the
hosts banlieue is now asked to run on, and its name does not say what it
prepares:

- **Packages.** The `packages` stage verifies, and with `--install-packages`
  installs, eight Debian packages through `apt-get`. Hosts are not all
  Debian. Kairos is installed on top of a Debian base but is image-built,
  and other hosts use dnf, zypper or something else. Every other stage is
  already distribution-neutral (ADR-0067 Decision 8): systemd, userdb,
  polkit and swtpm are requirements, not things banlieue has to install.
  None of those packages publishes a usable binary release on GitHub
  (swtpm and libtpms release source signatures only; systemd, polkit and
  dbus are the base OS), so banlieue has nothing better to offer than the
  host's own package manager, which the operator already runs.
- **Where the VMM comes from.** The `vmm` stage downloads three files from
  `github.com`: `cloud-hypervisor-static` and `ch-remote-static` from
  `cloud-hypervisor/cloud-hypervisor`, and `CLOUDHV.fd` from
  `cloud-hypervisor/edk2`. An air-gapped host cannot reach `github.com`; it
  reaches an internal proxy instead, typically an Artifactory VCS or
  generic remote repository whose URLs look like
  `https://internal.example.com/artifactory/vcs-github/<org>/<project>/…`.
  The only alternative today, `--artifacts-dir`, needs the files to be on
  the host already.
- **Which version.** ADR-0067 Decision 4 allows exactly the version
  compiled in, so a host cannot take a Cloud Hypervisor fix without a
  banlieue release. The client does not need that: its version gate
  (ADR-0061 Decision 5) refuses a VMM **older** than the vendored spec's
  `major.minor` and accepts any newer one.
- **What the command prepares.** `banlieue host install` does not say
  which backend it prepares a host for. Cloud Hypervisor is the only one
  today, because it is the only backend whose hypervisor banlieue manages
  itself; libvirt and Proxmox hosts are prepared by scripts. If either
  moves into the binary, `host install` stops meaning one thing. The rest
  of the CLI already names the backend: `banlieue provider cloud-hypervisor`,
  `banlieue bootstrap cloud-hypervisor-host`.

The three files above are the only ones banlieue downloads onto a host. The
`banlieue` binary is not among them: it is already on the host, because it
is what runs the installer.

## Decision

### 1. No `packages` stage

The `packages` stage, `--install-packages` and
`BANLIEUE_HOST_INSTALL_PACKAGES` are removed, along with every `apt-get`
and `dpkg-query` call. The host supplies its own OS components, installed
however that OS installs things (a package manager, an image build, a
Kairos `stages` entry). `preflight` checks for them by command, which works
on any distribution, and lists every one that is missing:

| Command | Typically from |
| --- | --- |
| `systemctl`, `systemd-tmpfiles` | systemd |
| `swtpm` | swtpm |
| `swtpm_setup`, `swtpm_localca` | swtpm-tools (Debian), swtpm (Fedora, Arch) |

polkit, dbus and the systemd NSS module are not commands. They are already
covered by what the later stages check: `preflight` reads
`nsswitch.conf`, the `host` stage resolves a guest uid through NSS, and the
self-test proves the provider's user can drive the units. `ca-certificates`
is needed only for an HTTPS download, and its absence fails that download
with a message naming it.

`tpm` and `polkit` now need `preflight` (the commands) and `host`, where
they needed `packages` and `host`. The stage order is `preflight`, `vmm`,
`host`, `tpm`, `polkit`, `provider`, `selftest`, with no reordering.

### 2. Each of the three files has a URL, `github.com` by default

| Flag | Environment | Default |
| --- | --- | --- |
| `--vmm-url` | `BANLIEUE_HOST_VMM_URL` | `https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/<version>/cloud-hypervisor-static` |
| `--ch-remote-url` | `BANLIEUE_HOST_CH_REMOTE_URL` | `…/cloud-hypervisor/releases/download/<version>/ch-remote-static` |
| `--firmware-url` | `BANLIEUE_HOST_FIRMWARE_URL` | `https://github.com/cloud-hypervisor/edk2/releases/download/<firmware-tag>/CLOUDHV.fd` |

A URL is taken verbatim: no templating. An Artifactory layout differs by
repository type and site, and the operator who knows it writes the whole
URL. Downloads stay HTTPS only. A URL flag conflicts with
`--artifacts-dir`, which stays for hosts with no network at all.

### 3. The VMM version and the firmware tag are selectable

`--vmm-version` (`BANLIEUE_HOST_VMM_VERSION`) and `--firmware-tag`
(`BANLIEUE_HOST_FIRMWARE_TAG`) default to the pinned release. A VMM
version below the client's gate is refused before anything is fetched,
with the same `major.minor` comparison the provider makes at runtime, so
`install` never puts a VMM on a host that the provider would then mark
`VmmVersionUnsupported`. A newer version is accepted. The firmware tag is
not checked: edk2 tags are commit names with no order.

Versions install side by side, as they already do: each lives in
`/opt/banlieue/cloud-hypervisor/<version>/` or
`/opt/banlieue/firmware/<tag>/`, and only the symlinks move. Rolling back
is another `install` with the old version, and a guest already running
keeps the VMM process it started with.

### 4. Every file is still verified against a sha256 before anything is installed

Where the digest comes from depends on the file:

1. **The pinned version or tag:** the sha256 compiled into the binary, as
   before. A URL flag changes where the bytes come from, never what they
   must hash to.
2. **Any other version or tag, with a digest flag:** `--vmm-sha256`,
   `--ch-remote-sha256` or `--firmware-sha256`.
3. **Any other version or tag, without one:** the digest GitHub publishes
   for that release asset, read from
   `https://api.github.com/repos/<org>/<project>/releases/tags/<tag>`. If
   that request fails (an air-gapped host), `install` stops before
   fetching anything and names the flag to pass.

A digest flag for a pinned file is refused, so a typo cannot quietly
replace a compiled pin. The fetch, verify-all-then-install and
nothing-on-mismatch behaviour of ADR-0067 Decision 4 is unchanged.

### 5. The host config's `[vmm]` section follows the installed release

The host config names the VMM version and the firmware path. An existing
host config is still kept without `--force`, but when its `[vmm]` section
differs from the release just installed, that section alone is rewritten:
the file is parsed with the provider's own parser, only `[vmm]` is
replaced, and the result is parsed back before it is written. `--force`
still regenerates the whole file and rotates the EK CA, so upgrading the
VMM never needs it. A host config that does not parse is left untouched,
with a warning. The provider reads the file at startup, so `install` says
to restart the provider unit after a `[vmm]` change. It does not restart
it itself.

### 6. `selftest` checks the release it is told about

`banlieue host cloud-hypervisor selftest` takes the same `--vmm-version`, `--firmware-tag`
and digest flags as `install`, so it checks the firmware a non-default
install placed instead of failing against the pin. `status` reports the
version and firmware the host config names, and the pinned release beside
them.

### 7. The verbs live under the backend: `banlieue host cloud-hypervisor <verb>`

```sh
banlieue host cloud-hypervisor preflight    # changes nothing
banlieue host cloud-hypervisor status       # changes nothing
banlieue host cloud-hypervisor selftest     # changes nothing, boots nothing
banlieue host cloud-hypervisor install      # every stage, in order
banlieue host ch install                    # the same; `ch` is a visible alias
```

`host` still means "this machine", as against `bootstrap`, which means "a
cluster" (ADR-0067 Decision 8); the backend comes next, as it does for
`banlieue provider <backend>`. The verbs, and ADR-0067 Decision 1's split
between read-only verbs and the one mutating verb, are unchanged. A future
`banlieue host libvirt install` fits beside it without renaming anything.

`banlieue host install` is removed, not kept as an alias: a top-level
`install` under `host` would again not say what it prepares, and the only
binaries that carried it predate any release that documents a Cloud
Hypervisor host as supported. A top-level `banlieue install ch` was
considered and rejected: next to `banlieue bootstrap` (install into a
cluster), a bare `install` is ambiguous about its target, and the backend
is spelled `cloud-hypervisor` everywhere else in the CLI and the API, so
`ch` is only the short alias here.

## Consequences

**Positive**

- The command names what it prepares, the same way `provider` and
  `bootstrap` already do, and leaves room for a second backend.
- One installer for any systemd distribution. The host's packages are the
  host operator's, installed with the tool their OS already uses.
- An air-gapped host installs from its own proxy with three flags and no
  files copied in by hand.
- A host takes a Cloud Hypervisor fix without waiting for a banlieue
  release, and never takes a version the provider would refuse.

**Negative / accepted costs**

- `banlieue host install`, `preflight`, `status` and `selftest` are now
  `banlieue host cloud-hypervisor …` (or `banlieue host ch …`). A host
  whose boot stage or script calls the old form fails with an unknown
  subcommand until it is updated; the repository's scripts and guides are
  updated in the same change.
- `--install-packages` is gone. A script that passed it now fails on an
  unknown flag; `scripts/bootstrap-cloud-hypervisor-host.sh` is updated in
  the same change. The flag was opt-in, so a host prepared without it sees
  no difference.
- For a version other than the pin, the digest is GitHub's, or the
  operator's. That is weaker than a pin reviewed in a banlieue change: it
  catches a corrupted or substituted download from a mirror, but not a
  release replaced on GitHub itself. The operator chooses it per host,
  explicitly. Recorded in the threat model's §8.
- A newer VMM runs against types written for the pinned spec. The client
  sends only fields that exist in the pinned schema, but a newer release
  could change their meaning. The version gate refuses only older ones.
- `api.github.com` is a new external endpoint, used only for a non-pinned
  file without a digest flag, and only by `install` and `selftest`, never
  by the provider.
- Rewriting `[vmm]` serializes the whole file again, which drops comments
  an admin added. Keys and values are kept.

**Follow-ups**

- `aarch64`: the `-static-aarch64` assets exist upstream; the architecture
  check in `preflight` still refuses anything but `x86_64`.
