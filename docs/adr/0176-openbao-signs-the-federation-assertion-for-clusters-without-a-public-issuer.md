<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0176: OpenBao signs the federation assertion for clusters without a public issuer

- **Status:** Accepted. Implemented; not yet verified end to end on a cluster (see Implementation status).
- **Date:** 2026-11-26
- **Proposed:** 2026-11-26
- **Deciders:** Erick Bourgeois
- **Related:** Extends [ADR-0159](0159-cloud-credentials-by-workload-identity-federation.md)
  (Decision 2 stays the default; this is its alternative for clusters whose
  ServiceAccount issuer cannot be published). Uses the OpenBao trust that
  [ADR-0146](0146-cloud-hypervisor-guests-may-attest-with-x509pop-and-a-certificate-their-host-signs.md)
  already relies on. Client side:
  [ADR-0164](0164-azure-inframachine-and-a-first-party-arm-client.md).

## Context

ADR-0159 makes the provider's projected ServiceAccount token its only cloud
credential: Entra ID trusts the cluster's service-account issuer through a
federated identity credential. Decision 2 makes publishing that issuer (the
OIDC discovery document and the JWKS, over public HTTPS) a prerequisite the
operator meets, with the API server's `--service-account-issuer` pointing at
it.

Some clusters cannot meet it. Changing the API server's issuer is a
cluster-level decision that every other workload on the cluster shares, and
in a controlled environment it may be a change the platform owner will not
make for one provider. Two things were considered and rejected in the
discussion that led here:

- **The tier's interactive login service** (a Dex hub). It signs tokens for
  people. Entra would have to trust a system that mints user identities as
  the provider's identity, it is reachable only on the internal network, and
  federating through it would need a directory password stored in the
  cluster. That is the stored credential ADR-0159 exists to avoid.
- **A short-lived bootstrap secret, as the Cloud Hypervisor host install
  uses.** That pattern works because OpenBao is a broker that already trusts
  the host and mints a token pinned to one policy for one use. Entra has no
  one-use or five-minute secret; the nearest thing, a client secret with an
  end date, must be held for as long as the provider needs ARM tokens, which
  is for as long as VMs exist.

What the cloud needs from an issuer is narrower than a running service.
Entra ID fetches a discovery document and a set of **public** keys and never
calls the issuer again. The signer can stay on the internal network; only
two static documents have to be public. OpenBao's identity secrets engine is
a signer that fits: it issues OIDC JWTs for an authenticated entity, and its
discovery and key endpoints are unauthenticated and reveal only public keys.
The platform already runs OpenBao and the provider's cluster already
authenticates to it (ADR-0146).

Facts from the OpenBao documentation (2.7.x) this decision rests on, and the
ones it does **not** settle:

- The token's `iss` is the engine's `issuer` setting (`identity/oidc/config`),
  defaulting to OpenBao's `api_addr`. The documentation says the issuer should
  point at the engine's `identity/oidc` path and be reachable by validators.
  **It does not say that a public URL unrelated to OpenBao's own address is
  supported.** Entra only requires that the token's `iss` equals the issuer
  in the federated credential and that `{issuer}/.well-known/openid-configuration`
  serves a matching document, so a static copy should work, but this is
  unproven and is the first thing to prove (Decision 6).
- `aud` is the role's `client_id`, set by the operator; the token read takes
  only the role name. The key's `allowed_client_ids` must list it.
- `sub` is the requester's **entity ID**, a UUID, and a role's template may
  not overwrite the standard claims. So the federated credential's subject
  is that UUID, stable for as long as the entity exists.
- Keys rotate (default every 24 hours); the previous public key is kept for
  `verification_ttl`.
- The documentation states neither the policy capability needed to read a
  token nor how the Kubernetes auth method names the entity alias. Both are
  checked against a real server before they are written into a policy.

## Decision

### 1. OpenBao signs; the cluster's issuer is left alone

For a Provider whose cluster cannot publish its issuer, the provider obtains
its federation assertion from OpenBao instead of from the projected
ServiceAccount token directly:

