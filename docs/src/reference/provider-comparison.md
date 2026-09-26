<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Provider comparison

> **Living document.** One column per provider. It is complete when all four
> are measured: Cloud Hypervisor ✅ · libvirt ✅ · vSphere ⏳ · Proxmox ⏳
> (provider not built yet, roadmap 06). Last updated **2026-09-27**.
> The open columns are tracked as tasks in roadmaps 05 (vSphere) and 06
> (Proxmox).
> To add or refresh a column, see [Adding a column](#adding-a-column).

banlieue runs the same `VirtualMachine` on any backend. This page answers
two questions for someone choosing one: *what does each backend support*,
and *how does it perform through banlieue*. Performance is measured with
one harness through banlieue's own API, so the numbers include everything a
user waits for: scheduling, the provider, the hypervisor and the guest.

## Capabilities

| | Cloud Hypervisor | libvirt | vSphere | Proxmox |
| --- | --- | --- | --- | --- |
| Status | Implemented (roadmap 09) | Implemented (roadmap 07) | Implemented (roadmap 05) | Not started (roadmap 06) |
| Where the provider runs | **On the KVM host**, as a systemd service; `External` ProviderClass (ADR-0060) | In the cluster, one Deployment per Provider (ADR-0012) | In the cluster, one Deployment per Provider | — |
| Talks to | The local VMM's REST API on a Unix socket (ADR-0061) | Remote `libvirtd`, native RPC over mutual TLS (ADR-0011) | vCenter SOAP API (ADR-0008) | — |
| Credential it holds | A bound ServiceAccount token on the host, self-renewing; **no hypervisor credential and no Secret reads** | mTLS client key (Secret) | vCenter username/password (Secret) | — |
| Hypervisor | Cloud Hypervisor v53 (Rust, KVM) | QEMU/KVM (C) via libvirt | ESXi | Proxmox VE (QEMU/KVM) |
| One VM is | A systemd unit (`banlieue-ch@<uid>`) running one VMM process | A libvirt domain | A vSphere VM | — |
| Guest isolation on the host | **Own uid and private group per guest**, systemd sandbox, seccomp and Landlock in the VMM (ADR-0063) | Host-configured: `libvirt-qemu` user, per-domain AppArmor/SELinux (sVirt) | ESXi | — |
| Image delivery | Admin-placed file in the host cache, or **OCI registry pull by digest** (ADR-0064) | Admin-placed pool volume, or import Job into each pool (ADR-0011) | Per-zone ISO import, templates (ADR-0020) | — |
| `Immediate` install (clone) | Reflink, or sparse copy (ADR-0064 D4) | Backing-volume overlay (ADR-0050) | Template clone | — |
| `Deferred` install | Installer as a read-only virtio disk, hot-unplugged when installed (ADR-0065) | Installer CD-ROM, ejected when installed (ADR-0044) | Installer CD-ROM (ADR-0040) | — |
| vTPM | swtpm per guest (ADR-0065) | swtpm via libvirt (ADR-0050) | vTPM, needs a KMS (ADR-0039) | — |
| EK certificate source | **Read on the host**, where it was minted | Reported by the guest (ADR-0045) | Read from vCenter | — |
| `GuestReady` signal | vsock report (ADR-0065 D5) | `qemu-guest-agent` file read (ADR-0043) | `guestinfo` (ADR-0043) | — |
| UEFI Secure Boot | **No**: no persistent UEFI variables (ADR-0065 D7) | Yes (OVMF secboot, SMM) | Yes | — |
| Live migration | No (roadmap 14 not started) | No | No | — |
| Snapshots | No (roadmap 10 not started) | No | No | — |

## Performance through banlieue

Measured with `make provider-bench`
(`crates/banlieue-controller/tests/bench_provider.rs`): create a
`VirtualMachine`, time each stage from the API, log in over SSH, wait for
the guest to **settle** (its boot unchanged for 60 s and systemd up), run
one fixed workload as root, delete it. Each cell is the median, with the
min–max range, over the runs. Times are from `VirtualMachine` creation.

<!-- provider-bench:begin (scripts/provider-bench-table.py output) -->
| Metric | `cloud-hypervisor` | `libvirt` | `cloud-hypervisor-kairos` |
| --- | --- | --- | --- |
| VirtualMachine created → scheduled (s, lower is better) | 0.5 (0.5–0.6) | 0.5 (0.5–0.6) | 0.5 (0.5–0.6) |
| → infrastructure provisioned (s, lower is better) | 4.7 (4.1–4.7) | 2.1 (2.1–2.1) | 5.2 (4.6–5.7) |
| → `Ready` (s, lower is better) | 4.7 (4.1–4.7) | 2.1 (2.1–2.1) | 5.2 (4.6–5.7) |
| → first address (s, lower is better) | 34.7 (34.4–34.8) | 32.3 (32.1–32.3) | 65.2 (65.0–66.5) |
| → sshd answers (s, lower is better) | 34.7 (34.4–34.8) | 32.3 (32.1–32.3) | 65.2 (65.0–66.5) |
| → SSH login (s, lower is better) | 35.3 (34.9–35.3) | 32.7 (32.6–32.8) | 65.9 (65.8–71.5) |
| → settled (boot stable 60 s, systemd up) (s, lower is better) | 35.0 (35.0–35.0) | 33.0 (33.0–33.0) | 151 (148–159) |
| Boots before settled (count, lower is better) | 1 (1–1) | 1 (1–1) | 2 (2–2) |
| delete → VirtualMachine gone (s, lower is better) | 5.7 (5.7–7.9) | 6.6 (1.1–8.1) | 6.8 (6.6–7.2) |
| CPU: sha256 of 4 GiB (s, lower is better) | 14.1 (14.1–14.2) | 12.3 (11.5–13.2) | 14.4 (14.0–14.5) |
| CPU: shell loop, 300k (s, lower is better) | 0.5 (0.5–0.5) | 0.4 (0.4–0.4) | 0.5 (0.5–0.5) |
| Memory: copy 32 GiB (MB/s, higher is better) | 16100 (15600–16200) | 22300 (22200–22300) | 16200 (15900–16400) |
| Disk: sequential write, 1 GiB, direct (MB/s, higher is better) | 541 (530–643) | 969 (901–1100) | 259 (188–420) |
| Disk: sequential read, 1 GiB, direct (MB/s, higher is better) | 1400 (1300–2100) | 2200 (2000–3400) | 1500 (1200–1700) |
| Disk: 4 KiB writes, 64 MiB, direct (MB/s, higher is better) | 8.2 (7.6–15.0) | 53.7 (26.4–70.9) | 50.2 (14.2–56.3) |
| Disk: 4 KiB reads, 64 MiB, direct (MB/s, higher is better) | 101 (96–124) | 35.7 (7.7–93.0) | 87.1 (78.4–92.0) |
| Runs | 3 | 3 | 3 |

- `cloud-hypervisor`: guest CPU Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz; class ch-small; measured 2026-09-27.
- `libvirt`: guest CPU Intel(R) Core(TM) i5-9400 CPU @ 2.90GHz; class bench-small; measured 2026-09-27.
- `cloud-hypervisor-kairos`: guest CPU Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz; class ch-small; measured 2026-09-27.
<!-- provider-bench:end -->

**Environment.**

- `cloud-hypervisor`: Debian 13 generic cloud image (the same image the
  libvirt column uses), 2 vCPU, 4 GiB, 20 GiB OS disk; two 8-core Intel
  Xeon E5-2630 v3 (32 threads), one Samsung 860 EVO SSD, ext4; bridge
  `virbr0`.
- `cloud-hypervisor-kairos`: the same host and shape, with a Kairos Ubuntu
  24.04 image, which reboots once after its first boot (it grows its
  persistent partition). *Settled* therefore includes a second boot, and
  the disk rows measure Kairos's `COS_PERSISTENT` (ext2) rather than an
  ext4 root.
- `libvirt`: the same Debian 13 image and shape, on a **different
  host**: one 6-core Intel Core i5-9400 (no SMT), one WD SN530 NVMe SSD,
  ext4; QEMU/KVM via libvirt. The provider talks to `libvirtd`
  over mutual TLS from outside the host, as it does in a cluster.

**Reading it.** *Ready* is when the provider reports the machine running;
*first address* and *login* are what a user waits for. CPU and memory rows
mostly measure the host CPU. The 4 KiB write row depends heavily on the
guest's filesystem (Debian's ext4 root and journal against Kairos's ext2
persistent partition, on the same host and hypervisor: 8 MB/s against
50 MB/s), which is why columns should use the same image.

