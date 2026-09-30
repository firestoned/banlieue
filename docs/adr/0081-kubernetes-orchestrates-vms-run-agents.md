<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0081: Kubernetes orchestrates, VMs run agents

- **Status:** Accepted
- **Date:** 2026-09-30
- **Deciders:** Erick Bourgeois
- **Scope:** banlieue, mediatore, the AgentSandbox platform
- **Origin:** Platform ADR-0001, recorded here unchanged in substance. Its
  sibling, platform ADR-0002, is [ADR-0082](0082-no-inbound-to-sandboxes.md).
  Identifiers of the form `T2.1` or `K4.5` refer to the AgentSandbox
  platform threat model, not to banlieue's
  [threat model](../src/security/threat-model.md).
- **Related:** [ADR-0045](0045-vtpm-endorsement-key-certificate.md) (per-VM
  EK certificate), [ADR-0048](0048-tpm-enabled-requires-deferred-install.md)
  (`tpmEnabled` requires a non-`Immediate` install),
  [ADR-0049](0049-attestation-trust-anchors.md) (attestation trust anchors),
  [ADR-0052](0052-instant-clone-vmfork-not-supported.md) (no memory
  forking), [ADR-0055](0055-agentsandbox.md) (`AgentSandbox`).

## Context

AgentSandbox runs agent-generated code with a user's credentials. The same
Kubernetes estate also runs other production workloads. The question was
whether sandboxes should run as pods (runc, gVisor, or Kata/microVM pods) or
as VMs created outside the cluster.

The threat model assumes the agent process will eventually be steered by
hostile content, so the boundary around it has to hold on its own. Nested
jails of the same kind (nsjail inside a container) share one kernel, so a
single kernel flaw defeats both layers. A hypervisor is a different class of
boundary.

The cluster nodes are themselves VMs with no nested virtualization, so
microVM pods (Kata and similar) are not available on them.

## Decision

1. Kubernetes is used for orchestration only. Banlieue, mediatore and the
   policy and task controllers run on a management cluster.
2. No workload that runs agent-generated or untrusted code runs in a
   Kubernetes cluster. Every such workload runs in its own VM, created by
   banlieue, outside any cluster. This is a hard floor in the threat model
   and cannot be relaxed by a knob.
3. One VM per identity. Each VM boots and installs independently with its
   own vTPM (`installMode: Deferred`). No vTPM, identity or disk-encryption
   material is shared between VMs.
4. The management cluster is separate from general-purpose clusters. The
   sandbox network segment never has a route to any Kubernetes API server.
   Sandboxes reach only mediatore and the egress gateway.

## Options considered

| Option | Boundary to the host | What stays exposed | Verdict |
| --- | --- | --- | --- |
| runc pod | Shared kernel | Node kernel, kubelet, service account tokens, flat pod network | Rejected |
| gVisor pod | User-space kernel, reduced host syscall surface | Host kernel still shared, same kubelet and cluster exposure | Rejected |
| Kata or microVM pod | Hypervisor | Kubelet and node credentials stay close; needs nested virtualization or bare metal | Rejected for now, revisit on bare metal |
| VM per identity outside the cluster | Hypervisor, independent vTPM identity, a network segment we define | Hypervisor and management-plane credentials | Chosen |

## Consequences

Positive:

- An escape from the jail lands in a VM with no neighbours and no kubelet.
- The attestation root (vTPM) does not depend on the cluster control plane.
- Network defaults are defined by us, not inherited from a pod network.

Negative:

- Slower boot, a heavier image lifecycle, lower density, more operations.
- Banlieue's hypervisor account and the management cluster become crown
  jewels.

Accepted residual risks: hypervisor escape, compromise of the platform
administrators, side channels between co-tenant VMs (reduced by dedicated
hosts, not eliminated).

### What this means for banlieue

Banlieue is not a policy engine. Policy evaluation, tokens and task delivery
belong to other projects (mediatore, a SandboxPolicy controller, a task
controller) and are not added here. Banlieue's share of this decision is the
set of guarantees in *Required follow-ons* that concern VM creation:
credential scope, placement, network attachment, vTPM independence and guest
configuration hygiene. The
[security boundary](../src/security/sandbox-boundary.md) page states them
for operators.

## Required follow-ons

- Hypervisor credentials used by banlieue are scoped to a sandbox folder and
  resource pool, with only the privileges its code actually uses.
- Sandbox VMs are placed on dedicated hosts.
- VMs attach only to allowlisted networks, and those networks have no route
  to Kubernetes API servers.
- Guest configuration data (cloud-config, guestinfo) carries no secrets.
- Admission on every cluster denies agent runtime images and agent-labelled
  workloads outside the VM path.

## Exceptions

The supervisor agent may run in the cluster only if it executes no generated
code and has no shell or code tools. It then runs in a dedicated namespace
and node pool, with egress limited to its model endpoint and RBAC that only
allows creating tasks. If it needs code execution it runs in its own VM. Any
other exception requires a new ADR.

## Verification

- A control test from the sandbox segment to every Kubernetes API endpoint
  fails.
- An admission test rejects an agent runtime image in each cluster.
- A periodic inventory of cluster workloads finds no agent images outside
  the allowed exception.
- A test confirms two VMs from the same template have different vTPM
  identities.

## Related

- Platform threat model: hard floors, T2.1, T6.1, T8.3, T8.7.
- [ADR-0082](0082-no-inbound-to-sandboxes.md): No inbound to sandboxes.
