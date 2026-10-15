<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0100: Sandbox guest software is consumer image content; a template carries no per-VM identity

- **Status:** Proposed
- **Date:** 2026-10-15
- **Deciders:** Erick Bourgeois
- **Amends:** [ADR-0049](0049-attestation-trust-anchors.md) Decision 3, on
  acceptance (where the agent's keys are generated).
- **Related:** [ADR-0088](0088-first-boot-sealing-for-immediate-images.md)
  (members become clones of one installed disk),
  [ADR-0097](0097-api-ends-at-virtualmachineclaim.md) (Decision 5, the
  scope test), [ADR-0049](0049-attestation-trust-anchors.md) Decision 9 (the
  in-guest agent lives in its own repository),
  [ADR-0082](0082-no-inbound-to-sandboxes.md) (no inbound path),
  [ADR-0037](0037-vmimage-layered-cloud-config.md) (layered cloud-config),
  [ADR-0045](0045-vtpm-endorsement-key-certificate.md) (per-VM EK
  certificate), [ADR-0046](0046-virtualmachinepool.md) (pools).

## Context

An agent sandbox needs three pieces of software running in every pool member
before any claim binds it:

- the **in-guest agent**, which sets up and jails the subject once a claim
  arrives;
- a **SPIFFE workload-identity agent** (SPIRE), which attests the VM through
  its vTPM and issues the subject's identity;
- a **forwarder**, which carries the subject's outbound connections to the
  pool's egress gateway under that identity.

Two questions follow: who puts that software into the image, and what is
banlieue allowed to assume about an image that carries it.

### Who owns the software

ADR-0097 Decision 5 is the test: a feature that only means something once a
subject, a token or an agent is involved belongs to the consumer. All three
components fail it. Each one exists to serve a subject, and none does
anything useful on a VM that nobody has claimed. ADR-0049 Decision 9 already
put the in-guest agent in its own repository for the same reason: it runs in
the guest, and its release cadence is the image's, not banlieue's.

banlieue already has everything a consumer needs to get the software into
an image, with no new field:

- a `Url` source built by `banlieue-imagebuilder` takes an ordered list of
  cloud-config layers, `VMImage.spec.cloudConfigs` (ADR-0037), so the
  consumer's install steps are one more layer;
- a `BackingFile` or `Template` source is a disk the consumer built
  themselves;
- a `VirtualMachinePool` names the image through its member template
  (`spec.template`, and `imageRef` inside it), as it does for any other
  image.

### Why template safety is now banlieue's problem

Up to now, every `tpmEnabled` pool member ran its own install (`Deferred`,
ADR-0040). Anything an install generated was per-VM by construction, and
ADR-0049 Decision 3 relies on that: *"the agent's keypair and AK are
generated during the per-VM install, never shipped in an image"*.

ADR-0088 changes the premise. A `firstBootSealing` image is installed once,
and every member is a clone of that one disk. Anything the install or the
image build writes to disk is now **shared by every member of every pool
built from the image**: a private key, a cached identity document, a host
key. ADR-0049 Decision 3 called this the `Immediate` hazard and left it to
the operator (ADR-0040 Decision 5). For sandbox pools it can no longer be
left there, because cloning is now the recommended shape.

Some of this is already safe for structural reasons:

- **No TPM-bound key can exist in the template.** ADR-0088's template is
  installed with no TPM attached, and each clone gets a fresh vTPM. An
  attestation key made against one TPM cannot be activated against
  another's EK, so a TPM-bound key cannot be cloned usefully, even by
  mistake.
- **Sealing runs before userspace.** `encrypt-pending` reformats and seals
  `COS_PERSISTENT` in the initramfs on first boot (ADR-0088 Context). Anything
  the guest writes to `COS_PERSISTENT` after that is on a volume sealed to
  this member's own vTPM.

What is **not** structurally safe is any key or identity document that does
not depend on the TPM: the SPIFFE agent's own private key and cached SVIDs,
the forwarder's state, SSH host keys, a machine ID, any join token. If an
image build or an install step writes one of these, every clone carries the
same copy.

## Decision

1. **banlieue ships none of the sandbox guest software.** No banlieue
   binary, crate, built-in imagebuilder layer or CRD field installs, configures
   or versions the in-guest agent, the SPIFFE agent or the forwarder. The
   consumer supplies them as image content, through the existing
   `VMImage.spec.cloudConfigs` layers or a pre-built source. banlieue imports
   none of them and does not depend on any of them.

2. **A template carries no per-VM identity.** An image used for a
   `tpmEnabled` pool must contain no private key material, no cached
   identity document (SVID, certificate, token) and no per-VM identifier.
   Public trust material, such as a SPIFFE trust bundle or a CA certificate,
   is allowed: it is the same for every member by design.

3. **Per-VM identity is generated on the member's first boot, after
   first-boot sealing, onto a sealed partition.** Keys and state that must
   survive a reboot go to `COS_PERSISTENT` (or another partition listed in
   `firstBootSealing.partitions`). They are never written to `COS_OEM` or to
   any other partition that ADR-0088 leaves plaintext.

4. **This amends ADR-0049 Decision 3, on acceptance.** "Generated during the
   per-VM install" becomes "generated on the member's first boot, onto a
   sealed partition, never in an image". For a `Deferred` image the two are
   the same event, so nothing changes there. Only the `Immediate` +
   `firstBootSealing` path gains a rule it did not have.

5. **The image opens no inbound path.** ADR-0082's share for banlieue is
   unchanged: banlieue injects no SSH keys and no listener configuration
   through guest data. Disabling SSH and every other listener *in the image*
   is the consumer's responsibility under Decision 1, and verifying it is
   part of the consumer's image test, not banlieue's.

6. **banlieue does not inspect images for Decision 2.** It cannot tell a
   public certificate from a private key inside a consumer layer, and a
   check that looks for one would give a false sense of coverage. Decisions 2
   and 3 are a contract on the image author, documented on the guide and
   recorded in the threat model, in the same class as ADR-0088's `Asserted`
   sources and ADR-0040's `Manual`.

## Consequences

**banlieue stays free of agent code and releases.** The guest stack can be
rebuilt, patched or replaced (a different forwarder, a different identity
agent) by rebuilding a `VMImage`, without a banlieue release. This keeps the
line ADR-0097 drew in the same place.

**A misbuilt template is the consumer's failure to catch, but it fails
loudly in the common case.** A cloned TPM-bound key fails attestation
against the clone's own EK. A cloned non-TPM key does not fail on its own:
every member would present the same key, and only the consumer's identity
layer can notice it (for example, by binding each agent's identity to the EK
that ADR-0045 publishes). This residual risk belongs in the threat model,
next to `Asserted` and `Manual`.

**Claim-time secrets stay off the disk.** A pool member exists before any
claim (ADR-0049 Context), so nothing subject-specific can be in the image or
in its user-data. The subject's identity is issued to the running guest
after attestation. ADR-0088's warning that per-VM user-data lands in
plaintext on `COS_OEM` therefore does not reach subject credentials. It
still applies to anything else an operator puts in `spec.userData`.

**ADR-0049 Decision 4 is not settled here.** The consumer may attest through
the SPIFFE agent's TPM node attestation instead of the broker's
quote-over-nonce handshake. banlieue's contract would not change: it still
publishes the EK certificate, the nonce and the trust anchors
(`Provider.spec.attestation.ekTrustBundle`). Whether Decision 4 is
superseded is decided where the handshake is built, and recorded here only
if banlieue's published fields change.

**On acceptance this ADR requires:**

- ADR-0049 Decision 3 amended, with an `Amended:` bullet pointing here;
- the pools guide (`docs/src/guides/virtualmachine-pools.md`) to state
  Decisions 2 and 3 for anyone building a sandbox image;
- a full threat-model pass covering the cloned non-TPM-key residual, next to
  the `Asserted` and `Manual` rows.

No CRD, controller or provider change is needed. This ADR is accepted
together with ADR-0088 or after it, and it has nothing to govern before
then: until ADR-0088 is accepted, every `tpmEnabled` member runs its own
install.
