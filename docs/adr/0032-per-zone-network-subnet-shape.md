# 0032 — Per-zone subnet shape for static network classes

## Status

Accepted — 2026-08-23. Extends [ADR-0030](0030-per-zone-capability-targets.md)
(per-zone capability targets) to cover a gap that ADR only partially
closed for **static** IPAM.

## Context

Some environments cannot use DHCP at all — every `VirtualMachine` must be
statically addressed. Combined with [ADR-0030](0030-per-zone-capability-targets.md)
(one `networkClass` name resolving to a different port group per cluster
of a `Provider`), this exposes a gap ADR-0030 didn't cover: **the subnet
facts that go with that port group — gateway, DNS, domain — have nowhere
correct to live.**

`VMClass.network.interfaces[].ipam.static` ([`IpamShape`]'s
`static_: Option<StaticNetworkShape>`) looks like the intended place, but
tracing `merge_ipam_override` (`crates/banlieue-controller/src/reconciler/infra.rs:239-250`)
shows it is **never actually read**:

```rust
fn merge_ipam_override(class_ipam: &IpamShape, override_: Option<&StaticIpamConfig>) -> IpamSpec {
    match override_ {
        Some(static_cfg) => IpamSpec { static_: Some(static_cfg.clone()), pool: None },
        None            => IpamSpec { static_: None, pool: class_ipam.pool.clone() },
    }
}
```

