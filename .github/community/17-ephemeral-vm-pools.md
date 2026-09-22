# 17: Ephemeral, single-use VM pools (AI agent sandboxes)

> **Goal.** A consumer asks for a VM for one identity and gets one in seconds:
> already installed, disk sealed to its own vTPM, never used by anyone else,
> destroyed when the identity is done with it. On vSphere first, with nothing
> in the design that blocks libvirt or Proxmox.
>
> **Stop condition.** `kubectl apply` of a `VirtualMachineClaim` against a warm
> `VirtualMachinePool` binds in under 5 seconds to a member that (a) reports
> `GuestReady`, (b) has no install media attached, (c) publishes its vTPM EK
> certificate, and (d) is gone from vCenter within one reconcile of the claim
> being deleted or expiring. A nightly `VMImage` rebuild rolls the warm set
> without `status.available` dropping below `warmReplicas`.

Baseline when written: `badc698` (2026-09). Re-based onto `8360e19`
(2026-09-18) when it landed in the repo; the deltas from that re-base are in
[Repo reality](#repo-reality-at-8360e19) below, and they change the running
order.

## Repo reality at `8360e19`

Three things differ from what the phase bodies below assume. They are
recorded here rather than edited into each section, so the original reasoning
stays readable.

1. **First live target is libvirt, not vSphere.** Maintainer decision
   (2026-09-18). Every phase below that says "vCenter" has a libvirt
   equivalent called out in phase D; the pool and claim layers (B1, B2) are
   backend-neutral and unchanged.
2. **The libvirt machine path is half-built.** As written, there was neither
   a `LibvirtMachine` CRD nor any domain procedure —
   `crates/banlieue-libvirt/src/procs.rs` had connect, network listing and
   storage volumes only. **Updated 2026-09-19:**
   [ADR-0050](../../docs/adr/0050-libvirtmachine-domain-lifecycle.md)
   (Accepted 2026-09-19) brought the domain lifecycle (lookup, define, create, shutdown, destroy, undefine
   — always `MANAGED_SAVE|NVRAM|TPM` — get_state, interface_addresses, with a
   live lifecycle test), and the `LibvirtMachine`/`LibvirtMachineTemplate`
   CRDs now exist and generate.
   **What phase D still needs is `reconciler/libvirt_machine.rs`** — plus
   the NoCloud cloud-init ISO and the deletion finalizer. The CRD, the
   `xml/` domain builder and the wire procedures are all in place; nothing
   drives them from a CR yet.
   **Phase D remains a prerequisite of live-testing anything here**: a pool
   whose members cannot be realised on the backend tests nothing.
3. **ADR numbering (resolved 2026-09-19).** `0041` and `0042` had each been
   used twice. The collisions are gone: the earlier-dated Accepted file kept
   each number, and the two this roadmap cites were renumbered —
   trusted-boot/UKI is now **ADR-0051**, Instant Clone is now **ADR-0052**.
   Every reference in this document has been updated accordingly.

   **Numbers 0043–0049 remain reserved by this roadmap** (phase table below)
   and are still free in `docs/adr/`. With 0050–0052 taken, an ADR *unrelated
   to this roadmap* starts at **0053**.

## Fixed constraints (not up for re-litigation here)

| Constraint | Source |
|---|---|
| One VM per identity. No multiplexing identities behind a shared guest kernel. | design decision |
| TPM-sealed Kairos partitions are mandatory, so `installMode: Deferred` is mandatory. Encryption is install-phase-only in Kairos; an `Immediate` template clone can never be encrypted per-VM. | ADR-0040 |
| No Instant Clone. Incompatible with a vTPM on the fork source. | ADR-0052 |
| Trusted Boot/UKI is experimental on vSphere: about 3 minutes of silent stalls per boot. Sandboxes use the classic GRUB image until section 0 says otherwise. | ADR-0051 |
| CR-to-CR only. Pools create `VirtualMachine`s; they never talk to a provider. | 01-DECISIONS |
| No nested virtualization. | environment |

Consequence: the only way to get low claim latency is to pay the Deferred
install ahead of demand. That is what a pool is.

---

## 0. Deferred: test a super-slim Kairos image

> **Status: deferred, 2026-09-18.** Not being run now. The libvirt hosts this
> roadmap is first being tested against have no vTPM (swtpm) configured yet,
> and the experiment's two rows that matter (UKI boot stall, Deferred install
> time) are both measured on a TPM-enabled, Trusted Boot guest. Revisit once
> swtpm is set up per phase D, or once there is vSphere time to spare — it is
> a day of work and it is independent of every phase below.
>
> **What deferring costs.** Sandboxes stay on the classic GRUB image, which
> was already the plan (ADR-0051). The sizing in phase B keeps using measured
> install time rather than a slimmed one, so `warmReplicas` is sized
> pessimistically. Nothing below is blocked.

