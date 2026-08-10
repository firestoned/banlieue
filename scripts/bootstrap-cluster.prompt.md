<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Prompt — bootstrap a k0s management cluster (vSphere backend)

Hand this prompt to Claude Code to stand up a new k0s management cluster on
vSphere the same way the `banlieue` cluster was built (ADR-0017): clone Kairos
templates with `govc`, install k0s **natively** (binary in `/opt/k0s`, symlink
`/usr/local/bin/k0s`), then (optionally, `FLUX_ENABLED=true`) fetch a registry
credential from Vault and automate flux-operator + a `flux-core` Kustomization
(ADR-0018). MetalLB is still a manual follow-up (§4).

> All real hostnames/IPs live ONLY in an untracked env file
> (`BANLIEUE_ENV_FILE`, e.g. `~/.k0s/<cluster>.env`) and the ambient `GOVC_*`
> environment — never in the repo (see `rules/no-real-infrastructure.md`).
> On-prem: always `unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY
> ALL_PROXY` before *any* on-prem call (`govc`/`kubectl`/`ssh`) -- a corporate
> proxy black-holes them. `bootstrap-k0s-cluster.sh` now does this itself on
> every invocation; do the same in any ad hoc command you run outside the
> script. The Mac ships bash 3.2, so keep helper snippets 3.2-safe.

## 1. Ask the operator for these inputs

Ask up front (one round of questions); nothing here has a safe default:

- **vCenter access** — confirm `GOVC_URL`, `GOVC_USERNAME/PASSWORD`,
  `GOVC_DATACENTER`, `GOVC_TLS_CA_CERTS` are exported (source their shell rc).
- **VM template** — name (e.g. `rhelXX-kairos-<ver>`); confirm it exists per
  compute cluster (`govc find / -type m -name '<tpl>'`).
- **Topology / node table** — per node: FQDN, vSphere cluster id, **static IP**,
  role (`controller+worker` | `worker`). Recommend an odd number of controllers
  spread one-per-cluster for an etcd quorum across failure domains.
- **Per-cluster placement** — resource pool, SDRS datastore-cluster path, DVS
  port group (discover with `govc find` / `govc ls`; confirm each resolves).
- **Networking** — prefix, gateways (default = first-3-octets`.1`), DNS servers,
  search domain, and the **API SAN / cluster name** (must eventually resolve;
  until DNS exists, use a kubeconfig pointed at a controller IP).
- **Sizing** — vCPU / memory / disk per node.
- **k0s** — version (e.g. `v1.35.1+k0s.1`), binary base URL (internal
  Artifactory mirror for air-gapped), image repository mirror, CNI
  (`calico`/`kuberouter`), and calico `can-reach` address.
- **SSH** — user (usually `root`) + key path (public key is injected into VMs).
- **MetalLB** — an address range per zone/subnet.
- **flux** (optional, `FLUX_ENABLED=true`; ADR-0018) — confirm `VAULT_ADDR`
  and `VAULT_TOKEN` are exported (token auth only, read by the `vault` CLI
  itself — **do not** write either into the env file); the Vault
  `VAULT_KV_MOUNT`/`VAULT_SECRET_PATH` and field names
  (`VAULT_FLUX_USER_KEY`/`VAULT_FLUX_PASS_KEY`, default `flux`/`flux-pass`)
  holding the registry credential; `FLUX_REGISTRY` (imagePullSecret target)
  and `FLUX_CORE_OCI_URL` (`oci://.../flux-core/main`); optionally
  `FLUX_CA_BUNDLE_FILE` (a local CA file — there is no automated CA fetch) and
  `FLUX_SUBSTITUTIONS` (operator-defined `KEY=VALUE` pairs for
  `postBuild.substitute`, alongside the always-injected `CLUSTER_DNS`). Never
  fabricate or paste secret values yourself — they come from Vault at
  bootstrap time.

Confirm the three node subnets are **mutually routable** before proceeding.

## 2. Write the untracked env file

Run `scripts/bootstrap-k0s-cluster.sh --print-env-template` and fill it in from
the answers (flat `VSPHERE_{TPL,RP,DSC,NET}_<id>` vars + `NODES` array +
`K0S_*`, plus the `VAULT_*`/`FLUX_*` vars from §1 if `FLUX_ENABLED=true`). Save
it outside the repo, `chmod 600`.

## 3. Bring up the cluster (gate before the destructive step)

```sh
source ~/.zshrc; unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY
export BANLIEUE_ENV_FILE=~/.k0s/<cluster>.env
# read-only discovery first; SHOW the resolved plan and get a go, THEN:
scripts/bootstrap-k0s-cluster.sh vms      # clone + wait for install (root fs -> /dev/loop0)
scripts/bootstrap-k0s-cluster.sh config   # stage k0s binary into /opt/k0s + write /etc/k0s/k0s.yaml
scripts/bootstrap-k0s-cluster.sh apply    # k0s install controller/worker + token joins
scripts/bootstrap-k0s-cluster.sh kubeconfig
scripts/bootstrap-k0s-cluster.sh label    # label workers banlieue.io/imagebuild=true
scripts/bootstrap-k0s-cluster.sh flux     # no-op unless FLUX_ENABLED=true -- see §5
```

(Or run `scripts/bootstrap-k0s-cluster.sh all`, which chains every step above
including `flux`.)

Verify: `k0s kubectl get nodes` all `Ready`, `k0s etcd member-list` shows one
controller per failure domain. If the API SAN has no DNS yet, make a kubeconfig
copy whose `server:` is a controller IP (it's already in the cert SANs).

Konnectivity: on a flat, routable network keep `K0S_DISABLE_KONNECTIVITY=true`
(the default). With it enabled on a multi-controller cluster that has no single
`externalAddress`/VIP, the agents pin to one controller and `kubectl logs/exec`
against any other returns **"No agent available"**. Disabling it makes the API
server reach kubelets directly. Set it `false` only if the network is not flat.

## 4. MetalLB (mirror the reference cluster)

Label nodes `topology.kubernetes.io/zone=<az>`; deploy the mirror-ized MetalLB
manifest + per-zone `IPAddressPool` + `L2Advertisement` (nodeSelector on the
zone label) via the k0s manifest deployer (`/var/lib/k0s/manifests/metallb/`).

## 5. flux-operator + flux-core (optional, `FLUX_ENABLED=true`)

Automated — no manual manifest staging. With `FLUX_ENABLED=true` and the Vault
vars from §1 set in the env file (`VAULT_ADDR`/`VAULT_TOKEN` exported in your
shell, not in the file):

```sh
scripts/bootstrap-k0s-cluster.sh flux   # or just include it in `all`
```

This fetches the registry credential from Vault via the `vault` CLI (token
auth; `vault kv get`, so KV v1/v2 doesn't matter), finds the first controller
from the same node table `apply`/`kubeconfig` already used, and pushes
`00-install.yaml` (flux-operator's install manifest), `10-pre-reqs.yaml`
(namespace + `flux-artifactory` dockerconfigjson + optional `flux-tls`),
`30-flux-instance.yaml` (`FluxInstance` + `flux-core` `OCIRepository`), and
`40-flux-bootstrap.yaml` (the `flux-core` `Kustomization`) onto
`/var/lib/k0s/manifests/flux-operator/` — k0s applies them automatically.
Once it reconciles, the operator can layer on whatever DNS/MetalLB publishing
(§4) their `flux-core` artifact expects.

## Guardrails
- Confirm before creating/destroying VMs (outward-facing, hard to reverse).
- Grep your diff for real identifiers before finishing.
- Idempotent: every step can be re-run; `destroy` tears the VMs down.
