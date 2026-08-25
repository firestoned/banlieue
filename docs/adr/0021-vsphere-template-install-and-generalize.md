# 0021 — vSphere template: install once, generalize, then mark as template

## Status

Proposed — 2026-08-14. Extends [ADR-0020](0020-vsphere-per-zone-iso-import.md)
(vSphere per-zone ISO import). Builds on [ADR-0008](0008-byoc-vsphere-http-client.md)
(BYOC vim client).

## Context

ADR-0020's per-zone import produces a "template" that is not actually
installed: `import_iso_template` creates an empty EFI VM, attaches the built
Kairos ISO as a CD-ROM (`startConnected: true`), and immediately calls
`MarkAsTemplate`. The disk is blank. Every future *clone* of that template must
boot from the still-attached ISO and run Kairos's unattended installer itself
(`wait_for_install_vsphere` in `scripts/bootstrap-k0s-cluster.sh` polls
`findmnt -n -o SOURCE /` over SSH for `/dev/loop0`, ~8-12 min per clone). This
is the process `docs/src/guides/alpine-vsphere-template.md` documents doing by
hand for Alpine and calls "the most important step" (Step 6 — Generalize) when
building a *real* template: install once, strip per-machine identity, then
template — so every clone boots straight into a ready OS with no install-media
dependency.

Two implementation options were considered for running the generalize step:

1. **vCenter Guest Operations API** (`GuestOperationsManager` /
   `GuestProcessManager` / `GuestFileManager`, confirmed present in the pinned
   `vim_rs` 0.5.0). This runs commands inside the guest through vCenter only —
   no network route from the import Job's pod to the zone's VM port group is
   needed. But it requires a guest username/password
   (`NamePasswordAuthentication`), and the cloud-config the environment
   currently bakes into the ISO (`kairos-base-cloud-config`) defines no user at
   all — only `install.{auto,device,reboot,grub-entry-name,grub_options}` and
   `eject-cd`. Adding this would mean a new credentials-handling surface (a
   `SecretKeySelector` field plus a contractual requirement that the user's own
   cloud-config create a matching guest user) purely to run four shell
   commands.
2. **A Kairos cloud-config stage**, run entirely inside the guest, no
   provider-side guest access at all. Kairos's `stages.after-install-chroot`
   hook runs "after installing active and grub inside chroot" — i.e. *before
   the installed disk is ever booted*. Since `iso`-kind build artifacts
   (ADR-0020's table: `iso` → vsphere) exist for exactly one purpose — this
   per-zone template build — there is no other consumer to protect from an
   unconditional wipe; no tombstone/gate is needed. Pairing this with
   `install.poweroff: true` / `install.reboot: false` (both are documented
   `install:` keys) means the golden disk is *never booted* by the build
   itself: identity files are truncated/removed while still empty (defensive —
   a `kairos-init`-built base image could otherwise bake a fixed
   `machine-id`), the VM powers itself off, and the first *real* boot of that
   disk is a clone's first boot, which generates its own machine-id and SSH
   host keys the normal way. No post-install reboot-then-rewipe round trip is
   needed at all.

Option 2 needs no new credentials, no new guest-facing API calls, and produces
a template that behaves exactly like a normal freshly-imaged disk. It is the
chosen approach.

## Decision

### 1. Cloud-config contract for `iso`-kind (vSphere) builds

The cloud-config referenced by `VMImage.spec.cloudConfig` for a vSphere `Url`
source **must**, in addition to today's `install.auto`/`install.device`:

```yaml
install:
  auto: true
  device: /dev/sda
  reboot: false
  poweroff: true
users:
  - name: kairos-admin
    groups: ["admin"]
    passwd: <hashed-or-plaintext-per-your-policy>
stages:
  after-install-chroot:
    - name: "banlieue: strip per-machine identity before templating"
      commands:
        - truncate -s 0 /etc/machine-id
        - rm -f /etc/ssh/ssh_host_*
```

**`users` (found live, 2026-08-18):** Kairos (agent v2.29.4+, the v3.3.x
line) refuses to run its install stage at all without at least one user in
the `admin` group anywhere in the merged cloud-config — it halts with `No
users found in any stage that are part of the 'admin' group` and never
reaches `install.poweroff`, so the same "import Job's wait times out" failure
mode as a missing `poweroff`/`reboot` pair. This is unrelated to
`banlieue`'s own SSH-host-key/machine-id wipe in `after-install-chroot` —
that strips identity from the *template*; `users` is what lets Kairos's
install stage complete at all. Set `install.nousers: true` instead of
`users` only if a userless system is genuinely intended (unusual — there is
then no way to log in for post-clone debugging).

This is a documented **contract** of the vSphere ISO import path, not
something `banlieue-imagebuilder` injects into the user's Secret — merging
YAML stage lists mechanically is fragile, and ADR non-negotiable #4 (explicit
over implicit) argues against silently rewriting user-supplied cloud-config.
`docs/src/guides/using-banlieue-imagebuilder.md` and
`examples/07-vmimage-kairos-url-source.yaml` document the required snippet.