The experiment, for when it is picked back up: it changes two numbers the rest
of this roadmap is sized around — the UKI boot stall and the Deferred install
time.

### 0.1 Can Kairos build a UKI that does *not* embed the whole rootfs?

No. There is no flag for it, and it is not an oversight.

`auroraboot build-uki` unpacks the source image, copies the kernel out, and
then builds the initramfs **from the entire remaining rootfs**
(`createInitramfs(sourceDir, ...)` in the build-uki action). `--output-type`
(`iso`, `container`, `uki`) only changes the wrapper around the same `.efi`.
`--overlay-rootfs` adds to the rootfs, `--overlay-iso` adds to the ISO
filesystem, `--single-efi-cmdline` / `--extend-cmdline` change cmdlines. None
of them changes what goes in the initrd. In Kairos Trusted Boot the signed,
measured unit *is* the operating system, which then runs from RAM.

What upstream offers instead:

1. **A smaller rootfs.** Kairos's own `-uki` artifacts drop kernel modules and
   firmware to land around 200 to 300 MB, because many EFI firmwares refuse
   files over 300 to 400 MB, and the project has seen hard limits around
   800 MB to 1 GB.
2. **Signed sysexts.** Anything that is not needed to reach the real system
   goes in a `*.sysext.raw`, signed with the same db key as the UKI, verified
   under Trusted Boot, and overlaid on `/usr` after boot. This is the only
   supported way to ship bulk *outside* the UKI while keeping it verified.
3. **Hadron**, Kairos's own minimal base, built for exactly this.

The model you are describing (small UKI with kernel plus a normal initrd, and
a dm-verity rootfs partition whose root hash is on the signed cmdline) is
systemd's reference design and what `ukify` plus `mkosi` produce. It is not
something Kairos builds today. Adopting it means leaving `kairos-agent
install`, upgrades, and `kcrypt` behind, so it is out of scope here.

### 0.2 Why size is the prime suspect for the 3 minute stall

ADR-0051 recorded multi-minute silence at shim → systemd-boot → UKI with no
vTPM traffic and no disk I/O in `vmware.log`. That is what firmware-side
hashing looks like: under Secure Boot the firmware Authenticode-hashes the
whole PE image before running it, then the stub measures its sections into
PCR 11, all at EFI speed, all in guest memory after the file has already been
read. Time should scale with UKI bytes. This test confirms or kills that
hypothesis with two data points.

### 0.3 Baseline: measure what you have

```bash
# UKI size inside the current ISO
mkdir -p /tmp/iso && sudo mount -o loop,ro current-uki.iso /tmp/iso
sudo mount -o loop,ro /tmp/iso/efiboot.img /tmp/esp 2>/dev/null || true
find /tmp/iso /tmp/esp -iname '*.efi' -size +20M -exec ls -lh {} \;
```

On a VM booted from it, let systemd split the time for you. With systemd-boot
on EFI this reports firmware and loader separately from kernel and initrd:

```bash
systemd-analyze
# Startup finished in 1min 52s (firmware) + 48s (loader) + 3.1s (kernel) + ...
systemd-analyze blame | head -20
sudo tpm2_eventlog /sys/kernel/security/tpm0/binary_bios_measurements | grep -c EventNum
```

`firmware` + `loader` is the number to beat. If it is most of the 3 minutes,
the guest kernel is not the problem and no amount of serial-console reading
will find it.

For wall-clock confirmation from outside the guest, add the file-backed serial
port ADR-0051 already planned, before first power-on:

```bash
govc device.serial.add -vm "$VM"
govc device.serial.connect -vm "$VM" -          # "-" = file-backed, in the VM's log dir
govc vm.power -on "$VM"
govc datastore.tail -f "$VM/serialport-9000.log" | ts '%H:%M:%.S'
```

Record: power-on to first kernel line, first kernel line to login.

### 0.4 Build the slim image

Two variants. Build both; the comparison is the experiment.

**Variant A, Hadron core.** Note ADR-0051: a Hadron image failed to boot
cleanly once on this vCenter and was not diagnosed. Treat a repeat as a
finding, capture serial, move on to variant B.

```dockerfile
# Dockerfile.hadron-slim   (pin all three tags to current releases)
FROM quay.io/kairos/kairos-init:v0.6.8 AS kairos-init
FROM ghcr.io/kairos-io/hadron-trusted:v0.0.1-beta2 AS base
ARG VERSION
# No --provider: a sandbox needs no k3s/k0s in the measured image.
RUN --mount=type=bind,from=kairos-init,src=/kairos-init,dst=/kairos-init \
    eval /kairos-init -l debug -s install --trusted true --version \"${VERSION}\" && \
    eval /kairos-init -l debug -s init    --trusted true --version \"${VERSION}\"
```

