<!-- Copyright (c) 2026 Erick Bourgeois, banlieue -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# banlieue admission policies

Optional, in-API-server hardening for banlieue CRDs using
[ValidatingAdmissionPolicy](https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/)
(CEL, GA in Kubernetes **1.30+**). These enforce invariants the CRD OpenAPI
schema cannot express — cross-field and immutability rules — at admission time,
before the object is ever persisted, with no webhook to run or certificates to
rotate.

| Policy | Enforces |
| --- | --- |
| `virtualmachine-immutability.yaml` | `VirtualMachine.spec.classRef` / `spec.imageRef` are immutable after creation. |
| `provider-immutability.yaml` | `Provider.spec.providerClassRef.name` is immutable after creation. |
| `provider-cabundle-source.yaml` | `Provider.spec.connection.caBundle` sets exactly one of `inline` / `configMapRef` / `secretRef` (ADR-0008). |
| `provider-connection.yaml` | `Provider.spec.connection.endpoint` is an absolute URL, `https://` for vsphere/proxmox, no userinfo or fragment; `insecureSkipTLSVerify: true` requires the opt-in annotation `banlieue.io/allow-insecure-tls: "true"` (security review 2026-07-31). |
| `provider-credentialsref-authorization.yaml` | The principal creating/updating a `Provider` must be authorized to `get` the Secret named by `spec.connection.credentialsRef` (CEL `authorizer`; security review 2026-07-31). |
| `vmimage-import-source.yaml` | Every `VMImage.spec.sources[].importFrom` is pinned to an `@sha256:` digest and references a registry in the `banlieue-vmimage-allowed-registries` parameter ConfigMap (security review 2026-07-31). |
| `providerclass-guardrails.yaml` | `ProviderClass.spec.additionalRules` may not grant on `secrets`, use `*` resources/verbs, or use `escalate`/`bind`/`impersonate`; `spec.workloadNamespace` may not be a Kubernetes system namespace (security review 2026-07-31). |

Apply after the CRDs:

```sh
kubectl apply -f deploy/crds/
kubectl apply -f deploy/admission/
```

Each file ships a `ValidatingAdmissionPolicy` (the rule) and a
`ValidatingAdmissionPolicyBinding` with `validationActions: ["Deny"]` (enforce).
Switch a binding to `["Warn","Audit"]` to roll out in report-only mode first.

Notes:

- `vmimage-import-source.yaml` also ships its parameter ConfigMap (first
  document in the file — the binding fails closed when it is missing). **Edit
  the `registries` list per site** before applying; the defaults are
  convenience values, not a recommendation.
- `provider-credentialsref-authorization.yaml` needs an apiserver new enough
  to support the CEL `authorizer` variable in admission policies; on an older
  apiserver that one file is rejected while the rest still apply.

Rationale (VAP vs. webhook vs. CRD-embedded CEL) is recorded in
[ADR-0007](../../docs/adr/0007-admission-policies.md); the attack chains the
three security policies break are in the 2026-07-31 security review.
