<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Threat Model

> **Status:** Living document. Last full pass **2026-09-23**, covering
> **ADR-0045** (the vTPM endorsement key certificate is published on the infra
> CR, mirrored onto a bound claim, and — for a `tpmEnabled` machine — gates
> `GuestReady`). A-6 is split so the EK certificate is its own asset; TB-4
> gains three rows for the guest-reported read path libvirt forces; §8 records
> the two residues it leaves — swtpm certificates that never expire, and a
> per-host local CA that is only as trustworthy as the host. **banlieue
> acquires no new privilege in the guest: the certificate is read with
> ADR-0043's read-only `guest-file-open` path, never `guest-exec`.**
> Against the architecture defined by ADR-0001 … ADR-0055 (0049 is Proposed,
> not implemented).
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
| `banlieue-controller` | `banlieue-controller` | `banlieue-system` | Watches `VirtualMachine`, schedules onto a `Provider`, creates provider infra CRs. Also runs the `VirtualMachinePool` loop (ADR-0046) and the `VirtualMachineClaim` loop (ADR-0047) — the latter binds a pool member to a subject and destroys it on release |
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
| A-9 | **The subject's own credential (a JWT)** — the thing a sandbox is handed so it can act as its subject | **Never in banlieue.** Broker → in-guest agent over mTLS, after attestation (ADR-0049). Not on a disk, not in a CR, not in a hypervisor channel | **Critical** — it *is* the subject's identity. Kept out of banlieue entirely, which is why no banlieue compromise discloses it |
| A-8 | **Guest readiness marker** — `/run/banlieue/phase`, written by the guest and read by the provider | guest tmpfs → `qemu-guest-agent` → `LibvirtMachine.status.guestInstalled` (ADR-0043) | Low on its own, but it gates pool membership: a guest that can assert it early gets handed out early. **Not an integrity signal** — see §6/TB-4 and §8 |
| A-7 | **Claim bindings** — which subject was given which VM, and when | `VirtualMachineClaim.spec.subject` + `status`, mirrored onto the member as `banlieue.io/claim-subject-*` annotations (ADR-0047) | Medium — discloses who was using which sandbox to every reader of the namespace; a *forged* binding makes the record say someone requested a VM they never asked for |
| A-10 | **The consumer's cached Kubernetes credential** — the ID token `kubectl oidc-login` writes to disk after a browser flow | `~/.kube/cache/oidc-login` on the consumer's own machine, outside every boundary below | High — it authenticates as that consumer, so it can create claims *attributed to them*. banlieue has no control here; see §8 |
| A-6 | vTPM identity and sealed disk-encryption keys | vSphere VM, per-clone (ADR-0039/0040); on libvirt, **swtpm state keyed by domain UUID** (ADR-0050) | High — a shared or surviving TPM identity breaks per-VM disk-encryption isolation |
| A-6a | **vTPM endorsement key certificate** — the public anchor an attestation quote is checked against | vCenter-issued and read host-side on vSphere; `swtpm_localca`-issued into the vTPM's NVRAM on libvirt, exported by the guest to `/run/banlieue/ek.pem` and mirrored to `VirtualMachineClaim.status` (ADR-0045) | Low confidentiality — it is a **public key**, deliberately readable by every reader of the claim. Its value is *integrity of binding*: it must name the VM banlieue actually created, or ADR-0049 verifies a quote from the wrong machine |

## 4. Actors

