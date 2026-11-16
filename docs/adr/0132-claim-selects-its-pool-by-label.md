# 0132: A claim may select its pool by label

- **Status:** Accepted
- **Date:** 2026-11-16
- **Proposed:** 2026-11-16
- **Deciders:** Erick Bourgeois
- **Related:** Amends [ADR-0047](0047-virtualmachineclaim.md) (Decisions 4
  and 11: how a claim finds a member, and how it reports waiting). Follows
  the `providerName` / `providerSelector` shape of
  [ADR-0127](0127-move-a-cloud-hypervisor-vm-between-hosts-in-a-zone.md)'s
  `MoveTarget`, and the "never all by default" rule of
  [ADR-0130](0130-namespace-selector-multi-tenancy.md) and
  [ADR-0131](0131-operator-and-provider-namespace-scope.md).

## Context

A `VirtualMachineClaim` names exactly one pool: `spec.poolRef` is a required
`LocalObjectReference`. A namespace with several interchangeable pools (one
per host, per zone, or per backend, all serving the same image) forces the
consumer to choose a pool before it claims. The consumer usually has no
basis for that choice: it does not see capacity, and the pool it picks may
be empty while its neighbour holds three Ready members. The claim then sits
`Pending` with `NoMemberAvailable` while a sandbox it could have used is
idle next door.

The claim controller already has everything it needs to choose better. It
lists the members of the named pool and ranks them (ADR-0047 Decision 4:
Ready only, current image revision first, longest-Ready next, then name).
Ranking across several pools is the same comparison over a larger set.

Two shapes were proposed:

- `spec.poolRef.labelSelector`: keep one field and give the reference a
  selector inside it.
- `spec.poolSelector`: a sibling of `spec.poolRef`.

Kubernetes uses siblings for "this one" versus "any matching": a Pod's
`nodeName` and `nodeSelector`, a PersistentVolumeClaim's `volumeName` and
`selector`. banlieue already does the same in `MoveTarget`
(`providerName` / `providerSelector`, exactly one, enforced by CEL).
`LocalObjectReference` is a shared type used across the API, so adding a
selector to it would put a selector on every reference that has no use for
one, and a reference whose `name` is optional is no longer a reference.

## Decision

1. **`spec.poolSelector` is added beside `spec.poolRef`, and exactly one of
   the two is set.** `poolRef` becomes optional. The rule is CEL on the spec
   (`has(self.poolRef) != has(self.poolSelector)`), so the API server
   rejects a claim with neither or both, before any controller sees it.
   `poolSelector` is banlieue's `LabelSelector` (`matchLabels`,
   `matchExpressions`), matched against `VirtualMachinePool` labels.

2. **"Any pool" is written down, never implied.** `poolSelector: {}` is the
   empty selector, which by Kubernetes convention matches every pool, so a
   claim on any pool in its namespace says so in its spec. A claim with
   neither field is refused rather than read as "any": the same reasoning as
   ADR-0130 and ADR-0131, where an omitted scope never widens to everything.

3. **A selector never leaves the claim's namespace.** Pools are matched in
   the claim's own namespace only, exactly as `poolRef` is resolved. Who
   may claim is still the namespace RBAC question of ADR-0047 Decision 10;
   a selector can reach no pool a `poolRef` in the same namespace could not
   already name.

4. **Ranking runs over the members of every matched pool at once.** The
   order is ADR-0047 Decision 4's, unchanged, with "current revision"
   judged against each member's *own* pool's `status.imageRevision`:
   - Ready and unclaimed only;
   - current revision of its own pool before stale;
   - longest-Ready first;
   - then member name. Member names are unique in a namespace, so two
     controllers reading one snapshot still choose the same member.

   No preference between pools is added (for instance "fullest pool
   first"). The member that has been Ready longest is still the one idle
   expiry would otherwise reap next, which was the reason for that rule.

5. **The member list is the record of a bind, across all pools.**
   ADR-0047 Decision 2 (amended 2026-09-28) finishes a bind the claim
   already made before picking again. That check now looks at every pool
   member in the namespace, not only those of the matched pools, so a pool
   relabelled between the bind and the claim's own status patch cannot make
   the claim take a second member.

6. **`status.poolRef` records the pool the bound member came from.** With a
   selector, the spec no longer says which pool supplied the VM, and
   "which pool was this from" is the first question asked of a bound claim.
   It is set at bind time from the member's `banlieue.io/pool` label, and
   backfilled on hold for a claim bound before this field existed. The
   `Pool` print column reads `.status.poolRef.name`, so `kubectl get vmclaim`
   shows the pool for both kinds of claim once bound (and is empty while
   `Pending`).

7. **Waiting reports name what was searched.** ADR-0047 Decision 11 stands:
   `Pending` is unbounded and says why. The reasons become:
   - `PoolNotFound` (unchanged): `poolRef` names a pool that does not exist;
   - `NoPoolMatches` (new): `poolSelector` matches no pool in the namespace;
   - `NoMemberAvailable` (unchanged): one or more pools matched and none
     has a Ready, unclaimed member. The message repeats each pool's own
     `Warm` diagnosis, up to five pools, then says how many more there are.

8. **Both fields are immutable, by the existing admission policy.** The
   claim-subject `ValidatingAdmissionPolicy` already refuses an edit to
   `poolRef` because the field is read once, at pick time. The same rule now
   covers `poolSelector`, and setting or clearing either on UPDATE counts as
   an edit.

## Consequences

- **Wire compatible for existing claims.** Every claim written so far has
  `poolRef` and no `poolSelector`, which is valid under the new schema and
  reconciles exactly as before.
- **Not source compatible for Rust consumers.** `spec.pool_ref` becomes
  `Option<LocalObjectReference>`, so code that reads `spec.pool_ref.name`
  (mediatore) stops compiling until it handles the selector case. Accepted:
  the consumer has to decide what to show for a selector claim anyway, and
  `status.pool_ref` is the field that answers it after binding.
- **A pick costs one pool LIST and one member LIST**, both in the claim's
  namespace, where it used to be one pool GET and one member LIST. The
  member LIST is wider (every pool member, not one pool's), which is what
  Decision 5 needs.
- **A pool's labels now steer claims.** Whoever may edit a pool's labels can
  move it into or out of a selector. That principal can already edit the
  pool's template and image, which is strictly more, so this adds no new
  power; it is recorded in the threat model as such.
- **Not done here.** Selecting pools across namespaces, weighting pools, and
  preferring a pool by locality are out of scope. Each would need its own
  decision, and the first would also need an authorization model that a
  namespaced claim does not have.
