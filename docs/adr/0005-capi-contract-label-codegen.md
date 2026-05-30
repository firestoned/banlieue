<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0005 — Emit the CAPI contract label from crdgen (not kustomize)

- **Status:** Accepted
- **Date:** 2026-05-31
- **Deciders:** Erick Bourgeois
- **Related:** D-001 (code-first CRDs), D-005 (CAPI contract compliance), ADR-0002 (InfraCluster). Refines the "applied at deploy time via kustomize" note in ADR-0002 and in the infra CRD docstrings.

## Context

CAPI discovers which CRDs implement its contracts by a **CRD-level label**,
`cluster.x-k8s.io/<contract-version>: <comma-separated CRD versions>` — for
banlieue, `cluster.x-k8s.io/v1beta2: v1alpha1`. Without it, CAPI core will not
treat `VSphereMachine` (InfraMachine), `VSphereMachineTemplate`
(InfraMachineTemplate), or `VSphereCluster` (InfraCluster) as contract-compliant
infrastructure objects, and conversion / reference resolution won't work.

`kube-derive` cannot emit CRD **metadata.labels** — only schema content. So the
label has to be added by something downstream. Until now the code carried a
note that it would be "applied at deploy time via kustomize," but **no such
overlay was ever written** — so the label is absent from every infra CRD today.
This is a real contract gap (flagged as a follow-up in ADR-0002).

Two ways to close it:

1. **Kustomize overlay** at deploy time — a patch (or `commonLabels` scoped to
   the infra CRDs) layered over `deploy/crds/`.
2. **crdgen post-processing** — add the label in `crdgen_support`, the same
   pipeline (`prepared()`) that already promotes the spec description. The
   generated YAML in `deploy/crds/` then carries the label intrinsically.

## Decision

**Emit the CAPI contract label from `crdgen`** (option 2). Add a
`add_capi_contract_label()` fix-up in `crdgen_support`, applied by `prepared()`
only to CRDs in the `infrastructure.banlieue.io` group. The label value is the
comma-joined list of **served** version names (today: `v1alpha1`), matching the
CAPI rule that the value enumerates the contract-conforming CRD versions.

The `banlieue.io` group (`Provider`, `VirtualMachine`, `VMClass`, `VMImage`) is
**not** labelled — those are not CAPI contract objects.

Rationale:

- **Code-first, single source of truth.** `deploy/crds/` is generated, never
  hand-edited (D-001). Baking the label in means it cannot drift from the types
  and there is no separate overlay to forget. A kustomize overlay is a second
  artifact that must be kept in sync with the set of infra CRDs by hand.
- **Group-scoped automatically.** The post-processor keys off `spec.group`, so
  every present and future infra CRD (Proxmox, libvirt) is covered the moment it
  is registered in `crdgen` — no overlay edit per new kind.
- **Consistent with the existing pipeline.** `prepared()` already mutates CRDs
  post-derive; this is one more fix-up of the same kind.

## Consequences

- `deploy/crds/infrastructure.banlieue.io_*.yaml` gain
  `metadata.labels."cluster.x-k8s.io/v1beta2": "v1alpha1"` on regeneration.
  This is a CRD change → `make crds` regenerates and the API reference refreshes.
- Installing CRDs from `deploy/crds/` (kubectl/kustomize/Helm) now yields
  contract-compliant infra CRDs with no extra step.
- The "via kustomize" wording in the `VSphereMachine` / `VSphereCluster`
  docstrings and in ADR-0002's consequences is corrected to "emitted by crdgen."
- The separate `cluster.x-k8s.io/aggregate-to-manager: "true"` label on the
  controller **ClusterRole** (D-005) is unaffected — that is RBAC, already
  present in `deploy/controller/rbac/clusterrole.yaml`, and out of scope here.
- If banlieue ever serves multiple CRD versions, the value automatically becomes
  the comma-joined served set; no code change needed.

## Alternatives considered

- **Kustomize overlay (option 1).** Rejected: a second artifact to maintain,
  easy to forget for a new infra CRD, and it splits the CRD definition across
  two places. Code-first keeps the contract label with the types.
- **Hand-edit the generated YAML.** Rejected outright — violates D-001
  (generated YAML is never hand-edited).
