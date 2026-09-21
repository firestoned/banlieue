# 0047 — `VirtualMachineClaim`: bound once, released by deletion

- **Status:** Accepted
- **Date:** 2026-09-20
- **Proposed:** 2026-09-20
- **Deciders:** Erick Bourgeois
- **Notes:** Implemented and validated end to end against a real cluster and
  libvirt host — pool → warm domains → claim → release, with the released
  domain verified gone from the hypervisor
  (`crates/banlieue-provider-libvirt/tests/e2e_pool_claim.rs`). Decision 10's
  `subject`-pinning admission policy is **not** implemented and is recorded
  as an accepted risk in the threat model.
- **Related:** Completes [ADR-0046](0046-virtualmachinepool.md)
  (`VirtualMachinePool`), which maintains a warm set but has no way to hand
  one out. Depends on the member lifecycle in
  [ADR-0026](0026-vspheremachine-deletion-lifecycle.md) and
  [ADR-0050](0050-libvirtmachine-domain-lifecycle.md) — a claim's deletion
  guarantee is only as strong as theirs.

## Context

A `VirtualMachinePool` fills, self-heals and rolls, and none of that is
useful yet: nothing can take a member out of it. The pool is inert capacity.

The thing being handed out is not a VM in the ordinary sense. Roadmap 17's
premise is **one VM per identity, used once, then destroyed** — the VM *is*
the isolation boundary, so the moment a member has been exposed to one
subject it can never be given to another. That single constraint decides
almost everything below.

### What already holds

`pool_plan`'s invariants 1 and 2 (ADR-0046) already say a claimed member is
never deleted by the pool and never returns to the warm set. The pool
recognises a member as claimed by a label. So the pool side needs no change;
what is missing is the object that puts the label there.

### What a claim must survive

- **Two consumers asking at the same instant.** Whatever binds must be safe
  without a lock, because banlieue's controllers are leader-elected per
  *kind*, not globally, and a lock would be a new failure mode anyway.
- **A pool being deleted while a sandbox is in use.** Members are owned by
  the pool, so a naive delete would take live sandboxes with it.
- **The consumer going away.** A claim that is never released must not pin a
  VM forever.

## Decision

1. **A member is bound at most once in its life. Release is always
   deletion.** There is no unbind, no return-to-pool, and no code path that
   makes a claimed member claimable again. This is the whole point of the
   design and every decision below serves it.

2. **Binding is a JSON merge patch on the member carrying its
   `resourceVersion`.** The patch adds the claim label, the subject
   annotations, and re-parents `ownerReferences`. Two claims racing for one
   member produce exactly one success and one `409 Conflict`; the loser
   picks again.

   Optimistic concurrency, not a lock. The API server is already the
   serialisation point and it is the only one that cannot disagree with
   itself.

3. **`ownerReferences` move from pool to claim at bind time.** This is what
   lets a pool be deleted without destroying sandboxes that are in use —
   ADR-0046 Decision 10 names it as the reason ownership is per-member
   rather than by label alone.

   It also means a claimed member is garbage-collected with its claim, which
   is the behaviour we want: the claim is now the thing that owns the VM's
   lifetime.

4. **The claim controller picks the member, not the pool.** The pool does
   not know claims exist (ADR-0046 Decision 1) and must not learn. Pick
   order: current image revision first, then longest-Ready, then name.

   A stale-revision member is still handed out when nothing fresher is
   ready — an available sandbox on yesterday's image beats no sandbox, and
   its life is bounded by the TTL anyway.

5. **`ttlSeconds` is mandatory.** No default. A claim without a deadline is
   a leaked VM waiting to happen, and the right value is entirely a property
   of the workload — there is no number banlieue could pick that is not
   wrong for someone.

   At expiry the member is deleted, then the claim. Expiry is not a grace
   period: the VM goes whether or not the consumer is finished.

6. **A finalizer holds the claim until the member is gone from the API
   server.** Through the machine finalizers (ADR-0026, ADR-0050) that means
   gone from the hypervisor. **"Claim deleted" must mean "sandbox
   destroyed"** — if the claim object could disappear first, the guarantee
   would be a lie in exactly the case it matters.

7. **A bound member that vanishes makes the claim `Failed`, terminally.** It
   is never silently rebound to another member. A consumer holding a claim
   believes it is talking to one specific VM; handing it a different one
   without saying so would be worse than failing.

8. **`status.nonce`: 128 random bits, generated at bind, not secret.** The
   consumer's attestation exchange echoes it so a TPM quote cannot be
   replayed across claims. Needs `getrandom` in
   `[workspace.dependencies]` — the first new dependency this roadmap adds.

9. **The claim records `subject.issuer` + `subject.id`, and never a token.**
   Opaque to banlieue: recorded, mirrored onto the member as annotations for
   audit, never interpreted.

   Credentials do not travel this way. `guestinfo` is readable by anyone
   with hypervisor read access *and* by processes inside the guest, so a
   token placed there is a token disclosed. Delivering it is the consumer's
   job, over the attested channel phase C builds.

10. **Who may create a claim is a Kubernetes RBAC question**, plus a
    `ValidatingAdmissionPolicy` alongside ADR-0007's that pins `subject` to
    the authenticated caller for anyone who is not the broker service
    account. Otherwise any claim author could attribute a sandbox to someone
    else, and the audit trail would be fiction.

11. **`Pending` is unbounded and reported, not timed out.** A claim with no
    member available waits, because the pool may simply be filling — that is
    the normal state after a burst. It publishes `Bound=False` with a reason
    naming the pool's own state, so "no capacity" and "pool misconfigured"
    are distinguishable without reading two objects.

## Consequences

- A pool becomes usable. This is the last piece between "a warm set exists"
  and "a consumer gets a VM in seconds."
- **Claims inherit ADR-0046's readiness caveat.** A claim binds what the
  pool considers available, which depends on `spec.readiness`. Until
  ADR-0043 lands, that can only be `InfrastructureReady` — correct for
  `Immediate` images, wrong for `Deferred` ones. So claims against a
  sandbox pool are not trustworthy until A2, and this ADR does not change
  that.
- Deleting a claim deletes a VM, and blocks until it is really gone. That is
  the intended contract and also means a claim deletion can take as long as
  a hypervisor takes.
- `status.tpmEndorsementCertificates` is specified here but stays empty
  until ADR-0045 publishes them. Mirroring an absent field costs nothing and
  avoids a second CRD change later.
- One new dependency (`getrandom`), for decision 8.
- Nothing about a claim is provider-specific. A claim binds a
  `VirtualMachine`; which backend realises it is invisible here, exactly as
  it is to the pool.
