<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0146: A Cloud Hypervisor guest may attest with x509pop, using a certificate its host signs with an attestation CA the administrator asserts

- **Status:** Proposed
- **Date:** 2026-11-19
- **Amended:** 2026-11-25 (Decision 8: verified against a real OpenBao by the OpenBao repository's `scripts/attestation-ca.sh` tests; the path length is pinned and sent as a string, all five parameters are required, no default policy on the token), 2026-11-20 (Decision 7: the CA certificate may be signed by an upstream CA, by CSR; Decision 8: the installer may have that one request signed by OpenBao, logging in as the host's own ServiceAccount with a token pinned to its own CA name)
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

### 7. The CA certificate may be signed by an upstream CA, by CSR; the key still never leaves the host

Decision 2 makes the CA self-signed, so the administrator must add each host's `ca.crt`
to the SPIRE server's bundle one host at a time. An organisation that runs a PKI
(here, OpenBao) can sign the host's CA instead, which chains every host CA to one root,
puts a signing event in the PKI's audit log, and lets the bundle hold that root alone.
The decision is how, without the key leaving the host:

- **The key is still born on the host.** The self-signed path (Decisions 2 and 5) stays
  the default and is unchanged. The CSR route is an alternative stage on its own flags,
  mutually exclusive with the default.
- **`--attestation-ca-csr`** creates the EC P-256 key as the provider's user (the same
  owner, modes and symlink rules as Decision 2) and writes a PKCS#10 request,
  `ca.csr` (public, `0644`), with the subject CN from `--attestation-ca-name`. It writes
  no `ca.crt` and **no `[attestation]` section**: the provider loads the CA at start and
  stops if it cannot (Decision 2), so a half-made CA must not look configured. The
  pending key and request live in a sibling directory, `<state root>/attestation-ca-pending/`
  (`0700`, `ca.key` `0600`), never in `ca_dir`, so a pending request cannot disturb a
  live CA. The stage prints the request's path and common name and what to ask the PKI.
- **The PKI side is an administrator's act, not banlieue's.** The administrator signs the
  request on a mount dedicated to node-attestation CAs, as an intermediate with
  `max_path_length=0`, `CA:TRUE` and `keyCertSign` only, and a validity they choose.
  banlieue holds no credential for the PKI and calls no PKI API (Non-Negotiable 1 is
  untouched, and the host gains no new identity).
- **That mount must not be one whose CAs the SPIRE server already uses to sign SVIDs.**
  `x509pop` trusts every certificate that chains to its bundle: a bundle holding the
  SVID-signing root would let each SPIRE server's own CA vouch for a node. The root in
  the `x509pop` bundle is its own trust anchor, in its own mount.
- **`--attestation-ca-import FILE`** takes the signed certificate (the host CA first, then
  any chain above it, PEM) and checks, before it writes anything: the first certificate's
  public key is the pending key's; its common name is the CSR's and inside the name
  alphabet; it is `CA:TRUE` with `pathLenConstraint` 0 and `keyCertSign`, and **not** able
  to sign anything else; it is valid now and its expiry is no further out than the
  pending request's cap (a certificate that has already expired is refused). Only then
  does it move key and certificate into `ca_dir` (a rename on one filesystem, then the
  directory sealed as in Decision 2) and write `[attestation]`. Failing any check leaves
  the pending directory as it was and the live CA untouched.
- **`ca.crt` is the host CA certificate alone**, so `Provider.spec.attestation.hostCa`
  (Decision 2) is still exactly one certificate and its admission rule is unchanged.
  `ca-chain.pem` beside it (public, `0644`) holds the host CA certificate and any
  intermediates above it, never a self-signed root, for the signer; it is absent for a
  self-signed CA.
- **The signer's reply carries the chain.** When `ca-chain.pem` exists the reply to a
  request is the guest certificate followed by the host CA certificate and the chain
  above it, up to but not including the root. The guest's agent presents those as its
  intermediates and SPIRE verifies to the root in its bundle, which is what lets the
  bundle hold the root and not one entry per host. For a self-signed CA nothing changes:
  the reply is the leaf alone. This extends the wire framing of mediatore ADR-0049
  Decision 6, which the guest helper is not yet built against; **it must agree before the
  helper is written**, and until it does the self-signed path is the one that is
  specified end to end.
- **Name uniqueness is still not checked here.** The issuer CN of the leaf is the host
  CA's CN and is in every agent ID, so two hosts with one CN lose the confinement of
  Decision 2. A PKI cannot enforce this by name constraints (they cover SANs, not the
  CN), so the administrator who signs checks the name against those already issued from
  the mount; the PKI's audit log is the record that they did.
