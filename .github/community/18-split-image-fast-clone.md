# 18: Split-image fast clone — verified base + per-VM sealed volume

> **Goal.** Cut a `tpmEnabled` pool member's provisioning time from minutes
> (a full per-clone OS install) to seconds, on every backend, **without
> giving up per-member disk-encryption isolation or per-member vTPM
> attestation identity**. The mechanism is a split disk: an immutable,
> integrity-verified base OS that is fast-cloned (CoW / linked clone /
> reflink), plus a per-VM encrypted data volume created empty at first boot
> and sealed to that VM's own freshly created vTPM.
>
> **Stop condition.** A `VirtualMachinePool` of `tpmEnabled` members built
> from a split image goes clone → `GuestReady=True` in **≤ 30 seconds** per
> member on libvirt and vSphere, each member publishing its own EK
> certificate (ADR-0045) and its own sealed volume — and a live test proves
> that member A's sealed volume key **cannot** be unsealed by member B's
> vTPM. Memory forking stays rejected (ADR-0052 stands).

Baseline when written: `677be59` (2026-09-24). ADRs 0001–0055 exist;
0056–0058 are reserved by roadmap 15 and 0059–0065 by roadmap 09. **This
roadmap reserves ADR-0066 to ADR-0071.**

## Why

Roadmap 17's live vSphere numbers (2026-09-23): a 2-member `GuestReady`-gated
pool reached `Warm=True` in 130.3s — **~9.6s** of clone + vTPM attach + power
on, and **~120.7s** of guest boot, install and announce. The per-clone
Deferred install is >90% of the cost, and it exists for exactly one reason:
Kairos can only encrypt at install time (ADR-0040), so the only way to get a
per-VM sealed disk today is to run the installer once per VM.

The insight this roadmap acts on: **the base OS needs no confidentiality at
all.** It is identical across the fleet and contains no secrets — what it
needs is *integrity* (nobody swapped it), which dm-verity provides at zero
per-clone cost. Only the per-VM data (workspace, agent keys, credentials)
needs encryption, and an **empty** LUKS volume created and sealed at first
boot costs seconds, not minutes. Encrypting the shared base was never buying
member-to-member isolation anyway — clones of one ciphertext share its key by
construction.

## The model

```
             built once per image revision                per member, at clone time
  ┌──────────────────────────────────────────┐   ┌────────────────────────────────────┐
  │ base disk (read-only, immutable)         │   │ CoW/linked/reflink clone of base   │
  │  - OS + agent runtime, NO secrets,       │──▶│  (seconds; shares base blocks)     │
  │    NO machine identity                   │   ├────────────────────────────────────┤
  │  - dm-verity hash tree; root hash        │   │ fresh vTPM (new EK, new state)     │
  │    pinned by the image artifact          │   ├────────────────────────────────────┤
  └──────────────────────────────────────────┘   │ empty per-VM volume; first boot:   │
                                                 │  LUKS format + key sealed to THIS  │
                                                 │  VM's vTPM; then GuestReady        │
                                                 └────────────────────────────────────┘
```

Integrity for what is shared; confidentiality for what is private; a fresh
TPM identity per member. The attestation chain (ADR-0045 EK certificate on
the claim, ADR-0049 quote verification) is **unchanged** — if anything it
gets stronger, because the verity root hash gives the verifier a statement
about the base image that today only "banlieue built it" provides.

## Fixed constraints (not up for re-litigation here)

| Constraint | Source |
|---|---|
| No memory forking, on any backend. Instant Clone, snapshot-restore fan-out and fork-from-golden all copy the parent's RAM — including its unsealed volume key — so no vTPM swap restores isolation afterwards. This roadmap is **disk-only** fast cloning. | ADR-0052 |
| Never clone TPM state. Fresh vTPM per member: swtpm keyed by domain UUID (libvirt), fresh `tpmstate0` at create (Proxmox), vTPM attached per clone while powered off (vSphere, ADR-0039). | ADR-0040, roadmap 17 §D/§E |
| Never clone a disk that has been *sealed*. The split base is cloneable precisely because nothing on it is sealed and nothing on it is secret. A base image that grows either property is a build failure. | ADR-0040 |
| The EK certificate gate stands: a `tpmEnabled` member is unbindable until it publishes `status.tpmEndorsementCertificates` (`TpmEndorsementPending`). | ADR-0045 |
| CR-to-CR only; pools and claims never talk to a provider. Everything here lands underneath the existing `VirtualMachine` / infra-CR contract. | 01-DECISIONS |
| The hypervisor operator can read the base disk. Already true and already semi-trusted (threat model §4); under this model it is *by design*, because the base carries no secrets. The claim the threat model makes changes from "disk encrypted" to "data volume encrypted, base verified" — phase G records that honestly. | threat model §4/§8 |