### 2. Import-Job sequence change

`import_iso_template` (`crates/banlieue-provider-vsphere/src/client/vim.rs`)
and `import.rs` change from *create → attach ISO → MarkAsTemplate* to:

1. Create the VM per `spec.template.*` and attach the ISO (unchanged) — no
   boot order set here, deliberately: matches `create-kairos-template.sh`.
2. **Set boot order via a separate `ReconfigVM_Task`**, once the VM exists
   and its devices have real (positive) keys: explicitly connect the CD-ROM
   (`connectable.connected = true`) and set
   `boot_options.boot_order = [cdrom, disk, ethernet]`, resolved by device
   key from `VirtualMachine.config().hardware.device` — mirroring
   `create-vm.sh`'s `govc device.connect` + `device.boot -order
   cdrom,disk,ethernet` exactly. Found live (twice): a boot order embedded
   in the *initial* `CreateVM_Task` spec, referencing the create spec's
   provisional negative keys, was not reliably honored by EFI firmware —
   the VM stopped at the interactive Boot Manager menu even though the
   device order visibly changed in that menu (proving the write partially
   landed but firmware still never auto-selected it). A `boot_retry_enabled`
   attempt at explaining this away as a generic VMware retry quirk was also
   tried and was wrong — the maintainer's own proven scripts never use it;
   the real fix is the create/reconfigure split, verified against those
   scripts directly.
3. **Power on** (`PowerOnVM_Task`) and confirm the task succeeds and
   `runtime.powerState` reports `poweredOn` — "validate it started" is this
   cheap, immediate check, distinct from the long poll in the next step.
4. **Poll for self-shutdown**: `runtime.powerState == poweredOff`, bounded by
   `spec.template.installTimeoutSeconds` (new optional field, default 1800s —
   generous over the ~8-12 min observed unattended-install time, since this
   VM never reboots into the OS the way a clone does). A misconfigured
   cloud-config (missing `poweroff: true`, install failure dropping to an
   emergency shell) times out here rather than hanging the Job forever.
5. On timeout: fail the Job with a clear message; **do not destroy the VM** —
   leave it powered on for console debugging, matching the existing
   fail-closed posture (checksum verification, SEC-004). A retry (`--force-
   create`) still destroys-and-recreates as today (and now does so *before*
   the datastore upload/reuse-check, not after — see the CHANGELOG entry on
   the NFC-lock ordering fix).
6. On success: **remove the CD-ROM device** (not merely disconnect it — no
   future clone should carry an ISO-backed device at all) and `MarkAsTemplate`.

### 3. New `VMImageTemplate` fields

```rust
/// Bound on how long the import Job waits for the unattended Kairos install
/// to finish and the VM to power itself off (`install.poweroff: true` in the
/// cloud-config) before failing the Job. Defaults to 1800s (30 min).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub install_timeout_seconds: Option<i32>,

