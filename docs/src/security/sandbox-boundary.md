<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Security Boundary

> **Security boundary.** Banlieue creates and manages virtual machines. It
> never runs workloads as pods. Workloads that execute agent-generated or
> untrusted code run in their own VMs created by banlieue, outside any
> Kubernetes cluster (see
> [ADR-0081](https://github.com/firestoned/banlieue/blob/main/docs/adr/0081-kubernetes-orchestrates-vms-run-agents.md)).
> Each VM boots and installs independently with its own vTPM and shares no
> identity or encryption material with any other VM. Banlieue's hypervisor
> credentials should be scoped to a dedicated folder and resource pool with
> the minimum privileges listed in the
> [least-privilege guide](vsphere-least-privilege.md). VMs should attach only
> to allowlisted networks that have no route to a Kubernetes API server,
> should carry no secrets in guest configuration, and should accept no
> inbound connections (see
> [ADR-0082](https://github.com/firestoned/banlieue/blob/main/docs/adr/0082-no-inbound-to-sandboxes.md)).

## Where each part of the boundary is documented

| Guarantee | Page |
| --- | --- |
| Hypervisor credentials scoped to a folder and resource pool | [vSphere least privilege](vsphere-least-privilege.md) |
| Each VM has its own vTPM, and nothing is inherited from a template | [vTPM per provider](vtpm-per-provider.md) |
| No secrets in guest configuration | [Guest configuration hygiene](guest-configuration.md) |
| Threats, controls and accepted risks for all of the above | [Threat model](threat-model.md) |

## What banlieue does not do

Banlieue is not a policy engine. It does not evaluate sandbox policy, mint
or carry tokens, deliver tasks to agents, or render in-guest firewall rules.
Those belong to [mediatore](../guides/sandbox-identity-mediatore.md) and to
the platform's policy and task controllers. Banlieue records who a claimed
VM is for (`VirtualMachineClaim.spec.subject`) and destroys the VM when the
claim ends.

The statements above marked *should* are deployment obligations. Some are
enforced by banlieue and some are not yet; the pages linked above say which,
per provider.
