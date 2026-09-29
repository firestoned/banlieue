---
name: regen-crds
description: Regenerate deploy/crds/ and the API reference from the Rust types after ANY change under crates/banlieue-api/src/ — including doc-comment-only edits, which land in the CRD descriptions. Never hand-edit generated CRD YAML. Load whenever a banlieue-api type changed, status patches silently don't persist, or a field doesn't appear in kubectl output.
---

# Regenerate CRDs from the Rust Types

`crates/banlieue-api` is the source of truth for every CRD shape. The YAML in
`deploy/crds/` and the API reference at `docs/src/reference/api.md` are
**generated** — never edit either by hand.

```sh
make crds        # crdgen → deploy/crds/*.yaml, then crddoc → docs/src/reference/api.md
```

(`make crds` runs both generators; `make api-docs` exists for the reference
alone, but after a type change you always want both.)

## When to run

- After **any** edit under `crates/banlieue-api/src/` — type changes, new
  fields, `#[serde]`/`#[schemars]` attribute changes, **and doc-comment-only
  edits**: rustdoc becomes the `description:` in the generated schema, so a
  comment tweak dirties `deploy/crds/` too.
- When a status patch "succeeds" (HTTP 200) but the field never persists, or
  a field doesn't appear in `kubectl get -o yaml` — the classic symptom of a
  deployed CRD that predates the Rust type. Regenerate, diff, re-apply.

## After regenerating

1. **Diff the output** (`git diff deploy/crds/ docs/src/reference/api.md`)
   and check it contains exactly the change you made — an unexpected diff
   means someone hand-edited a generated file or your change rippled further
   than intended.
2. **Update `examples/`** to match any schema change, then validate:
   `kubectl apply --dry-run=client -f examples/`.
3. **Search the docs** for examples using the changed CRD
   (`rg "kind: <CRDName>" docs/src/`) and fix field names — never guess them.
4. **Commit the generated files with the type change** — a PR that changes a
   type but not its YAML is incomplete.

## The deployment half (easy to forget)

Regenerating fixes the *repo*; a **live cluster keeps serving the old
schema** until the CRD is re-applied. Rolling out a new binary without
re-applying the CRD reproduces the silent-failure symptom above (found live:
`VSphereMachine.status.guestInstalled` 500'd for want of a regenerated CRD).
Tell the user which CRDs changed so they can re-apply — and never
blanket-apply `deploy/crds/` to a cluster that may carry schemas from a
branch ahead of yours; diff first (`kubectl diff -f deploy/crds/`).

## Gotchas

- `crdgen` needs the feature flag: it is `cargo run -p banlieue-api --bin
  crdgen --features crdgen` under the hood — running it without
  `--features crdgen` fails to build.
- Rust doc comments under `crates/banlieue-api/src/` also land in
  `deploy/crds/*.yaml`, so prose renames (e.g. roadmap renumbering) can
  require a regen with no code change at all.
- Generated YAML is deterministic: a dirty `deploy/crds/` after `make crds`
  on an untouched tree means the checked-in YAML had drifted — fix that in
  its own commit.
