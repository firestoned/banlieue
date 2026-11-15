<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0129: Cluster API InfraMachines receive the Machine's bootstrap data

- **Status:** Proposed
- **Date:** 2026-11-14
- **Deciders:** Erick Bourgeois
- **Related:** Completes the InfraMachine contract that
  [ADR-0096](0096-clusterctl-infrastructure-provider.md) packaged (its
  Decision 6, "bootstrap and control-plane providers come from upstream",
  stands); follows [ADR-0025](0025-vspheremachine-userdata-secret-rbac.md) and
  [ADR-0038](0038-userdata-configmap-support.md) (the in-cluster controller
  resolves user-data, host providers read no Secret) and
  [ADR-0042](0042-userdata-reference-authorization.md) (a reference is
  authorized at admission against the principal who makes it); applies to
  the four InfraMachine kinds of
  [ADR-0062](0062-cloudhypervisormachine-inframachine-contract.md) and its
  siblings. Tracks firestoned/banlieue#191.

## Context

A Cluster API `Machine` names its bootstrap data in
`spec.bootstrap.dataSecretName`: a Secret, in the Machine's namespace, that a
bootstrap provider (kubeadm's CABPK, k0smotron, ...) renders. The CAPI
v1beta2 InfraMachine contract leaves getting that data into the guest to the
infrastructure provider. banlieue implements the status side of the contract
(`initialization.provisioned`, `providerID`, `addresses`, deletion) and not
this. The result: a Machine created by CAPI, for example from a
`MachineDeployment` over a `CloudHypervisorMachineTemplate`, boots with no
bootstrap data and never joins its workload cluster.

What the tree has today:

- **Nothing reads a CAPI `Machine` or its bootstrap Secret.** In Rust
  outside `banlieue-api`, `cluster.x-k8s.io` appears only in the operator's
  aggregate-role test and in `e2e_capi_contract.rs`, which is also the only
  place in the tree `dataSecretName` appears.
- **`spec.userData` on all four InfraMachine kinds is a pre-rendered
  string**, filled only on the `VirtualMachine` path
  (`virtualmachine.rs` `resolve_rendered_user_data` → `infra.rs`
  `build_*_machine`). Every provider delivers it as cloud-init: a NoCloud
  seed on Cloud Hypervisor, libvirt and Proxmox, `guestinfo.userdata` on
  vSphere. No provider can deliver Ignition or a PowerShell script.
- **An InfraMachine names no `VMImage`.** `CloudHypervisorMachine` names a
  file in the host's image cache (`spec.bootSource.image`); `guestAgent`
  lives only on `VMImage`. A CAPI-owned machine has no `VirtualMachine`, so
  there is no image whose `guestAgent` could be checked.
- **The test cannot see the gap.** `e2e_capi_contract.rs` sets a placeholder
  `dataSecretName`, never creates the Secret, and plays the provider by
  patching status itself.
- **The controller has no cluster-wide Secret access**, deliberately
  (`deploy/controller/rbac/clusterrole.yaml`, security review SEC-008). Its
  only Secret read is the namespaced `banlieue-controller-userdata` Role in
  the install namespace (ADR-0025), which grants `get`, not `list` or
  `watch`.
- **Cloud Hypervisor machines live in their Provider's namespace.** The host
  provider watches only that namespace and ignores machines elsewhere
  (`is_ours`), so a CAPI cluster on a Cloud Hypervisor host sits in the
  install namespace today.
- **Providers own the whole status they write.** All four providers build
  status from the machine's previous status (the Cloud Hypervisor
  provider's `build_status`, for example, starts from `previous.cloned()`)
  and server-side apply it with `force`, conditions included. A condition
  another manager wrote would be re-sent by the provider from a possibly
  stale snapshot and could be overwritten.

What Cluster API does, checked against its `release-1.14` source:

- **An InfraMachine exists before it belongs to a Machine.** A MachineSet
  and a KubeadmControlPlane both create the InfraMachine with
  `external.GenerateTemplate`, owned by the **MachineSet** (group
  `cluster.x-k8s.io`) or the **KubeadmControlPlane** (group
  `controlplane.cluster.x-k8s.io`), with the label
  `cluster.x-k8s.io/cluster-name` and the annotations
  `cluster.x-k8s.io/cloned-from-name` and `-groupkind`. The Machine
  controller adopts it later (`ensureExternalOwnershipAndWatch`): it removes
  those creation-time owners and sets the Machine as controller.
  `e2e_capi_contract.rs` waits for exactly this adoption.
- **A Machine cannot adopt another Machine's bootstrap config.** That same
  function sets the Machine as controller with
  `controllerutil.SetControllerReference`, which refuses an object already
  controlled by a different owner.
- **The Secret exists before its name is published.** CABPK
  (`storeBootstrapData`) and k0smotron (`worker_bootstrap_controller.go`)
  write the Secret, then set the config's `status.dataSecretName`, which
  CAPI copies to the Machine.
- **What the Secret looks like.** Both write type `cluster.x-k8s.io/secret`,
  label `cluster.x-k8s.io/cluster-name: <cluster>`, a **controller**
  ownerReference to the bootstrap config (its kind and name), and keys
  `value` and `format`. CABPK writes `cloud-config` or `ignition`; k0smotron
  also writes `powershell` and `powershell-xml`. A missing `format` means
  `cloud-config`.
- In v1beta2, `Machine.spec.bootstrap.configRef` is `{apiGroup, kind,
  name}` and `spec.clusterName` is required. A Machine, MachineSet or
  MachineDeployment may also name `dataSecretName` directly, with no
  bootstrap config.

## Decision

### 1. Which machines

Two pure functions in `banlieue-api`, used by the controller and every
provider, classify a machine:

- **CAPI-intended**: it carries the label `cluster.x-k8s.io/cluster-name`,
  or an ownerReference whose API group is `cluster.x-k8s.io` or ends in
  `.cluster.x-k8s.io` (a MachineSet, a Machine, a control plane). CAPI's
  `GenerateTemplate` sets that label on every InfraMachine it creates, so a
  machine is CAPI-intended **from the moment CAPI creates it**, before any
  Machine owns it.
- **CAPI-owned**: it has an ownerReference with group `cluster.x-k8s.io` and
  kind `Machine`, which exists only after adoption.

A machine that is not CAPI-intended (owned by a `VirtualMachine`, or created
standalone without those markers) behaves exactly as today. banlieue's own
controller sets neither marker on the machines it builds. An InfraMachine
written by hand for a hand-written Machine must carry the
`cluster.x-k8s.io/cluster-name` label to be held before adoption; the guide
says so, because nothing else distinguishes it from a standalone machine
until CAPI adopts it.

### 2. Who delivers: banlieue-controller, one implementation for four kinds

`banlieue-controller` gains a bootstrap-data reconciler, generic over
`CloudHypervisorMachine`, `LibvirtMachine`, `VSphereMachine` and
`ProxmoxMachine`. Host providers still read no Secret (ADR-0025's division
of labour, extended from `VirtualMachine` user-data to CAPI bootstrap data).

It watches the four kinds and CAPI `Machine`s, mapping a Machine to the
InfraMachine its `spec.infrastructureRef` names. Adoption and the moment
CAPI sets `dataSecretName` are both Machine or InfraMachine changes, so they
wake the right machine without delay. It starts only when the API server
serves `machines.cluster.x-k8s.io` at controller startup, and logs that it
is off otherwise. A cluster that installs Cluster API after banlieue
restarts the controller to turn it on.

### 3. Waking up when only the Secret changes

A Secret that is missing, unreadable or wrong can be fixed (created, its
Role granted, rewritten) without any change to a watched object. The
controller does not watch Secrets: that needs `list` and `watch` on every
Secret in each namespace, and `list` returns their contents, which is more
than the `get` by name it holds today. Instead, a machine whose condition is
`False` for a reason only a Secret or RBAC change can clear is **requeued on
a fixed interval**:

| Reason | Requeue | Why this interval |
|---|---|---|
| `WaitingForBootstrapData`, Secret not found | 30 s | Not expected in the normal flow (bootstrap providers write the Secret before publishing its name), so it is short-lived when it happens, and a join token's lifetime is the budget |
| `BootstrapSecretInvalid`, `UnsupportedBootstrapDataFormat`, `BootstrapSecretForbidden` | 5 min | Needs a person to rewrite the Secret or grant a Role |

The intervals are named constants. A machine waiting only for its Machine
(no adoption yet, no `dataSecretName` yet) is not requeued: those changes
arrive through the watches. An operator who has just fixed a Secret need not
wait: any change to the InfraMachine's metadata wakes it, for example
`kubectl annotate <kind>/<name> banlieue.io/reconcile-requested="$(date +%s)" --overwrite`.
The annotation's key and value mean nothing and nothing reads them; the
change itself is the trigger. The guide documents this.

### 4. What it copies, and what it refuses

For a CAPI-owned machine it reads the owning Machine. While
`spec.bootstrap.dataSecretName` is unset it waits. Once set, it reads that
Secret **in the Machine's namespace** and requires all of:

1. `type: cluster.x-k8s.io/secret`;
2. label `cluster.x-k8s.io/cluster-name` equal to the Machine's
   `spec.clusterName`;
3. when the Machine has `spec.bootstrap.configRef`: a controller
   ownerReference on the Secret whose group, kind and name equal the
   `configRef`'s. Both CABPK and k0smotron set exactly this. A Machine with
   no `configRef` (its `dataSecretName` set directly) skips this check;
4. a non-empty `value` that is valid UTF-8;
5. `format` absent or `cloud-config`. Any other value (`ignition`,
   `powershell`, ...) is refused, because no banlieue backend can deliver
   it. This replaces checking `format` against the image's `guestAgent`,
   which a CAPI-owned machine does not have.

A Secret that passes is copied **byte for byte**: no ADR-0024 placeholder
substitution, since the bootstrap provider has already rendered the data.
The controller server-side applies `spec.userData` alone, under its own
field manager, with `force`. That replaces any `userData` the
InfraMachineTemplate carried: on a CAPI-owned machine, the bootstrap data
*is* the user-data. A Secret that fails is reported, never copied.

These checks **classify** the Secret: they refuse anything that is not
CAPI bootstrap data for this Machine's cluster, such as a Provider's
credentials or a registry token. They do not authorize anyone, and on their
own they would not give ADR-0042's guarantee: a principal who sets
`dataSecretName` directly could still name **another Machine's** bootstrap
Secret in the namespace, which passes every check, and read its join
credential back out of the InfraMachine. Decision 5 closes that.

### 5. Authorizing `dataSecretName` at admission

As ADR-0042 does for `VirtualMachine.spec.userData`, the principal who
names a bootstrap Secret must itself be allowed to `get` it, checked at
admission, where its identity still exists. A new policy,
`deploy/admission/capi-bootstrap-data-authorization.yaml`
(`ValidatingAdmissionPolicy` with the CEL `authorizer`,
`failurePolicy: Fail`), matches `CREATE` and `UPDATE` of every object through
which a person can name one:

| Resource (`cluster.x-k8s.io`) | Field |
|---|---|
| `machines` | `spec.bootstrap.dataSecretName` |
| `machinesets`, `machinedeployments` | `spec.template.spec.bootstrap.dataSecretName` |

The check runs only when the field is set and, on `UPDATE`, only when it
changed, so an update that leaves it alone (a controller adding a finalizer,
CAPI patching status) is never checked against an identity that did not
make the reference. MachineSets and MachineDeployments are covered because
CAPI's own manager copies their template into each Machine it creates; a
check on Machines alone would see only that manager. When CAPI copies a
bootstrap config's `status.dataSecretName` onto a Machine, the requester is
CAPI's manager, which can read Secrets and passes. MachinePools are not
covered: their machines use an InfraMachinePool, which banlieue does not
implement.

The `configRef` path needs no policy of banlieue's: CAPI makes a Machine the
controller of its bootstrap config and refuses one another Machine already
controls (Context), and check 3 ties the Secret to that config. A principal
who can create bootstrap configs and Machines can have CAPI render a join
credential for itself; that is Cluster API's trust model, not an escalation
through banlieue.

Shipped as its own file for the reason ADR-0042 gives: `authorizer` needs a
sufficiently recent API server, and an older one must reject only this file.
It matches nothing on a cluster without Cluster API.

### 6. `BootstrapDataReady`: written by the controller, honoured by providers

The controller reports on a condition of the InfraMachine,
`BootstrapDataReady`, which it alone writes. It applies only that entry of
`status.conditions` under its own field manager:

| Status | Reason | When |
|---|---|---|
| `False` | `WaitingForBootstrapData` | not yet owned by a Machine, the Machine has no `dataSecretName` yet, or the Secret does not exist yet; the message says which |
| `False` | `BootstrapSecretInvalid` | checks 1 to 4 failed; the message names which |
| `False` | `UnsupportedBootstrapDataFormat` | check 5 failed; the message names the format |
| `False` | `BootstrapSecretForbidden` | the controller may not read Secrets in this namespace (Decision 9) |
| `True` | `BootstrapDataDelivered` | `spec.userData` holds the Secret's `value` |

Host providers change in two ways, both for CAPI-intended machines only:

1. **Hold creation.** A provider creates nothing for a CAPI-intended machine
   until it is **CAPI-owned and** `BootstrapDataReady` is `True`. Before
   adoption the machine is held because it is CAPI-intended; an absent
   condition counts as not ready. There is therefore no window, from the
   moment CAPI creates the machine, in which a provider can start the guest
   without its data. While holding, the provider reports its own
   `Ready=False`, `WaitingForBootstrapData`. The hold applies only before
   the provider has created anything for the machine: a guest that already
   exists (created before an upgrade) is never held.
2. **Never write the controller's condition.** A provider drops
   `BootstrapDataReady` from the conditions it applies, so its
   whole-status apply never takes ownership of that entry or overwrites it
   from a stale snapshot.

`status.initialization.provisioned` keeps meaning "the guest exists".

### 7. One-shot

CAPI treats bootstrap data as one-shot, and so does banlieue. Once
`BootstrapDataReady` is `True`, the controller never rewrites
`spec.userData`: a later edit of the Secret, or a later change of
`dataSecretName`, is ignored. The controller also never delivers to a
machine whose `spec.providerID` is already set, because the provider has
passed the hold. A replacement Machine gets a new InfraMachine and new data.

### 8. Expired join tokens are not retried

Bootstrap data usually carries a short-lived join token. A machine whose
guest starts too late to use it fails to join. banlieue does not re-fetch or
re-deliver: replacing a Machine that never becomes a Node is the job of the
CAPI `MachineHealthCheck`. The guide says so.

### 9. RBAC

- **Machines:** `banlieue-controller`'s ClusterRole gains `get`, `list` and
  `watch` on `machines.cluster.x-k8s.io`. A Machine carries the *name* of
  its bootstrap Secret, never its content.
- **Secrets:** `get` only, through a **namespaced** Role in each namespace
  that holds CAPI-owned InfraMachines, never a ClusterRole rule:
  - The install namespace needs nothing new: `banlieue-controller-userdata`
    already grants `get` on Secrets there. Cloud Hypervisor machines must
    live there (Context), so the Cloud Hypervisor case works on a default
    install.
  - For any other namespace, the administrator creates a Role granting
    `get` on `secrets` and binds it to the controller's ServiceAccount. The
    guide ships the manifest. Without it, the machine reports
    `BootstrapSecretForbidden` and is never created.
  - A cluster-wide Secret rule is rejected: it would reverse SEC-008 for
    every namespace to serve the ones that hold CAPI clusters.
- CAPI's aggregate role (`banlieue-capi-infrastructure`, ADR-0096) does not
  change.

### 10. Milestones

1. **The controller (Decisions 1 to 9), the admission policy, and the Cloud
   Hypervisor provider's hold.** Cloud Hypervisor first, because it is the
   backend this was found on and the one the live test runs against.
2. **The libvirt, vSphere and Proxmox providers' hold**, the same two
   changes each. Until each lands, the controller still delivers
   `spec.userData` to a CAPI-owned machine of that kind, but the provider
   may create the guest first. The guide states which backends hold.

### 11. Tests

- **Unit**, in sibling `*_tests.rs` files:
  - classification: CAPI-intended by label alone, by a MachineSet owner, by
    a KubeadmControlPlane owner (`controlplane.cluster.x-k8s.io`), by a
    Machine owner; not CAPI-intended when owned by a `VirtualMachine` or
    carrying no marker;
  - **ordering**: a machine as CAPI creates it (MachineSet or control-plane
    owner and the label, no Machine owner) is held by the Cloud Hypervisor
    provider (through `fake.rs`), which creates nothing on the host and
    allocates no guest uid; adopted but not delivered, still held; adopted
    and `BootstrapDataReady=True`, created. A standalone machine with no
    marker is created at once, as today;
  - every check and reason in Decisions 4 and 6, and one-shot behaviour;
  - **a Secret created after a NotFound**: the first pass reports
    `WaitingForBootstrapData` and asks for the 30 s requeue; with the Secret
    present, the next pass delivers. The same for a Secret whose `format`
    is corrected after `UnsupportedBootstrapDataFormat`;
  - the admission policy, evaluated in a test: a principal without `get` on
    the named Secret is refused on each matched resource; an `UPDATE` that
    does not change the field is admitted for anyone.
- **kind e2e with nothing faked on the bootstrap side**: a new suite runs
  banlieue's controller from an image loaded into kind, and a real
  bootstrap provider renders the data (a `KubeadmControlPlane` with one
  replica over a `CloudHypervisorMachineTemplate`, whose first Machine gets
  init data with no running control plane). It asserts that CAPI sets
  `dataSecretName`, that `spec.userData` equals the Secret's `value` byte for
  byte, that `BootstrapDataReady` is `True`, and, with no host provider
  running, that nothing reports the machine provisioned. Negative cases: a
  `dataSecretName` that names a Secret without the CAPI type or label gets
  `BootstrapSecretInvalid` and no `userData`; a principal without `get` on a
  Secret cannot set `dataSecretName` to it.
- `e2e_capi_contract.rs` stops carrying a placeholder `dataSecretName` that
  pretends to be bootstrap data.
- A **live test** against a real Cloud Hypervisor host (a k0smotron worker
  joining a hosted control plane), kept outside the repository.

## Alternatives considered

- **Host providers read the bootstrap Secret.** Rejected: each would need
  Secret access in every CAPI namespace, and host-resident providers hold
  per-Provider credentials scoped to their own objects (ADR-0025, CHAIN-002).
- **A cluster-wide Secret rule for the controller.** Rejected: reverses
  SEC-008 everywhere (Decision 9).
- **Restrict CAPI-owned machines to the install namespace.** That is the
  default outcome of Decision 9, not a rule: Cloud Hypervisor already
  requires it, and other backends can be used elsewhere once an
  administrator grants the Role.
- **Hold only CAPI-owned machines.** Rejected: CAPI creates the InfraMachine
  before a Machine owns it, so a provider could start the guest in between,
  and the "already exists" exception would then keep the premature boot.
- **A `False` condition written at creation instead of "absent counts as
  not ready".** Nothing banlieue runs sees the machine before CAPI creates
  it, so there is always a window. Treating absence as not ready closes it
  without a write.
- **The gate on `spec.userData` being set, with no condition.** Rejected: an
  InfraMachineTemplate may carry `userData`, which would let a provider
  start the guest with it before the bootstrap data arrived.
- **Watching Secrets.** Rejected for now: `list` and `watch` on Secrets
  return every Secret's contents in the namespace, more than `get` by name.
  A metadata-only watch would still need those verbs. Revisit if a
  bounded alternative appears (Decision 3).
- **Requeueing every waiting machine on a timer.** Rejected: Machine
  adoption and `dataSecretName` arrive through watches; only the
  Secret-dependent reasons are requeued.
- **Treating the Secret checks as the authorization.** Rejected: they
  classify the Secret but cannot tell whose it is (Decision 4); the
  admission policy does that.

## Consequences

- A `MachineDeployment` or control plane over any banlieue
  InfraMachineTemplate gets its bootstrap data, and on Cloud Hypervisor the
  guest does not start without it.
- **Every CAPI-owned InfraMachine becomes credential-bearing.** Its
  `spec.userData` holds a join credential (a kubeadm or k0s join token),
  readable by anyone who can `get` that kind in its namespace. ADR-0025
  accepted this for `VSphereMachine` user-data; this extends it to all four
  kinds. The threat model's hardening section says who must be kept from
  reading them.
- A principal can have the controller copy only a bootstrap Secret it could
  already read (Decision 5) or one CAPI rendered for its own Machine.
  Automation that sets `dataSecretName` directly needs `get` on that Secret.
- The controller can read Secrets in each namespace an administrator
  grants, in addition to the install namespace. Its blast radius in such a
  namespace is every Secret there, bounded in what it will *copy* by
  Decision 4.
- A machine whose Secret is fixed by hand is picked up within the requeue
  interval, or at once after an annotation; the controller makes one `get`
  per waiting machine per interval.
- An InfraMachine with the `cluster.x-k8s.io/cluster-name` label is held
  even when no Machine ever adopts it. A standalone machine must not carry
  that label.
- `userData` from an InfraMachineTemplate is replaced on CAPI-owned
  machines. A setup that put cloud-config in the template and relied on it
  booting instead of the bootstrap data changes behaviour.
- A provider build without Decision 6 beside a controller with it never
  holds, and may take ownership of `BootstrapDataReady` through its
  whole-status apply. Upgrade a Cloud Hypervisor host's `banlieue` binary
  with the controller.
- Installing Cluster API after banlieue needs a controller restart to turn
  the reconciler on.

## Open questions, settled by the e2e before this is Accepted

- Whether CAPI v1.14 marks a `Cluster` with no `infrastructureRef` as
  infrastructure-provisioned, or whether the e2e needs an InfraCluster for a
  non-vSphere backend.
- Whether a MachineSet in CAPI v1.14 re-applies the template's spec to an
  existing InfraMachine (in-place propagation), which would contend with
  the controller's ownership of `spec.userData`.
