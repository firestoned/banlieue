<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0018 — Vault-backed flux-operator automation for the k0s bootstrap script

- **Status:** Accepted
- **Date:** 2026-08-08
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0017 (pluggable `BACKEND` for the k0s management-cluster
  bootstrap script, which this ADR extends).

## Context

`scripts/bootstrap-k0s-cluster.sh` already mirrors the VM-provisioning and
native k0s-install halves of `~/dev/mke-build`'s `k0s-control-plane.yaml`
playbook (ADR-0017). One piece of that Ansible flow is not yet automated: what
`k0s-deploy-manifests.yaml` + `roles/flux-prereqs` + `roles/vault` do — fetch
registry credentials from HashiCorp Vault and drop flux-operator + `flux-core`
manifests onto the cluster so Flux takes over reconciliation from there. The
maintainer wants the bash script + `scripts/bootstrap-cluster.prompt.md` to
fully replace the mke-build playbook, which requires closing this gap.

Studying the reference Ansible role clarified the actual shape of what needs
porting:

1. **Auth is token-only.** `roles/vault/tasks/setup.yaml` reads a mandatory
   `VAULT_TOKEN` from the environment and performs no login/AppRole/Kubernetes
   auth flow. All secret fetches go through the Ansible `hashi_vault` lookup
   plugin — there is no shell `vault` CLI usage anywhere in the reference repo,
   but the auth model it relies on (a pre-existing token in the environment)
   maps directly onto the `vault` CLI reading `VAULT_ADDR`/`VAULT_TOKEN` itself.
2. **Only one secret is actually needed here.** `roles/vault/tasks/vmware.yaml`
   fetches an SSH keypair and vCenter guest-customization credentials —
   irrelevant to banlieue, whose vSphere flow already gets VM SSH access from
   the local `SSH_PUBKEY`/`SSH_PRIVKEY` and vCenter auth from ambient `GOVC_*`
   (ADR-0017). The only secret the flux-operator manifests actually consume is
   the Artifactory-style registry username/password
   (`roles/vault/tasks/service_ids.yaml`: `flux`/`flux-pass`), used to build a
   `dockerconfigjson` image-pull secret.
3. **The CA bundle is not a Vault secret.** The CA bundle (injected into the
   `flux-tls` Secret) comes from an internal HTTP API call in
   `roles/common/tasks/main.yaml`, not from Vault. That endpoint and its
   response shape are estate-proprietary and out of scope for a public repo.
4. **Every path/hostname in the reference is estate-specific.** The Vault mount
   and path, the flux OCI registries, and the image mirror in the reference
   repo are all real infrastructure identifiers (`rules/no-real-infrastructure.md`)
   — none may be copied literally into banlieue; each becomes an
   operator-supplied env var instead.

## Decision

Add an opt-in (`FLUX_ENABLED=true`) `flux` step to
`scripts/bootstrap-k0s-cluster.sh`, generic and backend-agnostic:

**Vault access via the `vault` CLI, token auth only.** A `vault_kv_get <field>`
helper wraps `vault kv get -format=json -mount="$VAULT_KV_MOUNT" -field=<field>
"$VAULT_SECRET_PATH"`. Using the `vault kv` subcommand (not `vault read`)
means banlieue never needs to know or hardcode the KV v1 vs v2 `/data/` path
convention the way the reference repo's raw `hashi_vault` lookup does.
`VAULT_ADDR`/`VAULT_TOKEN` are read by the `vault` binary itself from the
ambient environment — no login flow is implemented, matching the reference's
actual auth model rather than inventing AppRole/Kubernetes auth that isn't
used there either. `VAULT_KV_MOUNT` and `VAULT_SECRET_PATH` are
operator-supplied (untracked env file), never a hardcoded prefix. A short
retry loop (3 attempts, 5s backoff) handles transient/rate-limited lookups —
a lighter version of the reference's 5-tier retry.

