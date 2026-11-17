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

`examples/20-virtualmachineclaim.yaml` is a complete claim against the pool
in `examples/19-virtualmachinepool.yaml`.

```sh
kubectl apply -f examples/19-virtualmachinepool.yaml
kubectl apply -f examples/20-virtualmachineclaim.yaml
kubectl get vmclaim -n banlieue-system -w
```

```text
NAME                POOL           VM                   PHASE     EXPIRES
sandbox-for-alice                                       Pending
sandbox-for-alice   sandbox-pool   sandbox-pool-9dmtx   Bound     2026-09-20T14:17:11Z
```

The `POOL` column reads `status.poolRef`, the pool the bound member came
from, so it stays empty until the claim binds.

Once `Bound`, everything a consumer needs is in one GET:

```yaml
status:
  phase: Bound
  virtualMachineRef: { name: sandbox-pool-9dmtx }
  poolRef: { name: sandbox-pool }
  addresses:
    - type: InternalIP
      address: 192.0.2.17
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
    over its own attested channel, anchored on the member's EK certificate
    (`status.tpmEndorsementCertificates`).

### `status.nonce` is retired

Up to v0.4.13 a bind minted a 128-bit `status.nonce` for ADR-0049's
quote-over-nonce handshake. That handshake was never used: the consumer
(mediatore) attests each guest with SPIRE's TPM node attestation, whose
credential-activation challenge is its own replay guard. Since v0.4.14 no
nonce is minted; the field stays in the schema, deprecated, so older claims
still parse, and goes with the next API version
([ADR-0049](https://github.com/firestoned/banlieue/blob/main/docs/adr/0049-attestation-trust-anchors.md),
amended 2026-11-05).

### Verifying the guest: where the trust anchor comes from

A bound `tpmEnabled` member publishes its vTPM's endorsement-key certificate
on the claim (`status.tpmEndorsementCertificates`,
[ADR-0045](https://github.com/firestoned/banlieue/blob/main/docs/adr/0045-vtpm-endorsement-key-certificate.md)).
That certificate is what a verifier checks the guest's attestation against, but a
certificate is only as good as its issuer, and which issuers count is a
per-backend decision the administrator makes explicitly on the **Provider**:

```yaml
spec:
  attestation:
    ekTrustBundle:          # exactly one of inline / configMapRef / secretRef
      configMapRef:
        name: swtpm-localca-issuers
        # key: ca.crt
```

`ekTrustBundle`
([ADR-0049](https://github.com/firestoned/banlieue/blob/main/docs/adr/0049-attestation-trust-anchors.md)
Decision 10) is the same value-or-source shape as `connection.caBundle`. The
issuing CA differs by backend: on vSphere it is vCenter's; on libvirt it is
each host's own `swtpm_localca` issuer certificate
(`/var/lib/swtpm-localca/issuercert.pem`), one entry per host.

Three things it deliberately is **not**:

- **Not discovered.** An anchor read off the hypervisor would let whoever
  controls the hypervisor choose the anchor that vouches for its own guests.
- **Not verified by banlieue.** banlieue publishes configuration; the
  verifier (the broker, [mediatore](https://github.com/firestoned/mediatore)) resolves the referenced
  ConfigMap/Secret in the Provider's namespace and does the checking.
- **Not subject to expiry.** swtpm EK certificates carry
  `notAfter 9999-12-31`, so removing a host's issuer from this bundle *is*
  the revocation mechanism on libvirt.

## `ttlSeconds` is mandatory, and it is a deadline

No default, on purpose. A claim without a deadline is a leaked VM waiting to
happen, and there is no number banlieue could pick that is not wrong for
somebody's workload.

Three properties worth knowing:

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

A selector that matches no pool is its own reason, and a selector claim
waiting on several pools repeats each one's diagnosis (up to five, then a
count):

```text
Bound=False   reason=NoPoolMatches
message: no pool in this namespace matches spec.poolSelector

