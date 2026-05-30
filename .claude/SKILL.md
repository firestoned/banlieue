# Claude Skills Reference

Reusable procedural skills extracted from CLAUDE.md. Each skill has a canonical name (kebab-case), trigger conditions, ordered steps, and a verification check. Invoke a skill by name: *"run the cargo-quality skill"* or *"do a verify-crd-sync"*.

---

## `verify-crd-sync`

**When to use:**
- Before investigating reconciliation loops or infinite loops
- Before debugging "field not appearing in kubectl output" issues
- After ANY modification to types under `crates/banlieue-api/src/`
- When status patches succeed but data doesn't persist
- When user reports unexpected controller behavior

**Steps:**
```bash
# 1. Check deployed CRD schema in cluster
kubectl get crd <kind>.banlieue.io -o yaml | grep -A 20 "<field-name>:"
# (or .infrastructure.banlieue.io for infra CRDs)

# 2. Check Rust struct definition
rg -A 10 "pub struct <StructName>" crates/banlieue-api/src/

# 3. If mismatch detected, regenerate CRDs
cargo run -p banlieue-api --bin crdgen --features crdgen > /tmp/banlieue-crds.yaml

# 4. Diff against deployed CRDs
diff /tmp/banlieue-crds.yaml deploy/crds/
```

**Verification:** Field appears in `kubectl get` output after patch; no infinite reconciliation loop.

---

## `regen-crds`

**When to use:**
- After ANY edit to Rust types in `crates/banlieue-api/src/`
- Before deploying CRD changes to a cluster

**Steps:**
```bash
# 1. Regenerate all CRD YAML from Rust types
cargo run -p banlieue-api --bin crdgen --features crdgen > /tmp/banlieue-crds.yaml

# 2. (Optional, once deploy/crds/ is wired up) split & write per-CRD YAMLs to deploy/crds/

# 3. Verify generated YAMLs
kubectl apply --dry-run=client -f /tmp/banlieue-crds.yaml

# 4. Update examples to match new schema (see validate-examples skill)
```

**Verification:** `kubectl apply --dry-run=client -f /tmp/banlieue-crds.yaml` succeeds with no `unknown field` or `required field missing` errors.

---

## `regen-api-docs`

**When to use:**
- After all CRD changes, example updates, and validations are complete (run this LAST)
- Before any documentation release

**Steps:**
```bash
# Regenerate the Markdown API reference from the code-first CRD types.
# Writes docs/src/reference/api.md (one page documenting every CRD, every field).
make api-docs

# `make crds` already runs this as its last step, so a normal CRD-change flow
# (regen-crds) refreshes the API reference automatically. Run `make api-docs`
# standalone only when iterating on docs without re-emitting the CRD YAML.
```

**Verification:** `docs/src/reference/api.md` reflects the current CRD schema; `cd docs && poetry run mkdocs build --strict` succeeds.

> The generator is `crates/banlieue-api/src/bin/crddoc.rs` (rendering logic in `src/crddoc.rs`). It is **generated, never hand-edited** — edit the rustdoc on the Rust types and regenerate.

---

## `cargo-quality`

**When to use:**
- After adding or modifying ANY `.rs` file
- Before committing any Rust code changes
- At the end of EVERY task involving Rust code (NON-NEGOTIABLE)

**Steps:**
```bash
# 0. Ensure cargo is in PATH
source ~/.zshrc 2>/dev/null || true

# 1. Format (workspace-wide)
cargo fmt --all

# 2. Lint with strict warnings (fix ALL warnings)
cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic -A clippy::module_name_repetitions

# 3. Test (ALL tests must pass)
cargo test --all

# 4. Security audit (optional, if installed)
cargo audit 2>/dev/null || true
```

**Verification:** All three commands exit with code 0. No warnings, no test failures.

---

## `tdd-workflow`

**When to use:**
- Adding any new feature or function
- Fixing a bug
- Refactoring existing code

**Steps:**

**RED — Write failing tests first (before any implementation):**
```bash
# Edit crates/<crate>/src/<module>_tests.rs — add test(s) that define expected behavior
cargo test -p <crate> <test_name>   # Must FAIL at this point
```

**GREEN — Implement minimum code to pass tests:**
```bash
# Edit crates/<crate>/src/<module>.rs — write simplest code that makes tests pass
cargo test -p <crate> <test_name>   # Must PASS now
```

**REFACTOR — Improve while keeping tests green:**
```bash
# Extract constants, add docs, improve error handling
cargo test --all                    # Must still PASS
cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic -A clippy::module_name_repetitions
```

**Test file pattern:**
- Source: `src/foo.rs` → declare `#[cfg(test)] mod foo_tests;` at the bottom
- Tests: `src/foo_tests.rs` → wrap in `#[cfg(test)] mod tests { use super::super::*; ... }`

**Verification:** All tests pass, clippy is clean, test covers success path + error paths + edge cases.

---

## `update-changelog`

**When to use:**
- After ANY code modification (mandatory for auditing and provenance)

**Steps:**

Open `.claude/CHANGELOG.md` and prepend an entry in this exact format:

```markdown
## [YYYY-MM-DD HH:MM] - Brief Title

**Author:** <Name of requester or approver>

### Changed
- `path/to/file.rs`: Description of the change

### Why
Brief explanation of the business or technical reason.

### Impact
- [ ] Breaking change (CRD schema, public API)
- [ ] Requires cluster rollout
- [ ] Config / examples change only
- [ ] Documentation only
```

