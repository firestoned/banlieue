# Phase 3 — Provider Lifecycle Automation

> **Goal.** Introduce the `ProviderClass` CRD and a lifecycle
> controller that creates, upgrades, and removes the per-provider
> Deployments, ServiceAccounts, RBAC, and Leases automatically.
>
> **Stop condition.** An admin can install banlieue, apply a
> `ProviderClass` CR, and the controller spins up the corresponding
> provider Deployment with correct RBAC and config. Updates to the
> ProviderClass image roll the Deployment. Removing the
> ProviderClass GC's everything.

## Preconditions

- Phases 1 and 2 working in their current "providers installed
  manually via Helm/kustomize" mode.
- Comfortable with the existing RBAC patterns from each provider.

## Add to banlieue-api

New CRD in `crates/banlieue-api/src/banlieue/provider_class.rs`:

```rust
pub struct ProviderClassSpec {
    /// What kind of provider this class describes.
    /// Conventional values: "vsphere", "proxmox", "libvirt".
    pub kind: String,

    /// Container image for the provider controller.
    pub image: String,

    /// Number of replicas. Default 2 (leader-elected, active/standby).
    #[serde(default = "default_replicas")]
    pub replicas: u32,

    /// Container resource requests/limits.
    #[serde(default)]
    pub resources: ResourceRequirements,

    /// Additional environment variables for the provider container.
    #[serde(default)]
    pub extra_env: Vec<EnvVar>,

    /// Additional volumes (e.g. for SSH known-hosts on libvirt).
    #[serde(default)]
    pub extra_volumes: Vec<Volume>,

    /// Additional volume mounts.
    #[serde(default)]
    pub extra_volume_mounts: Vec<VolumeMount>,

    /// Namespace to deploy the provider into. Default: same as the
    /// controller's namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_namespace: Option<String>,

    /// Service account name to create. Default: "banlieue-provider-<kind>".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_account_name: Option<String>,

    /// NodeSelector / tolerations / affinity for provider pods.
    #[serde(default)]
    pub pod_placement: PodPlacement,
}

pub struct ProviderClassStatus {
    /// Observed image actually running.
    pub deployed_image: Option<String>,
    /// Number of Providers referencing this class.
    pub provider_count: u32,
    /// Conditions: Ready, DeploymentAvailable.
    pub conditions: Vec<Condition>,
    pub observed_generation: Option<i64>,
}
```

Scope: **cluster-scoped**. Printer columns: `Kind`, `Image`,
`Replicas`, `Providers`, `Ready`, `Age`.

`Provider.spec.providerClassRef.name` (already present) now actually
resolves against this CRD. The main controller validates the
reference at admission.

## Lifecycle controller

Lives in the main `banlieue-controller`:

```
crates/banlieue-controller/src/provider_lifecycle/
├── mod.rs
├── reconciler.rs
├── manifest.rs         // builds Deployment, SA, ClusterRole, etc.
└── rbac_templates.rs   // per-kind RBAC templates
```

### Reconcile flow

```
1. Watch ProviderClass.
2. For each PC:
   - Determine install namespace (PC.spec.installNamespace or own ns).
   - Ensure ServiceAccount exists.
   - Ensure ClusterRole exists (templates per kind in rbac_templates.rs).
   - Ensure ClusterRoleBinding binding the SA to the ClusterRole.
   - Ensure a Role + RoleBinding for namespace-local resources
     (Secrets, Events) in every namespace where a referencing Provider
     lives. (Achieve via RoleBinding-per-Provider, owner-referenced.)
   - Ensure Deployment with the requested image, replicas, env, etc.
   - Ensure a Lease for leader election.
   - Patch ProviderClass.status.deployedImage on rollout completion.
3. On deletion: rely on owner refs to GC SA, Deployment, ClusterRole,
   ClusterRoleBinding. Use finalizer to also remove any cross-namespace
   RoleBindings that weren't owner-ref'd.
```

### Per-kind RBAC templates

Keep these as Rust string templates or as YAML files embedded with
`include_str!`.

vSphere ClusterRole template covers:
- `get, list, watch, patch` on `infrastructure.banlieue.io/vspheremachines`,
  `/vspheremachinetemplates`, `/vspheremachinesnapshots`.
