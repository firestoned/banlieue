# 0054 — NoCloud seed ISOs are built in-process, with Joliet

- **Status:** Accepted
- **Date:** 2026-09-20
- **Proposed:** 2026-09-19
- **Notes:** Implemented and verified live;
  the evidence is in Decision 3. Completes
  [ADR-0050](0050-libvirtmachine-domain-lifecycle.md) (`LibvirtMachine`
  lifecycle) — the last functional gap in roadmap 07, now closed.

## Context

`LibvirtMachineSpec.user_data` carries the guest bootstrap payload, already
resolved from a Secret or ConfigMap and placeholder-substituted by
`banlieue-controller` (ADR-0025, ADR-0038). The libvirt provider currently
does nothing with it: `converge` passes `cidata_iso_path: None` to the
domain XML builder, so a `VirtualMachine` with `spec.userData` provisions a
domain that never receives it. Roadmap 07's stop condition — "provisioned
end-to-end **via NoCloud**" — is unmet for exactly this reason.

vSphere has no equivalent gap: it delivers user-data through
`guestinfo.userdata`, a hypervisor channel with no filesystem involved.
libvirt has no such channel, so the payload has to arrive as a disk.

### What cloud-init actually requires

From the NoCloud datasource documentation:

- the filesystem volume must be **labelled `CIDATA`**;
- the files are **`user-data`** and **`meta-data`**, in the **root
  directory**, hyphenated;
- a labelled **vfat or iso9660** filesystem may be used;
- the documented way to build one is
  `genisoimage -output seed.iso -volid cidata -joliet -rock user-data meta-data`.

Those flags are the whole problem. ISO9660 Level 1 names are 8.3, uppercase,
and drawn from a d-character set that **excludes the hyphen** — `meta-data`
is nine characters with a hyphen and cannot be expressed. A bare ISO9660
image produces something like `meta_dat.` after the kernel's own mapping,
which cloud-init does not match. An extension is not optional.

The same is true of vfat: `meta-data` exceeds 8.3, so it needs VFAT long
filename entries.

### Why not shell out

Roadmap 07 originally proposed `genisoimage` in the provider image, with a
`debian:bookworm-slim` base. ADR-0050 already rejected that shape for the
libvirt client itself, and every reason holds here: the provider image is
distroless, and adding `genisoimage` means a system package, a base image
change, and a subprocess in a reconcile path — to produce a two-file
archive of a few kilobytes.

There is also a correctness reason. A subprocess needs the payload on disk
to hand it a path, so the user-data — which is the guest's bootstrap
credential material in many deployments — would be written to the
container's filesystem on every reconcile, where it was previously only in
memory.

## Decision

1. **Build the seed image in-process, first-party**, in
   `banlieue-provider-libvirt`'s own `cloudinit` module. No `genisoimage`,
   no `xorriso`, no subprocess, no base image change. The payload never
   touches a filesystem inside the controller.

2. **ISO9660 with a Joliet supplementary descriptor**, not vfat.

   Both need an extension to express `meta-data`. Joliet is the smaller
   correct implementation: a Supplementary Volume Descriptor is close to a
   copy of the Primary one with a different escape sequence and UCS-2BE
   names, over fixed 2048-byte sectors. VFAT long filenames need cluster
   chain arithmetic, the 8.3 shortname checksum, and name fragments split
   across 13-character slots — more code, and more of it fiddly, for the
   same result.

3. **Joliet only; no Rock Ridge.** The documented command passes both, but
   they solve the same problem twice here: Linux mounts iso9660 preferring
   Joliet when present, which yields the hyphenated names cloud-init looks
   for. Rock Ridge additionally carries POSIX ownership and permissions,
   which a two-file seed consumed by a root-run datasource does not need.

   This was the one decision in this ADR that was a judgement rather than a
   constraint, so it was **verified live** rather than assumed. Confirmed
   2026-09-19 on a real host: an Alpine `nocloud_*-bios-cloudinit` guest
   mounted a banlieue-generated seed, read `meta-data`, and announced its
   `local-hostname` over DHCP — a name that exists nowhere but in that ISO.
   Independently, macOS's own ISO9660 driver reports the image as
   `Volume Name: CIDATA`, `File System Personality: ISO Joliet`, with both
   hyphenated filenames readable.

4. **A fixed, flat layout.** Root directory only, no subdirectories, a
   handful of small files. That is the entire shape a cidata seed ever has,
   and refusing to generalise keeps the writer small enough to reason
   about. Nested paths are rejected rather than silently flattened.

5. **`meta-data` is generated, not supplied.** cloud-init requires it even
   when empty, and it carries `instance-id` — which is what makes
   cloud-init re-run its per-instance modules for a genuinely new VM rather
   than treating it as a reboot. The provider derives `instance-id` from
   the domain's UUID, so a rebuilt member is a new instance by
   construction, and `local-hostname` from the domain name.

6. **The seed volume is owned by the machine.** It is created in the same
   pool, named `<domain>-cidata.iso`, and deleted by `finalize_backend`
   alongside the OS disk. It is not a shared artifact and never outlives
   its domain.

## Consequences

- Roadmap 07's stop condition becomes reachable: a `VirtualMachine` with
  `spec.userData` on libvirt boots with cloud-init having consumed it.
- The provider image stays distroless, with no new system dependency, and
  user-data stays in memory.
- banlieue owns an ISO9660 writer. It is small and fully unit-testable —
  every structure is bytes at a known offset — but it is a filesystem
  format, and the offline tests can only prove self-consistency. Only a
  guest actually booting proves it right, which is why decision 3 names
  that as the acceptance test.
- Proxmox (roadmap 06) uses NoCloud too. The module is written for libvirt
  and lives there; when Proxmox needs it, moving it to a shared crate is a
  mechanical change and should be done then, not pre-emptively.
- vfat remains available as a fallback if a guest is ever found that
  dislikes Joliet. Nothing in this decision forecloses it.
