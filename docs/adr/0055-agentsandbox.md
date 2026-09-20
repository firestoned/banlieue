# 0055 — `AgentSandbox`: the agent layer owns a claim, not a member

- **Status:** Proposed
- **Date:** 2026-09-20
- **Deciders:** Erick Bourgeois
- **Related:** Layers on top of [ADR-0047](0047-virtualmachineclaim.md)
  (`VirtualMachineClaim`) and [ADR-0046](0046-virtualmachinepool.md)
  (`VirtualMachinePool`). Depends on ADR-0043 (`GuestReady`) to be
  trustworthy in production — see Consequences. The attestation handshake
  this object eventually carries is **out of scope** and belongs to roadmap
  70 phase C and ADR-0049.

## Context

Roadmap 70's premise is AI agent sandboxes, and everything built so far is
deliberately not about agents: a pool keeps warm VMs, a claim hands one out
for one subject and destroys it on release. Both are infrastructure, and
both are useful to anything that wants a disposable machine — CI, untrusted
build steps, a customer demo environment.

But an agent sandbox needs things a VM claim has no business knowing:
which model runtime to use, which token issuers and audiences are
acceptable, what egress is permitted, which tools are in scope, where the
workspace lives. Putting those on `VirtualMachineClaim` would make the
infrastructure API a lexicon of somebody else's domain.

Phase C already assumed this layer exists — as a **broker** that hands the
in-guest agent `{claim, nonce, JWT}` over mTLS. What this ADR decides is
that the broker's *API* is a Kubernetes object rather than a service
endpoint, and where the line between it and the claim falls.

### The thing that must not break

`VirtualMachineClaim` enforces one invariant: **a member is bound at most
once in its life.** It is enforced in exactly one place — a
`resourceVersion`-preconditioned patch that writes `banlieue.io/claim` — by
exactly one controller.

Any design where a second kind also binds members means two independent
implementations of that invariant, and a bug in either hands one VM to two
subjects. That is the single outcome the entire pool/claim design exists to
prevent, so the shape of this layer is decided by it.

## Decision

1. **`AgentSandbox` lives in a new API group, `agent.banlieue.io/v1alpha1`,
   served by its own controller.** `banlieue.io` stays infrastructure: a
   platform team running VMs for CI has no reason to install, run, or reason
   about an agent CRD. It also lets this layer move at a different pace from
   the contract-bearing APIs beneath it.

2. **Composition, not duplication: an `AgentSandbox` creates and owns a
   `VirtualMachineClaim`.** It never patches a pool member, never writes the
   claim label, never reads `resourceVersion` for a bind. The relationship
   is Deployment → ReplicaSet.

   This is the whole reason for the design. Bind-once stays in one
   controller, and it becomes *structurally impossible* for the agent layer
   to get it wrong — not merely unlikely.

3. **`AgentSandbox` never carries a credential.** Policy by value,
   credentials by reference only, resolved by the broker and delivered to
   the guest over the attested channel (ADR-0047 Decision 9).

   Stated as its own decision because this is exactly the object where an
   API key will end up — *because* it is the agent-specific one. A custom
   resource is readable by everyone with namespace read and lands in the API
   server's audit log, so a token field would be a token disclosure with
   extra steps. A field that could hold a key eventually will.

4. **`subject` stays on the claim; `AgentSandbox` carries issuer
   *policy*.** These are different things and the distinction is easy to
   lose: `VirtualMachineClaim.spec.subject` records *who this VM is for*,
   which is an audit question every consumer has. `AgentSandbox` declares
   *which issuers and audiences a presented token may come from*, which only
   an agent broker cares about.

5. **The agent runtime comes from the image, not from provisioning.** An
   `AgentSandbox` selects an image that already contains the runtime
   (built via `vm-build` plus a `cloudConfigs` layer, per phase C); it does
   not install one at bind time.

   Installing at bind time would put a container pull and an install on the
   path of every sandbox request — which is precisely the latency the pool
   exists to remove. A warm member that still has work to do before it is
   usable is not warm.

6. **One TTL, owned by the layer that can enforce it.**
   `AgentSandbox.spec.ttlSeconds` is passed through to the claim it creates,
   and expiry is the claim's job. Two deadlines on two objects would
   disagree the moment either controller lagged, and only the claim can
   actually destroy the VM.

7. **`AgentSandbox` holds a finalizer until its claim is gone.** ADR-0047
   Decision 6 made "claim deleted" mean "sandbox destroyed"; that guarantee
   has to survive being wrapped. Owner-reference GC deletes the claim but
   would let the `AgentSandbox` object disappear immediately, so a consumer
   watching the object it created would see success while the VM was still
   being torn down.

8. **Status mirrors the claim, and is not independently derived.** Phase,
   addresses, expiry and the bound VM come from the claim's status — the
   same rule as Non-Negotiable 6 (`VirtualMachine` status mirrors its infra
   ref), applied one layer up. An `AgentSandbox` is `Ready` only when its
   claim says the member is.

9. **The attestation handshake is out of scope.** This ADR defines the
   object, its ownership and its lifecycle. Whether the broker *is* this
   controller or remains a separate service — and therefore whether the
   TPM quote exchange happens inside banlieue — is phase C/F work and
   belongs with ADR-0049.

   Recorded explicitly because it is the largest question adjacent to this
   one, and leaving it implicit would invite it to be answered by accident.

## Consequences

- The agent layer is optional and separable: a cluster that does not install
  `agent.banlieue.io` loses nothing else.
- **`AgentSandbox` inherits ADR-0046's readiness caveat, and it matters more
  here.** A sandbox is bound when the pool considers a member available,
  which until ADR-0043 can only mean `InfrastructureReady` — for a
  `Deferred` image, the moment the install *starts*. An agent handed such a
  machine finds a half-built system. **A2 is a prerequisite for this being
  trustworthy in production, not a parallel track.** Against `Immediate`
  images it is correct today.
- Two objects exist per sandbox. That is the cost of the split, and it buys
  a bind-once invariant that only one controller can violate.
- A second consumer of `VirtualMachineClaim` (CI, build isolation) now has a
  clean seat at the table, because the agent concepts are not in its way.
- Nothing here is provider-specific, one layer further up than ADR-0047:
  an `AgentSandbox` knows about a pool, not about vSphere or libvirt.
- This does not make banlieue a CAPI `MachinePool` consumer and does not
  change the InfraMachine contract. `AgentSandbox` sits above the
  user-facing API, not inside the contract-bearing one.
