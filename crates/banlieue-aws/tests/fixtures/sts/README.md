<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# STS response fixtures

Loaded by `src/sts_tests.rs` and `src/auth_tests.rs` with `include_str!`.
Read on 2026-11-26.

| File | Source | Changes |
| --- | --- | --- |
| `assume-role-with-web-identity.xml` | [AssumeRoleWithWebIdentity](https://docs.aws.amazon.com/STS/latest/APIReference/API_AssumeRoleWithWebIdentity.html), Examples, sample response | The documentation account ID in the assumed-role ARN is replaced with the placeholder `111122223333`. The keys, session token and request ID are AWS's published `EXAMPLE` values. |
