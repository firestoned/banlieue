<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0096: banlieue as one clusterctl infrastructure provider, with a scoped CAPI aggregate role

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 12 §4.2 and §4.5; open decision O-004 in
  `.github/community/01-decisions.md` (closed by this ADR);
  [ADR-0005](0005-capi-contract-label-codegen.md) (the contract label);
  [ADR-0012](0012-providerclass-crd-and-operator-role.md),
  [ADR-0013](0013-banlieue-bootstrap-cli.md) (what an install contains);
  [ADR-0014](0014-kind-e2e-operator-contract.md) (contract e2e on kind)

## Context

banlieue's infrastructure CRDs (`VSphereMachine`, `ProxmoxMachine`,
`LibvirtMachine`, `CloudHypervisorMachine`, their templates, and
`VSphereCluster`) satisfy the Cluster API v1beta2 InfraMachine and
InfraCluster contracts, and carry the `cluster.x-k8s.io/v1beta2: v1alpha1`
label (ADR-0005). Three things stop a CAPI user from actually using them:

1. **There is no clusterctl packaging.** No `metadata.yaml`, no
   `infrastructure-components.yaml`, so `clusterctl init --infrastructure ...`
   has nothing to fetch.
2. **The CAPI aggregation over-grants.** `cluster.x-k8s.io/aggregate-to-manager: "true"`
   sits on `banlieue-controller`, the controller's *entire* ClusterRole. CAPI's
   manager therefore gains create and delete on every `banlieue.io` kind
   (`VirtualMachine`, `VirtualMachinePool`, ...), IPAM claims, Events and
   Leases. It needs none of that.
3. **Nothing tests the contract with CAPI itself.** No test runs a CAPI
   `Machine` against a banlieue infrastructure object.

## Decision

1. **One clusterctl provider, `banlieue`, type `InfrastructureProvider`,**
   not one per backend. Backends are compiled into one binary (ADR-0004) and
   selected at run time by `Provider` and `ProviderClass` resources
   (ADR-0012). Per-backend clusterctl providers would ship four copies of the
   same components with different names. `clusterctl init --infrastructure banlieue`
   installs banlieue; a `Provider` resource then chooses the backend.

2. **The components file is what `banlieue bootstrap operator` installs.**
   The release job renders it with `banlieue bootstrap operator --dry-run`
   into `infrastructure-components.yaml`, and attaches it with
   `metadata.yaml` to every GitHub Release. There is one install definition,
   ADR-0013's, with no second copy to drift. The namespace stays
   `banlieue-system`. The image tag is the release's, so the file needs no
   clusterctl variables. clusterctl passes every components file through
   envsubst, and the CRD descriptions quote ADR-0024's `${VM_NAME}` and
   `${FQDN}` placeholders, so the rendered file is the dry-run output
   **escaped for envsubst** (every `$` doubled). `make clusterctl-components`
   checks that unescaping it gives back the dry-run byte for byte.

3. **`metadata.yaml`** lives at `config/clusterctl/metadata.yaml` and maps
   each release series to contract `v1beta2`. A test asserts it has an
   entry for the workspace version's major and minor.

4. **A dedicated, minimal aggregate role.** A new ClusterRole,
   `banlieue-capi-infrastructure`, carries the aggregation label and grants
   CAPI's manager only what the contract needs, on `infrastructure.banlieue.io`
   only:
   - get, list, watch, create, update, patch and delete on the machine,
     machine-template and cluster kinds and their status (CAPI's
     MachineSet creates machines from templates and deletes them);
   - nothing in `banlieue.io`, no Secrets, IPAM, Events or Leases.

   The label comes **off** `banlieue-controller`. Both the static manifests
   and `banlieue bootstrap` install the new role.

5. **A contract e2e on kind, no hypervisor.** In the style of ADR-0014:
   1. Install CAPI core with `clusterctl init` (it brings cert-manager from
      its static manifest).
   2. Install banlieue from a local clusterctl repository built from this
      tree.
   3. Create a `Cluster` + `VSphereCluster` and a `Machine` + `VSphereMachine`.
   4. With no vSphere provider running, the test sets the infrastructure
      status the way a provider would (`initialization.provisioned`,
      `providerID`, addresses).
   5. Assert that CAPI's `Machine` controller reflects it
      (`status.initialization.infrastructureProvisioned=true`, the
      `providerID` copied), and that deleting the `Machine` deletes the
      `VSphereMachine`.

   This proves the contract and the RBAC against real CAPI controllers. A
   backend actually provisioning a CAPI-owned machine stays a live test
   against a real host.

6. **The constraint is documented:** banlieue provides the infrastructure
   role only. Bootstrap and control-plane providers (kubeadm, k0smotron,
   ...) come from upstream.

## Consequences

- CAPI's manager loses the `banlieue.io` grants it never needed. That
  narrows the threat model's CAPI-manager actor to the infrastructure group.
- Releases gain two assets, `infrastructure-components.yaml` and
  `metadata.yaml`, produced from code that is already tested.
- A user who installed banlieue before this change and relies on the
  aggregation must re-apply the RBAC. The old label disappears with the
  re-applied controller role, and the new role restores exactly the CAPI
  permissions.
- The e2e pins a CAPI release, because v1beta2 behaviour is still settling
  upstream. Bumping it is a deliberate change.
- O-004 ("CAPI clusterctl integration shape") is closed.
