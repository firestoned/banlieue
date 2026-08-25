# 0024 — VSphereMachine clone reconciler: static IP + templated cloud-config via guestinfo

## Status

Proposed — 2026-08-20. Depends on [ADR-0020](0020-vsphere-per-zone-iso-import.md)
(per-zone template import) and [ADR-0021](0021-vsphere-template-install-and-generalize.md)
(install-once, generalized templates). Extends `banlieue.io/v1alpha1`
`VirtualMachine` (`crates/banlieue-api/src/banlieue/virtualmachine.rs`) and
`VMClass` (`crates/banlieue-api/src/banlieue/vmclass.rs`).

## Context

`crates/banlieue-provider-vsphere/src/reconciler/mod.rs` currently ships only
`provider` (capability introspection, ADR-0019) and `vmimage` (per-zone
template build, ADR-0020/0021). The `VSphereMachine` reconciler — the piece
that clones a per-zone template into an actual running VM — has never been
implemented ("lands in iteration 2", per that module's own doc comment). A
`VirtualMachine` can already be scheduled (`banlieue-controller`'s scheduler
resolves a Provider + failure domain and would create a `VSphereMachine`
infra CR per the CAPI InfraMachine contract), but nothing on the vSphere
provider side consumes that CR yet.

Building this reconciler surfaces two requirements the existing schema
doesn't cover, confirmed by inspecting `extraConfig` on an existing,
hand-provisioned VM in the same vCenter (one of this project's own k0s
management-cluster nodes, `bar01.k8s.example.internal`):

```
guestinfo.network.ip        10.0.0.90
guestinfo.network.prefix    24
guestinfo.network.gateway   10.0.0.1
guestinfo.network.dns       10.0.1.53,10.0.1.54
guestinfo.network.domain    k8s.example.internal
guestinfo.userdata          <base64 Kairos cloud-config>
guestinfo.userdata.encoding base64
```

The decoded cloud-config sets `hostname:`/`fqdn:` at the top level and, in an
`initramfs` stage, writes a static `systemd-networkd` unit plus
`/etc/hostname` and `/etc/hosts` using the *same* address/gateway/DNS values
— i.e. the guestinfo network keys and the cloud-config's own static network
setup are always set consistently, not one-or-the-other. This is the
established, working convention this environment already uses for every
hand-provisioned VM; the new reconciler should reproduce it exactly rather
than invent a different mechanism.

Two schema gaps block reproducing this declaratively:

1. **No per-VM static address.** `VMClassSpec.network.interfaces[].ipam`
   already models `IpamSource::Static` (`StaticIpamConfig { address, prefix,
   gateway, nameservers }`), but that field lives on the **class**, which is
   shared across many VMs by design (`db-prod-large` is meant to be reused).
   A literal static address baked into a shared class can only ever serve
   one VM correctly — every other VM of that class would collide on the same
   address. Every one of the six `banlieueNN` nodes needs the *same* class
   (shape) but a *different* address.
2. **No hostname/FQDN templating.** `VirtualMachineSpec.userData` already
   points at a Secret holding a raw cloud-config blob (`secretRef` + `key`),
   but delivers it byte-for-byte — there's no way to say "this cloud-config,
   but with the VM's own name and FQDN filled in," which is exactly what the
   reference VM's `hostname:`/`fqdn:` lines need per-VM.

## Decision

### 1. Per-VM static address override on `VirtualMachine`, not `VMClass`

Add to `VirtualMachineSpec`:

```rust
/// Per-VM overrides for specific VMClass-declared network interfaces.
/// Keyed by `NetworkInterfaceSpec.name`. Absent entries use the VMClass's
/// own `ipam` verbatim (commonly `dhcp`). Lets many VMs share one VMClass
/// while each still gets its own address — a VMClass-level `ipam.static`
/// cannot express that, since a class is shared by design.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub network_overrides: Vec<NetworkInterfaceOverride>,
```

```rust
pub struct NetworkInterfaceOverride {
    /// Matches a `VMClass.spec.network.interfaces[].name`.
    pub name: String,
    pub static_: StaticAddressConfig,
}
```

`StaticAddressConfig` is `common::StaticIpamConfig` (`address`, `prefix`,
`gateway`, `nameservers`) plus one new field, `domain: Option<String>` —
needed for both `guestinfo.network.domain` and FQDN construction, and
currently modeled nowhere.

A CAPI-style `Pool` IPAM (already modeled via `PoolIpamConfig` /
`InClusterIPPool`) remains the answer for *dynamically allocated* addresses
and is untouched by this ADR. This decision is specifically for the
*statically assigned, admin-knows-the-address-in-advance* case this
environment's existing VMs all use — adding a full CAPI IPAM provider
dependency to reproduce a fixed `.90` address is disproportionate.

### 2. A fixed, minimal placeholder set — not a templating engine

`VirtualMachineSpec.userData`'s Secret content is substituted against a
**fixed, explicit set of placeholders** before delivery — deliberately not a
general templating language (Tera/Handlebars/etc.), matching the "explicit
over implicit" non-negotiable: the set of substitutions is exhaustive and
auditable from one doc comment, not Turing-complete.

| Placeholder | Source |
| --- | --- |
| `${VM_NAME}` | `VirtualMachine.metadata.name` |
| `${FQDN}` | `${VM_NAME}.<domain>`, where `<domain>` is the first resolved `NetworkInterfaceOverride.static_.domain` (empty string if none — see below) |
| `${IP}` | resolved static `address`, or empty for a `dhcp` interface |
| `${PREFIX}` | resolved static `prefix`, or empty |
| `${GATEWAY}` | resolved static `gateway`, or empty |
| `${DNS}` | resolved static `nameservers`, comma-joined, or empty |
| `${DOMAIN}` | resolved static `domain`, or empty |

An interface with no override (plain `dhcp`) leaves the `${IP}`/`${PREFIX}`/
`${GATEWAY}`/`${DNS}`/`${DOMAIN}` placeholders substituted with the empty
string rather than erroring — a cloud-config that doesn't reference them is
unaffected either way. `${FQDN}` with no domain resolves to `${VM_NAME}.`
(trailing dot, valid FQDN syntax) rather than erroring, so a VM with no
static override still gets a well-formed value.

Substitution is literal `${NAME}` string replacement (not regex, not
shell-eval) implemented once in `banlieue-provider-sdk` (a new
`guestdata::render_placeholders` helper) so it is backend-agnostic — libvirt
and Proxmox delivering cloud-init their own way (NoCloud ISO / Proxmox
cloud-init drive) reuse the same substitution, only the *delivery* mechanism
below is vSphere-specific.

### 3. vSphere delivery: `extraConfig` guestinfo keys, set at clone time

The new `VSphereMachine` reconciler (`banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`):

1. Resolves the per-zone template from the scheduled `VMImage`'s
   `status.perProvider[].zones[].resolvedRef` (already populated by the
   `vmimage` reconciler, ADR-0020).
2. `CloneVM_Task` from that template into the target folder/resource
   pool/datastore, with `VirtualMachineCloneSpec.config.extraConfig` set in
   the *same* clone call (not a follow-up `Reconfigure`) to:
   - `guestinfo.network.{ip,prefix,gateway,dns,domain}` for every interface
     with a resolved static override; omitted entirely for a `dhcp`
     interface (open-vm-tools/Kairos's VMware datasource fall through to
     DHCP when these keys are absent — matches today's manual convention,
     no new "mode" flag needed).
   - `guestinfo.userdata` = base64 of the placeholder-substituted
     cloud-config from `spec.userData`; `guestinfo.userdata.encoding =
     base64`. Omitted when `userData` is unset.
3. Powers the clone on/off per `spec.desiredPowerState`.

Mirroring status (`VirtualMachine.status.addresses`, power state,
conditions) and update/migration semantics are real `VSphereMachine`
reconciler concerns but are **out of scope for this ADR** — this ADR covers
create-time delivery only. A follow-up ADR (or an amendment here) covers the
rest of the reconciler's lifecycle once this lands.

## Consequences

- `VirtualMachine`/`VMClass` gain `network_overrides` / `StaticAddressConfig`
  — additive, `#[serde(default)]`, no change to any existing spec's meaning.
- The placeholder set is fixed by this ADR; adding a new one later is a
  schema-visible, ADR-worthy change (consistent with "explicit over
  implicit"), not a silent template-engine feature creep.
- `banlieue-provider-sdk` gains a `guestdata` module reusable by libvirt and
  Proxmox once they implement their own guest-data delivery — this ADR only
  wires the vSphere side, but the substitution logic isn't vSphere-specific.
- Two DIFFERENT VMs of the same `VMClass` can now have genuinely different
  static addresses (the actual `banlieueNN` fleet shape) without needing a
  CAPI IPAM pool provider installed.
- `import_job_name`/`k8s_name::collision_safe_name` (ADR-0020/0023) are
  unaffected — this ADR is entirely about the create-time clone path, not
  the per-zone template build.

## Follow-ups

- `VSphereMachine` reconciler's remaining lifecycle: status mirroring,
  power-state reconciliation, update semantics, deletion — separate
  implementation work under this same reconciler, likely its own ADR
  amendment once the create path (this ADR) is proven.
- `banlieue-provider-sdk::guestdata` adoption by libvirt/Proxmox providers —
  not scheduled, noted so the module isn't accidentally scoped
  vsphere-only in its own doc comments.