| Actor | Assumed capability | Trusted? |
| --- | --- | --- |
| Cluster admin | Full Kubernetes API | Yes — out of scope by policy (`SECURITY.md`) |
| Platform admin | Creates `ProviderClass`, `Provider`, `VMImage` | **Semi-trusted — must be treated as infrastructure-admin-equivalent** |
| Tenant / VM author | Creates `VirtualMachine` in a namespace | **Untrusted for confidentiality of A-1/A-2** — see §7 |
| Claim consumer / sandbox broker | Creates `VirtualMachineClaim`s, holding `create` on them in a namespace | **Bounded by admission**: `spec.subject.id` must equal the authenticated username unless the requester is a declared broker (§7.6). A broker is trusted for attribution by definition |
| Compromised controller pod | RCE inside one banlieue pod | Untrusted |
| Compromised hypervisor endpoint | Attacker-controlled host reachable at `spec.connection.endpoint` | Untrusted |
| External contributor | Opens a PR from a fork | Untrusted |
| **OIDC identity provider** (and any bridge in front of it, e.g. Dex for GitHub) | Mints the ID tokens the API server accepts, and therefore **decides what `request.userInfo.username` is** | **Semi-trusted, and entirely outside banlieue's control.** Every guarantee the claim-subject policy makes is downstream of this actor: banlieue checks `subject.id` against a username it did not derive. Compromise or misconfiguration here makes every claim attribution meaningless — §8 |
| Hypervisor operator | vCenter/libvirt privileges outside Kubernetes | Semi-trusted — **can read datastores and storage pools banlieue writes to**, and on libvirt can read swtpm state on the host filesystem |

## 5. Trust boundaries

