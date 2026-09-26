# Guide: Sandbox Identity with mediatore

Give a claimed sandbox a verifiable identity, and let it act *as the person who
claimed it* — without any user token ever entering the VM.

banlieue deliberately stops at the claim: a
[`VirtualMachineClaim`](virtualmachine-claims.md) records *who* a sandbox is
for (`spec.subject`, enforced by the claim-subject admission policy), binds a
warm pool member, and destroys it at the deadline. banlieue never carries or
validates a credential.

**[mediatore](https://github.com/firestoned/mediatore)** is the companion
project that finishes the story. It sits in front of the claim API and behind
every sandbox:

1. **Front door.** Validates the user's IdP login (Entra ID, or Dex in a
   homelab) and creates the claim with `spec.subject` taken from the token —
   mediatore is the allowlisted broker the VAP trusts to do this.
2. **Binding.** The pool member attested its vTPM to a SPIRE server at boot
   (the EK certificate chain, the same one
   [ADR-0045](https://github.com/firestoned/banlieue/blob/main/docs/adr/0045-vtpm-ek-certificate.md)
   publishes on the infra CR). On bind, mediatore creates a SPIFFE identity
   scoped to that claim, that VM, and a per-subject Linux user.
3. **Delegation.** The jailed workload trades its SPIFFE identity for
   short-lived, single-audience tokens — minted via Entra On-Behalf-Of or
   mediatore's own STS. Fifteen minutes, one audience, never a refresh token,
   never at rest.
4. **Cut-off.** Claim released or expired: the SPIFFE entry is deleted, token
   issuance stops, and banlieue destroys the VM as usual.

From banlieue's side nothing changes: the Kubernetes API remains the only
interface, and the claim lifecycle is exactly the one described in the
[claims guide](virtualmachine-claims.md).

## Enterprise deployment: split SPIRE onto its own identity tier

For anything beyond a single-operator lab, do **not** run the SPIRE server in
the same cluster as banlieue and the sandboxes. It is the root of workload
identity — treat it like your PKI: a root SPIRE server on a dedicated identity
cluster (HSM-backed via an `UpstreamAuthority`), issuing intermediate CAs to
one downstream SPIRE server per environment; mediatore's entry-management
rights are granted on the downstream server only, so a compromised broker is
contained to its own environment.

The full topology, rules and migration path live in mediatore's docs — linked
below rather than duplicated here:

- [Running workloads on a claimed VM](https://github.com/firestoned/mediatore/blob/main/docs/guides/running-workloads.md)
  — how a user, an agent acting on behalf of a user (Entra On-Behalf-Of), or an
  app-only service actually gets code executing in the sandbox, with sequence
  diagrams and the interaction patterns (nothing inbound ever reaches the VM).
- [Splitting SPIRE out: the enterprise topology](https://github.com/firestoned/mediatore/blob/main/docs/guides/enterprise-spire-topology.md)
  — the operational guide.
- [mediatore ADR-0005](https://github.com/firestoned/mediatore/blob/main/docs/adr/0005-spire-topology.md)
  — the decision record.
- [mediatore threat model](https://github.com/firestoned/mediatore/blob/main/docs/security/threat-model.md)
  — trust boundaries, STRIDE tables and the accepted-risk register (including
  the homelab collapse, R-7).
- [mediatore README](https://github.com/firestoned/mediatore#readme) — current
  implementation status.
