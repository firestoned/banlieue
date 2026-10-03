# Cloud Hypervisor Host Bootstrap

This guide turns a bare-metal Linux machine running systemd into a Cloud
Hypervisor host for
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

- **Bare metal.** An x86_64 Linux distribution running systemd, with VT-x
  or AMD-V enabled in firmware. Debian 13 (trixie) is what
  `make ch-host-install-test` exercises; nothing in the installer is
  specific to it. Nested virtualization is unsupported; `preflight` refuses
  a host that is itself a VM.
- **The host's own packages.** banlieue installs no OS packages (ADR-0084).
  Install these first, with whatever your OS uses (`apt-get`, `dnf`,
  `zypper`, a Kairos image build):

    | Needed for | Debian / Ubuntu / Kairos (Debian base) | Fedora / RHEL |
    | --- | --- | --- |
    | Guests are systemd units, guest uids resolve through NSS (ADR-0063) | `systemd`, `libnss-systemd`, `dbus` | `systemd`, `dbus` |
    | The provider starts its own units | `polkitd` | `polkit` |
    | vTPMs (ADR-0065) | `swtpm`, `swtpm-tools` | `swtpm`, `swtpm-tools` |
    | HTTPS downloads | `ca-certificates` | `ca-certificates` |

    `preflight` checks for `systemctl`, `systemd-tmpfiles`, `swtpm`,
    `swtpm_setup` and `swtpm_localca` on `PATH`, and names every one that is
    missing.