**Variant B, your current Debian base, stripped.** Same `vm-build` Dockerfile,
plus a final layer:

```dockerfile
# after kairos-init has run
RUN apt-get remove -y --purge 'linux-firmware*' 'firmware-*' || true && \
    apt-get autoremove -y --purge && \
    rm -rf /usr/share/doc /usr/share/man /usr/share/locale /usr/share/info \
           /var/lib/apt/lists/* /var/cache/* /usr/lib/x86_64-linux-gnu/dri && \
    KVER="$(ls /lib/modules | head -1)" && cd "/lib/modules/${KVER}/kernel" && \
    rm -rf sound drivers/gpu drivers/media drivers/net/wireless drivers/bluetooth \
           drivers/infiniband drivers/staging drivers/isdn drivers/usb/gadget \
           drivers/net/ethernet/{mellanox,intel,broadcom,chelsio,qlogic,netronome} \
           net/mac80211 net/wireless net/bluetooth fs/{btrfs,xfs,ocfs2,gfs2,ceph,nfs,nfsd,cifs,smb} && \
    depmod -a "${KVER}"
```

Do **not** strip these. The first four are how a Deferred install finds its
own ISO; losing them produces an installer that boots and then cannot see its
source:

| Keep | Why |
|---|---|
| `sr_mod`, `cdrom`, `isofs` | the install ISO is a CD-ROM |
| `ahci`, `ata_piix`, `libata` | the controller that CD-ROM hangs off |
| `vmw_pvscsi`, `sd_mod`, `nvme` | the target disk |
| `vmxnet3`, `vmw_vmci`, `vmw_vsock_vmci_transport`, `vmw_balloon` | vSphere guest basics, guestinfo |
| `dm_mod`, `dm_crypt`, `tpm_crb`, `tpm_tis`, `efivarfs` | `kcrypt` and the vTPM |
| `loop`, `squashfs`, `overlay`, `ext4`, `vfat`, `nls_*` | Kairos boot and install |
| `virtio_*`, `virtio_scsi`, `virtio_net` | keep now, so the same image serves libvirt/Proxmox in phases D/E |

Build, then look at the number that matters before going anywhere near
vCenter:

```bash
docker build -f Dockerfile.hadron-slim -t sandbox-slim:0.1.0 --build-arg VERSION=0.1.0 .
docker run --rm sandbox-slim:0.1.0 du -xsh / 2>/dev/null     # rootfs ~ UKI initrd before compression

docker run -v /var/run/docker.sock:/var/run/docker.sock \
  -v "$PWD/build":/output -v "$PWD/keys":/keys \
  quay.io/kairos/auroraboot:v0.27.0 build-uki \
  --output-dir /output/ --public-keys /keys \
  --tpm-pcr-private-key /keys/tpm2-pcr-private.pem \
  --sb-key /keys/db.key --sb-cert /keys/db.pem \
  --output-type uki sandbox-slim:0.1.0                         # bare .efi: fastest way to read the size
ls -lh build/*.efi
```

Target: **under 300 MB**. Then rebuild with `--output-type iso` for the boot
test. Build the classic (non-UKI) ISO from the same slim rootfs too
(`kairos-init ... --trusted false`, `auroraboot build-iso`): that one is what
phases A to C actually run on, and its install time is the pool's refill time.

### 0.5 Test matrix

Same VMClass, same datastore, same host, `tpmEnabled: true`, three runs each.

| # | Image | Measure |
|---|---|---|
| 1 | current UKI (baseline) | `systemd-analyze` firmware + loader; wall clock to login |
| 2 | slim UKI | same |
| 3 | current classic ISO, Deferred install | power-on → `guestinfo.banlieue.phase=installed` |
| 4 | slim classic ISO, Deferred install | same |

Through banlieue this is two extra `VMImage` objects (copy example 14 for UKI,
example 13 for classic, point `source.url` at the slim OCI ref) and four
`VirtualMachine`s.

### 0.6 Reading the result

- **Row 2 much faster than row 1, roughly in proportion to size.** Firmware
  hashing confirmed. The slim UKI is the fix; move anything else you need
  into signed sysexts. Trusted Boot comes back on the table for sandboxes,
  and with it measured boot for attestation (phase F gets stronger).
- **Row 2 about the same as row 1.** Size is not it. The serial log from 0.3
  now says which stage owns the time; file upstream with that log. Sandboxes
  stay on classic GRUB.
