<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0143: The Firecracker host holds an attestation CA the administrator asserts, and signs one certificate per guest

- **Status:** Proposed
- **Date:** 2026-11-18
- **Deciders:** Erick Bourgeois
- **Related:** mediatore ADR-0049 (Proposed; the attestor this serves, `x509pop`
  with a certificate the host signs for the guest's own key),
  [ADR-0049](0049-attestation-trust-anchors.md) Decision 10 (trust anchors are
  admin-supplied on the Provider, never discovered),
  [ADR-0045](0045-vtpm-endorsement-key-certificate.md) (the EK certificate is
  published on the infrastructure CR and mirrored onto the claim),
  [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md) Decision 6
  (the per-host EK CA), [ADR-0137](0137-firecracker-provider-class.md)
  Decision 6 (the per-guest vsock listener),
  [ADR-0140](0140-firecracker-guests-are-supervised-by-systemd-not-the-jailer.md),
  [ADR-0141](0141-shared-host-provider-layer.md),
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md)

## Context

A Firecracker guest has no vTPM, so it cannot attest with an endorsement key.
mediatore ADR-0049 chooses SPIRE's `x509pop` attestor: the guest makes its own
key and a CSR, the host signs a certificate for that key, and the in-guest
agent presents it. The SPIRE server trusts a bundle of per-host signing CAs, and
the agent ID is `x509pop/<issuer CN>/<subject CN>`, so the control plane can
compute a node entry's parent before the guest boots.

That leaves three things on banlieue's side, none of which exist:

1. **who the host's CA is, and who vouches for it.** ADR-0049 Decision 10 is
   explicit that an anchor fished off the hypervisor would let whoever controls
   the hypervisor choose the anchor that vouches for its own guests. The host
   creates a CA; the **administrator** must assert it;
2. **what the subject CN is, and how mediatore learns it.** The ID must be
   computable before the guest attests, from facts banlieue publishes, as the EK
   certificate is for the TPM path (ADR-0045);
3. **the signer itself**: the component that answers on `<vsock uds>_1025`
   (ADR-0137 Decision 6) and signs.

## Decision

### 1. The host creates one attestation CA at install, and keeps it

`banlieue host firecracker install` gains an **attestation CA** stage, the
counterpart of the per-host EK CA of ADR-0065 Decision 6: created once, as the
provider's user so the key is born with the right owner, EC P-256, never
overwritten by a re-run.

- **Where:** `<state root>/attestation-ca/ca.key` (mode `0600`, the provider's user
  only) and `ca.crt` (public, `0644`). The directory is `0700`: a guest's uid cannot
  traverse it (the confined VMM was observed unable to read other directories
  under the host's storage and run roots, ADR-0140).
- **Its name:** the certificate's common name, chosen by the operator with
  `--attestation-ca-name`, defaulting to the host's short name restricted to
  letters, digits, dots, dashes and underscores (the alphabet an agent-ID path
  segment allows; any other character is refused, not replaced). The name is the
  load-bearing part of mediatore ADR-0049's confinement, because the issuer CN is in
  every agent ID it signs: it must be unique across every CA the SPIRE server
  trusts, which the installer cannot check (it does not see the others), so it is
  documented here and checked where the bundle is assembled.
- **Rotation is explicit:** `--force` does not rotate it (unlike the EK CA, whose
  rotation invalidates certificates nobody renews). A separate `--rotate-attestation-ca`
  makes a **new** CA with a **new** name, since a CA name must never be reused for a
  different key. It invalidates every certificate the old CA issued once the old one
  is removed from the SPIRE bundle.

### 2. The administrator asserts the CA on the Provider; it is never discovered

`Provider.spec.attestation` (ADR-0049 Decision 10) gains, for the `firecracker`
class, **`hostCa`**: a `CABundleSource` holding exactly **one** certificate, the
host's `ca.crt`. The administrator who installed the host copies it there,
exactly as the libvirt administrator copies each host's `swtpm_localca` issuer
into `ekTrustBundle`.

banlieue neither resolves nor verifies against it (ADR-0049 Decision 2): it is
configuration published for the broker and for whoever assembles the SPIRE
server's bundle. The "exactly one source" invariant is an admission rule, as for
`ekTrustBundle` (`deploy/admission/provider-attestation-ektrustbundle.yaml` is the
pattern), and the single-certificate rule is an admission rule too. The host's
status carries **no** anchor: a status the host's own token writes would be the
discovery ADR-0049 rejects.

Revocation is removal of the CA from the Provider and from the SPIRE bundle:
membership is the control (certificates here are short lived, Decision 5, so
expiry bounds them too, unlike a swtpm EK certificate).

### 3. The subject is the machine's UID, published like the EK certificate

The signer sets the certificate subject CN to the **`FirecrackerMachine`'s
`metadata.uid`**, not to a value from the guest and not to the `VirtualMachine`'s
UID. The infrastructure machine is one guest instance: a `Recreate` (a `VMClass`
change, ADR-0137 Decision 7) makes a new machine and so a new subject, so two
live guests never share an agent ID.

`FirecrackerMachineStatus.attestationSubject` publishes that exact string, and it
is mirrored onto the bound claim (ADR-0045 Decisions 1 and 6, ADR-0047): public,
carrying no authorization, so a consumer needs one GET. mediatore computes the
node entry's parent as `spiffe://<td>/spire/agent/x509pop/<issuer CN from hostCa>/<attestationSubject>`
before the guest boots. This is the TPM path's shape: a fact banlieue publishes and
mediatore derives the parent from, with no call to the SPIRE server.

### 4. The signer is the provider, on the per-guest listener

The provider process listens, per guest, on `<uds_path>_1025` (the shared layer's
vsock listener, ADR-0141 Decision 4): created when the guest's unit is started,
ownership granted to the guest's uid so the VMM can connect (the provider
already holds `CAP_CHOWN` for this, as it does for Cloud Hypervisor's API
socket), removed when the guest stops. The socket belongs to **that guest**, so
the peer of a request is that guest by construction.

