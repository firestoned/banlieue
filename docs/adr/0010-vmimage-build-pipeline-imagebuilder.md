<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0010 — VMImage build pipeline: `banlieue-imagebuilder` + kairos-operator

- **Status:** Accepted
- **Date:** 2026-07-28
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0002 (InfraCluster failure-domain aggregation); ADR-0004
  (single-binary subcommand dispatch); `VMImage` / `ImageSource`
  (`crates/banlieue-api/src/banlieue/vmimage.rs`); `banlieue-provider-vsphere`
  `vmimage.rs` reconciler; `banlieue-provider-sdk` (`bootstrap`, `ssa`).

## Context

`VMImage.spec.sources[]` already has an `ImageSourceKind::Url` variant and an
`import_from` field, with a comment marking URL-import as deferred to a later
iteration ("Iter 2a does not support `Url`-import or `BackingFile`"). Today
`banlieue-provider-vsphere`'s `vmimage.rs` only handles `Template` sources: it
polls vCenter for a pre-existing template and reports readiness. There is no
code anywhere that turns an OCI-referenced OS image into a bootable template.

Requirement (from the original design discussion this project is built on):
nightly-built Kairos OCI images need to be tested and, on success, distributed
as ready-to-clone templates into every vSphere availability zone (zone ==
compute cluster in the current dev/qa environment — storage and DVS networking
are confirmed **not** shared across clusters, so each zone is a genuine,
separate import, not just a placement choice). The same *concept* — take an
OCI/Kairos image, produce a raw disk, hand it to a backend for import — must
be reusable for Proxmox and libvirt later without rewriting it per backend.

### What the OS/disk build step actually needs

Verified against the real `kairos-operator` docs (not guessed — the project's
own `documentation.md` rule and `CLAUDE.md`'s "always review official docs"
rule both apply to third-party CRDs we integrate against, not just our own):

- CRD: `build.kairos.io/v1alpha2`, kind `OSArtifact`. Two-stage spec: `spec.image`
  (a pre-built OCI ref, or build options to produce one) and `spec.artifacts`
  (which outputs to produce — `cloudImage: true` produces a raw disk named
  `<name>.raw`).
- `status.phase` is itself a state machine: `Pending → Building → Exporting →
  Ready` (or `Error`), with `status.message` on failure. We don't need to
  invent a phase model — we mirror kairos-operator's.
- Output lands in a PVC the operator creates and names `<osartifact-name>-artifacts`
  in the OSArtifact's own namespace, unless `spec.artifacts.volume` points at a
  custom volume.
- `spec.exporters` lets the OSArtifact itself run a post-build Job with the
  artifacts PVC mounted read-only at `/artifacts` — useful for backend-specific
  conversion later, but Stage 2 of this ADR does not use it (see Decision).
- Install is `kubectl apply -k https://github.com/kairos-io/kairos-operator/config/default`
  (kustomize, not Helm — no Helm chart is documented). No cert-manager
  prerequisite is documented. This corrects an assumption in an earlier,
  disconnected prototype of this same idea, which guessed at Helm chart names
  that don't exist upstream — a reminder that this class of assumption must be
  verified against upstream docs, not inferred from a project's naming
  conventions.

### Where the OCI→raw-disk step should live

This step (pull an OCI image, produce a raw disk via kairos-operator) has
**no vSphere-specific content whatsoever** — it is the literal shared piece
the "reusable for Proxmox/libvirt" requirement is about. `banlieue-controller`
(main controller) must stay free of any provider's build tooling per the
CRD-only non-negotiable, and `banlieue-provider-vsphere` must stay free of
generic OS-build logic so a future `banlieue-provider-proxmox` doesn't have to
duplicate it. Per ADR-0004, every role is a `crates/banlieue-*` library crate
exposing `Cli` + `run()`, dispatched from the one `banlieue` binary — a new
role is the correct shape here, not a bolt-on to an existing crate.

### Where the raw→VMDK conversion and per-zone import should live

The opposite is true here: converting a raw disk to a vSphere-importable VMDK,
and uploading it into a specific zone's datastore, is entirely backend-specific
— a Proxmox backend would want qcow2 or raw directly (no VMDK), a libvirt
backend wants the raw disk basically as-is. This stays in each provider's own
crate — for today, `banlieue-provider-vsphere`'s existing `vmimage.rs`
reconciler, extended rather than replaced.

### The handoff problem

