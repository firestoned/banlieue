<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0001 — CAPI-native cluster provisioning (no native cluster/tier abstraction)

- **Status:** Accepted
- **Date:** 2026-05-30
- **Deciders:** Erick Bourgeois
- **Supersedes:** the deferred "VMTier / VMCluster" idea explored during Phase 2 planning.
- **Related:** Locked decisions D-003 (CRD-only), D-005 (CAPI InfraMachine contract), D-007 (Provider model), D-009 (scheduling); open decision O-003. ADR-0002 (InfraCluster).

## Context

banlieue must be able to stand up Kubernetes clusters — concretely
**k0s clusters via k0smotron** — by fanning a desired count of VMs out
across failure domains (e.g. 2 vCenters × 3 clusters = 6 failure
domains). The motivating shape was a set of "tiers":

| Tier | Spread |
|---|---|
| platinum | 6 / 6 failure domains |
| gold | 5 / 6 |
| silver | 4 / 6 |
| bronze | 4 / 6 |

An early proposal was a banlieue-native `VMTier` / `VMCluster` CRD that
owns this fan-out: one CR producing N `VirtualMachine`s spread across
failure domains, with rolling upgrades and replica reconciliation.

"Fan out N replicas, spread across failure domains, for a Kubernetes
cluster, with rolling upgrades" is **exactly the problem Cluster API
(CAPI) already solves**. Non-negotiable #2 ("provider infra CRDs satisfy
the CAPI v1beta2 InfraMachine contract … what makes them reusable as
CAPI infra providers") shows this reuse was an explicit design goal, not
an afterthought. A native cluster controller would re-implement
MachineSet replica reconciliation, failure-domain balancing,
control-plane quorum handling, and rolling upgrades — most of what CAPI
*is*.

## Decision

**banlieue is a CAPI infrastructure provider. Cluster lifecycle — replica
management, failure-domain spread, and rolling upgrades — is owned by
CAPI and a control-plane provider (k0smotron for k0s), not by banlieue.**

Concretely:

1. **No `VMTier` / `VMCluster` CRD.** The "platinum = 6/6" behaviour is
   expressed as a CAPI `MachineDeployment` / control-plane object with
   `replicas: 6` over a cluster that advertises 6 failure domains. "gold
   = 5/6" is `replicas: 5`, and so on. Spread is CAPI's job.
2. **banlieue supplies the CAPI infrastructure contract objects:**
   - `VSphereMachine` — InfraMachine (already exists, contract-compliant).
   - `VSphereMachineTemplate` — InfraMachineTemplate (already exists).
   - `VSphereCluster` — **InfraCluster (new; see ADR-0002).** This is the
     missing piece required for cluster-side failure-domain spread.
3. **`VirtualMachine` stays CAPI-independent** (non-negotiable #3). The
   CAPI path and the standalone `VirtualMachine` path are parallel: a
   standalone VM is scheduled by banlieue's own (capacity-aware)
   scheduler; a CAPI-managed machine is placed by CAPI and realised by
   banlieue's InfraMachine reconciler.
4. **Generic by construction.** Because the integration is the CAPI
   contract — not a k0s-specific shim — any CAPI consumer (kubeadm, RKE2,
   k0smotron, …) can use banlieue as its infrastructure provider. k0s is
   the first consumer, not a coupling.

## Consequences

**Positive**
- We do not own a cluster/replica/upgrade controller. CAPI does it,
  battle-tested.
- Works with the whole CAPI ecosystem, not just k0s.
- Keeps the non-negotiables intact: CRD-only (D-003), CAPI contract
  (D-005), `VirtualMachine` independent of CAPI (#3).

**Negative / costs**
- We must add the **InfraCluster contract** (`VSphereCluster`), which
  banlieue did not previously have. Tracked in ADR-0002.
- **CAPI's failure-domain spread is count-balanced round-robin, not
  capacity-weighted.** True "place it where there's the most headroom"
  selection is not something CAPI does at the machine→FD level. In the
  CAPI path, capacity-awareness is therefore expressed as:
  - the provider **gating** failure domains in `Provider.status` (a
    cluster at/over a capacity threshold is simply not advertised), and
  - **DRS** (vSphere) optimising host placement *within* the chosen
    cluster (banlieue does not pick the ESXi host — see ADR-0002).
  banlieue's own capacity-weighted scheduler remains available, but only
  on the standalone `VirtualMachine` path.
- Operators provisioning clusters now take a dependency on CAPI core +
  a control-plane provider being installed in the management cluster.

## Alternatives considered

- **Native `VMCluster` / `VMTier` controller.** Rejected: duplicates
  CAPI, couples banlieue to a cluster model, and contradicts the reuse
  intent of non-negotiable #2.
- **k0s-specific integration.** Rejected: the CAPI contract is the
  generic seam; a k0s shim would exclude other CAPI consumers for no
  benefit.
