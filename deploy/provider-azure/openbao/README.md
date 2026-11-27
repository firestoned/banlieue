<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# OpenBao as the signer of the federation assertion

For an azure `Provider` that declares `spec.connection.azure.openbao`
([ADR-0176](../../../docs/adr/0176-openbao-signs-the-federation-assertion-for-clusters-without-a-public-issuer.md)).
Use it only when the cluster's own ServiceAccount issuer cannot be published;
otherwise leave the block out. The user guide is
[`docs/src/providers/azure.md`](../../../docs/src/providers/azure.md), section 2a.

| File | What it is |
| --- | --- |
| `policy.hcl` | The one-path policy each workload gets: `read` on its own `identity/oidc/token/<tokenRole>` and nothing else. |

Everything below uses placeholders; substitute your own. `<ns>` is the OpenBao
namespace (omit `BAO_NAMESPACE` for the root namespace), `<mount>` the JWT auth
mount that trusts this cluster, `<name>` the `Provider`'s name.

## One key per environment

A long rotation period, so republishing the public keys (below) is a rare
chore. A new key is published ahead of its use, and the previous public key is
kept for `verification_ttl`, which must outlast the slowest republish.

```sh
export BAO_NAMESPACE=<ns>
bao write identity/oidc/key/azure algorithm=RS256 \
  rotation_period=2160h verification_ttl=2400h \
  allowed_client_ids='api://AzureADTokenExchange'
```

`allowed_client_ids` names exactly the audiences in use, never `*`.

## Per workload: an OIDC role, a policy and a login role

```sh
# The role whose token is the assertion; client_id becomes its `aud`.
bao write identity/oidc/role/azure-<name> key=azure \
  client_id='api://AzureADTokenExchange' ttl=1h

# The one-path policy (edit <tokenRole> in the file first, or pipe it).
bao policy write azure-<name> deploy/provider-azure/openbao/policy.hcl

# The login role: bound to THIS workload's ServiceAccount and audience, with
# a short token that the provider revokes after reading the assertion.
bao write auth/<mount>/role/azure-<name> role_type=jwt user_claim=sub \
  bound_audiences=openbao \
  bound_subject=system:serviceaccount:<provider namespace>:banlieue-provider-<class>-<name> \
  token_policies=azure-<name> token_ttl=15m token_max_ttl=15m
```

`bound_audiences` is the `audience` of the `openbao` block (default `openbao`).
`bound_subject` is the ServiceAccount the operator creates for the `Provider`;
read it from the cluster when the name is long
(`kubectl -n <ns> get sa -l banlieue.io/provider=<name>`).

## The issuer and the two public documents

```sh
# Only a scheme, a host and an optional port: OpenBao adds /v1/<ns>/identity/oidc.
bao write identity/oidc/config issuer=https://oidc.example.com
```

Serve byte copies of OpenBao's two unauthenticated responses, as
`application/json`, at exactly these paths on that host (so the host is
dedicated, or a proxy keeps the `/v1/...` path):

```text
/v1/<ns>/identity/oidc/.well-known/openid-configuration
/v1/<ns>/identity/oidc/.well-known/keys
```

They hold public keys only. **Republish both after every key rotation.**

## The federated credential's subject

In a namespace the token's `sub` is `<entity uuid>.<namespace id>`. Log in once
as the Provider's ServiceAccount and read `entity_id` from the response, then
use it as `--subject` on the Entra federated credential (issuer
`https://oidc.example.com/v1/<ns>/identity/oidc`, audience
`api://AzureADTokenExchange`). Deleting the entity changes the subject and
breaks federation until the credential is updated.
