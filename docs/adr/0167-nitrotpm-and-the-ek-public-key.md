<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0167: NitroTPM and the EK public key on EC2

- **Status:** Proposed
- **Date:** 2026-11-26
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 24 (EC2), roadmap 17 (pools), roadmap 18 (sealing
  isolation). Amends, for EC2 only, [ADR-0045](0045-vtpm-endorsement-key-certificate.md)
  (what is published as the TPM's identity) and
  [ADR-0049](0049-attestation-trust-anchors.md) (`ekTrustBundle`); keeps
  [ADR-0088](0088-first-boot-sealing-for-immediate-images.md) (first-boot
  sealing). Parallels [ADR-0165](0165-trusted-launch-vtpm-identity-on-azure.md)
  (Azure: a certified AK, no EK certificate). Uses
  [ADR-0174](0174-ec2-inframachine-and-a-first-party-aws-client.md).

## Context

Every other backend gives a `tpmEnabled` machine an **EK certificate**:
libvirt's swtpm and Cloud Hypervisor's vTPM through the guest (ADR-0045's
read-only path), vSphere host-side from `VirtualTpm`. banlieue publishes it
in `status.tpmEndorsementCertificates`, mirrors it to the `VirtualMachine`
and a bound claim, and a verifier checks it against the `Provider`'s
`ekTrustBundle` (ADR-0049). Azure has no EK certificate and publishes a
certified AK instead (ADR-0165).

EC2's NitroTPM has neither. What the roadmap 24 spike found (2026-11-24 to
2026-11-26):

1. **No EK certificate is offered.** AWS documents NitroTPM's EK only as a
   public key through the API below; no certificate or issuing CA is
   published. The spike did not read the TCG NV indices from a guest, so an
   unpublished certificate there is not ruled out (open, a `tpm2_nvread`
   away; it would not change this decision, which needs no certificate).
2. **The EK public key comes from the EC2 API.** `GetInstanceTpmEkPub`
   returns the instance's EK public key, RSA-2048 or ECC P-384, as DER or
   TPM2B, over SigV4-authenticated TLS (post-quantum key exchange, ADR-0170),
   for an instance ID. The DER from the API matched, byte for byte, the EK
   the guest's own TPM reports, for both key types.
3. **It is stable across stop and start.** The key the API returned, and
   the one the guest reported, were unchanged by a stop and start on every
   run. That two instances from **one** AMI get different EKs is expected of
   a TPM but was not yet observed (open; the sealing isolation test in
   Decision 4 needs those two instances anyway).
4. **Sealing works.** A template whose policy seals `COS_PERSISTENT` on
   first boot (ADR-0088) sealed it to the instance's NitroTPM in about 5 s,
   unlocked it in 0.6 s on later boots, and the volume key's digest was
   stable across a restart.
5. **NitroTPM needs the AMI to ask for it.** The image must be registered
   with `BootMode=uefi` and `TpmSupport=v2.0`, and the instance type must
   support NitroTPM.

## Decision

### 1. On EC2 the published identity is the EK public key, from the API

`EC2Machine.status` gains `tpmEndorsementPublicKeys`: the instance's EK
public keys as PEM `SubjectPublicKeyInfo`, RSA-2048 and ECC P-384, read by
the provider with `GetInstanceTpmEkPub` once the instance is running. It is
mirrored to the `VirtualMachine` and a bound `VirtualMachineClaim` beside
`tpmEndorsementCertificates`, which stays empty on EC2.

The source is the cloud's control plane, not the guest: ADR-0045's rule
holds (banlieue publishes, it never vouches), and the provenance is
recorded so a verifier knows what it is reading. The key is read
**host-side only**; nothing the guest says changes it.

### 2. `ekTrustBundle` does not apply on EC2

There is no certificate to chain. An `ec2` `Provider` that sets
`spec.attestation.ekTrustBundle` is refused at admission, rather than
silently ignored: the trust root on EC2 is the AWS API that banlieue's own
identity called, and that is a different statement from "a CA issued this
TPM's certificate". It is declared by the class, never inferred.

### 3. Node attestation on EC2 is the verifier's choice, stated

banlieue does not attest nodes. For a consumer using SPIRE, the `tpm`
attestor's **EK public-key allowlist** (`hash_path` with the SHA-256 of each
instance's EK public key) is sound on EC2, unlike on Azure (ADR-0165
Decision 3): the hash comes from the cloud API through banlieue, a source
other than the guest. The guide documents that path; a verifier that wants
credential activation (`TPM2_ActivateCredential`) can encrypt to the
published EK directly.

### 4. Sealing is unchanged

First-boot sealing (ADR-0088) needs a TPM, not a certificate: it is valid on
NitroTPM (finding 4). The sealing isolation test (roadmap 18 phase B: a
sealed volume attached to another instance does not unlock) must be run on
EC2 before sealed pools are offered there.

### 5. Image and class flags

`tpmEnabled` and `firstBootSealing` are accepted when the published AMI was
registered with `TpmSupport=v2.0` and `BootMode=uefi` (finding 5, recorded
per `Provider` in the `VMImage`'s status by ADR-0162), and the mapped
instance type supports NitroTPM (from `DescribeInstanceTypes`, published on
the `Provider`). `trustedBoot` (ADR-0051) is refused at admission on EC2 in
v1: UEFI Secure Boot on EC2 needs the AMI's own UEFI variable store, which
the publisher does not write. `installMode: Deferred` is refused
(ADR-0162).

## Consequences

- The `tpmEnabled` contract on EC2 is "a TPM whose EK public key AWS reports
  for this instance". Consumers that require an EK certificate must add a
  public-key path for EC2; this ADR states the difference instead of letting
  a certificate read fail silently.
- With Azure (ADR-0165), two of the four cloud and host backends lack EK
  certificates; an attestor that handles a certified AK and an API-provided
  EK public key covers both clouds.
- The provider's role needs `ec2:GetInstanceTpmEkPub`, scoped to its tagged
  instances.
- The sealing isolation test on EC2 is open before sealed pools ship here.

## Alternatives considered

- **Leave the EK unpublished on EC2.** Rejected: a sealed pool's members
  would have no identity a broker can bind a claim to.
- **Have the guest report its EK public key.** Rejected: a guest-reported key
  is what an attestor must not trust on its own; the API's copy is the
  independent one (finding 2).
- **Wrap the public key in a banlieue-issued certificate.** Rejected: banlieue
  would become a CA vouching for TPMs, which ADR-0045 rules out.
