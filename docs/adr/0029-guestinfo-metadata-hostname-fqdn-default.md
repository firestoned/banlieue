# 0029 — Default hostname/FQDN via `guestinfo.metadata`, not `userData`

## Status

Accepted — 2026-08-22. Extends [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)'s
`build_guestinfo` (already sets `guestinfo.network.hostname` unconditionally
on every clone).

## Context

`build_guestinfo` (`crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`)
already sets `guestinfo.network.hostname = <VirtualMachine name>`
unconditionally on every `CloneVM_Task`, and `guestinfo.network.domain` when
a static network override supplies one. That covers guests running the
hand-rolled `configure-network.sh` convention (documented in
[Building a Kairos Hadron VM Template](../src/guides/building-kairos-hadron-template.md)),
which reads those flat `guestinfo.network.*` keys directly via `vmtoolsd`.

It does **not** cover a guest whose `VirtualMachine.spec.guestAgent` is
`cloud-init` proper (the [Alpine template guide](../src/guides/alpine-vsphere-template.md)'s
target) — real cloud-init's VMware GuestInfo datasource
(`DataSourceVMware`) does not read `guestinfo.network.hostname` at all. It
reads a distinct key, `guestinfo.metadata` — a base64 (optionally gzipped)
YAML document with its own schema (`instance-id`, `local-hostname`,
`network`, …) — which banlieue has never set. Without it, a plain
cloud-init guest boots with no hostname set unless the user's own
`spec.userData` cloud-config happens to include a `hostname:`/`fqdn:`
directive (`cc_set_hostname` module) — something most users have no reason
to think to add, since `VirtualMachine.spec` gives no indication a name
isn't already propagated to the guest.

The ask (this session): make hostname/FQDN a sane default for every VM,
metadata-only — not by templating something into the user's own
`userData`.

## Decision

**`build_guestinfo` also sets `guestinfo.metadata`, unconditionally, on
every clone — independent of whether `spec.userData` is set at all.**

1. **No new CRD field.** FQDN is synthesized exactly as
   [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)'s
   existing `${FQDN}` placeholder does it conceptually — `<vm-name>.<domain>`
   — but **only when a domain is already resolvable** from the first NIC
   with a static override (the same source `guestinfo.network.domain`
   already uses). A plain-`dhcp` VM gets a hostname, not a synthesized
   `name.` with a trailing dot — inventing a domain, or leaving one
   dangling, is worse than not having one.

2. **`guestinfo.metadata` content** (base64-encoded YAML, matching the real
   `DataSourceVMware` schema — not banlieue's own flat convention):
   ```yaml
   instance-id: <vm-name>
   local-hostname: <fqdn-if-domain-known-else-vm-name>
   ```
   `instance-id` is the VM's own Kubernetes name — already guaranteed unique,
   so no new identifier needs generating. `local-hostname` is what
   cloud-init's `cc_set_hostname` module actually reads: **a single field
   serves both hostname and FQDN**, because that module treats a
   dotted `local-hostname` as an FQDN and derives the short hostname from
   the part before the first dot — no separate `fqdn` metadata key exists
   in cloud-init's own schema to set.

3. **`spec.userData` is never parsed, merged into, or otherwise touched.**
   If the user's own cloud-config already sets `hostname:`/`fqdn:`, that
   module-level directive still wins over datasource metadata in cloud-init's
   own precedence — this ADR's default only fills the gap when they don't,
   without banlieue ever having to understand or risk corrupting
   user-authored YAML. This satisfies "sane default, no action needed" and
   "don't touch what the user supplied" simultaneously — because they are
   two different cloud-init inputs (datasource metadata vs. userdata
   module config), not one field two code paths would fight over.

4. **Guests using the hand-rolled `configure-network.sh` convention are
   unaffected but incidentally gain a real fallback.** That script already
   reads `guestinfo.metadata`'s `instance-id`/`domain` fields as a
   *fallback* when `guestinfo.network.hostname`/`.domain` are absent
   (see the guide above) — this ADR is the first time banlieue actually
   populates that fallback source, for free, as a side effect of doing the
   correct thing for real cloud-init.

### Not covered by this ADR

- **`network:` in `guestinfo.metadata`.** `DataSourceVMware` also accepts a
  full network-config document there; banlieue already delivers static
  network config via the separate `guestinfo.network.*` keys (ADR-0024),
  and cloud-init's own guestinfo datasource falls back to DHCP when
  `metadata.network` is absent — no gap to close here.
- **Proxmox / libvirt.** Neither backend has a `guestinfo` mechanism;
  this is vSphere-only, same scope as ADR-0024.

## Consequences

- Every VM cloned by this provider gets a working hostname (and FQDN, when
  a domain is known) on first boot with **zero required action from the
  user** and **zero risk to hand-authored `userData`** — the two properties
  this session's request asked for together.
- `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`'s
  `build_guestinfo` gains one more unconditional key; no CRD, no RBAC, no
  new `VirtualMachine.spec` field.
- `docs/architecture/calm/architecture.json`'s vSphere backend relationship
  description gains a short note on `guestinfo.metadata`, since it already
  documents the `guestinfo.network.*` / `guestinfo.userdata` convention in
  detail.
