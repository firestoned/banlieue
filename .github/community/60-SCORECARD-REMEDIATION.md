<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 60 — OSSF Scorecard remediation

Status as of **2026-09-07**. Scorecard runs weekly via
[`.github/workflows/scorecard.yaml`](../workflows/scorecard.yaml) and reports into
the Code Scanning dashboard. Four checks cannot be fixed from the repo tree —
they are GitHub *settings* or an external registration. This file records what
to click and, where a check is deliberately capped, why.

| Alert | Check | Score | Fixable in-repo? |
| --- | --- | --- | --- |
| [#1](https://github.com/firestoned/banlieue/security/code-scanning/1) | Branch-Protection | 0 | No — repo settings |
| [#5](https://github.com/firestoned/banlieue/security/code-scanning/5) | Code-Review | 0 | No — process |
| [#7](https://github.com/firestoned/banlieue/security/code-scanning/7) | CII-Best-Practices | 0 | No — external registration |
| [#21](https://github.com/firestoned/banlieue/security/code-scanning/21) | Pinned-Dependencies | 9 | **No — permanently capped, see below** |
| [#33](https://github.com/firestoned/banlieue/security/code-scanning/33) | Vulnerabilities | 9 | ✅ Fixed 2026-09-07 |

---

## #1 — Branch-Protection (score 0)

> `branch protection not enabled for branch 'main'`

**Settings → Rules → Rulesets → New branch ruleset**, targeting `main`:

- **Restrict deletions** ✅
- **Block force pushes** ✅
- **Require a pull request before merging** ✅
  - Required approvals: **1** (this is also what fixes #5 — see the caveat there)
  - Dismiss stale approvals on push ✅
  - Require review of the most recent reviewable push ✅
- **Require status checks to pass** ✅ — at minimum: `🎨 Check Formatting`,
  `📎 Clippy`, `🧪 Test`, `🧪 cargo-deny (License + Advisory + Sources)`,
  `🔍 CodeQL - rust`, `🔍 CodeQL - actions`
- **Require branches to be up to date before merging** ✅
- **Require signed commits** ✅ — the repo already verifies these in CI
  (`🔐 Verify Signed Commits`), so enforcing at the branch closes the gap where
  an unsigned commit lands and only fails after the fact.

Scorecard reads this through the branch-protection API, so a **ruleset** (not the
legacy "branch protection rule") is what it will see on a modern repo. Both work;
rulesets are what GitHub steers you to now.

> **Note on `Restrict who can push` / bypass lists:** do *not* add yourself as a
> bypass actor. Scorecard specifically checks whether admins are exempt, and an
> admin bypass keeps the score down even with everything above enabled.

## #5 — Code-Review (score 0)

> `Found 0/21 approved changesets`

Scorecard looks back over recent merged PRs and counts how many carried an
approving review. Every one of the last 21 was self-merged, so the score is 0.

**This one cannot be back-filled** — it is computed from merge history, so it
only improves as *new* PRs land with approvals. Requiring 1 approval in the
ruleset above starts that clock.

The honest problem for a single-maintainer repo: you cannot approve your own PR.
The options, in order of how much they actually buy:

1. **Add a second reviewer** (a collaborator with write access). Genuinely fixes
   both the score and the underlying risk. Best answer if there is anyone.
2. **Use a review bot** that posts an approving review on green CI. This games
   the metric without adding a second pair of eyes — it will raise the score and
   will not make the codebase safer. Only worth it if the badge matters to you
   more than the signal does.
3. **Accept the 0.** Defensible for a solo pre-1.0 project, and arguably more
   honest than option 2. The other checks still carry the overall score.

## #7 — CII-Best-Practices (score 0)

> `no effort to earn an OpenSSF best practices badge detected`

Register the project at <https://www.bestpractices.dev/> (formerly CII), then add
the badge to `README.md`. Scorecard detects the badge by looking the repo URL up
in the OpenSSF API, so the registration is what counts — the README badge is for
humans.

The **passing** tier is mostly things banlieue already does (public VCS, an OSI
licence, a documented release process, static analysis in CI, `SECURITY.md`).
Budget an hour for the questionnaire; a large share of the answers already have
evidence in this repo.

## #21 — Pinned-Dependencies (score 9, permanently)

> `third-party GitHubAction not pinned by hash`

**Do not "fix" this.** The single unpinned reference is the SLSA provenance
generator in [`build.yaml`](../workflows/build.yaml):

```yaml
uses: slsa-framework/slsa-github-generator/.github/workflows/generator_generic_slsa3.yml@v2.1.0
```

That workflow resolves the ref it was *called* with as its own `BUILDER_REF` and
rejects anything that is not a release tag:

```
Invalid ref: <sha>. Expected ref of the form refs/tags/vX.Y.Z
```

A blanket pin-everything pass (`041f34b`, 2026-08-13) digest-pinned it along with
every other action and broke the provenance job on every push to `main` until it
was reverted on 2026-09-07. The trade is SHA-pinning **or** SLSA provenance, and
provenance is worth more than the tenth of a point. Every other action in the
repo stays digest-pinned.

Expect this check to sit at 9/10 indefinitely. That is the intended state.

## #33 — Vulnerabilities — ✅ fixed 2026-09-07

> `Project is vulnerable to: GHSA-w5hq-g745-h8pq`

`uuid@9.0.1` (missing buffer bounds check in v3/v5/v6), pulled transitively as
`linkinator@6.1.2 → gaxios → uuid@^9`. The `^9` ceiling meant no override could
reach the fixed `11.1.1`. linkinator 8 replaced gaxios with undici and pulls
neither package, so `.github/tools/linkinator` was bumped to `8.1.0` — which also
needs Node >= 22, satisfied by the runner default (noted in `docs.yaml`).

`npm audit` is clean in both `.github/tools/*` lockfiles.