- **Rotation under the route** is `--rotate-attestation-ca` with `--attestation-ca-csr`
  and a **new** name: it stages a new pending key and request and leaves the live CA
  serving. `--attestation-ca-import` then refuses to replace a live CA unless
  `--rotate-attestation-ca` is also given, and replaces it only after every check has
  passed (the old files are removed, then the new ones installed; the provider reads the
  CA once, at start, and is restarted after). The old
  certificate stays in the SPIRE bundle until the administrator removes it, as in
  ADR-0143 Decision 1.
- **Expiry becomes the PKI's.** A self-signed CA lives ten years; a signed one lives what
  its signer chose, and the provider does not renew it. `preflight` and `status` already
  report the CA's expiry; they also say how many days remain, and the provider's
  `Ready` condition goes `False` with reason `HostCaExpired` once the CA is past its
  `notAfter`, rather than signing certificates no verifier will accept. Renewal is
  rotation, with a new name.

Rejected: **a CA key pair generated in the PKI and delivered in a Kubernetes Secret.**
Each host's provider token would need `get` on a Secret (today the host Role reads no
Secret or ConfigMap, TB-2), the key would sit in etcd and its backups where anyone with
Secret read in the namespace could copy every host's CA, and a cluster administrator
would then speak for every host's guests, which removes the per-host blast radius that
Decision 2's naming exists to give. Rejected too: **reusing the SPIRE server's CA** for
this, for the reason in the third bullet above.

### 8. The installer may have the request signed by OpenBao in one run, logging in as the host's own ServiceAccount

Decision 7 leaves two runs and a person between them. For an organisation whose PKI is
OpenBao, the person's step can be mechanical **without any new secret reaching the host**:
after bootstrap the host already holds a Kubernetes identity, its own ServiceAccount token
(`/etc/banlieue/credentials/token`, one ServiceAccount per host). OpenBao can trust that
identity for exactly one thing, and hand back a temporary token that can do nothing else.
The goal is the smallest credential that can possibly work, in layers, so that no single
layer failing gives a host a CA it should not have.

**The flow.** After the credentials are in place, the installer (root, once, at an
operator's request, as in ADR-0067) reads the ServiceAccount token, logs in to OpenBao's
JWT auth for this cluster, posts the pending request to the node-attestation mount's
`root/sign-intermediate`, hands the response to Decision 7's `import` unchanged, writes
`[attestation]`, revokes the OpenBao token and forgets both. The flags are
`--attestation-ca-openbao-addr URL`, `--attestation-ca-openbao-mount MOUNT` and
`--attestation-ca-openbao-role ROLE`, with `--only attestation`; a request that already
exists is reused, so a failed signing never wastes a key.

**The layers.** OpenBao's side is owned and built by the OpenBao repository (an onboarding
script run by an administrator when the host is bootstrapped); banlieue specifies what it
must enforce and verifies none of it by assumption:

