# 0036 — Live migration: phased approach, same-class first

## Status

Proposed — 2026-08-24. Depends on [ADR-0035](0035-virtualmachine-watches-provider-and-vmclass.md)
(placement drift is now detected promptly, not just on the next poll).
Does not change `crates/banlieue-controller/src/reconciler/migration.rs`
today — this ADR records the phasing decision and scopes Phase A; no code
lands with it. Execution plan: `~/dev/roadmaps/banlieue/51-LIVE-MIGRATION.md`.

## Context

`migration.rs`'s own doc comment already states the current design plainly:

> "Migration sub-loop — recreate-only path for Phase 1A iteration 3 ...
> `MigrationPolicy::Automatic` → surface `PlacementValid=False` and
> recreate (delete the old infra CR; the next reconcile creates a fresh
> one with the new placement). Live migration is Phase 2 work."
>
> "Live migration semantics differ across providers (vSphere has vMotion,
> Proxmox has live migration over shared storage, libvirt has no live
> migration in v1). Faking a uniform live-migration contract would leak
> per-backend behaviour into the user-visible status — exactly what the
> abstraction principle forbids."

That reasoning for deferring live migration originally was sound and
remains sound. What's changed: ADR-0035 makes placement drift detection
*prompt* — a `Provider` label edit now re-triggers scheduling immediately
instead of waiting out a poll interval. That's strictly better for
correctness, but it also means `MigrationAction::Recreate` — destroy the
backend VM, rebuild from scratch, zero data/IP/state preservation — can
now fire considerably sooner and more often than before, on
`migrationPolicy=Automatic`. The gap between "we detect drift promptly"
and "we handle drift gracefully" is now more consequential in practice
than it was when both were equally slow.

Two requests converged into this ADR: (1) the controller should react to
placement-affecting changes (ADR-0035, already done), and (2) when
placement legitimately needs to change (a Provider's labels moved a VM out
of scope, a failure domain went away, a genuinely better placement is now
available), the response shouldn't always be destroy-and-rebuild.

Confirmed while researching this: **no shared abstraction exists across
provider classes today beyond the CAPI InfraMachine status shape**
(conditions, addresses, power state). `VSphereMachineSpec` has no concept
of a portable disk reference — `disks` always clone fresh from a template.
Only vSphere has an infra CRD implemented at all
(`crates/banlieue-api/src/infrastructure/`); Proxmox and libvirt are
scaffold-only. There is nothing to build a cross-provider-class migration
path on top of yet — no export format, no transfer mechanism, no shared
storage assumption that would even hold across classes.

## Decision

1. **Split "migrate" into two explicitly different problems, not one
   feature with two implementations.**

   - **Same-class migration** (Phase A): source and destination
     `Provider`s share a `providerClassRef` (e.g. two vSphere `Provider`s,
     or the same `Provider`'s two failure domains). Each provider class
     defines its own native mechanism where one exists (vSphere:
     `RelocateVM_Task`/vMotion-equivalent, shared or cross-datastore;
     Proxmox: its own live-migration API over shared storage). This is
     provider-specific, bounded, and buildable per class independently —
     vSphere Phase A doesn't block on Proxmox or libvirt ever supporting
     it.
   - **Cross-class migration** (Phase B): source and destination
     `Provider`s have *different* `providerClassRef`s (vsphere→libvirt,
     etc.). This needs a portable disk artifact (export/convert/import —
     e.g. OVF or `qemu-img` conversion), no shared-storage assumption, and
     a transfer mechanism between two systems that share nothing today.
     Explicitly **out of scope for this ADR** — there is no groundwork to
     scope it against yet (see Context). A future ADR should only attempt
     to design Phase B once Phase A exists and this codebase has an actual
     "portable disk artifact" concept to point at, not before.

2. **`MigrationAction::Recreate` remains the only mechanism until Phase A
   ships**, but its user-visible framing should be honest about what it
   actually does. Today `PlacementValid=False` + delete-and-rebuild is
   indistinguishable, from `VirtualMachine.status`, from a hypothetical
   future graceful migration — an operator watching conditions can't tell
   which one is about to happen. A follow-up (tracked in the roadmap, not
   this ADR) should give `Recreate` its own explicit condition
   reason (something like `RecreateRequired`, distinct from a future
   `Migrating`) so "this VM is about to go down and come back on new
   infra" is visible before it happens, not inferred after the fact.

3. **Reserve the `Migrating` condition type for real migration only.**
   `VirtualMachineStatus.conditions`'s doc comment already lists an
   optional `Migrating` type that nothing sets today. Keep it reserved
   exclusively for Phase A/B once built — do not repurpose it for
   `Recreate` (see #2); conflating "recreating" and "migrating" in status
   is exactly the abstraction leak the original recreate-only decision was
   trying to avoid, just moved from behavior into status semantics
   instead.

4. **Phase A is a per-provider-class capability, not a core-controller
   one.** `banlieue-controller` only needs to know "can this specific
   `Provider` pair migrate in place, yes/no" (likely a new
   `Provider.status` capability flag or a trait-like contract each infra
   CRD satisfies) — it should never contain vSphere- or Proxmox-specific
   migration logic itself, matching the existing "main controller never
   talks to a backend" non-negotiable.

## Consequences

- No code changes yet — this ADR exists to make the phasing decision
  explicit and prevent "live migration" from being treated as one
  undifferentiated task that either blocks on the hardest case
  (cross-class) or gets built ad hoc per provider without a shared
  contract.
- Same-class migration (Phase A) is buildable incrementally, one provider
  class at a time, starting with vSphere (the only class with an infra CRD
  today) — does not require Proxmox/libvirt to exist first.
- Cross-class migration (Phase B) remains explicitly unscoped. Anyone
  picking this up must design the portable-disk-artifact contract first;
  that is real, hard, currently-nonexistent groundwork, not an
  implementation detail of "the same feature, harder."
- Until Phase A ships, `migrationPolicy=Automatic` continues to mean
  "destroy and rebuild," and that should be made more visible in status
  (Decision #2) rather than implied to be something gentler.
- See `~/dev/roadmaps/banlieue/51-LIVE-MIGRATION.md` for the execution plan
  once someone picks Phase A up.
