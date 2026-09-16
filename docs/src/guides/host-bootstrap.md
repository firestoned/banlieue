# Host Bootstrap: Bare Metal to a k0s Cluster

Every other guide in this section starts from a working management cluster.
This one starts from a machine with nothing on it.

`scripts/bootstrap-k0s-cluster.sh` builds that management cluster, but it
assumes its substrate already exists — it fails immediately in `check_deps_*`
if the tooling is absent, and `scripts/bootstrap-libvirt-tls.sh` assumes a
running `libvirtd`. This guide covers the step before both.

| Backend | What must exist first | Provided by |
| --- | --- | --- |
| libvirt | `virt-install`, `virsh`, `qemu-img`, `k0sctl`, `kubectl`, `curl`, `ssh`, `genisoimage`, a running `libvirtd`, storage pools | `bootstrap-libvirt-host.sh` |
| vSphere | `govc`, `jq`, `kubectl`, `ssh`, `base64`, a reachable vCenter, placement maps | `bootstrap-vsphere.sh` |

The two are deliberately asymmetric. A libvirt host is *provisioned* — packages,
daemon, pools, groups. A vSphere estate is not: vCenter and ESXi are managed
elsewhere, so there the work is preparing the client that drives `govc` and
discovering what already exists.

---

## The whole chain

=== "libvirt"

    ```sh
    sudo ./scripts/bootstrap-libvirt-host.sh all   # 1. hypervisor + tooling
    sudo ./scripts/bootstrap-libvirt-tls.sh  all   # 2. PKI, mutual TLS on 16514
    ./scripts/bootstrap-k0s-cluster.sh       all   # 3. Kairos VMs + k0sctl
    ```

=== "vSphere"

    ```sh
    ./scripts/bootstrap-vsphere.sh tools           # 1. govc, jq, kubectl
    ./scripts/bootstrap-vsphere.sh discover \
        > ~/.config/banlieue/hosts/prod.env        # 2. enumerate the estate
    # edit prod.env: pick placements, fill in NODES
    BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/prod.env \
      ./scripts/bootstrap-vsphere.sh verify        # 3. confirm it all exists
    BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/prod.env BACKEND=vsphere \
      ./scripts/bootstrap-k0s-cluster.sh all       # 4. clone templates + k0s
    ```

After either, continue with [End-to-End Setup](end-to-end-setup.md).

---

## Configuration lives outside this repository

All four scripts are configured entirely by environment variables, and all four
accept `BANLIEUE_ENV_FILE` pointing at a file of them. The convention is:

```
~/.config/banlieue/hosts/<name>.env
```

**Never commit one of these.** banlieue is a public repository and these files
name real hosts, addresses and accounts — see `rules/no-real-infrastructure.md`.
Keeping them under `~/.config` rather than in a `.gitignore`d directory means
there is no path by which a stray `git add -f` can publish them.

Every script prints a starting point:

```sh
./scripts/bootstrap-libvirt-host.sh --print-env-template
./scripts/bootstrap-vsphere.sh      --print-env-template
./scripts/bootstrap-k0s-cluster.sh  --print-env-template
```

Precedence is lowest to highest: built-in defaults → `BANLIEUE_ENV_FILE` →
process environment → subcommand.

!!! warning "Secrets do not belong in the env file either"
    A Tailscale auth key or `GOVC_PASSWORD` in a shared env file is a
    credential at rest. Keep them in a separate `0600` file that the env file
    sources, or export them in your shell. No script here writes a credential
    to disk or passes one on a command line.

---

## libvirt: bare Debian to hypervisor

Run **on the target host, as root**. Debian and Ubuntu are supported.

```sh
sudo ./scripts/bootstrap-libvirt-host.sh all
```

Subcommands run individually and are all idempotent — re-running is the
intended way to change a setting:

| Subcommand | Does |
| --- | --- |
| `base` | locale, `git curl wget ca-certificates rsync gnutls-bin` |
| `libvirt` | qemu/libvirt packages, `kvm-ok`, group membership, enable `libvirtd` |
| `pools` | storage pools and the default NAT network |
| `tools` | `k0sctl`, `kubectl`, `genisoimage` |
| `cockpit` | Cockpit web console |
| `tailscale` | join a tailnet |
| `status` | report what is installed; changes nothing |

### Storage pools do not go where libvirt puts them

The default is **not** `/var/lib/libvirt/images`. The script ranks
`/srv /data /home /opt /var/lib` by free space and uses the winner.

This is deliberate. A stock Debian installation puts `/var` on its own
partition, frequently under 10 GB. Disk images are the largest thing a
hypervisor stores, so the conventional location fills after two or three VMs
and fails mid-clone. Override with `POOL_ROOT` or reorder `POOL_CANDIDATES`.

