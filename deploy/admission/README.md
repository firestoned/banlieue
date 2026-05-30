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

Apply after the CRDs:

```sh
kubectl apply -f deploy/crds/
kubectl apply -f deploy/admission/
```

Each file ships a `ValidatingAdmissionPolicy` (the rule) and a
`ValidatingAdmissionPolicyBinding` with `validationActions: ["Deny"]` (enforce).
Switch a binding to `["Warn","Audit"]` to roll out in report-only mode first.

Rationale (VAP vs. webhook vs. CRD-embedded CEL) is recorded in
[ADR-0007](../../docs/adr/0007-admission-policies.md).
