# 0037 — VMImage: layered cloud-config (base + overlays)

## Status

Proposed — 2026-08-25. Extends `banlieue.io/v1alpha1` `VMImage`
(`crates/banlieue-api/src/banlieue/vmimage.rs`) and
`crates/banlieue-imagebuilder`.

## Context

`VMImageSpec.cloud_config: Option<CloudConfigSource>` is a single
value-or-source (today, `secretRef` only) passed straight through to
`kairos-operator`'s `OSArtifact.spec.artifacts.cloudConfigRef` — this is
the cloud-config `auroraboot build-iso --cloud-config` bakes into the
built ISO, consumed by Kairos's *unattended-install* stage (ADR-0020/0021:
`install.poweroff`, the admin user, etc.), not the guest's own runtime
`/oem/` cloud-init stages.

Asked: support a "base" cloud-config plus additional overlays layered on
top, so common boilerplate (a stock admin user, CrowdStrike Falcon setup,
machine-identity stripping — all already present in this project's own
`kairos-base-cloud-config` Secret) can live in one shared base document
instead of being copy-pasted into every `VMImage`'s cloud-config.

Two upstream facts researched before deciding anything:

1. **Kairos/yip's own runtime config loading genuinely does support
   multiple files, merged** — `/oem/*.yaml` on a running instance are all
   read and merged, later directories overriding earlier ones, with a
   documented gotcha: "each action in a stage should be given a unique
   name... to prevent data loss when multiple configurations target the
   same stage" (i.e., a stage's step list can lose entries across a merge
   if two configs write the same list position without unique `name:`
   fields). This confirms merging cloud-configs is a normal, supported
   *idea* in the Kairos ecosystem generally.
2. **But `kairos-operator`'s `OSArtifact.spec.artifacts.cloudConfigRef` is
   a single reference** (`name` + `key` — one Secret, one key), not a
   list, and there is no documented equivalent of the runtime `/oem/`
   multi-file merge for the *install-stage* cloud-config `auroraboot`
   bakes into the ISO. Whatever layering banlieue wants here, **banlieue
   itself must produce one final merged document** before handing
   anything to `OSArtifact` — there is no upstream mechanism to defer to.

Also confirmed: `banlieue-imagebuilder`'s reconciler **never reads Secret
content today** — for `cloudConfig` or `isoOverlay`, only the Secret's
`name`/`key` are ever referenced (explicitly documented in
`desired_os_artifact`'s doc comment for `isoOverlay`: "Only the Secret's
name and the caller-declared key list are ever referenced here; its
content is never read"). Its `ClusterRole`
(`deploy/imagebuilder/rbac/clusterrole.yaml`) grants no `secrets` verbs at
all. Merging multiple cloud-configs requires reading and parsing their
actual YAML content — a genuinely new capability and a real RBAC
widening, not a mechanical schema change.

## Decision

1. **Schema: `cloud_config` becomes `cloud_configs: Vec<CloudConfigSource>`**
   (renamed plural; breaking change, acceptable — no release/consumers
   exist yet). Empty list = no cloud-config, same as today's `None`.
   Order is merge order: index 0 is the base, each subsequent entry
   layers on top. Each entry is the same `CloudConfigSource`
   value-or-source shape already established (`secretRef` today; the
   type already anticipates future source kinds via its "exactly one
   source" invariant).

2. **`banlieue-imagebuilder` reads, merges, and owns a new merged
   Secret.** New reconcile step, before building the `OSArtifact`: fetch
   each referenced Secret's cloud-config key content, in list order;
   parse as YAML; deep-merge per Decision #3; SSA-apply the merged
   document into a new Secret this reconciler owns
   (`<vmimage-name>-cloud-config-merged`, owner-referenced to the
   `VMImage` — same GC pattern as `OSArtifact` itself); point
   `OSArtifact.spec.artifacts.cloudConfigRef` at *that* owned Secret,
   never at the user's original Secret(s) directly. A single-entry
   `cloud_configs` list still goes through the merge step (as a
   trivial one-document "merge") rather than special-casing "exactly
   one" to pass through unchanged — one code path, not two.

3. **Merge semantics (banlieue's own contract, not a claim of matching
   yip's internal mergo behavior byte-for-byte — this is a build-time
   document banlieue produces, not the runtime `/oem/` loader):**
   - Maps deep-merge; for a scalar or map value at the same key, the
     later (higher-index) source wins.
   - Lists at the same key **concatenate in order** (base's entries
     first, each overlay's appended after) — matches the layering intent
     ("add on more, don't replace") and Kairos's own documented
     `stages`-uniqueness gotcha implies append-not-replace is the
     ecosystem's own expectation for list-shaped config. Callers are
     responsible for giving `stages.<name>[].name` unique values across
     their base + overlays, exactly as Kairos's own docs already warn —
     banlieue does not deduplicate or validate this.
   - A type mismatch at the same key across sources (e.g. one config has
     `users:` as a list, another has it as a scalar) is a hard merge
     error, surfaced on `VMImage.status`, not a silent coercion —
     "explicit over implicit."
   - Implemented as a pure, unit-tested Rust function operating on
     `serde_yaml::Value` (or an equivalent typed intermediate) — no
     dependency on yip/mergo itself.

4. **New RBAC: `get`/`list`/`watch` on `secrets` in the imagebuild
   namespace, scoped as tightly as the existing `Credentials` handling
   elsewhere in this codebase treats secret material** (never logged;
   `Debug` redaction where applicable, mirroring the SEC-013 precedent
   already established for vSphere credentials). This is the first time
   `banlieue-imagebuilder` reads Secret *content* rather than just
   referencing Secrets by name — call this out explicitly in the
   `ClusterRole` change and the CHANGELOG, not just as an incidental
   diff.

## Consequences

- A base + N overlay `VMImage.spec.cloudConfigs` list replaces
  copy-pasted boilerplate across images — the actual motivating use case.
- `banlieue-imagebuilder` gains a real new capability (reading and
  parsing Secret content) and a new owned resource type (the merged
  cloud-config Secret) — this is more than a schema widening; it's new
  reconcile surface needing its own tests, RBAC review, and a documented
  merge contract users can reason about (list-append, map-deep-merge,
  type-mismatch-errors).
- Breaking change to `VMImageSpec` (`cloudConfig` → `cloudConfigs`) —
  every existing `VMImage` manifest and example needs updating; acceptable
  per this project's "no release yet" convention, but `examples/` and
  `docs/src/reference/api.md` must be updated in the same change, not
  left stale.
- Does **not** attempt to replicate or hook into yip's own runtime
  `/oem/` merge mechanism — this ADR is scoped strictly to the
  install-stage cloud-config `auroraboot` bakes into the ISO. A
  `VMImage.spec.cloudConfigs`-style layering for the guest's own runtime
  `/oem/` stages (if ever wanted) is a separate, unscoped follow-up.