- **systemd, D-Bus and polkit.** Guests are systemd units (ADR-0063).
- **A Linux bridge** for guest networking. banlieue never creates one; see
  [Step 1](#step-1-a-bridge-for-guests).
- **Root on the host**, directly or through `sudo`.
- **Outbound HTTPS** to `github.com` to fetch the VMM and firmware, or to a
  mirror you name for each file (see [Choosing the VMM](#choosing-the-vmm-version-and-where-it-comes-from)),
  or the release assets in a local directory (`--artifacts-dir`).
- **The `banlieue` binary.** It prepares the host (`banlieue host cloud-hypervisor`, ADR-0067)
  and then runs on it as the provider.

---

## The whole chain

=== "On the host"

    ```sh
    # 0. the host's own packages (see Requirements), e.g. on Debian:
    sudo apt-get install -y --no-install-recommends \
        systemd libnss-systemd dbus polkitd swtpm swtpm-tools ca-certificates
    # 1. a bridge (once; see Step 1)
    # 2. everything else, from the binary the host will run
    sudo banlieue host cloud-hypervisor install \
        --provider-name bar --network-class default=br0
    ```

=== "Air-gapped host (Artifactory)"

    ```sh
    # 0. packages from your internal OS mirror; 1. a bridge (see Step 1)
    # 2. the three Cloud Hypervisor files from your GitHub proxy
    MIRROR=https://internal.example.com/artifactory/vcs-github
    sudo banlieue host cloud-hypervisor install \
        --provider-name bar --network-class default=br0 \
        --vmm-url       "$MIRROR/cloud-hypervisor/cloud-hypervisor/v53.0/cloud-hypervisor-static" \
        --ch-remote-url "$MIRROR/cloud-hypervisor/cloud-hypervisor/v53.0/ch-remote-static" \
        --firmware-url  "$MIRROR/cloud-hypervisor/edk2/ch-97eeb7b09/CLOUDHV.fd"
    ```

    The URL layout is your Artifactory's; write each one exactly as your
    repository serves it. For the pinned release (v53.0 and `ch-97eeb7b09`
    here) the sha256 compiled into banlieue still applies, so the mirror
    decides only where the bytes come from. See
    [Choosing the VMM](#choosing-the-vmm-version-and-where-it-comes-from)
    for another release.

=== "From a workstation (remote host)"

    ```sh
    # 1. a bridge on the host (once; see Step 1 — do this with a console
    #    or the rollback timer, never blind over SSH)
    # 2. settings, kept on the workstation
    ./scripts/bootstrap-cloud-hypervisor-host.sh --print-env-template \
        > ~/.config/banlieue/hosts/bar.env      # then edit it
    # 3. copy the binary, run it under sudo on the host, clean up
    BANLIEUE_BINARY=target/release/banlieue \
    BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/bar.env \
      ./scripts/bootstrap-cloud-hypervisor-host.sh --remote admin@bar.foo.io all
    ```

    The script is a thin wrapper: `--remote` copies the binary to a private
    temporary directory on the host, turns the env file into `banlieue host cloud-hypervisor`
    flags, runs `sudo banlieue host cloud-hypervisor install` there (it asks
    for your password in the terminal), installs the same binary as the
    provider, and removes the copy whether the step succeeded or not.

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
    no `--network-class`, `banlieue host cloud-hypervisor` uses `virbr0` as the `default`
    class automatically.

    Guests on a NAT bridge are reachable only from the host. That is fine for
    trying things out, and usually not what a pool of sandboxes wants.

Check it:

```sh
ip -br link show type bridge
```

---

## Step 2: settings

Every setting is a flag, and each flag has a `BANLIEUE_HOST_*` environment
variable, so a cloud-init payload needs no file of its own
(`banlieue host cloud-hypervisor install --help` lists them all):

| Flag | Default | Meaning |
| --- | --- | --- |
| `--provider-name` | the host's short name | The `Provider` this host is. One `Provider` is one host (ADR-0060). |
| `--storage-class name=path` | `default=<roomiest of /srv, /data, /home, /opt, /var/lib>/banlieue/ch` | Where guest disks and the image cache live. Repeat, or comma-separate. |
| `--network-class name=bridge` | `default=virbr0` if it exists | Each bridge must exist. Repeat, or comma-separate. |
| `--guest-uid-base`, `--guest-uid-count` | `2000000`, `1024` | One unprivileged uid per guest. Must not overlap real accounts or `/etc/subuid`. |
| `--registry-repository` | none | The one repository `Url` images are pulled from, by digest (ADR-0064). |
| `--vmm-version`, `--firmware-tag` | the pinned release | Which Cloud Hypervisor release and edk2 firmware tag to install. See [Choosing the VMM](#choosing-the-vmm-version-and-where-it-comes-from). |
| `--vmm-url`, `--ch-remote-url`, `--firmware-url` | the GitHub release asset | Where each file is downloaded from, for a mirror. HTTPS only. |
| `--vmm-sha256`, `--ch-remote-sha256`, `--firmware-sha256` | the pin, or GitHub's digest | The sha256 of a file that is not the pinned release. |
| `--artifacts-dir` | none (download) | Take `cloud-hypervisor-static`, `ch-remote-static` and `CLOUDHV.fd` from here, verified the same way. |
| `--allow-virtualized-host` | off | Lab use only: run on a host that is itself a VM. |

By default the VMM is the release pinned in the binary, the one its client
is written against (ADR-0061), downloaded from GitHub and checked against the
sha256 compiled in.

The release flags, their variables, and the names the `--remote` script reads
from its env file (`--print-env-template` prints them all). `install` takes
every one; `selftest` takes all but `--artifacts-dir`, so it checks the
release you installed:

| Flag | Variable | Env file |
| --- | --- | --- |
| `--vmm-version` | `BANLIEUE_HOST_VMM_VERSION` | `VMM_VERSION` |
| `--firmware-tag` | `BANLIEUE_HOST_FIRMWARE_TAG` | `FIRMWARE_TAG` |
| `--vmm-url` | `BANLIEUE_HOST_VMM_URL` | `VMM_URL` |
| `--ch-remote-url` | `BANLIEUE_HOST_CH_REMOTE_URL` | `CH_REMOTE_URL` |
| `--firmware-url` | `BANLIEUE_HOST_FIRMWARE_URL` | `FIRMWARE_URL` |
| `--vmm-sha256` | `BANLIEUE_HOST_VMM_SHA256` | `VMM_SHA256` |
| `--ch-remote-sha256` | `BANLIEUE_HOST_CH_REMOTE_SHA256` | `CH_REMOTE_SHA256` |
| `--firmware-sha256` | `BANLIEUE_HOST_FIRMWARE_SHA256` | `FIRMWARE_SHA256` |
| `--artifacts-dir` | `BANLIEUE_HOST_ARTIFACTS_DIR` | `ARTIFACTS_DIR` |

`--artifacts-dir` cannot be combined with a URL flag: the files come from one
place or the other.

Storage and network classes are the only things a machine gets to choose.
Machines name a class; the host alone knows the path or bridge behind it
(ADR-0062 Decision 4). A stolen cluster credential can therefore choose among
what you declare here, and nothing else on the host.

---

## Step 3: run it

```sh
sudo banlieue host cloud-hypervisor install --network-class default=br0
```

`ch` is a short alias for the backend: `banlieue host ch install` is the same
command. The backend is part of the command (ADR-0084 Decision 7) because
`host` only says *this machine*, not what it is being prepared for.

Every stage also runs on its own (`--only <stage>`, which refuses if a stage
it builds on has not run), and a second `install` changes nothing.
`--dry-run` prints what would change and changes nothing. Re-running is the
intended way to change a setting.

| Stage | Does |
| --- | --- |
| `preflight` | Bare metal, `/dev/kvm`, the `kvm` group, x86_64, the commands the host supplies (`systemctl`, `systemd-tmpfiles`, `swtpm`, `swtpm_setup`, `swtpm_localca`), every declared bridge exists, the guest uid range is free, NSS has its `systemd` module. Changes nothing (also `banlieue host cloud-hypervisor preflight`). |
| `vmm` | Downloads `cloud-hypervisor`, `ch-remote` and `CLOUDHV.fd`, checks each against its sha256, and installs nothing, leaving the previous release as it was, on any mismatch. |
| `host` | The `banlieue` system user, one userdb user and private group per guest uid, state, storage and run directories, and `/etc/banlieue/cloud-hypervisor.toml` (rendered from the provider's own config type and parsed back before it is written). An existing file is kept, except that its `[vmm]` section follows the release just installed. |
| `tpm` | A per-host EK certificate authority for `swtpm_localca`, created as `banlieue`, readable by `banlieue` only. |
| `polkit` | A rule letting `banlieue` manage instances of its own templates, for uids in the guest range, and nothing else. |
| `provider` | The four template units and the provider unit. The provider is enabled only once its binary and kubeconfig exist. |
| `selftest` | The VMM runs, the firmware matches its sha256, guest uids resolve, `banlieue` can open `/dev/kvm`, the provider's own host checks pass, and a test vTPM gets an EK certificate with a `<name>:<uid>` CN. Boots nothing (also `banlieue host cloud-hypervisor selftest`). |

`banlieue host cloud-hypervisor status` reports what is installed and changes nothing.

### What ends up where

| Path | Owner, mode | What |
| --- | --- | --- |
| `/opt/banlieue/cloud-hypervisor/<version>/` | root, 0755 | `cloud-hypervisor` and `ch-remote`, one directory per release, the current one linked from `/usr/local/bin` |
| `/opt/banlieue/firmware/<tag>/CLOUDHV.fd` | root, 0644 | Firmware, one directory per tag |
| `/etc/banlieue/cloud-hypervisor.toml` | root:banlieue, 0640 | Host config: classes, paths, uid range, firmware |
| `/etc/banlieue/swtpm/` | root, 0644 | `swtpm_setup` and `swtpm_localca` configuration |
| `/var/lib/banlieue/swtpm-localca/` | banlieue, 0700 | The EK CA. Keys 0600; only `issuercert.pem` is 0644 |
| `/var/lib/banlieue/` | banlieue, 0751 | Provider state: `ek/` and `units/` (0700), `tpm/` (0711, one guest-owned directory per guest) |
| `/etc/userdb/` | root, 0644 files | One user and one private group per guest uid, for NSS |
| `<storage class path>/` | banlieue, 0711 | Per-guest directories (2770, the guest's uid and banlieue's group) and `images/` (0750, banlieue only) |
| `/run/banlieue/ch/` | banlieue, 0711 | Per-guest sockets, recreated at boot by `tmpfiles.d` |
| `/etc/polkit-1/rules.d/60-banlieue-cloud-hypervisor.rules` | root, 0644 | Unit rule |
| `/etc/systemd/system/banlieue-{ch,swtpm,swtpm-setup,ch-import}@.service` | root, 0644 | Template units the provider starts instances of |
| `/etc/systemd/system/banlieue-provider-cloud-hypervisor.service` | root, 0644 | Provider unit |

### Why the EK CA key is `banlieue`-only

Each guest's vTPM carries an endorsement key certificate signed by this host's
CA, and a verifier trusts a guest's attestation through it (ADR-0045,
ADR-0065). Manufacturing a vTPM means signing with that CA's key. So vTPMs are
manufactured by a one-shot unit running as `banlieue`, never as the guest's
uid: a guest uid that could read the key could mint certificates the host
vouches for.

!!! danger "`--force` rotates the EK CA"
    `--force` regenerates the host config **and the EK CA**. Every EK
    certificate already issued on this host stops verifying. Don't use it to
    change a setting: the host config is only rewritten if you delete it
    first, or edit it by hand.

---

## Verifying

```sh
sudo banlieue host cloud-hypervisor status
```

```text
--- host ---
  arch               x86_64
  virtualization     none
  systemd            running
--- vmm ---
  cloud-hypervisor   cloud-hypervisor v53.0
  pinned             v53.0
  firmware           present
  swtpm              TPM emulator version 0.7.1, ...
--- config ---
  host-config        present
  ek-ca              present
  polkit-rule        present
  kubeconfig         absent (banlieue bootstrap cloud-hypervisor-host)
--- provider ---
  unit               inactive
--- guests ---
```

Without `sudo`, files in directories only `banlieue` can enter show as
`unknown (run as root)` rather than missing.

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

## What gets installed, and why

Every systemd unit setting, the polkit rule, the tmpfiles entry, the guest
identities in `/etc/userdb` and the directory permissions are explained,
setting by setting, in
[Cloud Hypervisor host: systemd, polkit, identities](cloud-hypervisor-host-systemd.md).
The files themselves are templates in `deploy/provider-cloud-hypervisor/host/`,
which this script renders; running it against a remote host (`--remote`)
copies them along.

## Connect it to a cluster

The host is a `Provider` whose `ProviderClass` is **External** (ADR-0060):
the operator gives it an identity in the cluster but runs nothing, and the
process runs here. Three steps, in order.

1. **In the cluster**, install banlieue if you have not: `banlieue bootstrap
   operator` installs the `cloud-hypervisor` ProviderClass (`deployment:
   External`) and its ClusterRole alongside everything else. Then apply the
   host's `Provider` and what its VMs use — example 21 is a complete set.
   The Provider names **no** `credentialsRef` (this provider reads no
   Secret; one set is refused) and its name must be the host's
   `PROVIDER_NAME`:

    ```sh
    kubectl apply -f examples/21-virtualmachine-cloud-hypervisor.yaml
    ```

    The operator creates the Provider's ServiceAccount and a Role scoped to
    that one Provider, its machines, its Lease and renewing its own token.
    External Providers live in the operator's install namespace
    (`banlieue-system`): that is the only namespace where it may grant token
    renewal.

2. **Issue the host its first credential**, as a cluster admin, anywhere
   your kubeconfig works:

    ```sh
    banlieue bootstrap cloud-hypervisor-host --provider <PROVIDER_NAME> --output-dir /tmp/creds
    ```

    This writes `kubeconfig` (your cluster's server and CA, reading its token
    from `/etc/banlieue/credentials/token` on the host) and `token`, both
    `0600`. Copy the directory to the host.

3. **On the host, as root**, in one step: bootstrap, install the
   credentials, seed the image cache, start the provider.

    ```sh
    sudo PROVIDER_CREDENTIALS=/tmp/creds \
         BASE_IMAGE=/var/lib/libvirt/images/kairos-ubuntu-2404.raw \
         STORAGE_CLASSES="default=/srv/banlieue/ch" \
         NETWORK_CLASSES="default=virbr0" \
         scripts/ch-host-provider-up.sh
    ```

From then on the provider **renews its own token** at half its lifetime
(24 h by default) and replaces the token file in place; the kubeconfig
never changes. A host down for longer than one lifetime comes back with an
expired token — run step 2 again. To **revoke** a host's credential, delete
its ServiceAccount: every token issued for it stops working at once, and
the operator recreates the account for a fresh step 2.

Within seconds the `Provider` reports its failure domain and the `VMImage`
row for this host turns ready:

```sh
kubectl get provider -n banlieue-system -o yaml   # status.failureDomains, status.workload.mode: External
kubectl get vmimage kairos-ubuntu-2404-ch -o jsonpath='{.status.perProvider}'
```

The failure domain's `attributes.raw` also carries what the host has to
give: `cpus`, `cpuModel`, `memoryMiB`, `hugepagesMiB` (reserved, so not
free memory), `storageFreeGiB` per host storage class, and
`nestedVirtualization: "false"`, which this class never offers.

Upgrading the provider is replacing `/usr/local/bin/banlieue` and
restarting `banlieue-provider-cloud-hypervisor`: guests are their own
`banlieue-ch@` units and keep running, and the new process adopts them.
`make ch-restart-e2e` checks exactly that on a host.

## Images from a registry (`Url` sources)

A `BackingFile` source names a file you copied into a storage class's
`images/` directory yourself. A `Url` source is built in the cluster by
banlieue-imagebuilder like any other, and reaches this host through an OCI
registry, since the host cannot mount the cluster's artifacts volume
(ADR-0064):

1. **In the cluster**, give the imagebuilder a repository to push to. In
   `banlieue-imagebuilder-config`:

    ```yaml
    BANLIEUE_REGISTRY_REPOSITORY: "registry.internal:5000/banlieue/disks"
    BANLIEUE_REGISTRY_CREDENTIALS_SECRET: "banlieue-registry-push"
    ```

    The Secret is `kubernetes.io/basic-auth`, in the build namespace
    (`banlieue-imagebuild`). Once a `VMImage` with a `cloud-hypervisor`
    `Url` source finishes building, a push Job uploads it and
    `status.buildArtifact.ociArtifact.reference` names it by digest.

2. **On the host**, name the same repository, and give pull credentials if
   the registry needs them:

    ```sh
    # Regenerates the host config with the registry section. Repeat the
    # storage and network classes you installed with: the file is rewritten
    # from these flags. Only the host stage runs, so the EK CA is untouched.
    sudo banlieue host cloud-hypervisor install --only host --force \
         --network-class default=br0 \
         --registry-repository registry.internal:5000/banlieue/disks
    # only if the registry needs credentials:
    sudo install -m 0640 -o root -g banlieue /dev/stdin /etc/banlieue/registry/username <<<'robot'
    sudo install -m 0640 -o root -g banlieue /dev/stdin /etc/banlieue/registry/password <<<'…'
    sudo systemctl restart banlieue-provider-cloud-hypervisor
    ```

    This writes a `[registry]` section into the host config. **The host
    pulls only from that repository, only by digest.** `VMImage` status is
    written in the cluster; which registry a host trusts is the host
    owner's decision, so a reference naming anything else is refused
    (`ForeignReference`).

The provider then starts `banlieue-ch-import-<vmimage uid>.service`, which
pulls the image once, verifies it, writes it sparse as
`images/sha256-<digest>.raw` in one storage class, and reflinks (or
sparse-copies) it into the others. The row turns ready when every storage
class holds it:

```sh
kubectl get vmimage <name> -o jsonpath='{.status.buildArtifact.ociArtifact}'
kubectl get vmimage <name> -o jsonpath='{.status.perProvider}'   # reason: Importing, then Reconciled
systemctl status 'banlieue-ch-import-*'                           # while it runs
```

| `perProvider[].reason` | Meaning |
| --- | --- |
| `RegistryNotConfigured` | This host's config has no `[registry]` |
| `AwaitingArtifact` | The build is not pushed yet; see `status.buildArtifact.ociArtifact` |
| `ForeignReference` | The pushed reference is not a digest in this host's repository |
| `Importing` | The import unit is running |
| `ImportFailed` | The unit failed; the message carries systemd's reason. It is retried on the next reconcile |
| `Released` | The `VMImage` is being deleted and this host has let go of it |

**Cache housekeeping.** After each import the host deletes pulled images
nothing uses any more (a rebuild's previous digest), keeping the newest
`keep_unreferenced` of them (`REGISTRY_KEEP_UNREFERENCED`, default 1).
Files you placed yourself for `BackingFile` sources are never touched.
Deleting a `VMImage` removes its file from every host (reason `Released`)
unless another image or a machine there still uses it; running guests are
unaffected either way, because each OS disk is its own file. The image
stays in `Terminating` until every Cloud Hypervisor host with a row has
released it. For a host that is gone for good, delete its `Provider` or
remove the finalizer by hand:

```sh
kubectl patch vmimage <name> --type json \
  -p '[{"op":"remove","path":"/metadata/finalizers/0"}]'   # check the index first
```

## vTPM and Deferred install

A `VMClass` with `tpmEnabled: true` needs a host that offers `vtpm` and an
image installed `Deferred`, so the disk is sealed to that guest's own TPM
(ADR-0048, ADR-0065):

1. **Host:** the bootstrap's `tpm` step creates the host's EK CA; the host
   config's `[tpm]` section points at it. Declare the feature on the
   `Provider`:

    ```yaml
    spec:
      capabilities:
        features: [vtpm]
    ```

    The provider publishes `vtpm` only if the host can really run one, and
    publishes its EK CA on `status.ekCaCertificates`.

2. **Image:** `spec.template.installMode: Deferred`. For a `Url` source the
   imagebuilder builds the installer ISO; for `BackingFile`, put the ISO in
   the image cache yourself. Add example 16's
   `banlieue-guest-phase-cloud-hypervisor` stage, so the installed system
   reports over vsock (`systemd-notify`, systemd 256 or later; `socat` on
   older images).

What happens per machine: `swtpm_setup` manufactures the TPM once, as
`banlieue`, and writes the EK certificate to `/var/lib/banlieue/ek/<uid>/`;
the guest's own `swtpm` starts; the provider copies the installer into the
machine's directory (guests cannot read the cache), and the VMM boots it
from an empty disk; the installed system reports `phase=installed`; the
provider hot-unplugs the installer, deletes the copy and marks
`GuestReady`:

```sh
kubectl get cloudhypervisormachine <m> -o jsonpath='{.status.tpmEndorsementCertificates[0]}' \
  | openssl x509 -noout -subject -issuer        # CN=<machine>:<uid>, issuer swtpm-localca
kubectl get cloudhypervisormachine <m> -o jsonpath='{.status.guestInstalled} {.status.installMediaDetached}'
systemctl status 'banlieue-swtpm@*'
```

`make ch-vtpm-e2e` checks the vTPM half on a host (it creates a
`tpmEnabled` machine directly, since the controller refuses `tpmEnabled`
with an `Immediate` image). `make ch-deferred-e2e` checks the whole path
with a Kairos installer ISO in the cache: install, sealing, the vsock
report, the eject, and a clean delete. It runs its test binary as root,
since it reads the guest's disk and the provider's state.

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

One gotcha the provider cannot handle for you, because it lives in your
user-data: a **`Deferred` Kairos install whose `#cloud-config` has no
`users:` entry never starts**. The installer ISO boots, auto-logs in as root
on the serial console, and waits; `GuestReady` never comes, and a pool stays
at zero warm members until `provisioningTimeoutSeconds`. Observed with Kairos
Hadron v0.4.0 in the roadmap 17 phase G runs (2026-09-28): four of four
members idle without a `users:` entry, fifteen of seventeen installed with
one. Give the install at least one user, as the host-side smoke test above
does; a login is also the only way to read the installer's journal when an
install does stall.

---

## Image-based hosts (Kairos)

!!! warning "Not yet exercised by banlieue's tests"
    `make ch-host-install-test` runs on Debian 13. This recipe follows from
    Kairos's documented layout and from what `install` writes; try it on one
    host, reboot it, and run `banlieue host cloud-hypervisor selftest` before relying on it.

Nothing in `banlieue host cloud-hypervisor install` is Debian-specific, but an image-based OS
changes **what survives a reboot**. On Kairos, `/etc`, `/var` and `/srv` are
ephemeral; `/opt`, `/usr/local`, `/etc/systemd` and a fixed list of other
paths are bind-mounted from the persistent partition
([Kairos: immutable](https://kairos.io/docs/architecture/immutable/)).
`install` writes into both kinds:

| Path | On Kairos | Consequence if left ephemeral |
| --- | --- | --- |
| `/opt/banlieue/` (VMM, firmware), `/usr/local/bin/` (links, provider binary) | persistent | none |
| `/etc/systemd/system/` (units) | persistent | none |
| `/var/lib/banlieue/` (EK CA, vTPM state, provider state) | **ephemeral** | a new EK CA every boot: every guest's EK certificate stops verifying |
| `/etc/banlieue/` (host config, kubeconfig, credentials) | **ephemeral** | the provider loses its cluster credential at every boot |
| the default storage class (`/srv/banlieue/ch`, if `/srv` is roomiest) | **ephemeral** | every guest disk is lost at reboot |
| `/etc/userdb/`, `/etc/polkit-1/rules.d/`, `/etc/tmpfiles.d/`, the `banlieue` user | ephemeral | recreated by re-running `install`, which is idempotent |

So, on Kairos:

1. **Build the packages and the `banlieue` user into the image.** The user
   must have the same uid on every boot, or the persistent state it owns
   stops being its own. In the image's Dockerfile (Debian-based Kairos):

    ```dockerfile
    RUN apt-get update && apt-get install -y --no-install-recommends \
          systemd libnss-systemd dbus polkitd swtpm swtpm-tools ca-certificates \
     && useradd --system --home-dir /var/lib/banlieue --no-create-home \
          --shell /usr/sbin/nologin --user-group banlieue
    ```

    `install` keeps an existing `banlieue` user and adds it to `kvm`.

2. **Make banlieue's state persistent**, with a file under `/oem`
   ([Kairos: persistent paths](https://kairos.io/docs/examples/extra_persistent_paths_after_install/)),
   then reboot once:

    ```yaml
    # /oem/91_banlieue_paths.yaml
    stages:
      rootfs:
        - name: "banlieue persistent paths"
          environment_file: /run/cos/cos-layout.env
          environment:
            CUSTOM_BIND_MOUNTS: "/var/lib/banlieue /etc/banlieue"
    ```

3. **Put storage classes on a persistent path**, never the default:
   `--storage-class default=/usr/local/banlieue/ch`.

4. **Re-run `install` at every boot**, so the ephemeral pieces come back.
   A second `install` changes nothing that is already right: the EK CA, the
   host config and the guest records are kept.

    ```yaml
    # /oem/92_banlieue_host.yaml
    stages:
      network:
        - name: "banlieue host cloud-hypervisor install"
          commands:
            - >-
              /usr/local/bin/banlieue host cloud-hypervisor install
              --provider-name bar
              --network-class default=br0
              --storage-class default=/usr/local/banlieue/ch
    ```

    Add the release flags here too (`--vmm-url` and the rest) for an
    air-gapped host, or the `--vmm-version` you chose: a boot-time install
    without them puts the host back on the pinned release.

The first time, run the same command by hand and copy the provider's
kubeconfig into `/etc/banlieue/credentials` (see
[Connect it to a cluster](#connect-it-to-a-cluster)); from then on the boot
stage keeps the host as it is.

---

## Choosing the VMM version and where it comes from

`install` places three files banlieue does not build: `cloud-hypervisor-static`
and `ch-remote-static` from a
[cloud-hypervisor release](https://github.com/cloud-hypervisor/cloud-hypervisor/releases),
and `CLOUDHV.fd` from an
[edk2 release](https://github.com/cloud-hypervisor/edk2/releases)
(ADR-0084). By default they are the release pinned in banlieue, downloaded
from github.com and checked against the sha256 compiled into the binary.

**A newer release.** Name it, and `install` takes GitHub's published sha256
for each file:

```sh
sudo banlieue host cloud-hypervisor install --only vmm --vmm-version v54.0
```

A release older than the pinned one is refused before anything is
downloaded: the provider would refuse to drive it (`VmmVersionUnsupported`,
ADR-0061). A newer one is accepted, but banlieue's client is checked against
the pinned release's API, so try a new major version on one host first.

**An air-gapped host.** Name a mirror for each file, for example an
Artifactory remote repository proxying GitHub. The URL is used exactly as
written:

```sh
MIRROR=https://internal.example.com/artifactory/vcs-github
sudo banlieue host cloud-hypervisor install \
    --vmm-url       "$MIRROR/cloud-hypervisor/cloud-hypervisor/v53.0/cloud-hypervisor-static" \
    --ch-remote-url "$MIRROR/cloud-hypervisor/cloud-hypervisor/v53.0/ch-remote-static" \
    --firmware-url  "$MIRROR/cloud-hypervisor/edk2/ch-97eeb7b09/CLOUDHV.fd" \
    --network-class default=br0
```

For the pinned release the compiled sha256 still applies, so the mirror
only decides where the bytes come from. For any other release, a host that
cannot reach `api.github.com` needs the digests too:

```sh
sudo banlieue host cloud-hypervisor install --only vmm --vmm-version v54.0 \
    --vmm-url "$MIRROR/…/cloud-hypervisor-static" --vmm-sha256 <64 hex digits> \
    --ch-remote-url "$MIRROR/…/ch-remote-static" --ch-remote-sha256 <64 hex digits>
```

Take the digests from the release page on a machine that can reach GitHub
(each asset lists its `sha256:`), not from the mirror. Every flag has a
`BANLIEUE_HOST_*` variable (`BANLIEUE_HOST_VMM_URL`,
`BANLIEUE_HOST_VMM_SHA256`, …), and the `--remote` script reads
`VMM_VERSION`, `VMM_URL`, `VMM_SHA256` and the rest from its env file.

**How it lands.** Every file is downloaded and verified before any is
installed. On any mismatch nothing is installed and the links stay where
they were. Each release gets its own `/opt/banlieue/cloud-hypervisor/<version>/`
(and firmware `/opt/banlieue/firmware/<tag>/`); only the `cloud-hypervisor`
and `ch-remote` links move, and old releases stay until you remove them, so
rolling back is `install --only vmm --vmm-version <old>`. A running guest
keeps the VMM process it started with. The host config's `[vmm]` section
is updated to name the new release, and the rest of the file is kept;
restart `banlieue-provider-cloud-hypervisor.service` so the provider reads
it. Run `selftest` with the same `--vmm-version`/`--firmware-tag` flags, so
it checks the firmware you installed.

Upgrading the provider itself is replacing `/usr/local/bin/banlieue` and
restarting its unit; guests keep running (`make ch-restart-e2e`).

---

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `running inside a VM (kvm)` in `preflight` | The host is a VM. Supported only for labs, with `--allow-virtualized-host`. |
| `/dev/kvm missing` | VT-x/AMD-V disabled in firmware, or the `kvm_intel`/`kvm_amd` module is not loaded. |
| `network class default -> br0: not a bridge on this host` | The bridge does not exist yet, or has a different name. See [Step 1](#step-1-a-bridge-for-guests). |
| `no network class, and no virbr0 bridge` | No bridge was named and libvirt's isn't present. Create one and pass `--network-class`. |
| `account ... uses uid ..., inside the guest range` or `... overlaps guest uids` | Move `--guest-uid-base` to a free range, clear of `/etc/subuid` and `/etc/subgid` (rootless containers). |
| `...: sha256 ..., expected ...; nothing was installed` | The download (or the file in `--artifacts-dir`) is not the file its digest names: a wrong mirror URL, or a wrong `--*-sha256`. Nothing was installed. |
| `... is not on PATH: install it with this host's package manager` in `preflight` | banlieue installs no packages. Install `systemd`, `swtpm` and `swtpm-tools` (see [Requirements](#requirements)). |
| `invalid --vmm-version: ... is older than v53.0` | The provider cannot drive that release. Pick the pinned one or newer. |
| `invalid --vmm-sha256: ... GitHub's digest for it could not be read` | A non-pinned release on a host that cannot reach `api.github.com`. Pass `--vmm-sha256` (and `--ch-remote-sha256`, `--firmware-sha256` as needed). |
| `invalid --vmm-sha256: ... is the pinned release; its sha256 is compiled in` | Digest flags are only for other releases. Drop the flag. |
| `swtpm_setup as banlieue: ...` in `selftest` | The EK CA directory has the wrong owner, often after copying `/var/lib/banlieue` by hand. Re-run `install --only tpm`. |
| `sudo: a terminal is required to read the password` with `--remote` | The workstation side ran without a terminal (piped, or from CI). Run it in an interactive terminal. |
| Provider unit `inactive` | Expected until the provider binary and `/etc/banlieue/kubeconfig` exist. |
