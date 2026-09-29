---
name: cargo-quality
description: Mandatory quality gate after ANY Rust change — cargo fmt, clippy -D warnings, and the test suite. The task is NOT complete until all three pass. Load at the end of every task that touched a .rs file, before updating the changelog or declaring the work done.
---

# Cargo Quality Gate

Run all three, in this order, from the workspace root. Every one must pass —
a task that modified any `.rs` file is **not complete** until they do.

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Rules

1. **fmt first.** It can reflow code that clippy or a test diff would then
   report differently. `cargo fmt --all -- --check` is the CI form; locally,
   just format.
2. **Clippy warnings are errors.** Fix every warning — never `#[allow(...)]`
   one away without a comment stating the constraint that justifies it, and
   never weaken the `-D warnings` invocation.
3. **All tests, not just the crate you touched.** A type change in
   `banlieue-api` ripples into every crate that constructs the type
   exhaustively. Scope with `-p <crate>` only for a fast inner loop; the
   final gate is workspace-wide.
4. **Fix, re-run, repeat.** The gate is green output, not a first attempt.

## After the three commands pass, verify (rules/testing.md)

- Rustdoc on every public function you touched still matches what it does
  (`# Arguments`, `# Errors`, examples).
- Tests were updated/added/deleted to match the change, in the separate
  `*_tests.rs` file pattern (`src/foo.rs` → `#[cfg(test)] mod foo_tests;` →
  `src/foo_tests.rs`).
- No magic numbers introduced (any literal other than 0/1 is a named
  constant — rules/rust-style.md).
- If a type under `crates/banlieue-api/src/` changed: run the `regen-crds`
  flow (`make crds`), and remember a live cluster's CRD needs re-applying
  too, not just the binary.

## Common gotchas in this repo

- `cargo clippy` without `-- -D warnings` reports warnings but exits 0 —
  always pass the flag; green-looking output without it proves nothing.
- Tests behind `#[ignore]` (live/e2e) are excluded by design; do not try to
  make them pass here. They run via their `make *-live-test` / `make *-e2e`
  targets against real infrastructure.
