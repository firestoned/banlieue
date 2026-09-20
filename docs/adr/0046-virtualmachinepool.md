# 0046 — `VirtualMachinePool`: warm, never-reused VMs

- **Status:** Accepted
- **Date:** 2026-09-19
- **Proposed:** 2026-09-19

## Context

Roadmap [70](../../.github/community/70-ephemeral-vm-pools.md) wants a
consumer to ask for a VM and get one in seconds. Nothing in banlieue can do
that today, and the reason is not performance work left undone — it is a
constraint recorded two ADRs ago.

ADR-0040 establishes that a disk sealed to a per-VM vTPM can only be produced
by `installMode: Deferred`: the guest installs itself, on first boot, with its
own TPM already attached. ADR-0052 rules out Instant Clone. So a TPM-sealed VM
costs a full unattended install — minutes, not seconds — and no amount of
provisioning cleverness removes that, because the slowness *is* the security
property.

The only way to hand out such a VM quickly is to have installed it already.
That is what a pool is: capacity paid for before demand arrives.

### What already exists

- `VirtualMachine` reconciles to a backend through the infra CRs
  (ADR-0050 for libvirt, ADR-0005/0024 for vSphere), and its `Ready` /
  `InfrastructureReady` conditions are published and mirrored.
- The scheduler places a `VirtualMachine` on a Provider and failure domain.
- Static addressing exists per-VM through `spec.networkOverrides[].static`.
- CAPI IPAM (ADR-0033) does **not** exist: no reconciler creates an
  `IPAddressClaim` or reads one back.

### The ordering problem this ADR has to answer

Roadmap 70 specifies `readiness: GuestReady` as the pool's default, and
phase A2 (ADR-0043) is what would publish that condition: the *installed*
guest announcing itself, as distinct from `InfrastructureReady`, which for a
Deferred image fires when the install **starts**.

A2 is not implemented. `common::condition_types` today contains `Ready`,
`InfrastructureReady` and `ImageReady` — there is no `GuestReady`, and
nothing sets one. A pool defaulting to it would wait forever for a condition
that never arrives, reporting zero warm members and no error. That is the
worst available failure: silent, and indistinguishable from a slow install.

## Decision

1. **`VirtualMachinePool` lives in `banlieue-controller`, not in a sister
   project and not per-backend.** It creates nothing but `VirtualMachine`s
   and reads nothing but their conditions and labels, so it is a consumer of
   the public API either way; putting it in the controller gets it the
   `VMImage` watch that image rollout needs.

   There is deliberately **no `VSpherePool` or `LibvirtPool`**. Nothing about
   pooling is provider-specific — members are ordinary `VirtualMachine`s and
   no provider learns that pools exist. A per-backend pool kind would be
   three reconcilers for zero behavior, and would be the first place the
   abstraction leaked. (If CAPI's `MachinePool` infra contract is wanted
   later, that is a different kind with a different name and its own ADR.)

2. **`spec.readiness` is required, with no default.** This departs from
   roadmap 70's "GuestReady by default" for the reason in Context: until
   ADR-0043 lands, that default silently never warms. Making the field
   required forces the operator to state which signal they mean, which is
   Non-Negotiable #4 applied to the one field where guessing wrong produces
   no error at all.

   `GuestReady` is accepted as a value from day one and documented as the
   only correct choice for a Deferred image. It simply cannot be the
   *implicit* one while nothing publishes it.

3. **The reconciler reports a readiness signal it has never seen.** When no
   member has ever published the chosen condition type, the pool publishes
   `Warm=False` with reason `ReadinessSignalAbsent` naming the condition.
   A pool stuck at zero must say why. This is what makes decision 2 safe
   rather than merely strict, and it also catches the `GuestReady`-before-A2
   case for anyone who sets it early.

4. **All planning is a pure function**, `plan(&PoolInputs, &[MemberView]) ->
   PoolPlan`, with no clock reads and no I/O — the same split as
   `scheduler.rs` and `migration.rs`. The reconciler gathers a snapshot,
   calls `plan`, applies the result. Six invariants hold, of which three are
   load-bearing for correctness rather than sizing:

   - A claimed member is never deleted by the pool. Only its claim ending
     removes it.
   - A member is never returned to the warm set after being claimed. Reuse
     across identities is the one thing this design must never do.
   - `alive + creates <= maxReplicas` and `provisioning + creates <=
     maxSurge`, always.

5. **Members are cattle: `generateName`, never an index.** Nothing may
   depend on a member's ordinal, because members are deleted from the middle
   of the set constantly — poisoned installs, idle expiry, image rollout.

6. **Rollout is surge-style.** A stale Ready member is retired only once
   enough fresh Ready members exist to keep `warmReplicas` claimable, unless
   `maxReplicas` leaves no room to build the replacement first. Image
   revision is `VMImage.status.buildArtifact.osArtifactUid`, falling back to
   `metadata.generation`.

7. **`maxSurge` is a host-protection knob, not a throughput knob.** Each
   provisioning member is a full OS install's worth of datastore and CPU load
   on hosts that are also running *claimed* sandboxes. Raising it to refill
   faster is usually the wrong fix; raise `warmReplicas` or shorten the
   install.

8. **Poisoned members are deleted, never repaired.** A member still
   provisioning past `provisioningTimeoutSeconds` is destroyed and replaced.
   A pool has no way to diagnose a half-finished unattended install, and a
   warm set is worthless if it can contain one.

9. **Inline IPv4 addressing, because ADR-0033 does not exist yet.**
   `spec.addressing` stamps a per-member address into the template's
   `networkOverrides`. When IPAM lands this gains a `poolRef` alternative and
   the inline range stays as the zero-dependency option. An address is held
   until the deleted member's backend VM is really gone, not until its delete
   is issued — so a range must have spares beyond `maxReplicas`.

10. **The pool owns its members by `ownerReference`**, so deleting a pool
    garbage-collects its warm set. Re-parenting a member to its claim at bind
    time is what lets a pool be deleted without killing sandboxes that are in
    use; that belongs to ADR-0047 and is only noted here because it is the
    reason ownership is per-member rather than by label alone.

11. **`VirtualMachineClaim` is out of scope** and is ADR-0047. This ADR
    produces and maintains a warm set; handing one out is a separate
    decision with its own binding, expiry and finalizer semantics.

## Consequences

- A pool is only as fast as its `warmReplicas` is deep. Sizing is
  `warmReplicas >= peak claims/min x install minutes`, and
  `maxReplicas >= warmReplicas + peak concurrent claims + maxSurge` — without
  that last term a rollout trades warm capacity for replacements and
  `available` dips.
- Until ADR-0043 lands, the only usable `readiness` value is
  `InfrastructureReady`, which is correct for `Immediate` images and **wrong
  for Deferred ones** — it fires when the install starts. So a pool of
  TPM-sealed sandboxes, the thing roadmap 70 is ultimately for, is not
  achievable until A2. Pools of `Immediate` VMs are achievable immediately.
  This ADR deliberately lands the pool first anyway: the planner, rollout and
  capacity logic are independent of which condition is watched, and none of
  it should be written twice.
- The controller gains a second thing that creates `VirtualMachine`s. Every
  existing invariant on `VirtualMachine` still holds — a member is an
  ordinary VM, schedulable, mirrorable and deletable like any other.
- Deleting a pool deletes its warm members. That is correct and is also a
  sharp edge; it is why claimed members are re-parented (decision 10).
- No new external dependency. `getrandom` will be needed by ADR-0047 for a
  claim nonce, not here.
