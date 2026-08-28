# 0038 — userData supports both Secret and ConfigMap sources

## Status

Accepted — 2026-08-27. Extends [ADR-0025](0025-vspheremachine-userdata-secret-rbac.md)
(`UserDataSpec` shape and controller-side resolution).

## Context

ADR-0025 gave `VirtualMachine.spec.userData` a `secretRef` + `key` shape —
the controller reads a Secret, renders placeholders, and inlines the
content onto `VSphereMachineSpec.userData: Option<String>`.

In practice, much per-VM cloud-config data is not sensitive: hostname
overrides, package lists, mount directives. Requiring a Secret for every
VM adds operational friction (base64 encoding, RBAC grants for Secret
creation in GitOps, opaque diffs in `kubectl describe`). A ConfigMap is
a better fit for non-sensitive bootstrap data and is easier to review in
plain text.

## Decision

**`UserDataSpec` becomes a two-source exactly-one-of type (matching the
project's existing [`CABundleSource`] pattern), supporting `secretRef`
and `configMapRef`.** Both are optional `KeySelector`s; exactly one must
be set.

Concretely:

1. `UserDataSpec` fields change from `{ secretRef: LocalObjectReference,
   key: String }` to `{ secretRef: Option<KeySelector>, configMapRef:
   Option<KeySelector> }`. A `validate()` method enforces the
   exactly-one-of invariant. The default key for both is `user-data`
   (constant `DEFAULT_USER_DATA_KEY`).

2. `banlieue-controller`'s `resolve_rendered_user_data` reads from
   whichever source is set: `resolve_secret_data` for `secretRef`,
   `resolve_configmap_data` for `configMapRef`. The downstream
   `VSphereMachineSpec.userData: Option<String>` and the provider are
   unchanged — they receive the already-rendered content string.

3. RBAC: the namespace-scoped `Role` in `banlieue-system`
   (`deploy/controller/rbac/role.yaml`) adds `configmaps` alongside
   `secrets` to the `get` grant. Renamed from
   `banlieue-controller-secrets` to `banlieue-controller-userdata` to
   reflect the broader scope.

## Consequences

- Non-sensitive cloud-config can live in a ConfigMap, reducing
  operational friction for GitOps and `kubectl` review.
- The RBAC grant widens from `secrets` to `secrets + configmaps` within
  the single tenant namespace — acceptable for the same single-tenant
  posture ADR-0025 documents.
- The `v1alpha1` CRD shape changes: `userData.key` moves inside each
  ref (via `KeySelector`) and both refs become optional. This is a
  breaking change to existing `VirtualMachine` manifests, acceptable in
  `v1alpha1`.
- The provider layer (`VSphereMachineSpec.userData: Option<String>`) and
  `build_guestinfo` / `ensure_vm` are completely unchanged — they only
  see the rendered string, agnostic to its source.
