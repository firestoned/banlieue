# Provider lifecycle: installing and registering backends

banlieue installs itself and provisions backend controllers for you. Applying a
`Provider` is enough to bring a backend up — there are no manifests to edit and
no Helm values to thread through.

Two pieces make that work:

- **`ProviderClass`** — cluster-scoped, says *what banlieue runs* for a class of
  backends (which provider role, which image, what pod shape).
- **`banlieue-operator`** — watches `Provider` CRs and creates one Deployment,
  ServiceAccount, Role, RoleBinding and ClusterRoleBinding **per Provider**.

The topology decision is recorded in ADR-0003, and the CRD and role split in
ADR-0012 (both under `docs/adr/` in the repository).

## Install

```sh
banlieue bootstrap operator
```

That applies, in dependency order: the namespace, every CRD, RBAC for the
controller and operator, both Deployments, and one `ProviderClass` per backend
compiled into the binary.

The CRDs are generated from the binary's own Rust types at runtime, so the
schemas installed are by construction the ones the running binary implements —
the usual "regenerate the CRDs first or they silently drift" step does not
exist here.

Useful flags:

| Flag | Purpose |
| --- | --- |
| `--namespace` | Install somewhere other than `banlieue-system`. |
| `--version` | Image tag. Defaults to the binary's own version. |
| `--registry` | Air-gapped mirror: image becomes `<registry>/banlieue:<version>`. |
| `--dry-run` | Print the YAML and exit. Never contacts a cluster. |
| `--skip-provider-classes` | Do not seed a class per backend. |

### GitOps

`--dry-run` emits a `kubectl apply -f -`-ready stream and needs no kubeconfig,
so the same code path serves GitOps users:

```sh
banlieue bootstrap operator --dry-run > clusters/prod/banlieue.yaml
```

!!! note "Audit the operator's RBAC minting (SEC-007)"
    The operator holds RBAC-granting verbs cluster-wide so it can mint
    per-instance Roles and bind each provider's shared ClusterRole. The
    apiserver's escalation prevention caps what it can hand out, but a
    compromised operator could still grant its full permission set to any
    ServiceAccount. Worth an audit rule in production: alert on
    `ClusterRoleBinding` objects created by the `banlieue-operator` identity
    whose `roleRef` is not `banlieue-provider-*` (security review 2026-07-31,
    accepted risk with monitoring).

## Register a backend

Create the credentials Secret and a `Provider`:

```sh
kubectl -n banlieue-system create secret generic prod-vc-creds \
  --from-literal=username='svc-banlieue@vsphere.local' \
  --from-literal=password='...'
```

```yaml
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata:
  name: prod-vc
  namespace: banlieue-system
spec:
  providerClassRef:
    name: vsphere
  connection:
    endpoint: https://vcenter.example.com/sdk
    credentialsRef:
      name: prod-vc-creds
```

That is the whole workflow. Within a reconcile the operator creates:

```text
Deployment/banlieue-provider-vsphere-prod-vc
ServiceAccount/banlieue-provider-vsphere-prod-vc
Role/banlieue-provider-vsphere-prod-vc
RoleBinding/banlieue-provider-vsphere-prod-vc
ClusterRoleBinding/banlieue-provider-vsphere-prod-vc
```

Watch it come up:

```sh
kubectl get providers -A
kubectl get provider prod-vc -o jsonpath='{.status.workload}' | jq
```

## Why one Deployment per Provider

A single Deployment serving every vCenter is cheaper, and it is what banlieue
did before. It was replaced because sharing a process shares failure:

- **No cross-backend starvation.** A hung reconcile against one vCenter cannot
  stall any other — they are different processes.
- **Credential isolation.** Each pod's Role grants `get` on exactly the one
  Secret its Provider names, scoped with `resourceNames`. A compromised
  provider pod holds one backend's credentials, not all of them.
- **Network policy becomes expressible.** One pod, one egress target.
- **Proportional caches.** Each pod narrows its watch server-side to its own
  Provider, so its informer cache does not grow with the fleet.

The cost is a pod, Lease and ServiceAccount per backend. If that ever becomes
the binding constraint, the deferred `deploymentStrategy: Shared` knob is a
backward-compatible addition — the rationale is recorded in ADR-0003.

## Upgrading

The image lives on the class, so upgrading a fleet is one edit:

```sh
kubectl patch providerclass vsphere --type=merge \
  -p '{"spec":{"image":{"tag":"v0.2.0"}}}'
```

To upgrade a single backend first, create a second class pinning the new image
and repoint one Provider at it — see `examples/08-providerclass-vsphere.yaml`.

!!! note "Class edits apply immediately"
    The operator watches `ProviderClass` and maps each edit back to every
    Provider referencing it, so a fleet-wide image bump or an un-pause takes
    effect at once rather than waiting on a requeue. The mapper kube calls is
    synchronous and cannot list Providers itself, so it reads the controller's
    own reflector store — already maintained for the primary watch, and free.

