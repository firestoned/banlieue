# 0135: A claim chooses its member by strategy

- **Status:** Accepted
- **Date:** 2026-11-17
- **Implemented:** 2026-11-17 (the `spec.selection` field, the ranking in
  `claim_plan.rs`, the Provider read in `claim.rs`, the admission rule)
- **Proposed:** 2026-11-17
- **Deciders:** Erick Bourgeois
- **Amends:** [ADR-0132](0132-claim-selects-its-pool-by-label.md) Decision 4
  ("no preference between pools is added") and
  [ADR-0047](0047-virtualmachineclaim.md) Decision 4 (the member ranking).
- **Related:** Reuses the cordon of
  [ADR-0124](0124-unschedulable-providers.md) and the heartbeat of
  [ADR-0102](0102-provider-liveness-from-its-lease.md) as the scheduler judges them;
  resolves a member's `Provider` as
  [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md) Decision 1
  does.

## Context

A claim binds one Ready member, ranked in a single fixed order (ADR-0047
Decision 4, extended across pools by ADR-0132 Decision 4):

1. Ready and unclaimed only;
2. current image revision of the member's own pool before stale;
3. the member Ready longest;
4. member name, so two controllers reading one snapshot agree.

That order knows nothing about *where* a member runs. Every member already
records it: `status.scheduled.providerName` and
`status.scheduled.failureDomain` on the `VirtualMachine`. Ignoring it has
three costs.

- **Uneven depletion.** Claims drain whichever failure domain holds the
  oldest members. One domain can run dry while another still holds several
  Ready members, and a domain that has run dry is a domain whose loss takes
  every remaining sandbox with it.
- **Shared fate for one subject.** A subject that holds several sandboxes
  (one agent fanning out work, one user with several sessions) can receive
  all of them from one host. One host failure ends all of that subject's
  work at once.
- **Members on a host in trouble are handed out.** The scheduler stops
  placing *new* VMs on a cordoned `Provider` (ADR-0124) or one whose
  heartbeat is `Stale` (ADR-0102), but the claim reconciler never reads a
  `Provider`. A Ready member on a host being drained for maintenance, or on
  a host whose provider has stopped reporting, is offered exactly like any
  other. A claim is the moment a warm VM becomes someone's workload, so for
  the person receiving it that is a new placement in all but name.

ADR-0132 Decision 4 rejected "fullest pool first" on the grounds that the
longest-Ready member is the one idle expiry would reap next, so taking it
first wastes the least. That reasoning still holds for a claim that has no
opinion about placement. It does not hold for a claim that does: such a
claim is trading a little idle-expiry efficiency for availability, and that
trade is the claim author's to make.

A "least failed hardware" strategy was also considered. Nothing in the tree
records a hardware failure history: `MemberPhase` has no failed state, and
no counter exists on a pool or a `Provider`. Inventing that signal is its
own decision (where it lives, how it decays, who can inflate it), and is
left out of this one.

## Decision

1. **`spec.selection.strategy` chooses the ranking.** `spec.selection` is
   an optional struct on `VirtualMachineClaimSpec` with one field,
   `strategy`, an enum:
   - `LongestReady` (the default, and the value when `selection` is
     absent): today's order;
   - `MostAvailable`: take from the failure domain with the most Ready
     members (Decision 4);
   - `Spread`: take from the failure domain where the claim's subject holds
     the fewest members (Decision 5).

   A struct rather than a bare enum, so later knobs (a hard spread, a
   preferred domain) have somewhere to go without another top-level field.
   Every strategy is a fixed lexicographic order, never a weighted score: a
   score is hard to explain after the fact ("why did it pick that one?") and
   easy to make non-deterministic.

2. **A strategy only reorders; it never filters.** The full order is:
   1. Ready and unclaimed only (unchanged, the invariant the design rests
      on);
   2. current image revision of the member's own pool before stale
      (unchanged: this is how a rebuild retires an unpatched build, and no
      placement preference outranks a security rebuild);
   3. **host health** (Decision 3, every strategy, including the default);
   4. **the strategy's key** (Decisions 4 and 5; none for `LongestReady`);
   5. the member Ready longest;
   6. member name.

   No strategy ever leaves a claim `Pending` while a Ready member exists. A
   member on yesterday's image, or on a cordoned host, or in the busiest
   domain, still beats no sandbox, exactly as ADR-0047 already argues for
   stale revisions; its life is bounded by the claim's TTL.

3. **Members on a healthy host rank before members on an unhealthy one,
   under every strategy.** A host is unhealthy when its `Provider` is
   cordoned (`banlieue.io/unschedulable: "true"`, ADR-0124), when its
   heartbeat is `Stale` (ADR-0102), or when the member's `Provider` cannot
   be found. These are the scheduler's own predicates (`is_unschedulable`,
   `is_stale` in `reconciler/scheduler.rs`), shared with the claim
   reconciler rather than copied, so "unhealthy" means one thing in banlieue. The member's
   `Provider` is `status.scheduled.providerName`, resolved as ADR-0133
   Decision 1 resolves it: the claim's namespace first, then the configured
   provider namespace only if the claim's namespace holds no `Provider`.

   This is a ranking, not a filter, for the reason in Decision 2. It changes
   the default order, which is intended: no claim author asks for a
   sandbox on a host being drained.