- `patch` on `banlieue.io/providers/status`,
  `banlieue.io/vmimages/status`.
- `get, list, watch` on `banlieue.io/providers` (the ones it'll
  reconcile).
- `create, get, list, watch, patch, delete` on
  `ipam.cluster.x-k8s.io/ipaddressclaims`.
- `get, list, watch` on `ipam.cluster.x-k8s.io/ipaddresses`.
- `get, list` on `Secrets` (read-only).
- `create, patch` on `Events`.
- `create, get, list, watch, update` on `coordination.k8s.io/leases`
  (for leader election).

Proxmox and Libvirt are analogous; substitute their infra CR kinds.

### Cross-namespace problem

A Provider may live in any namespace. The provider's deployment lives
in `install_namespace` (typically `banlieue-system`). The provider's
SA must have:

- Cluster-wide read on its own infra CRDs (cluster-scoped grant via
  ClusterRoleBinding) — OK.
- Per-namespace read on Secrets and Events in *each* Provider's
  namespace.

Option A: grant Secrets cluster-wide. Simple but broad.
Option B: per-namespace RoleBindings, dynamically managed.

Use **Option B**. When a Provider CR is created, the lifecycle
controller ensures a `RoleBinding` exists in the Provider's namespace
binding the provider's ClusterRole (or a smaller Role) to the SA.
The RoleBinding has the Provider as its `ownerReference`, so it GC's
when the last Provider in that namespace goes away.

## Upgrade flow

`ProviderClass.spec.image` change:
1. Lifecycle controller patches the Deployment.
2. Kubernetes rolls pods.
3. Status reflects `deployedImage` once the new image is observed in
   `Deployment.status.observedGeneration` and
   `availableReplicas == replicas`.

## Tasks

> **Status (audited against the tree 2026-09-24): the lifecycle itself is
> built; only the Helm packaging is outstanding.** One structural change
> from the plan below: lifecycle does **not** live in the main controller.
> ADR-0012 gave it its own binary, `banlieue-operator`, so the controller
> stays out of the business of creating Deployments.

- [x] ~~Add `ProviderClass` CRD to `banlieue-api`. Regenerate.~~ **Done** —
      `crates/banlieue-api/src/banlieue/providerclass.rs` →
      `deploy/crds/banlieue.io_providerclasses.yaml` (ADR-0012).
- [x] ~~Wire admission webhook: `Provider.spec.providerClassRef.name`
      must reference an existing `ProviderClass`.~~ **Done, without a
      webhook** — ADR-0007 chose `ValidatingAdmissionPolicy` over a
      webhook (no cert rotation, no extra pod, fails closed on its own):
      `deploy/admission/providerclass-guardrails.yaml` and
      `provider-immutability.yaml`. Those policies are optional hardening,
      so the reconciler does not depend on them — a dangling
      `providerClassRef` surfaces as a `Provider` condition instead.
- [x] ~~Implement `provider_lifecycle/` in the main controller.~~ **Done,
      relocated** — `banlieue-operator`'s `reconciler/provider.rs` and
      `reconciler/providerclass.rs` (ADR-0012, ADR-0003).
- [x] ~~Implement manifest builders for Deployment/SA/ClusterRole/
      ClusterRoleBinding.~~ **Done** — `banlieue-operator/src/workload.rs`,
      pure functions returning a `WorkloadSet`, shared with
      `banlieue bootstrap` (ADR-0013) so a CLI install and an
      operator-spawned one are identically shaped.
- [x] ~~Implement the per-namespace RoleBinding dance (watch
      Provider CRs in addition to ProviderClass).~~ **Done** — Option B
      as designed above: Role + RoleBinding in the Provider's namespace,
      Deployment and ServiceAccount in the workload namespace, plus the
      per-Provider import Role/RoleBinding in the build namespace
      (ADR-0016 §4).
- [x] ~~Wire ownership/finalizers so deletion is clean.~~ **Done** —
      owner references plus a `Provider` finalizer that deletes what owner
      references cannot reach (the cluster-scoped ClusterRoleBinding, and
      objects in other namespaces).
