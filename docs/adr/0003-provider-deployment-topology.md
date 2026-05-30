<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0003 — Provider deployment topology (per-instance vs per-class)

- **Status:** Proposed
- **Date:** 2026-05-30
- **Deciders:** Erick Bourgeois
- **Related:** Phase 3 (Provider Lifecycle Automation); open decision O-003 ("multi-tenancy boundaries within a single Provider"); D-007 (Provider model); least-privilege project principle.

> This ADR is **Proposed**, not Accepted. It captures the decision space
> for how provider controllers are deployed, so it isn't re-derived later.
> It does **not** block ADR-0001/0002, which are about the CAPI contract
> and are independent of deployment topology.

## Context

Today one provider Deployment (e.g. `banlieue-provider-vsphere`) watches
*every* `Provider` of its class cluster-wide, filtering by
`spec.providerClassRef.name` in the reconciler. Phase 3 plans a
`ProviderClass` CRD + lifecycle controller that creates provider
Deployments + RBAC automatically.

A requirement surfaced during design: **no work-queue starvation across
backends** — a hung or slow reconcile against vCenter A must not stall
vCenter B. Two further drivers are credential isolation and per-backend
network policy (both least-privilege wins).

Three topologies:

1. **Per-class (current / planned default).** One Deployment per class
   handles all instances. Simplest, lowest overhead; shared blast radius;
   one pod loads every backend's credentials.
2. **Per-instance.** One Deployment per `Provider` CR. Maximum isolation
   (blast radius, credentials, network); pays pod + Lease + watch cost
   per backend (100 vCenters ⇒ 100 pods).
3. **Hybrid with a strategy knob.** `ProviderClass` is the template
   (image, RBAC, resources); `deploymentStrategy: Shared | PerInstance`
   selects per-class or per-instance instantiation. Default `Shared` for
   small installs; `PerInstance` for isolation / multi-tenancy.

## Decision (proposed)

Adopt **option 3 (hybrid)**, defaulting to `Shared`, with `PerInstance`
opt-in. Per-instance addresses the starvation, credential-isolation, and
network-policy drivers without forcing the pod-count cost on small
installs. In `PerInstance` mode each provider Deployment runs a
server-side filtered watch (`labelSelector banlieue.io/provider=<name>`)
so its cache holds only its own infra objects — strictly better than
today's filter-in-reconciler.

Routing stays at the **infra-CRD** layer (the provider watches
`VSphereMachine`, never `VirtualMachine`); the main controller's
scheduler stamps the `banlieue.io/provider` label when it emits the infra
CR. Explicit provider pinning, if added, is a scheduling **constraint**,
not a scheduler bypass (preserves D-009).

## Status / open questions

- Confirm the default (`Shared`) and the knob name.
- Decide Lease naming per-instance (`banlieue-provider-vsphere-<name>`).
- Decide how `PerInstance` interacts with the Phase 3 `ProviderClass`
  RBAC templates (per-instance ServiceAccount vs shared).

To be promoted to **Accepted** when Phase 3 lifecycle work begins.
