<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0126: A Provider's status publishes its ServiceAccount's name

- **Status:** Accepted
- **Date:** 2026-11-09
- **Proposed:** 2026-11-09
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md) (External
  providers: the operator gives the provider an identity and runs nothing);
  [ADR-0102](0102-provider-liveness-from-its-lease.md) (`status.workload`). faubourg ADR-0011
  and its accepted risk R-10. ADR-0116 to ADR-0122 are reserved by roadmap 20; ADR-0125 is
  proposed separately.

## Context

The operator gives every Provider an identity: a ServiceAccount named
`banlieue-provider-<class>-<provider>`, shortened with a hash when it would exceed a name's
length, in the workload's namespace (the Provider's own for an External class, ADR-0060). A
host-resident provider authenticates as it, with a token someone outside banlieue mints.

`status.workload` says the provider's mode, its Deployment and its namespace, but not that
name. So a consumer that mints the first token has to re-derive the name, truncation included.
faubourg does exactly that to bootstrap a Cloud Hypervisor host, and records it as an accepted
risk (its R-10): any change to banlieue's naming would have faubourg mint for a name that is not
the provider's, and a host that never comes up.

## Decision

1. **`status.workload.serviceAccountName`.** The operator writes the name of the ServiceAccount
   it created for the Provider, beside `namespace`, which is that ServiceAccount's namespace.
   For Managed and External providers alike.
2. **Written only once the ServiceAccount exists.** It is published in the same pass that applies
   the workload, after the ServiceAccount, so a consumer that reads it can mint for it.
3. **The naming stays the operator's.** Consumers read the name instead of deriving it; banlieue
   may change the naming in a later release without breaking them.
4. **Additive.** The field is optional. A Provider written by an older operator has no value, and
   a consumer that needs it should wait for it rather than guess.

## Alternatives considered

- **Publish the naming function** (a crate, or a documented rule). Every consumer would still
  carry a copy, and change in step with banlieue; reading the object removes the copy.
- **A label on the ServiceAccount**, found by selector. One more read and one more permission
  for every consumer, to learn a string the operator already knows.

## Consequences

- `ProviderWorkloadStatus` gains `service_account_name`, the CRD changes additively, and the
  operator's status tests cover both modes.
- faubourg stops deriving the name: it waits for the field, mints for it, and closes R-10. Its
  token policy still bounds the name by prefix, so a wrong value would be refused, not trusted.
- Nothing new is granted. The operator already creates the ServiceAccount and writes the
  Provider's status.