```sh
POOL_ROOT=/srv/libvirt sudo -E ./scripts/bootstrap-libvirt-host.sh pools
```

### kubectl is pinned to the k0s minor, not "stable"

`kubectl` supports only ±1 minor version against the API server. The `stable`
channel tracks upstream Kubernetes, which runs ahead of the k0s release train —
follow it and you eventually install a client that refuses to talk to the
cluster this repository builds.

The script derives the right patch release from `K0S_VERSION` instead:

```
K0S_VERSION=1.35.5+k0s.0  ->  https://dl.k8s.io/release/stable-1.35.txt  ->  kubectl v1.35.8
```

Set `KUBECTL_VERSION` to override. The binary is checksum-verified against the
published `.sha256` before installation.

### Group membership needs a new login

`libvirt` and `kvm` group membership is granted by `usermod`, which only
affects **new** login sessions. Until you log out and back in, `virsh` will
fall back to `qemu:///session` — a separate, empty, per-user libvirt that
reports no pools and no networks. A correctly-provisioned host looks
unconfigured. If that happens, log out, or use `qemu:///system` explicitly.

### Optional: Cockpit

```sh
INSTALL_COCKPIT=true sudo -E ./scripts/bootstrap-libvirt-host.sh cockpit
```

Installs `cockpit-machines` alongside Cockpit, and **binds to `127.0.0.1` by
default**. Cockpit authenticates through PAM, so exposing port 9090 puts a
password-guessable administrative path onto the network — which rather
undoes the point of putting libvirt behind mutual TLS. Reach it over a tunnel:

```sh
ssh -L 9090:localhost:9090 bar.foo.io      # then https://localhost:9090
```

Set `COCKPIT_BIND=lan` to listen on all interfaces anyway.

!!! note
    Cockpit's VM page talks to libvirt over the local Unix socket as root. It
    does **not** use the TLS certificates from `bootstrap-libvirt-tls.sh`, so
    it is a second, independent access path to the same hypervisor.

### Optional: Tailscale

```sh
INSTALL_TAILSCALE=true TAILSCALE_AUTHKEY=tskey-auth-... \
  sudo -E ./scripts/bootstrap-libvirt-host.sh tailscale
```

Tailscale is not packaged in Debian, so the script adds the upstream apt
repository. With no `TAILSCALE_AUTHKEY` it falls back to interactive browser
authentication — which is preferable when you have no key handy, since it
never places a reusable credential on the host. An expired, spent or
ACL-rejected key is reported and falls back the same way rather than aborting.

`TS_ADVERTISE_ROUTES=192.0.2.0/24` exposes the libvirt subnet to the tailnet,
which is how nodes on a NAT bridge become reachable from elsewhere. Approve the
route in the Tailscale admin console afterwards.

!!! warning "Route collisions"
    libvirt's default network is `192.168.122.0/24` on *every* host. Advertising
    it from two hypervisors on one tailnet collides. Renumber one of them first.

---

## vSphere: workstation and estate

vCenter and ESXi are assumed to exist. Credentials come from the ambient
`GOVC_*` environment and are never printed, stored, or passed on a command line
by these scripts.

```sh
export GOVC_URL=https://vcenter.example.com/sdk
export GOVC_USERNAME=admin
export GOVC_PASSWORD=...          # or keep it in a 0600 file you source
export GOVC_INSECURE=true         # self-signed vCenter certificate
```

### 1. Tooling

```sh
./scripts/bootstrap-vsphere.sh tools
```

Installs `govc` and `kubectl` (same k0s-minor pinning as above), and `jq` via
apt or Homebrew. Works on Linux and macOS.

### 2. Discovery

Writing the per-cluster placement maps by hand from the vSphere UI is tedious
and easy to get wrong. `discover` enumerates the estate and emits a populated
env file:

```sh
./scripts/bootstrap-vsphere.sh discover > ~/.config/banlieue/hosts/prod.env
```

For every datacenter and compute cluster it finds, it emits the four variables
`bootstrap-k0s-cluster.sh` reads, with alternatives commented out beneath:

```sh
# --- cluster: /dc1/host/cluster-a  ->  id 'cluster_a' ---
VSPHERE_RP_cluster_a=/dc1/host/cluster-a/Resources
VSPHERE_DSC_cluster_a=/dc1/datastore/sdrs-a
# alt: VSPHERE_DSC_cluster_a=/dc1/datastore/sdrs-b
VSPHERE_NET_cluster_a=/dc1/network/pg-servers
VSPHERE_TPL_cluster_a=/dc1/vm/templates/kairos-hadron
```

