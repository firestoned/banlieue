<!-- Copyright (c) 2026 Erick Bourgeois, banlieue -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# banlieue admission policies

Optional, in-API-server hardening for banlieue CRDs using
[ValidatingAdmissionPolicy](https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/)
(CEL, GA in Kubernetes **1.30+**) and, for one file,
[MutatingAdmissionPolicy](https://kubernetes.io/docs/reference/access-authn-authz/mutating-admission-policy/)
(CEL, GA in Kubernetes **1.36+**). These enforce invariants the CRD OpenAPI
schema cannot express — cross-field and immutability rules, and (for the one
mutating policy) stamping an identity annotation — at admission time, before
the object is ever persisted, with no webhook to run or certificates to
rotate.

| Policy | Enforces |
| --- | --- |
| `virtualmachine-immutability.yaml` | `VirtualMachine.spec.classRef` / `spec.imageRef` are immutable after creation. |
| `provider-immutability.yaml` | `Provider.spec.providerClassRef.name` is immutable after creation. |
| `provider-cabundle-source.yaml` | `Provider.spec.connection.caBundle` sets exactly one of `inline` / `configMapRef` / `secretRef` (ADR-0008). |
| `provider-attestation-ektrustbundle.yaml` | `Provider.spec.attestation.ekTrustBundle` sets exactly one of `inline` / `configMapRef` / `secretRef` ([ADR-0049](../../docs/adr/0049-attestation-trust-anchors.md) Decision 10). |
| `provider-connection.yaml` | `Provider.spec.connection.endpoint` is an absolute URL, `https://` for vsphere/proxmox, no userinfo or fragment; `insecureSkipTLSVerify: true` requires the opt-in annotation `banlieue.io/allow-insecure-tls: "true"` (security review 2026-07-31). |
| `provider-credentialsref-authorization.yaml` | The principal creating/updating a `Provider` must be authorized to `get` the Secret named by `spec.connection.credentialsRef` (CEL `authorizer`; security review 2026-07-31). |
| `virtualmachine-userdata-authorization.yaml` | The principal creating/updating a `VirtualMachine` must be authorized to `get` the Secret or ConfigMap named by `spec.userData` (CEL `authorizer`; [ADR-0042](../../docs/adr/0042-userdata-reference-authorization.md)). |
| `vmimage-import-source.yaml` | Every `VMImage.spec.sources[].importFrom` is pinned to an `@sha256:` digest and references a registry in the `banlieue-vmimage-allowed-registries` parameter ConfigMap (security review 2026-07-31). |
| `virtualmachineclaim-subject-authorization.yaml` | `VirtualMachineClaim.spec.subject.id` must equal the authenticated username (declared brokers exempt), `spec.subject.issuer` must be in the `banlieue-claim-subject-policy` parameter ConfigMap, and `spec` is immutable ([ADR-0047](../../docs/adr/0047-virtualmachineclaim.md) Decision 10). |
| `providerclass-guardrails.yaml` | `ProviderClass.spec.additionalRules` may not grant on `secrets`, use `*` resources/verbs, or use `escalate`/`bind`/`impersonate`; `spec.workloadNamespace` may not be a Kubernetes system namespace (security review 2026-07-31). |
| `virtualmachine-created-by.yaml` | Stamps `banlieue.io/created-by` on a `VirtualMachine` at CREATE from `request.userInfo.username` — an informational attribute, not an authorization boundary ([ADR-0089](../../docs/adr/0089-vspheremachine-custom-attributes.md)). The only `MutatingAdmissionPolicy` in this directory; needs Kubernetes 1.36+. |

Apply after the CRDs:

```sh
kubectl apply -f deploy/crds/
kubectl apply -f deploy/admission/
```

Each file ships a `ValidatingAdmissionPolicy` (the rule) and a
`ValidatingAdmissionPolicyBinding` with `validationActions: ["Deny"]` (enforce).
Switch a binding to `["Warn","Audit"]` to roll out in report-only mode first.
`virtualmachine-created-by.yaml` is the one exception: it ships a
`MutatingAdmissionPolicy` + `MutatingAdmissionPolicyBinding` instead, with
`failurePolicy: Ignore` rather than `Deny`/enforce — it stamps an annotation,
it does not reject anything.

Notes:

- `virtualmachineclaim-subject-authorization.yaml` ships its parameter
  ConfigMap first, for the same reason, and needs the `authorizer`-era
  apiserver plus CEL `variables`. **Edit its `issuers` list**: the shipped
  value is a placeholder, and an unedited cluster rejects every claim. Its
  `brokers` list is empty by default and is a trust concentration — anyone
  named there may attribute a sandbox to any identity.
- `vmimage-import-source.yaml` also ships its parameter ConfigMap (first
  document in the file — the binding fails closed when it is missing). **Edit
  the `registries` list per site** before applying; the defaults are
  convenience values, not a recommendation.
- `provider-credentialsref-authorization.yaml` and
  `virtualmachine-userdata-authorization.yaml` need an apiserver new enough to
  support the CEL `authorizer` variable in admission policies; on an older
  apiserver those files are rejected while the rest still apply.
- **`virtualmachine-userdata-authorization.yaml` changes who may create a
  `VirtualMachine` that references user-data.** Any automation (CI, GitOps)
  creating VMs must itself hold `get` on the referenced Secret/ConfigMap.
  Roll it out as `["Warn","Audit"]` first if you are unsure which identities
  are affected. Without it, `create virtualmachines` in the controller's
  namespace is equivalent to reading every Secret in that namespace — see the
  [threat model](../../docs/src/security/threat-model.md).

- `virtualmachine-created-by.yaml` needs Kubernetes 1.36+
  (`MutatingAdmissionPolicy` GA) — higher than every `ValidatingAdmissionPolicy`
  floor above. On an older apiserver this one file is rejected while the rest
  still apply; `VSphereMachine`'s `CreatedBy` vCenter custom attribute is then
  simply never set (`Template`/`CreatedAt` are unaffected — see ADR-0089).
  `MutatingAdmissionPolicy` is beta (off by default) on 1.34–1.35 and GA (on
  by default) from 1.36 — `scripts/bootstrap-k0s-cluster.sh`'s default
  `K0S_VERSION` is 1.35.x, so a cluster built with it needs the feature gate
  turned on explicitly before this file can be applied at all. Use
  `scripts/enable-feature-gate-k0s.sh enable` (`FEATURE_GATES=MutatingAdmissionPolicy=true`
  by default) to do that cluster-wide, one controller at a time, with a
  backup and automatic rollback if an apiserver doesn't come back healthy.

Rationale (VAP vs. webhook vs. CRD-embedded CEL) is recorded in
[ADR-0007](../../docs/adr/0007-admission-policies.md); the mutating-policy
rationale is in [ADR-0089](../../docs/adr/0089-vspheremachine-custom-attributes.md);
the attack chains the validating policies break are in the 2026-07-31
security review and the [threat model](../../docs/src/security/threat-model.md).
