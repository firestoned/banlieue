# Threat Modeling

> **After implementing an ADR, do a full pass over the threat model.**
> `docs/src/security/threat-model.md` is a living document under Architecture
> Driven Development. It is the **last step of the ADD cycle**, after docs —
> and an ADR is not "done" until it has been done.

## The rule

When the implementation of an ADR is complete (code written, tests green,
CHANGELOG and `docs/src/` updated), make a **full pass** over
`docs/src/security/threat-model.md` before declaring the task finished.

Full pass means walking **every** section, not appending a row to the one
table that obviously changed. An ADR that adds a controller also adds an
actor, probably an identity, possibly a trust boundary, and may invalidate an
accepted risk recorded three sections away.

## Why

banlieue's threat model states a **posture**: "here is what we defend, from
whom, and with which control in which file." Its header asserts a specific
claim —

```
Status: Living document. Last full pass YYYY-MM-DD, against the
architecture defined by ADR-0001 … ADR-NNNN.
```

Every merged ADR that isn't reflected makes that claim false. A stale threat
model is worse than no threat model: an absent one prompts analysis, while a
stale one asserts a posture nobody has actually checked and is trusted anyway.
The 2026-07-31 point-in-time security review going stale behind 30 subsequent
ADRs is exactly the failure this rule exists to prevent from recurring.

## Trigger questions

Run the ADR against each of these. Any **yes** means that section changes:

| Question | Section to revisit |
| --- | --- |
| New binary, crate, controller, provider, or Job? | §2 Components, §4 Actors |
| New CRD, contract, or field carrying user-controlled data? | §3 Assets, §6 |
| New credential, Secret read, token, or cloud-config path? | §3 Assets, TB-2 |
| New identity, ServiceAccount, RBAC grant, or admission policy? | §6, §7 |
| New namespace, or a change of PSA level? | §5 boundaries, TB-3 |
| New call out to a hypervisor, registry, or external API? | TB-4, TB-6 |
| Anything written to shared storage (datastore, ISO, disk artifact)? | TB-5 |
| New external dependency in the boot path? | TB-6, §3 |
| Does data now cross a boundary it didn't before — or is there a **new** boundary? | §5 (incl. the ASCII diagram), §6 |
| Does it weaken, strengthen, or invalidate a recorded accepted risk? | §8 |
| Does it break an assumption in §7 (e.g. single-tenant) or §9? | §7, §9 |

## Requirements for the pass

1. **Every new or changed threat maps to a concrete control** that exists in
   `deploy/` or `crates/` — cite the actual file, the way the existing tables
   do. A threat with no control is not a table row: it is either
   - an entry in **§8 Accepted risks** with an explicit *Revisit when*, or
   - a finding to fix **before** the ADR counts as implemented.

   Never write a control that does not exist yet as though it does.

2. **Classify with STRIDE**, per boundary, matching the existing §6 format.

3. **Update the ASCII trust-boundary diagram in §5** when components or
   boundaries change. A new component missing from the diagram is a missed
   pass, not a cosmetic omission.

4. **Bump the header stamp** — the date *and* the ADR range:
   `Last full pass 2026-09-09, against … ADR-0001 … ADR-0041`.
   **This is the deliverable.** An unchanged stamp means the pass did not
   happen, regardless of what else was edited.

5. **"No change" is a valid outcome** — but it is a *conclusion*, not a skip.
   Bump the stamp anyway and record it in `.claude/CHANGELOG.md`
   ("threat model pass: no boundary changes; stamp advanced to ADR-NNNN").

6. **Never record a specific unremediated vulnerability here.** This file is
   public. Findings go through
   [private vulnerability reporting](https://github.com/firestoned/banlieue/security/advisories/new),
   per the document's own header and `SECURITY.md`.

7. **No real infrastructure identifiers** — `rules/no-real-infrastructure.md`
   applies here like everywhere else. Threat models attract concrete
   hostnames; use the placeholder table.

## Scope

**Full pass required** for any ADR that reached implementation — the same set
of changes that required an ADR in the first place (`rules/architecture-driven-development.md`).

**Not required** for TDD-only changes (typos, isolated bugfixes, mechanical
refactors) — those never had an ADR. But if a "trivial" fix turns out to
change who can reach what, it wasn't trivial: write the ADR, then do the pass.

## Checklist

- [ ] All 10 sections of `docs/src/security/threat-model.md` walked, not just the obvious one
- [ ] Every trigger question above answered against this ADR
- [ ] New/changed threats classified with STRIDE and mapped to a real file in `deploy/` or `crates/`
- [ ] Uncontrolled threats either fixed or recorded in §8 with a *Revisit when*
- [ ] §5 ASCII diagram matches reality
- [ ] Header stamp bumped: date **and** ADR range
- [ ] `.claude/CHANGELOG.md` records the pass (with `**Author:**`)
- [ ] No unremediated finding and no real infrastructure identifier committed