**Discovery lists what exists; it cannot know what you intend.** Review every
value, delete the alternatives, and fill in the `NODES` table at the bottom.

The `<id>` is a sanitized cluster name — non-alphanumeric characters become
underscores, because the k0s script reads placement as flat shell variables
(`VSPHERE_RP_<id>`) rather than an associative array. macOS ships bash 3.2,
which has no `declare -A`.

### 3. Create what is missing

```sh
BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/prod.env \
  ./scripts/bootstrap-vsphere.sh create
```

Creates the VM folder, and a resource pool under each cluster if
`VSPHERE_CREATE_POOL` is set. It does **not** build VM templates — see
[Building a Kairos Hadron VM Template](building-kairos-hadron-template.md) or
[Building an Alpine VM Template](alpine-vsphere-template.md).

### 4. Verify before you clone

```sh
BANLIEUE_ENV_FILE=~/.config/banlieue/hosts/prod.env \
  ./scripts/bootstrap-vsphere.sh verify
```

Checks every object the env file names actually exists. Each of these failures
otherwise surfaces mid-run, after VMs have been cloned and half-configured —
cheap to catch here, expensive to unwind later.

### Spread nodes across clusters

Give each node a different `cluster_id` in `NODES` where your estate allows it.
Each vSphere cluster then becomes an **etcd failure domain**: losing one costs a
single control-plane node rather than the cluster. This is the same reasoning
ADR-0002 applies to `InfraCluster` placement.

```sh
NODES=(
  "k0s-01 CLUSTER_A 192.0.2.11 controller+worker"
  "k0s-02 CLUSTER_B 192.0.2.12 controller+worker"
  "k0s-03 CLUSTER_C 192.0.2.13 controller+worker"
)
```

---

## Sizing the cluster

`bootstrap-k0s-cluster.sh` defaults to `VM_COUNT=4` with
`NODE_ROLES="controller+worker controller+worker controller+worker worker"` —
three controllers plus a dedicated image-build worker.

!!! danger "Lowering VM_COUNT without NODE_ROLES"
    `NODE_ROLES` is consumed positionally. Setting `VM_COUNT=3` alone takes the
    first three entries — two controllers and one worker — giving an etcd
    quorum of **2**, which tolerates zero failures. Always set both:

    ```sh
    VM_COUNT=3 NODE_ROLES="controller+worker controller+worker controller+worker" \
      ./scripts/bootstrap-k0s-cluster.sh all
    ```

    With no pure worker the image-build labelling step reports that it is
    skipping and continues.

Per node the defaults are 2 vCPU, 8192 MB and 25 GB. Check the arithmetic
against the host before starting: three nodes is 24 GB of RAM and 75 GB of
disk, plus about 2 GB for the Kairos installer ISO.

---

## Verifying

```sh
./scripts/bootstrap-libvirt-host.sh status
```

```text
--- tooling ---
  virt-install   /usr/bin/virt-install
  k0sctl         /usr/local/bin/k0sctl
  kubectl        /usr/local/bin/kubectl
  genisoimage    /usr/bin/genisoimage
--- libvirt ---
  libvirtd active
   Name      State    Autostart
   default   active   yes
   iso       active   yes
```

`status` needs no privileges and changes nothing, so it is safe to run against
a host at any point.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `virsh` lists no pools or networks | Running as a non-root user without a fresh login, so `virsh` used `qemu:///session`. Log out and back in, or set `LIBVIRT_DEFAULT_URI=qemu:///system`. |
| `sudo: a terminal is required to read the password` | The script was piped or run from a non-interactive shell. `sudo` needs a tty; run it directly in a terminal. |
| `kvm-ok` reports no acceleration | VT-x/AMD-V is disabled in firmware. VMs will run under emulation, unusably slowly. |
| `Cannot read CA certificate` from `virsh` | `bootstrap-libvirt-tls.sh` has not run, or `/etc/pki` directories are `drwx------` and unreadable by non-root. |
| `govc about` fails | Wrong `GOVC_URL`, or a self-signed certificate without `GOVC_INSECURE=true`. |
| `mkdir: cannot create directory '/var/lib/libvirt/images/...': Permission denied` | `POOL_DIR` resolved to libvirt's stock path on a host whose pool lives elsewhere. Recent versions derive it from the `default` pool; on older ones set `POOL_DIR=<pool path>/k0s-bootstrap` explicitly. |
| k0sctl cannot reach the nodes | On libvirt, nodes are on the NAT bridge and reachable only from the hypervisor. Run the k0s script *on* the host, or advertise the subnet over Tailscale. |
