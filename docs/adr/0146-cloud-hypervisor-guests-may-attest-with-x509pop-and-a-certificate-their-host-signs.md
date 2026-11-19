<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0146: A Cloud Hypervisor guest may attest with x509pop, using a certificate its host signs with an attestation CA the administrator asserts

- **Status:** Proposed
- **Date:** 2026-11-19
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0143](0143-firecracker-host-attestation-ca-and-guest-signer.md)
  (Decisions 1 to 7 apply to a Cloud Hypervisor host unchanged unless this ADR says
  otherwise), mediatore ADR-0049 (the attestor, `x509pop`; the signer's wire
  protocol, Decision 6), [ADR-0065](0065-cloud-hypervisor-vtpm-and-deferred-install.md)
  (the vTPM path this sits beside, and the vsock device every CH guest already has,
  Decision 5), [ADR-0049](0049-attestation-trust-anchors.md) Decision 10 (trust anchors
  are asserted by an administrator, never discovered), [ADR-0141](0141-shared-host-provider-layer.md)
  (shared host layer), [ADR-0067](0067-banlieue-host-install-subcommand.md) (the
  installer), [ADR-0139](0139-hosts-without-systemd-userdb-or-cgroup-v2.md)

## Context

A Cloud Hypervisor guest attests today with its vTPM: SPIRE's TPM attestor, an
endorsement key whose certificate the host's `swtpm_localca` issued, and a trust
bundle of those per-host issuers (ADR-0065 Decision 6, mediatore ADR-0018).

That is the wrong anchor on a **shared** host. The issuer is `swtpm-localca`, state the
host's other tenants' root can read and replace, created by a package script rather than
by banlieue. Whoever holds it can issue an endorsement certificate for any key, so a
guest's identity is only as good as everyone with root on that machine, and no part of
the key's custody is the provider's alone. The sandbox fleet runs on exactly such hosts:
a RHEL machine shared with unrelated workloads, where the administrator cannot vouch for
the TPM tooling.

mediatore ADR-0049 offers a narrower anchor for a guest without a vTPM: the guest makes
its own key, the host signs a certificate for it with a dedicated CA, and SPIRE's
`x509pop` attestor trusts a bundle of those per-host CAs. ADR-0143 designs the host half
for Firecracker (the CA, its assertion on the Provider, the subject, the signer). Nothing
in it is specific to Firecracker except the transport, and a Cloud Hypervisor guest
already has the same transport: every guest gets a hybrid vsock whose host end is
`<run dir>/vsock.sock`, with a listener at `vsock.sock_1024` for the install report
(ADR-0065 Decision 5, `report.rs`).

This is not a claim that `x509pop` is stronger than a trusted TPM. The signer still
trusts the host it runs on: a compromised host mints certificates for its own guests.
What changes is the **custody and the blast radius**: the signing key is created by
banlieue, owned by the provider's user alone, never readable by a guest uid, and its
common name is in every agent ID it signs, so a compromised host speaks only for its
own guests (mediatore ADR-0049 Decision 2).

## Decision

### 1. A Cloud Hypervisor host opts in; the vTPM path is untouched

A host opts in with an `[attestation]` section in its host config, written by the
installer (Decision 5). A host without it behaves exactly as today. Nothing about the
vTPM, the EK CA, `ekTrustBundle` or the TPM attestor changes, and one host may serve
guests that attest either way: which attestor a guest uses is the guest image's
configuration, and the host offers both. A guest with a vTPM and a signer can run either
agent configuration; the image chooses.

### 2. ADR-0143's CA, assertion, subject and signer apply as written

For a host that opted in:

- the attestation CA is created at install, EC P-256, as the provider's user, at
  `<state root>/attestation-ca/{ca.key (0600), ca.crt (0644)}`, directory `0700`, never
  overwritten by a re-run, rotated only by an explicit flag that issues a new name
  (ADR-0143 Decision 1);
- the administrator asserts the single-certificate `ca.crt` as
  `Provider.spec.attestation.hostCa`; banlieue never discovers or verifies it (ADR-0143
  Decision 2; the API field and its admission rules already exist);
- the certificate subject CN is the **`CloudHypervisorMachine`'s `metadata.uid`**, and
  `CloudHypervisorMachineStatus.attestationSubject` publishes it and is mirrored to the
  `VirtualMachine` and the claim by the path ADR-0143 added for Firecracker
  (ADR-0143 Decision 3). A `Recreate` makes a new machine and so a new subject;
