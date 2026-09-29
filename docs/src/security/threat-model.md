<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Threat Model

> **Status:** Living document. Last full pass **2026-09-28**, against the
> architecture defined by **ADR-0001 … ADR-0067** (0057–0059 unallocated,
> 0066 reserved; 0060–0065 and 0067 Accepted 2026-09-27). The 2026-09-28
> pass covers **ADR-0043 Decision 8 as amended 2026-09-28** (an unreachable
> guest agent backs off only after a 10-minute agent-bootstrap grace window,
> so the machine reconciler's cadence can no longer lose the race against a
> pool's `provisioningTimeoutSeconds`): **no change** — no new component,
> asset, actor, boundary or control; A-8's liveness-not-integrity posture and
> both `GuestReady` rows in TB-4/§8 are unaffected by *when* the signal is
> polled. Recorded because the stamp is the deliverable, not because
> anything moved.
>
> A **2026-09-27** pass covered
> **ADR-0049 Decision 10** (`Provider.spec.attestation.ekTrustBundle` — the
> per-backend EK trust anchor is admin-supplied on the Provider, never
> discovered, and banlieue neither resolves nor verifies against it; ADR-0049
> is now **Accepted**, with the in-guest agent and broker deliberately outside
> banlieue). The pass re-walked every section: **no new component, actor,
> namespace or trust boundary** (the broker and agent were already modelled as
> external). §3 gains A-15 (the trust bundle — zero confidentiality, high
> integrity: a rogue CA added to it makes every forged EK certificate verify);
> TB-1 gains its tampering row, shape-validated at admission by
> `banlieue-provider-attestation-ektrustbundle` while content stays a §7.14
> custody obligation; §4 names the **sandbox workload** (a prompt-injectable
> AI agent, hostile by design) as an actor, making explicit what TB-4/TB-5
> already assumed; TB-1 also gains the stale-image-member row (bounded by
> pick order, TTL and rollout); §7 gains 14 (trust-bundle custody) and 15
> (vSphere VM Encryption on sandbox storage classes, roadmap 17 phase F);
> §8's two trust-anchor residues are updated now that the bundle exists.
>
> The **2026-09-27** pass for **ADR-0067** (`banlieue host`, the installer in
> the binary) adds a component, an asset (A-14), an actor row, a trust
> boundary (TB-11, with the §5 diagram), eight STRIDE rows and one accepted
> risk. A pass the same day covers the **host-resident Cloud Hypervisor
> provider** (ADR-0060 to ADR-0065), the first banlieue component that runs
> **outside** the cluster, on the hypervisor host itself: **a new component,
> a new actor (a compromised VMM), two new assets and two new trust
> boundaries** (TB-8 host ↔ cluster, TB-9 guest VMM ↔ host and other
> guests), a new hardening requirement (§7.11) and five new accepted risks.
> It also corrects §1: the workspace is no longer `unsafe`-free — the new
> provider has **two audited `ioctl` blocks**, compiler-confined to one
> module. What is implemented and what is not is stated per row: ADR-0061,
> 0062, 0063 and ADR-0064 Decision 5 are live-verified; ADR-0060's
> `External` mode and token self-renewal, the rest of ADR-0064 and all of
> ADR-0065 are not built, and the rows that depend on them say so.
>
> **Amended 2026-09-26 for ADR-0060 Decisions 3–5 (External mode and token
> self-renewal), now implemented:** A-11, TB-8 and §7.11 updated; the §8
> "namespace-wide Role" entry narrowed to machines (Providers are now
> `resourceNames`-scoped); the "token not renewed" entry replaced by two new
> ones — a stolen token can renew itself (revocation: delete the
> ServiceAccount), and the operator's namespaced token grant.
>
> **Amended 2026-09-26 for ADR-0064 Decisions 1–3 (registry delivery),
> now implemented:** two components (the registry push Job, the host's
> image import unit), A-3 extended and a new asset A-13 (registry
> credentials), a new actor (the registry), a new boundary **TB-10** (build
> namespace → OCI registry → host) with its STRIDE table, rows added to
> TB-3 and TB-8, hardening item §7.13, and four §8 entries. Every section
> was re-walked; TB-1, TB-2, TB-4, TB-6, TB-7 and TB-9 are unchanged. A
> second pass the same night covers **Decision 4** (cache eviction, and the
> `banlieue.io/host-image-cache` finalizer, held by the controller so host
> tokens gain no `VMImage` write): two TB-8 rows, TB-10 and §8 updated.
> A third pass (2026-09-27) covers **ADR-0065's vTPM** (Decisions 1, 2, 6):
> two components (swtpm and its one-shot manufacture), A-6 and A-6a
> extended, the TB-8 vTPM row rewritten and four TB-9 rows added. The EK
> certificate is read host-side, from a provider-only directory, so this
> backend avoids libvirt's guest-reported residue. The same pass covers
> `Deferred` install and the vsock report (Decisions 3–5): the report is
> a new guest → host input path, with three TB-9 rows.
> A fourth pass (2026-09-27) covers **ADR-0063's amendment**: guests, their
> vTPMs and image imports are instances of **root-owned template units**,
> not transient units. The transient design let the provider's user start
> a unit as root, because polkit sees only a unit's name; that is closed
> (verified on a host) and recorded as two TB-9 rows. Components, A-12, the
> §5 diagram and the §8 "compromised provider" entry are updated. The
> decompressed size of a pulled image is now capped by a digest-covered
> annotation, which removes that §8 entry.
> A fifth pass (2026-09-27) covers **ADR-0065 Decision 3's amendment**,
> found by the first live `Deferred` run: the VMM, running as the guest,
> could not read the provider's image cache, so each machine now gets its
> own read-only copy of its installer, deleted after the eject. A-12 lists
> it and TB-9 gains one row; no component, actor or boundary changes, and
> §8 is unchanged. `Deferred` install and the vsock report are now
> verified live (`make ch-deferred-e2e`).
>
> The **2026-09-24** pass covered
> **ADR-0045** (the vTPM endorsement key certificate is published on the infra
> CR, mirrored onto a bound claim, and — for a `tpmEnabled` machine — gates
> `GuestReady`). A-6 is split so the EK certificate is its own asset; TB-4
> gains three rows for the guest-reported read path libvirt forces; §8 records
> the two residues it leaves — swtpm certificates that never expire, and a
> per-host local CA that is only as trustworthy as the host. The 09-24 pass
> re-walked every section after the vSphere half was brought level with
> libvirt: **no new component, actor, asset or trust boundary**, one control
> strengthened (the `GuestReady` gate now covers both backends, so a
> `tpmEnabled` vSphere member with no certificate is no longer bindable) and
> one durability defect closed (`patch_status_failed` no longer retracts the
> anchor under SSA). Nothing moved into §8 — both residues the 09-23 pass had
> flagged on vSphere were fixed rather than accepted. **banlieue
> acquires no new privilege in the guest: the certificate is read with
> ADR-0043's read-only `guest-file-open` path, never `guest-exec`.**
> A second 09-24 pass covers **ADR-0056** (`VirtualMachinePool.spec.addressing`
> gains a list of address-pool entries in place of one range): **no new
> component, actor, asset or trust boundary**. The entries are parsed
> entirely inside `banlieue-controller`, still tenant-namespace-scoped
> `VirtualMachinePool` spec data, and a malformed or wide entry (e.g. a `/0`
> CIDR) cannot be used to force unbounded work — `pool_plan::plan()` only
> ever walks as many addresses as `max_surge` lets it create in one pass,
> so the CIDR's declared size never drives iteration by itself.
> Against the architecture defined by ADR-0001 … ADR-0056, with ADR-0049 as
> amended 2026-09-27 (banlieue's side implemented; the attestation exchange
> itself lives in the external broker and agent). ADR-0057 … ADR-0059 are
> reserved-unwritten (roadmap 15); ADR-0060 … ADR-0065 are Proposed (roadmap
> 09) and outside this stamp.
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
small and well-controlled: no subprocess execution anywhere in the workspace,
a fuzzed libvirt wire decoder, and `unsafe` in exactly **two** places — the
tap and bridge `ioctl`s of the Cloud Hypervisor provider, which have no safe
wrapper in any crate. Both are in
`crates/banlieue-provider-cloud-hypervisor/src/sys.rs`, take their request
number from a closed enum so no request can be paired with the wrong
argument, and are confined there by `#![deny(unsafe_code)]` on the crate.
**The meaningful risk in banlieue is authorization and data-flow, not memory
corruption.**

The Cloud Hypervisor provider (ADR-0060) changes the first fact's shape
rather than adding to it. It holds **no hypervisor credential** — the
hypervisor is local — but it runs **on the hypervisor host**, holding a
**cluster** credential there, and it manages guests with host capabilities
(`CAP_NET_ADMIN`, `CAP_CHOWN`, `CAP_FOWNER`). Its two boundaries, TB-8 and
TB-9, are the host's relationship to the cluster and each guest's
relationship to the host.

## 2. Components

| Component | Identity | Namespace | Scope |
| --- | --- | --- | --- |
| `banlieue-controller` | `banlieue-controller` | `banlieue-system` | Watches `VirtualMachine`, schedules onto a `Provider`, creates provider infra CRs. Also runs the `VirtualMachinePool` loop (ADR-0046) and the `VirtualMachineClaim` loop (ADR-0047) — the latter binds a pool member to a subject and destroys it on release |
| `banlieue-operator` | `banlieue-operator` | `banlieue-system` | Provider lifecycle (ADR-0012): creates provider Deployments, ServiceAccounts, Roles, RoleBindings |
| `banlieue-provider-vsphere` / `-libvirt` | per-`Provider` SA | `banlieue-system` | Talks to the hypervisor; reconciles infra CRs (`VSphereMachine`, `LibvirtMachine` — ADR-0050) and realises them as real VMs/domains |
| `banlieue-provider-cloud-hypervisor` | ServiceAccount `banlieue-provider-cloud-hypervisor` (a bound token in a kubeconfig **on the host**); host user `banlieue` | **Not in the cluster** — a systemd service on the KVM host; its Provider lives in `banlieue-system` | Watches its own `Provider`, `CloudHypervisorMachine`s and `VMImage`s over the API; creates taps, disks, seeds and one transient systemd unit per guest (ADR-0060, ADR-0063). Holds `CAP_NET_ADMIN`, `CAP_CHOWN`, `CAP_FOWNER` and nothing else — `deploy/provider-cloud-hypervisor/host/banlieue-provider-cloud-hypervisor.service` |
| Cloud Hypervisor VMM, one per guest | Its own host uid **and private gid** (`banlieue-g<uid>`, registered in `/etc/userdb`), plus `kvm` | The host, as `banlieue-ch@<guest uid>.service`, an instance of a root-owned template | Runs one guest. No capabilities; seccomp and Landlock on; systemd sandbox — `deploy/provider-cloud-hypervisor/host/banlieue-ch-guest.example.service` |
| `banlieue-imagebuilder` | `banlieue-imagebuilder` | `banlieue-system` | Drives kairos `OSArtifact` builds; merges cloud-config (ADR-0037) |
| swtpm, one per `tpmEnabled` Cloud Hypervisor guest | The guest's uid and private group | The host, as `banlieue-swtpm@<guest uid>.service` | Emulates the guest's TPM from state in `/var/lib/banlieue/tpm/<guest uid>/` (ADR-0065) — `plan.rs::swtpm_unit` |
| vTPM manufacture, once per guest | Host user `banlieue` | The host, as `banlieue-swtpm-setup@<guest uid>.service` | Runs `swtpm_setup`: creates the TPM and signs its EK and platform certificates with the host's `swtpm_localca` key — `plan.rs::swtpm_setup_unit` |
| per-zone import Job | `banlieue-import` | `banlieue-imagebuild` | Uploads a built ISO to a datastore, creates a template (ADR-0020) |
| registry push Job | **None** — `automountServiceAccountToken: false` | `banlieue-imagebuild` | Pushes a `Ready` build to the operator's OCI registry for host-resident providers (ADR-0064); reads the artifacts PVC read-only and the push Secret, nothing else — `crates/banlieue-imagebuilder/src/reconciler/push.rs` |
| image import unit (Cloud Hypervisor) | Host user `banlieue` | The host, as `banlieue-ch-import@<vmimage uid>.service` | Pulls one image by digest into the storage classes' image caches and exits; sandboxed, writable only in `images/` — `crates/banlieue-provider-cloud-hypervisor/src/vmimage.rs` (`import_unit`), `import.rs` |
| kairos build pod | kairos-operator's SA | `banlieue-imagebuild` | **Privileged** — loop devices, mount, chroot |
| `banlieue host` (installer) | **root**, once, at an operator's request; no cluster identity | A Cloud Hypervisor host, as a command, never a service | Installs the pinned VMM and firmware, the `banlieue` user, guest uid records, directories, the host config, the EK CA, the polkit rule and the units (ADR-0067). A separate crate no provider depends on — `crates/banlieue-host/` |

## 3. Assets

| ID | Asset | Where it lives | Impact if lost |
| --- | --- | --- | --- |
| A-1 | Hypervisor credentials — vCenter username/password, or a libvirt **mTLS client key** | Secret named by `Provider.spec.connection.credentialsRef` | **Critical** — full virtualization-layer compromise, independent of Kubernetes |
| A-2 | Guest bootstrap material (cloud-config, SSH keys, join tokens) | Secrets/ConfigMaps → `VSphereMachine.spec.userData` / `LibvirtMachine.spec.userData` / `CloudHypervisorMachine.spec.userData` → `guestinfo.userdata` or a NoCloud ISO (on Cloud Hypervisor, `seed.iso` in the machine directory on the host) → built image | High — guest compromise, lateral movement into provisioned fleet |
| A-3 | VM image artifacts (ISO / raw disk) | `OSArtifact` PVC, then a vSphere datastore under `banlieue-images/`; on Cloud Hypervisor, the operator's **OCI registry** (a single-layer artifact, addressed by digest) and then each storage class's image cache on the host (`<storage class>/images/sha256-<hex>.raw`, `0750 banlieue`), or a file an admin placed there (ADR-0064) | **Critical** — a tampered image compromises every VM built from it |
| A-4 | Integrity of the control plane's own decisions | `Provider`, `ProviderClass`, `VMImage`, `VMClass` CRs | High — a forged `Provider` redirects credentials; a forged `VMImage` redirects the fleet's boot media |
| A-5 | Released binaries and container images | GHCR, GitHub Releases | **Critical** — downstream supply-chain compromise |
| A-9 | **The subject's own credential (a JWT)** — the thing a sandbox is handed so it can act as its subject | **Never in banlieue.** Broker → in-guest agent over mTLS, after attestation (ADR-0049). Not on a disk, not in a CR, not in a hypervisor channel | **Critical** — it *is* the subject's identity. Kept out of banlieue entirely, which is why no banlieue compromise discloses it |
| A-8 | **Guest readiness marker** — a value the guest writes and the provider reads back: `/run/banlieue/phase` on libvirt, `guestinfo.banlieue.phase` on vSphere | guest tmpfs → `qemu-guest-agent` → `LibvirtMachine.status.guestInstalled`, or guest `vmware-rpctool` → `config.extraConfig` → `VSphereMachineStatus.guestInstalled` (ADR-0043; both transports verified against a real backend) | Low on its own, but it gates pool membership: a guest that can assert it early gets handed out early. **Not an integrity signal** — see §6/TB-4 and §8 |
| A-7 | **Claim bindings** — which subject was given which VM, and when | `VirtualMachineClaim.spec.subject` + `status`, mirrored onto the member as `banlieue.io/claim-subject-*` annotations (ADR-0047) | Medium — discloses who was using which sandbox to every reader of the namespace; a *forged* binding makes the record say someone requested a VM they never asked for |
| A-10 | **The consumer's cached Kubernetes credential** — the ID token `kubectl oidc-login` writes to disk after a browser flow | `~/.kube/cache/oidc-login` on the consumer's own machine, outside every boundary below | High — it authenticates as that consumer, so it can create claims *attributed to them*. banlieue has no control here; see §8 |
| A-11 | **The Cloud Hypervisor provider's cluster credential** — a bound ServiceAccount token, renewed by the provider itself at half-life | `/etc/banlieue/credentials/token` on the host, beside a kubeconfig that only points at it; directory `0700 banlieue` (ADR-0060 Decision 5) | High — it is the provider's cluster identity: its own `Provider` and status, every `CloudHypervisorMachine` in the namespace, and **minting further tokens for itself**. **No Secret access** — the operator-built Role (`crates/banlieue-operator/src/workload.rs`) and `deploy/provider-cloud-hypervisor/rbac/clusterrole.yaml` — so it discloses no other credential |
| A-12 | **Guest disks, seeds and API sockets on a Cloud Hypervisor host** | `<storage class>/<machine uid>/` (`os.raw`, `seed.iso`, `serial.log`, and `install.iso` while a `Deferred` installer is attached) and `/run/banlieue/ch/<guest uid>/api.sock`, each directory `2770 guest-uid:banlieue` | High — a guest's whole disk, its rendered user-data (A-2) in the seed, and control of its VMM. Encrypted at rest only for a `tpmEnabled` machine installed `Deferred` (ADR-0065), which seals to its own vTPM; otherwise plaintext, and ADR-0048 refuses `tpmEnabled` with an `Immediate` image |
| A-13 | **Registry credentials** (ADR-0064) — push: a `kubernetes.io/basic-auth` Secret in `banlieue-imagebuild`; pull: `username`/`password` files in the host's `[registry] credentials_dir` (`0750 root:banlieue`) | Build namespace; each Cloud Hypervisor host | Push: **High** — combined with a `VMImage` status write, it chooses what hosts boot (TB-10). Pull: Medium — reads every pushed image, including cloud-config baked into it (A-2) |
| A-14 | **What makes a Cloud Hypervisor host safe to run guests on** — the VMM and firmware, the template units, the polkit rule, the host config, the userdb records and directory modes | `/opt/banlieue/`, `/etc/systemd/system/`, `/etc/polkit-1/rules.d/`, `/etc/banlieue/`, `/etc/userdb/`, placed by `banlieue host install` (ADR-0067) | **Critical** — a tampered unit or polkit rule is root on the host, a tampered VMM runs every guest; integrity comes from the banlieue binary (A-5) and the sha256 pins compiled into it |
| A-6 | vTPM identity and sealed disk-encryption keys | vSphere VM, per-clone (ADR-0039/0040); on libvirt, **swtpm state keyed by domain UUID** (ADR-0050); on Cloud Hypervisor, swtpm state in `<storage class>/<machine uid>/tpm/`, owned by the guest's uid, deleted with the machine (ADR-0065) | High — a shared or surviving TPM identity breaks per-VM disk-encryption isolation |
| A-6a | **vTPM endorsement key certificate** — the public anchor an attestation quote is checked against | vCenter-issued and read host-side on vSphere; `swtpm_localca`-issued into the vTPM's NVRAM on libvirt, exported by the guest to `/run/banlieue/ek.pem` and mirrored to `VirtualMachineClaim.status` (ADR-0045); on Cloud Hypervisor, written by `swtpm_setup` at manufacture to `<state_root>/ek/<machine uid>/` (`0700 banlieue`) and read **host-side** (ADR-0065) | Low confidentiality — it is a **public key**, deliberately readable by every reader of the claim. Its value is *integrity of binding*: it must name the VM banlieue actually created, or ADR-0049 verifies a quote from the wrong machine |
| A-15 | **EK trust-anchor bundle** — the set of CA certificates trusted to issue this backend's vTPM EK certificates | `Provider.spec.attestation.ekTrustBundle` — inline PEM, or a ConfigMap/Secret it names in the Provider's namespace. Admin-supplied, never discovered; resolved by the **broker**, never by any banlieue identity (ADR-0049 Decision 10) | Zero confidentiality — public CA material. **High integrity**: a rogue CA added here makes every EK certificate that CA forges verify, defeating the whole attestation chain; and on libvirt *removing* a host's issuer is the revocation mechanism, so an entry an attacker can re-add is a revocation undone |

## 4. Actors

| Actor | Assumed capability | Trusted? |
| --- | --- | --- |
| Cluster admin | Full Kubernetes API | Yes — out of scope by policy (`SECURITY.md`) |
| Platform admin | Creates `ProviderClass`, `Provider`, `VMImage` | **Semi-trusted — must be treated as infrastructure-admin-equivalent** |
| Tenant / VM author | Creates `VirtualMachine` in a namespace | **Untrusted for confidentiality of A-1/A-2** — see §7 |
| Claim consumer / sandbox broker | Creates `VirtualMachineClaim`s, holding `create` on them in a namespace | **Bounded by admission**: `spec.subject.id` must equal the authenticated username unless the requester is a declared broker (§7.6). A broker is trusted for attribution by definition |
| Compromised controller pod | RCE inside one banlieue pod | Untrusted |
| **Sandbox workload** — the AI agent (or anything else) running inside a claimed VM, including one gone hostile via prompt injection | Arbitrary code as root inside its own guest: can write the readiness marker, present or withhold an EK certificate, answer on any port, read the still-attached NoCloud seed | **Untrusted, by design** — the VM boundary is the isolation model (ADR-0047), and every TB-4/TB-5 control involving the guest assumes it is hostile. What it cannot do from inside: escape its vTPM identity (the EK private key is the one thing it cannot substitute — TB-4), reach another member's claim, or make banlieue carry a credential to it (A-9) |
| Compromised hypervisor endpoint | Attacker-controlled host reachable at `spec.connection.endpoint` | Untrusted |
| **Compromised VMM** (Cloud Hypervisor) | A guest that has escaped into its VMM process: code execution as that guest's host uid, with `kvm` and write access to its own two directories | **Untrusted.** The unit's sandbox and the per-guest identity are what contain it (TB-9); the provider must never act on anything it can influence without checking |
| **Stolen host credential** (Cloud Hypervisor) | Holds A-11 — the provider's ServiceAccount token — without the host | Untrusted; bounded by the provider's namespaced RBAC (TB-8) and the token's lifetime |
| **OCI registry** and whoever operates it (ADR-0064) | Stores every pushed build; can read, withhold or delete one | **Untrusted for integrity** — hosts pull by digest and verify it, so the registry cannot substitute content. **Trusted for confidentiality and availability**: it sees every image in full (§7.13) |
| **Host operator running `banlieue host install`** | Root on the host, for one command | Trusted, like the hypervisor operator; the installer's controls protect the host **from the provider's user** during that run, not from the operator |
| External contributor | Opens a PR from a fork | Untrusted |
| **OIDC identity provider** (and any bridge in front of it, e.g. Dex for GitHub) | Mints the ID tokens the API server accepts, and therefore **decides what `request.userInfo.username` is** | **Semi-trusted, and entirely outside banlieue's control.** Every guarantee the claim-subject policy makes is downstream of this actor: banlieue checks `subject.id` against a username it did not derive. Compromise or misconfiguration here makes every claim attribution meaningless — §8 |
| Hypervisor operator | vCenter/libvirt privileges outside Kubernetes; root on a Cloud Hypervisor host | Semi-trusted — **can read datastores and storage pools banlieue writes to**, and on libvirt can read swtpm state on the host filesystem. On a Cloud Hypervisor host, root can read every guest's disk, seed and memory, and the provider's cluster token (A-11) |

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

  TB-10: push Job (banlieue-imagebuild, no SA token) ──▶ OCI registry
         ──▶ banlieue-ch-import@<V>.service on a KVM host (by digest, host-pinned repository)

  ┌──────────── KVM host running banlieue-provider-cloud-hypervisor ────────────┐
  │                                                                             │
  │  provider (user banlieue, CAP_NET_ADMIN/CHOWN/FOWNER) ──TB-8──▶ API server   │
  │     │  dials out with a bound SA token (A-11); nothing dials in             │
  │     │ D-Bus + polkit: start/stop/reset-failed root-owned template instances │
  │     ▼   for uids in the guest range only                                    │
  │  systemd ──▶ banlieue-ch@<A>.service          banlieue-ch@<B>.service        │
  │              VMM as guest A (uid:gid A)        VMM as guest B (uid:gid B)    │
  │              seccomp · Landlock · sandbox  ─ TB-9 ─  seccomp · Landlock      │
  │              <storage>/<A>/ 2770 A:banlieue   <storage>/<B>/ 2770 B:banlieue │
  │              tap A ─────────── host bridge ─────────── tap B                 │
  │                                                                             │
  │  banlieue host install (root, once) ──TB-11──▶ /opt, /etc, units, polkit,    │
  │     ▲    acts in banlieue-owned dirs by O_NOFOLLOW handle   userdb, EK CA    │
  │     └── upstream release assets, HTTPS, sha256-pinned in the binary          │
  └─────────────────────────────────────────────────────────────────────────────┘
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
| TB-8 | Cloud Hypervisor host ↔ cluster | A process on the hypervisor holds a cluster credential and acts on cluster state; cluster state tells a host what to run |
| TB-9 | Guest VMM → host and other guests | Guest code that escapes into its VMM runs on the host, next to the provider and every other guest |
| TB-10 | Build namespace → OCI registry → Cloud Hypervisor host | A build leaves the cluster for a registry the operator runs, and enters a host that cannot mount cluster storage (ADR-0064) |
| TB-11 | Installer (root, once) → Cloud Hypervisor host | A root process writes the host's trust base (A-14), in part inside directories the provider's user owns, from artifacts downloaded from upstream (ADR-0067) |

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
| **One claim binds two warm members**, so one subject drains the pool's warm capacity and holds a sandbox nobody tracks | D, E | Before picking, the reconciler looks in the fresh member list for a member already labelled for this claim and finishes that bind instead — `reconciler/claim.rs::pick`, `reconciler/claim_plan.rs::member_bound_to`. Found live on 2026-09-28 (a reconcile from a stale cached claim); regression test `tests/live_claim.rs::a_stale_copy_of_a_bound_claim_does_not_bind_a_second_member` against a real API server |
| **A released member is recycled to a second subject** | I | Release is always deletion: `claim_plan.rs::next_step` has no transition back to an unclaimed state, and `pool_plan`'s invariants 1–2 keep the pool from reclaiming a labelled member. The VM is the isolation boundary, so reuse is the one outcome the design must exclude (ADR-0047) |
| **A stale-image member is handed to a fresh claim** after the pool's `VMImage` moved on — an old userland, old agent, old patch level | T | Bounded, not prevented: `pick_member` prefers the freshest image revision and falls back to a stale one only when nothing fresh is `Ready` (`crates/banlieue-controller/src/reconciler/claim_plan.rs`); `spec.maxIdleSeconds` reaps members that sat unclaimed too long (`reconciler/pool_plan.rs`, invariant 5); the mandatory claim TTL bounds how long a stale member lives once bound; and a pool rollout replaces unclaimed stale members while holding `available ≥ warmReplicas` (ADR-0046). A claim that must never get a stale member is a claim the pool should refuse instead — not modelled today |
| **A claim attributes a sandbox to a subject that never requested one** | S, R | `banlieue-virtualmachineclaim-subject-authorization` VAP (ADR-0047 Decision 10): `spec.subject.id` must equal the authenticated username, `subject.issuer` must be in an operator allowlist, and `spec` is immutable so the check cannot be undone by a later patch — `deploy/admission/virtualmachineclaim-subject-authorization.yaml`. Declared brokers are exempt from the id check by design (§7.6) |
| A credential is written into `spec.subject`, which is world-readable in the namespace and copied onto the member | I | Partly controlled: `subject.id` must now equal the authenticated username, so it cannot be an arbitrary string, and `issuer` is allowlisted. Neither stops a determined author from putting a secret in a field shaped like a username — banlieue never reads it as a credential and never forwards it to a guest, but nothing rejects one. See §7.6, §7.10 |
| `delete virtualmachineclaims` destroys running VMs | D | Equivalent to `delete virtualmachines` by design — releasing a claim *is* destroying the sandbox. RBAC is the only control; §7.10 |
| User-influenced strings (`domainName`, `pool`, disk/volume names) injected into libvirt domain XML | T, E | Every value is escaped on the way in by `esc()` — all five XML entities, uniformly in text *and* attributes, so there is no context-dependent rule to get wrong — `crates/banlieue-provider-libvirt/src/xml/escape.rs`, applied throughout `xml/domain.rs`; both have dedicated `_tests.rs` |
| A rogue CA is slipped into `attestation.ekTrustBundle` (A-15) — on the `Provider` spec, or by editing the ConfigMap/Secret it references — so an attacker-forged EK certificate verifies | S, T | **Bounded, not prevented.** The `banlieue-provider-attestation-ektrustbundle` VAP validates *shape* (exactly one of inline/configMapRef/secretRef — `deploy/admission/provider-attestation-ektrustbundle.yaml`); content cannot be validated by banlieue, which has no idea which CAs are legitimate (ADR-0049 Decision 10 makes that an explicit admin assertion, the same posture as `capabilities.features`). `Provider` writes are platform-admin-only (§7.5), and the referenced object lives in the Provider's own namespace, out of tenant reach. Custody of that object is §7.14 |

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

Since ADR-0064 the **imagebuilder itself** creates a Job here: the registry
push Job. Its build-namespace Role therefore holds `jobs`
get/create/patch/delete and `pods` list (`deploy/imagebuilder/rbac/role.yaml`,
`crates/banlieue-operator/src/bootstrap.rs::build_cloud_config_role`). By the
statement above, that makes the imagebuilder's identity node-root-equivalent
in this namespace. It already was in effect — it creates `OSArtifact`s,
which kairos turns into privileged pods — but the grant is now direct. §8.
The push Job it creates runs with no ServiceAccount token, non-root, with a
read-only root filesystem and all capabilities dropped.

### TB-4 — Cluster → hypervisor

| Threat | Control |
| --- | --- |
| MITM / attacker-presented certificate | banlieue owns the `reqwest` client and honours `connection.caBundle` (ADR-0008); CA source validated by `banlieue-provider-cabundle-source` VAP |
| Hostile or unresponsive endpoint stalls every reconcile | 10 s connect / 120 s request timeouts on the vSphere client; timeouts on libvirt connect, recv, and `Session::send` |
| Malformed libvirt RPC frames | Wire decoder is continuously fuzzed (`crates/banlieue-libvirt/fuzz`, `.github/workflows/fuzz.yaml`, ClusterFuzzLite); ADR-0050's domain `decode_*` halves are pure and unit-tested against captured wire bytes, so they are in that fuzz surface too |
| A deleted VM leaves its sealed-key material behind (swtpm state, UEFI NVRAM varstore) | `domain_undefine` takes **no flags parameter** and unconditionally sends `MANAGED_SAVE\|NVRAM\|TPM` (ADR-0050 Decision 5) — the flag cannot be forgotten at a call site — `crates/banlieue-libvirt/src/procs.rs`, proven against a real libvirtd in `tests/live_libvirtd.rs`. Live since ADR-0050: `LibvirtMachine`'s finalizer calls it on every teardown — `crates/banlieue-provider-libvirt/src/machine_client.rs` (`undefine`), invoked from `reconciler/libvirtmachine.rs::finalize_backend`, which then verifies the domain is actually gone before deleting its volumes |
| Something other than the intended guest answers the broker's mTLS connection and receives the subject's token | S | The agent returns a **TPM quote over `status.nonce`**, verified against the EK certificate published on the claim (ADR-0049, ADR-0045). The vTPM is unique per VM by construction — on vSphere because deferred install never installs the golden template so each clone installs with its own vTPM (ADR-0040), on libvirt because swtpm state is keyed by domain UUID. **Half implemented since 2026-09-23**: ADR-0045 landed, so the anchor now exists on the claim (`status.tpmEndorsementCertificates`). A `tpmEnabled` member is unbindable until it publishes one **on both backends** — the gate lives in each reconciler's `GuestReady` (libvirt `build_status`, vSphere `status_with_observed_state`), withheld with reason `TpmEndorsementPending`. vSphere gained that gate when ADR-0043's vSphere transport landed and gave it a `GuestReady` to gate at all; before then a vSphere member with an empty list was held back by nothing. ADR-0049 is Accepted (2026-09-27) and banlieue's whole side now exists — the anchor on the claim *and* the admin-supplied issuer bundle (`Provider.spec.attestation.ekTrustBundle`, A-15) a verifier checks that anchor against — but nothing yet *performs* the verification: the broker and in-guest agent are deliberately not banlieue code (Decisions 2 and 9) and are not built |
| A token minted for a different service is presented to the agent and accepted | S | `aud` is the agent's **own configured audience** and is deliberately never read from the claim (ADR-0049 Decision 5) — otherwise whoever wrote the claim chooses the audience |
| A claim names an attacker-controlled issuer, so the agent fetches that attacker's JWKS and every forged token verifies | S, T | The issuer allowlist in `banlieue-virtualmachineclaim-subject-authorization` — added for audit honesty, and load-bearing here: `subject.issuer` is a CR field the agent is asked to trust as a key source (ADR-0049 Decision 7) |
| A guest asserts `GuestReady` while still installing, or a compromised guest asserts it to be handed out sooner | S, T | **Bounded, not prevented.** The marker is guarded on immucore's active/passive sentinels so the *live installer* cannot assert it (`examples/16-cloud-config-guest-phase.yaml`), but a guest that has already been compromised can write anything — on either transport: `vmware-rpctool info-set` is as available to a compromised vSphere guest as writing the marker file is on libvirt. This is why ADR-0043 Decision 9 states the signal is liveness, never integrity: it is not a control against a hostile guest, and the pool hands out fresh, unclaimed VMs. Integrity is ADR-0049's problem (§8) |
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

### TB-8 — Cloud Hypervisor host ↔ cluster

The provider runs on the hypervisor and dials the API server; nothing
dials the host (ADR-0060). Two directions matter: what a stolen host
credential can do in the cluster, and what cluster state can make the host
do.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A stolen host token reads Secrets or other credentials | I, E | The provider's identity has **no Secret or ConfigMap access at all**. It is the operator-built per-Provider `Role` of an External class (ADR-0060 Decision 3), which never emits a Secret rule without a `credentialsRef` and a `cloud-hypervisor` Provider may not have one — `crates/banlieue-operator/src/workload.rs` (`build_role`, `external_rules`), `deploy/admission/provider-connection.yaml`, and the provider's own `CredentialsNotAllowed` refusal (`provider.rs`) — plus a `ClusterRole` limited to reading `VMImage`s and patching their status, `deploy/provider-cloud-hypervisor/rbac/clusterrole.yaml` |
| A stolen host token mints or deletes machines | T, D | No `create` or `delete` on `cloudhypervisormachines`; the controller owns their lifecycle — same file |
| A stolen host token patches **another** host's `Provider` or its status | S, T | The External Role scopes `providers` and `providers/status` by `resourceNames` to the one Provider; the provider's watch filters on `metadata.name`, which `resourceNames` honours for `list`/`watch` — `workload.rs::external_rules`, tested in `workload_tests.rs` |
| A stolen host token patches **another host's machines** in the same namespace | T, D | **Not controlled** — machine names are unknowable when the Role is written, so `cloudhypervisormachines` is namespace-wide. §8 |
| A stolen token is renewed by the thief and never expires | S | **Revocable, not self-limiting.** The token can create tokens for its own ServiceAccount (that is how the host renews), so a thief who holds it can too. The control is revocation: bound tokens are tied to the ServiceAccount's UID, so deleting the ServiceAccount invalidates every token ever issued for it at once, and the operator recreates it for a fresh `banlieue bootstrap cloud-hypervisor-host` (§7.11). Token creation appears in the API server's audit log. §8 |
| A host's token outlives the host | S | Bound, 24 h by default, renewed only while the provider runs: a host down for longer than one lifetime comes back with a dead token and must be re-issued one — `crates/banlieue-provider-cloud-hypervisor/src/token.rs` |
| Cluster state chooses a host path or bridge | T, E | Machines name **classes**, never paths: the host resolves a class through its own `/etc/banlieue/cloud-hypervisor.toml`, which is `0640 root:banlieue` and read-only to the provider (`ProtectSystem=strict`) — `crates/banlieue-provider-cloud-hypervisor/src/host_config.rs`, `plan.rs`. An unknown class is refused (`PlanError::UnknownStorageClass`, `UnknownNetworkClass`) |
| Cluster state injects a path through an image name, MAC or machine UID | T, E | `plan.rs` accepts an image only as a plain file name (no `/`, no leading `.`), a MAC as six hex octets, and the machine UID as a canonical UUID before any of them reaches a path, unit name or tap name; tap names are derived, never taken from the spec |
| A machine for another host is realised here | T | `reconciler.rs::is_ours`: only machines whose `providerRef` names this host's Provider, in its namespace, are reconciled |
| Cluster state chooses **where a host downloads what it boots** — a `VMImage` status naming an attacker's registry | T, E | The host pulls only a **digest in the one repository its own config names** (`[registry] repository`); anything else is `ForeignReference` and never fetched — `vmimage.rs::check_reference`, re-checked by the import itself (`import.rs`). The cache file name is derived from the digest, never taken from the cluster. See TB-10 |
| Host paths leak into the cluster through status | I | The `Provider` failure domain and the `VMImage` row carry **class names only**; unit tests assert no `/` in either — `provider_tests.rs`, `vmimage_tests.rs` |
| A stolen host token holds a `VMImage` in `Terminating` by never reporting `Released`, or reports it falsely | D, R | The host token writes only its **own** `perProvider` row (it has `vmimages/status` patch, no `vmimages` write). Withholding blocks deletion of images that host held — removing that `Provider` releases it; a false `Released` only leaves a file on that host. The finalizer itself is the controller's (`crates/banlieue-controller/src/reconciler/vmimage.rs`, `HOST_CACHE_FINALIZER`), so no host token needed `VMImage` metadata write, which would also reach the spec |
| A cluster author asks for a vTPM or install media the host cannot provide | I | `vtpm` is advertised only when the host config has `[tpm]` and its swtpm binaries, setup configuration and CA certificate exist (`provider.rs::gather_facts`); `plan.rs` refuses `tpmEnabled` on a host without `[tpm]`. A `tpmEnabled` VM with an `Immediate` image is refused before scheduling (ADR-0048), so none boots unsealed while claiming otherwise |

### TB-9 — Guest VMM → host and other guests

Assume a guest has escaped into its VMM process. What contains it:

| Threat | STRIDE | Control |
| --- | --- | --- |
| The VMM reads or writes another guest's disk, seed or socket | I, T | Each guest runs as **its own uid and its own private group** (`User=<uid>`, `Group=<uid>`, registered in `/etc/userdb` by the bootstrap); machine and run directories are `2770 guest-uid:banlieue` under `0711` roots, and no guest is ever in the `banlieue` group; a uid is given to one machine at a time, under one lock, even when a pool creates members together — `plan.rs::vmm_unit`, `plan.rs::UidLedger`, `hostfs.rs::prepare_dirs`, `scripts/bootstrap-cloud-hypervisor-host.sh`. Live-verified: another user cannot even list a guest's directory |
| The VMM writes anywhere else on the host | T, E | `ProtectSystem=strict` with `ReadWritePaths=` its own two directories, `ProtectHome`, `PrivateTmp`, `NoNewPrivileges`; Cloud Hypervisor's own seccomp filters and Landlock, with Landlock widened only by one read-only rule per tap on `/sys/devices/virtual/net/<tap>` — `systemd.rs`, `banlieue-cloud-hypervisor/src/types.rs`; reference unit `deploy/provider-cloud-hypervisor/host/banlieue-ch-guest.example.service` |
| The VMM opens other devices | E | `DevicePolicy=closed` with only `/dev/kvm` and `/dev/net/tun` allowed |
| The VMM exhausts host memory or PIDs | D | `MemoryMax=` guest memory + 512 MiB, `TasksMax=1024` in the unit's cgroup |
| **The VMM plants a symlink** in its own directory so the provider's privileged `chown`/`chmod`/truncate lands on another guest's disk or the shared image | T, E | **Every privileged change the provider makes inside a guest's directory goes through a file handle, never a path**: disks and seeds are created `O_CREAT\|O_EXCL`, then grown, owned and re-moded through that handle (`fchown`, `fchmod`, `set_len`); directories are opened `O_NOFOLLOW\|O_DIRECTORY` before being re-owned; the seed is read `O_NOFOLLOW` — `hostfs.rs` (`own`, `ensure_os_disk`, `write_seed`, `prepare_dirs`), `sys.rs::clone_file`. Tests plant symlinks and assert their targets are untouched (`hostfs_tests.rs`, `sys_tests.rs`) |
| The VMM replaces its API socket so the provider talks to something else, or so the provider's `CAP_FOWNER` re-modes an arbitrary file | S, E | `hostfs::grant_api_socket` opens the path `O_PATH\|O_NOFOLLOW`, requires a **socket** owned by the guest uid in the `banlieue` group with no bits for others, and changes only that opened inode via `/proc/self/fd`; the VMM client re-checks owner and mode before every connect — `banlieue-cloud-hypervisor/src/socket.rs` |
| The VMM returns a malformed or huge API response to the provider | D, T | Typed decoding into bounded structs; errors are reported on the machine's status, never panics — `banlieue-cloud-hypervisor/src/wire.rs`. **Not fuzzed**, unlike the libvirt decoder. §8 |
| The guest (or its VMM) replaces its **EK certificate** with one it made, carrying the expected CN | S, T | The certificate is written by `swtpm_setup`, run by the provider's user, to `<state_root>/ek/<uid>/` (`0700 banlieue`), outside every directory a guest uid owns, and read from there — `plan.rs` (`TpmPlan::ek_dir`), `hostfs.rs::ek_certificates`. Nothing a guest sends is published as its EK |
| The guest deletes its TPM state to get a TPM **manufactured again**, this time with symlinks planted where `swtpm_setup` (run as the provider's user, which can read the CA key) will write | T, E | Whether a TPM was manufactured is the provider-only EK directory, not the guest-owned state: a machine with EK files is never manufactured again; lost state is an error (`hostfs.rs::tpm_manufactured`, `machine.rs::ensure_tpm`, tested in `machine_tests.rs`) |
| A symlink in the TPM state directory turns the provider's `CAP_CHOWN` onto another file when the state is handed to the guest | T, E | Each entry is opened `O_NOFOLLOW` relative to the directory handle and re-owned through that handle; anything but a regular file is refused — `hostfs.rs::adopt_tpm_state`, tested with a planted symlink in `hostfs_tests.rs` |
| A guest's swtpm reads another guest's TPM state, or the CA key | I, E | swtpm runs as the guest's own uid and private group, writing only its state directory and run directory (`plan.rs::swtpm_unit`); the CA key is `0600 banlieue` in a `0700` directory, and manufacture — the only step that reads it — runs as the provider's user (`plan.rs::swtpm_setup_unit`) |
| A guest floods or stalls the **vsock report** listener, or sends crafted bytes | D, T | Each connection is read with a 10 s timeout and a 16 KiB cap; only an exact `phase=installed` line counts, everything else is ignored, and nothing is ever sent back or executed — `report.rs` (`parse_report`, `Listeners`), tested in `report_tests.rs`. The listener is a Unix socket in the guest's own run directory, never a network port |
| A guest claims **another** guest's installation, or claims "installed" while its installer is still running | S | A report can only arrive on the socket in the reporting guest's own run directory, so it speaks for itself only. A guest that lies about itself makes only itself `GuestReady` early — the same bound as ADR-0043's marker (§8); with `tpmEnabled`, `GuestReady` also waits for the host-minted EK |
| A symlink planted at the report socket path turns the provider's `CAP_CHOWN` onto another file | T, E | The path is unlinked (a link, never its target) before binding; the bound socket is opened `O_PATH\|O_NOFOLLOW`, checked to be a socket the provider owns, and re-owned through `/proc/self/fd` — `report.rs::hand_to_guest`, tested with a planted symlink |
| The installer survives into the installed system and re-runs on a reboot | T, D | When the installed system reports, `vm.remove-device install` in the same pass, `installMediaDetached` sticky, every later start planned without it — `machine.rs`, `plan.rs::without_install_media`, `reconciler.rs`; `GuestReady` stays `False, InstallMediaAttached` until then |
| A guest's VMM reads the shared image cache, or another guest's installer, or follows a symlink the guest planted where its installer copy goes | I, T | The cache stays `0750 banlieue`, closed to guest uids; each `Deferred` machine gets its own copy in its own directory, `0440` owned by the guest, created `O_EXCL` under a temporary name and re-owned by handle; anything but a regular file at the path is removed, never followed, and the copy is deleted on the pass after the eject — `hostfs.rs::ensure_install_media`, tested with a planted symlink in `hostfs_tests.rs`; verified live by `tests/e2e_deferred.rs` |
| The VMM starts, stops or changes **other** systemd units | E | It holds no D-Bus authority; only the provider's user may manage units, through the polkit rule below — `deploy/provider-cloud-hypervisor/host/60-banlieue-cloud-hypervisor.rules` |
| **The provider's user starts a unit as root, or as a user outside the guest range** (a compromised provider process, or anyone holding its uid) | E | Units are instances of **root-owned templates** that fix `User=`, `ExecStart=` and the sandbox (`deploy/provider-cloud-hypervisor/host/*@.service`); the provider chooses only the instance. polkit allows start/stop/reset-failed only on those templates' instances, with the uid instance **inside the guest range** (so not `banlieue-ch@0`), and `set-property` only on VMM instances; systemd itself refuses a transient unit under a name with a unit file and refuses `User`/`ExecStart` changes through `set-property`. Tested by `scripts/test-cloud-hypervisor-polkit.js` (22 cases) and verified on a host as the `banlieue` user (ADR-0063, amended 2026-09-27). Previously the provider created transient units, which polkit could only check by name, so this was possible |
| The provider passes crafted arguments through a template's environment file | T, E | Only the two templates that run **as the provider's own user** read one, so nothing is gained; values are refused unless single safe words (no whitespace, quotes, `$`, `\\`), and reach `ExecStart` as `${NAME}` — `systemd.rs::EnvFile::render` |
| A guest spoofs its address, or another guest's, on the bridge | S | **Not controlled at L2**: the tap is a plain bridge port. The provider reports only complete neighbour entries for the guest's own MAC on its own bridge (`neigh.rs`), but a guest chooses what IP it answers for. §8 |
| A failed VMM loops silently, hiding a fault | R | Failed units are kept (`CollectMode=inactive`), their systemd result is reported as `Ready=False, VmmExited`, then cleared and retried with backoff — `machine.rs`, `systemd.rs::failure` |
| Deleting a machine leaves its disk, seed, tap or unit behind | I | `machine.rs::teardown` stops the unit, deletes the taps and removes both directories, then **verifies** each is gone before the finalizer is released (`hostfs::remove_machine`, `host.rs::remove_taps`) — live-verified repeatedly |

### TB-10 — Build namespace → OCI registry → Cloud Hypervisor host

A Cloud Hypervisor host cannot mount the artifacts PVC, so a build it needs
is pushed to an OCI registry and pulled by digest (ADR-0064). Integrity
rests on content addressing; confidentiality and availability rest on the
registry.

| Threat | STRIDE | Control |
| --- | --- | --- |
| The registry, or anyone with push access, substitutes an image's content | T | **Digest pinning end to end.** The push Job reports the manifest digest; the imagebuilder records the reference only if it is a digest in its configured repository (`push.rs::pushed_reference`); the host pulls that digest, verifies the manifest and the layer against their digests while streaming, and renames into place only on a match — `crates/banlieue-oci/src/client.rs` (`manifest`, `pull_file`). A moved tag changes nothing |
| A `VMImage` status writer points hosts at another registry or repository | T, E | Host-pinned repository — TB-8 |
| A `VMImage` status writer points hosts at **another digest in the same repository** | T | **RBAC only.** `vmimages/status` patch (the imagebuilder, every Cloud Hypervisor provider's token, cluster admins) together with push access to the repository chooses what hosts boot. Digest pinning gives integrity, not provenance; signature verification is ADR-0064's follow-up. §8 |
| A compromised push Job redirects hosts | T, S | It has no API credential at all (`automountServiceAccountToken: false`); its only output is a termination message the imagebuilder validates as above — `push.rs::build_push_job` |
| The push Job escalates or tampers with the build | E, T | Non-root, `readOnlyRootFilesystem`, `allowPrivilegeEscalation: false`, all capabilities dropped, PVC mounted **read-only**, an `emptyDir` for the compressed copy — `push.rs::build_push_job`, tested in `push_tests.rs` |
| The registry reads images, including cloud-config baked into them (A-2) | I | **Not controlled by banlieue.** Treat the registry as holding A-2 (§7.13); per-VM secrets belong in `VirtualMachine.spec.userData`, delivered per clone, not in a shared image — as TB-5 says for datastores |
| The registry withholds or deletes an image | D | The import unit fails, the row reports `ImportFailed` with systemd's reason, and the next reconcile retries. Images already cached, and running guests, are unaffected |
| An oversized or hostile layer exhausts the host | D | The layer is capped at its manifest-declared size while streaming, and so is the **decompressed** stream: the push records the uncompressed length as a layer annotation (`io.banlieue.disk.size`), covered by the manifest digest the host pins, and the pull refuses to write past it or to finish short of it — `crates/banlieue-oci/src/sparse.rs` (`with_limit`), `client.rs::pull_file`. Superseded pulls are evicted beyond `[registry] keep_unreferenced`, and a deleted image's file is removed (`vmimage.rs::eviction_candidates`, `release`), so steady-state growth is bounded by the images in use. §8 |
| Eviction deletes an image a guest depends on | D, T | Only digest-named pulls are candidates, never admin-placed files; a file is kept while this host's row of a live `VMImage` resolves to it or one of its machines names it (`referenced_files`). A running guest's OS disk is its own file (reflink or sparse copy), so removing the cache file cannot touch it |
| The import process writes outside the image cache | T, E | Its own unit, an instance of the root-owned `banlieue-ch-import@.service`, as the provider's user: `ProtectSystem=strict`, `ReadWritePaths=` only the storage classes' `images/`, `ProtectHome`, `PrivateTmp`, `NoNewPrivileges`, `DevicePolicy=closed` with no devices — the template file, checked by `systemd_tests.rs`. Writes go through a temporary name and a rename; a copy into a second class is created `O_EXCL` (`sys.rs::clone_file`) |
| Registry credentials leak | I | Push: a Secret in `banlieue-imagebuild`, mounted only into the push Job. Pull: files readable by `root:banlieue` only, never a cluster Secret, so ADR-0060's "no Secret reads" holds for the host (`host_config.rs`, bootstrap script) |

### TB-11 — Installer (root, once) → Cloud Hypervisor host

`banlieue host install` runs as root, at an operator's request, and places
everything A-14 names. It is the one piece of banlieue that is root by
design, so its controls are about two things: what it installs is what
banlieue pinned, and a provider's user that was compromised earlier cannot
use a later run as a lever.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A tampered or substituted VMM or firmware download | T | HTTPS only (rustls), and **sha256 pins compiled into the binary**: every artifact is fetched and verified before any is installed; on a mismatch nothing is written and the previous symlinks stay — `crates/banlieue-host/src/pins.rs`, `stages.rs::vmm`, tested (`a_pin_mismatch_installs_nothing`, `make ch-host-install-test` step 5) |
| The installed VMM drifts from the release the client is written against | T | One pinned release: `pins_tests.rs` asserts the installer's version equals `spec/PIN` and the client's version gate |
| The provider's user (compromised earlier) plants a symlink in a directory it owns — state root, EK CA directory, storage class — so the root installer chowns, chmods or writes another file | T, E | Every ownership and mode change acts on a handle opened `O_NOFOLLOW`; every write is a temporary file created `O_EXCL` beside its target and renamed over it; `mkdir` refuses anything but a directory — `real.rs`, tested with a planted symlink (`a_symlink_planted_where_a_directory_goes_is_refused`). Residual: §8 |
| The installer weakens a directory another package owns (polkit's `rules.d` is `root:polkitd`) | T | Directories the OS or a package owns are created when missing and otherwise never re-moded or re-owned — `stages.rs::shared_dir`, tested; found by a dry run on a host |
| A read-only verb changes a host | T | `preflight` and `status` take `ops::Probe`, which has no mutating methods; `selftest` removes its scratch directory; asserted in unit tests and in the container test (step 3) |
| A reconcile path reaches root installer code, or its subprocesses | E | Crate boundary: no provider crate depends on `banlieue-host` (`boundary_tests.rs`); the subcommand is behind the binary's `host` feature |
| A setting (flag, cloud-init environment) breaks out of the TOML or a unit file it is rendered into | T, E | Class names are `[a-z0-9-]`, values refuse whitespace and quotes; the host config is serialized from the provider's `HostConfig` and parsed back before it is written; a template placeholder left unfilled is an error — `settings.rs`, `render.rs`, tested |
| A hostile server makes the installer buffer without bound | D | Downloads stop at `fetch.rs::MAX_ARTIFACT_BYTES` before the digest check |

## 7. Deployment hardening requirements

These are properties of the **current** design that operators must enforce
themselves. They are not bugs; they are the trust model, and deploying against
a different assumption is unsafe.

1. **Every infra machine CR is a credential-bearing resource — restrict `get`
   on all of them.** `banlieue-controller` resolves `spec.userData` references
   and inlines the *rendered content* into the infra CR in plaintext
   (ADR-0025/ADR-0038) — `VSphereMachine.spec.userData` and, since ADR-0050,
   `LibvirtMachine.spec.userData` — and, since ADR-0062,
   `CloudHypervisorMachine.spec.userData` — all built by the same
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
11. **Cloud Hypervisor hosts are part of the trust base; prepare them with
    `banlieue host install` and keep them to it.** The provider on the host is only as
    contained as the host configuration around it
    (`docs/src/guides/cloud-hypervisor-host-systemd.md`):
    - Install with `banlieue host install` from a banlieue binary you
      verified (A-5: its signature and SBOM), since it is what places A-14
      (ADR-0067; the script is now an SSH wrapper around it), which
      registers per-guest uids and private groups, sets `0711`/`2770`
      directories, and installs the polkit rule and the sandboxed provider
      unit. A host set up by hand without the userdb records cannot start
      guests; one set up without the directory modes loses guest-to-guest
      isolation.
    - `/etc/banlieue/kubeconfig` is `0600 banlieue`; the host config is
      `0640 root:banlieue`. Nothing else on the host should read either.
    - The provider **renews its own token** (ADR-0060 Decision 5). To
      **revoke** a host — decommissioned, stolen disk, suspected compromise —
      delete its ServiceAccount; every token for it dies at once and the
      operator recreates the account. Alert on `serviceaccounts/token`
      creation for these accounts from anywhere but their host.
    - **Do not put two hosts' Providers in one namespace unless they are
      equally trusted** — each host's Role reaches every
      `CloudHypervisorMachine` in its namespace (§8). External Providers
      must live in the operator's install namespace, the only one where it
      holds the token grant.
    - Keep `nsswitch.conf`'s `passwd`/`group` lines including `systemd`;
      `preflight` refuses a host without it.
12. **Recommended audit rule:** alert on any `ClusterRoleBinding` created by the
   `banlieue-operator` identity whose `roleRef` is not `banlieue-provider-*`
   (accepted-risk monitoring for the operator's RBAC-minting capability).
13. **Treat the OCI registry used for Cloud Hypervisor images as holding
    guest bootstrap material** (ADR-0064, TB-10). Images can carry
    cloud-config baked in by `VMImage.spec.cloudConfigs`. Use a private
    repository with access control and TLS; give the imagebuilder a push
    credential scoped to that one repository, and hosts a **pull-only**
    credential. Push access to that repository, together with `VMImage`
    status write, decides what hosts boot: keep both lists short. Configure
    retention yourself — banlieue never deletes from the registry. On
    hosts, `keep_unreferenced` bounds superseded pulls; a host that is
    down holds deleted images in `Terminating` until it returns or its
    `Provider` is removed.
14. **Guard the EK trust bundle like the attestation root it is.**
    `Provider.spec.attestation.ekTrustBundle` (A-15) is the list of CAs whose
    EK certificates a verifier will believe, and banlieue validates its shape,
    never its content (ADR-0049 Decision 10). Whoever can edit the Provider,
    or the ConfigMap/Secret it references, can add a CA that vouches for a
    fake vTPM — and can *re-add* an issuer an administrator removed, which on
    libvirt undoes a revocation (§8). Keep the referenced object in the
    Provider's namespace under the same write discipline as the Provider
    itself (§7.5), and alert on changes to it — the same monitoring posture
    as the claim-subject policy's `brokers` list (§7.6). Populate it
    per backend: vCenter's issuing CA on vSphere; each host's
    `swtpm_localca` issuer certificate on libvirt, one entry per host.
15. **Put sandbox VMs on an encrypted storage class where the backend offers
    one** (roadmap 17 phase F). On vSphere, VM Encryption on the storage
    policy the sandbox `VMClass`'s `storageClass` maps to makes deleting the
    VM a cryptographic erase at the datastore layer as well — a second,
    independent layer under the guest's own TPM-sealed partitions, and the
    one that still holds for the disk regions Kairos never encrypts.
    banlieue does not configure this; it is a property of the datastore /
    storage policy the platform admin maps the class to.

## 8. Accepted risks

| Risk | Why accepted | Revisit when |
| --- | --- | --- |
| `banlieue-operator` holds the union of every permission it can delegate | Kubernetes escalation-prevention requires it; granting `escalate`/`bind` instead would be strictly worse. The ceiling is auditable in one file | A provider needs a materially more dangerous permission |
| `banlieue-imagebuild` runs `privileged` | kairos' builder genuinely requires loop devices and chroot; isolation is by namespace | kairos supports rootless builds |
| Rendered user-data is visible in `VSphereMachine.spec` **and `LibvirtMachine.spec`** | Single-tenant, single-namespace posture (ADR-0025). ADR-0042 closed the *escalation* (a principal reaching user-data it could not read); the *reflection* to anyone who can already `get` the infra CR is unchanged and deliberate, and ADR-0050 extends it to a second kind rather than introducing a new risk | A second tenant or namespace becomes real — ADR-0025's superseded per-VM Role design is the shape that scales |
| The controller's user-data Role is namespace-wide, not `resourceNames`-scoped | The names a validly admitted `VirtualMachine` may cite are unknowable when the manifest is written; authorization moves to admission, where the requesting identity still exists (ADR-0042). A compromise of the controller identity itself is still bounded only by the namespace | The install stops shipping `deploy/admission/`, or per-VM RBAC becomes tractable |
| A libvirt guest's TPM is **emulated by swtpm on the host**, so a host-root adversary can read the sealed-key material that a physical TPM would protect | This is the libvirt trust model, not a banlieue choice; the hypervisor operator is already semi-trusted (§4) and hypervisor compromise is out of scope (§9). EK trust anchors differ per backend, and since 2026-09-27 that is explicit rather than implicit: `Provider.spec.attestation.ekTrustBundle` (ADR-0049 Decision 10, A-15) names which issuers count, per backend, as an admin assertion | The attestation exchange itself ships (the broker and in-guest agent, outside banlieue), or a libvirt host is no longer operator-trusted |
| **swtpm EK certificates never expire** — the observed `notAfter` is `9999-12-31` — so validity-period checks are not a revocation mechanism on libvirt | Nothing banlieue controls: `swtpm_localca` issues them that way. Expiry would be a weak control regardless, since a sandbox's whole life is measured in minutes. Revocation on libvirt is removing the issuing host's CA from `ekTrustBundle` — a per-host decision an administrator now makes on a field that exists (ADR-0049 Decision 10, landed 2026-09-27) rather than one a certificate makes for them. §7.14 records the corollary: whoever can re-add an entry can undo a revocation | A verifier needs a per-certificate revocation story rather than per-host bundle membership |
| On libvirt the EK certificate is **reported by the guest**, not read from the hypervisor, because swtpm persists no host-side copy | Forced by swtpm's design, not chosen (ADR-0045): the certificate is loaded into the vTPM's NVRAM and the issuing temp directory is deleted, and no libvirt RPC exposes it. It is sound because an EK certificate is a public key — substituting another member's does not yield its private half, so ADR-0049's activation fails — and the subject-CN binding catches the substitution earlier still. The residue is that a guest can *withhold* its certificate, which denies only its own readiness | libvirt or swtpm grows a host-side read, or a member withholding its certificate becomes something worth distinguishing from one that is merely slow |
| `GuestReady` can be asserted by any code running as root inside the guest, so it proves which disk booted only for a guest that has not been compromised — true on both transports (`qemu-guest-agent` file read on libvirt, `config.extraConfig` on vSphere) | It is a *liveness* signal by construction (ADR-0043 Decision 9) and is consumed only to decide when a **fresh, unclaimed** VM joins a warm pool — before any subject has touched it. Treating it as integrity would be the error; the document and the ADR both say so explicitly | Attestation ships (ADR-0049), at which point a TPM quote over the claim nonce is the integrity signal and this one stays what it is |
| A **broker** both holds subject credentials and is the party that verifies TPM quotes, so its compromise is the design's worst case | Somebody has to hold the credential to deliver it, and somebody has to verify the quote; concentrating both in one audited component is preferable to spreading either. banlieue is deliberately not that component (ADR-0049 Decision 2), so a controller compromise discloses no subject credential | The broker is split into deliver/verify roles, or hardware-backed key custody becomes available to it |
| A declared **broker** may attribute a sandbox to any identity, so the audit trail is only as honest as the broker is | Handing sandboxes out on behalf of other people is a broker's entire purpose (roadmap phase C); a broker that could only name itself could not broker. The concentration is explicit, empty by default, and confined to one auditable ConfigMap (§7.6) rather than diffused across everyone holding `create` | A broker is compromised, or claims need per-request proof of the subject's consent rather than the broker's assertion |
| **The identity provider is trusted absolutely, and is outside banlieue** | The API server is the only thing that can attest a caller, and it attests whatever the configured issuer asserted. banlieue cannot verify an upstream IdP without becoming an IdP. The claim-subject policy is still worth having: it binds an attribution to *whatever* identity the cluster does authenticate, which is strictly better than free text | banlieue ever needs an attribution stronger than the cluster's own authentication — at which point the answer is per-request proof from the subject (a signed consent, or the attested channel of ADR-0049), not a better check on the caller |
| A consumer's **cached ID token** sits on their laptop (`~/.kube/cache/oidc-login`) and authenticates as them if stolen | Not specific to claims — every Kubernetes bearer token behaves this way, and client-side credential custody is out of scope (§9). Recorded because the claim-specific consequence is distinctive: the audit trail records the *victim* requesting a sandbox, which is exactly the fiction §6/TB-1 exists to prevent. Short token lifetimes are the issuer-side mitigation | Claims carry per-request proof of the subject's intent rather than only the caller's identity |
| `subject.issuer` is allowlisted but never *verified*: the API server does not reveal which issuer minted the caller's token | Nothing in Kubernetes can attest it, so an allowlist is the strongest available check — it stops a claim naming an issuer the site does not use, which is what would make the recorded attribution meaningless. The claim deliberately carries no token to verify (ADR-0047 Decision 9) | The in-guest agent's JWT validation lands (roadmap phase C), at which point the *guest* verifies issuer, audience and `oid` against the claim |
| An `installMode: Manual` image can claim a deferred install it does not perform, so a `tpmEnabled` VM built from it is unencrypted and says nothing | `Manual` exists precisely for builds banlieue does not drive (ADR-0040), so inspecting it is not possible without becoming its build system. ADR-0048 closes the case banlieue *can* see (`Immediate`, and an absent `template`) and fails closed there; `Manual` is a deliberate operator assertion, narrower than the blanket gap it replaced | A backend reports sealed-partition state back to banlieue, making the assertion verifiable rather than trusted |
| The NoCloud `cidata` seed stays attached after install, so a guest can read its own rendered user-data (A-2) from inside the sandbox | ADR-0044 Decision 4, deliberate and scoped: cloud-init re-reads its datasource on every boot, so removing the seed risks regressing per-boot modules in a way that needs its own live verification on a reboot — not a first boot. Ejecting the *installer* removes the build-time overlay shared across every VM, which is the broader exposure; what remains is each guest's own material, which that guest's workload could in principle obtain anyway | The seed eject is verified live across a reboot, or user-data delivery stops needing a persistent datasource |
| A stolen host token (A-11) can patch **any `CloudHypervisorMachine`** in its namespace — another host's included — clearing finalizers or rewriting status | Machine names are not known when the Role is written, so `resourceNames` cannot scope them. Providers and their status *are* scoped since External mode (2026-09-26). The token still cannot read Secrets or create or delete machines. Operators bound it by namespace (§7.11) | Per-host machine scoping becomes possible (a label-selector authorization, or one namespace per host) |
| A stolen host token can **renew itself**, so it does not expire on its own | Renewal by TokenRequest is what lets a host run unattended (ADR-0060 Decision 5), and the API cannot tell the host's request from a thief's. Revocation is immediate and total — delete the ServiceAccount (§7.11) — and every renewal is an audited API call | Token requests can be bound to a host attestation (a TPM-bound key), or the API server gains per-token revocation |
| `banlieue-operator` holds `serviceaccounts/token` `create` in its install namespace, so a compromised operator can mint tokens for **other** ServiceAccounts there, the controller's included | RBAC requires a grantor to hold what it grants, and each External provider's Role carries renewal on its own ServiceAccount. Held namespaced, never cluster-wide (`deploy/operator/rbac/role.yaml`; a test fails if the ClusterRole ever gains it). The operator already holds broader grants (SEC-007) | The operator stops needing to delegate token creation — e.g. the provider renews through a narrower API |
| Guest addresses reported on status come from the host's neighbour table, which **the guest influences**; the tap is a plain bridge port with no MAC/IP filtering | Same posture as a libvirt NAT network. Addresses are a convenience for consumers, not an identity — the same reasoning as `GuestReady` (ADR-0043 Decision 9). Static IPAM (ADR-0024) gives an address the guest did not choose | Bridge-level anti-spoofing (nftables `bridge` family) is added, or an address becomes load-bearing for authorization |
| A **compromised provider process** reaches every guest on its host: it can read their disks and reconfigure taps (`CAP_NET_ADMIN`) | Inherent in a host-resident hypervisor manager — something on the host has to own guests' storage and networking, and the alternative (root) is strictly worse. It holds three capabilities and no others, can manage only instances of its root-owned templates for uids in the guest range (so it cannot start anything as root), and writes only the run root, the storage classes and its own state directories | A split into a minimal privileged helper and an unprivileged reconciler becomes worth its complexity |
| Hosts verify an image's **integrity** (digest) but not its **provenance**: whoever can write `VMImage` status and push to the configured repository chooses what hosts boot | Digest pinning already defeats a registry that substitutes content, and the host pins the repository. Signing needs a key-management story of its own, which ADR-0064 defers to its own ADR (cosign/sigstore) | Signature verification on pull lands, or the repository is shared with parties outside the platform team |
| The imagebuilder's identity can **create Jobs in `banlieue-imagebuild`**, a privileged namespace, so its compromise is node-root-equivalent there | It already drove privileged builds through `OSArtifact`s; the push Job must run beside the PVC it reads. The Job it creates holds no API credential and no privilege | The push can run outside the privileged namespace (e.g. a restricted namespace with a read-only clone of the artifacts volume) |
| Images in the registry can carry cloud-config (A-2), readable by the registry's operator | The registry is the operator's, like the datastore in TB-5; banlieue cannot encrypt what a host must boot without a key-distribution scheme it does not have | Per-VM secrets move entirely out of shared images, or images are encrypted for their hosts |
| The Cloud Hypervisor API decoder (`banlieue-cloud-hypervisor`) is **not fuzzed** | The peer is a local VMM the provider started, not a network endpoint; decoding is into typed, bounded structs and failure is reported, not fatal. The libvirt decoder is fuzzed because its peer is remote | A fuzz target is added alongside the libvirt one, or the client ever talks to a VMM it did not start |
| The installer's `O_NOFOLLOW` protects only a path's **last component**: a directory the provider's user owns could be swapped for a symlink between the installer's check and its use, one level up | The window is one run of a root command started by an operator; the directories are created (and a symlink there refused) earlier in the same run; `openat2(RESOLVE_NO_SYMLINKS)` would forbid legitimate symlinks on the path, such as a storage class under a linked mount | A host where the provider's user is untrusted while an install runs; then resolve beneath each banlieue-owned root with `openat2(RESOLVE_BENEATH)` |
| Health endpoint binds `0.0.0.0` and returns a fixed `200` | Standard probe trade-off; carries no data | It ever reports real state |
| Provider condition messages are mirrored verbatim onto user-facing `VirtualMachine` status | Useful diagnostics; providers are in-tree | A third-party provider ships |

## 9. Out of scope

- Anything requiring cluster-admin as a starting position (`SECURITY.md` policy).
- Compromise of the hypervisor itself, or of vCenter/libvirt authorization —
  including root on a Cloud Hypervisor host, which owns every guest on it.
- Guest-OS hardening after boot; banlieue's responsibility ends at delivering
  the bootstrap material.
- `deploy/kind/` — development-only, not held to production standard.
- The MkDocs documentation toolchain (`docs/`), which ships nothing at runtime.
- Removing banlieue from a Cloud Hypervisor host (`uninstall`, host drain):
  not built (ADR-0067 Decision 8); `banlieue host status` shows what a
  manual removal must cover.

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