**Cloud Hypervisor against libvirt, through banlieue.** The two columns
ran on different hardware, so only the control-plane rows compare the
providers; the CPU, memory and disk rows mostly compare the hosts (a newer
desktop CPU with faster memory, and NVMe against SATA), and the same-host
table below is the fair comparison of the hypervisors.

- *Ready* comes about 2.5 s sooner on libvirt (2.1 s against 4.7 s). The
  Cloud Hypervisor provider does more before it reports: it creates a tap,
  writes the seed, starts a systemd unit and waits for the VMM's API
  socket, where libvirt defines and starts a domain in two RPCs.
- A user waits about the same on both: login at 32.7 s and 35.3 s. Almost
  all of it is the guest's firmware, GRUB and cloud-init, which the
  provider does not affect.
- Both guests settle on their first boot, and deletion takes 6–7 s on both.
- libvirt's 4 KiB direct reads varied from 7.7 to 93 MB/s across its three
  runs; treat that row as noise until it is rerun.

## The hypervisors alone, same host

Before banlieue drove either, the two KVM hypervisors were compared directly
on the same host, disk image and guest (roadmap 09, 2026-09-26), five runs
each: Cloud Hypervisor v53 against QEMU (q35 + OVMF), with the guest's own
firmware and bootloader, and with direct kernel boot (`dkb`).

