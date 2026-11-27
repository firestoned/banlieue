<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0175: The EC2 guest layer and its Rust helper

- **Status:** Accepted
- **Date:** 2026-11-27
- **Proposed:** 2026-11-26
- **Notes:** Partially implemented: the `GuestReady` console-line matcher in `banlieue-aws` (`console.rs`); the guest helper is not built. The rest is not implemented.
- **Deciders:** Erick Bourgeois
- **Related:** Implements [ADR-0163](0163-guestready-on-a-cloud.md) for EC2,
  with the transport [ADR-0174](0174-ec2-inframachine-and-a-first-party-aws-client.md)
  Decision 3 chose. Mirrors [ADR-0171](0171-azure-guest-layer.md) (the Azure
  guest layer and its helper) in shape and delivery. Uses
  [ADR-0113](0113-pre-laid-images-for-fast-sealed-clones.md) (pre-laid images),
  [ADR-0088](0088-first-boot-sealing-for-immediate-images.md) (first-boot
  sealing) and [ADR-0162](0162-publishing-a-vmimage-into-a-cloud.md)
  (publishing).

## Context

A Kairos image does not know EC2. The roadmap 24 spike (2026-11-24 to
2026-11-26) made one boot, seal and report on EC2 with a cloud-config layer
of shell steps; what it took:

1. **User data is behind IMDSv2.** Kairos's datasources do not read EC2's
   metadata service with a session token, and banlieue requires IMDSv2 on
   every instance (ADR-0174). A small reader did: `PUT /latest/api/token`,
   then `GET /latest/user-data` with the token.
2. **Fetching in the `network` stage is one boot too late.** The spike's
   reader ran in `network`, wrote the document to `/oem`, and Kairos applied
   it on the **next** boot: user data had not run when the first boot
   reported ready, which ADR-0163 Decision 2 forbids.
3. **The console is the report channel.** A line written to `/dev/kmsg` at
   error level (`<3>`) reached `GetConsoleOutput` even though Kairos v4 boots
   `quiet`; at the default level it was invisible.
4. **The first boot must not be a recovery boot.** A raw image that reset
   itself out of recovery on first boot took 413 s to report; a template
   installed once at build time, carrying the first-boot sealing policy,
   sealed on its first active boot and reported **49.9 s** after
   `RunInstances` (restart 40 s). The template was 8 GiB, so it stayed small
   to upload, and grew on first boot.
5. **Growing a sealed partition works online.** Moving the backup GPT to the
   end of the volume, growing partition 5 (`COS_PERSISTENT`), then
   `cryptsetup resize` on the open mapping (the volume key is in the
   kernel keyring, no passphrase) and an online `resize2fs` took 1.28 s
   for 2 GB to 3.9 GB, and was a no-op on restart. The disk had to be
   found from the partition's sysfs parent: `lsblk`'s `PKNAME` named the
   partition itself on NVMe.
6. **`systemd-networkd-wait-online` waits on EC2.** On a Kairos v4 image
   the ENA interface (`ens5`) was reported unmanaged and wait-online held
   the boot until its timeout; masking it removed the wait.

## Decision

### 1. Every build of a `VMImage` with an `ec2` source carries the layer

As ADR-0171 Decision 1, for `providerClass: ec2`: the image builder adds the
helper and the layer, with no new API field, the build is shared with the
image's other sources, and every step is gated, in the layer and again in
the helper, on EC2's DMI identity (`/sys/class/dmi/id/sys_vendor` and
`board_vendor` equal to `Amazon EC2`). Off EC2 nothing runs. An image
without an `ec2` source builds byte for byte as before.

### 2. The published disk is pre-laid, small, and grows

The publisher (ADR-0162, amended) publishes the **pre-laid** disk
(ADR-0113): installed once at build time, never a raw image that boots into
recovery (finding 4). Its persistent partition is laid out small, so the
upload carries little more than the image (8 GiB in the spike); the
`EC2Machine`'s volume is sized by the storage class (ADR-0160) and the
layer grows `COS_PERSISTENT` to fill it on first boot. A sealed image's
policy (`kcrypt.encrypt_on_boot`, ADR-0088) is the image's own layer,
unchanged.

### 3. The helper is delivered as a digest-pinned bundle

`banlieue-ec2-guest`, a static musl binary built with the `guest` profile,
published as a one-file OCI image and listed in the `OSArtifact`'s bundles,
exactly as ADR-0171 Decision 2 delivers `banlieue-azure-guest` (digest
pinned, `--ec2-guest-image`, the build refused with a reason when unset,
installed at `/usr/bin`). x86_64 first; an `aarch64` build for Graviton
when roadmap 24 runs there.

### 4. The layer

