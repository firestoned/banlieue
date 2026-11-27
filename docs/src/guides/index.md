# Guides

Production-oriented, step-by-step guides for installing and operating banlieue
on a real cluster, using a released container image, `ghcr.io/firestoned/banlieue:<release>`.
No build-from-source, no simulators.

## Get started

Bring up a management cluster and install banlieue.

<div class="grid cards" markdown>

- :material-server-plus: **[Host Bootstrap](host-bootstrap.md)**

    Bare metal to a running k0s cluster: provision a KVM hypervisor, or prepare
    a workstation and discover a vSphere estate — the step before every other
    guide here.

- :material-map: **[End-to-End Setup](end-to-end-setup.md)**

    The whole chain in one diagram: bootstrapping the management cluster,
    installing banlieue, registering a backend, building an image, and
    provisioning a VM — with links out to every guide below.

- :material-engine: **[Core Controller](core-controller.md)**

    Install the CRDs, the `banlieue-controller`, RBAC, and the optional
    ValidatingAdmissionPolicies — the foundation every provider builds on.

- :material-cog-sync: **[Providers](../providers/index.md)**

    Then set up a backend: what a `Provider` is, how to choose a class, and
    one page per class, from the Cloud Hypervisor host installer to Azure.

- :material-update: **[Upgrading from 0.6.0 to 0.6.1](upgrading-to-0.6.1.md)**

    The Azure provider's new objects, a `migrationPolicy` recreate that now
    recreates, provider Jobs confined to the build namespace, and the
    measured image digest.

- :material-update: **[Upgrading from 0.5.x to 0.6.0](upgrading-to-0.6.md)**

    What changed between v0.5.9 / v0.5.10 and v0.6.0, the order to roll it
    out in (CRDs, controllers, then host-resident providers), and what to
    do on Cloud Hypervisor hosts.

</div>

## Providers

Setting up a backend (Cloud Hypervisor, vSphere, libvirt, Proxmox, Azure)
has its own section: **[Providers](../providers/index.md)**, with one page
per provider class, the operator's provider lifecycle, and the comparison
that helps you choose.

## Workloads

Warm pools, single-use claims, the identity layer above them, and Cluster API.

<div class="grid cards" markdown>

- :material-layers-triple: **[VirtualMachine Pools](virtualmachine-pools.md)**

    Keep already-provisioned VMs standing by so a consumer does not wait out
    provisioning — sizing, image rollout, and the one field that fails
    silently if you guess it.

- :material-hand-extended: **[VirtualMachine Claims](virtualmachine-claims.md)**

    Take one warm member out of a pool, for one identity, once — binding,
    `ttlSeconds`, why release is always deletion, and why `subject` must
    never carry a credential.

- :material-account-check: **[Testing Claim Authorization](testing-claim-authorization.md)**

    A dev cluster that logs you in with your GitHub account through Dex, so
    the claim-subject admission policy can be watched accepting and refusing
    real identities.

- :material-lan-disconnect: **[Isolated Sandbox Segment](isolated-sandbox-segment.md)**

    Put sandboxes on a segment with no router and let them out only through
    gateway VMs: the network classes, the two-NIC gateway, a pool with no
    default route, and how to check it is isolated.

- :material-call-split: **[Environment / Provider Isolation](environment-provider-isolation.md)**

    When one `Provider` serves several environments (more classes) and when
    a second `Provider` is the right call (separate credentials or vCenter).

- :material-shield-account: **[Sandbox Identity with mediatore](sandbox-identity-mediatore.md)**

    What happens *after* the bind: the companion broker that turns a claim
    into an attested, delegated identity — and why the SPIRE server belongs
    on its own cluster in an enterprise. An entry point; the deep docs live
    in the mediatore repo.

- :material-graph-outline: **[The Claim Flow, End to End](../concepts/virtualmachine-claim-flow.md)**

    The same story as sequence diagrams: kubelogin and the OIDC flow that
    establishes *who* you are, the admission checks, the bind race, the state
    machine, and the two finalizers that make release mean something.

- :material-puzzle: **[Cluster API](cluster-api.md)**

    Install banlieue as a clusterctl infrastructure provider with
    `clusterctl init --infrastructure banlieue`, and the scoped ClusterRole
    that authorises Cluster API's manager on the infrastructure kinds only.

</div>

## Images

Building the disk images VMs boot from.

<div class="grid cards" markdown>

- :material-cloud-download: **[Setting up the Kairos Operator](kairos-operator-setup.md)**

    Install the third-party [Kairos operator](https://kairos.io) banlieue's
    image-build pipeline depends on, and confirm it with a smoke-test build.

- :material-image-sync: **[Using banlieue-imagebuilder](using-banlieue-imagebuilder.md)**

    Turn an OCI/Kairos image into a `VMImage` raw disk automatically (ADR-0010)
    — install `banlieue-imagebuilder`, watch the build, and see exactly what's
    implemented today versus tracked as a follow-up.

- :material-package-variant: **[Building a Kairos Hadron VM Template](building-kairos-hadron-template.md)**

    Build the OCI image the imagebuilder consumes when stock Hadron needs
    extra payloads baked in. For Alpine on vSphere, see
    [Building an Alpine VM Template](../providers/alpine-vsphere-template.md).

</div>

## Operations

Running banlieue day to day.

<div class="grid cards" markdown>

- :material-chart-line: **[Observability](observability.md)**

    Prometheus metrics on every role, scraping operator-spawned provider pods,
    the Grafana dashboard, opt-in OpenTelemetry traces, and why a standby
    replica reports Ready.

- :material-gauge: **[Capacity-Aware Scheduling](capacity-scheduling.md)**

    What each provider publishes as capacity, why a VM was refused, the
    two settings that change the check, and a scenario-by-scenario test
    plan for vSphere, Cloud Hypervisor, libvirt and Proxmox.

- :material-account-group: **[Multi-Tenancy](multi-tenancy.md)**

    Serve a namespace per team by label: the selector, the default scope,
    onboarding and removing a team, one shared provider that serves every
    team without handing out its credentials, and what the selector does not
    isolate.

</div>

!!! info "Looking to hack on banlieue itself?"
    Building from source, running against `kind`/`vcsim`, and the
    `*-run-local` workflow live under **[Developer → Local Development](../developer/local-development.md)**.

## Conventions used in these guides

- Everything is pinned to **one release**, named once in `BANLIEUE_VERSION`
  (pick it from the [releases page](https://github.com/firestoned/banlieue/releases)).
  The manifests the guides apply live under
  [`deploy/`](https://github.com/firestoned/banlieue/tree/main/deploy) at that
  tag. A tag is cut without editing those files, so the image tag inside them
  is only a reference: every guide that applies a Deployment sets its image to
  `ghcr.io/firestoned/banlieue:$BANLIEUE_VERSION` straight after.
- All workloads run in the **`banlieue-system`** namespace under the
  Pod Security **restricted** profile.
- A cluster of **Kubernetes 1.30+** is assumed (required for the
  ValidatingAdmissionPolicies; the controllers themselves work on older
  clusters).

```sh
# The release to install, from https://github.com/firestoned/banlieue/releases
export BANLIEUE_VERSION=vX.Y.Z
# Pin the repo to it so the manifests match the image.
git clone --branch "$BANLIEUE_VERSION" --depth 1 https://github.com/firestoned/banlieue
cd banlieue
```