1. The provider logs in to OpenBao with its own projected ServiceAccount
   token (audience `openbao`, distinct from the cloud audience), through the
   Kubernetes auth method, as a role bound to **that Provider's
   ServiceAccount only**.
2. It reads `identity/oidc/token/<role>`, a role per Provider, and holds the
   result in memory.
3. It presents that JWT to Entra ID as the `client_assertion` of the same
   token exchange ADR-0159 already specifies. Nothing else about the Azure
   side changes: the same user-assigned identity, the same custom role.

The provider writes neither the OpenBao token nor the assertion to disk or
to a `Secret`, and its Role still grants no `get` on any `Secret`.

### 2. One OpenBao role and one entity per Provider

- A **key** per environment, `RS256`, with a long `rotation_period` (see
  Decision 5) and a `verification_ttl` that covers a republish.
- A **role** per Provider with `client_id` set to the cloud audience
  (`api://AzureADTokenExchange`) and a `ttl` of one hour. The key's
  `allowed_client_ids` lists exactly the audiences in use, never `*`.
- A **Kubernetes auth role** per Provider, bound to the Provider's
  ServiceAccount name and namespace, with a policy that allows reading
  exactly that one `identity/oidc/token/<role>` and nothing else. A
  Provider cannot read another Provider's token, so one Provider cannot
  assume another's cloud identity.
- The federated credential's **subject** is that entity's UUID, recorded
  once at setup. Deleting the entity changes the subject and breaks
  federation until the credential is updated: a documented operational fact,
  not a surprise.

### 3. The public part is two static documents and nothing else

OpenBao's `identity/oidc/config` `issuer` accepts **only a scheme, a host and
an optional port**: a path is refused. OpenBao appends the engine's own path
to it, so the issuer a token carries is
`https://<host>/v1/<namespace path>/identity/oidc` (without a namespace,
`https://<host>/v1/identity/oidc`), and the federated credential names that
whole string. The operator therefore provides a public HTTPS host that serves,
at exactly those paths:

```text
https://<host>/v1/<namespace path>/identity/oidc/.well-known/openid-configuration
https://<host>/v1/<namespace path>/identity/oidc/.well-known/keys
```

These are byte copies of OpenBao's own unauthenticated responses (public keys
only); the discovery document's `jwks_uri` already follows the configured
issuer, so no rewriting is needed. The content type is `application/json`. The
host is dedicated to this purpose, or sits behind a proxy that preserves the
`/v1/...` path; it cannot be an arbitrary sub-path of a shared site.

banlieue does not publish them, as ADR-0159 Decision 2 says of the cluster's
issuer: it is a prerequisite the operator meets, and the guide shows how. A
helper that prints the two documents for upload is acceptable; a banlieue
component that holds a credential to publish them is not.

### 4. Federation is for the provider only; the signer is not a new authority

OpenBao decides who may obtain an assertion; Entra's federated credential
and the Azure role decide what that assertion may do. Neither is widened by
this ADR. The Azure custom role in `deploy/provider-azure/role-definition.json`
is unchanged and still scoped to the Provider's resource group.

The OpenBao role and policy are shipped as documents in `deploy/`, like the
Azure role, so the grant is reviewable and not described only in prose.

### 5. Key rotation is an operational duty, stated and bounded

Entra reads the published keys, so a rotation that is not republished makes
every assertion fail verification once the old key is gone. The default
24-hour rotation is therefore **not** used: the key rotates on a long period
(for example ninety days), with a `verification_ttl` that is at least as
long as the longest the operator takes to republish. A rotation is followed
by republishing the two documents, before `verification_ttl` elapses. This
is the same duty ADR-0159 puts on an operator who publishes the cluster's
own JWKS; it is recorded as an accepted risk in the threat model with a
*Revisit when* of "a publisher exists that needs no stored credential".

### 6. Prove it before building it

The decision was **Accepted but not implemented** until a spike showed, against a real Entra
tenant, that:

- a token whose `iss` is the static public URL (not an OpenBao address) is
  accepted by Entra ID for a federated credential naming that issuer and the
  entity UUID as subject;
- the discovery document OpenBao produces advertises a `jwks_uri` that is
  correct under the static URL (the documentation's own example is
  inconsistent about the path prefix), or the helper rewrites it;
- the policy capability and alias naming the documentation leaves open.

**Spike findings so far (2026-11-26, OpenBao 2.7.2, a scratch namespace).**
The OpenBao half is proven; the Entra half is not yet.

- A role's `client_id` of `api://AzureADTokenExchange` is accepted and becomes
  the token's `aud` exactly as written.
- With no `issuer` set, `iss` is the answering node's own `api_addr` plus
  `/v1/<namespace>/identity/oidc`: an internal node address, and it differs
  per node, so it can never be left at its default. `identity/oidc/config`
  `issuer` must be set, and the discovery document's `jwks_uri` follows it
  (`{issuer}/.well-known/keys`), so a static copy under the same base URL is
  self-consistent and needs no rewriting.
- In a namespace, `sub` is `<entity uuid>.<namespace id>`, and the token
  carries an extra `namespace` claim. The federated credential's subject is
  that whole string.
- The JWKS lists **two** keys: the current one and the next one, published
  ahead of its use. A republish after a rotation therefore never leaves a
  window in which the live key is unpublished, provided the documents are
  republished before the following rotation.
- A JWT role bound to one ServiceAccount subject and audience, with a policy
  that reads one token path, logged in and read exactly that token.

**Entra half, proven (2026-11-26/27).** The two documents were uploaded to a
static website on a storage account and the issuer set to its host. Entra ID
accepted an OpenBao-signed assertion whose `iss` is that static URL for a
user-assigned managed identity with a federated credential naming that issuer,
the entity's `sub` and the audience, and returned an ARM access token (24-hour
lifetime, as ADR-0159 records). `banlieue-azure`'s own `FederatedToken`
(the `live_arm` token-exchange test) passes with the OpenBao assertion placed
in its token file, so the exchange needs no client change beyond where the
assertion comes from. A second Provider, a different ServiceAccount and so a
different entity, was refused by Entra with `AADSTS700213`: one Provider cannot
use another's cloud identity. Two practical points: a new federated credential
took a short while to propagate, and the first exchange attempts can answer
`AADSTS70025` (no federated credentials) until it does.