Bound=False   reason=NoMemberAvailable
message: no Ready, unclaimed member in any of 2 matching pools:
         pool-a (pool: Filling: 0 of 2 members ready);
         pool-b (pool has not published a Warm condition yet)
```

## Choosing the pool: `poolRef` or `poolSelector`

A claim takes **exactly one** of two fields (ADR-0132):

- `spec.poolRef` names one pool in the claim's namespace.
- `spec.poolSelector` is a label selector over the pools in the claim's
  namespace. The members of every matching pool are ranked together, as
  one set, by the rules in the next section.

```yaml
spec:
  poolSelector:
    matchLabels:
      sandbox.example.com/image: agent
```

Use a selector when several pools are interchangeable (one per host, one
per zone, all serving the same image). A claim on one named pool waits
while that pool is empty, even when the pool next to it holds a Ready
member; a selector claim takes that member.

For **any pool in the namespace**, write the empty selector,
`poolSelector: {}`. A claim with neither field, or with both, is refused by
the API server: an omitted pool is never read as "any pool". A selector
never reaches outside the claim's namespace.

Both fields are immutable once the claim exists. Since the spec of a
selector claim names no pool, `status.poolRef` records the one that
supplied the VM. `examples/27-virtualmachineclaim-pool-selector.yaml` is a
complete selector claim.

## Which member you get

In order:

1. **current image revision** of the member's own pool;
2. a member on a **healthy host** before one on an unhealthy host;
3. the claim's **selection strategy** (below);
4. **longest Ready**;
5. name.

With a `poolSelector`, these rules run over the members of every matching
pool at once, and "current revision" means current *for the member's own
pool*. No pool is preferred over another.

- *Current revision first*, so a rebuild that was meant to retire an
  unpatched build actually does. No placement preference outranks it.
- *Healthy host next*, under every strategy. A host is unhealthy when its
  `Provider` is cordoned (`banlieue.io/unschedulable: "true"`), its
  heartbeat is `Stale`, or it cannot be found. These are the scheduler's
  own tests, so a host the scheduler will not place new VMs on is also the
  last one a claim takes from.
- *Longest Ready*, so the member closest to being reaped for staleness
  is the one handed out and idle expiry rarely has to fire.
- *Name last*, purely so the choice is deterministic.

None of these rules ever leaves a claim waiting. A **stale-revision
member**, or a member on a **cordoned host**, is still handed out when
nothing better is Ready. An available sandbox beats no sandbox, and its
life is bounded by the TTL anyway. Refusing would mean a consumer waits out
a full install during exactly the rollout the pool exists to hide.

### Choosing a strategy: `spec.selection.strategy`

```yaml
spec:
  poolSelector:
    matchLabels:
      sandbox.example.com/image: agent
  selection:
    strategy: Spread