**Verification:** Entry has `**Author:**` line (MANDATORY — no exceptions), timestamp, and at least one `### Changed` item.

---

## `update-docs`

**When to use:**
- After any code change under `crates/`
- After CRD changes, API changes, configuration changes, or new features

**Steps:**
1. Identify what changed (feature, CRD field, behavior, error condition).
2. Update `.claude/CHANGELOG.md` (see `update-changelog` skill).
3. Update affected pages under `docs/`:
   - Design docs / ADRs (`docs/adr/`, `docs/design/`) for architectural decisions
   - User guides (`docs/user/`, Phase 4)
   - (Roadmap docs live outside the repo at `~/dev/roadmaps/banlieue/`. Update them there if scope or status shifted.)
4. Update `examples/*.yaml` to reflect schema or behavior changes.
5. If CRDs changed: run `regen-crds` skill, then `regen-api-docs` (LAST).
6. If README getting-started or features changed: update `README.md`.

**Verification checklist:**
- [ ] `.claude/CHANGELOG.md` updated with author
- [ ] All affected `docs/` pages updated
- [ ] All YAML examples validate: `kubectl apply --dry-run=client -f examples/`
- [ ] API docs regenerated if CRDs changed
- [ ] Roadmap phase status reflects reality

---

## `validate-examples`

**When to use:**
- After any CRD schema change
- Before committing changes to `examples/`
- As part of the `pre-commit-checklist`

**Steps:**
```bash
# Validate all example YAML files (client-side schema check)
kubectl apply --dry-run=client -f examples/

# Or validate individually
for file in examples/*.yaml; do
  echo "Validating $file"
  kubectl apply --dry-run=client -f "$file"
done

# Also validate that generated CRDs themselves are well-formed:
cargo run -p banlieue-api --bin crdgen --features crdgen > /tmp/banlieue-crds.yaml
kubectl apply --dry-run=client -f /tmp/banlieue-crds.yaml
```

**Verification:** All files pass dry-run with no errors. No `unknown field` or `required field missing` errors.

---

## `add-new-crd`

**When to use:**
- When adding a new Custom Resource Definition to banlieue

**Steps:**
1. Decide the API group:
   - User-facing CR → `banlieue.io/v1alpha1` → add under `crates/banlieue-api/src/banlieue/`
   - Provider infra CR → `infrastructure.banlieue.io/v1alpha1` → add under `crates/banlieue-api/src/infrastructure/`
2. Add the new `CustomResource` struct (and `Status`/sub-types) to the chosen module:
   ```rust
   #[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
   #[kube(
       group = "banlieue.io",            // or "infrastructure.banlieue.io"
       version = "v1alpha1",
       kind = "MyNewResource",
       namespaced,
       status = "MyNewResourceStatus",
   )]
   #[serde(rename_all = "camelCase")]
   pub struct MyNewResourceSpec {
       pub field_name: String,
   }
   ```
3. Re-export the new type from the module's `mod.rs` and (if appropriate) from `lib.rs::prelude`.
4. Register it in `crates/banlieue-api/src/bin/crdgen.rs` so `crdgen` emits its YAML.
5. Run `regen-crds` skill.
6. Add examples to `examples/`.
7. Run `validate-examples` skill.
8. Add documentation under `docs/` (`docs/adr/` for architectural choices, `docs/design/` for contract docs). Phase plans go in the maintainer's out-of-repo roadmap.
9. Run `regen-api-docs` skill (LAST).
10. Run `cargo-quality` skill.
11. Run `update-changelog` skill.

**Verification:** `kubectl apply --dry-run=client -f /tmp/banlieue-crds.yaml` succeeds; new resource appears in `crdgen` output; tests cover Spec/Status serde round-trips.

---

## `pre-commit-checklist`

**When to use:**
- Before committing any change (mandatory gate)

**Checklist:**

### If ANY `.rs` file was modified:
- [ ] Tests updated/added/deleted to match changes (TDD — see `tdd-workflow`)
- [ ] All new public functions have tests
- [ ] All deleted functions have tests removed
- [ ] `cargo fmt --all` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes (fix ALL warnings)
- [ ] `cargo test --all` passes (ALL tests green)
- [ ] Rustdoc comments on all public items, accurate to actual behavior

### If any file under `crates/banlieue-api/src/` was modified:
- [ ] `cargo run -p banlieue-api --bin crdgen --features crdgen` succeeds
- [ ] `examples/*.yaml` updated to match new schema
- [ ] `docs/` updated for any schema or behavior change
- [ ] `kubectl apply --dry-run=client -f examples/` passes
- [ ] (When `crddoc` exists) API docs regenerated LAST

### If reconciler code was modified (Phase 1+):
- [ ] Reconciliation flow diagrams updated under `docs/design/`
- [ ] New behaviors documented in user docs (and the out-of-repo roadmap if scope shifted)
- [ ] Status conditions, finalizers, owner references all verified

### Always:
- [ ] `.claude/CHANGELOG.md` updated with **Author:** line (MANDATORY)
- [ ] All YAML examples validate: `kubectl apply --dry-run=client -f examples/`
- [ ] CRD output validates: `kubectl apply --dry-run=client -f /tmp/banlieue-crds.yaml`
- [ ] No secrets, tokens, credentials, internal hostnames, or IP addresses committed
- [ ] No `.unwrap()` in production code (tests are fine)

**Verification:** Every checked box above passes. A task is NOT complete until the full checklist is green.
