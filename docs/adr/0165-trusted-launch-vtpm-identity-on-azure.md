<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0165: Trusted Launch, the vTPM, and its identity on Azure

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 23 (Azure), roadmap 17 (pools), roadmap 18 (sealing
  isolation). Amends, for Azure only, [ADR-0045](0045-vtpm-endorsement-key-certificate.md)
  (the EK certificate read path) and [ADR-0049](0049-attestation-trust-anchors.md)
  (`ekTrustBundle`); keeps [ADR-0088](0088-first-boot-sealing-for-immediate-images.md)
  (first-boot sealing). Parallels ADR-0167 (EC2 NitroTPM: an EK public key,
  no certificate). Confidential VMs (SEV-SNP, TDX) are out of scope.

## Context

`tpmEnabled` pools rest on two things a vTPM provides: a key that never
leaves it (so a disk sealed on first boot unlocks only on that VM, ADR-0088),
and an identity a verifier can trust (ADR-0045 publishes the vTPM's **EK
certificate** on the infra CR; ADR-0049's `ekTrustBundle` tells a verifier
which CAs issue those certificates). On libvirt and Cloud Hypervisor, swtpm
writes an EK certificate at the TCG index `0x01c00002`; on vSphere it is read
host-side.

Azure Trusted Launch gives every VM a vTPM. Roadmap 23 assumed it would look
the same. The phase 0 spike read it from inside real guests with `tpm2-tools`
(2026-11-24):

1. The vTPM is Microsoft's (manufacturer `MSFT`), present as `/dev/tpm0` and
   `/dev/tpmrm0`, with Secure Boot on.
2. **There is no EK certificate.** The TCG EK-certificate indices
   (`0x01c00002`, `0x01c0000a`, `0x01c00012`, `0x01c00014`) are not defined;
   reading them is a TPM error.
3. The only NV index defined is **`0x01C101D0`**, holding an **attestation key
   (AK) certificate** issued by Azure: subject
   `CN=<vm id>.TrustedVM.Azure.windows.net`, one-year validity, `CA:FALSE`,
   Digital Signature only. Persistent handles `0x81000003` (the AK, by its
   certificate) and `0x81010001` (where an EK conventionally sits) exist.
4. The AK certificate **chains**, verified with `openssl verify`, through a
   regional issuing CA (`Azure Cloud Virtual TPM CA - NN`) and a policy CA
   (`Azure Cloud Virtual TPM CA 2025`) to **`Azure Virtual TPM Root
   Certificate Authority 2023`** (self-signed, 2023 to 2048). The root matched
   the copy Microsoft embeds in its Trusted Launch FAQ (serial, subject key
   identifier, validity, key). The intermediates the guest's chain used are
   **not** the ones that FAQ documents.
5. A SPIRE `tpm` node attestor configured with EK-certificate CAs only
   (`ca_path`, no `hash_path`) cannot admit such a guest: there is no EK
   certificate to present. The guest attested with a one-time join token
   instead, over the same network path.

## Decision

### 1. On Azure the published identity is the AK certificate and its chain

The `AzureVirtualMachine` publishes the guest's AK certificate (read by the
guest from `0x01C101D0` and reported through ADR-0045's read-only path, as
the EK certificate is elsewhere) in the field ADR-0045 uses, with its kind
recorded as `AttestationKey` rather than `EndorsementKey`. ADR-0045's rule
holds: banlieue publishes, it never vouches.

### 2. `ekTrustBundle` generalizes to the vTPM's root

ADR-0049's `Provider.spec.attestation.ekTrustBundle` keeps its shape and
meaning ("the CAs that issue this backend's vTPM identity certificates"). For
an Azure `Provider` the admin sets it to **`Azure Virtual TPM Root
Certificate Authority 2023` only**, pinned by fingerprint in the guide.
Intermediates are **not** pinned: they are regional, rotate, and are not all
documented (finding 4); a verifier resolves them by AIA or from a bundle it
refreshes.

### 3. Node attestation on Azure is the verifier's choice, stated

banlieue does not attest nodes. For a consumer using SPIRE, two paths work
with what Azure provides, and the guide documents both:

- **AK-certificate attestation**: the verifier checks the AK certificate
  chain to the pinned root and a TPM quote (or `TPM2_Certify` of a fresh key)
  signed by that AK. This needs an attestor that understands AK certificates;
  the `tpm` attestor used on libvirt does not.
- **EK public-key allowlist**: the `tpm` attestor's `hash_path` with the
  SHA-256 of each VM's EK public key. Only sound when the hash comes from a
  source other than the guest itself; on Azure that is the AK-certified
  quote above, so this reduces to the first path.

A one-time join token remains possible for tests; it is not an identity.

### 4. Sealing is unchanged

First-boot sealing (ADR-0088) needs a vTPM, not a certificate: it is valid on
Trusted Launch. The sealing isolation test (roadmap 18 phase B: a sealed disk
copied to another VM does not unlock) must be run on Azure before sealed pools
are offered there.

### 5. Image flags on Trusted Launch

`tpmEnabled` and `firstBootSealing` are accepted when the image definition
declares `SecurityType=TrustedLaunch` (ADR-0162). `trustedBoot` (a UKI under
Secure Boot, ADR-0051) is accepted only with an image signed under a key the
Trusted Launch firmware trusts (the Microsoft UEFI CA); unsigned UKIs are
refused at admission. `installMode: Deferred` is refused (ADR-0162).

## Consequences

- The `tpmEnabled` contract on Azure is "a vTPM whose AK Azure certifies",
  not "an EK certificate". Consumers that check EK certificates must add an
  AK path for Azure; this ADR makes the difference explicit instead of
  letting an EK read fail silently.
- Azure and EC2 (ADR-0167) both lack EK certificates; an attestor that
  handles a certified AK (Azure) and an API-provided EK public key (EC2)
  covers both clouds.
- The sealing isolation test and a sealed first boot inside Azure's
  provisioning window (ADR-0163 Decision 5) are open before sealed pools ship
  on Azure.

## Alternatives considered

- **Treat the AK certificate as if it were an EK certificate.** Rejected: an
  AK is a signing key, not an endorsement key; protocols that encrypt to the
  EK (credential activation) cannot use it.
- **Use Azure's own guest attestation service.** Possible for a consumer; it
  puts an Azure service in the trust path and a call out from the guest,
  which ADR-0163's read-only stance avoids for banlieue itself.
