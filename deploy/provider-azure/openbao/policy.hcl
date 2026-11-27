# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# The policy for ONE azure Provider's workload (ADR-0176 Decision 2): it reads
# exactly one identity OIDC token and nothing else. Replace <tokenRole> with the
# Provider's `spec.connection.azure.openbao.tokenRole`, and write it once per
# workload, never shared: a workload that can read another's token can act as
# another's cloud identity.
#
#   bao policy write azure-<provider name> deploy/provider-azure/openbao/policy.hcl
#
# No `list`, no `update`, no other path. Revoking its own login token needs no
# grant: `auth/token/revoke-self` is part of OpenBao's default policy.
path "identity/oidc/token/<tokenRole>" {
  capabilities = ["read"]
}