**Only the registry credential is fetched from Vault.** `VAULT_FLUX_USER_KEY`/
`VAULT_FLUX_PASS_KEY` (default `flux`/`flux-pass`, matching the reference's
field names, but overridable) resolve to a username/password used to build a
single `flux-artifactory` `dockerconfigjson` Secret for `FLUX_REGISTRY` — one
operator-supplied registry, not the reference's three estate-specific mirrors.

**CA bundle and image mirror become operator-supplied, not automated.**
`FLUX_CA_BUNDLE_FILE` is an optional local file; if set, its contents become
the `flux-tls` Secret and `OCIRepository.spec.certSecretRef` is wired in — if
unset, the `flux-tls` Secret and `certSecretRef` are both skipped entirely.
No HTTP CA-fetch flow is implemented. `FLUX_OPERATOR_REGISTRY` defaults to the
public `ghcr.io/controlplaneio-fluxcd` and `FLUX_OPERATOR_INSTALL_URL`
defaults to the public GitHub releases URL; both are overridable to an
internal mirror the same way `K0S_BINARY_BASEURL` already is (ADR-0017).

**Deployment is backend-agnostic.** `deploy_flux` reuses the existing
`populate_node_table` (already resolves a uniform `NODE_TABLE` for both
`vsphere` and `libvirt`) to find the first `controller*` node, then `ssh_run`
heredocs four manifests onto `/var/lib/k0s/manifests/flux-operator/`:
`00-install.yaml` (flux-operator's install manifest, curled and optionally
mirror-rewritten), `10-pre-reqs.yaml` (namespace + `flux-tls` +
`flux-artifactory`), `30-flux-instance.yaml` (`FluxInstance` +
`flux-core` `OCIRepository`), `40-flux-bootstrap.yaml` (the `flux-core`
`Kustomization`, `postBuild.substitute` built from `CLUSTER_DNS`/`CLUSTER_ENV`
plus an operator-supplied `FLUX_SUBSTITUTIONS` KEY=VALUE list — the reference's
hardcoded appcode substitution has no banlieue equivalent, so substitution
keys are entirely operator-defined). k0s's manifest deployer auto-applies
anything dropped there for both the native (vsphere) and k0sctl (libvirt)
install paths, so no per-backend branching is needed here.

`check_deps` gains a `FLUX_ENABLED`-gated block requiring the `vault` binary
and failing fast if `VAULT_TOKEN`/`VAULT_ADDR`/`VAULT_KV_MOUNT`/
`VAULT_SECRET_PATH`/`FLUX_CORE_OCI_URL` are unset — the same fail-fast pattern
`check_deps_vsphere` already uses for `NODES`/`DNS_SERVERS`.

## Consequences

**Positive**

- Closes the remaining gap between the bash script and the mke-build
  playbook: VM provisioning, native k0s install, and now flux-operator
  bootstrap are all one script, one prompt.
- No real infrastructure identifier is introduced — Vault mount/path,
  registry hostname, OCI artifact URL, and CA bundle are all operator-supplied.
- `deploy_flux` needs no backend-specific code because it rides on the
  `NODE_TABLE` abstraction ADR-0017 already established.

**Negative / trade-offs**

- **MetalLB automation is explicitly deferred.** `bootstrap-cluster.prompt.md`
  §4 remains a manual step; this ADR only covers flux-operator.
- **Vault auth is token-only**, matching the reference repo's actual usage —
  AppRole/Kubernetes auth is not implemented. An estate that requires those
  will need to export a token some other way (e.g. `vault write` against
  AppRole role/secret IDs before invoking the script) rather than have the
  script do it.
- **No automated CA-bundle fetch.** Estates whose registry needs a custom CA
  must supply `FLUX_CA_BUNDLE_FILE` themselves; there is no equivalent of the
  reference's internal CA-fetch API integration, by design (that endpoint is
  estate-proprietary).

**Follow-ups**

- MetalLB automation, if wanted later, is a natural next ADR — the node
  table and zone labelling already exist (`label_imagebuild_node`'s pattern
  generalizes).
