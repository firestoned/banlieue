<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0095: Credential rotation by per-reconcile reads, not Secret watches

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 12 §4.4; [ADR-0003](0003-provider-deployment-topology.md);
  [ADR-0008](0008-byoc-vsphere-http-client.md) (CA-bundle references);
  [ADR-0012](0012-providerclass-crd-and-operator-role.md) (the provider Role)

## Context

The roadmap item read "providers re-read credentials on Secret change events
(already watching, just ensure cache invalidates)". Neither half is true:

- **No provider watches Secrets.** Every provider reads its credential
  Secret with a direct `get` on every reconcile:
  - vSphere: `read_credentials`;
  - Proxmox and libvirt: `credentials::resolve`;
  - Cloud Hypervisor: no Secret, a renewing token file (ADR-0060).
- **No provider caches a client built from credentials.** vSphere logs in to
  vCenter per call, Proxmox builds a client per call, and libvirt opens a TLS
  session per call. So a rotated Secret is used by the next reconcile.

Each operator-spawned provider's Role grants `get` on its own Secret only, by
`resourceNames`. A watch is a `list` and `watch` on the `secrets` resource,
and Kubernetes cannot scope a namespace-wide list or watch to one name. So
watching would mean granting every provider read access to **every Secret
in its namespace**, including other Providers' credentials.

## Decision

1. **Do not watch Secrets.** The `get`-by-name grant stays. Read access to a
   namespace's Secrets is not worth trading for a faster rotation.

2. **Rotation is bounded by the requeue, and that bound is a contract:**
   - Credentials are read when they are used, in every reconcile that talks
     to a backend.
   - No client, session or token built from them outlives a reconcile.
   - A reconciled object requeues within the SDK's default interval
     (`banlieue-provider-sdk::reconciler`, 30 s), so a rotation takes effect
     within about that interval, with no restart.

   Tests pin each half per provider: credentials are resolved inside the
   reconcile path, and the client factory builds per call.

3. **A future client cache must key on the Secret's `resourceVersion`.** Then
   the next reconcile after a rotation rebuilds the client, and the bound in
   2 still holds. This is the rule for anyone adding connection reuse.

4. **Rotating safely is an operator procedure, documented in the provider
   guides:**
   1. Create the new credential on the backend.
   2. Update the Secret.
   3. Wait one requeue interval, and check the Provider's `Ready` condition.
   4. Revoke the old credential.

## Consequences

- Rotation takes up to about 30 s instead of a second. A credential that has
  to be cut off immediately is revoked on the backend, which stops it
  everywhere at once, not just in banlieue.
- Least privilege is preserved: each provider can read exactly one Secret.
- The roadmap item closes as a decision, not a feature. The ~30 s bound
  becomes something tests protect, rather than an accident of the code.