```

| Strategy | Takes from | Use it when |
| --- | --- | --- |
| `LongestReady` (default) | the member Ready longest, wherever it runs | you have no placement preference |
| `MostAvailable` | the failure domain with the most Ready members | you want warm capacity to drain evenly, so losing one domain costs the fewest warm members |
| `Spread` | the failure domain where this subject holds the fewest members, then as `MostAvailable` | one subject holds several sandboxes and they must not share a host |

A **failure domain** is the pair `(provider, failureDomain)` from the
member's `status.scheduled`. Two hosts whose failure domain carries the same
name (each Cloud Hypervisor or libvirt host's single domain, typically) are
two domains, not one.

`Spread` matches the subject on **issuer and id together**, from the
annotations a bind writes on each member. It only sees members in the
claim's own namespace, and it is **soft**: when every Ready member is in a
domain the subject already occupies, the claim binds the one in the domain
it occupies least rather than waiting. Two claims for one subject racing on
one snapshot pick the same member; the loser retries on a fresh list and
usually lands elsewhere, but two members can still share a domain.

The `Bound` condition says what was chosen and why:

```text
bound to sandbox-pool-b-4kq7z by Spread from host-b/default
```

and adds a note when the choice was a fallback (an unhealthy host, or a
domain the subject already held). `spec.selection` is immutable once the
claim exists. `examples/29-virtualmachineclaim-spread.yaml` is a complete
example.

## What happens when two consumers race

Nothing special, and deliberately so. Binding is a merge patch on the member
carrying its `resourceVersion`, so if anyone, including another claim,
wrote that member since the controller listed it, the patch is rejected with
`409 Conflict` and the loser simply picks again. The version is the one from
the list the member was picked from: re-reading the member just before the
patch would return the winner's version and let the loser overwrite it.

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
    is the in-guest agent's job, after the guest has attested against the
    member's EK certificate (ADR-0049).

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
is the correct choice for a Deferred pool, and works on both backends: on
libvirt provided the image carries the guest-phase layer and
`qemu-guest-agent`, and on vSphere from `guestinfo.banlieue.phase`
(verified live against a real vCenter, 2026-09-23). See
[VirtualMachine Pools](virtualmachine-pools.md#the-one-field-that-can-fail-silently).

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `Pending` forever, `reason=PoolNotFound` | `spec.poolRef.name` names a pool that does not exist in this namespace. |
| `Pending` forever, `reason=NoPoolMatches` | `spec.poolSelector` matches no pool's labels in this namespace. Check `kubectl get vmpool --show-labels`. |
| Claim refused: `exactly one of poolRef or poolSelector` | The claim has neither field or both. For any pool, use `poolSelector: {}`. |
| `Pending` forever, `reason=NoMemberAvailable` | Read the pool reason in the message. `Filling` means wait; `ReadinessSignalAbsent` means the pool will never warm. |
| `Phase: Failed` | The bound member was deleted out from under the claim. Terminal by design — create a new claim. |
| `kubectl delete vmclaim` hangs | Expected while the backend VM is being destroyed. Check the member: `kubectl get virtualmachine <name> -o yaml`. If its own finalizer is stuck, the provider cannot reach the backend. |
| Claim bound to an old image | Nothing fresher was Ready. Expected; see "Which member you get". |
| Claim bound to a member on a cordoned host | Nothing on a healthy host was Ready (or only stale-revision members were). The `Bound` message says `its host is cordoned or not reporting`. Expected; see "Which member you get". |
| `Spread` put two of a subject's sandboxes in one domain | Every Ready member was in a domain the subject already held, or two claims raced on one snapshot. Spread is soft; the `Bound` message says `already holds N member(s)`. |
| Claim refused: `spec.selection is immutable` | The strategy is read once, at pick time. Create a new claim. |
| Bound, but `status.addresses` is empty | The member has no address yet. The claim mirrors what the VM reports. |
| `status.tpmEndorsementCertificates` always empty | The member's class is not `tpmEnabled` — publication gates `GuestReady` for `tpmEnabled` members (ADR-0045), so a bound one always carries its certificate. |

## Reference

- [`VirtualMachineClaim` API reference](../reference/api.md#virtualmachineclaim)
- [ADR-0047 — `VirtualMachineClaim`: bound once, released by deletion](https://github.com/firestoned/banlieue/blob/main/docs/adr/0047-virtualmachineclaim.md)
- [ADR-0132: A claim may select its pool by label](https://github.com/firestoned/banlieue/blob/main/docs/adr/0132-claim-selects-its-pool-by-label.md)
- [ADR-0135: A claim chooses its member by strategy](https://github.com/firestoned/banlieue/blob/main/docs/adr/0135-claim-selection-strategies.md)
- [The Claim Flow, End to End](../concepts/virtualmachine-claim-flow.md) — every actor and message, end to end
- [VirtualMachine Pools guide](virtualmachine-pools.md) — where the members come from
