# 0049 — Attestation: the guest proves itself, banlieue never holds the token

- **Status:** Proposed
- **Date:** 2026-09-22
- **Deciders:** Erick Bourgeois
- **Related:** Completes the delivery half of
  [ADR-0047](0047-virtualmachineclaim.md) Decision 9 (the claim carries no
  credential) and consumes
  [ADR-0045](0045-vtpm-endorsement-key-certificate.md)'s
  EK certificates. Depends on the per-VM vTPM that
  [ADR-0040](0040-deferred-install-for-vtpm-encryption.md) exists to
  guarantee, and on the issuer allowlist in
  `deploy/admission/virtualmachineclaim-subject-authorization.yaml`.

## Context

A `VirtualMachineClaim` says *who* a sandbox is for and deliberately carries
nothing that proves it. Decision 9 is explicit: a custom resource is
readable by every reader of the namespace and lands in the audit log, so a
token in it is a token disclosed.

That leaves an unanswered question, and it is the one that decides whether
the claim model is usable: **how does the subject's credential reach the
guest, and what convinces the guest it is genuine?**

### Why nothing can be injected

Every channel banlieue already has is disqualified, and not by preference:

| Channel | Why not |
| --- | --- |
| cloud-init / NoCloud seed | The seed volume sits in a shared storage pool, and the rendered payload is plaintext in `LibvirtMachine.spec.userData` — ADR-0025's accepted reflection. Anyone who can `get libvirtmachines` reads it. |
| vSphere `guestinfo` | Readable with vCenter read access *and* by any process in the guest (ADR-0047 Decision 9). |
| `qemu-guest-agent guest-file-write` | Mechanically available — the read half already exists for ADR-0043 — but it makes banlieue a credential carrier and requires the token to reach the *provider* first, which relocates the problem rather than solving it. |

There is also a structural reason, which is the decisive one: **a warm pool
member exists before any claim does.** At provisioning time there is no
subject and no token, so nothing baked in at build or boot can be
subject-specific. Injection-at-provision is not unsafe here so much as
impossible.

### What the guest already has that is unique to it

The per-VM vTPM, and it is unique *by construction* on both backends —
for different reasons:

- **vSphere** — deferred install (ADR-0040) never installs the golden
  template, so each clone runs Kairos's installer itself with its own
  already-attached, already-unique vTPM present *during* that install. The
  duplication hazard ADR-0040 documents is specifically about copying a
  *template's* vTPM and its secrets, which deferring avoids entirely.
- **libvirt** — swtpm state is keyed by **domain UUID**, so a new domain
  always gets a fresh TPM and the duplication problem never arises.

So every pool member holds a hardware-rooted identity nobody else has, and
it was established before any subject was assigned. That is the anchor this
ADR builds on.

## Decision

1. **The credential is pushed to the guest over an attested mTLS channel,
   and is never at rest anywhere.** Not on a disk, not in a CR, not through
   a hypervisor channel. The broker holds it, the in-guest agent receives
   it, and nothing in between stores it.

2. **banlieue neither carries nor validates the token.** It records the
   attribution, publishes `status.nonce`, and mirrors the EK certificate
   from the infra CR. Verification belongs to the agent, which is the only
   party that can legitimately hold the credential.

   Keeping banlieue out of the path is what lets `subject` stay opaque
   (Decision 9) and keeps the controller's compromise radius free of
   subject credentials.

3. **The agent's keypair and AK are generated during the per-VM install,
   never shipped in an image.** With `installMode: Deferred` this is
   natural rather than a constraint: the install *is* per-VM, so anything it
   generates is per-VM.

   The hazard this rules out belongs to `Immediate` images, where a
   pre-installed disk is cloned and a baked-in key really would be shared by
   every VM from it. That combination is already called out as an operator
   responsibility (ADR-0040 Decision 5); this ADR does not rescue it.

4. **The handshake is quote-over-nonce, verified against the claim's EK
   certificate.** The broker sends `{claim, nonce, JWT}`; the agent returns
   a TPM quote over `status.nonce`; the broker checks it against the EK
   certificate published on the claim (ADR-0045).

   The nonce is why this cannot be replayed: it is 128 bits minted at bind
   time and unique to one claim, so a quote captured from one sandbox proves
   nothing about another.

5. **`aud` is the agent's own configured audience and must never be read
   from the claim.** If the audience came from the claim, whoever wrote the
   claim would choose it, and a token minted for any other service would
   satisfy the check.

   Stated as a decision because taking it from the claim is the obvious
   implementation and it silently removes the guarantee.

6. **The agent checks the token against the claim: `iss` equals
   `subject.issuer`, the subject claim equals `subject.id`, plus `aud`,
   `exp`, `nbf` and the signature.** This is what ADR-0047's amended
   Decision 9 exists to make possible — `subject.id` holds the raw subject
   as the issuer spells it, so the comparison is direct.

7. **Signature verification needs the issuer's JWKS, so the issuer's
   discovery host is in the sandbox's egress allowlist — and the admission
   policy's issuer allowlist is what makes that safe.**

   The agent learns *which* issuer to trust from `subject.issuer`, a field
   on a CR. Without the allowlist, an attacker who could create a claim
   would point the agent at their own JWKS and every token they minted
   would validate. The allowlist added for audit honesty turns out to be
   load-bearing for verification too, which was not obvious when it was
   written.

8. **A failed handshake leaves the sandbox unused and reaped, not
   degraded.** No token, no workload; the claim's TTL destroys the VM as
   usual. There is no partial-trust mode in which an unattested guest runs
   anything, because a sandbox that ran unattested is indistinguishable
   afterwards from one that did not.

9. **The in-guest agent lives in its own repository.** It is not a banlieue
   binary: it runs inside the guest, its release cadence is the image's, and
   nothing in banlieue imports it. banlieue's side of the contract is the
   three fields it publishes.

## Consequences

- The claim model becomes usable end to end: a subject's credential reaches
  its sandbox without banlieue ever seeing it.
- **`GuestReady` and this are different signals and must not be conflated.**
  ADR-0043 Decision 9 already says so; this is the integrity signal it
  disclaims being. A guest that announced itself has booted from its
  installed disk; only a verified quote says *which* guest it is.
- **ADR-0045 becomes a hard dependency.** Without the EK certificate on the
  claim there is nothing to verify a quote against, and the handshake
  degrades to "trust whatever answered on the port". **Satisfied
  2026-09-23**: ADR-0045 is Accepted and implemented, so the anchor is on
  the claim and a `tpmEnabled` member is not bindable until it publishes
  one. Note the asymmetry it records — on vSphere the certificate is read
  from the hypervisor, on libvirt it is reported by the guest, because
  swtpm persists no host-side copy. That does not weaken the verification
  here (an EK certificate is a public key; Decision 4's activation is what
  proves possession), but a verifier should know the provenance differs.
- The issuer's discovery host must be reachable from inside the sandbox.
  That is a real constraint on the egress allowlist and on air-gapped
  deployments, where a pre-provisioned key set is the alternative.
- A broker is now load-bearing twice over: it holds credentials *and* it is
  the party that verifies quotes. Its compromise is the design's worst case,
  which the threat model records rather than mitigates.
- Nothing here is provider-specific. The vTPM's uniqueness is guaranteed by
  different mechanisms on vSphere and libvirt, but the anchor it provides is
  the same and the handshake does not know which backend it is on.
