<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0159: Cloud credentials by workload identity federation

- **Status:** Accepted
- **Date:** 2026-11-24
- **Proposed:** 2026-11-24
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 22 (public-cloud foundation), roadmap 23 (Azure).
  [ADR-0003](0003-provider-deployment-topology.md) (per-Provider workload and
  Role), [ADR-0095](0095-credential-rotation-without-secret-watches.md)
  (credential rotation), [ADR-0157](0157-referencing-provider-credentials-is-its-own-permission.md)
  (referencing a credential is its own permission), [ADR-0158](0158-shape-of-a-cloud-provider-class.md)
  (cloud provider shape), ADR-0162 (the publish Job's separate identity).

## Context

Every provider banlieue has today presents a credential it reads from a
`Secret`: a vCenter password, a Proxmox API token, a libvirt client
certificate. The `Secret` has to be created, rotated (ADR-0095) and guarded
(ADR-0157), and the provider's Role has to grant `get` on it.

Both public clouds can instead trust a Kubernetes ServiceAccount token
directly. AWS trusts it through an IAM OIDC identity provider and
`AssumeRoleWithWebIdentity`; Azure through a federated identity credential on
a user-assigned managed identity or an app registration, exchanged at the
Entra ID token endpoint. Then no cloud key exists anywhere in the cluster.

What the roadmap 23 phase 0 spike established (2026-11-22 to 2026-11-24):

1. **Long-lived keys expire, and do it quietly.** Two long-lived credentials
   for a third-party service, kept in an operator's shell environment, had
   both expired (90-day limits); the failure surfaced only at first use, as an
   opaque "invalid key". A design that depends on a stored long-lived key
   inherits this.
2. **A cloud identity reading one secret works, scoped to that secret.** A VM
   with a system-assigned managed identity, granted `Key Vault Secrets User`
   on a **single secret** (Azure supports role assignment at secret scope),
   read it on the first attempt with no propagation delay, and nothing else
   in the vault.
3. **Federation needs a reachable issuer, and then works.** A cluster whose
   issuer is `https://kubernetes.default.svc` cannot federate: Entra ID
   cannot fetch it. A throwaway cluster whose issuer was a public static
   location (its discovery document and JWKS as two JSON files in a public
   blob container) federated end to end: a ServiceAccount token (10 minutes,
   audience `api://AzureADTokenExchange`) was exchanged at Entra ID in about
   0.6 s for an ARM token of a user-assigned managed identity, which read the
   resource group and was refused a write (Reader). A token of **another
   ServiceAccount** (`AADSTS700213`) and of **the right one with another
   audience** (`AADSTS700212`) were refused.
5. **The exchanged token outlives the ServiceAccount token.** The ARM token
   was valid for 24 hours; the ServiceAccount token that bought it, for 10
   minutes.
4. **No post-quantum key exchange at the cloud control planes yet.** Azure
   Resource Manager and the Entra ID token endpoint both negotiate classic
   ECDH (secp521r1, secp384r1); a client offering only X25519MLKEM768 is
   reset. Measured from two networks with two OpenSSL versions.

## Decision

### 1. The provider's ServiceAccount token is its only cloud credential

Each cloud `Provider` names the cloud identity it acts as in
`spec.connection`: an Azure client ID and tenant ID (of a user-assigned
managed identity or an app registration with a federated credential), or an
AWS role ARN. `credentialsRef` is **not set** for a federated cloud
`Provider`, and the provider's Role grants **no** `get` on any `Secret`.

The operator mounts a **projected ServiceAccount token** into the provider's
pod with the cloud's audience (`api://AzureADTokenExchange` for Azure,
`sts.amazonaws.com` for AWS), a short expiry (1 hour), refreshed by the
kubelet. The provider exchanges it for a cloud access token when it needs one
and never writes either token anywhere.

### 2. The issuer must be published: a stated prerequisite, not a banlieue job

Federation requires the cluster's service-account issuer, its OIDC discovery
document and its JWKS, to be fetchable by the cloud. The guide states this as
a prerequisite and shows how (a static bucket or web endpoint serving the two
documents, with the API server's `--service-account-issuer` set to that URL).
banlieue does not publish the issuer itself: that would make banlieue a
dependency of every other workload's federation, and it is a cluster-level
decision.

### 3. Least privilege per class, as policy documents in `deploy/`

Each cloud class ships the exact actions its provider needs as a policy
document in `deploy/` (an Azure custom role definition; an IAM policy),
scoped to the `Provider`'s resource group (Azure) or by tag condition
(AWS). The **image publisher** (ADR-0162) gets a **separate** identity and
policy: creating snapshots and registering images are rights the runtime
provider must not hold. A secret a provider legitimately needs from a vault
(none in the base design) is granted **at secret scope**, never vault-wide
(finding 2).

### 4. Static keys are a documented fallback only

A `Secret` holding a client secret or access key is accepted only where
federation is shown to be unworkable for a specific deployment, is documented
as a fallback, and is never used in examples. ADR-0157's reference check
applies to it unchanged.

### 5. Post-quantum: offer it first, record its absence

The cloud clients offer X25519MLKEM768 first, so they upgrade without a
change when the endpoints do. Until then the absence is an **accepted risk**
in the threat model, with a revisit date, not a silent gap (finding 4).

## Consequences

- No cloud key in the cluster: nothing to rotate, nothing for ADR-0095 to
  watch, no `Secret` read in the provider's Role.
- A cluster whose issuer is not public **cannot use the cloud providers**
  until it is. That is a real adoption cost; the guide puts it first.
- The exchange is verified end to end against a real tenant (finding 3);
  what phase B still verifies is the operator-rendered projected token in a
  real provider pod, on a cluster whose issuer is published.
- **A cloud access token can outlive its ServiceAccount token by hours**
  (finding 5). The provider holds it in memory only, never logs or persists
  it, and re-exchanges before expiry; deleting the provider's pod does not
  revoke a token it already holds. Recorded in the threat model.
- The published JWKS must be refreshed when the cluster's ServiceAccount
  signing key rotates, or federation stops.
- Two identities per cloud deployment (provider and publisher), each with a
  narrow policy, are more setup than one broad one; that is the point.

## Alternatives considered

- **A client secret or access key in a `Secret` (today's pattern).** Works
  everywhere but brings rotation, expiry (finding 1) and a `Secret` read;
  kept only as the fallback in Decision 4.
- **The provider reads its cloud credential from a vault.** Moves the
  long-lived secret instead of removing it, and still needs an identity to
  reach the vault.
- **banlieue publishes the issuer.** Rejected in Decision 2.
