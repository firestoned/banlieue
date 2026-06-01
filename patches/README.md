# Local dependency patches

This directory holds patches we carry against upstream crates while a fix is
in-flight. They are applied to vendored checkouts by Makefile targets and wired
into the build via `[patch.crates-io]` in the workspace `Cargo.toml`.

## `vim_rs.patch` — noclue/vim_rs

We build `vim_rs` from a local checkout (`third_party/vim_rs/`, gitignored)
pinned to an upstream commit, with `vim_rs.patch` applied on top. The crate lives
in the `vim_rs/` subdirectory of that multi-crate repo, so the patch target is
`third_party/vim_rs/vim_rs`. `make vendor-vim-rs` does this idempotently:

1. Clone `noclue/vim_rs` into `third_party/vim_rs` (first run only).
2. `git reset --hard` to `VIM_RS_REF` (a clean upstream base).
3. Decide what to do with `patches/vim_rs.patch`:
   - **applies cleanly** → apply it;
   - **reverse-applies** (the change is already in the ref, i.e. merged
     upstream) → skip, nothing to do;
   - **neither** → hard error (the patch is stale; refresh it or bump the ref).

### Why a commit, not a tag

The version we depend on (`=0.4.4`, the first to carry the `vcsim_compat` feature
the vSphere provider uses) was published to crates.io and lives on `main`, but
was never git-tagged — the newest tag (`v0.4.3`) predates that feature. So
`VIM_RS_REF` in the `Makefile` is the commit SHA of the "Prepare 0.4.4 release"
commit. The `Cargo.toml` dep is pinned **exactly** (`=0.4.4`) so cargo's resolver
lands on that version and the `[patch.crates-io]` path override actually takes
effect; a range like `0.4` would let cargo prefer the crates.io 0.4.4 and
silently ignore the patch (`cargo tree` would warn `patch ... was not used`).

### How it's wired into builds

- **Local:** `make build` / `test` / `lint` / `crds` / `sbom` / image targets all
  declare `vendor-vim-rs` as a prerequisite, so the checkout is present and
  patched before any cargo runs. A bare `cargo build` outside `make` needs you to
  run `make vendor-vim-rs` once first (the patch source is gitignored, so it's
  absent on a fresh clone).
- **CI:** the `.github/actions/vendor-vim-rs` composite action (which just runs
  `make vendor-vim-rs`) is dropped into every cargo-using job in
  `.github/workflows/build.yaml` right after checkout. `docs.yaml` vendors
  transitively through `make docs` → `api-docs`.

### Creating / refreshing the patch

```sh
make vendor-vim-rs                 # ensure third_party/vim_rs is at VIM_RS_REF
cd third_party/vim_rs/vim_rs
# …make your changes…
cd ..                             # back to repo root of the clone (third_party/vim_rs)
git diff > ../../patches/vim_rs.patch
cd ../..
make vendor-vim-rs                 # verify it re-applies cleanly from a clean base
```

> Generate the diff from the clone root (`third_party/vim_rs`) so paths are
> repo-relative (e.g. `vim_rs/src/...`) and `git -C third_party/vim_rs apply`
> resolves them.

If `patches/vim_rs.patch` is absent, `make vendor-vim-rs` builds against the
unmodified upstream commit — so the workspace still compiles before the patch
exists.

### Retiring the patch

Once the change ships in an upstream release:

1. Bump `vim_rs` in `[workspace.dependencies]` to that version (drop the exact
   `=` pin if you no longer need to match a specific untagged commit).
2. Delete the `[patch.crates-io]` block in `Cargo.toml`.
3. Delete `patches/vim_rs.patch` and the `vendor-vim-rs` target + its
   prerequisites and the CI composite action (or leave the target — it will
   report "already present (merged upstream)" and no-op).