| Stage | Step | Why |
| --- | --- | --- |
| `initramfs` | `systemctl mask --runtime systemd-networkd-wait-online.service` | finding 6 |
| `initramfs` | `banlieue-ec2-guest userdata` | Decision 5; early enough that `boot` and `network` apply it (finding 2) |
| `boot` | `banlieue-ec2-guest grow` | Decision 5 |
| `initramfs` | write and enable `banlieue-ec2-ready.service` | Decision 6 |

The layer sets no top-level key but `stages`: users, `install` and `kcrypt`
stay the site's.

### 5. The helper

One crate, `crates/banlieue-guest-ec2`, three subcommands and no flags.
Dependencies as `banlieue-guest-azure`'s (`libc`, `thiserror`), no async
runtime, no TLS (IMDS is plain HTTP on a link-local address).

- **`userdata`.** On an active or passive boot: IMDSv2 token (`PUT`, TTL
  60 s, hop limit 1 is the instance's), then `GET /latest/user-data`
  within 16 KiB, which must be a `#cloud-config` document (UTF-8, no NUL) or
  nothing is written. Written to `/oem/91_ec2_userdata.yaml`, mode 0600,
  through a temporary file, `fsync` and `rename`; the same bytes again
  change nothing. It waits up to 60 s for a route to `169.254.169.254`.
  **IMDS turned off** (ADR-0174 Decision 3, every boot after the first
  ready) is not an error: the file already on `COS_OEM` is the copy Kairos
  applies, so later boots never need IMDS. User data is never logged. It
  also records the instance ID for `ready`.
- **`grow`.** On an active or passive boot: finds the persistent partition
  by label, its disk from the sysfs parent (finding 5), and when the GPT's
  last usable sector is short of the disk's end, rewrites the backup GPT at
  the end and extends partition `COS_PERSISTENT` to it (GPT and CRC32 in the
  helper, no `growpart` or shell); then `cryptsetup resize` on its open
  mapping when the partition is LUKS, and `resize2fs` on the filesystem,
  both executed directly, never through a shell. A disk already filled is a
  no-op (finding 5).
- **`ready`.** Only on an active or passive boot, after user data applied:
  writes one line to `/dev/kmsg` at level 3 (finding 3):
  `banlieue-ec2-guest: GuestReady instance=<instance-id>`. The provider
  accepts the line only with its own instance's ID. It is written on every
  boot; `GuestReady` is sticky (ADR-0043 Decision 5), so a later line only
  confirms.
- All three exit 0 and do nothing off EC2, and log to `/dev/kmsg` with a
  `banlieue-ec2-guest:` prefix.

### 6. Ordering: user data applies before the report

`userdata` runs in `initramfs`, after the initramfs's networkd holds a
lease, so the `boot`, `network` and `network.after` stages of the **same
boot** apply it. `ready` is a oneshot ordered `After=cos-setup-network.service`
(every stage of the image and of user data has run), refusing recovery as
ADR-0171 Decision 5 does. A sealing image seals in immucore before any of
this (ADR-0088).

### 7. What a site's base image must not do

ADR-0171 Decision 6's list, for EC2: run another IMDS consumer that reports
or applies user data (cloud-init's EC2 datasource), ship its own
`/usr/bin/banlieue-ec2-guest` or mask its unit, mount `COS_OEM` read-only,
or drop `console=ttyS0` from the kernel command line (the report would
not reach the console).

## Consequences

- The `GuestReady` line is written by the guest and read through the
  console, which any process in the guest that can write `/dev/kmsg` (root)
  can also write. That is the same trust as Azure's ready report (the guest
  says so), and it gates readiness, not identity: identity is the EK
  (ADR-0167).
- User data persists on `COS_OEM`, which is not part of the sealed
  partition. It is protected at rest by EBS encryption (ADR-0174), and in
  the guest by mode 0600. A join secret in it should be single-use.
- **Not verified live.** A live run must confirm that IMDS is reachable from
  the `initramfs` stage on EC2 (finding 2 is the reason to move it there),
  that the Rust `grow` matches finding 5, and that the bundle unpacks as
  ADR-0171's does.

## Alternatives considered

- **Kairos's own datasource for EC2.** It does not use an IMDSv2 token; a
  patch upstream would be the better end state, and the helper's
  `userdata` can go when it lands.
- **Fetch user data in `network` and reboot once.** Costs a whole boot on
  every first boot (finding 4's numbers), to work around ordering.
- **`growpart` from `cloud-guest-utils`.** It worked in the spike, but it is
  a shell script, and in-guest helpers are Rust.
- **A larger template instead of growing.** Uploads and imports scale with
  the template's size; growing costs about a second (finding 5).
