<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Upgrading from 0.5.x to 0.6.0

This guide takes a running banlieue from **v0.5.9** or **v0.5.10** to
**v0.6.0**. It covers the cluster (CRDs, RBAC, admission policies, the
controller, operator and image builder, and the in-cluster providers), the
Cloud Hypervisor hosts, whose provider runs on the host and is upgraded by the
host installer rather than by `kubectl`, and Rust code that depends on the
`banlieue-api` crate.

Everything here comes from the difference between the two tags. Where a change
already shipped in v0.5.10, it says so: a v0.5.10 cluster has done that part
already.

## At a glance

- **No stored object needs editing.** The API groups stay at `v1alpha1`, with
  no new served or storage version. No CRD, field or enum value was removed or
  renamed, and every new field is optional. One field became optional that was
  required (`Provider.spec.attestation.ekTrustBundle`).
- **CRDs go first.** The v0.6.0 controller writes new status fields
  (`status.attestationSubject`); against v0.5.x CRDs those writes succeed and
  the field is silently dropped.
- **Two new CRDs**, `FirecrackerMachine` and `FirecrackerMachineTemplate`.
  Nothing serves them yet (see [Not usable yet](#in-the-tree-but-not-usable-yet)).
- **Cloud Hypervisor hosts**: re-running `install` now follows the host config
  it keeps, and refuses a flag that contradicts it. Read
  [the host section](#cloud-hypervisor-hosts-the-installer-follows-its-kept-config)
  before you upgrade a host.
- **Rust**: `banlieue-api` 0.6.0 needs Rust **1.95** and kube **5.2**, and
  changes the type of `providerRef` on every machine kind.

## Breaking changes and required actions

For a cluster, no change requires you to edit an existing object or rewrite a
manifest of your own. What can break is the upgrade path itself (on Cloud
Hypervisor hosts) and Rust code built against `banlieue-api`.

### Cloud Hypervisor hosts: the installer follows its kept config

**What changed.** `banlieue host cloud-hypervisor install` keeps
`/etc/banlieue/cloud-hypervisor.toml` on a host it has already prepared (only
`--force` regenerates it). In 0.5.x a re-run still rendered the later stages
from fresh defaults, so a re-run without every original flag could produce
units that disagreed with that file. It now takes every setting the file
records (provider name and namespace, storage and network classes, the guest
uid range, the registry) from the file when the flag is not given, and
**refuses** a flag that says something else, naming the flag, the file and
`--force`.
([ADR-0067](https://github.com/firestoned/banlieue/blob/main/docs/adr/0067-banlieue-host-install-subcommand.md)
Decision 5, PR #205.)

**Who is affected.** Every Cloud Hypervisor host, when you upgrade its
provider. A re-run that passes a storage class, network class or uid range
different from what the host was installed with now fails instead of
silently rendering mismatched units.

**What to do.** Leave out the flags the host config records, or pass exactly
the values it holds. The guests' slice budget (`--guest-cpus`,
`--guest-cpu-weight`, `--guest-cpu-quota`, `--guest-memory-max`,
`--guest-tasks-max`) is **not** in that file: pass those again on every run,
or the slice loses its limits. Do not reach for `--force` to get past a
refusal: it regenerates the host config **and the EK CA**.

**How to verify.** Run the upgrade with `--dry-run` first (see
[step 5](#5-cloud-hypervisor-hosts)); a contradiction is reported there and
nothing is written.

### Cloud Hypervisor hosts on systemd older than 248

**What changed.** The installer now checks the unit directives the host's
systemd can enforce
([ADR-0139](https://github.com/firestoned/banlieue/blob/main/docs/adr/0139-hosts-without-systemd-userdb-or-cgroup-v2.md),
PR #209). The guest units use `PrivateIPC=` (since v0.5.9), which needs
systemd 248. On an older systemd, `install` and `preflight` refuse unless
`--accept-reduced-hardening` names exactly the directives the host lacks. In
0.5.x the directive was written anyway, and such a systemd logged an
`Unknown lvalue` warning and ran the guest without it.

**Who is affected.** Hosts whose systemd is 247 or older. Check with
`systemctl --version`.

**What to do.** Accept the gap explicitly, once; it is recorded in the host
config (`[hardening] accepted_reduced`), so later re-runs do not ask again:

```sh
sudo banlieue host cloud-hypervisor install --only provider \
  --provider-binary ./banlieue --accept-reduced-hardening=PrivateIPC --dry-run
```

What that gives up is described in
[Host internals](cloud-hypervisor-host-systemd.md#directives-an-old-systemd-cannot-enforce).

### `--guest-cpus` is refused where it would not be enforced

**What changed.** `--guest-cpus` (`AllowedCPUs=` on the guests' slice) is now
refused on a cgroup v1 host and on systemd before 244, which accept the
directive and never enforce it (ADR-0139, PR #209).

**Who is affected.** A host on cgroup v1 or an old systemd that was installed
with `--guest-cpus`. On that host the limit never applied.

**What to do.** Use the new `--guest-cpu-quota` instead, a share of CPU time
that works on cgroup v1 and v2 (for example `--guest-cpu-quota 4800%` for 48
CPUs' worth). See
[VM resource limits](cloud-hypervisor-host.md#vm-resource-limits-banlieueslice).

### Rust: the `banlieue-api` crate

Only for code that depends on `banlieue-api`; see
[For developers](#for-developers).

## Behaviour changes that need no action

- **The controller can now restart itself.** The `VirtualMachine` controller
  audits its own progress: an object it has never been invoked for, for five
  minutes in a row, turns `/livez` into `503 stale` and the kubelet restarts
  the pod. A reconcile that keeps failing does not trip it.
  ([ADR-0145](https://github.com/firestoned/banlieue/blob/main/docs/adr/0145-controllers-audit-their-own-progress-and-fail-liveness-when-objects-go-unreconciled.md),
  PR #215.) Alert on `banlieue_audit_unreconciled_objects` above `0`; see
  [Observability](observability.md#when-livez-says-stale-adr-0145).
- **Claims avoid unhealthy hosts** (already in v0.5.10). Every claim, including
  one with no `spec.selection`, now takes a member on a cordoned or `Stale`
  host last instead of in turn
  ([ADR-0135](https://github.com/firestoned/banlieue/blob/main/docs/adr/0135-claim-selection-strategies.md),
  PR #204).
- **A placed VM keeps its host.** A tenant VM whose infra machine already
  exists is never rescheduled onto another provider, and a Cloud Hypervisor
  host never hands out a guest uid that still has a run directory on it
  (PR #234). This needs the new controller **and** the new provider on every
  Cloud Hypervisor host.
- **A Cloud Hypervisor guest whose vTPM state is gone says so, and stops
  retrying.** A machine with a manufactured vTPM whose state directory was
  deleted on the host (as the uid reuse fixed by PR #234 could do) reports
  `Ready=False` with reason `TpmStateLost`, `tpmAttached: false` and the
  power state the host shows, instead of `HostError` every 5 seconds. Its TPM
  identity cannot be recovered and banlieue never manufactures a new one in
  its place: delete and recreate the `VirtualMachine`. Deleting it works as
  usual
  ([ADR-0065](https://github.com/firestoned/banlieue/blob/main/docs/adr/0065-cloud-hypervisor-vtpm-and-deferred-install.md),
  amendment 2026-11-25).
- `status.scheduledAt` on a `VirtualMachine` is kept while its placement is
  unchanged (PR #215).
- **The `credentialsRef` admission policy is narrower and accepts a new verb**,
  once you re-apply it: it checks only a create, or an update that changes
  `spec.connection`, and admits `get` **or** the custom verb `reference` on the
  named Secret
  ([ADR-0157](https://github.com/firestoned/banlieue/blob/main/docs/adr/0157-referencing-provider-credentials-is-its-own-permission.md),
  PR #231). Nobody holds `reference` by default.

## Order of operations

Pin a checkout to the release, as every guide does
([Conventions](index.md#conventions-used-in-these-guides)):

```sh
export BANLIEUE_VERSION=v0.6.0
git clone --branch "$BANLIEUE_VERSION" --depth 1 https://github.com/firestoned/banlieue
cd banlieue
DIGEST=$(crane digest ghcr.io/firestoned/banlieue:"$BANLIEUE_VERSION")
```

Then, in this order: CRDs, RBAC, admission policies, the platform
Deployments, the in-cluster providers, the Cloud Hypervisor hosts.

### 1. CRDs and RBAC

**If you installed with `banlieue bootstrap operator`**, run the v0.6.0 binary
the same way. It server-side applies, in dependency order, the CRDs (built
from its own types), the ClusterRoles and bindings, the controller, operator
and image builder Deployments, and the seeded `ProviderClass`es. Pass the
flags you installed with (`--namespace`, `--registry`,
`--watch-namespace-selector`, `--build-node-selector`, `--build-toleration`),
and review the change first:

```sh
banlieue bootstrap operator --version "$BANLIEUE_VERSION" --image-digest "$DIGEST" --dry-run \
  | kubectl diff --server-side -f -
banlieue bootstrap operator --version "$BANLIEUE_VERSION" --image-digest "$DIGEST"
```

The seeded `ProviderClass`es are force-applied, which is what moves every
operator-managed provider to the new image. If you manage your classes
yourself, add `--skip-provider-classes` and update them as in
[step 4](#4-in-cluster-providers). That one command covers steps 1, 3 and 4;
the admission policies (step 2) and the hosts (step 5) remain.

**From the release assets**, the same objects in two files:

```sh
URL=https://github.com/firestoned/banlieue/releases/download/"$BANLIEUE_VERSION"
kubectl apply --server-side -f "$URL"/crds.yaml
kubectl wait --for=condition=Established --timeout=60s -f "$URL"/crds.yaml
kubectl apply --server-side -f "$URL"/install.yaml
```

**From the manifests**, apply the CRDs, then the RBAC you installed:

```sh
kubectl apply --server-side -f deploy/crds/
kubectl wait --for=condition=Established --timeout=60s -f deploy/crds/
kubectl apply -R -f deploy/controller/rbac/
kubectl apply -R -f deploy/operator/rbac/
```

The RBAC changes in this range:

| File | Change | Needed for |
| --- | --- | --- |
| `deploy/provider-vsphere/rbac/clusterrole.yaml`, `deploy/provider-libvirt/rbac/clusterrole.yaml`, `deploy/provider-proxmox/rbac/clusterrole.yaml` | `list`, `watch` on `namespaces` (no write verb, no Secret rule) | a shared provider ([ADR-0133](https://github.com/firestoned/banlieue/blob/main/docs/adr/0133-shared-provider-serves-tenant-namespaces.md)); already in v0.5.10 |
| `deploy/provider-cloud-hypervisor/rbac/clusterrole-shared.yaml` (new) | `banlieue-provider-cloud-hypervisor-shared`: machines and read-only namespaces; the operator binds it only to a Cloud Hypervisor `Provider` with a `namespaceSelector` | a shared Cloud Hypervisor host ([ADR-0134](https://github.com/firestoned/banlieue/blob/main/docs/adr/0134-cloud-hypervisor-serves-tenant-namespaces.md)); already in v0.5.10 |
| `deploy/capi/clusterrole-aggregate.yaml` | `firecrackermachines`, its `/status`, `firecrackermachinetemplates` | Cluster API users |

Apply each one you use, for example
`kubectl apply -f deploy/capi/clusterrole-aggregate.yaml`. The controller and
operator ClusterRoles, the namespaces and their Pod Security labels, and the
NetworkPolicies are unchanged.

**Verify:**

```sh
kubectl get crd firecrackermachines.infrastructure.banlieue.io
kubectl explain virtualmachineclaim.status.attestationSubject
kubectl explain provider.spec.attestation.hostCa
```

### 2. Admission policies

The policies are opt-in, so this step applies only if you installed them
([Core Controller, step 6](core-controller.md#6-recommended-admission-policies)).
Apply them **after** the CRDs, because they name the new fields. What changed:

| File | Change | Already in v0.5.10 |
| --- | --- | --- |
| `provider-attestation-ektrustbundle.yaml` | evaluated only when `ekTrustBundle` is present, so a `Provider` asserting only `hostCa` is no longer denied | no |
| `provider-attestation-hostca.yaml` (new) | `spec.attestation.hostCa` sets exactly one of `inline`, `configMapRef`, `secretRef` | no |
| `provider-credentialsref-authorization.yaml` | ADR-0157, above | no |
| `machine-provider-ref.yaml` (new) | only `banlieue-controller` sets `spec.providerRef.namespace` on a machine or machine template | yes |
| `provider-namespace-selector-authorization.yaml` (new) | setting `Provider.spec.namespaceSelector` needs `update` on `providers/namespaceselector`; ships the `banlieue-provider-namespaceselector-admin` ClusterRole | yes |
| `virtualmachineclaim-subject-authorization.yaml` | `spec.selection` is immutable | yes |

!!! warning "Keep your parameter ConfigMaps"
    `virtualmachineclaim-subject-authorization.yaml` and
    `vmimage-import-source.yaml` carry the parameter ConfigMaps you edited at
    install time (`banlieue-claim-subject-policy`,
    `banlieue-vmimage-allowed-registries`). Applying them as shipped puts the
    defaults back. Diff first, and carry your values over:

    ```sh
    kubectl diff -f deploy/admission/
    ```

`machine-provider-ref.yaml` exempts the controller by its username,
`system:serviceaccount:banlieue-system:banlieue-controller`. If banlieue runs in
another namespace, edit that `matchCondition` in both policies of the file
before applying it, or the controller is refused when it places a VM on a
shared provider. A GitOps controller that applies `Provider`s carrying a
`namespaceSelector` needs the `banlieue-provider-namespaceselector-admin`
ClusterRole bound to it.

```sh
kubectl apply -f deploy/admission/
```

or the `admission-policies.yaml` release asset. The file-by-file notes are in
`deploy/admission/README.md`.

### 3. Controller, operator and image builder

Skip this if `banlieue bootstrap operator` did it in step 1. Otherwise apply
the Deployments you use and set their image to the release:

```sh
IMAGE=ghcr.io/firestoned/banlieue:"$BANLIEUE_VERSION"@"$DIGEST"
kubectl -n banlieue-system set image deployment/banlieue-controller controller="$IMAGE"
kubectl -n banlieue-system set image deployment/banlieue-operator operator="$IMAGE"
kubectl -n banlieue-system set image deployment/banlieue-imagebuilder imagebuilder="$IMAGE"
kubectl -n banlieue-system rollout status deployment/banlieue-controller
```

No new flag is required. The controller's one new setting,
`BANLIEUE_PROVIDER_NAMESPACE`, is opt-in (see
[New features](#new-features-that-need-opting-in)).

**Verify:** the Deployments run the new image, and `/livez` answers `ok`:

```sh
kubectl -n banlieue-system get deploy -o wide
kubectl -n banlieue-system port-forward deploy/banlieue-controller 8081:8081 &
curl -s localhost:8081/livez
```

### 4. In-cluster providers

vSphere, libvirt and Proxmox providers run the image named by their
`ProviderClass`. Skip this if step 1 re-seeded the classes. Otherwise update
each class you use; the operator rolls every `Provider` of the class:

```sh
kubectl patch providerclass vsphere --type=merge \
  -p '{"spec":{"image":{"tag":"'"$BANLIEUE_VERSION"'","digest":"'"$DIGEST"'"}}}'
```

A class pinned by digest pulls the digest, not the tag, so set both. A
provider installed without the operator is upgraded with
`banlieue bootstrap provider <backend> --version "$BANLIEUE_VERSION"`. Both
paths: [Provider lifecycle](provider-lifecycle.md#upgrading).

**Verify:** `kubectl get providers.banlieue.io -A` and each Provider's
`status.workload.heartbeat` is `Alive`.

### 5. Cloud Hypervisor hosts

A Cloud Hypervisor provider runs on its host, so `kubectl` does not upgrade
it: re-run the host installer with the new binary, one host at a time. Guests
are their own systemd units and keep running; the new provider adopts them.
Read [the kept-config change](#cloud-hypervisor-hosts-the-installer-follows-its-kept-config)
first, then:

```sh
# Repeat the slice flags the host was installed with; leave out the ones the host config records.
sudo banlieue host cloud-hypervisor install --only provider \
  --provider-binary ./banlieue --guest-memory-max 20G --dry-run
sudo banlieue host cloud-hypervisor install --only provider \
  --provider-binary ./banlieue --guest-memory-max 20G
```

`--only provider` leaves the VMM alone; a full `install` also relinks the VMM
release it is given, which matters on a host running a VMM build of its own.
The full procedure, and choosing a VMM release, are in the
[Cloud Hypervisor guide](cloud-hypervisor-host.md#choosing-the-vmm-version-and-where-it-comes-from).

**Verify:** `systemctl status banlieue-provider-cloud-hypervisor` on the
host shows the provider active, and its `Provider` reports
`status.workload.heartbeat: Alive`.

### Rolling back

The CRD changes are additive, and the v0.5.x controller and providers ignore
fields they do not know, so the v0.6.0 CRDs do not stop you rolling the
Deployments and provider images back to a v0.5.x image. Keep the v0.6.0
CRDs when you do:

- a v0.5.x `providers` CRD requires `ekTrustBundle` again, so a `Provider`
  that asserts only `hostCa` would fail validation on its next write;
- rolling back to **v0.5.9** also gives up shared providers (ADR-0133 shipped
  in v0.5.10): its controller does not read `providerRef.namespace`.

On a Cloud Hypervisor host, rolling back is re-running the old installer with
the old binary. A v0.5.x provider reads its host config strictly and refuses a
section it does not know, so remove a `[hardening]` or `[attestation]` section
the v0.6.0 installer wrote before you start the old provider.

## New features that need opting in

| Feature | Opt in with | Read |
| --- | --- | --- |
| One provider serves tenant namespaces and keeps its credentials (vSphere, libvirt, Proxmox, Cloud Hypervisor). Already in v0.5.10 | `Provider.spec.namespaceSelector`; the controller's `BANLIEUE_PROVIDER_NAMESPACE`; the two admission policies above | [Multi-tenancy](multi-tenancy.md#one-shared-provider-for-every-team), [ADR-0133](https://github.com/firestoned/banlieue/blob/main/docs/adr/0133-shared-provider-serves-tenant-namespaces.md), `examples/28-provider-vsphere-shared.yaml` |
| A claim's selection strategy (`LongestReady`, the default, `MostAvailable`, `Spread`). Already in v0.5.10 | `VirtualMachineClaim.spec.selection.strategy` | [Claims](virtualmachine-claims.md#choosing-a-strategy-specselectionstrategy), `examples/29-virtualmachineclaim-spread.yaml` |
| Cloud Hypervisor guests attest with `x509pop` and a certificate their host signs, with an optional upstream PKI or OpenBao signing the host CA | `install --attestation` or `--attestation-ca-name`; then `Provider.spec.attestation.hostCa` | [Attesting with x509pop](cloud-hypervisor-host.md#attesting-with-x509pop-adr-0146), [ADR-0146](https://github.com/firestoned/banlieue/blob/main/docs/adr/0146-cloud-hypervisor-guests-may-attest-with-x509pop-and-a-certificate-their-host-signs.md) |
| Cloud Hypervisor hosts without systemd userdb or cgroup v2 (RHEL 8 class) | `--guest-accounts files`, `--guest-cpu-quota`, `--accept-reduced-hardening` | [Step 2: settings](cloud-hypervisor-host.md#step-2-settings), [ADR-0139](https://github.com/firestoned/banlieue/blob/main/docs/adr/0139-hosts-without-systemd-userdb-or-cgroup-v2.md) |
| Naming a Provider's credentials without reading them | grant the `reference` verb on that Secret, by exact `resourceNames` | `deploy/admission/README.md`, [ADR-0157](https://github.com/firestoned/banlieue/blob/main/docs/adr/0157-referencing-provider-credentials-is-its-own-permission.md) |

!!! warning "An opted-in host holds until the CA is asserted"
    A Cloud Hypervisor host installed with `--attestation` is `Ready=False`
    (`HostCaMissing`) and creates no guest until its `Provider` carries
    `spec.attestation.hostCa`. Do not opt a host in during the upgrade unless
    you are ready to assert its CA.

### In the tree but not usable yet

- **Firecracker.** The `FirecrackerMachine` and `FirecrackerMachineTemplate`
  CRDs, the `OciArtifact` image source kind and the attestation signer
  library ship, but there is no Firecracker provider, `ProviderClass` or host
  installer: ADR-0137 to ADR-0143 are Proposed and not implemented
  ([ADR-0137](https://github.com/firestoned/banlieue/blob/main/docs/adr/0137-firecracker-provider-class.md)).
  A `VMImage` source of kind `OciArtifact` is reported `UnsupportedSourceKind`
  by the vSphere and libvirt providers.
- **Azure.** `banlieue-azure` is an internal library (the ARM client, roadmap
  23); there is no Azure CRD or provider
  ([ADR-0164](https://github.com/firestoned/banlieue/blob/main/docs/adr/0164-azure-inframachine-and-a-first-party-arm-client.md)).
- **VFIO devices** for Cloud Hypervisor guests (ADR-0136) are Proposed.

## Deprecations and removals

None. No CRD, field, enum value, admission policy, CLI subcommand or flag was
removed or renamed between v0.5.9 and v0.6.0. On the host installer,
`--provider-namespace`, `--guest-uid-base` and `--guest-uid-count` lost their
built-in defaults only so that a given flag can be told from an omitted one;
when nothing is kept the defaults are the same (`banlieue-system`, `2000000`,
`1024`).

## For developers

**Toolchain.** The workspace MSRV moved from **1.89 to 1.95**, with kube
**5.2** (the RUSTSEC-2026-0600 fix; banlieue builds kube on rustls only and
was not affected itself). Building banlieue from source, or any crate that
depends on `banlieue-api` 0.6.0, needs Rust 1.95 or newer.

**The `banlieue-api` crate.** Pin it to the release whose CRDs your cluster
runs ([Using banlieue from Rust](../developer/rust-api-crate.md)):

```toml
[dependencies]
banlieue-api = "=0.6.0"
kube = { version = "5.2", features = ["client", "runtime"] }
k8s-openapi = { version = "0.29", features = ["latest"] }
```

`banlieue-api` requires `kube ~5.2`, so a lock or a manifest that holds kube
at 5.0 or 5.1 no longer resolves. k8s-openapi stays at 0.29. Source changes:

| Change | What to do |
| --- | --- |
| `spec.provider_ref` on `VSphereMachine`, `LibvirtMachine`, `ProxmoxMachine` and `CloudHypervisorMachine` (and their templates) is `common::ProviderReference { name, namespace }`, was `LocalObjectReference` | Build it with `ProviderReference { name, namespace: None }` or `LocalObjectReference::into()`; read the effective namespace with `namespace_or(referrer_namespace)`. The wire form is unchanged when `namespace` is absent |
| `ProviderAttestation::ek_trust_bundle` is `Option<CABundleSource>`; new `host_ca: Option<CABundleSource>` | Wrap in `Some(..)`; call `ProviderAttestation::validate()` for the at-least-one-anchor rule |
| New `VirtualMachineClaimSpec::selection: Option<ClaimSelection>` | Struct literals need `selection: None` (or `..Default::default()`); `spec.strategy()` returns the effective `SelectionStrategy` |
| New `attestation_subject: Option<String>` on `VirtualMachineStatus`, `VirtualMachineClaimStatus` and `CloudHypervisorMachineStatus` | Struct literals need the field |
| New variant `ImageSourceKind::OciArtifact` | Exhaustive `match`es need an arm |
| New kinds `FirecrackerMachine`, `FirecrackerMachineTemplate` in `infrastructure` | Nothing, unless you enumerate every machine kind |