- **Row 4 vs row 3.** Whatever this ratio is, it multiplies straight into
  `warmReplicas` (see sizing in phase B). Install time is dominated by copying
  the image into active, passive and recovery, so expect it to track rootfs
  size closely.

---

## Phases

Order is the critical path. A2 gates B; nothing else gates anything —
*for correctness*. For **live testing**, phase D also gates everything,
because until `LibvirtMachine` exists a pool member is a `VirtualMachine` no
provider can realise (see [Repo reality](#repo-reality-at-8360e19)).

| Phase | What | ADR | Status |
|---|---|---|---|
| 0 | Slim image experiment | none (no code) | ⏸️ deferred (no vTPM on the libvirt hosts yet) |
| A2 | `GuestReady`: the installed guest reports in | 0043 | 🔶 libvirt implemented and the **read path is now verified live** against a real `qemu-guest-agent` (the seed installs it, so no special image is needed). Open: a Kairos image with the phase layer, to prove the marker is written at the right *moment*; vSphere transport deferred |
| A4 | Detach install media once installed | 0044 | ⛔ |
| A5 | vTPM EK certificate in machine status | 0045 | ⛔ — **now the gate for F** (ADR-0049 Decision 4 verifies quotes against it) |
| A3 | `tpmEnabled` requires `installMode: Deferred` | 0048 | ⛔ |
| B1 | `VirtualMachinePool` | 0046 | ✅ landed and validated e2e — fills, self-heals, rolls, cascades on delete |
| B2 | `VirtualMachineClaim` | 0047 | ✅ landed — bind/hold/release, TTL expiry, finalizer, nonce; a pool is now consumable |
| C | In-guest agent (separate repo) | own repo | ⛔ |
| D | libvirt provider: `LibvirtMachine` reconciler | 07 + 0050 + 0054 | ✅ complete — CRD, domain XML, reconciler, NoCloud user-data; roadmap 07 closed |
| E | Proxmox provider, same | amend 12 | ⛔ |
| F | Attestation trust anchors, threat model | 0049 | 📄 ADR-0049 written (Proposed); **blocked on A5** — without the EK certificate on the claim there is nothing to verify a quote against |

Per `rules/architecture-driven-development.md` each ADR lands before its
code. Skeleton decisions are below so the ADRs are an hour each, not a day.

### A2: `GuestReady` (ADR-0043)

**Problem.** `provisioned=true` fires when `CloneVM_Task` + power-on succeed.
For a Deferred image that is the moment the install *starts* (ADR-0040
Decision 4 documents the gap and defers it). A pool that trusted it would
hand out VMs mid-install.

**Why not VMware Tools heartbeat.** The live installer environment can run
Tools too. A heartbeat proves a guest is up, not that it is the installed one.
The same objection sinks a `qemu-guest-agent` ping, a DHCP lease and an open
SSH port: all are satisfied by the installer while it is still overwriting
the disk.

**Landed 2026-09-20, libvirt only.** `common::condition_types::GUEST_READY`,
`LibvirtMachineStatus.guestInstalled` (sticky), the marker read in
`crates/banlieue-provider-libvirt/src/guest.rs`, and conditional mirroring in
`status_mirror.rs`. The libvirt transport needed a **second RPC program** —
`virDomainQemuAgentCommand` lives in `0x2000_8087`, not the remote program —
verified against a real libvirtd in
`crates/banlieue-libvirt/tests/live_libvirtd.rs`
(`qemu_agent_program_is_understood_by_real_libvirtd`), which asserts libvirtd
returns a *semantic* error rather than a protocol one.

Two things beyond the skeleton: the mirror publishes `GuestReady` **only when
the provider does**, because `readiness_signal_absent` decides by condition
type and a blanket `False` would turn "this will never warm" into "wait
longer"; and `examples/16-cloud-config-guest-phase.yaml` keeps the vSphere
stanza commented out so nobody announces into a channel nothing reads.

**Still open:**

1. ~~**The read path is unverified against a real guest agent.**~~
   **Closed 2026-09-21.** `tests/live_guest.rs` passes against a real
   host: agent ping, `guest-file-open`/`read`/`close`, and both halves of
   the tri-state (`NotAnnounced` with the marker absent, `Installed` once
   it is written through the agent).

   The unlock was to stop waiting for an image that ships
   `qemu-guest-agent` — neither the Kairos build nor Debian's
   `genericcloud` does — and have the test **install it at boot through
   the NoCloud seed** (ADR-0054). Any cloud-init image that can reach a
   package mirror now works.

   The first green run cost two real bugs, neither of which any offline
   test could have found:

   - **Overlays declared `raw` over a `.qcow2` backing image.**
     `ensure_disks` passed the constant instead of calling
     `backing_format()` — a function that existed, was documented and was
     unit-tested, but was never called. libvirt does not probe a backing
     file, so it accepted the lie and every guest read a qcow2 header as
     its partition table. Nothing booted, and nothing said so: the
     pool/claim e2e uses `InfrastructureReady`, which fires when the
     *domain* runs, not when the *guest* boots.
   - **`probe_guest` conflated "no marker" with "no agent."** libvirtd
     turns an agent-level error into an RPC fault rather than an `Ok`
     carrying JSON, so a marker that did not exist yet read as
     `AgentUnreachable` — inverting the requeue cadence Decision 8 rests
     on, for the entire install window of every Deferred member. It now
     pings to classify the failure.

   What this still does **not** prove: that a real Kairos/immucore
   `Deferred` install writes the marker at the right *moment* — that its
   `/run/cos/active_mode` guard keeps it out of the live installer. That
   needs an image built with the phase layer and is a separate gap.
2. **The vSphere half** (`guestinfo.banlieue.phase` read from
   `config.extraConfig`), deferred for want of a vCenter to verify against.

Side finding, which retires an earlier suspicion: a qcow2 overlay over a
**raw** backing volume *does* boot. The guest reached the network in
`live_guest.rs`, so the "BackingFile does not boot" note recorded earlier
was an artefact of that test's upload path, not of the shipped one.

**Decision.**
1. The installed system writes `guestinfo.banlieue.phase=installed` on every
   boot, from a cloud-config `boot` stage guarded on immucore's
   `/run/cos/active_mode` / `passive_mode` sentinels (so it never runs in the
   live installer or recovery). See `examples/16-cloud-config-guest-phase.yaml`.
2. New `VSphereClient::guest_info(vm, key)`; reads `config.extraConfig`.
3. `VSphereMachineStatus.guestInstalled: Option<bool>`, sticky once true
   (the runtime guestinfo value does not survive a power cycle, and a stopped
   VM has not become uninstalled).
4. New shared condition type `GuestReady` in `common::condition_types`,
   mirrored to `VirtualMachine` by `status_mirror.rs`. `Ready` does **not**
   start depending on it: that would change the meaning of `Ready` for every
   existing `Immediate`-mode VM whose image has no phase stage.
5. While `guestInstalled != true`, requeue at `REQUEUE_DEFAULT_SECS`, not
   `REQUEUE_LONG_SECS`.

This is a liveness signal from a fresh, unclaimed guest. It is not an
integrity signal and must never be read as one. Backend-neutral by design:
libvirt and Proxmox will satisfy the same condition from `qemu-guest-agent`
(`guest-exec` of a marker check, or simply `guest-ping` plus a marker file
read), with no change to anything above the provider.

Code: `crates/banlieue-provider-vsphere/src/guest_state_additions.rs`
sections 1 to 4, including the tested `next_guest_step` ordering function.

### A4: Detach install media (ADR-0044)

A Deferred clone keeps the ISO attached forever today (`clone_vm` carries the
template's CD-ROM over; only the `Immediate` template build strips it). That
ISO carries the baked cloud-config overlay. A sandbox workload should not be
able to read it.

**Decision.** `VSphereClient::detach_install_media`, called when
`guestInstalled` flips true, recorded as `status.installMediaDetached`.
`GuestReady=True` is set only **after** the detach (`next_guest_step`
enforces this), so a pool can never bind a member with media attached.
Refactor `import_iso_template`'s inline remove-CD-ROM block onto the same
`build_remove_cdroms_reconfigure_spec` helper.

Verify live that removing a SATA CD-ROM from a running VM is accepted on your
hardware version. If vCenter insists on powered-off, fall back to disconnect
(`connectable.connected=false`, `startConnected=false`) plus clearing the ISO
backing, which is always hot-safe, and do the real remove at next power-off.

### A5: vTPM EK certificate in status (ADR-0045)

vCenter issues an endorsement key certificate for each vTPM. vim_rs 0.6
exposes it as `VirtualTpm.endorsement_key_certificate: Option<Vec<Vec<u8>>>`
(DER). Publish it, PEM-encoded, as
`VSphereMachineStatus.tpmEndorsementCertificates`, mirror through
`VirtualMachineStatus`, and from there onto a bound claim.

Recorded **before** the install is even finished (first step in
`next_guest_step`), so the cert-to-VM binding is established while the VM is
still something only banlieue has touched. A verifier then checks: the quote
is signed by an AK certified under this EK; this EK belongs to the VM the
claim is bound to; banlieue created that VM from a known image revision.
Provenance by construction, which is what you have without measured boot.

Public material. No RBAC change beyond what already reads machine status.

### A3: `tpmEnabled` requires `Deferred` (ADR-0048)

ADR-0040 Decision 5 left this open: `tpmEnabled: true` + `Immediate` clones
fine, attaches a vTPM, and silently encrypts nothing. With sandboxes that
silence is a security defect, not a papercut.

**Decision.** In `banlieue-controller`'s `VirtualMachine` reconcile, after
class and image are resolved and before scheduling (the only place both are
known), a pure check:

```rust
/// `Some(message)` when this class/image pairing would attach a vTPM that
/// nothing ever seals to (ADR-0048).
#[must_use]
pub fn image_class_mismatch(tpm_enabled: bool, mode: InstallMode) -> Option<&'static str> {
    (tpm_enabled && mode == InstallMode::Immediate).then_some(
        "VMClass.tpmEnabled requires a VMImage with installMode: Deferred; \
         an Immediate (pre-installed) template cannot be encrypted per VM (ADR-0040)",
    )
}
```

On `Some`, publish `Ready=False`, reason `ImageClassMismatch`, do **not**
create the infra CR, requeue long (the existing VMImage/VMClass watches
re-trigger on fix). `Manual` passes: it is the documented escape hatch.

### B1: `VirtualMachinePool` (ADR-0046)

**Why in banlieue and not a sister project.** It creates nothing but
`VirtualMachine`s and needs nothing but their conditions and labels, so it is
a consumer of the public API either way. It lives in `banlieue-controller`
because pooling a slow-to-provision VM is a generic need (CI runners, VDI,
these sandboxes) and because the image-rollout logic wants the `VMImage`
watch the controller already has. The identity, attestation and agent logic
in phase C is the part that is sandbox-specific, and that stays out.

**Why there is no `VSpherePool`.** Nothing about pooling is provider-specific.
Members are ordinary `VirtualMachine`s; the scheduler and providers do not
know pools exist. A per-provider pool kind would be three reconcilers for zero
behavior, and would be the first place the abstraction leaked. (If what you
want later is the CAPI `MachinePool` infra contract for worker nodes, that is
a different thing with a different name, `VSphereMachinePool`, and a
different roadmap.)

**Decisions.**
- Sizing knobs: `warmReplicas`, `maxReplicas`, `maxSurge`.
- Hygiene: `provisioningTimeoutSeconds` (poisoned members are deleted, never
  repaired), `maxIdleSeconds`, `recycleOnImageChange`.
- Rollout is surge-style: stale warm members are retired only as fresh ones
  become Ready. Image revision = `status.buildArtifact.osArtifactUid`, falling
  back to `metadata.generation`.
- Inline IPv4 range addressing, because ADR-0033 IPAM is deferred and members
  need static addresses now. Gains a `poolRef` alternative when 50 lands.
- `readiness: GuestReady` by default. `InfrastructureReady` exists for
  `Immediate` images and is documented as wrong for Deferred ones.
- Members get `generateName`, not an index. Cattle.
- `.owns(VirtualMachine)` plus a `VMImage` watch mapped to pools by
  `template.spec.imageRef.name`, same shape as the existing VM controller's
  watches. Periodic requeue covers the time-based rules no event announces.

**Sizing.** `warmReplicas ≥ peak claims/min × install minutes`.
`maxReplicas ≥ warmReplicas + peak concurrent claims + maxSurge`. Without that
last term a rollout has to trade warm capacity for replacements; the planner
handles it, but `available` dips. `maxSurge` is a host-protection knob: each
provisioning member is a full OS install's worth of datastore and CPU load on
hosts that are also running claimed sandboxes.

Code:
- `crates/banlieue-api/src/banlieue/virtualmachinepool.rs` (+ tests): both CRDs.
- `crates/banlieue-controller/src/reconciler/pool_plan.rs` (+ 15 tests,
  **compiled and passing**): every decision, no I/O. Six invariants in the
  module doc; the sweep test checks the three that matter most across about
  2,600 combinations.
- `crates/banlieue-controller/src/reconciler/pool.rs`: gather, plan, apply,
  publish.
- `examples/15-virtualmachinepool-sandbox.yaml`.

Wiring left to do: `pub mod pool; pub mod pool_plan; pub mod claim;` in
`reconciler/mod.rs`; re-exports in `banlieue-api/src/banlieue/mod.rs`; two
`Controller::new` blocks in `app.rs` following the `VSphereCluster` one;
`crdgen`; RBAC for the two kinds plus `create/delete/patch` on
`virtualmachines`; `getrandom` in `[workspace.dependencies]`; CALM update.

### B2: `VirtualMachineClaim` (ADR-0047)

**The rule.** A member is bound at most once in its life. Release is always
deletion. There is no unbind and no "return to pool".

**Decisions.**
- Bind = JSON merge patch on the member carrying its `resourceVersion`:
  claim label, subject annotations, and `ownerReferences` re-parented from
  pool to claim. Two claims racing for one member get one success and one
  409; the loser picks again. No locks, no leader-only assumptions.
- Pick order: fresh image revision first, then longest-Ready, then name. A
  stale-revision member is still bound if nothing fresh is Ready; its life is
  bounded by the TTL.
- `ttlSeconds` is mandatory. Expiry deletes the member, then the claim.
- Claim finalizer holds until the member is actually gone from the API
  server, which (through ADR-0026's machine finalizer) means gone from
  vCenter. "Claim deleted" must mean "sandbox destroyed".
- A bound member that vanishes makes the claim `Failed`, terminally. A claim
  is never silently rebound to a different VM.
- `status.nonce`: 128 random bits, not secret, for the consumer's attestation
  exchange to echo so quotes cannot be replayed across claims.
- The claim carries `subject.issuer` + `subject.id` for audit. It never
  carries a token. `guestinfo` is readable by anyone with vCenter read access
  and by processes in the guest; credentials travel only over the phase C
  attested channel.
- Who may create claims is a Kubernetes RBAC question; a
  `ValidatingAdmissionPolicy` alongside ADR-0007's pins `subject` to the
  authenticated caller for callers that are not the broker service account.

**Landed 2026-09-20.** `crates/banlieue-api/src/banlieue/virtualmachineclaim.rs`
(CRD), `crates/banlieue-controller/src/reconciler/claim_plan.rs` (every
decision, pure) and `claim.rs` (the reconciler: gather, apply, report).
Guide: `docs/src/guides/virtualmachine-claims.md`; example
`examples/19-virtualmachineclaim.yaml`.

Two things landed beyond the skeleton above:

- **`Releasing` is a real phase.** The skeleton let expiry jump straight to
  deletion, which left the variant dead and gave a consumer no way to tell
  "being torn down" from "gone". Release now publishes `Releasing` with
  `Released` or `Expired` as the reason, so those three endings are
  distinguishable — they differ in *who* ended the hold.
- **A waiting claim repeats the pool's own diagnosis.** Decision 11 asked
  for "no capacity" and "pool misconfigured" to be distinguishable without
  reading two objects, so `Pending` carries `PoolNotFound`, or
  `NoMemberAvailable` with the pool's `Warm` reason inlined — which is what
  makes a pool stuck on `ReadinessSignalAbsent` visible from the claim.

~~Still open: nothing yet pins `subject` to the authenticated caller.~~
**Closed.** `deploy/admission/virtualmachineclaim-subject-authorization.yaml`
implements Decision 10: `spec.subject.id` is checked against
`request.userInfo.username` (once the cluster's username prefix is applied),
`spec.subject.issuer` against an operator-supplied allowlist in ConfigMap
`banlieue-claim-subject-policy`, with a broker service account exempt from the
id check because handing sandboxes out on behalf of other people is its job.

Note what the split buys and what it does not: the API server vouches for the
username, so the `id` check is a real binding, but nothing can verify which
issuer minted the caller's token — the `issuer` allowlist only stops a claim
naming an issuer the site does not use. That asymmetry is now modelled as
**TB-7** in the threat model, with the `usernamePrefix` operator obligation
recorded alongside it.

### C: In-guest agent (separate repo under `firestoned`)

Out of banlieue. Baked into the sandbox image via `vm-build` and one
`cloudConfigs` layer. Starts only from the installed system.

1. Boot: generate a keypair and a TPM AK. Never in the image, never before
   install.
2. Listen on mTLS. On connect from the broker: receive `{claim, nonce, JWT}`,
   return a TPM quote over the nonce. Broker verifies against the EK
   certificate in the claim's status (A5).
3. Validate the JWT itself: issuer, audience, expiry, and that `oid` matches
   the claim's `subject.id`.
4. Run the AI agent as a per-lease uid inside nsjail or bubblewrap. Here a
   jail is the right tool: same identity on both sides, so an escape crosses
   no trust boundary. Workspace on the sealed `COS_PERSISTENT`. Egress through
   an allowlist proxy only.
5. On release or TTL: power off. The claim controller deletes the VM.

### D: libvirt (amend roadmap 07)

State at `badc698`: `Provider` and `VMImage` reconcilers and an own-protocol
client exist; there is no `LibvirtMachine` CRD, and `banlieue-libvirt/procs.rs`
has connect and storage procedures but no domain procedures.

Deferred mode makes this provider *simpler* than roadmap 07 assumes: there is
no backing-file template clone at all for TPM classes. Create an empty volume,
attach the ISO the `VMImage` reconciler already uploads, add the TPM, boot.

Add to roadmap 07's task list:
- ~~Domain procedures: define XML, create, destroy, undefine **with
  `VIR_DOMAIN_UNDEFINE_NVRAM | VIR_DOMAIN_UNDEFINE_TPM`** (otherwise swtpm
  state and the vars file leak per deleted sandbox), get state, interface
  addresses.~~ **Done 2026-09-18** (ADR-0050) — `domain_undefine` takes no
  flags parameter and unconditionally passes `MANAGED_SAVE|NVRAM|TPM`, so the
  swtpm/vars leak is closed by construction.
- ~~The `LibvirtMachine` CRD itself (`banlieue-api`).~~ **Done** — both
  `LibvirtMachine` and `LibvirtMachineTemplate` exist and generate.
- **Still to do: the `LibvirtMachine` reconciler** in
  `banlieue-provider-libvirt` (`xml/domain.rs` — only `xml/escape.rs` exists
  — NoCloud ISO, deletion finalizer). The CRD and the procedures are both in
  place; nothing drives them from a CR yet.
- `<tpm model='tpm-crb'><backend type='emulator' version='2.0'/></tpm>`;
  advertise `FEATURE_VTPM`. swtpm state is keyed by domain UUID, so a new
  domain is a new TPM.
- OVMF with a vars template that has no enrolled keys, only if section 0 brings
  UKI back.
- `GuestReady` from `qemu-guest-agent`; `detach_install_media` =
  `virDomainUpdateDeviceFlags` ejecting the cdrom.
- EK certificates: read from swtpm's `swtpm_localca`-issued cert; trust anchor
  is per host (phase F).

### E: Proxmox (amend roadmap 06)

No crate yet. Roadmap 06 assumes template clone; for TPM classes replace that
with create-from-ISO. Rule to write down: **never clone a VM that has a
`tpmstate0` volume**, a full clone copies it, which is ADR-0040's shared-vTPM
problem again. `tpmstate0` v2.0 is added at create. `efidisk0` with
`pre-enrolled-keys=0` only if UKI returns. `GuestReady` from the agent API;
media detach is `ide2: none`; destroy with purge.

### F: Attestation anchors and threat model (ADR-0049)

- EK trust roots differ by backend: vCenter-issued on vSphere, per-host
  `swtpm_localca` on libvirt and Proxmox. Add
  `Provider.spec.attestation.ekTrustBundle` (a `CABundleSource`, same shape as
  `connection.caBundle`). Explicit, admin-supplied, not discovered.
- Extend the threat model (#39) with: prompt-injected agent as the adversary
  inside the guest; credential theft via guestinfo (mitigated: never there);
  pool poisoning via a member that lies about `installed` (bounded: it is
  pre-claim, and attestation does not depend on that signal); stale-image
  members (bounded by `maxIdleSeconds` and rollout); claim subject spoofing
  (admission policy).
- vSphere VM Encryption on the sandbox storage class so that deleting the VM
  is a cryptographic erase at the datastore layer as well.

## Definition of done

- [ ] Section 0 matrix filled in and linked from ADR-0051.
- [ ] ADRs 0043 to 0048 accepted; CALM updated.
- [ ] `examples/15` applied live: pool reaches `Warm=True`; a claim binds in
      under 5 s; `govc device.ls` on the bound member shows no CD-ROM;
      `status.tpmEndorsementCertificates` parses with `openssl x509`.
- [ ] Two claims created in the same second bind two different members.
- [ ] Deleting a claim removes the VM from vCenter before the claim object
      disappears.
- [ ] Bumping the `VMImage` rolls the pool with `available ≥ warmReplicas`
      throughout (given the `maxReplicas` headroom above).
- [ ] A member whose install is sabotaged (detach the ISO mid-install) is
      deleted at `provisioningTimeoutSeconds` and replaced.
- [ ] `tpmEnabled` class + `Immediate` image yields `ImageClassMismatch` and no
      infra CR.
- [ ] `ROADMAPS.md` rows for 06, 07 and 17 reflect reality — keep them
      current as phases land.

## Gotchas

- Address ranges need spares beyond `maxReplicas`: an address is held until a
  deleted member's backend VM is really gone, not until its delete is issued.
- `guestinfo` values set from inside the guest are runtime-only. The phase
  stage must run every boot, and `guestInstalled` must be sticky.
- Do not make `Ready` depend on `GuestReady`. Existing `Immediate` VMs have no
  phase stage and would all go unready.
- Re-parenting `ownerReferences` at bind is what lets a pool be deleted
  without killing sandboxes that are in use. Test it explicitly.
- `maxSurge` protects hosts running claimed sandboxes from refill storms after
  a claim burst. Raising it to refill faster is usually the wrong fix; raise
  `warmReplicas` or shorten the install (section 0) instead.
