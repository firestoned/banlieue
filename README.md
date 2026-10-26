<!-- Copyright (c) 2026 Erick Bourgeois, banlieue -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# banlieue

> **Pronunciation:** IPA `/bɑ̃.ljø/` — "bohn-lyuh": a nasalized "bon" (as in
> French "blanc") followed by "lyuh" with a rounded French **u** (like
> German ü, or French "tu").

> A Kubernetes-native, **provider-agnostic** virtualization API.
> One CRD. Many backends. Swap them without touching the user's manifest.

[![Build](https://github.com/firestoned/banlieue/actions/workflows/build.yaml/badge.svg?branch=main)](https://github.com/firestoned/banlieue/actions/workflows/build.yaml)
[![Documentation](https://github.com/firestoned/banlieue/actions/workflows/docs.yaml/badge.svg?branch=main)](https://github.com/firestoned/banlieue/actions/workflows/docs.yaml)
[![CodeQL](https://github.com/firestoned/banlieue/actions/workflows/codeql.yaml/badge.svg?branch=main)](https://github.com/firestoned/banlieue/actions/workflows/codeql.yaml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/firestoned/banlieue/badge)](https://scorecard.dev/viewer/?uri=github.com/firestoned/banlieue)

[![License](https://img.shields.io/github/license/firestoned/banlieue?color=blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.89%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![Docs site](https://img.shields.io/badge/docs-firestoned.github.io%2Fbanlieue-informational?logo=materialformkdocs)](https://firestoned.github.io/banlieue/)
[![Status](https://img.shields.io/badge/status-In%20Development-orange)](#project-status)
[![Issues](https://img.shields.io/github/issues/firestoned/banlieue)](https://github.com/firestoned/banlieue/issues)
[![Last commit](https://img.shields.io/github/last-commit/firestoned/banlieue/main)](https://github.com/firestoned/banlieue/commits/main)
[![PRs welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/firestoned/banlieue/pulls)

---

**banlieue** lets you declare a virtual machine the same way you declare a
`Deployment` or `Service` — one Kubernetes Custom Resource — and have it
scheduled onto whatever hypervisor or VM platform you run: **Cloud
Hypervisor** on your own KVM hosts (the reference backend), **vSphere**,
**libvirt**, **Proxmox VE**, and any backend a third party writes. The user's
manifest never changes when the backend does.

```yaml
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: db-prod-01
spec:
  classRef:                 # references a VMClass (the hardware "shape")
    name: db-prod-large
  imageRef:                 # references a VMImage (the OS)
    name: ubuntu-22.04-cloudinit
  placement:                # optional — where it may land
    providerSelector:
      matchLabels: { dc: dc1, env: prod }
  desiredPowerState: PoweredOn
```

That single CR is scheduled onto a `Provider`, which resolves it to a concrete,
backend-specific infrastructure CR (e.g. a `VSphereMachine`) that a provider
controller turns into a real VM — all over the Kubernetes API, with no new
transport or auth to operate.

## Cloud Hypervisor first

Every backend runs the same `VirtualMachine`; **Cloud Hypervisor** is the
reference one. A small Rust VMM on a plain KVM host, with no hypervisor
daemon:

- the provider runs on the host, holding a self-renewing ServiceAccount token
  and **no hypervisor credential and no Secret**;
- each guest is one VMM in its own systemd unit, **under its own uid**,
  sandboxed by systemd, seccomp and Landlock;
- all guests share a **declared budget**, and the scheduler places a VM only
  where it fits;
- images come from an OCI registry **by digest**, and every guest can have a
  vTPM whose EK certificate is read on the host;
- a warm pool refills a claimed `GuestReady` VM in about 100 to 120 s.

Start with the
**[Cloud Hypervisor Provider guide](https://firestoned.github.io/banlieue/guides/cloud-hypervisor-host/)**;
the **[Provider Comparison](https://firestoned.github.io/banlieue/reference/provider-comparison/)**
lines it up against vSphere, libvirt and Proxmox.

## Why banlieue?

The VM control plane is fragmented: every team running more than one hypervisor
writes the same glue twice. banlieue makes the VM a first-class, declarative,
backend-agnostic Kubernetes object.

- **One declarative API** for VMs, regardless of backend.
- **Swap or mix providers** without rewriting workloads: Cloud Hypervisor on
  bare-metal KVM hosts, vSphere in the data centre, libvirt or Proxmox at the
  edge, in the same cluster.
- **Zero new transports.** The contract between the main controller and the
  providers is the Kubernetes API itself: no gRPC, no REST, no custom auth.
- **A battle-tested status model.** Provider infrastructure CRDs satisfy the
  [Cluster API v1beta2 InfraMachine contract](https://cluster-api.sigs.k8s.io/developer/providers/contracts/),
  so they double as reusable CAPI infrastructure providers.

### Warm pools and single-use sandboxes

Some VMs cannot be provisioned on demand. A disk sealed to a per-VM vTPM has to
install itself on first boot with its own TPM attached, which costs minutes —
and that slowness *is* the security property, so no provisioning trick removes
it. The only way to hand such a VM over quickly is to have built it already.

- A **`VirtualMachinePool`** keeps a warm set standing by: it refills, reaps
  members poisoned mid-install, and rolls the set when the image is rebuilt. It
  creates nothing but ordinary `VirtualMachine`s, so no provider learns that
  pools exist.
- A **`VirtualMachineClaim`** hands one member to one subject, **once**. There
  is no unbind and no return-to-pool — the VM itself is the isolation boundary,
  so release is always deletion, and deleting a claim blocks until the backend
  VM is really gone. `ttlSeconds` is mandatory: a claim without a deadline is a
  leaked VM.

This is what makes banlieue usable as a sandbox substrate for CI jobs and AI
agents. Pools are validated end to end on Cloud Hypervisor, libvirt and
vSphere hosts, and claims on Cloud Hypervisor and libvirt. The claim is the top of banlieue's API
([ADR-0097](docs/adr/0097-api-ends-at-virtualmachineclaim.md)): agent-specific
layers such as `AgentSandbox` live in [mediatore](https://github.com/firestoned/mediatore).

> **Guides:** [VirtualMachine Pools](https://firestoned.github.io/banlieue/guides/virtualmachine-pools/)
> · [VirtualMachine Claims](https://firestoned.github.io/banlieue/guides/virtualmachine-claims/)

### What banlieue is **not**

- Not a hypervisor.
- Not a "VMs-as-containers" shim (that's [KubeVirt](https://kubevirt.io/)).
- Not a CAPI replacement — a `VirtualMachine` is **not** a `clusterv1.Machine`.
  banlieue coexists with Cluster API but does not depend on it.
- Not a closed system — providers are a documented contract; anyone can write one.

## Architecture

The main controller **never speaks directly to a provider.** Both sides watch
the Kubernetes API server — that is the bus. The controller schedules a
`VirtualMachine` and creates a provider-specific infrastructure CR; the provider
controller reconciles that CR against its backend and reports status back; the
main controller mirrors that status onto the `VirtualMachine`.

> **📐 Architecture diagram + reconcile flow:**
> [`docs/src/concepts/architecture.md`](docs/src/concepts/architecture.md)
> (rendered: [Architecture](https://firestoned.github.io/banlieue/concepts/architecture/)).
> It's the single source of truth for the component diagram — GitHub renders the
> Mermaid inline when you open the file.

| Resource | Group | What it is |
| --- | --- | --- |
| `VirtualMachine` | `banlieue.io/v1alpha1` | The user-facing request for a running VM. |
| `VMClass` | `banlieue.io/v1alpha1` | A reusable hardware "shape" + capability requirements. |
| `VMImage` | `banlieue.io/v1alpha1` | A backend-agnostic, multi-source OS image catalog entry. |
| `Provider` | `banlieue.io/v1alpha1` | One registered backend instance (a Cloud Hypervisor host, a vCenter, a libvirt host, a Proxmox node). |
| `VirtualMachinePool` | `banlieue.io/v1alpha1` | A self-refilling set of warm, already-provisioned `VirtualMachine`s. |
| `VirtualMachineClaim` | `banlieue.io/v1alpha1` | One subject's exclusive, time-boxed hold on one pool member. |
| `ProviderClass` | `banlieue.io/v1alpha1` | Cluster-scoped install metadata for a backend class (image, resources, RBAC). |
| `CloudHypervisorMachine` / `CloudHypervisorMachineTemplate` | `infrastructure.banlieue.io/v1alpha1` | Concrete, CAPI-contract infra CRs the Cloud Hypervisor provider reconciles. |
| `VSphereMachine` / `VSphereMachineTemplate` / `VSphereCluster` | `infrastructure.banlieue.io/v1alpha1` | The same for vSphere, plus its InfraCluster. |
| `LibvirtMachine` / `LibvirtMachineTemplate` | `infrastructure.banlieue.io/v1alpha1` | The same for libvirt. |
| `ProxmoxMachine` / `ProxmoxMachineTemplate` | `infrastructure.banlieue.io/v1alpha1` | The same for Proxmox VE. |

Full schema reference: **[API Reference](https://firestoned.github.io/banlieue/reference/api/)** ·
Design rationale: **[Concepts](https://firestoned.github.io/banlieue/concepts/)** &
**[Why banlieue?](https://firestoned.github.io/banlieue/reasoning/)**

## Repository layout

```
banlieue/
├── crates/
│   ├── banlieue/                    # single binary: dispatches controller/operator/provider/bootstrap/imagebuilder subcommands
│   ├── banlieue-api/                # authoritative CRD type system (code-first); generates deploy/crds/
│   ├── banlieue-cloud-hypervisor/   # first-party client for the Cloud Hypervisor VMM's REST API
│   ├── banlieue-controller/         # main controller lib: schedules VirtualMachine CRs + mirrors status
│   ├── banlieue-host/               # `banlieue host cloud-hypervisor`: prepares a KVM host (install, status, overhead)
│   ├── banlieue-imagebuilder/       # provider-agnostic VMImage build pipeline (OCI/Kairos → raw disk)
│   ├── banlieue-libvirt/            # first-party pure-Rust libvirt RPC client (no native libvirt lib)
│   ├── banlieue-oci/                # OCI registry client: VM disk images pushed and pulled by digest
│   ├── banlieue-operator/           # provider lifecycle operator: Provider CRs → workloads; `banlieue bootstrap`
│   ├── banlieue-provider-cloud-hypervisor/  # Cloud Hypervisor provider, host-resident (the reference backend)
│   ├── banlieue-provider-libvirt/   # libvirt/KVM provider: host capabilities, image import, VM lifecycle
│   ├── banlieue-provider-proxmox/   # Proxmox VE provider: template clones, NoCloud seed ISO
│   ├── banlieue-provider-sdk/       # shared provider/controller building blocks
│   ├── banlieue-provider-vsphere/   # vSphere provider: capability checks, image build, VM lifecycle
│   ├── banlieue-proxmox/            # first-party Proxmox VE REST client
│   └── banlieue-vex/                # auto-VEX tooling for the release/supply-chain pipeline
├── deploy/                      # CRDs (generated) + kustomize manifests
├── docs/                        # MkDocs site + CALM architecture model + ADRs (docs/adr/)
└── examples/                    # sample CRs
```

`crates/banlieue-api` is the **source of truth**: CRD YAML in `deploy/crds/` and
the API reference are generated from the Rust types (`make crds`). Never
hand-edit generated YAML.

## Getting started

There's no banlieue-managed cluster to bootstrap *into* on day one, so
`scripts/bootstrap-k0s-cluster.sh` stands up the k0s **management** cluster
banlieue's own controllers will run on — the one piece of the pipeline that
talks to the backend directly (`govc`/`virt-install`) instead of through a
banlieue CRD, since no CRD API exists yet at that point.

```sh
# See every subcommand + env var this script accepts
./scripts/bootstrap-k0s-cluster.sh --help

# Scaffold the untracked vSphere env file (BACKEND defaults to libvirt
# otherwise — set it explicitly even for this template-only step)
BACKEND=vsphere ./scripts/bootstrap-k0s-cluster.sh --print-env-template > ~/.k0s/banlieue.env
```

<details>
<summary><b>vSphere backend</b></summary>

Clones cluster-specific Kairos VM templates with `govc`, spreading nodes
across compute clusters so each is its own etcd failure domain. vCenter
creds/URL/datacenter come from the ambient `GOVC_*` environment; the node
table and per-cluster placement come from an **untracked** env file (never
commit real hostnames/IPs — see
[`rules/no-real-infrastructure.md`](.claude/rules/no-real-infrastructure.md)):

```sh
# 1. edit ~/.k0s/banlieue.env: node table (name/cluster/ip/role) + placement maps
$EDITOR ~/.k0s/banlieue.env

# 2. run the full pipeline (clone -> reconfigure -> power on -> k0sctl)
BANLIEUE_ENV_FILE=~/.k0s/banlieue.env BACKEND=vsphere \
  ./scripts/bootstrap-k0s-cluster.sh all

# or drive it phase by phase: vms | config | apply | kubeconfig | label | flux | destroy
```

</details>

<details>
<summary><b>libvirt backend (default)</b></summary>

Creates Kairos "Hadron" VMs on a KVM host and lets Kairos install itself
from its ISO. Run on the hypervisor host. Each node is a Cloud Hypervisor VM
run by systemd units by default (ADR-0099), which needs a `CLOUDHV.fd` and
passwordless `sudo`:

```sh
CH_FIRMWARE=/opt/banlieue/firmware/<version>/CLOUDHV.fd \
VM_COUNT=4 VCPUS=2 MEM_MB=8192 DISK_GB=25 \
  ./scripts/bootstrap-k0s-cluster.sh all
```

`DEFAULT_VMM=libvirt` creates libvirt domains with `virt-install` instead,
and `LIBVIRT_URI` may then point at a remote host.

</details>

Either backend finishes with [k0sctl](https://github.com/k0sproject/k0sctl)
installing k0s onto the resulting VMs; an opt-in `flux` step
(`FLUX_ENABLED=true`) can push `flux-operator`/`flux-core` next.

**Output:** a running k0s management cluster with nothing banlieue-specific
on it yet — install banlieue itself with `banlieue bootstrap operator`
(ADR-0013). Full walkthrough, phase by phase:
[End-to-End Setup](docs/src/guides/end-to-end-setup.md)
(rendered: [docs](https://firestoned.github.io/banlieue/guides/end-to-end-setup/)).

## Development

banlieue follows **ADD — Architecture Driven Development**: significant changes
start with an [ADR](docs/adr/) and a [CALM](https://github.com/finos/architecture-as-code)
architecture update, *then* test-driven implementation (`ADR → CALM → TDD`).
See [`.claude/rules/architecture-driven-development.md`](.claude/rules/architecture-driven-development.md).

```sh
# Generate CRDs + API reference from the Rust types
make crds

# Quality gate (run after any Rust change)
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all

# Bring up a local kind cluster with the CRDs applied
make kind-up

# Build the documentation site (also validates the CALM model)
make docs
```

## Project status

banlieue is **early, and runs real VMs on all four backends**:

| Provider | State |
| --- | --- |
| Cloud Hypervisor | Complete: host install, VM lifecycle, vTPM, OCI images, pools, capacity-aware placement |
| vSphere | Complete: validated end to end against real vCenter and ESXi |
| libvirt | Complete: VM lifecycle, NoCloud cloud-init, vTPM, pools |
| Proxmox VE | Implemented end to end and tested against a real node; the cluster end-to-end test is the last item |

The controller, `banlieue-operator`, `banlieue-imagebuilder`, warm pools and
claims are in place. Live migration and snapshots are not started (see
[`ROADMAPS.md`](ROADMAPS.md)). The CRD surface is `v1alpha1` and **will break
before `v1`**: don't run production workloads against it yet.

## License

Apache License 2.0 — see [LICENSE](LICENSE).