4. **A failure domain is the pair `(providerName, failureDomain)`.**
   Failure-domain names are scoped to their `Provider`
   (`status.failureDomains[].name`), and two hosts commonly carry the same
   name (each Cloud Hypervisor or libvirt host's single domain), so the
   name alone would merge distinct hosts into one bucket. A member with no
   `status.scheduled` (which a Ready member should never be) is its own
   bucket and ranks after every member that has one.

   **`MostAvailable`** counts, per domain, the Ready unclaimed members of
   every matched pool, and prefers the domain with the most. The count comes
   from the member snapshot the claim already lists (ADR-0132 Decision 5),
   never from `pool.status.available`, which lags the members it summarises.
   Taking from the fullest domain keeps every domain's warm capacity level,
   so the loss of any one domain costs the fewest warm members.

5. **`Spread` counts the subject's existing members per domain, and
   prefers the domain where it holds the fewest.** A member belongs to the
   subject when it is `Claimed`, not `Deleting`, and its
   `banlieue.io/claim-subject-issuer` and `banlieue.io/claim-subject-id`
   annotations both equal the claim's `spec.subject`. The id is matched
   together with its issuer, never alone: ADR-0047 holds that an id without
   an issuer is ambiguous across identity providers, and two people with the
   same id at two issuers are not one subject. Ties on that count fall
   through to `MostAvailable`'s key, then the rest of the order, so for a
   subject holding nothing yet, `Spread` behaves as `MostAvailable`.

   Spread is **soft**. If every Ready member is in a domain the subject
   already occupies, the claim binds one of them; it never waits for a
   domain to free up. A hard spread (refuse rather than share a domain) is
   out of scope.

   Spread is **namespace-scoped**, as every claim lookup is (ADR-0132
   Decision 3). Members the same subject holds in other namespaces are not
   counted, because the claim reconciler has no reason to read them and
   should not start.

   Spread is **best-effort under concurrency.** Two claims for one subject
   reconciled from one snapshot rank identically and race for the same
   member; the bind's `resourceVersion` precondition lets exactly one win
   (ADR-0047 Decision 2). The loser requeues, re-lists, and then sees the
   winner's bind, so it spreads as intended. Only a stale cache on the
   retry can make two members share a domain, and that outcome is the soft
   fallback above, not a failure.

6. **The bound claim says what was chosen and why.** The `Bound`
   condition's message names the strategy and the domain the member came
   from (`provider/failureDomain`), and says when the member's host was
   unhealthy or the subject already held a member in that domain. No new
   status field: the member's own `status.scheduled` remains the record of
   where it runs, and the condition answers "why this one".

7. **`spec.selection` is immutable, by the existing admission policy.** It
   is read once, at pick time, like `poolRef` and `poolSelector`; the
   claim-subject `ValidatingAdmissionPolicy` refuses an edit to it, and
   setting or clearing it on UPDATE counts as an edit.

## Consequences

- **Wire compatible.** Every existing claim has no `spec.selection` and
  reconciles as `LongestReady`. The only behaviour change for them is
  Decision 3: a member on a cordoned or stale host is now taken last rather
  than in turn.
- **Not source compatible for Rust consumers that build a spec literally.**
  `VirtualMachineClaimSpec` has no `Default` (its subject and TTL are
  required), so a struct literal (mediatore's `sandbox.rs` builds one) stops
  compiling until it adds `selection: None`. Code that only reads a spec is
  unaffected. mediatore may expose the strategy to its callers; it does not
  have to.
- **A pick costs one or two `Provider` LISTs more** (the claim's namespace,
  then the provider namespace if the first is empty), only when there is
  more than one Ready candidate. The claim reconciler gains no new watch:
  a waiting claim waits for members, not for `Provider`s, and a `Provider`
  changing health does not re-rank a claim that is already bound. The
  controller's ClusterRole already lists `Provider`s for scheduling, so no
  RBAC grant is added.
- **`MostAvailable` and `Spread` give up some idle-expiry efficiency.**
  They may take a recently-Ready member while an older one in a fuller
  domain waits, so idle expiry (when the pool sets `maxIdleSeconds`) reaps
  a little more. That is the trade the claim author opted into.
- **The subject annotations gain a reader.** `Spread` reads the subject
  annotations the bind already writes; nothing new is recorded or exposed.
  Whoever can edit a member's annotations could make a subject look present
  in a domain and steer that subject's next claim elsewhere, but only
  within Ready members of its own namespace, and that principal can already
  delete the member outright. Recorded in the threat model as no new power.
- **Not done here**, each needing its own decision:
  - `LeastFailures`: a failure history per domain, kept in the API server,
    with decay, and an account of who can inflate it;
  - `MostHeadroom`: prefer the member whose host has the most free measured
    capacity (ADR-0128);
  - `Pack`: the opposite of `Spread`, co-locating one subject's members;
  - preferred domains, a hard spread, spread by zone (`banlieue.io/zone`)
    rather than by host and domain, and a per-namespace or per-pool default
    strategy.