One connection is one request. The signer:

- reads a PEM CSR (at most 4096 bytes), refuses anything else without a reply;
- accepts an **EC P-256** key only (the one tested) and checks the CSR's own
  signature;
- takes **only the public key** from the CSR, and discards its subject and
  every extension;
- issues a certificate with subject CN `attestationSubject`, `basicConstraints`
  `CA:FALSE` (the server does not enforce it, so the signer must), `keyUsage`
  `digitalSignature` (the server requires it), no SAN, no extended key usage, a
  random serial, and a validity from the host config (`[attestation]
  certificate_ttl`, bounded);
- answers with one PEM certificate and closes;
- rate-limits per guest (renewal at half life is a few requests; a burst is a bug
  or an attack), and **never logs a CSR or a certificate body**: it counts outcomes
  (issued, refused for size, key type, signature or rate).

It is part of the Firecracker provider, not a separate service: a second process
would hold the CA key beside the provider for no gain.

### 5. Certificates are short lived, and the guest renews them

Validity and the rate limit are host configuration, set at implementation from
the sandbox's own life, and are not decided here. The renewal protocol is
mediatore ADR-0049's (the guest renews with the same key at half life); an agent
stops one SVID lifetime after its certificate expires, so a signer that is down for
longer than that costs the guest its identity.

### 6. Pure Rust, no subprocess

The signer is code in the provider, not `openssl` run from it (ADR-0011: first-party,
pure-Rust, no FFI, no subprocess in a request path). The X.509 and ECDSA crates it
needs are chosen and recorded in the CHANGELOG when they are added, vetted as the
dependency rule requires; this ADR does not name them.

## Consequences

- A Firecracker guest's identity needs one administrator action per host: copy the
  host's `ca.crt` into the Provider (and the SPIRE bundle). That is the same
  assertion the libvirt administrator already makes, and it is what stops a host
  choosing its own anchor.
- Compromising a Firecracker provider yields the host's CA key, so the attacker can
  mint certificates for **that host's guests only**: the issuer CN is in every agent
  ID (mediatore ADR-0049 Decision 2). It cannot mint an identity another host's
  guests hold.
- API changes, code-first and regenerated: `Provider.spec.attestation.hostCa`,
  `FirecrackerMachineStatus.attestationSubject` and its claim mirror (a new CRD
  field on a CRD that does not exist yet, `FirecrackerMachine`, so only the Provider
  and claim types change in existing CRDs).
- A new installer stage and flags (`--attestation-ca-name`, `--rotate-attestation-ca`),
  a `[attestation]` host-config section, and a pure-Rust X.509 dependency.
- A host's CA cannot be rotated without a new name, and so without re-asserting it on
  the Provider and updating the SPIRE bundle in step. The window in which the old and
  new CAs are both trusted is an operational procedure, not a mechanism here.
- Threat model: a new trust-anchor class (per-host signing CAs, admin-asserted),
  a new key at rest on the host (`0600`, provider only), and a new bounded request
  path on vsock. Pass due when implemented.
- Not implemented. Open: the certificate lifetime and rate limit; the crates; the
  rotation procedure's overlap window; whether `hostCa` is also needed for a future
  class that has no vTPM but is not Firecracker (it is general to `x509pop`).
