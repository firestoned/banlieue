# Phase 1E — Documentation site (MkDocs Material)

> **Goal.** Scaffold a documentation site under `docs/` modeled on
> `~/dev/5-spot/docs/`, using MkDocs Material as the static-site generator,
> with content authored in Markdown, Poetry-managed Python deps, a CI
> workflow that builds the site on every PR / push and deploys to GitHub
> Pages on release, and a Makefile target (`make docs`) that builds the
> site plus rustdoc plus the generated CRD reference into a single artifact.
>
> **Stop condition.** A first-time visitor can clone the repo, run
> `make docs-serve`, see the site at `localhost:8000` with brand styling,
> navigate Getting Started → Concepts → Reference, and find the generated
> CRD reference + rustdoc linked from the Reference section. A `Documentation`
> GitHub workflow builds the site on every PR (no deploy) and deploys to
> `https://firestoned.github.io/banlieue/` on release.

## Why now, not in Phase 4

Phase 4 §4.1 already lists "Documentation" as a polish workstream — but the
tooling itself shouldn't wait that long. The cost of writing docs goes up
sharply once features ship without a place to put them; today we have
Phase 0 (the API types) and an empty `docs/` directory, which is the right
moment to wire the scaffold.

This phase ships **infrastructure only** — the empty MkDocs site, the
Makefile, the CI workflow, the brand styling. Filling in the prose is a
continuous activity from Phase 1A onward, and the bulk of the content
backlog still lives in Phase 4 §4.1.

## Preconditions

- Phase 0 done (✅ — `crates/banlieue-api` ships with `crdgen`).
- `~/dev/5-spot/docs/` available locally to copy from.
- GitHub Pages enabled on `firestoned/banlieue` (source: GitHub Actions).
  This is a one-time repo setting; the CI workflow itself doesn't require
  Pages to be enabled to *build*, only to *deploy*.

## Non-goals

- Authoring page content beyond stubs and the existing README copy.
- Custom Material theme features beyond the brand palette.
- Search backend other than Material's built-in client-side search.
- Versioned docs (mike). Defer until there's a v1.0 to compare against.
- Translations / i18n.

## What we copy from 5-spot

| Source (`~/dev/5-spot/docs/`)      | Destination (`docs/`)         | Adjustments for banlieue                              |
|---|---|---|
| `mkdocs.yml`                       | `docs/mkdocs.yml`             | `site_name`, `site_url`, `repo_url`, `nav` rewritten. Drop CALM-specific entries (banlieue may add later). |
| `pyproject.toml`                   | `docs/pyproject.toml`         | Rename package to `banlieue-docs`. Same dep pins. |
| `poetry.lock`                      | `docs/poetry.lock`            | Regenerated locally via `poetry lock` to ensure reproducibility. |
| `src/stylesheets/extra.css`        | `docs/src/stylesheets/extra.css` | Keep file structure; brand palette is banlieue's choice (proposed below). |
| `src/javascripts/mermaid-init.js`  | `docs/src/javascripts/mermaid-init.js` | Verbatim. |
| `src/index.md`                     | `docs/src/index.md`           | Rewrite — banlieue tagline + diagram. |
| `src/installation/`                | `docs/src/installation/`      | `quickstart.md`, `prerequisites.md`, `crds.md`. Controller install deferred until Phase 1A lands. |
| `src/concepts/`                    | `docs/src/concepts/`          | `Provider`, `VirtualMachine`, `VMClass`, `VMImage`, `VSphereMachine`, scheduling, status mirroring. |
| `src/reference/api.md`             | `docs/src/reference/api.md`   | Empty stub; populated by the future `crddoc` binary (Phase 4) OR a `cargo run --bin crdgen` fence. |
| `src/architecture/system.md`       | `docs/src/architecture/system.md` | Mermaid diagram of the CR graph. |
| `src/development/`                 | `docs/src/development/`       | `setup.md`, `building.md`, `testing.md`, `contributing.md`. |
| `src/operations/`                  | `docs/src/operations/`        | Empty stubs — Phase 1A controller fills these in. |
| `src/security/`                    | `docs/src/security/`          | `index.md` only for now; threat model is Phase 4. |
| `Makefile` docs targets            | (root) `Makefile`             | Repo doesn't have a Makefile yet — this phase adds one with only the `docs-*` targets. Cargo targets can join later. |
| `.github/workflows/docs.yaml`      | `.github/workflows/docs.yaml` | Drop CALM jobs initially. Keep MkDocs build + GH Pages deploy. Add `concurrency:` group from 5-spot. |

## Brand palette (proposal — change before scaffolding if not approved)