`banlieue-imagebuilder` and `banlieue-provider-vsphere` run as separate
Deployments (ADR-0003) and must not call each other directly (non-negotiable
#1). The only channel is CRD state. The artifact itself is a multi-hundred-MB
disk image — its *bytes* cannot travel through a CRD field, only a *pointer* to
where those bytes live can. The cheapest pointer that avoids re-inventing an
artifact store is the PVC kairos-operator already created; `banlieue-provider-vsphere`'s
per-zone import Jobs can mount that same PVC read-only if it is
`ReadOnlyMany`-capable, or fall back to running zone imports serially if the
cluster's default `StorageClass` is `ReadWriteOnce`-only (still correct, just
not concurrent across zones).

## Decision

**New library crate `crates/banlieue-imagebuilder`**, following the ADR-0004
pattern exactly: `pub struct Cli` (`clap::Args`) + `pub async fn run(cli: Cli)
-> anyhow::Result<()>`, built on `banlieue-provider-sdk`'s `bootstrap` /
`leader` / `ssa` / `status` / `reconciler` modules (no new bootstrap code).
Wired into `crates/banlieue` as `banlieue imagebuilder [flags]`, a top-level
subcommand alongside `controller` and `provider`, gated behind a default-on
`imagebuilder` Cargo feature (mirrors the per-provider feature-gating pattern
even though this role has no heavy backend SDK to gate out — consistency and
future-proofing over a real weight saving today).

### Scope: `banlieue-imagebuilder` produces a raw disk. Nothing more.

For every `VMImage` with at least one `spec.sources[]` entry where `kind ==
Url`:

1. **Pending → Building.** Server-side-apply an `OSArtifact` named
   `<vmimage-name>-build` in the configured build namespace (default
   `banlieue-system`), setting `spec.image.ref = importFrom`,
   `spec.artifacts.cloudImage = true`, `spec.artifacts.arch` from
   `VMImage.spec.architecture`. Field manager: `banlieue.io/imagebuilder`
   (new constant in `banlieue-provider-sdk::ssa`).
2. **Building / Exporting.** Watch the `OSArtifact`'s `status.phase` and mirror
   it into a **new top-level** `VMImage.status.rawDiskArtifact` field (not
   inside `per_provider[]` — this is provider-agnostic, one raw disk regardless
   of how many provider-class sources reference it):
   ```rust
   pub struct RawDiskArtifactStatus {
       pub phase: RawDiskArtifactPhase,     // Pending | Building | Ready | Failed
       pub os_artifact_ref: String,         // name of the OSArtifact CR
       pub pvc_ref: Option<LocalObjectReference>, // "<os_artifact_ref>-artifacts"
       pub disk_file: Option<String>,       // "<os_artifact_ref>.raw"
       pub reason: Option<String>,
       pub message: Option<String>,
   }
   ```
3. **Ready.** Once `OSArtifact.status.phase == Ready`, populate `pvc_ref` /
   `disk_file` and set `phase = Ready`. This is the entire contract
   `banlieue-provider-vsphere` (and later `-proxmox` / `-libvirt`) reads.
   `banlieue-imagebuilder` never touches `VMImage.status.perProvider[]` —
   that stays owned by each provider's own field manager, so SSA field
   ownership never contends between the two controllers even though both
   patch the same `VMImage.status` subresource.

`banlieue-imagebuilder` does not know vSphere, Proxmox, or libvirt exist. It
does not create Jobs, does not run `qemu-img`, does not open a datastore
connection. That is deliberate — it is the one piece of this pipeline that is
genuinely shared, and keeping it that way is the point of splitting it out.

### Extending `banlieue-provider-vsphere`'s `vmimage.rs`

`find_vsphere_source` currently only matches `ImageSourceKind::Template`.
Extend the per-provider reconcile to also match `Url` sources, gated on
`VMImage.status.rawDiskArtifact.phase == Ready`:

- Not ready yet → per-provider row `ready=false`, `reason=BuildPending`. No
  new failure mode; this is a normal, requeued waiting state.
