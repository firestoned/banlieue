<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0045 — Publish the vTPM endorsement key certificate

- **Status:** Accepted
- **Date:** 2026-09-23
- **Deciders:** Erick Bourgeois
- **Related:** Consumed by
  [ADR-0049](0049-attestation-trust-anchors.md) Decision 4 (a quote is
  verified against this certificate); depends on the per-VM vTPM that
  [ADR-0039](0039-vsphere-vtpm-support.md) attaches and
  [ADR-0040](0040-deferred-install-for-vtpm-encryption.md) keeps unique;
  reuses the read path built by
  [ADR-0043](0043-guestready-installed-guest-signal.md) and orders itself
  against `GuestReady` the same way
  [ADR-0044](0044-detach-install-media-after-install.md) does; mirrored onto
  [ADR-0047](0047-virtualmachineclaim.md)'s claim status; phase A5 of
  [roadmap 17](../../.github/community/17-ephemeral-vm-pools.md)

## Context

ADR-0049 settles how a sandbox receives its subject's credential: the guest
proves itself with a TPM quote over `status.nonce`, and banlieue never holds
the token. That exchange has one unmet prerequisite — **the verifier needs
something to check the quote against.** A quote is signed by an attestation
key; an AK is trustworthy only when it is certified under an endorsement key
whose certificate chains to an issuer the verifier already trusts.

Today no part of banlieue publishes an EK certificate, so ADR-0049 is a design
with nothing to verify against and roadmap 17's stop condition — a claim binds
to a member that "publishes its vTPM EK certificate" — cannot be met.

`VirtualMachineClaimStatus.tpmEndorsementCertificates` already exists,
documented as "stays empty until ADR-0045 publishes them". This ADR fills it.

### The two backends are not symmetric, and the roadmap assumed they were

Roadmap 17 phase D says, in one line: *"EK certificates: read from swtpm's
`swtpm_localca`-issued cert."* That describes a host-side read. **On libvirt
no such read exists.** Established empirically against a real host
(libvirt 11.3.0, swtpm 0.7.1) on 2026-09-23:

- libvirt does invoke `swtpm_setup` with `--createek --create-ek-cert
  --create-platform-cert --lock-nvram --vmid`, so every emulator-backed vTPM
  **does** get a certificate, issued by the host's `swtpm_localca`.
- `swtpm_localca` writes that certificate into a temporary directory,
  `swtpm_setup` loads it into the vTPM's NVRAM, and **the temporary directory
  is deleted.** Nothing persists it. `/var/lib/swtpm-localca/` keeps the
  issuer key, the issuer certificate and a serial counter — not the
  certificates it issued.
- No libvirt RPC exposes it. The domain's swtpm state
  (`/var/lib/libvirt/swtpm/<uuid>/tpm2/`) is an opaque blob on the host
  filesystem, which banlieue has no access to: it speaks libvirt RPC over
  mTLS and nothing else (ADR-0011).

So the certificate exists, in exactly one readable place: **inside the guest**,
at TPM NV index `0x01c00002`. Verified by reading it out of a live domain:

```
subject = CN=banlieue-ek-probe:7a9856d5-e515-49d5-826f-9a261a78fbc6
issuer  = CN=swtpm-localca
SAN     = tcg-at-tpmManufacturer / tcg-at-tpmModel / tcg-at-tpmVersion (critical)
```

vSphere is the opposite: vCenter issues the certificate and exposes it
host-side as `VirtualTpm.endorsementKeyCertificate` (DER), readable before the
guest has booted at all.

| | vSphere | libvirt |
| --- | --- | --- |
| Issuer | vCenter | per-host `swtpm_localca` |
| Readable at | the hypervisor, pre-boot | the vTPM's NVRAM, in-guest only |
| Earliest available | before install finishes | after the guest is up |

The roadmap's stated goal — record the certificate "before the install is even
finished, while the VM is still something only banlieue has touched" — is
achievable on vSphere and **not achievable on libvirt**. That asymmetry is
forced by swtpm's design, not chosen here.

### Why a guest-reported EK certificate is still sound

This is the objection that matters, because on libvirt the data crosses a
boundary from inside a VM that may later be handed to a prompt-injected agent
(§6/TB-5 of the threat model). It is worth stating precisely why the weaker
acquisition path does not weaken the guarantee.

**An EK certificate is a public key.** A guest that reports some *other*
member's certificate does not thereby obtain that member's EK private key,
which never leaves the vTPM that generated it. ADR-0049's exchange does not
trust the certificate on presentation — it requires an AK certified under that
EK, which is an activation the holder of the EK private key alone can
complete. A substituted certificate therefore fails the very step it was
substituted to pass. The lie is self-defeating.

**The certificate is bound to the domain by name.** libvirt passes
`--vmid <domain-name>:<domain-uuid>`, and swtpm_localca puts it in the
subject CN — both values banlieue itself assigned when it defined the domain.
A mismatch is detectable with no cryptography at all, so the substitution is
caught one step earlier than ADR-0049 would catch it.

What a guest *can* do is refuse to report, or report garbage. Both are
denial of service against its own readiness, which is already the guest's
prerogative under ADR-0043 and costs the attacker its own sandbox.

### Why not `guest-exec`

Reading NV index `0x01c00002` is a `tpm2_nvread`, and the obvious way to run
it is `guest-exec`. It works — it is how this was first proven. It is also
**arbitrary code execution from the host into the guest**, and adopting it
would reverse a deliberate property of the ADR-0043 read path, which opens
guest files with an explicit `mode: "r"` precisely so that "a reconcile loop
inspecting a guest must not be able to modify it".