- [x] ~~Add startup self-install option: when the main controller boots,
      it can optionally install default ProviderClasses for vsphere,
      proxmox, libvirt pointing at known image tags (`--auto-install`
      flag). Off by default.~~ **Superseded by ADR-0013** — an explicit
      `banlieue bootstrap operator` subcommand rather than a boot-time
      side effect, which keeps install auditable and dry-runnable
      (`--dry-run`). Covered by `make kind-e2e-bootstrap`,
      `kind-e2e-dry-run` and `kind-e2e-escape-hatch`.
- [ ] Update Helm chart (Phase 4 work) so banlieue installation only
      needs the main controller; ProviderClasses bring up the rest.
      **Still open** — no chart exists yet (roadmap 12 §4.6). `banlieue
      bootstrap` is today's single-command install.

## Tests

- [x] ~~Manifest builder unit tests: golden YAML files per provider kind.~~
      **Done** — `workload_tests.rs` and `bootstrap_tests.rs` assert the
      built objects directly rather than against golden files, which is
      what let them encode *why* (e.g. that a ClusterRoleBinding has no
      namespace scope, so a Secret rule in it reaches every namespace).
- [x] ~~Reconciler integration test on `kind`: apply a ProviderClass,
      assert Deployment becomes Ready.~~ **Done** —
      `tests/e2e_provider_workload.rs` (`make kind-e2e-workload`), ADR-0014.
- [x] ~~Upgrade test: change image, observe rollout completes.~~ **Done** —
      `e2e_provider_class.rs::editing_the_class_image_rolls_the_existing_workload`
      (`make kind-e2e-class`).
- [x] ~~Cleanup test: delete ProviderClass, assert all resources GC.~~
      **Done** —
      `e2e_provider_class.rs::changing_the_provider_class_prunes_the_previous_workload`
      covers the harder case: the orphaned ClusterRoleBinding nothing owns,
      which GC cannot reach.
- [x] ~~Cross-namespace RoleBinding test.~~ **Done** —
      `tests/e2e_workload_namespace.rs` (`make kind-e2e-workload-namespace`).
      `tests/e2e_provider_pause.rs` additionally covers `spec.paused`.

## Definition of done

- Single-command install: `helm install banlieue ...` brings up the
  main controller only. Operators add ProviderClasses to enable each
  backend.
- Upgrading a provider is a YAML edit to `ProviderClass.spec.image`.
- Removing a backend is `kubectl delete providerclass <kind>`.

> **Two of three met (2026-09-24).** Upgrade-by-edit and
> delete-the-class both work and are covered by `make kind-e2e-class`.
> The single-command install exists as `banlieue bootstrap operator`
> (ADR-0013); the Helm form of it is still roadmap 12 §4.6, which is why
> this phase stays 🔶 on [`ROADMAPS.md`](../../ROADMAPS.md).

## Gotchas

- **Operator self-install race**: if the controller auto-installs
  default ProviderClasses on first boot, watch out for the
  chicken-and-egg between CRD creation and the first reconcile. Wait
  for CRDs to be Established before the reconciler starts.
- **ClusterRole drift**: an admin may edit the cluster role
  out-of-band. The lifecycle controller should reconcile it back to
  the template, but log a warning rather than silently overwriting
  custom rules. Consider a `banlieue.io/managed: "true"` annotation
  on managed resources, and refuse to touch resources lacking it.
- **Image pull secrets**: ProviderClass should accept
  `imagePullSecrets`. Defer to `extraVolumes` or add an explicit
  field if cleaner.
- **RBAC scope creep**: it's tempting to grant providers broad
  permissions. Keep them minimal; if a provider needs more, add it
  explicitly to the template with a clear comment.
- **Cross-namespace deletion**: when the last Provider in a namespace
  is deleted, the RoleBinding should GC. But if multiple Providers
  share a RoleBinding (same kind, same ns), only delete when the last
  one goes. Implement via owner ref count check or a generic
  "namespace-binding owner" reference scheme.
- **Image tag drift**: pinning a `:latest`-style tag in ProviderClass
  means restarts may bring up unexpected versions. Webhook can warn
  on non-digest, non-semver tags. Phase 4 enforcement.