## Security invariants (every phase must preserve all five)

1. **The base image contains no secrets and no identity.** No machine-id, no
   SSH host keys, no tokens, no enrolled TPM material, no pre-created
   keyslots. Enforced at build time (phase A) with a scanner, not a
   convention.
2. **One volume key per member, sealed to that member's vTPM only.** No
   fleet key, no escrow copy, no duplicable sealing keys. The negative test
   (cross-member unseal fails) is a live-test deliverable, not an assumption.
3. **The base is integrity-bound to the image artifact.** dm-verity root
   hash travels with the `VMImage` revision and reaches the guest through a
   channel the guest can't be lied to about cheaply (kernel cmdline in the
   boot config the clone gets; signed UKI when ADR-0051's stall is resolved).
4. **`GuestReady` implies sealed.** The ADR-0043 phase marker is written
   only *after* the per-VM volume exists, is LUKS-formatted, is sealed to
   the local vTPM, and is mounted. A member that cannot seal never
   announces.
5. **Fresh vTPM per clone, never migrated, never duplicated.** Same rule as
   today; the split changes what is on the disk, not the TPM lifecycle.

## ADR reservations

| ADR | Decision it will record | Phase |
|---|---|---|
| 0066 | Split-image mode: the `VMImage` contract (base artifact + verity root hash + per-VM volume spec) and how it amends ADR-0040's install modes and ADR-0048's `tpmEnabled ⇒ Deferred` gate — split becomes the second sealed mode | 0 |
| 0067 | First-boot sealing mechanism: what runs in the guest to create, seal and mount the per-VM volume (candidates: `systemd-repart` `Encrypt=tpm2` + `systemd-cryptenroll`; Kairos `kcrypt` if it grows first-boot support; custom initramfs hook) and how failure holds back `GuestReady` | A |
| 0068 | Base integrity: dm-verity layout, root-hash pinning, and the interim non-UKI trust statement (what an unsigned cmdline does and does not prove) — successor to the ADR-0051 stall | A |
| 0069 | vSphere linked-clone provisioning path (`createNewChildDiskBacking` off a frozen template snapshot) and its interaction with the imported pre-laid base (roadmap 15) | D |
| 0070 | Proxmox split-image provisioning (linked clone `full=0`, fresh `tpmstate0`) — written with roadmap 06 when that provider starts | E |
| 0071 | Cloud Hypervisor split-image provisioning (reflink base, raw per-VM volume) — written with roadmap 09 phase 0's outcome | F |

## 0. Decision gate (ADR-0066)

The whole roadmap hangs on one contract decision, so it goes first and alone:

- [ ] **What a split `VMImage` is.** Proposal: `installMode` gains a fourth
      value (working name `Split`) whose artifact is a *disk*, not an ISO —
      a verity-protected base image plus metadata (root hash, per-VM volume
      size/mount contract). This is the first sealed mode whose artifact is
      pre-laid, which is why ADR-0048's gate must be amended, not just
      bypassed: the gate's reasoning ("a pre-installed template cannot be
      encrypted per VM") stays true *for the base* and becomes irrelevant
      *for the data volume*.
- [ ] **Whether the OS stack is still Kairos.** Kairos encryption is
      install-phase-only (ADR-0040) and its trusted-boot path stalls on
      vSphere (ADR-0051). If Kairos cannot do first-boot sealing on a
      verity base, the split image may be a plain immutable distro with
      `systemd-repart`/`systemd-cryptenroll` — which is exactly this model,
      upstream. Decide, don't drift.
