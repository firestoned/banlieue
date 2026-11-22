<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0157: Referencing a Provider's credentials is its own permission, checked when the connection changes

- **Status:** Accepted
- **Date:** 2026-11-22
- **Proposed:** 2026-11-19
- **Deciders:** Erick Bourgeois
- **Related:** Amends the `banlieue-provider-credentialsref-authorization` admission policy
  (security review 2026-07-31, CHAIN-001; `deploy/admission/provider-credentialsref-authorization.yaml`).
  [ADR-0003](0003-provider-deployment-topology.md) (the per-Provider Role that grants the
  workload `get` on the named Secret); [ADR-0011](0011-libvirt-provider-own-client.md) (libvirt
  over mutual TLS, the client certificate as the credential);
  [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md) (credentials stay in the
  provider's home namespace). faubourg ADR-0018 (libvirt zones), whose live test found this.
  ADR-0146 to ADR-0156 were held for roadmaps 22 to 25 when this was written; that
  block has since moved to ADR-0158 to ADR-0169.

## Context

`banlieue-provider-credentialsref-authorization` closes a confused-deputy hop (CHAIN-001). The
operator gives a Provider's workload `get` on exactly the Secret its `credentialsRef` names
(ADR-0003), and the provider then presents that credential to the Provider's endpoint. Without
the policy, a principal allowed to write Providers but not to read Secrets could name someone
else's credential and an endpoint it runs, and have banlieue deliver it: a vCenter password or a
Proxmox token is sent in the request. The policy makes the creating or updating principal prove it
could have read the Secret itself: `check('get')` on it, through the CEL `authorizer`.

Two things in that rule are broader than the threat:

1. **Every update is checked, whatever it changes.** A label, an annotation, a capability or a
   finalizer, written by a principal that cannot read the Secret, is refused, although it moves no
   credential anywhere. faubourg found this live (2026-11-19): it labels the Providers of the hosts
   it groups into zones (its ADR-0003), and its controller deliberately holds no Secret access (its
   ADR-0008), so it cannot label any Provider that names a credential: every libvirt Provider.
2. **Only `get` will do.** A controller that should place a reference without ever holding the
   credential has no way to be authorized. Granting it `get` defeats the reason it was built not
   to read Secrets: a compromise of it would then yield every credential it may reference. faubourg
   creates libvirt Providers for the hosts it bootstraps, naming a client certificate an operator
   made for it, and must not be able to read that key.

The threat is narrower than "writes a Provider": it is **choosing which credential goes to which
endpoint**. That choice is made by `spec.connection` as a whole: `credentialsRef`, but also
`endpoint`, `caBundle` and `insecureSkipTLSVerify`, since an unchanged reference re-pointed at an
attacker's endpoint, or stripped of its server verification, delivers the credential just as well.

## Decision

1. **Checked when the connection is set or changes.** On `CREATE`, and on an `UPDATE` whose
   `spec.connection` differs from the old object's, the rule applies as today. An `UPDATE` that
   leaves `spec.connection` exactly as it was is not checked: it cannot move a credential.
   Equality is CEL's structural equality on the whole object, so a change to any field of the
   connection, including one added later, is checked.
2. **`reference` on the Secret is enough.** The rule admits a principal that may `get` the Secret
   **or** may `reference` it: a custom verb, like `use` and `bind`, that RBAC grants and nothing in
   Kubernetes implements. `reference` lets a principal name a Secret in a Provider's connection. It
   reads nothing.
3. **`reference` is as strong as `get` toward the endpoint.** Whoever may reference a Secret and
   set an endpoint can have banlieue present that credential to an endpoint of their choice. For a
   password or a token, that is disclosure. For a libvirt client certificate, the private key never
   leaves the provider, but the holder's endpoint receives an authenticated session it can act on
   no further. So `reference` is granted only where `get` would be acceptable for the endpoint, or
   bounded the way `resourceNames` bounds it: by exact names, or by the grantee's own admission
   policy limiting which Secrets it may name. banlieue grants it to nobody by default.
4. **Messages say which permission is missing.** A refusal names the Secret and says the writer
   may neither `get` nor `reference` it.
5. **Nothing else changes.** The provider workload's Role still grants `get` on exactly the named
   Secret, created by the operator from the Provider. A host-resident Provider still names no
   credential (ADR-0060 Decision 4). `insecureSkipTLSVerify` and the endpoint checks in
   `provider-connection.yaml` are unchanged.

## Consequences

- A controller that groups or labels Providers no longer needs to read their credentials, and a
  label change by any principal stops failing on a Secret it cannot read.
- A principal granted `reference` can direct the credentials it may reference to any endpoint it
  may write. CHAIN-001's guarantee for such a principal therefore rests on how narrowly
  `reference` is granted: exact `resourceNames`, or the grantee's own policy. For faubourg, that is
  its `faubourg-provider-connection` policy, which admits a `credentialsRef` only to a Secret named
  `faubourg-...` that an operator made for it, a CA only from a `faubourg-...` ConfigMap, and never
  `insecureSkipTLSVerify` (faubourg ADR-0018 decision 6). The threat model's confused-deputy row
  says so, and the grant is documented beside the policy.
- A principal that could already `get` the Secret is unaffected.
- `oldObject` is null on `CREATE`, so the rule keeps checking every create.
- Implemented with: the policy change and its message
  (`deploy/admission/provider-credentialsref-authorization.yaml`); admission tests against a real
  API server (`crates/banlieue-controller/tests/live_credentialsref_policy.rs`,
  `make credentialsref-policy-live-test`) for a label update without `get` (admitted), an endpoint, CA or
  `insecureSkipTLSVerify` change with an unchanged `credentialsRef` and no `get` (refused), a create
  and an update by a principal with `reference` only (admitted), and a principal with neither
  (refused); CALM; the threat model's confused-deputy row. faubourg then grants its controller
  `reference` on Secrets in the provider namespace and amends its ADR-0018.
