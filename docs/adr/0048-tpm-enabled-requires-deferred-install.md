<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0048 — `tpmEnabled` requires a `Deferred` (or `Manual`) install

- **Status:** Accepted
- **Date:** 2026-09-23
- **Deciders:** Erick Bourgeois
- **Related:** Closes the open item in
  [ADR-0040](0040-deferred-install-for-vtpm-encryption.md) Decision 5;
  constrains [ADR-0039](0039-vsphere-vtpm-support.md); phase A3 of
  [roadmap 17](../../.github/community/17-ephemeral-vm-pools.md)
- **Revisit when:** [kairos-io/kairos#4556](https://github.com/kairos-io/kairos/issues/4556)
  ships — see [Expiry](#expiry-this-decision-has-a-known-end-date)

## Context

[ADR-0039](0039-vsphere-vtpm-support.md) gave `VMClass` a `tpmEnabled` flag
that attaches a vTPM to every VM of the class.
[ADR-0040](0040-deferred-install-for-vtpm-encryption.md) then established that
Kairos partition encryption is **install-phase-only**: `kcrypt` seals LUKS keys
to the TPM during `kairos-agent install`, so the sealing can only happen on a
disk that is laid down *after* a per-VM vTPM exists.

`InstallMode::Immediate` builds a template by installing once, centrally, then
marking the VM as a template. Every clone of that template boots into an
already-installed OS. There was never a per-clone install, so there was never a
moment at which `install.encrypted_partitions` could seal anything.

ADR-0040 Decision 5 recorded the resulting combination as an open question and
left it unenforced:

> `tpmEnabled: true` + `Immediate` clones fine, attaches a vTPM, and silently
> encrypts nothing.

Both halves of that sentence are true today, and the silence is the problem.
The VM comes up `Ready`. `kubectl describe` shows a vTPM attached. The class
says `tpmEnabled: true`. Nothing anywhere reports that the disk is plaintext.
An operator reading the CRs has every reason to believe the guest is encrypted.

For general-purpose VMs that is a latent papercut. For [roadmap
17](../../.github/community/17-ephemeral-vm-pools.md)'s sandboxes it is a
security defect: the whole point of a single-use sandbox is that its disk is
sealed to its own vTPM and unreadable to the next tenant of the same host. A
pool built on an `Immediate` image would hand out sandboxes whose "sealed"
workspace is readable by anyone who can read the datastore — and would report
them as healthy while doing it.

### Why not fix this by making `Immediate` encrypt

There is nothing to fix. The sealing has to happen while the disk is being
written and while the target TPM is present. An `Immediate` template is written
once, on a machine whose vTPM (if any) is not the vTPM any clone will have.
Re-sealing afterwards would mean rewriting the clone's disk at first boot,
which is what `Deferred` already is.

### Why not reject it at admission only

A `ValidatingAdmissionPolicy` (ADR-0007) cannot see both objects. `tpmEnabled`
lives on the `VMClass` and `installMode` lives on the `VMImage`; the
combination only exists when a `VirtualMachine` names both, and the API server
does not resolve refs for a policy. The check has to happen where both are
already in hand, which is the controller's reconcile.

This is the same shape as ADR-0035's provider/class resolution: the controller
is the only component that sees the whole graph.

## Decision

**1. The pairing is rejected in `banlieue-controller`'s `VirtualMachine`
reconcile**, after `VMClass` and `VMImage` are resolved and *before*
scheduling — the first point at which both are known, and before any placement
decision or infra CR is made.

**2. The check is a pure function**, unit-tested without a cluster:

```rust
#[must_use]
pub fn image_class_mismatch(tpm_enabled: bool, mode: InstallMode) -> Option<&'static str>
```

It returns `Some(message)` only for `tpm_enabled && mode == Immediate`.

**3. `Manual` passes.** ADR-0040 defines `Manual` as "identical mechanics to
`Deferred`, for a build that is not Kairos-driven" — the install is deferred to
each clone's first boot, so a per-VM vTPM does exist when the disk is written.
banlieue cannot verify what a `Manual` image actually does with it, and does
not try: `Manual` is the documented escape hatch, and treating it as an error
would break the only path available to a non-Kairos encrypted image.

**4. On mismatch: publish `Ready=False` with reason `ImageClassMismatch`, do
not create the infra CR, and requeue long.** No infra CR is the operative
half — the VM must not reach a provider at all, because a provider that
receives it will happily build the unencrypted thing.

The long requeue is safe because the existing `VMImage` and `VMClass` watches
(ADR-0035) re-trigger the VM immediately when either object is edited. The
requeue is a backstop, not the recovery path.

**5. `Ready` does not gain a new dependency.** This sets `Ready=False`
directly on a terminal misconfiguration; it does not add `tpmEnabled` to the
normal readiness computation. Nothing changes for any VM that is not in this
exact pairing.

**6. A `VMImage` with no `template` is treated as `Immediate` and rejected.**
`VMImageSpec.template` is `Option`, and is documented as "only meaningful for
`Url` sources; ignored for `Template` / `BackingFile`". An image with no
template is therefore a **pre-built disk** — an existing backend template, or
a qcow2 backing file — and a pre-built disk is a *pre-laid* disk in exactly
the sense ADR-0040 rules out. [Roadmap
15](../../.github/community/15-vsphere-disk-image-import.md) states the same
thing independently for its own disk-image import path: "a clone of a pre-laid
disk can never be sealed to its own vTPM. A disk image is a pre-laid disk."

The absent case takes `InstallMode`'s own `#[default]`, which is already
`Immediate`, so this needs no special-casing in the predicate — but it is a
deliberate decision rather than an accident of the type, and it is recorded
here because the two errors are not symmetric. **Fail closed:** wrongly
rejecting a sealable image is visible, reported, and fixed by one field edit,
while wrongly accepting an unsealable one is silent and is the entire defect
this ADR exists to remove.

This does mean a `tpmEnabled` class can no longer pair with a `BackingFile`
image at all. That is correct on today's backends — libvirt's `BackingFile`
path lays down a disk built elsewhere — and it is the same conclusion roadmap
15 reached for vSphere `DiskImage` import. If a backend ever gains a way to
seal a pre-laid disk per VM, this is the decision to revisit, and roadmap 15's
open question Q4 is where that evidence would come from.

## Consequences

**A silently-unencrypted VM becomes a loudly-broken one.** The failure moves
from "provisions successfully and lies" to "never provisions and says why".
That is the entire point, and it is a behaviour change: a
`tpmEnabled` + `Immediate` `VirtualMachine` that reconciles today will stop
reconciling after this lands.

**This can break an existing deployment on upgrade.** Anyone relying — knowingly
or not — on that pairing gets `Ready=False` on their next reconcile. This is
judged correct rather than unfortunate: the VMs in question were never
encrypted, so nothing that was true before stops being true. Only the reporting
changes. The condition message names both objects and the ADR, so the fix
(switch the image to `Deferred`, or drop `tpmEnabled` from the class) is
readable off `kubectl describe` without reading code.

**Roadmap 17 phase A5 is unaffected but adjacent.** The vTPM endorsement
certificate work still has to record a cert per machine; this ADR only stops
the case where the vTPM is decorative.

**The check is backend-neutral.** It reads only `VMClass.spec.tpmEnabled` and
`VMImage.spec.template.installMode`, both provider-agnostic, so it constrains
vSphere, libvirt and any future backend identically without any of them
participating. No provider code changes.

## Expiry: this decision has a known end date

**Everything above rests on one upstream fact: Kairos encryption is
install-phase-only.** That is true of Kairos today, and it is the sole reason
`Immediate` cannot work — not anything about vTPMs, vSphere, libvirt or
banlieue.

[kairos-io/kairos#4556](https://github.com/kairos-io/kairos/issues/4556)
(open, `area/immucore` + `area/kcrypt` + `area/security`) proposes exactly the
missing capability: an `OpEncryptPending` DAG step in immucore that reads an
encryption policy from `COS_OEM`, detects plaintext partitions marked for
encryption, and encrypts them **before mounting, on first boot**, failing the
boot closed if it cannot. Its own stated goal is this ADR's rejected case:

> a Kairos image can be installed once, captured as a template or raw image
> with **no TPM and no key material in it**, and every clone or instance
> encrypts itself against its own TPM on first boot.

If that lands, `tpmEnabled: true` + `Immediate` stops being a silent lie and
becomes the *preferred* shape — one install amortised across every clone,
instead of paying a full install per VM — and roadmap 17's
`warmReplicas` sizing, which is dominated by install time, changes with it.

**This ADR should then be superseded, not amended**, because its Decision
inverts rather than narrows. The replacement needs to answer two things this
one did not have to: how banlieue knows an image was *built* with the pending
-encryption policy in `COS_OEM` (a `VMImage` assertion, or something readable
back from the artifact — not an assumption), and what `GuestReady` means when
first boot now includes an encryption step that can fail closed.

Until then the rejection stands, because the capability does not exist in any
released Kairos and a check that anticipates one would be the same silent
failure in the other direction.

**`Manual` remains an unverified assertion.** An operator who sets
`installMode: Manual` on an image that really does bake a pre-installed disk
gets the old silent behaviour back. That is the accepted cost of having an
escape hatch at all, and it is recorded in the threat model rather than
pretended away.