- [ ] **What happens to `Deferred`.** Nothing — it remains the mode for
      persistent, fully-encrypted-disk VMs. Split is for cattle: pools,
      sandboxes, anything where the base being fleet-shared is acceptable.
      The two modes coexist; ADR-0066 says which classes may use which.

**Exit:** ADR-0066 Accepted; ADR-0048's check updated in the same change
(`image_class_mismatch` learns the new mode); threat-model pass for the ADR.

## A. Image build + first boot (ADR-0067, ADR-0068)

- [ ] Extend the image pipeline (`banlieue-imagebuilder` / `vm-build`) to
      produce the split artifact: base disk with verity hash tree, root hash
      recorded in `VMImage.status.buildArtifact`, boot config that
      references it.
- [ ] First-boot unit: create the per-VM volume (the provider attaches an
      empty disk; the guest formats it), LUKS2 + key sealed to the local
      vTPM, mount, **then** write the ADR-0043 phase marker and export the
      EK certificate (ADR-0045 path unchanged).
- [ ] Build-time secret/identity scanner over the base image (invariant 1):
      fails the build on machine-id, host keys, keyslots, TPM state.
- [ ] Boot-time measurement: sealing failure and verity failure are loud —
      the guest never reaches `GuestReady`, and the marker channel carries a
      distinguishable phase so the pool's `provisioningTimeoutSeconds` reaps
      it with a reason.

**Exit:** an image revision builds reproducibly; booted by hand on one
backend, it seals an empty volume in single-digit seconds and announces.

## B. Core: API, controller, pool (no new controller)

- [ ] `banlieue-api`: the ADR-0066 fields on `VMImage` (and whatever the
      per-VM volume contract needs on `VMClass`); `regen-crds`.
- [ ] `banlieue-controller`: amend `image_class_mismatch` (ADR-0048) —
      `tpmEnabled` + split is valid; `tpmEnabled` + `Immediate` stays
      rejected.
- [ ] Readiness: nothing new — `GuestReady` + `TpmEndorsementPending`
      already gate exactly right, because phase A moved the marker to
      after-seal. Verify with unit tests, don't add a parallel condition.
- [ ] `VirtualMachinePool`: no code change expected; re-derive the sizing
      guidance (roadmap 17 §B1) since `install minutes` collapses — warm
      pools shrink or disappear for split classes. Update the pool guide.

**Exit:** a split-image `VirtualMachine` schedules; a wrong pairing is
rejected with the amended reason; `cargo-quality` green.

## C. libvirt (first live target)

