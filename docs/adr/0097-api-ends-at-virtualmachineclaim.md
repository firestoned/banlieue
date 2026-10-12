<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0097: banlieue's API ends at VirtualMachineClaim; the agent layer lives in mediatore

- **Status:** Accepted
- **Date:** 2026-10-12
- **Deciders:** Erick Bourgeois
- **Supersedes:** [ADR-0055](0055-agentsandbox.md) (`AgentSandbox` in
  `agent.banlieue.io`, Proposed, never built)
- **Related:** [ADR-0046](0046-virtualmachinepool.md) (`VirtualMachinePool`),
  [ADR-0047](0047-virtualmachineclaim.md) (`VirtualMachineClaim`),
  [ADR-0049](0049-attestation-trust-anchors.md) (the broker is not banlieue
  code), [ADR-0081](0081-kubernetes-orchestrates-vms-run-agents.md) and
  [ADR-0082](0082-no-inbound-to-sandboxes.md) (platform decisions, now kept
  in mediatore); mediatore
  [ADR-0008](https://github.com/firestoned/mediatore/blob/main/docs/adr/0008-agentsandbox-crd-owned-by-mediatore.md)
  (`AgentSandbox` as a mediatore CRD).

## Context

banlieue's purpose is an abstract, multi-provider VM API: `VirtualMachine`
realised on vSphere, libvirt, Proxmox or Cloud Hypervisor through
provider infra CRDs that satisfy the CAPI InfraMachine contract. Roadmap 17
added two layers above it, `VirtualMachinePool` (warm, single-use members)
and `VirtualMachineClaim` (one member for one subject, destroyed on
release).

The scope question was whether banlieue should stop there, or whether pool
and claim should leave too and banlieue shrink to VM creation alone. Two
facts decide it:

- **Pool and claim are already isolated and generic.** No provider, nor the
  VM reconciler, scheduler or infra builder, knows they exist (ADR-0046
  Decision 1 holds in the tree). They serve CI, untrusted builds and demo
  environments as well as agents, and prior art keeps the equivalents
  in-tree (KubeVirt's `VirtualMachinePool`; ReplicaSet over Pod; PVC
  binding). Moving them would cost a CRD group rename, a second release
  pipeline and a git dependency on an unpublished `banlieue-api`, and would
  remove about 2,150 production lines without removing any of the trust
  work beneath them (`GuestReady`, vTPM, EK certificates), which lives on
  `VirtualMachine` and the providers.
- **`AgentSandbox` (ADR-0055) is not infrastructure.** It wraps a claim and
  adds one field a claim does not carry, issuer and audience policy, whose
  only reader is the broker. mediatore is that broker, and already holds the
  same request (`POST /v1/claims`: pool, TTL, audiences, workload) as a
  store row. Keeping the CRD here would leave an agent-specific API in an
  infrastructure project whose only consumer is another repository.

## Decision

1. **`VirtualMachineClaim` is the top of banlieue's API.** banlieue adds no
   kind above it. Requests that carry agent, identity, policy or token
   concepts belong to a consumer of the claim, not to banlieue.
2. **`VirtualMachinePool` and `VirtualMachineClaim` stay in
   `banlieue-controller`**, in `banlieue.io`, as ADR-0046 and ADR-0047
   decided.
3. **`AgentSandbox` moves to mediatore** as a mediatore CRD
   (`mediatore.io/v1alpha1`), reconciled by mediatore, per mediatore
   ADR-0008. ADR-0055 is superseded; `agent.banlieue.io` is never created.
4. **The AgentSandbox platform record moves with it.** ADR-0081 and
   ADR-0082 were platform decisions recorded here; their canonical copies
   are now mediatore ADR-0009 and ADR-0010. The files here are frozen with
   a pointer, and their *What this means for banlieue* sections still bind
   banlieue. The companion CALM model `agentsandbox-stack.calm.json` moves
   to mediatore too.
5. **The test for a future feature:** if the feature is meaningful for a VM
   nobody has claimed, it may belong here. If it only means something once
   a subject, a token or an agent is involved, it belongs to the consumer.

## Consequences

- banlieue's description becomes "multi-provider VMs, plus warm pools and
  single-use claims of them". Nothing agent-specific is added.
- mediatore creates `VirtualMachineClaim`s through an `AgentSandbox` it
  owns. To banlieue it is still the allowlisted broker that the
  claim-subject admission policy (ADR-0047 Decision 10) trusts, so the
  threat model's claim-consumer actor and TB-7 are unchanged.
- Sandbox-driven work still open here is judged by Decision 5 when it is
  next picked up, not settled by this ADR: roadmap 18 (split-image fast
  clone), ADR-0088 (first-boot sealing), and whether
  `Provider.spec.attestation.ekTrustBundle` (ADR-0049 Decision 10) belongs
  on an infrastructure CRD.
- The splitting question is re-opened only if one of these holds:
  `banlieue-api` is published on crates.io, a second independent consumer
  of pools needs a release cadence the core cannot match, or pool or claim
  needs a provider-specific hook.