```
  ┌───────────────────────── TB-7 ───────────────────────────┐
  │ OIDC identity provider — external, unmanaged by banlieue │
  │   consumer ──▶ browser flow ──▶ signed ID token          │
  │   token cached on the consumer's laptop (A-10)           │
  │   API server verifies via JWKS ──▶ userInfo.username     │
  └────────────────────────────┬─────────────────────────────┘
                               │ the username every claim's
                               ▼ spec.subject.id is checked against
                    ┌──────────────────────── TB-1 ─────────────────────────┐
  tenant namespace  │  banlieue-system (restricted PSA)                     │
  ┌──────────────┐  │  ┌────────────┐   ┌──────────┐   ┌─────────────────┐  │
  │VirtualMachine├──┼─▶│ controller ├──▶│ operator ├──▶│ provider pod    │  │
  ├──────────────┤  │  └─────┬──────┘   └──────────┘   └────────┬────────┘  │
  │    Pool /    │  │        │  pool fills; a claim BINDS one   │           │
  │    Claim     ├──┼───────▶│  member to a subject and         │           │
  └──────────────┘  │        │  DESTROYS it on release          │           │
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
                    │  guest → qemu-guest-agent → EK cert, phase (read-only)  │
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
| TB-7 | External identity provider → API server | The assertion of *who the caller is*, on which the whole claim attribution model rests |

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
| **Two claims bind the same warm member**, so one VM is handed to two subjects | I, E | The bind is a JSON merge patch carrying the member's `resourceVersion`, so a member written since the snapshot is rejected `409` and the loser re-picks — `crates/banlieue-controller/src/reconciler/claim.rs`. A member already carrying `banlieue.io/claim` reads as `MemberPhase::Claimed` (`reconciler/pool.rs::member_view`) and `pick_member` filters to `Ready` only (`reconciler/claim_plan.rs`) |
| **A released member is recycled to a second subject** | I | Release is always deletion: `claim_plan.rs::next_step` has no transition back to an unclaimed state, and `pool_plan`'s invariants 1–2 keep the pool from reclaiming a labelled member. The VM is the isolation boundary, so reuse is the one outcome the design must exclude (ADR-0047) |
| **A claim attributes a sandbox to a subject that never requested one** | S, R | `banlieue-virtualmachineclaim-subject-authorization` VAP (ADR-0047 Decision 10): `spec.subject.id` must equal the authenticated username, `subject.issuer` must be in an operator allowlist, and `spec` is immutable so the check cannot be undone by a later patch — `deploy/admission/virtualmachineclaim-subject-authorization.yaml`. Declared brokers are exempt from the id check by design (§7.6) |
| A credential is written into `spec.subject`, which is world-readable in the namespace and copied onto the member | I | Partly controlled: `subject.id` must now equal the authenticated username, so it cannot be an arbitrary string, and `issuer` is allowlisted. Neither stops a determined author from putting a secret in a field shaped like a username — banlieue never reads it as a credential and never forwards it to a guest, but nothing rejects one. See §7.6, §7.10 |
| `delete virtualmachineclaims` destroys running VMs | D | Equivalent to `delete virtualmachines` by design — releasing a claim *is* destroying the sandbox. RBAC is the only control; §7.10 |
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
| A deleted VM leaves its sealed-key material behind (swtpm state, UEFI NVRAM varstore) | `domain_undefine` takes **no flags parameter** and unconditionally sends `MANAGED_SAVE\|NVRAM\|TPM` (ADR-0050 Decision 5) — the flag cannot be forgotten at a call site — `crates/banlieue-libvirt/src/procs.rs`, proven against a real libvirtd in `tests/live_libvirtd.rs`. Live since ADR-0050: `LibvirtMachine`'s finalizer calls it on every teardown — `crates/banlieue-provider-libvirt/src/machine_client.rs` (`undefine`), invoked from `reconciler/libvirtmachine.rs::finalize_backend`, which then verifies the domain is actually gone before deleting its volumes |
| Something other than the intended guest answers the broker's mTLS connection and receives the subject's token | S | The agent returns a **TPM quote over `status.nonce`**, verified against the EK certificate published on the claim (ADR-0049, ADR-0045). The vTPM is unique per VM by construction — on vSphere because deferred install never installs the golden template so each clone installs with its own vTPM (ADR-0040), on libvirt because swtpm state is keyed by domain UUID. **Half implemented since 2026-09-23**: ADR-0045 landed, so the anchor now exists on the claim (`status.tpmEndorsementCertificates`). **On libvirt only** is a `tpmEnabled` member unbindable until it publishes one — that gate lives in the libvirt reconciler's `GuestReady`, and vSphere neither publishes `GuestReady` (ADR-0043's vSphere transport is deferred) nor gates on the certificate, so a vSphere member with an empty list is not held back by anything. ADR-0049 itself is still Proposed, so nothing yet *performs* the verification — the anchor is in place, the exchange is not |
| A token minted for a different service is presented to the agent and accepted | S | `aud` is the agent's **own configured audience** and is deliberately never read from the claim (ADR-0049 Decision 5) — otherwise whoever wrote the claim chooses the audience |
| A claim names an attacker-controlled issuer, so the agent fetches that attacker's JWKS and every forged token verifies | S, T | The issuer allowlist in `banlieue-virtualmachineclaim-subject-authorization` — added for audit honesty, and load-bearing here: `subject.issuer` is a CR field the agent is asked to trust as a key source (ADR-0049 Decision 7) |
| A guest asserts `GuestReady` while still installing, or a compromised guest asserts it to be handed out sooner | S, T | **Bounded, not prevented.** The marker is guarded on immucore's active/passive sentinels so the *live installer* cannot assert it (`examples/16-cloud-config-guest-phase.yaml`), but a guest that has already been compromised can write anything. This is why ADR-0043 Decision 9 states the signal is liveness, never integrity: it is not a control against a hostile guest, and the pool hands out fresh, unclaimed VMs. Integrity is ADR-0049's problem (§8) |
| A guest returns a huge or malformed payload to `guest-file-read`, hoping to fault the provider's reconcile loop | D | The read is capped before decoding (`MARKER_READ_MAX`, checked on the base64 *and* the decoded bytes), every parse failure returns "not installed" rather than an error, and the handle is closed on every path so an agent's handle table cannot be exhausted — `crates/banlieue-provider-libvirt/src/guest.rs`, with dedicated tests for oversize, undecodable and garbage input |
| A guest reports **another member's** EK certificate, so a verifier later checks a quote against the wrong machine | S, T | Two independent checks, and the weaker one runs first. The subject CN must equal `<domain-name>:<domain-uuid>` — both values banlieue itself assigned when it defined the domain — or the certificate is discarded, never published, and the machine reports `GuestReady=False`/`TpmEndorsementMismatch` (`crates/banlieue-provider-libvirt/src/guest.rs`, `ek_cn_matches`). The check that cannot be forged is ADR-0049's: an EK certificate is a **public key**, so a guest presenting one it does not hold the private half of cannot certify an AK under it, and the substitution fails the step it was made to pass |
| A guest publishes bytes that are not a certificate at all, into a status field consumers feed to a certificate library | T | Parsed as X.509 before publication, with the PEM label required to be `CERTIFICATE` — a `PRIVATE KEY` block is valid PEM and is refused (`parse_ek_pem_str`, `x509-parser`). Size-capped at `EK_READ_MAX` on the base64 *and* the decoded bytes, like the phase marker. Unit-tested against a real `swtpm_localca` certificate and against junk |
| banlieue's read of the certificate becomes a way to run code inside a sandbox | E | The certificate lives in the vTPM's NVRAM, and the obvious way to fetch it is `tpm2_nvread` over `guest-exec` — **host-to-guest arbitrary code execution**. banlieue does not use `guest-exec` anywhere: the guest exports the certificate to `/run/banlieue/ek.pem` and banlieue reads that file with `guest-file-open` at an explicit `mode: "r"` (ADR-0045 Decision 2). A provider that only ever opens guest files read-only cannot be turned into a remote shell by a compromised controller |
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

A second exposure at this boundary is the guest's **own disk**, which is only
outside an operator's reach if it was actually encrypted:

| Threat | STRIDE | Control |
| --- | --- | --- |
| A VM presents every outward sign of a sealed disk — vTPM attached, `tpmEnabled: true` on its class, `Ready` — while its disk is plaintext on the datastore or storage pool, readable by any hypervisor operator and by the next tenant of the same host | I | **ADR-0048**, since 2026-09-23: `banlieue-controller` rejects `tpmEnabled: true` paired with an `installMode: Immediate` image (or an image with no `template` block, which is a pre-built and therefore pre-laid disk) — `Ready=False`, `reason=ImageClassMismatch`, and **no infrastructure CR is created**, so the machine never reaches a provider that would build it. `crates/banlieue-controller/src/reconciler/virtualmachine.rs` (`image_class_mismatch`), checked after the class and image resolve and before scheduling. This is the only place the combination is visible: `tpmEnabled` is on the `VMClass` and `installMode` on the `VMImage`, so no `ValidatingAdmissionPolicy` can see both |
| A sandbox workload mounts the still-attached install ISO and reads the build-time cloud-config overlay baked into it (`VMImage.spec.cloudConfigs`, `isoOverlay`) | I | **ADR-0044**, since 2026-09-23: the medium is ejected when the ADR-0043 `guestInstalled` marker flips, via `virDomainUpdateDeviceFlags` with `AFFECT_LIVE\|AFFECT_CONFIG` — `crates/banlieue-libvirt/src/procs.rs` (`DEVICE_MODIFY_EJECT`). `GuestReady` is published only **after** the eject, so a `VirtualMachinePool` — whose sole readiness input is that condition — cannot bind a member whose installer is still attached. `converge()` also suppresses the ISO from the domain XML it redefines each pass, or a redefine would restore it while status claimed otherwise |
| A guest reboots into its still-attached installer and re-runs the install, re-sealing a fresh disk over the previous tenant's workload | T, D | Same control. Both flags are passed deliberately: a `LIVE`-only eject leaves the medium in the persistent definition, where it returns at the next boot. The rendered `<os>` block also stops offering `<boot dev='cdrom'/>` once detached |
| A guest holds the cdrom tray locked so the eject fails, and is handed out anyway | D | `VIR_DOMAIN_DEVICE_MODIFY_FORCE` is **not** passed. A failed eject leaves `GuestReady` unpublished, so the member never becomes available and `provisioningTimeoutSeconds` (ADR-0046) reaps it as poisoned. Failing toward an unavailable member rather than an exposed one is the intended direction |
| An `installMode: Manual` image asserts a deferred install it does not perform, and seals nothing | I | **Not controlled.** `Manual` is ADR-0040's escape hatch for a non-Kairos build and banlieue cannot inspect what such an image does. Recorded in §8 |

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

### TB-7 — External identity provider → API server

Every other boundary in this document is one banlieue can place a control on.
This one is not: the API server derives `request.userInfo.username` from a
token minted elsewhere, and the claim-subject policy compares
`spec.subject.id` against that derived name. **banlieue's strongest
attribution guarantee is therefore no stronger than the issuer behind it**,
which is worth stating plainly rather than leaving implicit in §8.

| Threat | STRIDE | Control |
| --- | --- | --- |
| The issuer is compromised, or mints a token for an attacker under a victim's name | S, R | **None in banlieue.** The API server vouches for the username; nothing downstream can second-guess it. Recorded in §8 — the mitigation is issuer-side (MFA, short lifetimes, key custody) and is the operator's, not banlieue's |
| `usernamePrefix` in the policy's ConfigMap disagrees with the API server's `--oidc-username-prefix` | T | **Fails in the safe direction, but silently.** Too short a prefix makes the comparison unsatisfiable and every claim is refused; too long forces authors to store the *prefixed* name in `subject.id`, which is the exact shape ADR-0047 Decision 9 was amended to eliminate and which leaves the in-guest agent a value it cannot compare to any JWT. Neither is a bypass. It is a configuration coupling between two independently-managed objects, so §7.6 now names it |
| A stolen cached ID token is used to create claims in the victim's name | S, R | **None in banlieue**, and not specific to claims — a stolen bearer token authenticates as its owner everywhere in Kubernetes. It is called out because the *consequence* here is an audit record that says the victim asked for a sandbox. §8 |
| An issuer the site does not use is named in `spec.subject.issuer` | S, R | The `issuers` allowlist in `banlieue-virtualmachineclaim-subject-authorization`. This is a check on the *claim*, not on the caller — nothing reveals which issuer actually minted the caller's token (§8) |
| The agent is pointed at an attacker's JWKS via `spec.subject.issuer` | S, T | Same allowlist, doing double duty — see TB-4. Load-bearing for verification, not merely for audit tidiness (ADR-0049 Decision 7) |

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
   version of this grant** — see requirement 7. Automation that creates
   `VirtualMachine`s must therefore hold `get` on the user-data it names.
3. **Do not co-locate unrelated Secrets in `banlieue-system`.** The controller's
   grant is namespace-wide, not `resourceNames`-scoped; admission bounds who can
   *trigger* a read, not what the controller identity could read if compromised.
4. **Treat `banlieue-imagebuild` RoleBindings as node-root grants** (§TB-3), and
   do not grant pod-create there to anyone who should not hold every Provider's
   hypervisor credentials.
5. **`Provider` and `ProviderClass` creation is platform-admin-only.** The VAPs
   bound the damage; they do not make these safe to delegate.
6. **Install the claim subject policy, and audit its ConfigMap.**
   `deploy/admission/virtualmachineclaim-subject-authorization.yaml` is what
   makes `spec.subject` an attribution the API server vouched for rather than
   free text (ADR-0047 Decision 10). It binds `subject.id` to the
   authenticated username, confines `subject.issuer` to an allowlist, and
   makes `spec` immutable so the check cannot be undone by a later patch.

   Three operator obligations come with it:
   - **The `issuers` list ships with a placeholder.** A cluster that does not
     edit it rejects every real claim — visibly, which is the intended
     failure direction.
   - **The `brokers` list is the trust concentration.** Anyone named there
     may attribute a sandbox to any identity, so that ConfigMap is as
     sensitive as the audit trail it underwrites. It is empty by default;
     alert on changes to it.
   - **`usernamePrefix` must match the API server's
     `--oidc-username-prefix`.** These are two independently-managed objects
     that have to agree, and nothing checks that they do. `spec.subject.id`
     stores the **raw** subject as the issuer spells it — a JWT carries no
     `oidc:` prefix, so the prefixed form would leave the in-guest agent a
     value it cannot compare (ADR-0047 Decision 9, amended; ADR-0049) — and
     the policy re-applies the prefix at comparison time instead. A mismatch
     fails safe but silently: see §6/TB-7. Re-check it whenever the cluster's
     authentication configuration changes, not only at install.

   The binding is `parameterNotFoundAction: Deny`, so a missing ConfigMap
   blocks claims rather than degrading to "any subject is fine".

   **Re-applying the policy file resets the ConfigMap**, because the file
   ships it — the same property `vmimage-import-source.yaml` has. A
   `kubectl apply -f deploy/admission/` therefore reverts `issuers`,
   `brokers` and `usernamePrefix` to their shipped defaults, and the
   placeholder issuer rejects every real claim. Keep site values in your
   own overlay or re-apply them after.

7. **Install the admission policies.** Every control in §6/TB-1 is a
   `ValidatingAdmissionPolicy` in `deploy/admission/`. A cluster that skips
   them reverts to the pre-hardening threat surface. **`banlieue bootstrap`
   (ADR-0013) does not emit these policies** — it installs workloads, RBAC and
   namespaces only. Applying `deploy/admission/` is a separate, mandatory step
   in every install path, including GitOps. Two of the policies
   (`*-credentialsref-authorization`, `*-userdata-authorization`) need an API
   server new enough for the CEL `authorizer` variable and are shipped as
   separate files for exactly that reason; both are `failurePolicy: Fail`.
8. **Pin `VMImage.spec.sources[].importFrom` to digests.** The
   `banlieue-vmimage-import-source` VAP enforces a registry allowlist supplied
   as a parameter ConfigMap and **fails closed** if that ConfigMap is absent —
   configure it.
9. **If you enable `VMClass.spec.tpmEnabled`, use deferred install.** The
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

   **The pairing is now enforced** (ADR-0048, 2026-09-23), closing what
   ADR-0040 Decision 5 left open. `banlieue-controller` rejects a
   `tpmEnabled: true` `VMClass` paired with an `installMode: Immediate`
   image — or with an image carrying no `template` block, which is a
   pre-built and therefore pre-laid disk — before scheduling, and creates
   no infrastructure CR. What used to attach a real TPM and silently
   encrypt nothing is now `Ready=False`, `reason=ImageClassMismatch`. The
   residual operator responsibility is `installMode: Manual`, the escape
   hatch for non-Kairos builds, which banlieue cannot inspect (§8).

   **On libvirt, a `tpmEnabled` image must also export its EK certificate**
   (ADR-0045). The guest writes the PEM to `/run/banlieue/ek.pem` — two
   lines of cloud-config beside the ADR-0043 phase marker, needing
   `tpm2-tools` in the image — because swtpm keeps no host-side copy for
   banlieue to read. An image that cannot do this never reports
   `GuestReady` for a `tpmEnabled` class and so is never bound: the failure
   is a member visibly stuck with `reason=TpmEndorsementPending`, not one
   handed out without an attestation anchor. Machines with
   `tpmEnabled: false` are unaffected.
10. **A claim isolates at the VM boundary, not the Kubernetes one — grant
    `create` and `delete` on `virtualmachineclaims` narrowly.** A
    `VirtualMachineClaim` guarantees that one *VM* is used by one subject and
    then destroyed (ADR-0047). It does **not** partition the Kubernetes
    namespace: two subjects' sandboxes are ordinary `VirtualMachine`s side by
    side, so anyone with namespace read sees both bindings, and — per
    requirement 1 — anyone who can `get` the infra CRs can read the user-data
    behind either. This is the same single-tenant posture as ADR-0025, applied
    to a feature whose name invites the opposite assumption.

    Three consequences to enforce by RBAC until Decision 10's policy exists:
    - `create virtualmachineclaims` lets the holder record **any**
      `spec.subject`, including someone else's. The binding record (A-7) is
      only as trustworthy as that grant.
    - `delete virtualmachineclaims` destroys running VMs.
    - `spec.subject` is world-readable in the namespace and is copied onto the
      member as an annotation. Put an identifier there, never a token — the
      subject's credential belongs on the phase C attested channel, keyed to
      `status.nonce`.
11. **Recommended audit rule:** alert on any `ClusterRoleBinding` created by the
   `banlieue-operator` identity whose `roleRef` is not `banlieue-provider-*`
   (accepted-risk monitoring for the operator's RBAC-minting capability).

## 8. Accepted risks

| Risk | Why accepted | Revisit when |
| --- | --- | --- |
| `banlieue-operator` holds the union of every permission it can delegate | Kubernetes escalation-prevention requires it; granting `escalate`/`bind` instead would be strictly worse. The ceiling is auditable in one file | A provider needs a materially more dangerous permission |
| `banlieue-imagebuild` runs `privileged` | kairos' builder genuinely requires loop devices and chroot; isolation is by namespace | kairos supports rootless builds |
| Rendered user-data is visible in `VSphereMachine.spec` **and `LibvirtMachine.spec`** | Single-tenant, single-namespace posture (ADR-0025). ADR-0042 closed the *escalation* (a principal reaching user-data it could not read); the *reflection* to anyone who can already `get` the infra CR is unchanged and deliberate, and ADR-0050 extends it to a second kind rather than introducing a new risk | A second tenant or namespace becomes real — ADR-0025's superseded per-VM Role design is the shape that scales |
| The controller's user-data Role is namespace-wide, not `resourceNames`-scoped | The names a validly admitted `VirtualMachine` may cite are unknowable when the manifest is written; authorization moves to admission, where the requesting identity still exists (ADR-0042). A compromise of the controller identity itself is still bounded only by the namespace | The install stops shipping `deploy/admission/`, or per-VM RBAC becomes tractable |
| A libvirt guest's TPM is **emulated by swtpm on the host**, so a host-root adversary can read the sealed-key material that a physical TPM would protect | This is the libvirt trust model, not a banlieue choice; the hypervisor operator is already semi-trusted (§4) and hypervisor compromise is out of scope (§9). EK trust anchors differ per backend, which roadmap 17 phase F (ADR-0049) is the plan to make explicit via `Provider.spec.attestation.ekTrustBundle` | Attestation ships (ADR-0049), or a libvirt host is no longer operator-trusted |
| **swtpm EK certificates never expire** — the observed `notAfter` is `9999-12-31` — so validity-period checks are not a revocation mechanism on libvirt | Nothing banlieue controls: `swtpm_localca` issues them that way. Expiry would be a weak control regardless, since a sandbox's whole life is measured in minutes. Revocation on libvirt is removing the issuing host's CA from the trust bundle, which is a per-host decision an administrator makes explicitly (ADR-0049) rather than one a certificate makes for them | The trust bundle lands (ADR-0049) and needs a per-certificate revocation story rather than a per-host one |
| On libvirt the EK certificate is **reported by the guest**, not read from the hypervisor, because swtpm persists no host-side copy | Forced by swtpm's design, not chosen (ADR-0045): the certificate is loaded into the vTPM's NVRAM and the issuing temp directory is deleted, and no libvirt RPC exposes it. It is sound because an EK certificate is a public key — substituting another member's does not yield its private half, so ADR-0049's activation fails — and the subject-CN binding catches the substitution earlier still. The residue is that a guest can *withhold* its certificate, which denies only its own readiness | libvirt or swtpm grows a host-side read, or a member withholding its certificate becomes something worth distinguishing from one that is merely slow |
| `GuestReady` can be asserted by any code running as root inside the guest, so it proves which disk booted only for a guest that has not been compromised | It is a *liveness* signal by construction (ADR-0043 Decision 9) and is consumed only to decide when a **fresh, unclaimed** VM joins a warm pool — before any subject has touched it. Treating it as integrity would be the error; the document and the ADR both say so explicitly | Attestation ships (ADR-0049), at which point a TPM quote over the claim nonce is the integrity signal and this one stays what it is |
| A **broker** both holds subject credentials and is the party that verifies TPM quotes, so its compromise is the design's worst case | Somebody has to hold the credential to deliver it, and somebody has to verify the quote; concentrating both in one audited component is preferable to spreading either. banlieue is deliberately not that component (ADR-0049 Decision 2), so a controller compromise discloses no subject credential | The broker is split into deliver/verify roles, or hardware-backed key custody becomes available to it |
| A declared **broker** may attribute a sandbox to any identity, so the audit trail is only as honest as the broker is | Handing sandboxes out on behalf of other people is a broker's entire purpose (roadmap phase C); a broker that could only name itself could not broker. The concentration is explicit, empty by default, and confined to one auditable ConfigMap (§7.6) rather than diffused across everyone holding `create` | A broker is compromised, or claims need per-request proof of the subject's consent rather than the broker's assertion |
| **The identity provider is trusted absolutely, and is outside banlieue** | The API server is the only thing that can attest a caller, and it attests whatever the configured issuer asserted. banlieue cannot verify an upstream IdP without becoming an IdP. The claim-subject policy is still worth having: it binds an attribution to *whatever* identity the cluster does authenticate, which is strictly better than free text | banlieue ever needs an attribution stronger than the cluster's own authentication — at which point the answer is per-request proof from the subject (a signed consent, or the attested channel of ADR-0049), not a better check on the caller |
| A consumer's **cached ID token** sits on their laptop (`~/.kube/cache/oidc-login`) and authenticates as them if stolen | Not specific to claims — every Kubernetes bearer token behaves this way, and client-side credential custody is out of scope (§9). Recorded because the claim-specific consequence is distinctive: the audit trail records the *victim* requesting a sandbox, which is exactly the fiction §6/TB-1 exists to prevent. Short token lifetimes are the issuer-side mitigation | Claims carry per-request proof of the subject's intent rather than only the caller's identity |
| `subject.issuer` is allowlisted but never *verified*: the API server does not reveal which issuer minted the caller's token | Nothing in Kubernetes can attest it, so an allowlist is the strongest available check — it stops a claim naming an issuer the site does not use, which is what would make the recorded attribution meaningless. The claim deliberately carries no token to verify (ADR-0047 Decision 9) | The in-guest agent's JWT validation lands (roadmap phase C), at which point the *guest* verifies issuer, audience and `oid` against the claim |
| An `installMode: Manual` image can claim a deferred install it does not perform, so a `tpmEnabled` VM built from it is unencrypted and says nothing | `Manual` exists precisely for builds banlieue does not drive (ADR-0040), so inspecting it is not possible without becoming its build system. ADR-0048 closes the case banlieue *can* see (`Immediate`, and an absent `template`) and fails closed there; `Manual` is a deliberate operator assertion, narrower than the blanket gap it replaced | A backend reports sealed-partition state back to banlieue, making the assertion verifiable rather than trusted |
| The NoCloud `cidata` seed stays attached after install, so a guest can read its own rendered user-data (A-2) from inside the sandbox | ADR-0044 Decision 4, deliberate and scoped: cloud-init re-reads its datasource on every boot, so removing the seed risks regressing per-boot modules in a way that needs its own live verification on a reboot — not a first boot. Ejecting the *installer* removes the build-time overlay shared across every VM, which is the broader exposure; what remains is each guest's own material, which that guest's workload could in principle obtain anyway | The seed eject is verified live across a reboot, or user-data delivery stops needing a persistent datasource |
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

**Auditing the CALM model counts as a trigger.** TB-7 exists because
`architecture.json` gained an `network-oidc-issuer` node and the wires around
it, and the question "is that actor in the threat model?" answered *no* — for
an actor every claim attribution already depended on. The two documents
describe the same system from different angles, so a component that is new in
one is a prompt to check the other.