The backing-file clone that roadmap 17 §D removed for TPM classes ("create an
empty volume, attach the ISO, install") **comes back** for split classes —
it was banned because the installed disk was sealed, and a split base is not.

- [ ] `LibvirtMachine` split path: qcow2 overlay on the base volume
      (backing file), plus one empty per-VM volume, plus the existing fresh
      swtpm (domain UUID keying, ADR-0050 teardown flags — both unchanged).
- [ ] Verity root hash into the domain's kernel cmdline / boot config per
      ADR-0068.
- [ ] `VMImage` reconciler: upload/refcount the base volume per pool the way
      ISO artifacts are handled today; never delete a base that overlays
      still reference.
- [ ] Live tests (`tests/live_*`): clone-to-`GuestReady` timing; **the
      cross-member negative test** — export member A's LUKS header, prove
      unseal fails on member B (invariant 2); teardown removes overlay,
      data volume, swtpm state, NVRAM (existing `MANAGED_SAVE|NVRAM|TPM`).

**Exit:** stop-condition numbers on a real libvirt host, negative test
included.

## D. vSphere (ADR-0069)

- [ ] Linked clone: template imported as a pre-laid disk (this is roadmap
      15's import path — the two roadmaps meet here; the base is *supposed*
      to be pre-laid now), snapshot frozen, clone with
      `createNewChildDiskBacking`. Fall back to full clone of the (small)
      base where linked clones are operationally unwanted; the model does
      not depend on the linked-ness, only the install-skip.
- [ ] Per-clone: attach fresh vTPM (ADR-0039, unchanged), add one empty
      VMDK for the data volume, power on. `ensure_vm`'s cold-clone
      sequencing survives intact — this is why ADR-0052 stands: we sped up
      the clone, not forked the memory.
- [ ] `GuestReady`/EK path: unchanged (ADR-0043 guestinfo transport,
      host-side `VirtualTpm` read).
- [ ] Live test mirroring phase C, including the negative test.

**Exit:** stop-condition numbers against a real vCenter; roadmap 17's pool
re-measured with a split class.

## E. Proxmox (ADR-0070 — blocked on roadmap 06)

No crate exists. When roadmap 06 starts, it should start *here*: linked
clone (`full=0`) of a split-base template, fresh `tpmstate0` at create,
never copy `tpmstate0` (roadmap 17 §E's rule, unchanged), empty per-VM
volume attached at clone. Recorded now so roadmap 06 doesn't build the
install-per-clone path first and this one second.

## F. Cloud Hypervisor (ADR-0071 — blocked on roadmap 09 phase 0)

Roadmap 09 already plans reflink (`FICLONE`) base copies and refcounted
images; the split model slots straight in: reflink the base, raw per-VM
volume, swtpm per VM, and keep roadmap 09's "no fork-from-golden" line —
snapshot-restore fan-out is memory forking and stays out (ADR-0052).

## G. Docs + threat model (the pass ADR-0066 through 0071 each owe)

- [ ] Guides: what a split class does and does not encrypt, in plain terms —
      "your data volume is sealed to your VM; the OS is shared, verified,
      and readable by the hypervisor operator like every disk is."
- [ ] Threat model full pass per `rules/threat-modeling.md`: asset A-6 gains
      the per-VM-volume distinction; a new asset row for the verity root
      hash (integrity, not confidentiality); §8 accepted risk for the
      unsigned-cmdline interim (ADR-0068) with *Revisit when: UKI lands*;
      TB rows wherever the base-volume refcounting adds a shared-storage
      write. Header stamp bumped.
- [ ] `ROADMAPS.md` row + this doc's checkboxes, same commit as each landing
      (rules/documentation.md).

## Testing (the four tiers, per `rules/testing.md`)

| Tier | What it proves here |
|---|---|
| Unit | mode gating (amended ADR-0048 check), artifact/root-hash plumbing, marker-after-seal ordering as pure functions |
| Live protocol | libvirtd accepts the overlay + verity domain XML; volume refcounting against a real pool |
| Live API server | nothing new — claims/pools unchanged |
| E2E | clone→`GuestReady` ≤ 30s; **cross-member unseal fails**; base deletion blocked while referenced; teardown leaves no overlay, volume, or TPM state |

The negative test is the point of the whole roadmap: a suite that only shows
members *can* seal, and never that siblings *can't* unseal each other, would
pass with a fleet-shared key.

## Explicitly out of scope

- **Memory forking in any form** — ADR-0052 is reaffirmed, not weakened.
- **Re-encrypting the base per member** (`cryptsetup reencrypt`) — it
  rewrites every block, destroying the CoW sharing; it is the slow path in
  disguise.
- **Network-bound / broker-released base keys** (Tang/Clevis-style). Viable
  if a site requires the base ciphertext-at-rest, but it adds a boot-time
  network dependency and a crown-jewel broker for a volume that holds no
  secrets. If ever wanted, it is its own ADR on top of ADR-0049's broker —
  not part of this roadmap.
- **Sharing a vTPM across members**, ever. It nullifies both properties
  `tpmEnabled` sells (see the threat model's A-6 rationale).

## Relationship to other roadmaps

| Roadmap | Relationship |
|---|---|
| 17 (pools/sandboxes) | First consumer. Split classes collapse its warm-pool math; its phases A–F are all unchanged by this roadmap — same EK, same claims, same agent. |
| 15 (vSphere disk import) | Prerequisite for phase D: the split base *is* an imported pre-laid disk. |
| 09 (Cloud Hypervisor) | Phase F lands inside it; reserve the interaction in its phase 0 gate. |
| 06 (Proxmox) | Phase E should be its starting shape for TPM classes. |
| ADR-0051 (UKI) | ADR-0068's interim integrity story is honest about not having it; a resolved UKI stall upgrades verity from "pinned hash" to "signed, measured chain" with no re-architecture. |
