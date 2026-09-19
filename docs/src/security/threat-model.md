<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Threat Model

> **Status:** Living document. Last full pass **2026-09-19**, against the
> architecture defined by ADR-0001 … ADR-0050 (0043–0049 are reserved by
> roadmap 70 and unissued).
> **Method:** asset/actor enumeration, trust-boundary decomposition, STRIDE per
> boundary, control mapping to the manifests in `deploy/` and the crates in
> `crates/`.
>
> This document describes *what banlieue defends, from whom, and how*. It is
> the companion to [`SECURITY.md`](https://github.com/firestoned/banlieue/blob/main/SECURITY.md),
> which describes how to **report** a vulnerability. Specific unremediated
> findings are handled through
> [private vulnerability reporting](https://github.com/firestoned/banlieue/security/advisories/new),
> not this page.

## 1. What banlieue is, in security terms

banlieue is a Kubernetes control plane that turns a namespaced `VirtualMachine`
custom resource into a real virtual machine on a hypervisor (vSphere, libvirt,
Proxmox). It holds two things an attacker wants:

1. **Hypervisor credentials.** A `Provider` names a Secret containing
   infrastructure-admin credentials for a vCenter or libvirtd — a username and
   password for vCenter, an **mTLS client certificate and private key** for
   libvirtd (there is no password in the libvirt path by design). Those
   credentials are, by construction, more powerful than the Kubernetes cluster
   banlieue runs in — they can create, delete, and read the disks of every VM
   on the backend, including VMs banlieue never created.
2. **Guest bootstrap material.** cloud-config / user-data routinely carries SSH
   authorized keys, cluster join tokens, and registration secrets. banlieue
   moves that material from a Kubernetes Secret into a guest, across several
   intermediate representations.

Everything below follows from those two facts. The memory-safety surface is
small and well-controlled (no `unsafe`, no subprocess execution anywhere in the
workspace, a fuzzed libvirt wire decoder); **the meaningful risk in banlieue is
authorization and data-flow, not memory corruption.**

## 2. Components

| Component | Identity | Namespace | Scope |
| --- | --- | --- | --- |
| `banlieue-controller` | `banlieue-controller` | `banlieue-system` | Watches `VirtualMachine`, schedules onto a `Provider`, creates provider infra CRs |
| `banlieue-operator` | `banlieue-operator` | `banlieue-system` | Provider lifecycle (ADR-0012): creates provider Deployments, ServiceAccounts, Roles, RoleBindings |
| `banlieue-provider-vsphere` / `-libvirt` | per-`Provider` SA | `banlieue-system` | Talks to the hypervisor; reconciles infra CRs (`VSphereMachine`, `LibvirtMachine` — ADR-0050) and realises them as real VMs/domains |
| `banlieue-imagebuilder` | `banlieue-imagebuilder` | `banlieue-system` | Drives kairos `OSArtifact` builds; merges cloud-config (ADR-0037) |
| per-zone import Job | `banlieue-import` | `banlieue-imagebuild` | Uploads a built ISO to a datastore, creates a template (ADR-0020) |
| kairos build pod | kairos-operator's SA | `banlieue-imagebuild` | **Privileged** — loop devices, mount, chroot |

## 3. Assets

| ID | Asset | Where it lives | Impact if lost |
| --- | --- | --- | --- |
| A-1 | Hypervisor credentials — vCenter username/password, or a libvirt **mTLS client key** | Secret named by `Provider.spec.connection.credentialsRef` | **Critical** — full virtualization-layer compromise, independent of Kubernetes |
| A-2 | Guest bootstrap material (cloud-config, SSH keys, join tokens) | Secrets/ConfigMaps → `VSphereMachine.spec.userData` / `LibvirtMachine.spec.userData` → `guestinfo.userdata` or a NoCloud ISO → built image | High — guest compromise, lateral movement into provisioned fleet |
| A-3 | VM image artifacts (ISO / raw disk) | `OSArtifact` PVC, then a vSphere datastore under `banlieue-images/` | **Critical** — a tampered image compromises every VM built from it |
| A-4 | Integrity of the control plane's own decisions | `Provider`, `ProviderClass`, `VMImage`, `VMClass` CRs | High — a forged `Provider` redirects credentials; a forged `VMImage` redirects the fleet's boot media |
| A-5 | Released binaries and container images | GHCR, GitHub Releases | **Critical** — downstream supply-chain compromise |
| A-6 | vTPM identity and sealed disk-encryption keys | vSphere VM, per-clone (ADR-0039/0040); on libvirt, **swtpm state keyed by domain UUID** (ADR-0050) | High — a shared or surviving TPM identity breaks per-VM disk-encryption isolation |

## 4. Actors

| Actor | Assumed capability | Trusted? |
| --- | --- | --- |
| Cluster admin | Full Kubernetes API | Yes — out of scope by policy (`SECURITY.md`) |
| Platform admin | Creates `ProviderClass`, `Provider`, `VMImage` | **Semi-trusted — must be treated as infrastructure-admin-equivalent** |
| Tenant / VM author | Creates `VirtualMachine` in a namespace | **Untrusted for confidentiality of A-1/A-2** — see §7 |
| Compromised controller pod | RCE inside one banlieue pod | Untrusted |
| Compromised hypervisor endpoint | Attacker-controlled host reachable at `spec.connection.endpoint` | Untrusted |
| External contributor | Opens a PR from a fork | Untrusted |
| Hypervisor operator | vCenter/libvirt privileges outside Kubernetes | Semi-trusted — **can read datastores and storage pools banlieue writes to**, and on libvirt can read swtpm state on the host filesystem |

## 5. Trust boundaries

```
                    ┌──────────────────────── TB-1 ─────────────────────────┐
  tenant namespace  │  banlieue-system (restricted PSA)                     │
  ┌──────────────┐  │  ┌────────────┐   ┌──────────┐   ┌─────────────────┐  │
  │VirtualMachine├──┼─▶│ controller ├──▶│ operator ├──▶│ provider pod    │  │
  └──────────────┘  │  └─────┬──────┘   └──────────┘   └────────┬────────┘  │
                    │        │ TB-2 (Secret read)               │           │
                    │        ▼                                  │ TB-4      │
                    │   ┌─────────┐                             │           │
                    │   │ Secrets │                             │           │
                    │   └─────────┘                             │           │
                    └──────────────────────────────────┬────────┼───────────┘
                                                 TB-3  │        │
                    ┌──────────────────────────────────▼──┐     │
                    │ banlieue-imagebuild (PRIVILEGED PSA) │     │
                    │ cloud-config Secrets │ Job │ kairos │     │
                    └───────────────────┬──────────────────┘     │
                                   TB-5 │                        ▼
                    ┌───────────────────▼────────────────────────────────────┐
                    │ Hypervisor — vCenter (HTTPS + creds)                    │
                    │             libvirtd (mTLS, native RPC; ADR-0011/0050)  │
                    │  datastores / storage pools, VMs & domains, vTPM/swtpm  │
                    └────────────────────────────────────────────────────────┘

  TB-6: GitHub Actions / GHCR ──▶ released images & binaries
```

| ID | Boundary | Crossing |
| --- | --- | --- |
| TB-1 | Tenant namespace → `banlieue-system` | A `VirtualMachine` causes privileged work in another namespace |
| TB-2 | Control-plane pod → Kubernetes Secrets | Credential and user-data reads |
| TB-3 | `banlieue-system` (restricted) → `banlieue-imagebuild` (privileged) | Image builds and per-zone imports. The imagebuilder and provider **pods** run in `banlieue-system`; what sits across the boundary is the build inputs (cloud-config Secrets), the import Job and kairos' privileged builder — so both the imagebuilder's cloud-config `Role` (ADR-0041) and the operator-minted import `Role` are cross-namespace bindings |
| TB-4 | Cluster → hypervisor | Authenticated API calls carrying A-1 |
| TB-5 | Cluster → shared datastore | ISO/disk artifacts written to storage other people can read |
| TB-6 | Contributor / CI → published artifact | Build and release |

## 6. Threats by boundary

### TB-1 — Tenant namespace → control plane

| Threat | STRIDE | Control |
| --- | --- | --- |
| A `Provider` points at an attacker host and ships real credentials to it | S, I | `banlieue-provider-connection` VAP: absolute URL, `https://` for vsphere/proxmox, no plain `http://`, no `@` userinfo, no `#` fragment |
| TLS verification silently disabled | T, I | Same VAP: `insecureSkipTLSVerify: true` requires the explicit, auditable annotation `banlieue.io/allow-insecure-tls: "true"` |
| A `Provider` names a Secret its author cannot read (confused deputy) | E, I | `banlieue-provider-credentialsref-authorization` VAP uses the CEL `authorizer` — the creating principal must itself be able to `get` that Secret |
| A `ProviderClass` mints RBAC or places pods in `kube-system` | E | `banlieue-providerclass-guardrails` VAP: `additionalRules` may not name `secrets`, `*`, `escalate`, `bind`, `impersonate`; system namespaces rejected for `workloadNamespace` |
| Ref-swapping an existing VM onto another class/image | T | `banlieue-virtualmachine-immutable-refs`, `banlieue-provider-immutable-class` VAPs |
| Resource-exhaustion via absurd specs (`numCpus`, disk counts) | D | schemars `range`/`length`/`maxItems` constraints on `VMClass` and `VSphereMachine` |
| A `VirtualMachine` names user-data its author cannot read (confused deputy) | E, I | `banlieue-virtualmachine-userdata-authorization` VAP (ADR-0042) uses the CEL `authorizer`: the creating principal must itself be able to `get` the Secret / ConfigMap named by `spec.userData`, in the `VirtualMachine`'s own namespace — `deploy/admission/virtualmachine-userdata-authorization.yaml` |
| Rendered user-data is readable from `VSphereMachine.spec` / `LibvirtMachine.spec` | I | **No code control — this is the accepted reflection of ADR-0025.** See §7.1 and §8 |
| User-influenced strings (`domainName`, `pool`, disk/volume names) injected into libvirt domain XML | T, E | Every value is escaped on the way in by `esc()` — all five XML entities, uniformly in text *and* attributes, so there is no context-dependent rule to get wrong — `crates/banlieue-provider-libvirt/src/xml/escape.rs`, applied throughout `xml/domain.rs`; both have dedicated `_tests.rs` |

### TB-2 — Pods → Secrets

The design principle is: **no banlieue identity holds cluster-wide Secret
access**, and each identity reads only the objects it can name.

| Control | Where |
| --- | --- |
| Provider `ClusterRole`s grant **zero** Secret/ConfigMap access | `deploy/provider-{vsphere,libvirt}/rbac/clusterrole.yaml` |
| Per-`Provider` `Role` is `resourceNames`-scoped to exactly that Provider's credentials Secret (+ CA bundle if named) | `crates/banlieue-operator/src/workload.rs` (`build_import_role`, `named_rule`) |
| An empty `resourceNames` list is treated as a bug, not a default — the CA-ConfigMap rule is emitted only when the Provider names one | same |
| `banlieue-controller`'s `ClusterRole` carries no Secret rule at all; its Secret/ConfigMap `get` is a namespaced `Role` | `deploy/controller/rbac/clusterrole.yaml`, `deploy/controller/rbac/role.yaml` |
| `banlieue-imagebuilder`'s cloud-config Secret access is a namespaced `Role` in the **build** namespace, not a `ClusterRole` rule (ADR-0041) | `deploy/imagebuilder/rbac/role.yaml` |
| No `ClusterRole` grants Secret access **except `banlieue-operator`'s deliberate `get`** — held only so RBAC's escalation-prevention permits it to delegate that verb into a per-Provider `resourceNames`-scoped `Role`, and never exercised by the operator itself (SEC-007, accepted-with-monitoring) | `deploy/operator/rbac/clusterrole.yaml` |
| That invariant is mechanically **enforced**, not merely checkable: a unit test fails the build if any other `ClusterRole` grants `secrets`/`configmaps` | `crates/banlieue-operator/src/bootstrap_tests.rs` (`only_the_operator_cluster_role_may_grant_secret_access`) |
| The CLI install path emits the same namespaced RBAC as the manifests, so `banlieue bootstrap` and GitOps cannot drift apart | `crates/banlieue-operator/src/bootstrap.rs` (`build_cloud_config_role`) |
| `Credentials` has a hand-written redacting `Debug` | `crates/banlieue-provider-vsphere/src/client/mod.rs` |
| `TlsIdentity` likewise — the libvirt credential *is* the client private key, so `client_key_pem` renders as `<redacted>` and the public CA/cert halves render as byte counts. A regression test asserts the key never reaches `{:?}` | `crates/banlieue-libvirt/src/transport.rs`, `transport_tests.rs` (`tls_identity_debug_redacts_the_private_key`) |
| The libvirt provider `ClusterRole` grants **no `create` and no `delete`** on `libvirtmachines` — the controller owns their lifecycle; a compromised provider cannot mint machines the scheduler never placed | `deploy/provider-libvirt/rbac/clusterrole.yaml` |

### TB-3 — Restricted → privileged namespace

`banlieue-imagebuild` enforces Pod Security Admission `privileged` because
kairos' builder needs loop devices, `mount`, and `chroot`. This is deliberate
and isolated (ADR-0010, ADR-0016), but it has a consequence operators must
internalise:

> **Any principal granted pod-create in `banlieue-imagebuild` is effectively
> node root, and can assume any ServiceAccount in that namespace.** Treat every
> RoleBinding there as a node-root-equivalent grant and review them accordingly.

The `banlieue-import` identity is deliberately *not* the provider controller's
own identity (which can create Jobs), and starts with zero permissions;
`banlieue-operator` grants it narrowly-scoped, per-Provider read access. See
§7 for the hardening this still requires.

### TB-4 — Cluster → hypervisor

| Threat | Control |
| --- | --- |
| MITM / attacker-presented certificate | banlieue owns the `reqwest` client and honours `connection.caBundle` (ADR-0008); CA source validated by `banlieue-provider-cabundle-source` VAP |
| Hostile or unresponsive endpoint stalls every reconcile | 10 s connect / 120 s request timeouts on the vSphere client; timeouts on libvirt connect, recv, and `Session::send` |
| Malformed libvirt RPC frames | Wire decoder is continuously fuzzed (`crates/banlieue-libvirt/fuzz`, `.github/workflows/fuzz.yaml`, ClusterFuzzLite); ADR-0050's domain `decode_*` halves are pure and unit-tested against captured wire bytes, so they are in that fuzz surface too |
| A deleted VM leaves its sealed-key material behind (swtpm state, UEFI NVRAM varstore) | `domain_undefine` takes **no flags parameter** and unconditionally sends `MANAGED_SAVE\|NVRAM\|TPM` (ADR-0050 Decision 4) — the flag cannot be forgotten at a call site — `crates/banlieue-libvirt/src/procs.rs`, proven against a real libvirtd in `tests/live_libvirtd.rs`. **The procedure exists and is correct; no reconciler calls it yet** (§8) — this control is live only once `reconciler/libvirt_machine.rs` lands |
| A half-failed teardown silently leaves domains defined | libvirt 11.3 *fails* undefine on a UEFI domain without `NVRAM` rather than warning; the error is returned, never swallowed, and the live lifecycle test asserts the domain is actually gone — `crates/banlieue-libvirt/tests/live_libvirtd.rs` |
| Credentials leak into logs | No secret is ever logged; redacting `Debug`; provider condition messages are the only verbatim text mirrored to user-facing status |

### TB-5 — Cluster → shared datastore

Built ISOs are uploaded to `banlieue-images/<vmimage>.iso` on a vSphere
datastore — or to a libvirt **storage pool** — and, under deferred install
(ADR-0040), remain CD-ROM-attached to every clone. **Datastore-browse in
vCenter, or filesystem access on a libvirt host, is a much broader privilege
than banlieue admin.** Anything embedded in that ISO — including cloud-config
supplied through `VMImage.spec.cloudConfigs` or `isoOverlay` — should be
treated as readable by every hypervisor operator, not just by banlieue's own
principals. Put per-VM secrets in `VirtualMachine.spec.userData`
(guest-delivered per clone) rather than baking them into a shared image.

### TB-6 — Supply chain

This is the strongest area of the project and is largely already ADR-0006.

| Control | Detail |
| --- | --- |
| Provenance | SLSA build provenance on every release |
| SBOM + VEX | Generated per release; VEX statements are reachability- and presence-derived, and the generators **fail closed** on empty or oversized inputs (256 MiB cap) |
| Signing | cosign signatures on published images |
| Base images | Digest-pinned (Chainguard + distroless), non-root, tracked by Dependabot |
| Actions | SHA-pinned, with one documented exception (`slsa-github-generator` must be tag-referenced or it rejects its own ref) |
| Privileged triggers | No `pull_request_target`, no `issue_comment`. `docs.yaml`'s `workflow_run` is hard-gated to same-repository runs, with a defence-in-depth re-check before any checkout — fork SHAs are never checked out privileged, and the default-branch cache cannot be poisoned |
| Untrusted input | Event fields reach `run:` steps only through `env:` indirection, never inline interpolation |
| Auto-merge | Gated on `pull_request.user.login == 'dependabot[bot]'` (the PR author, not `github.actor`); major-version updates are held for human review |
| Scanning | CodeQL, OpenSSF Scorecard, SAST, grype/OSV, `cargo audit`, `cargo deny`, gitleaks |

## 7. Deployment hardening requirements

These are properties of the **current** design that operators must enforce
themselves. They are not bugs; they are the trust model, and deploying against
a different assumption is unsafe.

1. **Every infra machine CR is a credential-bearing resource — restrict `get`
   on all of them.** `banlieue-controller` resolves `spec.userData` references
   and inlines the *rendered content* into the infra CR in plaintext
   (ADR-0025/ADR-0038) — `VSphereMachine.spec.userData` and, since ADR-0050,
   `LibvirtMachine.spec.userData`, both built by the same
   `build_*_machine` path in `crates/banlieue-controller/src/reconciler/infra.rs`.
   Anyone who can read one of these can read the user-data that produced it,
   SSH keys and join tokens included. This reflection is ADR-0025's accepted
   trade-off (§8), not a bug — but it makes `get vspheremachines` **or `get
   libvirtmachines`** equivalent to reading every user-data Secret any VM in
   that namespace has referenced. **A new provider inherits this property the
   moment it gains a `userData` field; it is a contract-level consequence, not
   a per-provider one.**
2. **`VirtualMachine` create is no longer a Secret-read grant — provided the
   admission policies are installed.** The controller's Role grants `get` on
   Secrets and ConfigMaps across the whole `banlieue-system` namespace with no
   `resourceNames`, because the names a valid `VirtualMachine` may cite are not
   knowable when the manifest is written. What stops that from becoming an
   effective namespace-wide `get secrets` for anyone holding
   `create virtualmachines` is admission, not RBAC:
   `banlieue-virtualmachine-userdata-authorization` (ADR-0042) requires the
   *requesting principal* to be authorized for the same read. **A cluster that
   applies `deploy/controller/` without `deploy/admission/` gets the un-checked
   version of this grant** — see requirement 6. Automation that creates
   `VirtualMachine`s must therefore hold `get` on the user-data it names.
3. **Do not co-locate unrelated Secrets in `banlieue-system`.** The controller's
   grant is namespace-wide, not `resourceNames`-scoped; admission bounds who can
   *trigger* a read, not what the controller identity could read if compromised.
4. **Treat `banlieue-imagebuild` RoleBindings as node-root grants** (§TB-3), and
   do not grant pod-create there to anyone who should not hold every Provider's
   hypervisor credentials.
5. **`Provider` and `ProviderClass` creation is platform-admin-only.** The VAPs
   bound the damage; they do not make these safe to delegate.
6. **Install the admission policies.** Every control in §6/TB-1 is a
   `ValidatingAdmissionPolicy` in `deploy/admission/`. A cluster that skips
   them reverts to the pre-hardening threat surface. **`banlieue bootstrap`
   (ADR-0013) does not emit these policies** — it installs workloads, RBAC and
   namespaces only. Applying `deploy/admission/` is a separate, mandatory step
   in every install path, including GitOps. Two of the policies
   (`*-credentialsref-authorization`, `*-userdata-authorization`) need an API
   server new enough for the CEL `authorizer` variable and are shipped as
   separate files for exactly that reason; both are `failurePolicy: Fail`.
7. **Pin `VMImage.spec.sources[].importFrom` to digests.** The
   `banlieue-vmimage-import-source` VAP enforces a registry allowlist supplied
   as a parameter ConfigMap and **fails closed** if that ConfigMap is absent —
   configure it.
8. **If you enable `VMClass.spec.tpmEnabled`, use deferred install.** The
   requirement is the same on every backend, for different reasons:
   - **vSphere** — the default clone policy duplicates a template's vTPM *and
     its secrets* onto every clone, and banlieue does not set
     `vpxd.clone.tpmProvisionPolicy`. Only per-clone install (ADR-0040) yields
     a unique, per-VM sealed key.
   - **libvirt** — swtpm state is keyed by **domain UUID**, so a new domain
     always gets a fresh TPM and the vSphere duplication problem does not
     arise. Deferred install is still required, because Kairos only ever seals
     partitions to a TPM present *during* install (ADR-0040); an
     already-installed image cannot be encrypted later on any backend.

   Nothing currently *enforces* the pairing — a `tpmEnabled: true` `VMClass`
   with an `installMode: Immediate` image attaches a real TPM and silently
   encrypts nothing (ADR-0040 Decision 5; roadmap 70 phase A3 proposes
   ADR-0048 to close it). Until then this is an operator responsibility.
9. **Recommended audit rule:** alert on any `ClusterRoleBinding` created by the
   `banlieue-operator` identity whose `roleRef` is not `banlieue-provider-*`
   (accepted-risk monitoring for the operator's RBAC-minting capability).

## 8. Accepted risks

| Risk | Why accepted | Revisit when |
| --- | --- | --- |
| `banlieue-operator` holds the union of every permission it can delegate | Kubernetes escalation-prevention requires it; granting `escalate`/`bind` instead would be strictly worse. The ceiling is auditable in one file | A provider needs a materially more dangerous permission |
| `banlieue-imagebuild` runs `privileged` | kairos' builder genuinely requires loop devices and chroot; isolation is by namespace | kairos supports rootless builds |
| Rendered user-data is visible in `VSphereMachine.spec` **and `LibvirtMachine.spec`** | Single-tenant, single-namespace posture (ADR-0025). ADR-0042 closed the *escalation* (a principal reaching user-data it could not read); the *reflection* to anyone who can already `get` the infra CR is unchanged and deliberate, and ADR-0050 extends it to a second kind rather than introducing a new risk | A second tenant or namespace becomes real — ADR-0025's superseded per-VM Role design is the shape that scales |
| The controller's user-data Role is namespace-wide, not `resourceNames`-scoped | The names a validly admitted `VirtualMachine` may cite are unknowable when the manifest is written; authorization moves to admission, where the requesting identity still exists (ADR-0042). A compromise of the controller identity itself is still bounded only by the namespace | The install stops shipping `deploy/admission/`, or per-VM RBAC becomes tractable |
| A libvirt guest's TPM is **emulated by swtpm on the host**, so a host-root adversary can read the sealed-key material that a physical TPM would protect | This is the libvirt trust model, not a banlieue choice; the hypervisor operator is already semi-trusted (§4) and hypervisor compromise is out of scope (§9). EK trust anchors differ per backend, which roadmap 70 phase F (ADR-0049) is the plan to make explicit via `Provider.spec.attestation.ekTrustBundle` | Attestation ships (ADR-0049), or a libvirt host is no longer operator-trusted |
| `LibvirtMachine` has no reconciler yet — the CRD is admitted and schedulable but nothing realises it | Not a security exposure: the failure mode is a VM that never appears, not one that appears unsafely. Recorded so the gap is not mistaken for a control | `reconciler/libvirt_machine.rs` lands (roadmap 13) — re-check the deletion/finalizer path at that point, since that is where NVRAM/TPM cleanup is actually invoked |
| Health endpoint binds `0.0.0.0` and returns a fixed `200` | Standard probe trade-off; carries no data | It ever reports real state |
| Provider condition messages are mirrored verbatim onto user-facing `VirtualMachine` status | Useful diagnostics; providers are in-tree | A third-party provider ships |

## 9. Out of scope

- Anything requiring cluster-admin as a starting position (`SECURITY.md` policy).
- Compromise of the hypervisor itself, or of vCenter/libvirt authorization.
- Guest-OS hardening after boot; banlieue's responsibility ends at delivering
  the bootstrap material.
- `deploy/kind/` — development-only, not held to production standard.
- The MkDocs documentation toolchain (`docs/`), which ships nothing at runtime.

## 10. Maintenance

This threat model is a first-class artifact under
Architecture Driven Development (ADR → CALM → TDD → implement → docs → threat
model). It is the **last step of the cycle**: after any ADR is implemented, a
**full pass** over this document is mandatory, and an ADR does not count as
implemented until that pass is done.

A full pass walks every section above — components, assets, actors, trust
boundaries (including the §5 diagram), the STRIDE tables, hardening
requirements, and the accepted-risk register — rather than appending a row to
whichever table obviously changed. Each new or changed threat must name a
control that actually exists in `deploy/` or `crates/`, or be recorded in §8
with an explicit *Revisit when*. The pass then bumps the header stamp above:
the date **and** the ADR range. "No change" is a valid conclusion, but the
stamp still advances — an unchanged stamp means no pass happened.

Sections most likely to move: a new provider or binary (§2, §4), a new CRD or
contract (§3, §6), a new namespace or PSA level (§5), a new identity or RBAC
grant (§6, §7), a new external dependency in the boot path (TB-6), or any
change to the single-tenant assumption in §7.