- the signer answers on `<run dir>/vsock.sock_1025`, per guest, created with the guest's
  unit and removed when it stops, owned by the guest's uid; one connection is one
  request, bounded, EC P-256 only, takes only the public key, forces `CA:FALSE`,
  `digitalSignature`, no SAN, a bounded validity, rate-limited per guest, and never logs
  a CSR or certificate (ADR-0143 Decisions 4 and 5; mediatore ADR-0049 Decision 6);
- it is pure Rust with no subprocess (ADR-0143 Decision 6).

### 3. One signer implementation, in a crate both providers use

The CA, the CSR checks, the certificate builder and the vsock request loop live in **one
new crate, `banlieue-attestation-ca`**, a library with no Kubernetes dependency, used by
the Cloud Hypervisor provider now and the Firecracker provider when it is written. Two
copies of certificate-issuing code would be two places for the `CA:FALSE` rule to be
forgotten. The crate takes the CA directory, the subject and the request socket path;
it knows nothing of machines, classes or the cluster, so what to sign for stays the
provider's decision.

### 4. An opted-in host with no `hostCa` is not Ready

As ADR-0143 Decision 7: when the host config has `[attestation]` and the Provider has no
`spec.attestation.hostCa`, the Cloud Hypervisor provider sets `Ready=False`, reason
`HostCaMissing` (or `HostCaUnreadable`), names the field, and creates no guest while the
condition holds. A host that must keep running vTPM guests without the assertion does not
opt in. This is deliberately blunt: a guest started without the assertion would boot and
sign, then have no node-entry parent mediatore could compute, and fail late and silently.

### 5. The installer gains the same stage, on its own flags

`banlieue host cloud-hypervisor install` gains an attestation CA stage and
`--attestation-ca-name NAME` (letters, digits, dots, dashes, underscores; refused, not
replaced, otherwise; default the host's short name), `--attestation-certificate-ttl
DURATION` (bounded) and `--rotate-attestation-ca` (a new CA with a new name). Giving
`--attestation-ca-name` (or accepting the default via `--attestation`) is the opt-in;
it writes `[attestation]` (`ca_dir`, `certificate_ttl`) and prints the `ca.crt` path and
its common name for the administrator to assert. `--force` does not rotate the CA.
Preflight and `status` report the CA's presence, name and expiry, and refuse a CA key
that is not `0600` or not owned by the provider's user.

### 6. The guest half is not decided here

The in-guest helper (key, CSR, vsock request, renewal at half life, the agent
configuration with `discover_workload_path = true`) and the image layer belong to
mediatore ADR-0049 and the sandbox image build. This ADR fixes only the host contract the
helper talks to: vsock port 1025 on the guest's own hybrid vsock, the framing of
Decision 2. The port is a named constant shared with the install report's `1024`.

## Consequences

- A Cloud Hypervisor sandbox can attest without trusting the host's TPM tooling, using a
  CA whose key banlieue made and only the provider reads. It still trusts the host that
  signs.
- One more key at rest on the host (`0600`, provider only), one more bounded request path
  (vsock, per guest), one more trust-anchor class (per-host signing CAs, admin-asserted).
  The threat model needs a full pass when this is implemented (§3 assets, §5 boundaries,
  §6, §8).
- One administrator action per host: copy `ca.crt` onto the Provider and into the SPIRE
  server's bundle, with a name unique across the bundle (unchecked here; mediatore
  ADR-0050 moves uniqueness to the issuer).
- API: `CloudHypervisorMachineStatus.attestationSubject` and the status mirror for the
  kind (CRDs regenerate); no new spec field, no new CRD.
- A new library crate and a new dependency set (X.509 and ECDSA, pure Rust), vetted and
  recorded in the CHANGELOG as the dependency rule requires.
- A host that opts in is `Ready=False` until the Provider asserts the CA, on purpose.
- Existing hosts and guests are unchanged until they opt in.
- Not implemented. Open: the certificate lifetime and rate limit (ADR-0143 leaves them
  open); the crate choices; whether `[attestation]` should also be advertised on the
  Provider's failure domain so a consumer can tell which hosts offer it; rotation overlap
  window (an operational procedure, as in ADR-0143).
