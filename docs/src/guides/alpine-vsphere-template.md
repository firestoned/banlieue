# Guide: Building an Alpine VM Template on VMware vSphere 8 (govc)

This guide builds a **cloud-init-ready Alpine Linux template** on vSphere 8
(vCenter + ESXi 8) using **[govc](https://github.com/vmware/govmomi/tree/main/govc)**
for every vCenter operation — no Web Client. The result is a template the
banlieue [vSphere provider](vsphere-provider.md) can clone.

The provider defaults to `guestAgent: cloud-init` and delivers
`VirtualMachine.spec.userData` into the guest through VMware's **guestinfo**
datasource — so a bare Alpine install is not enough. The template must have:

1. **`open-vm-tools`** — so vCenter can read/write guest properties and report
   the IP back to the provider.
2. **`cloud-init` with the VMware guestinfo / NoCloud datasource** — so the
   `userData` banlieue injects at clone time is actually applied.
3. **A clean, generalized image** — no machine-id, no SSH host keys, no leases,
   so every clone is unique.

Skip any of these and clones boot but ignore `userData` (no users, no SSH keys,
no network config), and the provider never sees a guest IP.

> **banlieue contract.** The template's **name** is what you put in
> `VMImage.spec.sources[].ref` for `providerClass: vsphere` (`kind: Template`).
> This guide produces `alpine-3.21-cloudinit`; the matching `VMImage` is at the
> end.

---

## Prerequisites

- **govc** installed (`brew install govc`, or grab a release from
  [vmware/govmomi](https://github.com/vmware/govmomi/releases)).
- Permissions to upload to a datastore, create VMs, and mark a VM as a template.
- Network reachability to vCenter on 443.

### Configure govc

banlieue already standardises on `GOVC_*` env vars (see the
[vSphere Provider guide](vsphere-provider.md) and
`deploy/provider-vsphere/README.md`). Set them once:

This guide targets a **standalone ESXi 8 host** (no vCenter — the common lab
setup). ESXi's implicit datacenter is always **`ha-datacenter`** ("ha-" is
historical, from its HA-agent origins); you don't create it. If you *are* going
through vCenter, see the note after the block.

```sh
# Point govc at the ESXi HOST itself (not a vCenter).
export GOVC_URL="https://esxi-01.example.com"      # the ESXi host, no /sdk needed
export GOVC_USERNAME="root"
export GOVC_PASSWORD="********"
export GOVC_INSECURE=1                              # 1 for the host's self-signed cert

# Standalone ESXi: the datacenter is ALWAYS ha-datacenter; the only resource pool
# is the host's root pool. There is no cluster.
export GOVC_DATACENTER="ha-datacenter"
export GOVC_DATASTORE="datastore1"                 # `govc datastore.info` to list
export GOVC_RESOURCE_POOL="*/Resources"            # the host's root pool (ha-datacenter)
export GOVC_NETWORK="VM Network"                   # a port group with DHCP

govc about                                         # sanity check: prints ESXi version
govc ls -l /ha-datacenter/...                      # see the inventory if curious
```

> **Through vCenter instead?** Set `GOVC_URL` to the vCenter, use your SSO user,
> and replace the bottom three: `GOVC_DATACENTER` is your real datacenter name
> (`govc datacenter.info` to list), `GOVC_RESOURCE_POOL` is
> `/<DC>/host/<Cluster>/Resources`, and add `GOVC_HOST=<esxi-host>` only if you
> want to pin placement (otherwise DRS picks). The rest of the guide is identical.

Pick a working set of names for the rest of the guide:

```sh
VM=alpine-template                 # build VM (becomes the template)
TEMPLATE=alpine-3.21-cloudinit     # final template name → VMImage ref
ALPINE_VER=3.21.0
ISO_LOCAL="alpine-virt-${ALPINE_VER}-x86_64.iso"
ISO_DS="iso/${ISO_LOCAL}"          # path on the datastore
```

---

## Step 1 — Get the Alpine ISO into the datastore

Download the **"Virtual"** ISO (smallest, tuned for VMs) and upload it with
`govc datastore.upload`. No Web Client, no datastore browser.

```sh
# 1. Download the Alpine virt ISO + checksum, and verify it.
base="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VER%.*}/releases/x86_64"
curl -fSLO "${base}/${ISO_LOCAL}"
curl -fSL  "${base}/${ISO_LOCAL}.sha256" -o "${ISO_LOCAL}.sha256"
shasum -a 256 -c "${ISO_LOCAL}.sha256"     # must print: OK   (Linux: sha256sum -c)

# 2. Make an iso/ folder on the datastore (idempotent) and upload.
govc datastore.mkdir -p iso
govc datastore.upload "${ISO_LOCAL}" "${ISO_DS}"

# 3. Confirm it's there.
govc datastore.ls -l iso/
```

> `govc datastore.upload <local> <remote>` puts the file at
> `[$GOVC_DATASTORE] <remote>`. The upload streams over HTTPS to vCenter; large
> ISOs resume cleanly if re-run.

---

## Step 2 — Create the base VM

Create an empty VM with vSphere-8 virtual hardware and paravirtual devices, then
add a thin disk. `govc vm.create` uses `GOVC_DATASTORE` / `GOVC_RESOURCE_POOL` /
`GOVC_NETWORK` from the environment (plus `GOVC_HOST` if you set it, vCenter
only).

```sh
govc vm.create \
  -on=false \
  -version=20 \
  -firmware efi \
  -g otherLinux64Guest \
  -c 2 -m 1024 \
  -disk 2GB -disk.controller pvscsi \
  -net "$GOVC_NETWORK" -net.adapter vmxnet3 \
  "$VM"
```

- `-version=20` — HW v20 (ESXi 8.0+).
- `-firmware efi` — **UEFI**, the right default for a new vSphere 8 template (BIOS
  is legacy). Use plain `efi`, **not** `efi-secure`: Alpine doesn't ship signed
  shim/bootloaders, so UEFI **Secure Boot** would refuse to boot the installer.
  Firmware is fixed once the OS is installed — pick it here, at create time; you
  can't flip BIOS↔UEFI on an installed guest without reinstalling.
- `-g otherLinux64Guest` — Alpine has no dedicated guest-id; this is the correct
  generic 64-bit Linux.
- `-disk.controller pvscsi` + `-net.adapter vmxnet3` — paravirtual disk and NIC.
  The Alpine **virt** kernel ships both drivers, so the install and every clone
  come up with working disk and network out of the box.

Attach the ISO and point the VM at the CD:

```sh
# Add a CD-ROM, capture the device name it returns (e.g. cdrom-3000).
CD=$(govc device.cdrom.add -vm "$VM")

# Insert the uploaded ISO and make sure it's connected at power-on.
govc device.cdrom.insert -vm "$VM" -device "$CD" "$ISO_DS"
govc device.connect      -vm "$VM" "$CD"

# Boot from CD first (so the installer comes up before the empty disk).
# (Firmware is already EFI from vm.create above — nothing to set here.)
govc device.boot -vm "$VM" -order cdrom,disk
```

---

## Step 3 — Install Alpine (serial / VNC console)

Power on and open a console. govc gives you both a remote-console URL and direct
VNC:

```sh
govc vm.power -on "$VM"

# Option A: print an HTML5 console URL (opens in a browser, but it's govc-issued
# — no manual Web Client navigation):
govc vm.console -h5 "$VM"

# Option B: enable + open a VNC endpoint and connect with any VNC client:
# govc vm.vnc -enable -port 5901 -password secret "$VM"
# govc vm.vnc -ls "$VM"        # prints the vnc://host:port to connect to
```

In the console, log in as `root` (no password) and run the installer:

```sh
setup-alpine
```

Answer the prompts:

- **Keyboard / hostname:** anything (hostname is reset later — e.g. `alpine-template`).
- **Network:** `eth0`, **dhcp**; decline manual config.
- **Root password:** set one (used only during the build; cloud-init manages
  real users on clones).
- **Timezone:** `UTC`.
- **Mirror:** pick a fast one (or `f` for fastest).
- **SSH server:** **openssh**.
- **Disk:** select `sda`, mode **sys** (installs to disk — *not* `data`/`lvm`).
  Confirm the wipe.

When it finishes, **don't reboot from the console** — eject the ISO from the host
side so the VM next boots from disk:

```sh
# Power off, disconnect + remove the CD, set boot order to disk.
govc vm.power -off -force "$VM"
govc device.disconnect -vm "$VM" "$CD"
govc device.cdrom.eject -vm "$VM" -device "$CD"
govc device.boot -vm "$VM" -order disk
govc vm.power -on "$VM"
```

Reconnect the console (`govc vm.console -h5 "$VM"`) and log in as `root`.

---

## Step 4 — Install open-vm-tools, cloud-init, and dependencies

Enable the **community** repository (cloud-init lives there), then install. Run
these **inside the guest** console:

```sh
# Enable the community repo for THIS release (3.21 shown; edit for yours).
sed -i '/v3\.21\/community/s/^#//' /etc/apk/repositories
apk update

apk add open-vm-tools open-vm-tools-plugins-all \
        cloud-init \
        e2fsprogs-extra blkid \
        py3-netifaces \
        openssh sudo bash chrony
```

> **No `cloud-init-vmware-guestinfo` package.** The standalone connector is
> legacy — the VMware **guestinfo** datasource is now **built into cloud-init**
> (`DataSourceVMware`). If you see `ERROR: ... cloud-init-vmware-guestinfo (no
> such package)`, just drop it from the line above (already done here). Confirm
> the built-in source is present:
> ```sh
> ls /usr/lib/python3*/site-packages/cloudinit/sources/ | grep -i vmware
> # expect: DataSourceVMware.py   (older builds: DataSourceOVF.py)
> ```

Key packages:

- **`open-vm-tools` (+ `-plugins-all`)** — guest agent; lets vCenter report the
  guest IP (which banlieue surfaces in VM status) and run guest operations.
- **`cloud-init`** — applies `userData` on first boot; ships the built-in VMware
  guestinfo datasource that receives `userData` from a vSphere clone.
- **`e2fsprogs-extra`** — provides `resize2fs` so cloud-init's `growpart`/`resizefs`
  expands the root disk to the clone's size.
- **`chrony`** — time sync (clones boot at arbitrary times).

Enable services on boot:

```sh
rc-update add open-vm-tools default
rc-update add cloud-init default
rc-update add cloud-init-local default
rc-update add cloud-config default
rc-update add cloud-final default
rc-update add chronyd default
rc-update add sshd default
```

> If a `cloud-*` service "does not exist", your cloud-init build uses a single
> `cloud-init` service — fine; just ensure `cloud-init` is added.

---

## Step 5 — Point cloud-init at the VMware datasource

Inside the guest, constrain cloud-init to the datasources that work on vSphere so
it doesn't waste boot time probing clouds:

```sh
cat > /etc/cloud/cloud.cfg.d/99-vsphere.cfg <<'EOF'
# banlieue/vSphere: read user-data from VMware guestinfo, fall back to NoCloud.
# 'VMware' is the modern built-in datasource name; 'VMwareGuestInfo'/'OVF' cover
# older cloud-init. cloud-init ignores names it doesn't recognise, so listing all
# is harmless and version-proof.
datasource_list: [ VMware, VMwareGuestInfo, OVF, NoCloud, None ]

# Let the deploy tool (banlieue) own hostname + users via user-data.
preserve_hostname: false
EOF
```

- **`VMware`** (built into cloud-init; `VMwareGuestInfo`/`OVF` on older builds)
  reads `guestinfo.userdata` / `guestinfo.metadata` set on the VM — the mechanism
  a vSphere clone uses to hand `userData` to the guest.
- **`NoCloud`** is the fallback (e.g. a seed ISO), handy for manual testing.

---

## Step 6 — Generalize (the most important step)

Inside the guest, remove every per-machine identity so each clone is unique. **Run
this last, then power off — do not reboot.**

```sh
cloud-init clean --logs --seed            # clear cloud-init "already ran" state
rm -f /etc/ssh/ssh_host_*                  # regenerated per clone
truncate -s 0 /etc/machine-id              # regenerated per clone (empty, not missing)
rm -f /var/lib/dbus/machine-id 2>/dev/null || true
rm -f /var/lib/dhcp/* /var/lib/dhcpcd/* 2>/dev/null || true
rm -rf /var/log/* /tmp/* /root/.ash_history
apk cache clean 2>/dev/null || true
dd if=/dev/zero of=/zero bs=1M 2>/dev/null; rm -f /zero; sync   # zero free space (optional)

poweroff
```

> **Why "no reboot after this":** the first boot after generalization is meant to
> be a *clone's* first boot, where cloud-init regenerates IDs and applies
> user-data. Booting the template itself consumes that first-run state.

The `poweroff` drops the VM; confirm it's off from outside:

```sh
govc vm.power -off "$VM" 2>/dev/null || true   # no-op if already off
govc vm.info "$VM" | grep -i 'Power state'      # want: poweredOff
```

---

## Step 7 — Convert to a template

```sh
# (Optional) rename the build VM to the final template name first.
govc object.rename "/${GOVC_DATACENTER}/vm/${VM}" "$TEMPLATE"

# Mark it as a template.
govc vm.markastemplate "$TEMPLATE"

# Verify.
govc vm.info "$TEMPLATE" | grep -iE 'Name|Template|Power'
```

`vm.markastemplate` flips the VM to a template in place. The name
(`alpine-3.21-cloudinit`) is exactly what banlieue's provider looks up. Keep it in
a folder/datacenter the provider's vCenter user can see — the same datacenter the
`Provider` connects to.

---

## Step 8 — Register it with banlieue

Create a `VMImage` whose vSphere source `ref` is the template name (mirrors
`examples/04-vmimage-ubuntu.yaml`):

```yaml
apiVersion: banlieue.io/v1alpha1
kind: VMImage
metadata:
  name: alpine-3.21-cloudinit
spec:
  osFamily: linux
  osDistribution: alpine
  osVersion: "3.21"
  architecture: amd64
  guestAgent: cloud-init
  sources:
    - providerClass: vsphere
      kind: Template
      ref: alpine-3.21-cloudinit       # <-- the vCenter template name from Step 7
```

Apply it, then reference it from a `VirtualMachine` (see the
[vSphere Provider guide](vsphere-provider.md) for the full `Provider` +
`VMClass` + `VirtualMachine` flow). banlieue clones the template, injects
`spec.userData` via guestinfo, and cloud-init applies it on first boot.

---

## Verifying a clone consumed user-data

After a clone boots (via banlieue, or a manual `govc vm.clone` for testing):

```sh
# Quick manual clone to test the template end-to-end (optional):
govc vm.clone -vm "$TEMPLATE" -on=false alpine-smoke
# inject test user-data via guestinfo, base64-encoded:
govc vm.change -vm alpine-smoke \
  -e guestinfo.userdata="$(printf '#cloud-config\nusers:\n  - name: tester\n    sudo: ALL=(ALL) NOPASSWD:ALL\n' | base64)" \
  -e guestinfo.userdata.encoding=base64
govc vm.power -on alpine-smoke

# Watch the guest IP appear (proves open-vm-tools works — same IP banlieue reports):
govc vm.ip alpine-smoke
```

On the guest itself:

```sh
cloud-init status --long          # want: status: done
cloud-init query userdata | head  # the user-data that was delivered
cat /var/log/cloud-init.log
```

Clean up the smoke-test clone:

```sh
govc vm.destroy alpine-smoke
```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `vm.create` → "Current license or ESXi version prohibits…" | free ESXi (`esx.hypervisor.*`) has no API write path; or expired eval; or a pinned `GOVC_HOST` that's restricted | `govc license.ls` — edition must be Standard/Enterprise/Evaluation, not `esx.hypervisor.*`. Assign one (`govc license.add KEY` + `govc license.assign KEY`), or `unset GOVC_HOST` to let the cluster place it. |
| Installer never appears / UEFI shell or "no boot device" | created with `efi-secure` (Alpine is unsigned), or booted disk before CD | Recreate with `-firmware efi` (not `efi-secure`); ensure `device.boot -order cdrom,disk` while the ISO is connected. |
| `govc datastore.upload` 401/403 | bad `GOVC_*` creds or perms | `govc about` to confirm auth; check datastore privileges. |
| Clone boots but ignores user-data | cloud-init didn't find the datasource | Re-check Step 5 `datasource_list`; on the clone run `cloud-init query --all`; confirm `guestinfo.userdata.encoding=base64` is set when the data is base64. |
| All clones share an SSH host key / machine-id | generalization skipped or template rebooted after Step 6 | Re-run Step 6, **don't** power on, re-mark as template. |
| No guest IP from `govc vm.ip` / in banlieue | `open-vm-tools` not running | In the template: `rc-update add open-vm-tools default && service open-vm-tools start`, then re-generalize + re-template. |
| Root disk stays 2 GB on bigger clones | `resize2fs` missing | Ensure `e2fsprogs-extra` is installed. |
| cloud-init re-applies user-data every boot | state not cleaned | `cloud-init clean --seed` was missing in Step 6. |
| `apk add cloud-init` → not found | community repo disabled | Uncomment the `community` line for your release in `/etc/apk/repositories`, `apk update`. |

---

## Notes on versions

- Pin a specific Alpine release in the template **name** and **`VMImage`** so an
  upgrade is a new, separately-reviewable template, not an in-place change.
- Alpine's cloud-init packaging shifts between releases — if a Step 4 package name
  isn't found, check `apk search cloud-init` and the
  [Alpine cloud-init wiki](https://wiki.alpinelinux.org/wiki/Cloud-init).
- The VMware guestinfo datasource is **built into cloud-init** (`DataSourceVMware`,
  named `VMware`); there is no `cloud-init-vmware-guestinfo` package on current
  Alpine. The Step 5 `datasource_list` lists `VMware`/`VMwareGuestInfo`/`OVF` so it
  works across cloud-init versions, with `NoCloud` as fallback.
- All `govc` commands here read `GOVC_DATACENTER` / `GOVC_DATASTORE` /
  `GOVC_RESOURCE_POOL` / `GOVC_NETWORK` (and `GOVC_HOST` on vCenter) from the
  environment — set them once (top of the guide) and the commands stay short. On
  standalone ESXi the datacenter is `ha-datacenter` and there's no `GOVC_HOST`.
