<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0104: vSphere `Template`-kind VMImages resolve per zone, like `Url`

- **Status:** Accepted
- **Date:** 2026-10-21
- **Proposed:** 2026-10-19
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0020](0020-vsphere-per-zone-iso-import.md)
  (per-zone template folders, Decision #5) to sources banlieue did not build;
  amends [ADR-0028](0028-vmimage-template-deletion-lifecycle.md) (deletion
  destroys per-zone templates) with an ownership rule; touches the scheduler filter order
  of [ADR-0098](0098-spread-placement-across-failure-domains.md) only for
  `Template` sources

## Context

A vsphere `VMImage` source is one of two kinds. `Url` imports an ISO into
every failure domain and builds a template there. `Template` adopts a
template that **already exists** in vCenter; banlieue builds nothing.

`Url` and `Template` publish different status shapes, and the `Template`
shape does not work:

1. **The controller hands the provider a display string.** For `Template`
   sources the provider writes `status.perProvider[].resolvedRef` as the
   human-oriented `[<datacenter(s)>] <name>`
   (`render_resolved_ref`, `reconciler/vmimage.rs`). The controller copies
   that string verbatim into `VSphereMachine.spec.template`
   (`resolve_template_ref`, `reconciler/infra.rs`), and the provider then
   looks the template up by exact name equality (`find_template`,
   `client/vim.rs`). `[dc] name` never equals `name`, so the machine fails with
   "template not found" and `InfrastructureReady=False / ProvisionFailed`
   even though the template exists. A clone from a `Template`-kind image has
   never been able to succeed.
2. **The lookup is datacenter-wide and first-match.** `find_template` is
   called with `folder: None`, which roots the view at the whole datacenter.
   When the same template name exists in several per-zone folders, which copy
   is cloned is arbitrary, and per-zone placement is not respected.
3. **No per-zone status.** `zones[]` is empty, so neither the operator nor the
   scheduler can see which zone has the template.
4. **A latent deletion hazard.** `finalize` destroys every template listed in
   `perProvider[].zones[]` unless `spec.template.retainOnDelete` is true. A
   `Template`-kind image is safe today only because its `zones` is empty.
   Giving it per-zone status without a guard would make deleting a `VMImage`
   destroy the operator's own pre-existing vCenter templates.

Verified against `origin/main` (`deeaf84`) and two live clusters (read-only):
a `Url`-kind image publishes three `zones[]` with a bare `resolvedRef` and a
`templateFolder` of `<rootFolder>/<failure-domain>`; a `Template`-kind image
publishes one decorated top-level `resolvedRef` and no zones.

## Decision

A `Template`-kind vsphere source behaves like `Url` for lookup, status and
placement, and banlieue never destroys a template it did not import.

1. **Always per zone, same folder rule as `Url`.** For each
   `Provider.status.failureDomains[]` entry the provider looks the template up
   under `effective_folder(spec.template.rootFolder, <failure-domain-name>)`
   (`import.rs`), i.e. `<rootFolder>/<failure-domain>`, or just
   `<failure-domain>` under the datacenter VM-folder root when `rootFolder` is
   unset. No opt-in flag: parity with `Url`, whose root folder is optional with
   the same default.
2. **Per-zone status.** `perProvider[].zones[]` gets one entry per failure
   domain: `name`, `ready`, `reason`, a **bare** `resolvedRef` (the source's
   `ref`) and `templateFolder`. A zone whose template is absent reports
   `TemplateNotFound`; a vCenter error reports `LookupFailed`. A provider with
   no `status.failureDomains[]` reports `NoFailureDomains`, as `Url` does. The
   top-level `resolvedRef` is no longer set for `Template` (as for `Url`).
3. **Aggregate `ready` is "any zone ready"** for `Template`, so one zone
   missing its template does not take the image away from the others.
   (`Url` keeps "all zones ready"; it is unchanged.)
4. **The scheduler gates per zone, for `Template` sources only.** When the
   image's source for the candidate provider's class is `Template` **and** the
   provider row has `zones[]`, a failure domain is a candidate only if its zone
   entry is ready. Rows with no `zones[]` (libvirt, Proxmox, an empty
   `Template` row) keep the existing provider-level check. `Url` scheduling is
   untouched. This is a deliberate asymmetry, chosen over making `Url` per-zone
   too because the requirement was that `Url` behaviour must not change; the
   cost is that the two kinds gate differently, which this ADR records.
5. **The controller only ever writes a bare name into
   `VSphereMachine.spec.template`.** The zone entry is preferred; a legacy
   top-level `resolvedRef` of the form `[<dc>] <name>` (a row written by an
   older provider, during a rolling upgrade) has the display prefix removed.
   The shared `resolve_template_ref` is not changed for the libvirt, Proxmox
   and cloud-hypervisor builders, whose top-level refs are already bare.
6. **Never destroy an adopted template.** Deletion destroys templates only for
   images whose vsphere source is `Url` (templates banlieue imported itself).
   For a `Template` source `finalize` skips the vCenter work entirely, with or
   without `zones[]`, and `retainOnDelete` is irrelevant. The check is on the
   source kind, not on whether status happens to be empty, so it cannot be
   bypassed by status contents.
7. **`rootFolder` is reused; no new field.** `Template` reads
   `spec.template.rootFolder` and ignores every other `spec.template` field.
   Those fields (`cpus`, `memoryMib`, `disk`, `network`, `firmware`, `guestId`,
   `installMode`, `installTimeoutSeconds`, `forceUpload`, `forceCreate`,
   `retainOnDelete`) describe how banlieue *builds* a template, which a
   `Template` source never does; the API documentation says so. They are not
   rejected: a CRD validation rule would turn existing manifests that merely
   carry the block into hard errors for no safety gain, since item 6 and the
   audit below already make them inert.

### Rejected alternative: a new field on the source

`ImageSource.templateFolder` (or similar) would keep `spec.template` "Url-only"
and make intent explicit. Rejected: the semantics are identical to
`rootFolder` (a root, scoped per zone), so a second field would give one folder
two spellings depending on source kind, and every `Url` image would still need
`spec.template.rootFolder`. Reusing the field keeps one rule to document.

### Audit: every path that could modify or delete a template

| Path | `Template` source |
| --- | --- |
| `finalize` → `destroy_zone_templates` | Skipped by Decision 6 |
| `image-import --force-create` → `destroy_if_present` | Only reachable through the import Job, which `reconcile_for_provider` returns before creating for `Template` sources |
| `image-import --force-upload` (datastore ISO delete) | Same: the Job is never created |
| `import_iso_template` / `MarkAsTemplate` | Same |
| `spec.template.retainOnDelete` | Ignored; Decision 6 does not depend on it |
| libvirt / Proxmox providers | `Template` is a verify-only check (`reconciler/vmimage.rs`, `reconciler/image.rs`); neither destroys images |
| `VSphereMachine` clone path | Read-only on the template (clone source) |

## Consequences

- **Behaviour change for existing `Template` images.** A template that does not
  sit under `<rootFolder>/<failure-domain>` becomes not-ready in that zone.
  Because of problem 1 no clone from a `Template`-kind image could ever have
  succeeded, so no working deployment depends on the old datacenter-wide
  lookup. A repo search found the old model only in unit-test fixtures,
  `examples/04-vmimage-ubuntu.yaml`, and the `Template` sections of the vSphere
  provider guide and the Alpine template guide; all are updated here. Operators
  must move or copy adopted templates into the per-zone folders (or accept
  `TemplateNotFound`).
- `Template` and `Url` images differ in scheduler gating (Decision 4). A later
  ADR may unify them under per-zone gating if `Url`'s all-or-nothing aggregate
  proves too strict.
- Binaries affected: the controller (bare name, scheduler gate) and the vsphere
  provider (lookup, status, finalize guard). No change to `banlieue-imagebuilder`
  or the import Job. API types are unchanged (doc comments only), so CRDs need
  re-applying only to pick up the corrected descriptions.
- The vSphere fake client now scopes folder lookups by datacenter, as vCenter
  does (folders are per-datacenter), so a test can no longer pass by finding a
  template in the wrong datacenter.
