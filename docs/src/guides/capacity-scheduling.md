# Guide: capacity-aware scheduling, reading it and testing it

Every provider publishes what its failure domains can still take: CPUs,
memory, measured free memory, and free disk per storage class. The
scheduler places a new `VirtualMachine` only where it fits
([ADR-0107](https://github.com/firestoned/banlieue/blob/main/docs/adr/0107-capacity-aware-scheduling.md),
[ADR-0128](https://github.com/firestoned/banlieue/blob/main/docs/adr/0128-every-provider-publishes-measured-capacity.md)).
This guide covers three things:

1. how to read what a Provider publishes and why a VM was refused;
2. the two settings that change the check;
3. how to test it on a real backend, with a scenario-by-scenario plan for
   vSphere and the commands used for Cloud Hypervisor and libvirt.

## 1. What each failure domain publishes

Each `Provider.status.failureDomains[].attributes.capacity` carries:

```yaml
capacity:
  cpus: 32                      # what guests may use, in total
  memoryMiB: 61440              # what guests may use, in total
  memoryOverheadMiBPerVM: 256   # added to each VM's memory (absent on vSphere)
  memoryAvailableMiB: 40960     # measured free for a new guest, whole GiB
  storage:                      # one entry per available storage class
    - storageClass: gold
      totalGiB: 718
      freeGiB: 512              # whole GiB, rounded down
      shared: false             # true: the same store backs other failure domains
```

Where each figure comes from:

| Backend | Failure domain | `cpus`, `memoryMiB` | `memoryAvailableMiB` | `storage[]` | `shared` |
| --- | --- | --- | --- | --- | --- |
| Cloud Hypervisor | the host | `/proc/cpuinfo`, `MemTotal` less hugepages, capped by the guests' slice budget | `MemAvailable`, or the slices' `MemoryMax` less `MemoryCurrent` when smaller | `statvfs` of each storage class directory (`f_bavail`) | always false |
| libvirt | the host | `virNodeGetInfo` | `virNodeGetMemoryStats`: free + buffers + cached | `virStoragePoolGetInfo` of each pool a class targets, after a refresh | always false |
| Proxmox | one per node | `GET /nodes`: `maxcpu`, `maxmem` | `maxmem` less `mem` | `GET /nodes/{node}/storage`: `total`, `avail` | the storage's own `shared` flag |
| vSphere | one per (datacenter, cluster) | cluster summary: `numCpuThreads`, `effectiveMemory`, capped by the root resource pool's `maxUsage` | the smaller of `effectiveMemory` less the hosts' `overallMemoryUsage`, and the root pool's `unreservedForVm` | `Datastore.summary`: `capacity`, `freeSpace`; for a `datastoreCluster` class, the member with the most free space | true when more than one cluster of the Provider reaches the datastore |

A figure the provider could not read is left out, and the scheduler does not
check what is not there. vSphere publishes no per-VM overhead, because
vCenter accounts for its own. libvirt and Proxmox use an estimated 256 MiB
per QEMU process.

### Reading it

```sh
# Every failure domain of a Provider, with its capacity
kubectl get provider <name> -n banlieue-system -o json \
  | jq '.status.failureDomains[] | {name, capacity: .attributes.capacity}'

# Just the storage, as a table
kubectl get provider <name> -n banlieue-system -o json \
  | jq -r '.status.failureDomains[] | .name as $fd
      | .attributes.capacity.storage[]?
      | [$fd, .storageClass, .totalGiB, .freeGiB, .shared] | @tsv'
```

The measured figures refresh on the provider's reconcile (every 30 s by
default). Providers round them down to whole GiB and patch status only when
the rounded value changes, because every Provider status change requeues
every `VirtualMachine`.

## 2. How a placement is checked

The checks run in this order, for each failure domain that already matches
the VM's selectors, classes and features:

1. **Reservation (ADR-0107), always.** The memory of the VMs banlieue has
   scheduled there (plus overhead), plus this VM's, must fit `memoryMiB`.
   Their vCPUs must fit `cpus` times `spec.scheduling.cpuOvercommitRatio`.
2. **Measured (ADR-0128), new placements only.** This VM's memory plus
   overhead must fit `memoryAvailableMiB`, less the memory of *in-flight*
   VMs. For each storage class its disks use, the sum of its disk sizes in
   that class (`hardwareOverride.diskOverrides` applied) must fit:

   ```text
   usable = freeGiB - (totalGiB x storageReservePercent / 100) - in-flight disks in that class
   ```

An **in-flight** VM is scheduled to that failure domain but its
infrastructure object is not provisioned yet. Its disks are promised but
not yet visible in `freeGiB`. For a store marked `shared`, in-flight VMs on
every failure domain of the same Provider count.

A **placed VM is never moved or flagged by a measured figure**: its own use
is already part of the measurement. A placed VM is moved only when its
placement fails ADR-0125's checks (selectors, classes, reservation).

!!! warning "Known gaps"
    - **Thin disks count at full size only while in flight.** Once a VM is
      provisioned, `freeGiB` holds only what its thin disk occupies so far,
      so a later VM can be placed against space the first may still grow
      into. Thick and `eagerZeroed` disks take their full size from
      `freeGiB` at once.
    - **`hardwareOverride.cpus` and `hardwareOverride.memoryMiB` are not
      counted.** The scheduler sizes a VM by its `VMClass`. Use a class of
      the size you mean when testing.
    - **Two storage classes on one store are checked separately** and can
      together overbook it. Give each store one class.

### The two settings

```yaml
apiVersion: banlieue.io/v1alpha1
kind: Provider
spec:
  scheduling:
    cpuOvercommitRatio: 4       # 1 to 64, default 4: vCPUs per published CPU
    storageReservePercent: 10   # 0 to 50, default 10: share of each store kept free
```

### Why a VM was refused

A VM that fits nowhere stays `Scheduled=False` with reason
`NoFailureDomainMatched`, and the message names each failure domain and why:

```sh
kubectl get vm <name> -n <ns> \
  -o jsonpath='{.status.conditions[?(@.type=="Scheduled")].message}{"\n"}'
```

| Message (per failure domain) | Check | What to do |
| --- | --- | --- |
| `insufficient capacity: memory 34816 MiB > 32768 MiB` | reservation: memory already scheduled plus this VM | add a host, use a smaller class, or raise the budget |
| `insufficient capacity: vCPUs 132 > 32 CPUs x 4` | reservation: vCPUs against the overcommit ratio | raise `cpuOvercommitRatio`, or add capacity |
| `insufficient capacity: memory available 8192 MiB < 16640 MiB (measured, less in-flight VMs)` | measured free memory | something else uses the memory: free it, or wait for in-flight VMs to finish |
| `insufficient capacity: storage class 'gold': 500 GiB > 182 GiB (225 free less 43 reserve and 0 in flight)` | measured free disk | free space on the store, lower `storageReservePercent`, or add a store |

The message keeps the first few reasons and ends with `… and N more` on a
large topology.

## 3. Testing

Each layer proves something the one below cannot.

| Layer | Proves | Run |
| --- | --- | --- |
| Unit | every rule above, against fakes | `cargo test -p banlieue-controller --lib scheduler` and each provider's `cargo test` |
| Live read, per backend | the backend answers the reads, and the figures are plausible | below, read-only |
| In-cluster scenarios | the scheduler places and refuses against real figures | section 4 |

Live reads, all read-only:

```sh
# Cloud Hypervisor, on the host itself
CH_STORAGE_DIR=/srv/banlieue/ch \
  cargo test -p banlieue-provider-cloud-hypervisor --test live_capacity -- --ignored --nocapture

# libvirt, from anywhere that reaches the host over TLS
LIBVIRT_HOST=bar.foo.io LIBVIRT_TLS_DIR=~/.config/banlieue/<host>/libvirt \
  cargo test -p banlieue-libvirt --test live_libvirtd capacity_reads_against_real_libvirtd -- --ignored --nocapture

# vSphere
VSPHERE_ENDPOINT=https://vcenter.example.com/sdk \
VSPHERE_USERNAME='svc-banlieue@vsphere.local' VSPHERE_PASSWORD='…' \
VSPHERE_CA_BUNDLE=/path/to/ca.pem \
  cargo test -p banlieue-provider-vsphere --test live_vcenter capacity_reads_against_real_vcenter -- --ignored --nocapture
```

Run the vSphere test by name, not through `make vsphere-live-test`: that
target runs every ignored test in the file, and the template and guestinfo
tests need variables of their own.

## 4. Test plan for vSphere

Use the account and the topology the Provider uses in production: the
provider's own vCenter role and one Provider per vCenter. A test with more
privilege than production can pass where production fails.

### 4.0 Preflight: read before installing

1. Run `capacity_reads_against_real_vcenter` (section 3). It prints, for
   every cluster, the raw reads and the failure domain the reconciler would
   publish, and fails on an implausible figure. It maps every reachable
   datastore to a storage class of its own, so it covers datastores your
   Provider does not use too.
2. Compare it with the vSphere Client:

    | Printed | vSphere Client |
    | --- | --- |
    | `cpu_threads`, `cpus` | cluster, Summary: logical processors |
    | `effective_memory_mib`, `memoryMiB` | cluster, Summary: memory capacity, less what HA admission control or maintenance mode holds back |
    | `hosts_memory_used_mib` | the sum of each host's memory used |
    | `memoryAvailableMiB` | capacity less used, rounded down to a whole GiB (or the root pool's unreserved memory, if smaller) |
    | `totalGiB`, `freeGiB` | datastore, Summary: capacity and free, rounded down |
    | `shared: true` | the datastore's Hosts tab lists hosts of more than one cluster |

### 4.1 Install the exact version

Install the same image tested on Cloud Hypervisor and libvirt, pinned by
digest. Render, diff, then apply:

```sh
banlieue bootstrap operator --version <tag> --image-digest sha256:<digest> --dry-run > install.yaml
kubectl diff --server-side -f install.yaml
kubectl apply --server-side --force-conflicts -f install.yaml
```

The diff should show only image and ProviderClass pin changes, plus
additions to the Provider CRD (`memoryAvailableMiB`, `storage`,
`storageReservePercent`). Then wait for the vSphere provider's workload to
roll and its Provider to report `Ready=True` with a fresh heartbeat.

### 4.2 Scenarios

Run them in a test namespace with a test label, so cleanup is one command.
Use a `thin` test class, `desiredPowerState: PoweredOff` where a scenario
does not need the guest to boot, and a `failureDomainSelector` so each
scenario names one cluster.

```yaml title="capacity-test-class.yaml"
apiVersion: banlieue.io/v1alpha1
kind: VMClass
metadata:
  name: capacity-test
spec:
  hardware:
    cpus: 2
    memoryMiB: 4096
    disks:
      - { name: root, sizeGiB: 20, storageClass: gold, provisioning: thin }
  network:
    interfaces:
      - { name: eth0, networkClass: prod, ipam: {} }
```

Work out the usable space for a class on a failure domain first:

```sh
kubectl get provider <name> -n banlieue-system -o json | jq --arg fd <failure-domain> --arg sc gold '
  (.spec.scheduling.storageReservePercent // 10) as $r
  | .status.failureDomains[] | select(.name == $fd)
  | .attributes.capacity.storage[] | select(.storageClass == $sc)
  | {freeGiB, totalGiB, reserve: (.totalGiB * $r / 100 | floor),
     usable: (.freeGiB - (.totalGiB * $r / 100 | floor))}'
```

| # | Scenario | How | Expected |
| --- | --- | --- | --- |
| V1 | Figures published | read the Provider (section 1) | every failure domain carries `cpus`, `memoryMiB`, `memoryAvailableMiB` and one `storage[]` entry per available class; they match the preflight |
| V2 | A VM that fits | one VM of `capacity-test` | `Scheduled=True`, then provisioned and Ready; in vCenter, the VM sits on the class's datastore |
| V3 | A disk too big | a VM with `diskOverrides: [{name: root, sizeGiB: <usable + 100>}]` | stays `Scheduled=False`, message `storage class 'gold': <size> GiB > <usable> GiB (<free> free less <reserve> reserve and 0 in flight)`; nothing created in vCenter |
| V4 | In flight | two VMs, each with a root disk of 60 % of usable, created together | the first is scheduled; the second is refused with `… and <size> in flight` while the first clones; see V5 for what follows |
| V5 | Thin versus thick, after provisioning | V4 with a `thin` class, then again with an `eagerZeroed` class | thin: once the first is provisioned, the second is placed (the known gap); thick: `freeGiB` drops by the disk's size and the second stays refused |
| V6 | Shared datastore | a class whose datastore two clusters reach; V4 with each VM selecting a different cluster | `storage[].shared: true` on both failure domains; the second VM is refused with the first counted in flight, although it was placed on the other cluster |
| V7 | Datastore cluster | a class targeting `datastoreCluster` | `storage[]` shows the member with the most free space; V3 against it is refused |
| V8 | Measured memory | a `VMClass` whose `memoryMiB` is above `memoryAvailableMiB` but below `memoryMiB` | refused with `memory available … MiB < … MiB (measured, less in-flight VMs)` |
| V9 | Reservation memory | a `VMClass` whose `memoryMiB` is above `memoryMiB` | refused with `memory … MiB > … MiB` |
| V10 | The reserve is applied | on a test Provider, set `storageReservePercent: 0`, then `50` | V3's message shows `0 reserve`, then half of `totalGiB`; a VM refused at 50 fits at 0 |
| V11 | Placed VMs stay | with V2 running, fill the datastore from outside banlieue until V3's usable is below V2's disk | V2 stays scheduled and Ready, not moved; only new VMs are refused |
| V12 | Status stays quiet | watch the Provider for 5 minutes with no change in vCenter | `resourceVersion` changes only on heartbeats, not on every reconcile |

For V12:

```sh
kubectl get provider <name> -n banlieue-system -w \
  -o custom-columns=RV:.metadata.resourceVersion,HB:.status.workload.heartbeat
```

### 4.3 Cleanup

```sh
kubectl delete vm -n <ns> -l test=capacity
kubectl delete vmclass capacity-test
```

Then check vCenter: deleting the VMs removes their clones and disks.

## 5. Cloud Hypervisor and libvirt

The same scenarios apply. What differs:

- **Cloud Hypervisor.** One failure domain per host. `memoryAvailableMiB`
  follows the guests' slice budget when one is set
  (`--guest-memory-max`). A small storage partition gates placement: if no
  VM fits on a host, check `df -h` of its storage class directory first. See
  [Cloud Hypervisor Provider](cloud-hypervisor-host.md).
- **libvirt.** One failure domain per host. `freeGiB` is the pool's
  `available`; compare with `virsh pool-info <pool>`, and free memory with
  `virsh nodememstats`. A pool on the same filesystem as another Provider's
  store is counted by each Provider separately.
- **Proxmox.** One failure domain per node; storage marked `shared` in
  Proxmox (Ceph, NFS) counts in-flight disks across nodes, as V6 does for
  vSphere.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| No `capacity` on a failure domain | provider logs for `unreadable`: each read is best effort, and a failed one leaves its figure out. On vSphere, the account needs read access to the cluster, its hosts, its root resource pool and the datastores |
| `storage[]` has no entry for a class | the class is not in `availableStorageClasses` for that failure domain, or its target is a tag (`tagCategory`/`tag`), which is not measured |
| Every new VM refused for storage on one store | `freeGiB` less the reserve is below the class's disk size: grow the store or lower `storageReservePercent` |
| Figures lag the backend | up to one provider reconcile (30 s), plus rounding to whole GiB |
| A VM placed beyond what the store can hold | thin disks after provisioning (section 2, known gaps), or two classes on one store |

## Reference

- [ADR-0107: capacity-aware scheduling](https://github.com/firestoned/banlieue/blob/main/docs/adr/0107-capacity-aware-scheduling.md)
- [ADR-0125: a placed VM keeps a placement that still fits](https://github.com/firestoned/banlieue/blob/main/docs/adr/0125-a-placed-vm-keeps-a-placement-that-still-fits.md)
- [ADR-0128: every provider publishes measured capacity](https://github.com/firestoned/banlieue/blob/main/docs/adr/0128-every-provider-publishes-measured-capacity.md)
- [API reference: `FailureDomainCapacity`](../reference/api.md)