- Ready → for each of the Provider's `status.failureDomains[]`, run a
  **per-zone import**: a Job (created by `banlieue-provider-vsphere`, in the
  artifact's namespace) mounts the shared PVC read-only, converts
  `<disk_file>` to a `streamOptimized` VMDK via `qemu-img convert`, then
  uploads it into that zone's datastore and registers it as a template. This
  is where the still-open `vim_rs` question from earlier design work lives:
  whether the upload goes through an OVF/`HttpNfcLease` import or a plain
  datastore file PUT followed by manual VM creation. That call needs
  hands-on verification against `vim_rs`'s real API surface (its own MCP
  server, per its docs, is built for exactly this "does this crate support
  X" question) before being written for real — it is **explicitly out of
  scope for this ADR** and tracked as a follow-up, consistent with how the
  existing `vmimage.rs` module already only implements `Template` lookups
  today.
- `ImagePerProviderStatus` gains a `zones: Vec<ZoneImageStatus>` field so
  per-zone import progress (not just the aggregate provider-level `ready`) is
  observable — required once "ready" can legitimately mean "ready in 2 of 3
  zones, importing into the third."

### Namespacing and the PVC access requirement

`banlieue-imagebuilder`'s build namespace (default `banlieue-system`,
configurable via `--build-namespace`) is where the OSArtifact and its PVC
live. `banlieue-provider-vsphere`'s per-zone import Jobs run in that **same**
namespace (read from `rawDiskArtifact.pvc_ref`), so this is same-namespace PVC
access, not cross-namespace — simpler than the alternative (an in-cluster HTTP
artifact server) and reuses a primitive Kubernetes already has. Concurrent
per-zone Jobs need a `ReadOnlyMany`-capable `StorageClass` for the artifacts
PVC to import zones in parallel; on a `ReadWriteOnce`-only default
`StorageClass`, `banlieue-provider-vsphere` serializes zone imports instead of
rejecting the VMImage — documented as an operational note, not a hard
requirement, since this dev/qa environment's default `StorageClass` has not
been confirmed to support `ROX`.

## Consequences

**Positive**

- The genuinely shared piece (OCI → raw disk) is isolated once, in a crate
  with zero vSphere knowledge — Proxmox/libvirt providers can reuse it
  unchanged when they land, matching the original "same concept, any backend"
  requirement directly.
- `ImageSourceKind::Url` / `import_from`, already present in the schema and
  previously a dead field, becomes real. No schema churn for existing
  `Template`-source `VMImage`s — this is purely additive.
- CRD-only handoff is preserved: the two controllers never call each other;
  `VMImage.status` is the entire interface, and SSA field ownership keeps
  `rawDiskArtifact` and `perProvider[]` from contending.
- Mirrors kairos-operator's own `status.phase` state machine instead of
  inventing a parallel one — one less state machine to keep mentally in sync
  with upstream.

**Negative / trade-offs**

- Adds a same-namespace PVC dependency between two independently-deployed
  controllers — a break from "CRD is the *only* coupling" in the strictest
  sense, though the bytes have to live somewhere and a CRD field cannot carry
  them; the *control* channel (when/whether to act) stays CRD-only, only the
  *artifact* channel is a shared volume.
- `banlieue-provider-vsphere`'s per-zone import (convert + upload) is left
  with a real open question — the exact `vim_rs` call shape — deliberately
  unresolved here rather than guessed. Until resolved, `Url`-sourced VMImages
  will sit at `ready=false, reason=BuildPending` even after the raw disk is
  built, same as `Template` sources sit unready when the template is simply
  missing. Not a regression, but not delivering the full nightly-test loop yet
  either.
- One more Deployment to run (`banlieue imagebuilder`), one more RBAC surface
  (OSArtifacts, PVCs, and — once conversion Jobs land — Jobs, in the build
  namespace).
- `kubectl apply -k` install for kairos-operator has no documented
  cert-manager or Helm path today; operators following this ADR's docs need
  the kustomize URL, not a Helm chart (a discrepancy worth flagging since an
  earlier, disconnected prototype assumed the opposite).

## Alternatives considered

- **Fold the OCI-pull/build step into `banlieue-provider-vsphere` directly.**
  Simpler short term (one less crate, one less Deployment) since vSphere is
  the only backend implemented today. Rejected: it re-couples generic
  OS-image-building to a specific backend, which is exactly the coupling the
  original requirement ("same concept... for proxmox, libvirt, kvm") rules
  out, and it would need extracting later anyway once a second provider lands.
- **Serve the raw disk over an in-cluster HTTP endpoint instead of a shared
  PVC.** Avoids the same-namespace-volume coupling, and would make the
  artifact fetchable by a provider running in a different namespace or even
  cluster. Rejected for now: it means running and securing an extra HTTP
  service for no immediate need (`banlieue-provider-vsphere` and
  `banlieue-imagebuilder` are deployed to the same management cluster), adds
  a new component to the supply-chain surface, and kairos-operator already
  gives us a PVC for free. Revisit if a future provider needs to consume the
  artifact from outside the build namespace/cluster.
- **One `VMImageStatus.perProvider[].buildArtifact` entry per provider class
  instead of one shared `rawDiskArtifact`.** Would let each provider request a
  differently-configured build (different `diskSize`, different `bundles`).
  Rejected for v1: the OCI source and architecture are the same regardless of
  destination backend in every case seen so far; a per-provider build can be
  added later as an *additional* optional field without breaking the shared
  one, if a real need appears.