Neither branch reads `class_ipam.static_`. Static addressing requires a
per-VM `address`, which a `VMClass` can never declare (it's shared by many
VMs) — so a per-VM `networkOverrides[].static` (`StaticIpamConfig`) always
exists whenever static addressing is used, and per the `Some` branch's
documented behavior ("An override always wins outright, replacing the
class's IPAM entirely"), it **discards** the class's gateway/nameservers/
domain rather than filling gaps. Found live twice: a `VirtualMachine`'s
`guestinfo.network.gateway` came back empty because its `networkOverrides`
supplied only `address`/`prefix`, and the class's own declared gateway was
silently dropped.

Even if that merge were fixed to fill gaps from the class, it would not
solve the actual problem: `IpamShape.static_` is **one shape for the whole
class**, and a class is exactly the thing ADR-0030 made portable across
multiple clusters with genuinely different subnets. A class-level gateway
can be correct for at most one cluster.

## Decision

**Subnet facts move to the same place ADR-0030 already put the port group
they describe: the `Provider`'s per-zone network-class target — not the
`VMClass`.** A port group implies a subnet; both are backend topology, not
a VM-class fact.

```rust
/// Gateway/DNS/domain for a subnet — deliberately NOT prefix, which stays
/// a per-VM field (an address's prefix is closer to per-VM addressing
/// detail than backend topology, and every StaticIpamConfig already
/// requires it explicitly).
pub struct SubnetShape {
    pub gateway: Option<String>,
    pub nameservers: Vec<String>,
    pub domain: Option<String>,
}

pub struct NetworkClassMapping {
    pub name: String,
    pub target: Option<BTreeMap<String, String>>,      // ADR-0030
    pub per_zone: Vec<ScopedTarget>,                     // ADR-0030
    pub subnet: Option<SubnetShape>,                     // NEW: default subnet
    pub per_zone_subnet: Vec<ScopedSubnet>,              // NEW: per-zone overrides
}

pub struct ScopedSubnet {
    pub datacenter: String,
    pub cluster: String,
    pub subnet: SubnetShape,
}

impl NetworkClassMapping {
    /// Same precedence as `target_for` (ADR-0030): an exact `per_zone_subnet`
    /// match wins, else the default `subnet`, else `None`.
    pub fn subnet_for(&self, datacenter: &str, cluster: &str) -> Option<&SubnetShape> { ... }
}
```

`ScopedSubnet` is deliberately its own type, not folded into ADR-0030's
`ScopedTarget` — `StorageClassMapping` has no subnet concept, and giving it
one just because the shape is superficially similar (keyed by
`(datacenter, cluster)`) would be a meaningless field on every disk class.

### The merge: per-VM override field wins, else the zone's subnet, else empty

`build_vsphere_machine` (`crates/banlieue-controller/src/reconciler/infra.rs`)
already resolves `datacenter`/`cluster` for the chosen failure domain and
already receives the `Provider` (currently `_provider`, unused). Once it
also resolves `nic.network_class`'s `NetworkClassMapping` and calls
`subnet_for(datacenter, cluster)`, `merge_ipam_override` fills in — **field
by field**, not wholesale — whichever of `gateway`/`nameservers`/`domain`
the per-VM override left unset:

```rust
fn merge_ipam_override(
    class_ipam: &IpamShape,
    override_: Option<&StaticIpamConfig>,
    zone_subnet: Option<&SubnetShape>,
) -> IpamSpec {
    match override_ {
        Some(o) => IpamSpec {
            static_: Some(StaticIpamConfig {
                address: o.address.clone(),
                prefix: o.prefix,                                    // always per-VM
                gateway: o.gateway.clone().or_else(|| zone_subnet.and_then(|s| s.gateway.clone())),
                nameservers: if o.nameservers.is_empty() {
                    zone_subnet.map(|s| s.nameservers.clone()).unwrap_or_default()
                } else {
                    o.nameservers.clone()
                },
                domain: o.domain.clone().or_else(|| zone_subnet.and_then(|s| s.domain.clone())),
            }),
            pool: None,
        },
        None => IpamSpec { static_: None, pool: class_ipam.pool.clone() },
    }
}
```

An explicit per-VM value always wins for that specific field (explicit
over implicit) — this only fills what the VM genuinely left unset. A VM
needing a non-default DNS server, say, still can set `nameservers` itself
without losing the zone's gateway.

**Result: a `VirtualMachine`'s static override can shrink to just
`address` and `prefix`.** Gateway, DNS, and domain resolve automatically
from whichever failure domain the scheduler picked — the same mechanism,
same precedence pattern, same `(datacenter, cluster)` key as ADR-0030's
port-group resolution, so one `VMClass` genuinely works across every
cluster of a `Provider` even under a hard static-addressing requirement.

### Not covered by this ADR

- **`prefix` stays per-VM**, per explicit decision — it's requested on
  every `StaticIpamConfig` regardless of this ADR.
- **`VMClass.network.interfaces[].ipam.static` (`StaticNetworkShape`)
  remains as-is, still unused.** Re-purposing it as a third fallback tier
  below the zone-resolved subnet was considered and rejected — it adds an
  ambiguous third precedence level for no case this ADR's mechanism
  doesn't already cover. It's a pre-existing dead field, not something
  this ADR fixes; flagged in Consequences, not resolved here.
- **libvirt / Proxmox** have no `(datacenter, cluster)` concept yet — same
  scoping note as ADR-0030. `subnet_for` still resolves correctly for them
  (falls through to the default `subnet`, if declared), it just never has
  a `per_zone_subnet` entry to match against.

## Consequences

- A `VirtualMachine` under a strictly-static-addressing requirement no
  longer needs to know a cluster's gateway/DNS/domain at all when its
  `VMClass` already spans multiple clusters — only the address (and
  prefix). This closes the gap found live twice (a missing gateway) at its
  actual root cause, not just the immediate symptom.
- `crates/banlieue-api` — a source-of-truth CRD schema change
  (`NetworkClassMapping` gains `subnet`/`per_zone_subnet`) — requires
  regenerating `deploy/crds/banlieue.io_providers.yaml`.
- `crates/banlieue-controller/src/reconciler/infra.rs`'s `_provider`
  parameter becomes genuinely used, not just contractually present for a
  future backend.
- `VMClass.network.interfaces[].ipam.static` remains dead code — a known,
  documented pre-existing gap, not a regression introduced here. A future
  ADR could remove it outright (no released consumers to break, per this
  project's unreleased-software posture) or repurpose it; neither is
  decided by this one.
