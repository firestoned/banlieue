# 15: vSphere disk-image import (raw and VMDK), alongside the ISO path

> **Goal.** A `Url`-source `VMImage` can be imported into every vSphere
> failure domain from a **disk image** (the `cloudImage` raw artifact
> `banlieue-imagebuilder` already builds for libvirt) by writing a VMDK into
> the zone's datastore and marking a never-booted VM as a template. No install
> boot inside the import Job, no ISO, no CD-ROM.
>
> **Stop condition.** A `VMImage` with `spec.template.importMethod: DiskImage`
> reaches `Ready` in every failure domain of a vSphere `Provider` without the
> import Job ever powering a VM on. A `VirtualMachine` cloned from it boots,
> reads its `guestinfo` user-data and reports `provisioned`. Deleting the
> `VMImage` destroys the per-zone templates exactly as ADR-0028 does today. The
> provider image is still distroless, with no `qemu-img` in it.
>
> **Status: not started.** This closes the follow-up
> [ADR-0010](../../docs/adr/0010-vmimage-build-pipeline-imagebuilder.md) left
> open ("whether the upload goes through an OVF/`HttpNfcLease` import or a
> plain datastore file PUT") and that
> [ADR-0020](../../docs/adr/0020-vsphere-per-zone-iso-import.md) stepped around
> by shipping the ISO path instead.

## Why this exists

1. **The ISO path pays an install boot per zone, per build.** ADR-0021's
   `Immediate` sequence is create, reconfigure boot order, power on, poll for
   self-shutdown (8 to 12 minutes observed, 1800 s timeout), strip the CD-ROM,
   template. Every step after "create" is a failure mode ADR-0021 had to find
   live: boot order not honoured on provisional device keys, missing `users`
   halting the installer, a missing `poweroff` timing the Job out. A disk-image
   import has none of them. It uploads bytes and sets the template bit.
2. **The ISO path is Kairos-shaped.** It depends on an unattended installer
   baked into the ISO. A generic cloud image (the README's own
   `ubuntu-22.04-cloudinit`, or the Alpine template that
   `docs/src/guides/alpine-vsphere-template.md` builds by hand) has no such
   thing. Disk-image import is what makes non-Kairos `Url` sources first-class
   on vSphere.
3. **One artifact kind for every backend.** Today `banlieue-imagebuilder`
   picks `iso` for vsphere and `cloudImage` for libvirt (ADR-0020 Decision 1),
   and one `VMImage` produces one artifact. With this roadmap a `VMImage`
   carrying vsphere, libvirt and (roadmap 09) cloud-hypervisor sources can
   share a single `cloudImage` build.

## What this does not replace

**`installMode: Deferred` for `tpmEnabled` classes stays on the ISO path.**
ADR-0040 established that Kairos partition encryption is install-phase-only
and that a clone of a pre-laid disk can never be sealed to its own vTPM. A
disk image is a pre-laid disk. Until open question Q4 below says otherwise,
`DiskImage` plus `Deferred` is rejected at admission, and roadmap 17's
sandboxes keep using the ISO.

## Repo reality (at the time of writing)

| Already there | Where |
|---|---|
| Datastore `/folder` PUT with progress milestones | `upload_iso_to_datastore`, `crates/banlieue-provider-vsphere/src/import.rs` |
| `ensure_datastore_dir`, `destroy_if_present`, `find_template` | `VSphereClient` trait, `client/mod.rs` |
| Template VM config builder (EFI, `pvscsi`, `vmxnet3`, guestId, multi-NIC per ADR-0031) | `build_template_config_spec`, `client/vim.rs` |
| Fail-closed checksum verification (SEC-004) | `verify_checksum`, `import.rs` |
| Per-zone import Jobs, owned by the `OSArtifact` (ADR-0027), per-zone folders (ADR-0020 Decision 5) | `reconciler/vmimage.rs` |
| `VMImage` finalizer that destroys templates (ADR-0028) | `reconciler/vmimage.rs` |
| `grow_os_disk` on the clone path | `VSphereClient` |
| `BuildArtifactKind::{cloudImage, iso}`; kind chosen from `providerClass` | `banlieue-api`, `banlieue-imagebuilder` |

| Missing | Notes |
|---|---|
| Any VMDK code | `grep -i vmdk crates/` finds only NFC-lock comments |
| `ImportVApp` / `HttpNfcLease` usage | `vim_rs` is at 0.6; the surface needs checking (Q2) |
| A way to ask for a disk-image import on a vsphere source | kind is derived from `providerClass` alone |

## Fixed constraints (not up for re-litigation here)

| Constraint | Source |
|---|---|
| Content Library is off. Datastore and inventory templates only. | ADR-0020 Decision 3, environment |
| Storage is not shared across zones. Every zone is a separate import. | ADR-0010, roadmap 03 |
| No subprocesses and no new system packages in provider images. Distroless stays distroless. This supersedes ADR-0010's `qemu-img convert` sketch. | ADR-0011, ADR-0050, ADR-0054 |
| Bulk bytes move in Jobs, never through a reconcile loop. | ADR-0011 |
| Hand-off is PVC plus `VMImage.status`. No RPC. | ADR-0010, non-negotiable 1 |
| Verify third-party behaviour against upstream docs or source, never infer it. | `CLAUDE.md`, ADR-0010 |

---

## 0. Spike (scripts and `govc` only, nothing lands in the tree)

One or two days against one nonprod zone. Each answer goes into an ADR in
phase 1, so the ADRs are an hour each and not a day.

**Q1. What backs each zone's datastore?** VMFS and NFS accept a hand-written
descriptor plus a `-flat.vmdk` uploaded through `/folder`. vSAN and vVols do
not: a VMDK there is an object, and a file PUT of the flat extent does not
produce a usable disk. This decides whether method B below is viable anywhere
in the target environment.

**Q2. Does `vim_rs` 0.6 expose the NFC import surface?** Needed:
`ResourcePool.ImportVApp`, `HttpNfcLease` (`state`, `info.deviceUrl`,
`HttpNfcLeaseProgress`, `HttpNfcLeaseComplete`, `HttpNfcLeaseAbort`). Nice to
have: `OvfManager.CreateImportSpec`. If it is absent, build a
`VirtualMachineImportSpec` by hand around the `ConfigSpec` the template
builder already produces, which also avoids generating an OVF envelope. Use
the crate's own MCP server for this, as ADR-0010 suggested.

**Q3. What does a Kairos `cloudImage` do on its first boot under vSphere?**
Per the Kairos docs a raw image carries recovery only, boots into it, runs an
automatic reset to lay down the active system, and reboots. Measure that time
per clone, confirm `guestinfo.userdata` is read on that boot (ADR-0024,
ADR-0029), and confirm a disk grown by `grow_os_disk` before first power-on is
picked up by the reset. This number decides whether `DiskImage` is a win for
Kairos at all, see [phase 4](#4-clone-side-behaviour).

**Q4. Does that first-boot reset honour `install.encrypted_partitions` when a
vTPM is present?** ADR-0040 examined the install action and the initrd "RAM
mode" path. It did not examine the `cloudImage` reset path, where
`COS_PERSISTENT` genuinely does not exist yet. Read the `kairos-agent` reset
action, then test it. If yes, a `Deferred`-equivalent without an ISO becomes
possible and ADR-0044 (detach install media) is moot for that path. If no,
record it as an amendment to ADR-0040 and move on. Either answer is worth the
afternoon.

**Q5. Where do the bytes go, and can the Job get there?** `/folder` PUTs go
through vCenter. NFC lease URLs point at an **ESXi host** on 443. Check that
the imagebuild namespace's pods can reach the hosts, and what certificate the
hosts present (VMCA-signed, so covered by the Provider's existing CA bundle,
or self-signed, which needs thumbprint pinning).

**Exit:** a template made by hand with each viable method boots a clone.

## 1. ADRs, then CALM

ADR numbers are the next free as of 2026-09-20: `0055` went to roadmap 17's
`AgentSandbox`, so this roadmap starts at `0056`. `0043` to `0049` stay
reserved by roadmap 17. Renumber at landing if something else takes them.

### ADR-0056: `importMethod` on a vSphere template

- `VMImageTemplate.importMethod: Iso | DiskImage`, default `Iso`. Existing
  `VMImage`s do not change behaviour.
- Amends ADR-0020 Decision 1: artifact kind is chosen from `providerClass`
  **and** `importMethod` (vsphere plus `DiskImage` gives `cloudImage`).
- A `VMImage` whose sources disagree on kind (one vsphere `Iso` source, one
  libvirt source) is rejected with a clear message. One `VMImage`, one build
  artifact, stays true. Two `VMImage`s is the answer.
- `DiskImage` with `installMode: Deferred` is rejected unless Q4 came back
  positive. `installTimeoutSeconds` is ignored for `DiskImage` and says so.
- Extend the admission policy (ADR-0007) to match.

### ADR-0057: a first-party VMDK writer

Same reasoning as the ISO9660 writer in ADR-0054: a small, fully specified
binary format, every structure at a known offset, versus a system package, a
base image change and a subprocess in the import path. Decision content is
[phase 2](#2-banlieue-vmdk-first-party-pure-rust).

### ADR-0058: NFC lease import and ESXi host trust

Which method is the default, when the fallback applies, how the BYOC client
(ADR-0008) trusts an ESXi host, and the NetworkPolicy change for the
imagebuild namespace. Decision content is
[phase 3](#3-the-import-path-in-banlieue-provider-vsphere).

### CALM

New flow "disk-image import". New relationship from the import Job to the ESXi
NFC interface (it does not exist for the ISO path, which only talks to
vCenter). `make docs` validates the model.

## 2. `banlieue-vmdk`: first-party, pure Rust

New crate `crates/banlieue-vmdk`. Write-only. Two outputs.

**a. Descriptor for a flat extent.** A text file of a dozen lines:
`createType="vmfs"`, one extent `RW <sectors> VMFS "<name>-flat.vmdk"`, and
the `ddb.*` geometry block. The raw image **is** the flat extent, byte for
byte, so there is no conversion at all.

**b. `streamOptimized` sparse extent.** Sparse header (`KDMV`, version 3,
compressed-grains and markers flags), embedded descriptor, 64 KiB grains each
deflate-compressed behind a grain marker, grain tables of 512 entries, grain
directory, footer, end-of-stream marker. All-zero grains are skipped, which is
where the size win comes from.

Design rules:

- **Single pass, constant memory.** `AsyncRead` in, `AsyncWrite` out. The
  format was designed for this, and it is what lets the Job pipe PVC file to
  writer to HTTP body with no scratch volume.
- **Pure-Rust deflate** (`flate2` on its `miniz_oxide` backend). No C zlib, in
  line with ADR-0009.
- Input length not a multiple of 512 is an error. Capacity is rounded up to a
  whole grain and the tail zero-filled.
- The grain directory uses 32-bit sector offsets, which caps a stream at
  2 TiB. Reject larger inputs with a message that names the limit.

Tests, following the ADR-0054 convention:

- [ ] Byte-offset unit tests for header, markers, grain table, footer.
- [ ] A minimal in-crate reader, test-only, for round-trip assertions.
- [ ] Cases: all-zero image, single non-zero grain, non-grain-multiple length,
      image over 4 GiB (sparse test file), empty image rejected.
- [ ] **CI-only oracle:** `qemu-img check` on the output and
      `qemu-img convert` back to raw with a SHA-256 comparison. `qemu-img` is a
      test-container dependency, never a runtime one.
- [ ] Fuzz target under `.clusterfuzzlite/`: arbitrary input through the
      writer, then through the test reader, asserting round-trip equality.

**Exit:** a `streamOptimized` VMDK written by the crate imports by hand with
`govc import.vmdk` and boots.

## 3. The import path in `banlieue-provider-vsphere`

New trait method `VSphereClient::import_disk_template(&DiskImportRequest)`
next to `import_iso_template`, so both stay mockable. The `imageImport`
subcommand gains `--method disk-image` and reads `<osArtifactRef>.raw` from
the PVC. `verify_checksum` runs on the raw file **before** any conversion.

### Method A: NFC lease (default)

1. Build the template `ConfigSpec` with the existing builder, minus the
   CD-ROM, plus one disk at the image's capacity. EFI firmware is set here, not
   later.
2. `ImportVApp` against the zone's resource pool, per-zone template folder and
   datastore. Wait for the lease to reach `ready`.
3. Stream raw to `streamOptimized` straight into a `PUT` on the lease's device
   URL. Replace the `*` placeholder host with the host the lease names.
4. Call `HttpNfcLeaseProgress` on a timer for the whole upload. A lease that
   hears nothing for a few minutes times out and vCenter deletes the VM.
5. `HttpNfcLeaseComplete`, then `MarkAsTemplate`.
6. On any failure: `HttpNfcLeaseAbort`. vCenter removes the partial VM, so
   there is nothing for the Job to clean up.

Works on every datastore type, lands thin, transfers only non-zero grains.

### Method B: flat upload (fallback)

For zones on VMFS or NFS where the Job cannot reach the ESXi hosts (Q5).

1. `ensure_datastore_dir`.
2. `PUT` the raw file as `<name>-flat.vmdk` through the existing `/folder`
   uploader, generalised from `upload_iso_to_datastore` to
   `upload_file_to_datastore`. Then `PUT` the descriptor.
3. Optional `CopyVirtualDisk_Task` to a thin copy, then delete the flat one. A
   flat upload is full-size and thick, and the copy also proves the descriptor
   parses.
4. `CreateVM_Task` with a disk backed by the existing VMDK, then
   `MarkAsTemplate`.

Selected per `Provider` (`spec.diskImageTransport: Nfc | DatastoreFile`,
default `Nfc`), not guessed at runtime. Explicit over implicit.

### Shared behaviour

- **Idempotency.** The template carries the artifact checksum in an
  `extraConfig` key (`banlieue.io/artifact-checksum`). Same checksum means
  reuse. `force.reimport` keeps its destroy-then-recreate ordering, and for
  method B that ordering matters for the same NFC-lock reason the ISO path
  already documents: a template still referencing the old VMDK holds a lock on
  it.
- **Ownership and deletion.** Unchanged. ADR-0027 owner references and the
  ADR-0028 finalizer apply as they are, because a template is a template
  however its disk arrived.
- **Status.** `ZoneImageStatus` gains `importMethod` so an operator can see
  which path produced a given zone's template. Reasons `Converting` and
  `Uploading` reuse the existing progress-milestone logging.
- **RWO PVCs** still serialise zone imports, as ADR-0010 describes.

**Exit:** `examples/NN-vmimage-vsphere-disk-image.yaml` applied live reaches
`Ready` in every zone.

## 4. Clone-side behaviour

No new clone code. `clone_vm`, `grow_os_disk`, guestinfo, power on, as today.
Two consequences need writing down rather than implementing:

- **There is no CD-ROM on the template.** Nothing to detach, so ADR-0044 does
  not apply to `DiskImage` templates.
- **For Kairos, work moves from the import to each clone.** An ISO `Immediate`
  template boots straight into an installed OS. A `cloudImage` clone spends its
  first boot in the recovery reset (Q3). That is a good trade for an image
  imported nightly into many zones and cloned rarely, and a poor one for a
  pool that clones constantly. Put the measured number in the guide and let
  the operator choose. For generic cloud images the cost does not exist.
- `provisioned=true` still fires at power-on, so `GuestReady` (ADR-0043,
  roadmap 17 A2) is what tells a consumer the reset has finished.

## 5. Docs and threat model

- [ ] `guides/vsphere-provider.md` and `guides/using-banlieue-imagebuilder.md`:
      when to pick which method, with the Q3 timing.
- [ ] Rewrite `guides/alpine-vsphere-template.md` around a `DiskImage` import
      once pre-built sources land (see follow-ups). Until then, note it.
- [ ] Threat model pass (`.claude/rules/threat-modeling.md`): new egress from
      the imagebuild namespace to ESXi hosts, ESXi host certificate trust, and
      the VMDK writer as new code on the artifact path (write-only, input is
      checksum-verified before it is read).
- [ ] Update the status row in [`ROADMAPS.md`](../../ROADMAPS.md) in the same
      commit as each state change.

## Out of scope, recorded as follow-ups

- **Pre-built image sources.** A `Url` source that points at a downloadable
  raw, `streamOptimized` VMDK or OVA, with no OCI build at all. Needs a fetch
  Job and an `ImageSource.format` field, so it gets its own ADR. An OVA is the
  easy case: it already contains an OVF for `CreateImportSpec` and a
  `streamOptimized` VMDK that uploads as it is. qcow2 input would need a
  first-party reader and is not planned.
- **Content Library import**, behind the existing `useContentLibrary` toggle.
- **Proxmox.** Wants raw or qcow2, not VMDK. Nothing here blocks it.

## Tasks

- [ ] Spike, Q1 to Q5 answered in writing.
- [ ] ADR-0056, ADR-0057, ADR-0058 accepted. CALM updated.
- [ ] `VMImageTemplate.importMethod`, `ZoneImageStatus.importMethod`,
      `Provider.spec.diskImageTransport` in `banlieue-api`. `make crds`.
- [ ] Admission policy for the rejected combinations.
- [ ] `banlieue-imagebuilder`: kind selection from `providerClass` plus
      `importMethod`.
- [ ] `crates/banlieue-vmdk` with both writers and the test suite above.
- [ ] `VSphereClient::import_disk_template`, method A.
- [ ] ESXi host trust in the BYOC client per ADR-0058.
- [ ] Method B, behind the transport field.
- [ ] `imageImport --method disk-image`.
- [ ] NetworkPolicy for imagebuild namespace egress to ESXi hosts.
- [ ] Example manifest, guides, threat model.

## Definition of done

- [ ] The stop condition holds live, in every zone of a real Provider.
- [ ] A nightly rebuild re-imports every zone without a stuck PVC (ADR-0027
      still holds) and without orphaned VMDKs on any datastore.
- [ ] Import wall-clock per zone is recorded next to the ISO path's for the
      same image.
- [ ] `cargo deny` and the image scan show no new native dependency.

## Gotchas

- **Lease keepalive.** No progress call for a few minutes and the lease dies
  mid-upload. Run it on its own timer, not from the upload loop's progress
  callback, which stalls exactly when the network does.
- **The `*` host in device URLs.** vCenter returns `https://*/nfc/...`. The
  real host is in the lease info.
- **ESXi certificates are not vCenter's certificate.** A CA bundle that
  validates vCenter does not necessarily validate a host.
- **EFI must be on the import `ConfigSpec`.** A UEFI image imported as BIOS
  fails silently at a blank console.
- **Descriptor adapter type.** `pvscsi` controllers still use
  `ddb.adapterType = "lsilogic"` in the descriptor. Confirm in the spike.
- **vSAN and flat files.** Method B on vSAN produces a file that looks right
  in the datastore browser and cannot be attached.
- **Thick flat uploads.** Method B transfers and stores the full virtual size,
  zeros included. Thin it, or size the datastore for it.
- **Never power the template on.** The first boot of a `cloudImage` must be a
  clone's, or every clone inherits one machine-id and one set of SSH host
  keys, which is the problem ADR-0021's generalise step exists to prevent.