banlieue is a French word meaning "suburb" — picking a palette that evokes
the urban-periphery feel and stays distinct from 5-spot's blue/terracotta:

```css
/* Light mode */
--md-primary-fg-color:        #2E4057;  /* deep slate */
--md-primary-fg-color--light: #66839F;
--md-primary-fg-color--dark:  #1A2333;
--md-accent-fg-color:         #D97757;  /* warm clay (echoes 5-spot terracotta) */

/* Dark mode */
--md-primary-fg-color-dark:        #66839F;  /* lift slate for contrast on dark */
--md-primary-fg-color-dark--light: #8AA5BF;
--md-accent-fg-color-dark:         #E89372;
```

Mirror 5-spot's `palette: custom` arrangement and drive everything from
`extra.css`. If the user prefers a different palette, swap the hex values
before the CSS is committed.

## File layout (target)

```
banlieue/
├── Makefile                          # NEW — docs targets only for now
├── docs/
│   ├── mkdocs.yml                    # NEW
│   ├── pyproject.toml                # NEW
│   ├── poetry.lock                   # NEW (generated locally)
│   ├── README.md                     # NEW — how to build/serve docs
│   ├── src/                          # NEW — all markdown lives here
│   │   ├── index.md
│   │   ├── installation/
│   │   ├── concepts/
│   │   ├── architecture/
│   │   ├── operations/               # stubs
│   │   ├── advanced/                 # stubs
│   │   ├── security/                 # stubs
│   │   ├── development/
│   │   ├── reference/
│   │   ├── images/
│   │   ├── stylesheets/
│   │   │   └── extra.css             # brand palette
│   │   ├── javascripts/
│   │   │   └── mermaid-init.js
│   │   ├── changelog.md              # symlink or include of .claude/CHANGELOG.md
│   │   └── license.md
│   └── site/                         # build output — gitignored
├── .github/workflows/docs.yaml       # NEW
└── (docs/adr/, docs/design/ remain as planned for future ADRs)
```

## Makefile targets (new file at repo root)

Mirror the 5-spot subset relevant to docs only. Cargo targets stay in-tree
via `cargo` directly until/unless we decide to add them here too.

```makefile
.PHONY: help docs docs-serve docs-rustdoc docs-clean docs-deploy

help: ## Show available targets
	@awk 'BEGIN{FS=":.*##"; printf "Usage: make <target>\n\nTargets:\n"} \
	     /^[a-zA-Z_-]+:.*##/{printf "  %-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

docs: ## Build site + rustdoc + CRD reference into docs/site/
	@cd docs && poetry install --no-root --quiet
	@cd docs && poetry run mkdocs build
	@cargo doc --no-deps --all-features --workspace
	@mkdir -p docs/site/rustdoc
	@cp -r target/doc/* docs/site/rustdoc/
	@cargo run -p banlieue-api --bin crdgen --features crdgen > docs/site/reference/crds.yaml || true

docs-serve: ## Live-reload docs server at http://localhost:8000
	@cd docs && poetry install --no-root --quiet
	@cd docs && poetry run mkdocs serve --livereload

docs-rustdoc: ## Build + open rustdoc only
	@cargo doc --no-deps --all-features --workspace --open

docs-clean: ## Remove build artifacts
	@rm -rf docs/site/ target/doc/
```

## CI workflow (`.github/workflows/docs.yaml`)

Drop the CALM jobs from 5-spot's workflow for now (banlieue doesn't have a
CALM architecture yet — that lands later if at all). Keep:

- `pull_request` + `push: main` triggers, gated on `docs/**`, `crates/**/*.rs`, `.github/workflows/docs.yaml`.
- `workflow_run: ["Build"]` trigger for deploy on release-only success.
- `concurrency:` group from 5-spot (cancel-in-progress except on release).
- Two jobs:
  1. **`build`** — installs Python 3.12 + Poetry, runs `make docs`, uploads `docs/site/` as a Pages artifact.
  2. **`deploy`** — gated on `workflow_run.event == 'release'` and `workflow_run.conclusion == 'success'`, uses `actions/deploy-pages@v4`.
- Same `permissions:` shape as 5-spot (`contents: read`, `pages: write`, `id-token: write`).

Pin all actions by commit SHA, matching the existing banlieue workflows
(`actions/checkout@de0fac…`, etc.).

## CRD reference integration

Two options, both deferred to a follow-up but worth noting now so the
`reference/` directory has a clear destination:

1. **Phase 1E (minimal):** `make docs` runs `crdgen` and dumps the multi-doc
   YAML at `docs/site/reference/crds.yaml`. The MkDocs page
   `reference/api.md` links to it as a raw download.
