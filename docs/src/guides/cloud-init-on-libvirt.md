# Guide: cloud-init on libvirt

How `spec.userData` reaches a guest on a libvirt host, what banlieue puts on
the seed disk, and how to tell — from outside the guest — whether cloud-init
actually consumed it.

## Why libvirt needs a disk when vSphere does not

vSphere delivers user-data through `guestinfo.userdata`: a hypervisor
channel the guest reads through VMware Tools, with no filesystem anywhere.

libvirt has no equivalent. So the payload has to arrive as a **disk** — and
the convention for that is cloud-init's
[NoCloud](https://docs.cloud-init.io/en/latest/reference/datasources/nocloud.html)
datasource: a small filesystem labelled `CIDATA` holding `user-data` and
`meta-data` at its root.

banlieue builds that image itself, in-process. There is no `genisoimage` in
the provider container and no subprocess
([ADR-0054](https://github.com/firestoned/banlieue/blob/main/docs/adr/0054-nocloud-seed-iso-first-party.md)):
the image stays distroless, and a subprocess would need the payload written
to the container's filesystem to hand it a path — which for most deployments
means writing the guest's bootstrap credentials to disk on every reconcile.

## What you write, and what the guest gets

You set user-data on the `VirtualMachine`, exactly as on any other backend:

```yaml
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: web-01
spec:
  classRef: { name: small }
  imageRef: { name: ubuntu-22.04-libvirt }
  userData:
    secretRef:
      name: web-01-cloudinit
      key: user-data
```

banlieue resolves that Secret (the *controller* does, never the provider —
the provider has no Secret access at all), substitutes placeholders, and
hands the rendered text to the libvirt provider, which turns it into a seed
volume named `<domain>-cidata.iso` in the machine's storage pool.

### The seed's contents

```text
/user-data     your payload, byte for byte
/meta-data     generated:

               instance-id: <the domain's UUID>
               local-hostname: <the domain name>
```

`meta-data` is always present, even when you set no user-data at all —
cloud-init requires it and **silently ignores a seed without it**.

!!! important "`instance-id` is the domain's UUID, deliberately"
    cloud-init keys "have I already run for this instance?" on
    `instance-id`. Deriving it from the domain UUID means a rebuilt VM — a
    replaced pool member, say — is a *new* instance by construction, so its
    per-instance modules run.

    Reusing an id across rebuilds makes a fresh VM treat its first boot as a
    reboot and skip every one of them. That failure is silent and looks like
    "cloud-init did nothing".

## Why the image is Joliet

This is the one piece of the format worth understanding, because it explains
an otherwise strange implementation choice.

cloud-init looks for files named `user-data` and `meta-data`. ISO9660
Level 1 filenames are 8.3, uppercase, and drawn from a character set that
**excludes the hyphen** — `meta-data` is nine characters with a hyphen and
cannot be written in the primary directory tree at all. It comes out as
`META_DATA.`, which cloud-init does not match.

That is why the documented way to build a seed is:

```sh
genisoimage -output seed.iso -volid cidata -joliet -rock user-data meta-data
```

The `-joliet` is not decoration. banlieue writes a Joliet supplementary
directory tree carrying the real names in UCS-2, which Linux prefers when
mounting. Rock Ridge is omitted: it solves the same problem a second way,
and additionally carries POSIX ownership that a two-file seed read by a
root-run datasource does not need.

You can inspect any seed banlieue produced:

```sh
# macOS
hdiutil attach seed.iso
diskutil info /Volumes/CIDATA | grep -E 'Volume Name|Personality'
#   Volume Name:               CIDATA
#   File System Personality:   ISO Joliet

# Linux
mount -o loop seed.iso /mnt && ls /mnt
#   meta-data  user-data
```

## Verifying a guest actually consumed it

This is harder than it sounds, because every easy signal is ambiguous. A
domain that is `Running` with no address might have booted and failed to
network, or never reached a bootloader at all.

The signal that works without shell access or a guest agent: **the DHCP
lease hostname**. The seed's `meta-data` sets `local-hostname`, cloud-init
applies it, and the guest announces it in its DHCP request, where libvirt
records it:

```sh
virsh net-dhcp-leases default
#  Expiry              MAC                IP              Hostname
#  2026-09-19 22:41:03 52:54:00:aa:bb:cc  192.0.2.168/24  web-01
```

A lease bearing the domain's name can only have come from the guest having
parsed the seed. banlieue's own test for this is
`crates/banlieue-provider-libvirt/tests/live_cloudinit.rs`.

### Not every guest can prove it

Verified against a real host — and the differences matter, because two of
these look exactly like "the seed is broken":

| Image | Boots | Announces hostname | Use for this? |
| --- | --- | --- | --- |
| Alpine `nocloud_*-bios-cloudinit` | yes | **yes** | ✅ the reliable choice |
| CirrOS 0.6.2 | yes | no — busybox `udhcpc` sends none | ❌ boots fine, proves nothing |
| Kairos | yes | its own `kairos-<hash>` | ❌ consumes yip config, not cloud-init |

A Kairos image ignoring `local-hostname` is **not** a bug in the seed.
Kairos has its own configuration system; its cloud-config is yip-format and
arrives by a different route entirely.

## Two ways to get an unbootable VM

Both produce a domain that reports `Running` forever with no address, which
is indistinguishable from a guest that ignored its user-data. Neither is
banlieue reporting something wrong — the VM genuinely is running, it just
never reached a bootloader.

**Firmware that does not match the image.** Kairos builds are EFI-only;
CirrOS and Alpine's `-bios-` images are MBR. Set `VMClass.spec.firmware` to
match:

```yaml
firmware: efi     # Kairos, most modern cloud images
firmware: bios    # CirrOS, Alpine -bios- images
```

**A disk smaller than the image.** The OS disk is a copy-on-write overlay
over the backing image, and an overlay must be at least the backing image's
*virtual* size. A `VMClass` declaring `sizeGiB: 1` over a 20 GiB image gives
the guest a truncated disk.

```sh
qemu-img info /var/lib/libvirt/pools/images/<image>   # check "virtual size"
```

## Lifecycle

The seed volume is owned by its machine:

- **Created** after the domain is first defined — `instance-id` needs the
  UUID, which libvirt assigns at define time. The domain is then redefined
  with the CD-ROM attached, which is why you will see two defines in the
  logs for a machine's first reconcile.
- **Reused** on later reconciles. The image is deterministic, so an existing
  seed is left alone rather than rewritten — otherwise every reconcile would
  churn the host's storage for no change.
- **Deleted** with the domain, alongside the OS disk. The shared base image
  is never touched; it belongs to the `VMImage`.

!!! warning "Changing `userData` on an existing VM does not re-seed it"
    The seed is written once. Editing `spec.userData` afterwards leaves the
    existing seed in place, and cloud-init would not re-run its per-instance
    modules anyway — the `instance-id` has not changed. Recreate the VM.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| Guest boots but ignores user-data | Confirm the seed is attached: `virsh domblklist <domain>` should list a second CD-ROM. Mount the volume and check `user-data` is at the root. |
| Guest is `Running`, never gets an address | It never reached a bootloader. Check firmware against the image, then the disk size against the image's virtual size. |
| cloud-init ran, but skipped per-instance modules | `instance-id` was reused, so cloud-init treated first boot as a reboot. banlieue derives it from the domain UUID, so this means the domain was redefined rather than recreated. |
| `meta-data` present, `user-data` absent | You set no `spec.userData`. That is valid — the guest still gets its hostname and instance-id. |
| Hostname in the lease is not the domain name | The guest may not be cloud-init-based (Kairos), or its DHCP client may not send a hostname (CirrOS). Neither indicates a bad seed. |
| Editing `userData` changed nothing | Expected — see the warning above. Recreate the VM. |

## Reference

- [ADR-0054 — NoCloud seed ISOs are built in-process, with Joliet](https://github.com/firestoned/banlieue/blob/main/docs/adr/0054-nocloud-seed-iso-first-party.md)
- [ADR-0050 — `LibvirtMachine`: the InfraMachine contract on libvirt](https://github.com/firestoned/banlieue/blob/main/docs/adr/0050-libvirtmachine-domain-lifecycle.md)
- [libvirt Provider guide](libvirt-provider.md)
- [cloud-init NoCloud datasource](https://docs.cloud-init.io/en/latest/reference/datasources/nocloud.html)
