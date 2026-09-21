# Guide: VirtualMachine Claims

Take one warm VM out of a pool, for one identity, once — and destroy it when
you are done.

A `VirtualMachineClaim` is the only way a member ever leaves a
`VirtualMachinePool`
([ADR-0047](https://github.com/firestoned/banlieue/blob/main/docs/adr/0047-virtualmachineclaim.md)).
Without one, a pool is inert capacity: it fills, self-heals and rolls, and
nothing can consume it.

!!! tip "Prefer diagrams?"
    [The Claim Flow, End to End](../concepts/virtualmachine-claim-flow.md) draws this whole path —
    the `kubelogin` OIDC flow that establishes who you are, admission, the
    bind race, the state machine, release, and the proposed credential
    handshake — as eleven Mermaid diagrams.

## The rule everything else follows from

**A member is bound at most once in its life. Release is always deletion.**

There is no unbind, no return-to-pool, and no code path that makes a claimed
member claimable again. The VM *is* the isolation boundary between subjects,
so the moment a member has been exposed to one subject it can never be given
to another.

Every other behaviour on this page is a consequence of that one sentence.

## Quick start

`examples/19-virtualmachineclaim.yaml` is a complete claim against the pool
in `examples/18-virtualmachinepool.yaml`.

```sh
kubectl apply -f examples/18-virtualmachinepool.yaml
kubectl apply -f examples/19-virtualmachineclaim.yaml
kubectl get vmclaim -n banlieue-system -w
```

```text
NAME                POOL           VM                   PHASE     EXPIRES
sandbox-for-alice   sandbox-pool                        Pending
sandbox-for-alice   sandbox-pool   sandbox-pool-9dmtx   Bound     2026-09-20T14:17:11Z
```

Once `Bound`, everything a consumer needs is in one GET:

```yaml
status:
  phase: Bound
  virtualMachineRef: { name: sandbox-pool-9dmtx }
  addresses:
    - type: InternalIP
      address: 192.0.2.17
  nonce: 7f3a91c2e45b8d06a1f4c9e2b7d05386
  boundAt: "2026-09-20T14:02:11Z"
  expiresAt: "2026-09-20T14:17:11Z"
```

## `spec.subject` is an identifier, never a credential

This is the one field that can be misused in a way nothing will warn you
about.

```yaml
subject:
  issuer: https://issuer.example.com   # who authenticated the subject
  id: 0c3b7f2e-1d4a-4b6c-9e8f-5a2d1c0b9e7f   # the `sub` / `oid` claim
```

banlieue records it, mirrors it onto the member as annotations so "who was
this VM for" survives on the object an operator actually looks at, and never
interprets it.

!!! danger "Never put a token, password or session key in `subject`"
    A claim is a plain API object. Anything written here is readable by
    every reader of the namespace, is copied onto the member, and lands in
    the API server's audit log. A credential placed here is a credential
    disclosed.

    Delivering the subject's token to the guest is the **consumer's** job,
    over its own attested channel — using `status.nonce` to tie that session
    to this claim.

### What `status.nonce` is for

128 random bits, generated at bind time, and **not secret**. Its only job is
to be unpredictable to a guest that has not been told it, so an attestation
quote produced by this VM cannot be replayed against a different claim.

It is drawn from the OS CSPRNG, never derived from the claim's UID, name or
the clock — all three of which a guest could guess.

## `ttlSeconds` is mandatory, and it is a deadline

No default, on purpose. A claim without a deadline is a leaked VM waiting to
happen, and there is no number banlieue could pick that is not wrong for
somebody's workload.

Two properties worth knowing:

- **It runs from binding, not creation.** A claim that waited ten minutes
  for capacity still gets its full TTL.
- **It is not a grace period.** At expiry the member is deleted whether or
  not the consumer is finished with it. Then the claim is deleted too, so an
  expired hold cannot be mistaken for a live one.
- **`kubectl`'s `EXPIRES` column is the absolute deadline**, not a countdown.
  It is a `string` printer column on purpose: `kubectl` renders a `date`
  column as time *since* the timestamp, which for a deadline in the future is
  negative and prints `<invalid>`. `AGE` beside it is a real `date` column,
  and correctly so — that one looks backwards, this one looks forwards.

## Phases

```text
Pending  ──▶  Bound  ──▶  Releasing  ──▶  (gone)
                │
                └──▶  Failed   (terminal)
```

| Phase | Means |
| --- | --- |
| `Pending` | No Ready, unclaimed member yet. Retries, unboundedly. |
| `Bound` | A member is held. `status.virtualMachineRef` names it. |
| `Releasing` | Deleted or expired; the member is being destroyed. |
| `Failed` | The bound member vanished. **Terminal.** |

Nothing moves backwards. In particular a `Failed` claim is **never rebound**
to a different member: a consumer holding that claim believes it is talking
to one specific VM, and silently handing it another would be worse than
failing.

### `Pending` does not time out

A claim with no member available waits, because the pool may simply be
filling — which is the normal state after a burst. Rather than guessing, the
claim reports the pool's own diagnosis:

```text
Bound=False   reason=NoMemberAvailable
message: no Ready, unclaimed member in pool sandbox-pool
         (pool: ReadinessSignalAbsent: no member has published the
          "GuestReady" condition)
```

That parenthesis is the point. "The pool is busy" and "the pool will never
warm" look identical from a claim, and the second one needs a human. A
missing pool is reported separately:

```text
Bound=False   reason=PoolNotFound
message: pool sandbox-pool does not exist
```

## Which member you get

In order: **current image revision**, then **longest Ready**, then name.

- *Current revision first*, so a rebuild that was meant to retire an
  unpatched build actually does.
- *Longest Ready next*, so the member closest to being reaped for staleness
  is the one handed out and idle expiry rarely has to fire.
- *Name last*, purely so the choice is deterministic.

A **stale-revision member is still handed out** when nothing fresher is
Ready. An available sandbox on yesterday's image beats no sandbox, and its
life is bounded by the TTL anyway — refusing would mean a consumer waits out
a full install during exactly the rollout the pool exists to hide.

## What happens when two consumers race

Nothing special, and deliberately so. Binding is a merge patch on the member
carrying its `resourceVersion`, so if anyone — including another claim —
wrote that member since the controller listed it, the patch is rejected with
`409 Conflict` and the loser simply picks again.

Optimistic concurrency, no lock, no lease. The API server is already the
serialisation point and it is the only participant that cannot disagree with
itself.

## Deleting a claim destroys the VM

```sh
kubectl delete vmclaim sandbox-for-alice -n banlieue-system
```

This **blocks until the VM is really gone from the hypervisor**, not until
the delete is issued. The claim holds a finalizer
(`banlieue.io/claim-protection`) until its member is gone from the API
server, and the member's own finalizer holds *it* until the backend VM is
destroyed
([ADR-0026](https://github.com/firestoned/banlieue/blob/main/docs/adr/0026-vspheremachine-deletion-lifecycle.md),
[ADR-0050](https://github.com/firestoned/banlieue/blob/main/docs/adr/0050-libvirtmachine-domain-lifecycle.md)).

So "claim deleted" means "sandbox destroyed", transitively. A claim deletion
can therefore take as long as a hypervisor takes — that is the contract
working, not a hang.

The pool refills on its own afterwards.

### Deleting the pool does not kill claimed VMs

At bind time the member's `ownerReferences` are **re-parented from the pool
to the claim**. A claimed member lives and dies with its claim, so deleting
the pool underneath a live sandbox garbage-collects only the warm, unclaimed
members.

## Who may create a claim

Ordinary Kubernetes RBAC on `virtualmachineclaims`. The controller itself
deliberately has **no `create` and no `update`** on them: minting a claim
attributes a sandbox to a named subject, and a controller that could do that
could forge the audit trail.

`subject` **is** enforced against the caller, by
`deploy/admission/virtualmachineclaim-subject-authorization.yaml`:

- `spec.subject.id` must equal the authenticated username;
- `spec.subject.issuer` must be in an operator-maintained allowlist;
- `spec` is immutable, so the check cannot be undone by patching the claim
  afterwards.

```text
$ kubectl apply -f claim-for-someone-else.yaml
Error from server (Forbidden): ... denied request: spec.subject.id is
alice-uuid but you are authenticated as bob. A claim records who a sandbox
VM was handed to, so it may only name its own creator ...
```

A **broker** — a service account whose job is handing sandboxes to other
people — is exempt from the id check. Add its username to the `brokers` list
in ConfigMap `banlieue-claim-subject-policy`.

!!! warning "Two things the policy cannot do"
    **It does not verify the issuer.** The API server does not reveal which
    issuer minted the caller's token, so the allowlist only stops a claim
    naming an issuer your site does not use. Verifying the subject's *token*
    is the in-guest agent's job (roadmap phase C), against `status.nonce`.

    **A broker is trusted for attribution.** Anyone on the `brokers` list can
    attribute a sandbox to any identity, which is what brokering means.
    That ConfigMap is as sensitive as the audit trail it underwrites — it is
    empty by default, and worth alerting on.

To exercise this against a **real identity** rather than a certificate CN,
see [Testing claim authorization](testing-claim-authorization.md) — it sets
up a dev cluster that logs you in with your GitHub account.

!!! note "Install it"
    The policy lives in `deploy/admission/`, which `banlieue bootstrap` does
    **not** apply. Applying `deploy/admission/` is a separate, mandatory
    step. The binding is `parameterNotFoundAction: Deny`, so a missing
    ConfigMap blocks claims rather than silently allowing any subject — and
    the shipped `issuers` list is a placeholder you must edit.

## Claims inherit the pool's readiness caveat

A claim binds what the **pool** considers available, which depends on the
pool's `spec.readiness`. `InfrastructureReady` is correct for
`installMode: Immediate` images and **wrong for `Deferred` ones**, where it
fires when the install *starts*.

`GuestReady` ([ADR-0043](https://github.com/firestoned/banlieue/blob/main/docs/adr/0043-guestready-installed-guest-signal.md))
is the correct choice for a Deferred pool, and works on libvirt provided the
image carries the guest-phase layer and `qemu-guest-agent`. On vSphere the
transport is not implemented yet, so a Deferred vSphere pool still cannot be
trusted. See
[VirtualMachine Pools](virtualmachine-pools.md#the-one-field-that-can-fail-silently).

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `Pending` forever, `reason=PoolNotFound` | `spec.poolRef.name` names a pool that does not exist in this namespace. |
| `Pending` forever, `reason=NoMemberAvailable` | Read the pool reason in the message. `Filling` means wait; `ReadinessSignalAbsent` means the pool will never warm. |
| `Phase: Failed` | The bound member was deleted out from under the claim. Terminal by design — create a new claim. |
| `kubectl delete vmclaim` hangs | Expected while the backend VM is being destroyed. Check the member: `kubectl get virtualmachine <name> -o yaml`. If its own finalizer is stuck, the provider cannot reach the backend. |
| Claim bound to an old image | Nothing fresher was Ready. Expected; see "Which member you get". |
| Bound, but `status.addresses` is empty | The member has no address yet. The claim mirrors what the VM reports. |
| `status.tpmEndorsementCertificates` always empty | Expected until ADR-0045 publishes them. |

## Reference

- [`VirtualMachineClaim` API reference](../reference/api.md#virtualmachineclaim)
- [ADR-0047 — `VirtualMachineClaim`: bound once, released by deletion](https://github.com/firestoned/banlieue/blob/main/docs/adr/0047-virtualmachineclaim.md)
- [The Claim Flow, End to End](../concepts/virtualmachine-claim-flow.md) — every actor and message, end to end
- [VirtualMachine Pools guide](virtualmachine-pools.md) — where the members come from