/// Run the install-then-generalize sequence (steps 2-5 above) at all.
/// Defaults to `true`. `false` reverts this `Url` source's per-zone import
/// to ADR-0020's original behavior — create the VM, attach the ISO,
/// `MarkAsTemplate` immediately, no power-on — for a build that isn't
/// Kairos-driven or whose install/generalize is managed some other way.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub auto_manage_install: Option<bool>,
```

Both threaded as `--install-timeout-seconds` / `--auto-manage-install` on
`banlieue provider vsphere image-import`, mirroring how every other
`spec.template.*` knob already threads through `ImportArgs` (ADR-0020's
"fully parameterized" work).

`autoManageInstall` is deliberately scoped to *whether the sequence runs*,
not *how the cloud-config gets authored*. An earlier draft of this ADR
considered having `banlieue-imagebuilder` auto-inject the required
`install.poweroff`/`after-install-chroot` directives into the user's
cloud-config Secret — rejected: `banlieue-imagebuilder`'s RBAC is explicitly
documented as never touching Secrets of any kind (`deploy/imagebuilder/rbac/
clusterrole.yaml`), and reading/merging into a user's Secret to auto-manage
install would break that boundary for a convenience that's just as well
served by documentation. The cloud-config contract (Decision #1) stays the
user's responsibility when `autoManageInstall` is `true`; setting it `false`
opts out of needing that contract satisfied at all.

No new credentials field, no guest-ops client code, no change to
`banlieue-imagebuilder`'s Secret-free RBAC posture.

## Consequences

- **Clones no longer need the ISO.** A per-zone template produced this way has
  a fully installed, generalized disk and no CD-ROM device; clones boot
  straight into Kairos, closing the gap this ADR opened with (ADR-0020's
  template was not actually usable as a template without a further per-clone
  install).
- **No guest credentials, no new attack surface.** The generalize step is
  guest-side automation the user already controls via their own cloud-config,
  not a provider-held guest username/password.
- **A new operational contract.** Anyone authoring a `cloudConfig` Secret for
  a vSphere `Url` source must include `install.poweroff: true` /
  `install.reboot: false` and the `after-install-chroot` wipe stage documented
  above, or the import Job will time out waiting for a poweroff that never
  comes — unless `spec.template.autoManageInstall: false` opts out of the
  whole sequence. Documented in `docs/src/guides/using-banlieue-imagebuilder.md`
  and `examples/07-vmimage-kairos-url-source.yaml`; the import Job's
  timeout-failure message references this ADR/guide.
- **`--force-create` destroys the stale target before the datastore
  reuse-check, not after** (found live testing this ADR). A template whose
  CD-ROM backing still references the target ISO holds an NFC lock on that
  file; vCenter's datastore HTTP API returns `500 NFC_FILE_LOCKED` for
  GET/HEAD while the lock holds, which the reuse-check (deliberately)
  cannot distinguish from the file being genuinely absent — so it fell
  through and re-uploaded onto a different, emptier datastore member instead
  of reusing the already-present ISO. `VSphereClient::destroy_if_present` now
  runs in `import.rs::run()` right after the datacenter resolves, before any
  datastore work, so a `--force-create` re-run releases the lock in time for
  its own reuse-check to succeed.
- **Provider-side vCenter work stays not-unit-testable for the live
  interaction** (same as ADR-0020): `FakeClient` covers the power-on /
  poll-for-poweroff / device-removal / MarkAsTemplate *sequencing* and
  timeout behavior; the real power-state transition is verified live against
  vCenter.
- **Faster than the original plan considered mid-design.** No reboot-then-
  rewipe round trip, no guest-ops polling loop — just two vSphere task calls
  and a power-state poll already in the vim client's existing vocabulary.

## Follow-ups

- If a future `useContentLibrary: true` path lands (ADR-0020 follow-up), this
  install/generalize step still applies before importing into the Content
  Library, since it operates on the source VM, not the library entry.
- Consider surfacing the timeout failure as a distinct `ZoneImageStatus`
  reason (e.g. `InstallTimedOut`) so operators can distinguish "cloud-config
  contract violated" from other per-zone import failures at a glance.