1. **Identity.** A JWT role on the cluster's mount bound to the host's exact ServiceAccount
   (`bound_subject`, the cluster's audience): no other host's token logs in to it.
2. **Authorization.** A policy on `sign-intermediate` and nothing else, with
   `allowed_parameters` pinning `common_name` to this host's CA name, `max_path_length` to `0`,
   `ttl` to a fixed value and `use_csr_values` to `false`, and `required_parameters` naming
   **all five** (a missing `max_path_length` would default to unlimited, and a missing `ttl` to
   the mount's). A parameter not listed is refused, so the request cannot ask for more. The ACL
   compares a value **and its JSON type** (an integer pin never matches a JSON number), so
   `max_path_length` is pinned as the string `"0"` and the installer sends it as a string. A host can
   only obtain a CA under **its own** name, which is what keeps the per-host confinement of
   Decision 2.
3. **The token.** `token_ttl` of five minutes at most, `token_num_uses` of `1` (one signing
   per login), `token_bound_cidrs` set to the host's address (a login from elsewhere is refused
   too) and no `default` policy: a stolen token is spent, expired or unusable from anywhere else,
   and can do the one signing and nothing else.
4. **Uniqueness, closed at the issuer.** Before creating anything, the script lists the
   certificates already on the mount and refuses a CA name that has been issued to anyone.
   Decision 7 left this unchecked; here the administrator's step is mechanical.
5. **The role does not outlive the onboarding.** A JWT role cannot count logins, so the
   script **deletes the role and its policy** once the import has succeeded, and a rotation
   makes a new pair under a new name. Until then the only thing a compromised host can do
   is log in again and obtain more CAs **under its own name**, each valid for the fixed
   TTL; that is recorded as an accepted risk in the threat model, bounded by layers 2, 3 and
   5 and visible in layer 7.
6. **The mount.** A dedicated mount whose root key is generated inside OpenBao and never
   leaves it, never the root whose CAs sign SVIDs (Decision 7), with `max-lease-ttl` set to
   the longest CA an operator would accept (the installer's own cap, Decision 7, is the
   second check). Optionally, name constraints on the issued CA so it cannot be used for TLS
   if a bundle is ever trusted elsewhere; they cover SANs, not the CN, so they are an extra
   layer, not a substitute for layer 2.
7. **Detection.** OpenBao's audit log records every login and every signing; an alert on a
   `sign-intermediate` call outside an onboarding is the cheap way to notice layer 5 failing.

**On the host.** The ServiceAccount token is read by root, sent only to the login endpoint
of the address given, never logged and never put in argv or the environment; the OpenBao
token lives in memory, is revoked with `auth/token/revoke-self` once used, and is never
written anywhere. `URL` must be `https`; the host's own trust store is used; there is no
flag to skip verification; connections have a timeout; an error body is never printed (it
can echo a request). **The response is evidence, not authority:** whatever OpenBao returns
goes through the full `import` (the pending key, the request's name, `CA:TRUE` with path
length 0 and `keyCertSign`, validity, chain links), so a wrong or hostile answer is refused
before anything is installed.

**A host without a cluster identity** (or an OpenBao without JWT auth for this cluster)
uses `--attestation-ca-openbao-token-file FILE` instead: a token an administrator minted
with the same pins (layers 2, 3 and 4), `ttl` five minutes and `num_uses` 1, read from a
`0600` file owned by root, never argv or the environment, and removed once used
(whether or not the signing worked).

**What banlieue gains and holds.** One outbound HTTPS exchange with one external service,
once, by the installer; no service, CRD, field or standing credential, and no PKI client in
the provider or the controller. The controller deliberately has no part in it: a controller
that could request signing for any host would be a single credential able to mint a CA for
every host, which is the concentration this design avoids. TB-4 and TB-6 of the threat model
gain a row, and the ServiceAccount's reach grows by one OpenBao login role for the length of
an onboarding.

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
- Decision 7 (2026-11-20) adds, for a host that wants a chain to an organisation's PKI,
  two installer flags (`--attestation-ca-csr`, `--attestation-ca-import`), one pending
  directory, one extra public file (`ca-chain.pem`) and one `Ready` reason
  (`HostCaExpired`). It needs no new CRD, no new field on the Provider, no new host
  credential and no PKI client in banlieue. The default self-signed path is unchanged.
- The CSR route trades the per-host bundle edit for an administrator's signing step and
  a root in the `x509pop` bundle: one more trust anchor class to name in the threat model
  (§3, §8), and the unchecked-uniqueness risk of Decision 2 moves to whoever signs.
- **Implemented** (Decision 7, 2026-11-20): `crates/banlieue-attestation-ca/src/pending.rs`
  (the pending key and request, and `import` with its checks, capped at ten years),
  `ca.rs` and `signer.rs` (the chain in the reply, expiry, a signer that refuses once its CA
  has expired), `crates/banlieue-host/src/{settings,stages}.rs` (`--attestation-ca-csr`,
  `--attestation-ca-import`) and the `HostCaExpired` hold in the provider. Open: whether
  mediatore ADR-0049 Decision 6 accepts a chain in the reply; whether the pending directory
  needs an expiry of its own so an abandoned request does not leave a key at rest.
- Decision 8 (2026-11-20) adds three installer flags (plus a token-file fallback), one
  outbound HTTPS exchange per onboarding, and a script in the OpenBao repository that
  enforces layers 1 to 5; it adds no service, no CRD, no field and no standing credential.
  The manual route of Decision 7 stays: it is what Decision 8 calls, and what an
  organisation without OpenBao uses.
- **Implemented in the installer** (Decision 8, 2026-11-20): `crates/banlieue-host/src/openbao.rs`
  (the client: https only, five parameters, no body or secret ever printed, a one-line
  secret read from a file) and `stages.rs::attestation_openbao`, tested against a fake
  server and a fake PKI. **The OpenBao side is not in this repository and nothing here
  verifies it**: layers 1 to 5 are the OpenBao repository's onboarding script, and the
  installer has not yet been run against a real OpenBao. Open: the reply format the
  installer reads (`certificate` and `ca_chain` from `sign-intermediate`, as OpenBao
  documents) confirmed against the running server; whether OpenBao's
  ACL parameter constraints, `token_num_uses` and `token_bound_cidrs` on a JWT role behave as
  layers 2 and 3 assume (the OpenBao repository verifies, against the running server); whether
  the role's deletion should be automatic after first success; whether a wrapped token is
  preferable for the fallback.
