# Cloud Hypervisor Host Bootstrap (Debian)

This guide turns a bare-metal Debian machine into a Cloud Hypervisor host for
banlieue's host-resident provider (roadmap 09,
[ADR-0060](https://github.com/firestoned/banlieue/blob/main/docs/adr/0060-cloud-hypervisor-first-class-provider-topology.md)
to ADR-0065).

!!! warning "The provider itself is not released yet"
    This guide prepares the host completely: VMM, firmware, vTPM tooling, the
    `banlieue` account, storage, the host config and the provider's systemd
    unit. The unit stays **installed but not enabled** until the
    `banlieue` binary with the `cloud-hypervisor` provider and its kubeconfig
    exist. Until then, [smoke-boot a guest by hand](#smoke-boot-a-guest-by-hand)
    to prove the host works.

A Cloud Hypervisor host differs from a [libvirt host](host-bootstrap.md) in
three ways that shape everything below:

| | libvirt | Cloud Hypervisor |
| --- | --- | --- |
| Daemon on the host | `libvirtd`, reached over mTLS | none: one VMM process per guest |
| Where the provider runs | a Deployment in the cluster | **on the host**, as a systemd service |
| Emulator in the guest's trust base | QEMU | Cloud Hypervisor (Rust, virtio only) |

Because the provider runs on the host, the host carries a cluster credential,
and a guest that escapes its VMM lands next to it. The bootstrap is built
around keeping both small: guests run as their own unprivileged uids, the
provider is not root, and the credential can read no Secrets.

---

## Requirements

- **Bare metal.** Debian 13 (trixie) or Ubuntu 24.04, x86_64, with VT-x or
  AMD-V enabled in firmware. Nested virtualization is unsupported; `preflight`
  refuses a host that is itself a VM.
- **systemd, D-Bus and polkit.** Guests are systemd units (ADR-0063).
- **A Linux bridge** for guest networking. The script never creates one; see
  [Step 1](#step-1-a-bridge-for-guests).
- **Root on the host**, directly or through `sudo`.
- **Outbound HTTPS** to `github.com` to fetch the pinned VMM and firmware.

---

## The whole chain

=== "On the host"

    ```sh
    # 1. a bridge (once; see Step 1)
    # 2. settings
    ./scripts/bootstrap-cloud-hypervisor-host.sh --print-env-template \
        > ~/.config/banlieue/hosts/bar.env      # then edit it
    # 3. everything else
    sudo BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
        ./scripts/bootstrap-cloud-hypervisor-host.sh all
    ```

=== "From a workstation (remote host)"

    ```sh
    # 1. a bridge on the host (once; see Step 1 — do this with a console
    #    or the rollback timer, never blind over SSH)
    # 2. settings, kept on the workstation
    ./scripts/bootstrap-cloud-hypervisor-host.sh --print-env-template \
        > ~/.config/banlieue/hosts/bar.env      # then edit it
    # 3. copy, run under sudo on the host, clean up
    BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
      ./scripts/bootstrap-cloud-hypervisor-host.sh --remote admin@bar.foo.io all
    ```

    `--remote` copies the script and the env file to a private temporary
    directory on the host, runs the step there with `sudo` (it asks for your
    password in the terminal), and removes both copies afterwards, whether the
    step succeeded or not.

**Never commit an env file.** It names real hosts. Keep it under
`~/.config/banlieue/hosts/`, as the [host bootstrap](host-bootstrap.md#configuration-lives-outside-this-repository)
guide explains.

---

## Step 1: a bridge for guests

Each guest gets a tap device enslaved to a bridge that its **network class**
names. The bootstrap only checks that the bridge exists.

!!! danger "Re-bridging the uplink over SSH is how you lose a remote host"
    Moving the host's address from its NIC onto a new bridge drops the
    connection you are typing into. If the new configuration is wrong, nothing
    brings it back. Do it from an out-of-band console (IPMI, iDRAC, iLO), or use
    the rollback timer below so a mistake reverts itself.

Pick one:

=== "Bridge the uplink (guests on the LAN)"

    Debian's server install uses `ifupdown`. Install the bridge helper, then
    replace the uplink's stanza. Names and addresses here are placeholders:
    `enp1s0` for your NIC, `192.0.2.0/24` for your network.

    ```sh
    sudo apt-get install -y bridge-utils
    sudo cp -a /etc/network/interfaces /etc/network/interfaces.pre-bridge
    ```

    `/etc/network/interfaces`:

    ```text
    auto lo
    iface lo inet loopback

    # The NIC carries no address of its own any more.
    iface enp1s0 inet manual

    auto br0
    iface br0 inet static
        address 192.0.2.10/24
        gateway 192.0.2.1
        bridge_ports enp1s0
        bridge_stp off
        bridge_fd 0
    ```

    Use `iface br0 inet dhcp` instead if the host takes its address from DHCP.
    The bridge then gets the NIC's MAC, so the lease usually follows.

    **Apply with a rollback timer.** This schedules a revert in five minutes,
    then applies. If you can still reach the host afterwards, cancel the
    revert:

    ```sh
    sudo systemd-run --unit=bridge-rollback --on-active=5min /bin/sh -c \
      'cp -a /etc/network/interfaces.pre-bridge /etc/network/interfaces && systemctl restart networking'
    sudo systemctl restart networking

    # still connected? keep the bridge:
    sudo systemctl stop bridge-rollback.timer
    ```

=== "Reuse libvirt's NAT bridge"

    If the host already runs libvirt (for example after
    [`bootstrap-libvirt-host.sh`](host-bootstrap.md)), its `virbr0` bridge
    works as it is: guests get a NAT address from libvirt's `dnsmasq`. With
    `NETWORK_CLASSES` unset, the script uses `virbr0` as the `default` class
    automatically.

    Guests on a NAT bridge are reachable only from the host. That is fine for
    trying things out, and usually not what a pool of sandboxes wants.

Check it:

```sh
ip -br link show type bridge
```

---

## Step 2: settings

```sh
./scripts/bootstrap-cloud-hypervisor-host.sh --print-env-template
```

The settings that matter most:

| Variable | Default | Meaning |
| --- | --- | --- |
| `PROVIDER_NAME` | the host's short name | The `Provider` this host is. One `Provider` is one host (ADR-0060). |
| `STORAGE_CLASSES` | `default=<largest mount>/banlieue/ch` | `name=path`, space-separated. Where guest disks and the image cache live. |
| `NETWORK_CLASSES` | `default=virbr0` if it exists | `name=bridge`, space-separated. Each bridge must exist. |
| `GUEST_UID_BASE`, `GUEST_UID_COUNT` | `2000000`, `10000` | One unprivileged uid per guest. Must not overlap real accounts or `/etc/subuid`. |
| `CH_VERSION`, `CH_SHA256`, `CH_REMOTE_SHA256` | `v53.0`, pinned | The VMM banlieue's client is written against. |
| `FIRMWARE_TAG`, `FIRMWARE_SHA256` | `ch-97eeb7b09`, pinned | `CLOUDHV.fd`, Cloud Hypervisor's edk2 build. |
| `ALLOW_VIRTUALIZED_HOST` | `false` | Lab use only: run on a host that is itself a VM. |

Storage and network classes are the only things a machine gets to choose.
Machines name a class; the host alone knows the path or bridge behind it
(ADR-0062 Decision 4). A stolen cluster credential can therefore choose among
what you declare here, and nothing else on the host.

---

## Step 3: run it

```sh
sudo BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
  ./scripts/bootstrap-cloud-hypervisor-host.sh all
```

Every step also runs on its own and is idempotent. Re-running is the intended
way to change a setting.

| Step | Does |
| --- | --- |
| `preflight` | Bare metal, `/dev/kvm`, the `kvm` group, x86_64, systemd, every declared bridge exists, the guest uid range is free. Changes nothing. |
| `packages` | `swtpm`, `swtpm-tools`, `polkitd`, `dbus`, `iproute2`, `openssl`, `curl`. No QEMU, no libvirt. |
| `vmm` | Downloads `cloud-hypervisor`, `ch-remote` and `CLOUDHV.fd`, checks each against its pinned sha256, and installs nothing on a mismatch. |
| `host` | The `banlieue` system user, storage and run directories, and `/etc/banlieue/cloud-hypervisor.toml`. |
| `tpm` | A per-host EK certificate authority for `swtpm_localca`, readable by `banlieue` only. |
| `polkit` | A rule letting `banlieue` manage its own units and no others. |
| `provider` | The provider's systemd unit. Enabled only once its binary and kubeconfig exist. |
| `selftest` | The VMM runs, the firmware matches its pin, `banlieue` can open `/dev/kvm`, and a test vTPM gets an EK certificate with a `<name>:<uid>` CN. Boots nothing. |
| `status` | Reports what is installed. Changes nothing. |

### What ends up where

| Path | Owner, mode | What |
| --- | --- | --- |
| `/opt/banlieue/cloud-hypervisor/<version>/` | root, 0755 | Pinned `cloud-hypervisor` and `ch-remote`, linked from `/usr/local/bin` |
| `/opt/banlieue/firmware/<tag>/CLOUDHV.fd` | root, 0644 | Pinned firmware |
| `/etc/banlieue/cloud-hypervisor.toml` | root:banlieue, 0640 | Host config: classes, paths, uid range, firmware |
| `/etc/banlieue/swtpm/` | root, 0644 | `swtpm_setup` and `swtpm_localca` configuration |
| `/var/lib/banlieue/swtpm-localca/` | banlieue, 0700 | The EK CA. Keys 0600; only `issuercert.pem` is 0644 |
| `<storage class path>/` | banlieue, 0750 | Per-guest directories (0700, owned by the guest's uid) and `images/` |
| `/run/banlieue/ch/` | banlieue, 0750 | Per-guest sockets, recreated at boot by `tmpfiles.d` |
| `/etc/polkit-1/rules.d/60-banlieue-cloud-hypervisor.rules` | root, 0644 | Unit rule |
| `/etc/systemd/system/banlieue-provider-cloud-hypervisor.service` | root, 0644 | Provider unit |

### Why the EK CA key is `banlieue`-only

Each guest's vTPM carries an endorsement key certificate signed by this host's
CA, and a verifier trusts a guest's attestation through it (ADR-0045,
ADR-0065). Manufacturing a vTPM means signing with that CA's key. So vTPMs are
manufactured by a one-shot unit running as `banlieue`, never as the guest's
uid: a guest uid that could read the key could mint certificates the host
vouches for.

!!! danger "`FORCE=true` rotates the EK CA"
    `FORCE=true` regenerates the host config **and the EK CA**. Every EK
    certificate already issued on this host stops verifying. Don't use it to
    change a setting. Edit the env file and re-run the step instead; the host
    config is only rewritten if you delete it first.

---

## Verifying

```sh
sudo ./scripts/bootstrap-cloud-hypervisor-host.sh status
```

```text
--- host ---
  Debian GNU/Linux 13 (trixie)  kernel 6.12.x  32 vCPU  virt=none
--- vmm ---
  cloud-hypervisor   cloud-hypervisor v53.0
  firmware           /opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd
  swtpm              TPM emulator version 0.7.1, ...
--- config ---
  host-config        /etc/banlieue/cloud-hypervisor.toml
  ek-ca              present
  polkit-rule        present
  kubeconfig         absent (banlieue bootstrap)
--- provider ---
  unit               inactive
```

`provider: inactive` and `kubeconfig: absent` are expected until the provider
ships.

---

## Smoke-boot a guest by hand

This proves the host can run the kind of guest banlieue will run: the
firmware boots a Kairos raw disk, the guest takes user-data from a NoCloud seed
on a virtio disk, and the network works. It is the roadmap 09 phase 0 check,
condensed. Run it as root, in a scratch directory.

```sh
mkdir -p /root/ch-smoke && cd /root/ch-smoke

# 1. A guest disk. Any Kairos cloudImage raw disk works; grow it first.
#    A Kairos cloudImage is exactly its payload size, and its first boot
#    creates a ~9 GiB state partition. Without room it stays in recovery.
cp --sparse=always /path/to/kairos.raw os.raw
truncate -s 20G os.raw

# 2. A NoCloud seed. The label must be CIDATA: there is no CD-ROM on this
#    VMM, so the guest finds the seed by filesystem label only.
cat > user-data <<'EOF'
#cloud-config
hostname: ch-smoke
users:
  - name: kairos
    groups: [admin]
    ssh_authorized_keys:
      - ssh-ed25519 AAAA... you@workstation
EOF
printf 'instance-id: ch-smoke-1\nlocal-hostname: ch-smoke\n' > meta-data
apt-get install -y genisoimage
genisoimage -quiet -output seed.iso -volid CIDATA -joliet -rock user-data meta-data

# 3. A tap on the bridge (br0 here; use your network class's bridge).
ip tuntap add dev chsmoke0 mode tap
ip link set chsmoke0 master br0 up

# 4. Boot. image_type=raw and nested=off are not optional; see Gotchas.
cloud-hypervisor \
  --api-socket path=/root/ch-smoke/api.sock \
  --firmware /opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd \
  --cpus boot=2,nested=off \
  --memory size=4G \
  --disk path=os.raw,image_type=raw path=seed.iso,readonly=on,image_type=raw \
  --net tap=chsmoke0,mac=52:54:00:00:00:51 \
  --rng src=/dev/urandom \
  --serial file=/root/ch-smoke/serial.log \
  --console off &
```

Watch `serial.log` for `ch-smoke login:`. The first boot installs the system
and reboots in place, which takes about two minutes. Find the guest's address
from the host's neighbour table and log in with your key:

```sh
ip neigh show dev br0 | grep 52:54:00:00:00:51
ssh kairos@<address> hostname      # prints ch-smoke if the seed was applied
```

Clean up:

```sh
ch-remote --api-socket /root/ch-smoke/api.sock shutdown-vmm
ip link del chsmoke0
rm -rf /root/ch-smoke
```

---

## Gotchas

Found in the roadmap 09 phase 0 spike, and handled by banlieue's provider so
that a `VirtualMachine` never trips them. They matter when you run the VMM by
hand.

| Gotcha | What happens | Do |
| --- | --- | --- |
| Disk image type left to auto-detect | v53 warns that auto-detection is deprecated and **disables sector-0 writes**, which breaks anything that rewrites the partition table | Always `image_type=raw` |
| `nested` left at its default | Nested virtualization is **on** by default | Always `nested=off` |
| Kairos disk not grown | The first boot fails to add its state partition and stays in recovery, with no user-data applied | Grow the disk before first boot |
| Relative swtpm paths | Daemonized swtpm changes directory to `/`; the VMM then dies with `CmdInit returned error code : 0x9` | Absolute paths only |
| Reading `serial.log` for boot history | The VMM truncates it on every guest reboot | Treat it as the current boot only |
| Addressing guest disks as `/dev/vdX` | Names shift once a disk is unplugged | Use filesystem labels |
| Looking for guests in `virsh` | Cloud Hypervisor guests are processes, not libvirt domains | `pgrep -a cloud-hypervisor`, or `status` |

---

## Upgrading the VMM

The versions are pinned because banlieue's client is written against one
upstream API (ADR-0061). To move to a new release, set the new version **and
its checksums** together, then re-run `vmm`:

```sh
CH_VERSION=vNN.0 CH_SHA256=<sha256> CH_REMOTE_SHA256=<sha256> \
  sudo -E ./scripts/bootstrap-cloud-hypervisor-host.sh vmm
```

A version without matching checksums fails closed: it downloads, reports the
mismatch, and installs nothing. Old versions stay under
`/opt/banlieue/cloud-hypervisor/` until you remove them, so rolling back is
re-running with the old values.

---

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `running inside a VM (kvm)` in `preflight` | The host is a VM. Supported only for labs, with `ALLOW_VIRTUALIZED_HOST=true`. |
| `/dev/kvm missing` | VT-x/AMD-V disabled in firmware, or the `kvm_intel`/`kvm_amd` module is not loaded. |
| `network class default -> br0: not a bridge on this host` | The bridge does not exist yet, or has a different name. See [Step 1](#step-1-a-bridge-for-guests). |
| `NETWORK_CLASSES is empty and there is no virbr0` | No bridge was named and libvirt's isn't present. Create one and set `NETWORK_CLASSES`. |
| `an account already uses a uid in ...` or `... overlaps guest uids` | Move `GUEST_UID_BASE` to a free range, clear of `/etc/subuid` and `/etc/subgid` (rootless containers). |
| `checksum mismatch for ...` | The download is not the pinned artifact, or you changed a version without its checksums. Nothing was installed. |
| `swtpm_setup failed as banlieue` in `selftest` | The EK CA directory has the wrong owner, often after copying `/var/lib/banlieue` by hand. Re-run `tpm`. |
| `sudo: a terminal is required to read the password` with `--remote` | The workstation side ran without a terminal (piped, or from CI). Run it in an interactive terminal. |
| Provider unit `inactive` | Expected until the provider binary and `/etc/banlieue/kubeconfig` exist. |
