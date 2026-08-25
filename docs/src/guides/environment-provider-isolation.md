# Guide: Environment / Provider Isolation

A recurring question once you have more than one environment (dev, qa,
prod) hitting the same vCenter: **does each environment need its own
`Provider` CR, or can one `Provider` serve all of them?** This guide gives
the rule and walks through the concrete case that motivates it — the same
vCenter clusters, the same datastores, but a different network per
environment.

## The rule

**A `Provider` represents one backend *connection*** — one vCenter (or one
libvirt host), one set of credentials, one `banlieue-provider-vsphere` (or
`-libvirt`) Deployment watching it ([ADR-0003](https://github.com/firestoned/banlieue/blob/main/docs/adr/0003-provider-deployment-topology.md):
per-instance topology, one Deployment per `Provider` so a hung backend
never stalls another). It is **not** a tenancy or environment boundary.

- **Same vCenter, same credentials, only the declared classes/features
  differ → one `Provider`.** Add more `storageClasses[]`/`networkClasses[]`
  entries; don't create a second `Provider`.
- **Genuinely separate credentials, a separate vCenter, or a deliberate
  RBAC/blast-radius boundary between environments → separate `Provider`s.**
  That's an access-control decision, not a networking one.

Splitting a `Provider` for the *first* case spins up a second
`banlieue-provider-vsphere` pod that logs into the **identical** vCenter
endpoint with the **identical** credentials and walks the **identical**
inventory — pure duplicated work and vCenter API load, with zero isolation
benefit, since nothing was actually separated.

## The motivating case: dev and qa share a vCenter, differ only by network

Say one vCenter (`vcenter1`) has three clusters, each with a dev network
and a separate qa network (a common naming convention: `-dev-` vs. `-qa-`
in the port group name):

| Cluster | Dev port group | QA port group | Datastore cluster (shared) |
| --- | --- | --- | --- |
| cluster-01 | `dvs-dev-vlan101` | `dvs-qa-vlan201` | `dsc-cluster-01` |
| cluster-02 | `dvs-dev-vlan102` | `dvs-qa-vlan202` | `dsc-cluster-02` |
| cluster-03 | `dvs-dev-vlan103` | `dvs-qa-vlan203` | `dsc-cluster-03` |

Storage is identical between dev and qa — the same datastore clusters serve
both. Only the network differs, and it differs **per cluster**, which is
exactly what [per-zone capability targets](https://github.com/firestoned/banlieue/blob/main/docs/adr/0030-per-zone-capability-targets.md)
(ADR-0030) exists for.

### Why this is one dimension too many for `perZone` alone

`ScopedTarget` in a `storageClasses[]`/`networkClasses[]` mapping is keyed
by `(datacenter, cluster)` — one target per zone, per class name. A dev VM
and a qa VM scheduled onto the *same* `cluster-01` share that exact
`(datacenter, cluster)` key, so a single class name like `network-01`
cannot resolve to two different port groups depending on which environment
the VM belongs to — that's not a per-zone difference, it's a per-*class*
difference. The fix is a **second class name**, not a second `Provider`
and not more `perZone` entries on the first name.

### The shape

One `Provider`, one storage-class set (shared), two network-class sets (one
per environment), each already spread across every cluster via `perZone`:

```yaml title="provider.yaml (excerpt)"
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata:
  name: vcenter1
  namespace: banlieue-system
  labels:
    dc: dc1
spec:
  # ... connection, failureDomainNameOverrides unchanged ...
  capabilities:
    storageClasses:
      # Shared between every environment — one class, per-cluster targets,
      # no environment split needed at all.
      - name: gold
        perZone:
          - datacenter: dc1
            cluster: cluster-01
            target: { datastoreCluster: dsc-cluster-01 }
          - datacenter: dc1
            cluster: cluster-02
            target: { datastoreCluster: dsc-cluster-02 }
          - datacenter: dc1
            cluster: cluster-03
            target: { datastoreCluster: dsc-cluster-03 }
    networkClasses:
      # Dev traffic.
      - name: prod
        perZone:
          - datacenter: dc1
            cluster: cluster-01
            target: { distributedPortGroup: dvs-dev-vlan101 }
          - datacenter: dc1
            cluster: cluster-02
            target: { distributedPortGroup: dvs-dev-vlan102 }
          - datacenter: dc1
            cluster: cluster-03
            target: { distributedPortGroup: dvs-dev-vlan103 }
      # QA traffic — a distinct class name, same perZone shape, same clusters.
      - name: prod-qa
        perZone:
          - datacenter: dc1
            cluster: cluster-01
            target: { distributedPortGroup: dvs-qa-vlan201 }
          - datacenter: dc1
            cluster: cluster-02
            target: { distributedPortGroup: dvs-qa-vlan202 }
          - datacenter: dc1
            cluster: cluster-03
            target: { distributedPortGroup: dvs-qa-vlan203 }
```

No `target:` default is needed on any of these — every cluster is covered
by an explicit `perZone` entry, since each cluster genuinely has a
differently-named port group/datastore cluster and there is no
"same-everywhere" fallback case here.

### Static addressing across the same clusters (no DHCP)

If DHCP isn't an option, ADR-0030's `perZone` alone isn't quite enough —
it resolves *which port group* a NIC uses, but a static `VirtualMachine`
still needs a gateway, DNS servers, and a domain for whichever subnet that
port group belongs to, and those genuinely differ per cluster the same way
the port group itself does. [ADR-0032](https://github.com/firestoned/banlieue/blob/main/docs/adr/0032-per-zone-network-subnet-shape.md)
extends the same `networkClasses[]` entry with that subnet shape, keyed the
same way:

```yaml title="provider.yaml (excerpt, continued)"
    networkClasses:
      - name: prod
        perZone:
          - datacenter: dc1
            cluster: cluster-01
            target: { distributedPortGroup: dvs-dev-vlan101 }
          # ... cluster-02 / cluster-03 targets as above ...
        perZoneSubnet:
          - datacenter: dc1
            cluster: cluster-01
            subnet:
              gateway: 192.0.2.1
              nameservers: [198.51.100.53, 198.51.100.54]
              domain: k8s.example.internal
          - datacenter: dc1
            cluster: cluster-02
            subnet:
              gateway: 203.0.113.1
              nameservers: [198.51.100.53, 198.51.100.54]
              domain: k8s.example.internal
          # ... cluster-03 ...
```

With this declared, a `VirtualMachine` targeting `cluster-01` only needs to
supply the address — the gateway, DNS, and domain shown above resolve
automatically from the same zone the port group came from:

```yaml title="virtualmachine.yaml (excerpt)"
spec:
  classRef: { name: small }
  networkOverrides:
    - name: eth0
      static:
        address: 192.0.2.104
        prefix: 24
        # no gateway / nameservers / domain — resolved from cluster-01's
        # own perZoneSubnet entry above
  placement:
    failureDomainSelector:
      matchLabels:
        name: cluster-01
```

Any of `gateway`/`nameservers`/`domain` the VM *does* set explicitly still
wins for that field — this only fills in what's left unset, never
overrides an intentional per-VM value.

### Two `VMClass`es, one `Provider`

```yaml title="vmclass-small.yaml (dev)"
apiVersion: banlieue.io/v1alpha1
kind: VMClass
metadata:
  name: small
spec:
  hardware:
    cpus: 2
    memoryMiB: 4096
    disks:
      - { name: root, sizeGiB: 20, storageClass: gold }
  network:
    interfaces:
      - { name: eth0, networkClass: prod, ipam: { source: dhcp } }
```

```yaml title="vmclass-small-qa.yaml (qa)"
apiVersion: banlieue.io/v1alpha1
kind: VMClass
metadata:
  name: small-qa
spec:
  hardware:
    cpus: 2
    memoryMiB: 4096
    disks:
      - { name: root, sizeGiB: 20, storageClass: gold }   # same storage
  network:
    interfaces:
      - { name: eth0, networkClass: prod-qa, ipam: { source: dhcp } }   # different network
```

Both classes schedule onto any of `cluster-01`/`02`/`03` — `small` lands on
the dev network wherever it's placed, `small-qa` on the qa network,
regardless of which cluster the scheduler picks. Neither `VMClass`
hardcodes a cluster, and neither needs a second `Provider`.

## When a separate `Provider` *is* the right call

- **A genuinely different vCenter** (different endpoint) — obviously a
  different `Provider`; there's no connection to share.
- **Deliberate credential/RBAC isolation** — e.g. qa and prod must never be
  reachable with the same vCenter service account, even though they happen
  to live in the same vCenter today. This is a real, defensible reason; it
  just isn't a *networking* reason, and it should be a conscious choice
  (documented in an ADR if it's architecturally significant, per this
  project's [ADD methodology](https://github.com/firestoned/banlieue/blob/main/rules/architecture-driven-development.md)),
  not a default reached for out of habit.
- **A `Provider.metadata.labels` convention like `env: dev`/`env: qa`**
  (used by `VirtualMachine.spec.placement.providerSelector` to pick a
  `Provider`) is fine *as a label* on a single `Provider` representing
  multiple environments' worth of classes — but if you find yourself
  actually creating two separate `Provider` objects pointed at the *same*
  endpoint with the *same* credentials just to carry different `env`
  labels, that's the anti-pattern this guide is about — the label alone
  doesn't justify the split; check whether the credentials and endpoint
  are actually different before reaching for a second `Provider`.

## See also

- [vSphere Provider](vsphere-provider.md) — the base install/registration guide.
- [Provider Lifecycle & Install](provider-lifecycle.md) — how a `Provider`
  becomes a running workload.
- [ADR-0003](https://github.com/firestoned/banlieue/blob/main/docs/adr/0003-provider-deployment-topology.md) —
  why Providers are per-instance, not per-class.
- [ADR-0030](https://github.com/firestoned/banlieue/blob/main/docs/adr/0030-per-zone-capability-targets.md) —
  the `perZone` mechanism this guide builds on.
