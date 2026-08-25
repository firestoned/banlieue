# 0031 — VMImage templates support multiple NICs

## Status

Accepted — 2026-08-23. Amends [ADR-0020](0020-vsphere-per-zone-iso-import.md)
(per-zone template import), specifically the `network`/`networkAdapter`/
`nicPciSlot` fields `VMImageTemplate` has carried since that ADR.

## Context

`VMImageTemplate` has only ever modeled one NIC: `network: Option<String>`,
`networkAdapter: Option<NicAdapter>`, `nicPciSlot: Option<i32>` are three
independent, singular fields, and `build_template_config_spec`
(`crates/banlieue-provider-vsphere/src/client/vim.rs`) builds exactly one
`VirtualEthernetCard` device. There is no way to declare a template with
more than one network interface. This project has no release and no
consumers yet, so this ADR changes the shape outright rather than adding a
parallel field and deprecating the old one.

## Decision

### 1. `VMImageTemplate.network: Vec<VMImageTemplateNic>`

Replaces the three singular fields with a list:

```rust
pub struct VMImageTemplateNic {
    /// Port group. `None` -> the zone's first reachable network class,
    /// same fallback the single-NIC field used.
    pub network: Option<String>,
    /// `None` -> vmxnet3 (unchanged default).
    pub adapter: Option<NicAdapter>,
    /// `None` -> 192 + this NIC's index in the list (see Decision #2).
    pub pci_slot: Option<i32>,
}
```

An empty list preserves today's exact default behavior (one NIC, zone-
derived network, vmxnet3, slot 192) — no existing example needs to change
just to keep the current single-NIC default.

### 2. Unset `pciSlot` auto-increments from 192 by list index

NIC index 0 defaults to 192, index 1 to 193, index 2 to 194, and so on —
giving predictable `ens192`/`ens193`/`ens194` naming for a multi-NIC
template without requiring the caller to hand-pick every slot. An
explicit `pciSlot` on any entry always wins over the derived default.
Considered requiring an explicit slot for every NIC beyond the first (no
auto-derivation) — rejected as needless friction for the common case
(every NIC just wants the next stable slot); nothing prevents overriding a
specific entry when one is needed.

### 3. CLI: repeatable `--nic key=value` flag, not parallel repeated flags

`banlieue provider vsphere image-import`'s `ImportArgs` replaces
`--network`/`--network-adapter`/`--nic-pci-slot` with a repeatable
`--nic "network=<name>,adapter=<type>,pciSlot=<n>"`, one occurrence per
NIC — mirroring the existing `parse_tolerations`/`parse_node_selector`
delimited-string pattern (`banlieue-provider-sdk/src/scheduling.rs`)
rather than three separately-repeated flags (`--network a --network b
--network-adapter x`), which is ambiguous the moment the repeat counts
don't line up.

### 4. Zone-network resolution moves from a single field to per-NIC

`resolve_zone`'s `ZonePlan` no longer carries a single `network` — each
NIC resolves its own port group independently (explicit override, else
the zone's first reachable network class), then each resolved name is
looked up against the zone's cluster for its moref/distributed flag,
exactly as the single-NIC path already did, just per-entry instead of once.

## Consequences

- Breaking change to `VMImageTemplate`, `ImportArgs`, `IsoImportRequest`,
  and `ZonePlan` — accepted per the no-release, no-consumers state.
- `build_template_config_spec` now loops over N ethernet devices instead of
  building exactly one; each gets its own `ethernetN.pciSlotNumber`
  `extraConfig` entry (the mechanism actually governing guest-visible PCI
  placement, found live and documented in this session's CHANGELOG —
  `VirtualDevice.slotInfo` is not it), numbered by that NIC's 0-based
  position among ethernet devices specifically, matching vSphere's own
  `ethernetN` naming.
- The post-create PCI-slot-pin reconfigure correlates a newly created NIC
  device back to its intended `VMImageTemplateNic` entry by **device-list
  order**, not by backing network identity — the same trust this file's
  existing single-NIC helpers (`find_disk_key`/`find_cdrom_key`) already
  place in "the Nth match is the right one." Documented explicitly on
  [`find_all_nic_keys`]'s doc comment as a deliberate simplification, not
  an oversight.

## Follow-ups

- **`clone_vm` remains single-NIC.** This ADR scopes multi-NIC support to
  the *template build* path (`import_iso_template` /
  `build_template_config_spec`) only — `CloneVmRequest` and `clone_vm`'s
  NIC-carry-forward logic (reading the template's first
  `ethernet0.pciSlotNumber` entry and reapplying it on the clone) are
  unchanged, still only handling the first NIC. `VSphereMachineSpec.network`
  is already `Vec<VSphereNicSpec>` in the schema, so cloning a multi-NIC
  template today would silently only wire up its first interface — a
  separate piece of work, not addressed here.
