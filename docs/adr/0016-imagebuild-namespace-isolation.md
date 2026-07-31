# 16. Isolate image builds in their own namespace and PodSecurity domain

Date: 2026-07-31

## Status

Accepted — amends ADR-0010 (VMImage build pipeline).

## Context

ADR-0010 delegates raw-disk construction to kairos-operator: `banlieue-imagebuilder`
creates an `OSArtifact` (`build.kairos.io/v1alpha2`), kairos-operator builds the
disk into a PVC, and each provider imports it. ADR-0010 named a
`--build-namespace` but did not say what that namespace must permit, and both
`banlieue-imagebuilder` and `banlieue-provider-libvirt` defaulted it
inconsistently. Deploying to a real cluster made the omission concrete.

### Evidence

`banlieue-system` carries the Pod Security Standards `restricted` labels
(`deploy/controller/namespace.yaml`). With the imagebuilder pointed there,
kairos-operator could create the `OSArtifact` but the apiserver rejected its
build pod:

```
pods "kairos-ubuntu-2404-build-g9k67" is forbidden:
violates PodSecurity "restricted:latest":
  privileged (container "build-cloud-image" must not set securityContext.privileged=true),
  allowPrivilegeEscalation != false (containers "pull-image-baseimage", "build-cloud-image"),
  unrestricted capabilities (must set securityContext.capabilities.drop=["ALL"]),
  runAsNonRoot != true,
  seccompProfile (must set securityContext.seccompProfile.type to "RuntimeDefault" or "Localhost")
```

This is intrinsic, not a kairos misconfiguration. Building a bootable disk image
requires loop devices, `mount`, and raw block access. `privileged: true` is the
mechanism kairos uses to get them, and it is **rejected by the `baseline`
profile as well as `restricted`**. There is no intermediate profile that admits
it. The only PSS level that admits a privileged container is `privileged` —
which is the *absence* of enforcement, not a relaxation of it.

### What relaxing `banlieue-system` would actually cost

`banlieue-system` is not a build namespace. It holds, and will keep holding:

| Workload | Credential reach |
| --- | --- |
| `banlieue-controller` | cluster-wide: VirtualMachine, VMImage, all infra CRs |
| `banlieue-operator` | cluster-wide: creates RBAC, binds ClusterRoles (a grantor) |
| `banlieue-imagebuilder` | VMImage status, OSArtifacts |
| `banlieue-provider-<class>-<name>` | one per backend, per instance, forever — each holds that backend's credentials Secret |

Setting `enforce: privileged` on that namespace to accommodate one build pod
removes the admission floor from **all** of them. The operator is the sharpest
case: it is an RBAC grantor, so a compromise there escalates by design rather
than by exploit.

The asymmetry is the point — the exception is needed by exactly one workload,
and granting it in `banlieue-system` extends it to every current and future
control-plane component.

## Decision

**1. Image builds get a dedicated namespace, `banlieue-imagebuild`.**

It holds the `OSArtifact`, the artifacts PVC, kairos' build pods, and the
provider import Jobs. `banlieue bootstrap` creates it, labelled:

```yaml
pod-security.kubernetes.io/enforce: privileged
pod-security.kubernetes.io/audit: restricted
pod-security.kubernetes.io/warn: restricted
```

`enforce: privileged` is required by kairos. `audit`/`warn` stay at
`restricted` deliberately: enforcement must be off, but every pod that exceeds
`restricted` should still generate an audit event and a warning. Silence would
make a *new* privileged workload appearing there indistinguishable from the one
we knowingly allowed.

**2. `banlieue-system` keeps `enforce: restricted`, unchanged.**

**3. Both components default `--build-namespace` to `banlieue-imagebuild`.**

They must agree: the imagebuilder creates the artifacts PVC there and the
provider's import Job mounts it, and a PVC cannot be mounted across namespaces.
A cross-crate test (`crates/banlieue/src/cli_tests.rs`) asserts the defaults are
equal — the disagreement it now prevents was a real bug that neither crate's own
tests could see.

**4. The import Job runs under a ServiceAccount in `banlieue-imagebuild`,
granted narrowly back into the Provider's namespace.**

The Job must read the `Provider` and its credentials Secret, which live with the
Provider. That is a cross-namespace read, so:

- `ServiceAccount banlieue-import` in `banlieue-imagebuild`
- `Role` in the **Provider's** namespace granting `get` on that Provider and its
  credentials Secret / CA ConfigMap, `resourceNames`-scoped as ADR-0012 already
  does for provider workloads
- `RoleBinding` in the Provider's namespace naming the cross-namespace subject

The Job does **not** reuse the provider controller's ServiceAccount. That SA can
create Jobs (ADR-0011); a build-namespace workload holding it could create
further privileged pods. The import identity is read-only by construction.

## Consequences

### What this buys

- The privileged exception is confined to a namespace whose only inhabitants are
  pods that are privileged by nature. `banlieue-system` keeps its admission floor.
- Control-plane ServiceAccount tokens are no longer mounted in the same
  namespace as privileged build pods.
- "Which namespaces permit privileged workloads?" has a one-line answer that
  names a build namespace, rather than one that names the control plane.
- `ResourceQuota` / `LimitRange` are namespace-scoped, so a multi-gigabyte build
  cannot exhaust quota the controllers depend on.

### What this explicitly does NOT buy

**A namespace is not a containment boundary for a privileged pod.** A privileged
container can access host devices, mount the host filesystem, and escape to the
node it runs on, regardless of namespace. It can then read every secret the
kubelet on that node has materialised for *any* pod scheduled there.

This decision therefore reduces **admission surface**, not **escape capability**.
It is defence-in-depth, and it should not be described internally or in docs as
"isolating" the build in a security sense.

The control that actually bounds an escape is scheduling: builds pinned to
dedicated, tainted nodes that run nothing else of value. **This has since been
implemented** — `banlieue bootstrap` accepts `--build-node-selector` and
`--build-toleration`, and the imagebuilder sets them on the `OSArtifact`, which
kairos-operator propagates to the build pod. Verified on hardware: the
privileged build pod runs on the dedicated node and nowhere else.

Note what is **not** pinned: the provider's **import Job**. It is unprivileged,
so it gains nothing from the dedicated node, and its placement is not ours to
choose — it mounts the artifacts PVC, and the scheduler resolves placement from
the bound PV's own constraints. On node-local storage that confines the Job to
the volume's node without any help from us; on network-attached storage there is
nothing to confine. A node selector there would add a constraint Kubernetes
never needed and would be wrong as soon as the storage is not node-local.

The import Job does take **tolerations**, which are not a placement decision: a
toleration grants permission to land on a node the scheduler has already
chosen, and is needed only because a dedicated build node is tainted.

Even with the build pinned, the honest posture is: **a compromised kairos build
image compromises the node it runs on.** Pinning means that node is a dedicated
build node rather than a control-plane node, which is what bounds the damage —
it does not sandbox the build. The namespace split limits which credentials sit
beside it and keeps a bad provider image from silently gaining the same power.

### Costs accepted

- One more namespace for `bootstrap` to create and for operators to know about.
- Cross-namespace RBAC for the import Job — three objects, `resourceNames`-scoped.
- `banlieue-imagebuild` cannot be `restricted`, so any workload landing there
  gets no admission floor. Mitigated by keeping the namespace single-purpose and
  by `audit`/`warn: restricted` making violations visible.

## Alternatives considered

**Run builds in `banlieue-system` with `enforce: privileged`.** Simplest, one
namespace, no cross-namespace RBAC. Rejected: it removes the admission floor
from the controller, the operator (an RBAC grantor), and every provider pod, to
accommodate a single workload. The blast radius grows with every backend added.

**Use `baseline` instead of `privileged` on a shared namespace.** Does not work
— `privileged: true` is denied by `baseline`. There is no profile between
"denies privileged" and "enforces nothing".

**Build without privileges** (rootless buildah / kaniko style). This would
dissolve the problem entirely and is the outcome to prefer if it becomes
available. Not available: `OSArtifact` builds need loop devices, and
kairos-operator is third-party. Revisit if kairos gains an unprivileged builder.

**Keep the PVC in the build namespace but run the import Job in
`banlieue-system`.** Rejected: a PVC cannot be mounted from another namespace,
which is the constraint that forces Job and PVC to be co-located in the first
place.