Granting banlieue a general RCE primitive into every sandbox, to read one
public certificate, is a bad trade — and it is unnecessary, because the guest
can write the certificate out for the same file-read path ADR-0043 already
uses and proved against a real agent.

## Decision

**1. The EK certificate is published as PEM on the infrastructure CR's
status.** `LibvirtMachineStatus.tpmEndorsementCertificates` and
`VSphereMachineStatus.tpmEndorsementCertificates`, both `Vec<String>`,
omitted when empty.

A list, not a scalar: vSphere's `VirtualTpm.endorsementKeyCertificate` is
already a list, and a TPM may carry both an RSA and an ECC EK certificate
(NV `0x01c00002` and `0x01c0000a`). PEM, not DER, because every consumer of
this field is a verifier that wants to feed it to a certificate library, and
because a CR status is a text document.

**2. On libvirt the guest writes the certificate; banlieue reads it
read-only.** The installed system writes PEM to `/run/banlieue/ek.pem` —
beside ADR-0043's `/run/banlieue/phase`, on the same tmpfs, written by the
same cloud-config layer — and banlieue reads it with the existing
`guest-file-open`/`guest-file-read` path. **banlieue never calls
`guest-exec`.**

`/run` for the same reason ADR-0043 chose it: tmpfs cannot carry a stale
assertion across a power cycle.

**3. The certificate's subject CN must equal
`<domain-name>:<domain-uuid>`.** A certificate that does not match the domain
banlieue defined is discarded and not published, and the machine reports
`Ready=False`, reason `TpmEndorsementMismatch`. This is the cheap half of the
check; ADR-0049's activation is the half that cannot be forged.

**4. On vSphere the certificate is read host-side, before the install
finishes.** `VirtualTpm.endorsementKeyCertificate` is DER, converted to PEM
and recorded as the first step of `next_guest_step`, so the binding is
established while the VM is still something only banlieue has touched. No
guest cooperation and no CN check — vCenter is the authority for both the
certificate and the VM's identity.

**5. For a `tpmEnabled` machine, publishing the certificate gates
`GuestReady`.** Exactly ADR-0044's ordering and for exactly its reason: a
pool binds a member the moment it reports `GuestReady` (ADR-0046), so a
condition published before the certificate is available opens a window in
which a claim binds a member that cannot attest. Roadmap 17's stop condition
requires a bound member to publish its EK certificate; gating is what makes
that true by construction rather than by timing.

Machines with `tpmEnabled: false` are unaffected — there is no vTPM, no
certificate, and nothing to wait for.

**6. The field is public and carries no new authorization.** An EK
certificate is public key material. It is readable by every reader of the
infra CR, which is the same audience that already reads machine status, and
mirrored onto the claim so a consumer needs one GET (ADR-0047).

## Consequences

**A `tpmEnabled` libvirt image must be able to write the marker.** The same
cloud-config layer that satisfies ADR-0043 gains one more file, and the image
needs a TPM client (`tpm2-tools` or equivalent) to produce it. An image that
cannot will never report `GuestReady` under Decision 5 — loudly, as a member
stuck un-Ready with a reason, not silently as one that binds without an
identity. This is the same image requirement ADR-0043 already imposes,
extended by one file.

**Trust anchors are out of scope and remain ADR-0049's.** This ADR publishes
a certificate; it does not say who should be believed. The issuer differs per
backend — vCenter on vSphere, a per-host `swtpm_localca` on libvirt — and
`Provider.spec.attestation.ekTrustBundle` (ADR-0049, phase F) is where an
administrator declares which issuers count. Until that lands, a consumer that
verifies a chain must supply its own roots.

**swtpm EK certificates do not expire.** The observed `notAfter` is
`9999-12-31`, so validity-period checks are not a revocation mechanism on
libvirt. A compromised host's local CA is revoked by removing it from the
trust bundle, which is ADR-0049's problem and another reason that bundle is
explicit and admin-supplied rather than discovered.

**vSphere's half is unverifiable, and is read once.** There is no vCenter
available to this project, so Decision 4 ships as code with unit tests and
without a live run — the same position ADR-0043's and ADR-0044's vSphere
halves are in. It carries a known gap on top of that: the read happens only
on the create path, because `reconcile` short-circuits every provisioned
machine to a power-state refresh that does not read it. If vCenter populates
`endorsementKeyCertificate` asynchronously with the vTPM attach, the first
read returns empty and nothing retries, so the machine publishes no anchor
at all. Which behaviour is real is precisely what no environment has been
available to determine. **Treat the vSphere path as unproven.** The libvirt
path is verified end-to-end against a real host.

**A pre-existing SSA retraction reaches this field.**
`patch_status_failed` applies only `{conditions, observedGeneration}` from
the same field manager that elsewhere applies the whole status — the exact
hazard `status_with_observed_power_state`'s own comment documents having hit
live ("the same field manager must always apply the same complete field
set"). So one transient vCenter error retracts `vmRef`, `tpmAttached` and
now `tpmEndorsementCertificates`. This predates this ADR and is not fixed
here; it is recorded because it makes the vSphere anchor strictly less
durable than this document otherwise implies.

**This unblocks phase F.** ADR-0049 was Proposed and blocked on exactly this
field. With the certificate on the claim, the remaining work there is the
trust bundle and the in-guest agent, not the anchor itself.