| Metric | Cloud Hypervisor | QEMU | CH, direct kernel boot | QEMU, direct kernel boot |
| --- | --- | --- | --- | --- |
| Start → kernel (s) | 13.6 | 14.2 | 0.4 | 0.5 |
| Start → login prompt (s) | 39.6 | 41.6 | 32.0 | 28.4 |
| VMM memory at login (RSS, MiB) | 1397 | 1905 | 1301 | 1950 |
| CPU: sha256 of 4 GiB (s) | 13.7 | 13.8 | 13.7 | 14.0 |
| Memory: copy 32 GiB (MB/s) | 15900 | 16100 | 16600 | 15700 |
| Disk: sequential write, direct (MB/s) | 611 | 538 | 581 | 535 |
| Disk: sequential read, direct (MB/s) | 1600 | 1600 | 1600 | 1500 |
| Disk: 4 KiB writes, direct (MB/s) | **53.2** | 38.2 | 57.8 | 40.3 |
| Disk: 4 KiB reads, direct (MB/s) | **90.7** | 51.2 | 84.1 | 45.0 |

Compute is the same (it is the same CPU); Cloud Hypervisor uses about a
quarter less memory per guest, and does about 40 % more 4 KiB direct writes
and 75 % more 4 KiB direct reads. Most of a boot is firmware and GRUB (about 15 s on both), which direct
kernel boot removes.

## Adding a column

The harness needs only banlieue's API and SSH to the guests, so the same
command measures any provider:

```sh
export KUBECONFIG=~/.kube/<cluster>.yaml
BANLIEUE_BENCH_PROVIDER=<Provider name> \
BANLIEUE_BENCH_IMAGE=<VMImage ready for it> \
BANLIEUE_BENCH_CLASS=<VMClass: 2 vCPU, 4 GiB, 20 GiB disk> \
BANLIEUE_BENCH_LABEL=vsphere \
  make provider-bench                       # 3 runs by default
scripts/provider-bench-table.py target/provider-bench/*.jsonl
```

- Use the same shape (2 vCPU, 4 GiB, 20 GiB) and, where the backend allows,
  the same Kairos image; say so in the environment notes if not.
- The machine running it must reach the guests' addresses on port 22. The
  user-data creates a `bench` user with the run's own SSH key and sudo
  (cloud-init's `sudo:` and Kairos's `admin` group).
- `BANLIEUE_BENCH_PLACEMENT=any` drops the placement selector, for a
  provider too old to label its failure domain `name=<Provider>`, on a
  cluster where it is the only one.
- Paste the table between the `provider-bench` markers above, add the
  environment line, move the column's status mark at the top, and bump the
  date. Results never include host names or addresses; the harness records
  only the CPU model the guest sees.