## Pausing

`spec.paused` on a `Provider` suspends just that backend; on a `ProviderClass`
it suspends every Provider of that class. Already-running workloads are left
untouched — pausing stops reconciliation, it does not tear anything down.

## Deleting

Deleting a `Provider` removes its whole workload. Most objects go by owner
reference; the ClusterRoleBinding cannot, because a cluster-scoped object owned
by a namespaced one is deleted immediately by the garbage collector as if its
owner were missing. The operator therefore holds a finalizer
(`banlieue.io/provider-workload`) and removes that binding explicitly.

If a Provider ever hangs in `Terminating`, that finalizer is why — check the
operator's logs before removing it by hand, since doing so leaks the
ClusterRoleBinding.

## Running without the operator

The operator is convenience, not a hard dependency. For air-gapped or
tightly-controlled installs where a controller that mints workloads is not
acceptable, you can run banlieue with **no operator at all** and install each
provider statically:

```sh
# Platform layer: CRDs + the main controller, no operator.
kubectl apply -f deploy/crds/
kubectl apply -R -f deploy/controller/

# One statically installed provider per backend you use.
banlieue bootstrap provider vsphere
```

(`banlieue bootstrap operator` always installs the operator together with the
controller, so the operator-less platform layer comes from the manifests —
or from `banlieue bootstrap operator --dry-run` with the operator objects
dropped. `banlieue bootstrap provider <backend>` installs only that
provider's own workload: ServiceAccount, RBAC, ConfigMap, Deployment.)

A statically installed provider serves every `Provider` of its class **in the
install namespace**. It is not owned by any `Provider` CR, so there is no
operator to adopt or delete it — you manage its Deployment like any other
statically deployed controller.

What you give up, compared to the operator path:

- **Per-instance isolation.** One provider pod serves every `Provider` of its
  class in the namespace instead of one pod per `Provider` — the shared
  failure and shared identity model described above.
- **Workload lifecycle.** There is no `status.workload` reporting, no
  finalizer-based cleanup, and upgrades mean re-running
  `banlieue bootstrap provider <backend> --version <new>` (or `--dry-run`)
  instead of patching a `ProviderClass` image tag.

What you gain: nothing in the cluster holds workload-minting or RBAC-granting
rights — the most powerful identity in the default install simply does not
exist.

!!! note "Namespace-scoped by design (security review 2026-07-31)"
    No provider identity holds cluster-wide Secret access. The standalone
    install therefore ships a namespaced Role granting `get` on Secrets and
    ConfigMaps in the install namespace, and passes `--namespace` so the watch
    is scoped to match. Keep every `Provider` and its credentials Secret in
    that namespace; Providers elsewhere are not served.

!!! warning
    Do not run both paths for the same backend. Two controllers reconciling one
    Provider will fight over its status.

## Verifying it end to end

The lifecycle above is covered by an e2e suite that runs against a real API
server in a kind cluster (ADR-0014):

```sh
make kind-e2e            # fast loop: installs from deploy/ manifests, runs the suite
make kind-e2e-bootstrap  # installs via `banlieue bootstrap operator`, then runs both suites
make kind-e2e-ci         # what CI runs: bootstrap path + teardown + diagnostics on failure
make kind-e2e-logs       # dump operator + workload state when something fails
```

The two install paths are tested separately on purpose. `kind-e2e` applies
`deploy/operator/` directly — the GitOps path — while `kind-e2e-bootstrap` runs
the installer real users run. They can drift, and when they do the failure is
invisible from the other side: an early bug where `bootstrap operator` never
installed the shared per-backend ClusterRole was caught only because a stale
copy happened to linger in a reused cluster. CI therefore runs the bootstrap
path.

It asserts what unit tests structurally cannot: that the apiserver *accepts*
what the operator builds — selector/template agreement, `resourceNames`
validity, owner-reference correctness, that `metadata.managedFields` shows the
operator owning `status.workload` and never `status.conditions`, and that
deletion removes all five objects (garbage collection for the owned four, the
finalizer for the ClusterRoleBinding).

It also covers the `workloadNamespace` override, where the Deployment and
ServiceAccount are deliberately left **unowned** — a cross-namespace
`ownerReference` is invalid and would have the garbage collector delete them
immediately — so the finalizer has to remove them itself. A leak there is
silent: the Provider disappears while its workload keeps running, still holding
credentials.

!!! note "The spawned pod is expected to be NotReady"
    The suite's Provider points at `vcenter.invalid`, which by RFC 2606 can
    never resolve, so its provider pod never reaches a backend and
    `status.workload.readyReplicas` stays `0`. That is the expected outcome —
    the operator's contract is *creating a correctly shaped workload*, not the
    provider's ability to log in. Backend connectivity is covered separately
    against `vcsim`.

CI runs the same Makefile target via `.github/workflows/e2e.yaml`. It needs no
secrets, registry or backend, so it runs on fork pull requests.