2. **Phase 4 (proper):** the planned `crddoc` binary emits per-CRD
   Markdown pages (one per kind) into `docs/src/reference/crds/` and
   MkDocs picks them up automatically. Until then, option (1) gives users
   *something* better than reading `deploy/crds/*.yaml`.

This phase ships option (1).

## Tasks

- [ ] Run `poetry init` inside `docs/` and pin the same versions as 5-spot's
      `pyproject.toml` (mkdocs 1.6+/<2.0, mkdocs-material ^9.5, plus the
      five plugins 5-spot uses).
- [ ] Copy `mkdocs.yml` from 5-spot; rewrite `site_name`, `site_url`,
      `repo_url`, `nav`. Keep theme, palette, features.
- [ ] Copy `src/stylesheets/extra.css`; substitute banlieue brand palette
      (see proposal above).
- [ ] Copy `src/javascripts/mermaid-init.js` verbatim.
- [ ] Author `src/index.md` (tagline + system overview + 3 quick-links).
- [ ] Author `src/installation/{quickstart,prerequisites,crds}.md` — CRD
      install instructions can use the existing `cargo run --bin crdgen`
      output piped to `kubectl apply`.
- [ ] Author `src/concepts/index.md` + one page per CR
      (`provider.md`, `vmclass.md`, `vmimage.md`, `virtualmachine.md`,
      `vsphere-machine.md`). Lean on the rustdoc comments already in
      `crates/banlieue-api/src/`.
- [ ] Stub `src/operations/`, `src/advanced/`, `src/security/` — one
      `index.md` per section saying "coming with Phase X".
- [ ] Author `src/development/{setup,building,testing,contributing}.md`.
- [ ] Add `src/reference/api.md` pointing at the generated `crds.yaml`
      plus a link to rustdoc.
- [ ] Add `src/changelog.md` (use mkdocs-include-markdown to inline
      `.claude/CHANGELOG.md` — or a symlink if the include plugin is
      excluded for simplicity).
- [ ] Add `src/license.md` — short text + link to `LICENSE`.
- [ ] Author `src/architecture/system.md` — Mermaid diagram of the CR
      graph (Provider ← VirtualMachine → VMClass/VMImage; VirtualMachine
      → VSphereMachine).
- [ ] Add root `Makefile` with the 5 targets above.
- [ ] Add `.github/workflows/docs.yaml`. Validate locally with `actionlint`.
- [ ] Add `docs/site/` and `target/doc/` to `.gitignore`.
- [ ] Enable GitHub Pages on `firestoned/banlieue` (source: GitHub
      Actions). One-time repo setting outside this checklist.
- [ ] Smoke test: `make docs-serve`, open the site, click through every
      nav entry, confirm no broken links or styling regressions.
- [ ] Smoke test: `make docs`, confirm `docs/site/rustdoc/` and
      `docs/site/reference/crds.yaml` are present.

## Open questions

- **Should the docs Makefile live at the repo root or under `docs/`?**
  5-spot puts everything in one root Makefile (which currently has
  many non-docs targets). banlieue has no Makefile yet. Default
  recommendation: root `Makefile` so `make docs` works from the
  repo root without `cd`.
- **Do we want `mkdocs-include-markdown` to pull in `.claude/CHANGELOG.md`,
  or should the changelog be its own file at `docs/src/changelog.md` and
  the `.claude/` one phase out?** The latter is cleaner long-term but
  requires migrating existing entries. Default: include-plugin for now.
- **Brand palette.** Proposed above; needs user sign-off before CSS is
  committed.

## Acceptance criteria

- `make docs-serve` renders the site locally with no MkDocs warnings.
- `make docs` produces `docs/site/index.html`, `docs/site/rustdoc/index.html`,
  and `docs/site/reference/crds.yaml`.
- The `Documentation` workflow runs on a PR that only changes a doc page,
  succeeds, and uploads the Pages artifact but does not deploy.
- A test release (or `workflow_dispatch`) triggers the deploy job and
  serves the site from `https://firestoned.github.io/banlieue/`.
- Brand palette renders identically in light/dark modes; no FOUC; mermaid
  diagrams render.

## Out-of-scope (deferred)

| Item | Goes to |
|---|---|
| `crddoc` binary that emits per-CRD Markdown pages | Phase 4 §4.1 |
| CALM architecture + Mermaid auto-render workflow | Future (banlieue may never need it) |
| Versioned docs via `mike` | Post-v1.0 |
| Translations / i18n | Never (out of project scope) |
| Custom Material plugins beyond the 5-spot set | As needed, per-PR |