Not proven: a real cluster ServiceAccount token as the OpenBao login (the
spike used a stand-in signed with a throwaway key; the cluster's JWT mount
already pins the cluster key, see the OpenBao repository's playbook), and
republishing after a real key rotation.

If Entra rejects an issuer that is not OpenBao's own, this ADR is
Superseded in favour of making OpenBao's `identity/oidc` path itself
publicly reachable, which is a larger decision with its own risks, or of
ADR-0159 Decision 2.

### 7. The client: one alternative source, chosen explicitly

`banlieue-azure`'s federated token source takes its assertion from an
`AssertionSource`. The file source (the projected token) stays the default;
`OpenBaoAssertion` logs in and reads the token as above. The exchange, the
caching and the refresh margin are unchanged, so OpenBao is called once per
ARM token and never per ARM request. The provider selects the source with
`--assertion-source file|openbao`; the operator, not a person, sets it
(Decision 8).

### 8. The Provider declares it; the operator renders it

The choice is the admin's, declared on the `Provider` and never inferred
(non-negotiable 4): an optional `spec.connection.azure.openbao` block
(`address`, `namespace`, `authMount`, `loginRole`, `tokenRole` and an
`audience` that defaults to `openbao`). Its absence means the file source, so
an existing `Provider` is unchanged. The schema refuses what the client would
refuse: a non-`https` address, and any name that could change a request path.

From that block the operator renders, into the provider's pod:

- a **second projected ServiceAccount token**, audience as declared, one hour,
  at `/var/run/secrets/openbao/token`, beside the cloud-audience token (which
  is still mounted: the two never share an audience);
- `BANLIEUE_ASSERTION_SOURCE=openbao` and the `BANLIEUE_OPENBAO_*` settings.

The `Provider` stays the one source of truth: the block is read once, by the
operator, and the pod's environment is a rendering of it, not a second place to
edit.

**One OpenBao identity per workload, not per tenant `Provider`.** The login
token is the pod's ServiceAccount token, so the entity, and the federated
credential's subject, belong to the workload. For a shared provider
(ADR-0133) that serves tenant namespaces, every tenant `Provider`'s identity
trusts the same subject, exactly as the file source already works; the block
is read from the `Provider` that owns the workload. Per-tenant separation of
cloud identities on one pod is not provided by this ADR.

**Egress.** The provider's pod needs to reach OpenBao's address in addition to
Entra ID and ARM. Where the cluster restricts egress, that is a policy the
operator of the cluster grants; banlieue documents the destination and does not
open it.

**Implementation status (2026-11-27).** Built and tested: `AssertionSource`,
`FileAssertion` and `OpenBaoAssertion` in `banlieue-azure`
(`src/assertion.rs`), `FederatedToken::with_source`, the provider's
`--assertion-source file|openbao` flag, the `spec.connection.azure.openbao`
block (schema patterns, a CEL rule against `.` and `..`, and a rule refusing the
cloud audience as the login audience, which the threat-model pass found
missing), the operator's rendering of the second projected token and the
`BANLIEUE_OPENBAO_*` settings (Azure backend only), the OpenBao role and policy
documents in `deploy/provider-azure/openbao/`, an example
(`examples/33-provider-azure-openbao.yaml`), the user guide and the threat-model
pass (TB-24, A-33, section 7.37 and three accepted risks). The live
`live_arm` OpenBao test passes against a real Entra tenant. Not yet verified
live: a real cluster ServiceAccount token as the OpenBao login (the spike used a
stand-in signed with a throwaway key), the operator-rendered pod running on a
cluster, and a republish after a real key rotation. The image publisher keeps
the file source: it is a separate identity (ADR-0162) outside this decision.

## Consequences

- A cluster whose API-server issuer cannot change can still federate to
  Azure, with no stored cloud credential and no directory password.
- **A new dependency in the provider's authentication path:** OpenBao must be
  reachable from the provider's pod, and an OpenBao outage stops new ARM
  tokens once the cached one expires (about an hour). Running VMs are
  unaffected; reconciliation pauses. The ServiceAccount-token source has no
  such dependency and stays the default.
- **A new trust boundary** (provider pod to OpenBao) and a new asset (the
  signing key). Both belong in the threat model on implementation.
- OpenBao's signing key is now a root of trust for cloud access: whoever can
  read a Provider's token role can act as that Provider's cloud identity.
  Decision 2's per-Provider role and policy are the control; their review is
  part of onboarding.
- The manual republish on rotation is real operational toil, mitigated by
  the long rotation period and bounded by `verification_ttl`.
- The entity UUID as subject is stable but opaque; the guide must say how to
  read it and what recreating the entity does.
- Entra ID accepts an issuer that is not the signer's own address (spike,
  Decision 6). The issuer is a bare host that OpenBao extends with its own
  path, which constrains where the two documents can be served (Decision 3).

## Alternatives considered

- **Publish the cluster's own ServiceAccount issuer (ADR-0159 Decision 2).**
  Still the default and still simpler. Not an option here only because the
  API-server flag change is not available; it remains the better answer
  wherever it is.
- **The tier's Dex login service as the issuer.** Rejected above: wrong
  principal (people, not workloads), internal-only, and it implies a stored
  directory password.
- **A client secret on an Entra app registration, short-lived or not.**
  Rejected: Entra has no one-use secret, and the provider needs it for the
  life of the VMs. ADR-0159 Decision 4 keeps static keys a documented
  fallback only.
- **Make OpenBao's own address the public issuer.** Matches the
  documentation most literally, but puts a secrets manager on the public
  internet to serve two static documents. The static copy keeps OpenBao
  internal; this is the fallback only if the spike fails.
