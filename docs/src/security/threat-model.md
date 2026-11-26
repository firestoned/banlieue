<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Threat Model

> **Status:** Living document. Last full pass **2026-11-26**, against the
> architecture defined by **ADR-0001 … ADR-0139, ADR-0144 to ADR-0146, ADR-0157 to ADR-0165, ADR-0170 and ADR-0171**, and, for **ADR-0137, 0142
> and 0143, the parts that are in the tree**: the served `FirecrackerMachine` API and `OciArtifact` image kind, `hostCa`,
> the `attestationSubject` mirror to the `VirtualMachine` and the claim, and two library modules nothing calls.
> For **ADR-0160, 0162 and 0165, the parts that are in the tree**: the explicit instance-size mapping (not 0160
> Decision 3's disk-controller check against the image), the cloud install-mode admission policy and the
> `Template` (gallery image version) source, and, since the pass of 2026-11-26, **0162 as amended that day
> (Decisions 1 to 8), implemented in part**: the publish Job and its identity, and the provider's orchestration of it
> (publishing has not been run against Azure; the runtime path has), and Trusted Launch on a `tpmEnabled` class (not 0165 Decision 1: no AK or EK certificate is published for an
> Azure machine). **ADR-0166** (Proposed 2026-11-19, no code) is not covered.
> **Not covered, and the stamp makes no claim about them:** ADR-0136 (VFIO passthrough), ADR-0138, 0140 and
> 0141 (Firecracker host supervision and the shared host layer: no code), and the unbuilt rest of 0137, 0142
> and 0143 (there is still no Firecracker provider, host installer verb or image pull on a host; 0143's attestation
> CA and guest signer are the design 0146 reuses and implements for Cloud Hypervisor, in a crate a Firecracker
> provider would share). Each is Proposed, or Accepted without an implementation of the part named, so no pass is due for it
> until the work that implements it lands with its own full pass (**0146** Proposed 2026-11-19 and amended 2026-11-20 (Decisions 7 and 8: a CA signed by an upstream PKI, by CSR, and signed by OpenBao in one run), its **host half implemented in code**: a Cloud Hypervisor host may opt in to a per-host attestation CA and a per-guest certificate signer on the guest's own vsock, so a guest can attest with SPIRE's `x509pop` beside the vTPM path; the **guest helper that asks for the certificate is not built** (mediatore's, its ADR-0049), so nothing attests this way yet; one new component, three assets (A-24 to A-26), one new boundary TB-21, and a new trust-anchor class: §1, §2, §3, §4, §5, §6 (TB-8, TB-9, TB-11, TB-21), §7.33, §8, §9; **0144** Accepted 2026-11-19 and implemented:
> a Cloud Hypervisor host serves sandboxes from an isolated, routeless segment, behind two egress gateway VMs that reach the
> outside only through the host's own address, by NAT on a second, uplink bridge. A network class may be
> declared `isolate_ports`, and the provider then sets kernel port isolation on each of its taps with one
> netlink request; the host rules (a rules file and a unit) are an operator recipe the binary does not install, and
> `preflight` refuses an isolated class unless the three rule chains exist with their jumps first (it does not read
> their contents); gateway-side neighbour pinning and gateway failover
> are not built: two new boundaries TB-19 and TB-20 and TB-18's Cloud Hypervisor half, §1, §2, §3 (A-23), §4,
> §5, §7.32, §8, §9; **0139** Accepted and implemented 2026-11-18, phase 1, the installer only: a host without systemd userdb (before 245) registers guest accounts in its own passwd and group databases with its own `useradd`/`groupadd` (`--guest-accounts files`), never by hand-editing them and never touching `nsswitch.conf`; the installer refuses `--guest-cpus` where cgroup v1 would silently not enforce it, adds `--guest-cpu-quota`, and on a systemd older than a unit directive's version (today `PrivateIPC=`, 248) refuses unless `--accept-reduced-hardening` names exactly the set the host lacks, recorded in the host config, where a re-run without the flag keeps it only for the directives the host still lacks, and a flag replaces it (it is checked against the host's facts, never trusted): §2, A-14, TB-9, TB-11, §7.11, §8. **0137 and 0138 have nothing implemented, so no pass is due for them**; publishing the accepted list in `Provider.status` is a follow-up of 0139 and not yet implemented; **0135** Accepted and implemented 2026-11-17: a claim ranks Ready members by an optional, immutable `spec.selection.strategy` (`LongestReady`, `MostAvailable`, `Spread`), and under every strategy takes members on a cordoned, `Stale` or unknown host last, reading `Provider`s it already could: TB-1, A-7; **0134** Accepted and implemented 2026-11-16: a Cloud Hypervisor host serves tenant namespaces too, through a second, opt-in ClusterRole bound only for a `Provider` with a `namespaceSelector`, and reads that selector from its own `Provider` at startup: §2, A-11, TB-8, §7.31, §8; **0133** Accepted and implemented 2026-11-16 for the vSphere, libvirt and Proxmox providers: one shared provider in the platform namespace serves tenant namespaces chosen by label and keeps the hypervisor credentials in its own namespace; a machine names its provider by name and namespace, a field only the controller may set (admission policy); `Provider.spec.namespaceSelector` is set only by a holder of `providers/namespaceselector`: §2, A-1, §4, TB-1, §5, §7.31, §8, §9; **0063** Decision 6 amended 2026-11-15 and 2026-11-16, Decision 3 amended 2026-11-16: the CH provider sees `/dev`, `/sys` and `/proc` read-only, without `ProtectKernelTunables=`/`ProtectControlGroups=` (on systemd 257 both re-mount `/dev` and `/sys` writable), leaves the `kvm` group, and is restarted by `install` when its unit or binary changes; every guest template gets a read-only `/dev` and a private IPC namespace: §2, A-14, §4, TB-9, TB-10, TB-11, §7.11, §8; **0132** Accepted and implemented 2026-11-16: a claim takes `spec.poolRef` or `spec.poolSelector`, exactly one, and draws from every matching pool in its own namespace; TB-1, A-7, §7.6; 0129 Accepted 2026-11-17, its milestone 1 implemented 2026-11-14 (CAPI bootstrap data); 0136 Proposed 2026-11-16 (PCI passthrough through VFIO), not implemented and outside this pass: its threats (a root grant on provider-written input, DMA, pinned memory, device state between tenants) are in the ADR, and the pass comes with its implementation; **0127** Decision 4 amended 2026-11-14: only `banlieue-controller` writes a `CloudHypervisorMachine`'s move fields, by admission policy: TB-8; **0131** Accepted and implemented 2026-11-15: the operator serves a label-selected set of namespaces and by default only its own, and the providers no longer default to every namespace: §2, §4, TB-1, §5, §7.30, §8; **0130** Accepted and implemented 2026-11-14: the controller serves a label-selected set of namespaces and by default only its own, never every namespace; selector mode adds a read-only `namespaces` grant: §2, §4, TB-1, §5, §7.30, §8; the CAPI bootstrap-data controllers follow the same scope; 0116–0122 reserved by roadmap 20, 0123
> Rejected, 0125 Accepted 2026-11-10; 0127 Accepted 2026-11-11, its API milestone only (types served, nothing
> acts on them); 0128 Accepted and
> implemented 2026-11-11; 0057–0059 unallocated,
> 0066 and 0068–0073 reserved; 0074 and 0075 Accepted 2026-09-28; 0076–0080
> reserved by roadmap 19; 0081–0082 recorded but not implemented in banlieue, and kept canonically in
> mediatore since ADR-0097; **ADR-0115** (2026-11-04: the imagebuilder's rebuild gate
> reads the artifacts PV and its node, two read-only cluster-scoped grants; a
> volume lost with a replaced node is rebuilt: §2, TB-3, §7, §8);
> **ADR-0101, amended 2026-11-04** (a vSphere pass decides from a live read of
> the machine, not the watch cache, and a recorded `vmRef` whose VM is gone is
> recovered by the ownership marker: TB-4 vSphere ownership, two rows, and the
> §8 marker risk);
> 0088 Accepted 2026-10-26, superseding 0048; 0108 Accepted 2026-11-05
> (proposed 2026-10-15); 0049 amended 2026-11-05 (SPIRE TPM node
> attestation supersedes Decision 4, `status.nonce` retired); 0114 amended
> 2026-11-06 (a static machine reports its declared address, labelled
> `addressSource: Static`, when nothing observes one; TB-1 row; no new
> component, identity, grant or boundary, §5 diagram unchanged); 0109 Accepted
> 2026-11-07 (the isolated sandbox segment: TB-18); 0108 and 0109 renumbered 2026-10-26 from duplicate 0100 and 0103; 0112
> Accepted 2026-10-28, renumbered 2026-10-30 from 0110, which main had
> meanwhile taken twice; 0113 Proposed, its tool, the provider's pacing and
> the imagebuilder's pre-lay Job implemented; 0087 amended 2026-11-01 and
> 2026-11-03; 0113 accepted 2026-11-02 and amended twice 2026-11-04; 0114
> Accepted 2026-11-04; 0054 amended 2026-11-04: every NoCloud seed carries
> `user-data`, an empty cloud-config when none is set, so cloud-init stops
> ignoring seeds without it; no row changes, the seed's contents and who
> controls them are as before). The pass of 2026-11-26 also covers **ADR-0171, the Azure guest layer**, implemented and unit tested, not run on Azure: every build of a `VMImage` with an `azure` source unpacks the digest-pinned `banlieue-azure-guest` bundle onto the image's root filesystem (`spec.artifacts.bundles`; `--azure-guest-image` must be `<repository>@sha256:<hex>`, `crates/banlieue-imagebuilder/src/azure_guest.rs`, `reconciler/vmimage.rs`) and merges a generated, asset-tag-gated cloud-config layer last; in the guest, the helper (`crates/banlieue-guest-azure`) writes customData from the local provisioning disk to `COS_OEM` (0600) and reports ready to WireServer over its host-local plain HTTP. New: one component (§2), the in-guest copy of A-2 (§3), WireServer on the §5 diagram, three TB-4 rows, a TB-6 row, §7.36 item (l), two §8 risks; no new cluster identity, grant or Secret. It also covers **ADR-0010 as amended that day**: `buildArtifact.checksum` is now measured by banlieue from the built file (a digest Job, `crates/banlieue-imagebuilder/src/reconciler/digest.rs`, running `banlieue imagebuilder digest`, `crates/banlieue-imagebuilder/src/file_digest.rs`), not copied from the source, and a declared checksum is verified by the imagebuilder before anything uses the build: one new component (§2), the SEC-004 chain re-stated (TB-10, TB-23), the §8 Job-creation risk widened to name it; no new identity, grant or boundary, §5 diagram unchanged (the Job is inside TB-3 like the push and pre-lay Jobs, and crosses nothing). The most recent pass, 2026-11-26, is **ADR-0162 as amended 2026-11-26 (Decisions 1 to 8), publishing a `VMImage` into an Azure Compute Gallery**, with ADR-0159 Decision 3 (a separate publisher identity). In the tree: the `banlieue provider azure image-publish` Job (`crates/banlieue-provider-azure/src/publish.rs`), which checks the build's SHA-256 before any network call, creates the image definition and an upload disk, takes a one-hour write SAS, uploads a fixed VHD by Put Page (`crates/banlieue-azure/src/upload.rs`, `crates/banlieue-diskformat/src/vhd.rs`) only to a host under the cloud's pinned upload suffix (`crates/banlieue-provider-azure/src/cloud.rs`), with no bearer token, revokes the SAS, snapshots, creates the version tagged with its owner and `banlieue-source-sha256` (`crates/banlieue-azure/src/gallery.rs`) and deletes its temporaries, and in delete mode refuses a version its owner did not tag; the provider's declared-state loop (`crates/banlieue-provider-azure/src/reconciler/{image,gallery,publish_job}.rs`), which creates those Jobs in the build namespace as `banlieue-azure-publisher` (no RBAC, no API token: `deploy/imagebuilder/rbac/serviceaccount-azure-publisher.yaml`), re-publishes a missing version, deletes superseded owned versions no `AzureVirtualMachine` references, and holds the finalizer `banlieue.io/vmimage-azure`; the publisher's own Azure role (`deploy/provider-azure/publisher-role-definition.json`) and NetworkPolicy (`deploy/network-policies/azure-publish.yaml`); the provider's ClusterRole gains `vmimages` `update`/`patch` (`deploy/provider-azure/rbac/clusterrole.yaml`), while its `jobs` and `pods` `list` are a `Role` in the build namespace only (`crates/banlieue-operator/src/workload.rs::build_jobs_role`), as the vSphere and libvirt providers' Job verbs now are too, and `deploy/admission/provider-jobs.yaml` confines what a provider identity's Jobs run; and the controller boots a `Url` image only from the chosen `Provider`'s ready row whose `sourceDigest` equals the build's checksum (`crates/banlieue-controller/src/reconciler/infra.rs`). **Publishing has not been run against Azure**; the runtime path has (2026-11-26: the management cluster's ServiceAccount issuer published for Entra ID, with the previous issuer still accepted by the API server; an Azure `Provider` running in-cluster; federation, a VM created, tagged and reported `GuestReady`). Every section was walked and every trigger question answered. New: TB-23 (build namespace to Azure gallery and upload host) with its §5 diagram block, table row and §6 table; one §2 row (the publish Job); A-30 (the publisher's identity and the write SAS) and A-31 (published image versions); one §4 actor (whoever can obtain the publisher's token); §7.36 (i) to (k); four §8 rows. Changed: §1's Azure paragraph; the §2 Azure provider row (it now creates Jobs); A-16 and A-29; the §4 resource-group writer and provider-token rows; the §5 TB-3 row; TB-3's prose; three TB-4 rows (Azure stolen token and image choice, and NetworkPolicy reach); §7.4; §7.21; the §8 Azure ownership-tag and NetworkPolicy rows; the §9 Azure bullet. Unchanged: §10. Trigger questions: new binary or Job yes (a Job mode of the existing binary); new CRD or user-controlled field yes (`spec.connection.azure.imageGallery` and `publisher` now drive a Job; `status.perProvider` gains `sourceDigest`, `minOsDiskSizeGiB` and `supersededRefs`, read back by the controller and the provider); new credential yes (the publisher's ARM token and a one-hour write SAS, both in the Job's memory only); new identity or RBAC grant yes (the `banlieue-azure-publisher` ServiceAccount, the publisher Azure role, and a cluster-wide Job grant on the Azure provider, §8); new namespace or PSA change no (the Job runs in the existing privileged build namespace with a restricted securityContext); new call out to an external API yes (the managed disk's blob upload host); shared storage yes (the Compute Gallery every Azure VM of a `Provider` boots from); new external dependency in the boot path no (the gallery replaces a hand-published `Template`); new boundary yes (TB-23); accepted risk weakened or invalidated: the tag row widened to the gallery's resource group and the NetworkPolicy row to the Job, four new; §7 assumption broken no, three added. The `unsafe` count stays at two: the new code is safe Rust over `reqwest` and `sha2`. The pass before it, also 2026-11-26, is **ADR-0170, post-quantum TLS**: every banlieue TLS client now uses one rustls provider, aws-lc-rs, built and installed by the new library `crates/banlieue-tls`, offering the hybrid group X25519MLKEM768 first with classical groups after it; `ring` is gone from the build and banned by `deny.toml`. No component gains an identity, a grant or a boundary, and the §5 diagram is unchanged (a library, not a process). Every section was walked and every trigger question answered. It adds one §2 row, one TB-4 row, one TB-6 row, updates §7.20 and rewrites the §8 risk "every TLS connection banlieue itself makes uses classical key exchange" into one limited to peers that do not accept ML-KEM, with the groups measured on 2026-11-26. The pass before, 2026-11-25, is **ADR-0158 to ADR-0165, the Azure provider**, with ADR-0158 Decisions 4 and 5 as amended that day (the `banlieue-owner-namespace` tag, and a provider serving only its home namespace sweeps only resources tagged with it). In the tree: the `banlieue provider azure` role of the binary (`crates/banlieue/src/cli.rs`, `crates/banlieue-provider-azure`) over the first-party client `crates/banlieue-azure`, an operator-rendered `Managed` workload per azure `Provider` with a projected ServiceAccount token and no Secret (`crates/banlieue-operator/src/{workload,bootstrap}.rs`), the `AzureVirtualMachine` and `AzureVirtualMachineTemplate` CRDs, the custom Azure role, ClusterRole and NetworkPolicy (`deploy/provider-azure/`, `deploy/network-policies/provider-azure.yaml`) and three admission changes (`provider-connection.yaml`, `machine-provider-ref.yaml`, the new `vmimage-cloud-install-mode.yaml`). Not built, and not claimed: 0160 Decision 3, 0162's publish Job and 0165 Decision 1 (above). Every section was walked and every trigger question answered. New: one §1 paragraph, one §2 row, A-29, three §4 actors (Resource Manager and Entra ID; a resource-group writer; whoever can obtain the provider's ServiceAccount token), the §5 Azure block, a TB-4 Azure section of sixteen rows, two TB-1 rows, one TB-2 row, one TB-15 row, §7.36, five §8 rows and one §9 bullet. Changed: A-1, A-2, A-6a and A-16; the §5 TB-4 table row; the TB-1 connection row and the TB-4 NetworkPolicy and tracing rows; §7.1 and §7.21; the §8 classical key exchange row (it now names the Azure links, and records that ADR-0159 Decision 5's offer of ML-KEM cannot be made while the process crypto provider is `ring`), the NetworkPolicy row and the Cluster API bootstrap-data row. Unchanged: §10. No new boundary number: Azure is TB-4 with a different credential, and the issuer trust it rests on is the cluster admin's. Trigger questions: new binary or controller yes; new CRD or user-controlled field yes (two CRDs; `spec.userData` reaches Azure as `customData`; `status.pendingOperation` is read back as a URL); new credential yes (A-29, and no Secret); new identity or RBAC grant yes (a ClusterRole with no Secret rule; the Azure kinds in the controller's, the operator's and CAPI's aggregate roles; an Azure custom role); new namespace or PSA change no; new call out to an external API yes (Resource Manager, Entra ID); shared storage no (each managed disk belongs to one VM and is deleted with it); new external dependency in the boot path yes (the published ServiceAccount issuer, outside banlieue); new boundary no, TB-4 widened; accepted risk weakened or invalidated: two widened (classical key exchange, NetworkPolicy reach), five new; §7 assumption broken no, one added (one resource group per `Provider`, §7.36). The `unsafe` count stays at two. Findings from this pass were reported to the maintainer privately and are not recorded here. The pass before it, 2026-11-24, is **ADR-0145, and the Firecracker code now in the tree**: walked on 2026-11-19 against that day's tree, and merged onto the 2026-11-20 and 2026-11-22 passes below on 2026-11-24, after checking that no file it cites has changed in substance since. **ADR-0145** (Proposed, implemented for the `VirtualMachine` controller): once a minute the controller lists its `VirtualMachine`s (metadata only, over its served scope, with the `list` it already holds), compares the API, its own cache and a ledger of the UIDs it has been invoked for, and after 300 s of objects it has not reconciled answers `503` on `/livez` so the kubelet restarts it (`crates/banlieue-provider-sdk/src/{audit,metrics,runner,health}.rs`; spawned after the leader election in `crates/banlieue-controller/src/app.rs`, so a standby never audits). No new component, identity, grant, namespace or boundary, and the §5 diagram is unchanged. **Firecracker**: since ADR-0146's pass, main gained the `FirecrackerMachine` and `FirecrackerMachineTemplate` CRDs (`crates/banlieue-api/src/infrastructure/firecracker_machine.rs`, `deploy/crds/`), the `OciArtifact` `VMImage` source kind (the existing providers report it unsupported: `crates/banlieue-provider-{vsphere,libvirt}/src/reconciler/vmimage.rs`), the Firecracker kinds in the CAPI aggregate role (`deploy/capi/clusterrole-aggregate.yaml`), the controller's mirror of `status.attestationSubject` from a machine to its `VirtualMachine` and on to a bound claim (`crates/banlieue-controller/src/reconciler/{status_mirror,claim}.rs`, which carries Cloud Hypervisor's subject under ADR-0146 too), the microvm artifact format (`crates/banlieue-oci/src/microvm.rs`) and a second attestation CA and signer in a new crate, `crates/banlieue-provider-firecracker`. **Nothing acts on a `FirecrackerMachine`**: no Firecracker provider exists, the controller neither watches the kind nor holds a grant on it (`deploy/controller/rbac/clusterrole.yaml`), nothing calls the microvm push or pull, and nothing links the new crate. Every section was walked. New: two §2 rows (the two libraries, as uncalled), the §2 controller row extended, two TB-1 rows (the subject on a claim; objects created to make the controller restart), two TB-14 rows, one TB-15 row, §7.35 and one §8 row. Changed: A-7 and the §9 Firecracker bullet. Unchanged: §1, §4, §5 (diagram and table) and §10. Trigger questions: new binary or controller no (an audit task inside the existing controller; two libraries nothing calls); new CRD or user-controlled field yes (two CRDs and an image kind, served and inert; `attestationSubject` on the VM and the claim, written only by the controller); new credential no (no binary creates the second CA); new identity or RBAC grant yes (CAPI's aggregate role reaches the Firecracker kinds, which nothing acts on); new namespace or PSA change no; new external call no; shared storage no; new boot-path dependency no; new boundary no; accepted risk weakened no, one new (liveness now depends on what the API server delivers); §7 assumption broken no. The `unsafe` count stays at two: `banlieue-provider-firecracker` is `#![deny(unsafe_code)]` and the audit and microvm code use none. The pass before it, 2026-11-22, is **ADR-0157** (Accepted and implemented): `banlieue-provider-credentialsref-authorization` checks a Provider's create and any update that changes its `spec.connection`, and admits `get` or a new custom verb, `reference`, on the named Secret, so a controller can name a credential it never reads (faubourg's) while a label change by anyone stays unchecked. Proven on a real API server (`crates/banlieue-controller/tests/live_credentialsref_policy.rs`, `make credentialsref-policy-live-test`). Every section walked: no new component, boundary or default grant (nobody holds `reference` unless an administrator grants it); A-1 notes the verb; the TB confused-deputy row rewritten; one accepted risk added. The pass before it, 2026-11-20, is **ADR-0146 Decisions 7 and 8**: the host's attestation CA may be signed by an upstream PKI by CSR (Decision 7), and the installer may have OpenBao sign that one request in a single run, logging in as the host's own ServiceAccount (Decision 8). Every section was walked and every trigger question answered (below). In code: `crates/banlieue-attestation-ca/src/pending.rs` (the pending key and request, and `import`, which checks a signed certificate against the pending key before installing it), `ca.rs` and `signer.rs` (the chain in the signer's reply, `ca-chain.pem`, the CA's expiry and a signer that refuses once it has passed), `crates/banlieue-host/src/{openbao,settings,stages,attestation}.rs` (the installer's OpenBao client and flags) and the `HostCaExpired` hold in `crates/banlieue-provider-cloud-hypervisor/src/provider.rs`. **The OpenBao side does not exist in this tree and is not claimed**: the pinned policy, the JWT role, the uniqueness check and the role's deletion are the OpenBao repository's onboarding script, specified in ADR-0146 Decision 8 and the guide and verified by nobody yet; each control below that lives there is marked, and the gaps are §8 rows with a *Revisit when*. New: one §2 row (OpenBao's node-attestation PKI mount, external), the installer and CA-crate rows extended, two §3 assets (A-27 the pending key and request, A-28 the right to have a CA signed), two §4 rows (the PKI operator; a compromised host as a repeated OpenBao login), TB-22 (installer to OpenBao) with its §5 diagram block, table row and §6 table, rows in TB-11 and TB-21, §7.34, eight §8 rows and one §9 bullet. Changed: A-24 and A-26 (a CA may now chain to an upstream root, so the bundle can hold one root, which is a global trust anchor: §8) and the host's ServiceAccount token, A-11 (it is now also an OpenBao login credential for the length of an onboarding). Trigger questions: new component yes (external and administrator-operated; nothing banlieue runs); new CRD or field no; new credential yes (an OpenBao token that lives five minutes and works once, never stored, and the pending key at rest); new identity or RBAC grant no in the cluster (the controller's and the host's Roles gain nothing), yes in OpenBao (a login role per host, outside this repository); new namespace or PSA change no; new call out to an external API yes (TB-22); shared storage no; new external dependency in the boot path no; new boundary yes; accepted risk weakened or invalidated: the unchecked name-uniqueness risk moves from the administrator to the onboarding script on the OpenBao route (banlieue still does not check it), new ones yes; a §7 assumption broken no. §10 unchanged. The `unsafe` count stays at two: the new code is safe Rust over `reqwest` and `rustix`. Gaps this pass found are §8 rows; none is an unremediated vulnerability in this tree. The pass before it, 2026-11-19, is **ADR-0146** as first implemented: a Cloud Hypervisor guest may attest with `x509pop`, using a certificate its host signs with an attestation CA the administrator asserts. Every section was walked and every trigger question answered (below). The host half exists in code: the CA, the signer and the request loop in the new library crate `crates/banlieue-attestation-ca`, the per-guest listener and the `HostCaMissing`/`HostCaUnreadable` gate in `crates/banlieue-provider-cloud-hypervisor/src/{attestation,provider,reconciler,machine,plan}.rs`, and the installer stage in `crates/banlieue-host/src/{attestation,stages,settings}.rs`. **The guest half does not exist**: no helper makes a key, sends a CSR or renews, so no guest attests with this path yet, and every control below is for a signer nothing yet calls in production. New: §2 three rows (the signer, the attestation CA crate, the guest helper as not built) and the installer row extended, A-24 (the CA private key at rest), A-25 (issued certificates), A-26 (the `hostCa` assertion, a trust-anchor class beside A-15), two §4 rows (a guest or its VMM at its own signer; a compromised provider or host as a certificate issuer), TB-21 with its §5 diagram block and table row, TB-21 rows in §6, two TB-11 rows and one TB-8 row, §7.33, §8 rows (nine, listed there) and one §9 bullet. Changed: the TB-9 swtpm row, A-6a and A-15 (the vTPM path is untouched, and is now one of two anchor classes), and the §4 Platform admin and Hypervisor operator rows (the first asserts each host's CA, the second can read its key). Trigger questions: new component yes; new CRD or field carrying user data yes (`attestationSubject`, a machine uid, public); new credential yes (the CA key, provider-only); new identity or RBAC grant no (the host token's Role gains nothing, and still reads no ConfigMap or Secret); new namespace or PSA change no; new call out to an external API no; shared storage no; new external dependency in the boot path no (two pure-Rust crates, no FFI, no subprocess); new boundary yes; accepted risk weakened or invalidated no (the EK-bundle risks are untouched), new ones yes; a §7 assumption broken no. §10 unchanged. The `unsafe` count stays at two: the new crate has none and the provider uses no new `unsafe`. **Findings from this pass were reported to the maintainer privately and are not recorded here.** The pass before it, also 2026-11-19, is **ADR-0144**: two internal segments on a Cloud Hypervisor host. The sandbox segment has no uplink and no host address, and each sandbox tap is isolated by the kernel (`crates/banlieue-provider-cloud-hypervisor/src/netlink.rs`, `sys.rs::ensure_tap_isolated`, from the class attribute in `host_config.rs`); the uplink segment carries the gateways and the host's own address, which the host masquerades (the recipe `docs/src/guides/cloud-hypervisor-sandbox-segment.md`). Every section was walked. New: TB-19 and TB-20 (with TB-18 pointing at them), two §2 rows, A-23, one §4 actor (another tenant of the host), the §5 diagram and table, §7.32, nine §8 rows and one §9 bullet. Changed: the TB-9 spoofing row (partly controlled on an isolated class, not on a plain one), the §8 compromised-provider row (it can also clear isolation), §1 (the provider's one new call adds no `unsafe`: it is `rustix`). Unchanged: §10, and the unsafe count of two. Every threat with no control in the tree is a §8 row with a *Revisit when*. The pass of 2026-11-17 is **ADR-0135**: a claim chooses among Ready members by strategy and by host health. No component, identity, grant, namespace or boundary changes, and the §5 diagram is unchanged: the claim reconciler reads `Provider`s with the lookup and the ClusterRole the scheduler already uses (`reconciler/provider_lookup.rs::candidate_providers`, `deploy/controller/rbac/clusterrole.yaml`), and reads subject annotations the bind already writes. TB-1 gains three rows (placement disclosed in the `Bound` message; a `Provider`'s cordon or heartbeat steering claims; forged subject annotations steering `Spread`), and two rows are updated (the stale-image row now names `rank_pick` and records that no preference outranks the revision; `spec.selection` joins the immutable fields). A-7 records the placement now in the claim's condition. §8 is unchanged: every new threat is bounded by an existing control, none accepted. The pass before it, 2026-11-16, is **ADR-0132**: a `VirtualMachineClaim` may select its pool by label instead of naming one. No component, identity, grant, namespace or boundary changes, and the §5 diagram is unchanged: the selector is matched only in the claim's own namespace, by the controller's existing namespaced reads. TB-1 gains two rows (pool labels steering a selector claim; the claim policy failing closed on a claim missing a pool field, a real defect caught by a live test before release and fixed with optional CEL comparisons), and its bind-race row now records that the `resourceVersion` precondition comes from the list the member was picked from (a fresh GET let two claims bind one member; regression test `live_claim.rs::two_claims_cannot_bind_the_same_member`). The pass before it, 2026-11-15, is **ADR-0129's milestone 1**, merged over ADR-0130 (its bootstrap-data controllers serve only the namespaces ADR-0130 resolves, and selector mode gates them like every other loop): banlieue-controller copies a Cluster API Machine's bootstrap Secret into its InfraMachine after classifying it, a new admission policy decides who may name one, and the Cloud Hypervisor provider holds a CAPI machine's guest until it is delivered. No new component, namespace or boundary: TB-15 gains the crossing (§5 diagram and table, twelve rows, two of them from the 2026-11-15 review: `configRef` authorized at admission, and the policies scoped to banlieue Machines; one of them the controller confirming that the Machine an InfraMachine's ownerReference names really owns it before it reads any Secret; three from the second review round the same day: an update switching `infrastructureRef` to banlieue is checked, a `VirtualMachine` labelled with a cluster's name is not held, and the reconciler starts whenever Cluster API is served, by watching one CRD, so neither a late install nor an API error leaves it off or stops the controller) and, by ADR-0096's 2026-11-14 amendment, KubeadmControlPlane's manager as a second holder of the aggregate role (§4 row, TB-15 row), TB-2 two rows (the controller's Secret reads stay namespaced; its ClusterRole gains only Machine reads and a read of the one Machine CRD, by name), §2 the controller and Cloud Hypervisor rows, §3 A-2's new source, §4 the Machine-author actor, §7 requirements 1 and 2, §8 five risks (three backends not yet served, an unlabelled hand-written machine, the un-scoped Secret read in a granted namespace, an expired join token, `get` on a bootstrap config treated as the right to name it), §1 and §9 one sentence each. The pass before it, 2026-11-14, is
> **ADR-0127 Decision 4, amended**: `deploy/admission/cloudhypervisormachine-move-fields.yaml` lets
> only `banlieue-controller` write a `CloudHypervisorMachine`'s move fields, proven on a real API
> server; the TB-8 row it answers now cites it. No component, identity, grant or boundary
> changes; §5 diagram unchanged. The pass before it, also 2026-11-14, is
> **ADR-0127 Decision 4, the VM's machine**: the VM reconciler addresses its infrastructure machine
> by `status.infrastructureRef.name` (else the VM's name) in every path, so a moved VM keeps its
> machine. One TB-1 row (a forged reference), bounded by the existing status RBAC. No component,
> identity, grant or boundary changes; §5 diagram unchanged. The pass before it, 2026-11-11, is
> **ADR-0127's API milestone**: the `VirtualMachineMove` CRD, the `banlieue.io/zone` key and the
> `CloudHypervisorMachine` fields `spec.identity`, `spec.moveIn`, `spec.moveOut` and their status
> halves are served, and nothing reads or acts on any of them: no controller, no provider code,
> and no role grants `virtualmachinemoves`. So no component, identity, grant or boundary
> changes and the §5 diagram is unchanged. One TB-1 row (who can request a move, and CEL
> immutability of `vmRef`) and one TB-8 row (a host token can write the new machine fields, which
> Stage 1 must close before anything acts on them). Stage 1's own full pass adds the host-to-host
> boundary and the zone as a trust unit. The pass before it, also 2026-11-11, is
> **ADR-0128**: every provider (vSphere, libvirt, Proxmox, Cloud Hypervisor) now publishes
> capacity, adding a measured `memoryAvailableMiB` and per-storage-class `storage[]`, and the
> scheduler fits a new placement against them, less in-flight VMs and a per-Provider
> `storageReservePercent`. The new backend calls are read-only (libvirt node info, node memory
> stats and pool info on the existing mTLS session; vSphere property reads under the
> `System.Read` every role carries, on objects the cluster assignment already propagates to;
> Proxmox fields the provider already fetched), so no identity, grant, boundary or §5 diagram
> change. A backend now steers placement through what it reports: one TB-4 row (false capacity,
> bounded to that Provider), one TB-5 row (a filling store, now stopped by the reserve), one
> TB-1 row (the reserve's bounds), the TB-8 false-capacity row widened, §7.27 extended, two §8
> risks revised (placement window, memory outside banlieue) and three added (QEMU overhead is an
> estimate, two classes on one store, vSphere figures are per cluster). The pass before it,
> 2026-11-10, is
> **ADR-0125**: a placed VM keeps its recorded placement while it still passes every filter
> (image readiness aside), so the spread no longer moves it to an emptier domain, and a VM no
> longer counts against itself for required anti-affinity. Both were silent recreates under the
> default `migrationPolicy`, losing a local disk: TB-1's placement row widens to cover them. No
> new component, identity, grant or boundary; §5 diagram unchanged. The pass before it,
> 2026-11-09, is
> **ADR-0126**: the operator writes the name of the ServiceAccount it created for a Provider into
> `status.workload.serviceAccountName`, so a consumer minting a host-resident provider's first
> token reads it instead of re-deriving banlieue's naming. The operator already creates that
> account and writes the Provider's status, so nothing is granted; the name is not a secret (it
> follows from the class and the Provider); a consumer that mints for it still needs its own
> `serviceaccounts/token` grant, which this does not give. No row changes. The pass before it,
> also 2026-11-09, is
> **ADR-0124 Decision 2's amendment**: when placement fails after the cordon
> removed a candidate, the scheduler re-runs the same pure function with the
> cordon ignored, and reports `ProviderUnschedulable` naming the cordoned
> Providers if that would have placed the VM. It changes a reason and a
> message, not who can place what: no actor, identity, grant, boundary or
> accepted risk changes. The second pass runs only on failure. The pass before
> it, 2026-11-08, is
> **ADR-0124**: a Provider labelled `banlieue.io/unschedulable: "true"` (a cordoned host) is
> dropped by the scheduler right after the selector and takes no new VM; VMs already on it are
> not moved and the provider keeps reconciling them. Implementing it found that ADR-0102's
> stale filter moved placed VMs after all (the scheduler re-runs for them, and drift under the
> default `migrationPolicy` recreates a VM); a VM's own Provider now stays a candidate through
> both filters, and ADR-0102 is amended. Three TB-17 rows. Setting the label takes
> `patch` on Providers, which already lets its holder change every label a selector matches,
> so no grant, identity, boundary or accepted risk changes. The pass before it, also 2026-11-08,
> is **ADR-0043 Decision 8's third amendment**: the libvirt provider polls a
> machine still expecting its guest's report every 2 s for its first 90 s
> (then 30 s, as before). The reads are the same read-only guest-agent calls
> over the same mutual-TLS libvirt session (TB-4), only sooner; the extra
> load on libvirtd is bounded per member (at most 45 probes) by a window
> counted from creation, which a guest cannot extend. No actor, identity,
> grant, boundary or accepted risk changes; nothing changes beyond the stamp.
> The pass before it, 2026-11-07, is
> **ADR-0109's acceptance**: a new boundary, **TB-18** (sandbox segment →
> egress gateway → routable network), in the §5 diagram and table; a §6
> section mapping its threats to banlieue's topology controls (no default
> route for members, egress first in the gateway class, ADR-0083) and to the
> host and image controls that are not banlieue's; §7.29 (a segment routes
> nowhere); two §8 risks (a gateway's image-set `sandbox` address is unseen
> by ADR-0083; the gateways' hardening is the image author's). The pass
> before it, 2026-11-06, is **ADR-0114's amendment, the declared address**: a TB-1 row for a declared
> address read as an observed one (labelled `Static`, replaced by any
> observation, ADR-0083 keeps it from naming another VM's address). The
> pass before it, 2026-11-05, is **ADR-0108's acceptance and ADR-0049's
> amendment**: the handshake the
> consumer built is SPIRE's TPM node attestation (a credential-activation
> challenge sealed to the vTPM's EK), not quote-over-nonce, so the controller
> stops minting `status.nonce`. The TB row on who receives the subject's
> token now names that control and banlieue's published anchors (EK
> certificate, `ekTrustBundle`); A-6a, the EK-mismatch row and the broker
> risk drop "quote"; §8 gains the ADR-0108 residual (a key that is not
> TPM-bound baked into a sandbox image), beside `Asserted`. Less is
> published, no grant, identity or boundary changes, and the §5 diagram
> stands. The pass before it is ADR-0115's (noted above); the one before
> that, the second of 2026-11-04, is **ADR-0114**: libvirt and Cloud Hypervisor now write a cloud-init
> `network-config` into the NoCloud seed for a static NIC, through the
> renderer Proxmox already used, and libvirt derives MACs for such a
> machine. A tenant's address, gateway, nameserver and domain strings now
> reach two more seeds as YAML: TB-1 gains a row (the renderer quotes every
> value; the author already controls the guest through user-data) and A-2
> names the file. No component, identity, grant or boundary changes and the
> §5 diagram stands. The pass before it, the first of 2026-11-04, covers three
> changes. **ADR-0113's second
> amendment of that day, a Grype gate for the pre-lay image**: `make
> grype-prelay` (the `grype-prelay` CI job) scans every pushed
> `banlieue-prelay` image, reports every finding to Code Scanning, and fails
> on a high or critical finding Debian has already fixed, with curated
> statements from `.vex/prelay/`; the release's asset upload waits for it.
> CI only: no component, actor, identity or boundary changes and the §5
> diagram stands. The TB-6 *Pre-lay image* and *Scanning* rows name the
> gate, and the §8 risk it replaces narrows to the findings Debian has not
> fixed. **ADR-0113's first amendment, a
> published pre-lay image**: CI publishes `ghcr.io/firestoned/banlieue-prelay`
> (Cosign-signed; provenance and SBOM on main and releases), the
> imagebuilder's `--prelay-image` defaults to it and `banlieue bootstrap
> imagebuilder` pins it with `--prelay-image-digest`. The privileged Job's
> code now comes from the project's supply chain instead of each operator's:
> A-5 and the §2 row name it, TB-6 gains a row, the TB-10 row's trust
> sentence changes, and §8 gained a risk (the image had no Grype gate) that
> the second amendment narrows. And
> **ADR-0087's amendment of 2026-11-03**: vSphere's `banlieue.io/force-reimport`
> is handled per `Provider`, each workload writing its own marker annotation
> on the `VMImage` and the last clearing the request. Those writes use the
> `VMImage` patch the workloads already held; whoever can forge a marker can
> already delete the request, so no row changes. The pass before it,
> 2026-11-02, is **ADR-0113's acceptance**: the pre-lay Job ran in a cluster. Its one new behaviour, the
> script making missing loop partition device nodes inside its privileged
> container (no udev there), adds no capability the Job did not already
> hold as a privileged pod (§4 row, TB-10 rows and the §8 risk stand), so no
> row changes; the failure path (`PrelayFailed`, no `file`) was observed
> live, as the TB-10 row says. The pass before it, 2026-11-01, is
> **ADR-0087's amendment**: a provider
> workload started for one `Provider` (as the operator deploys them)
> reconciles only that `Provider`'s `VMImage` rows, under its own field
> manager, on libvirt, Proxmox and vSphere, and vSphere's deletion becomes
> per-`Provider`. Found live as a v0.4.9 libvirt regression (workloads
> reading each other's Secrets, refused, and overwriting each other's rows),
> rolled back; vSphere (rows, and a delete that hung) and Proxmox (rows
> erased by a shared manager) were latent. It adds one TB-4 row and one §8
> risk (a deleted vSphere `Provider`'s templates are left in vCenter); no
> new identity, grant or boundary: it makes the workloads stay inside the
> grants they already had. The pass before it, 2026-11-01, is **ADR-0113's
> amendment**: the imagebuilder pre-lays a `Url`
> image when its `VMImage` sets `spec.prelay`, with a new privileged Job in
> the build namespace that boots the build once under QEMU/KVM and reports
> only a digest. It adds one §4 identity row, three TB-10 rows (a wrong
> digest, a QEMU escape, a rewritten build) and one §8 risk (a privileged
> Job running a guest on a build node); no new namespace, RBAC grant or
> boundary, since the namespace already runs privileged builds. The pass
> before it, 2026-10-31, is **ADR-0113**: pre-laid
> images, where the Kairos recovery reset every raw clone runs on first boot
> is done once at build time (`scripts/prelay-kairos-raw.sh`), and the Cloud
> Hypervisor provider requeues a VMM start step after 1 s. A pre-laid image is
> a disk artifact made by booting the image once, so the pass adds five TB-5
> rows: an image sealed to the builder, build-boot identity and logs, operator
> stages run at build time, a reset that never ran, and a host rewriting an
> artifact. Each maps to a check in the script or to ADR-0064's existing
> rule that the provider never writes the image cache. No actor, identity,
> RBAC grant, namespace or boundary is added (the build boot runs on an
> operator's machine, with no network and no TPM), so §4, §5, §7 and §8
> stand; the pacing change alters timing only. The pass before it,
> 2026-10-30, reconciles
> **ADR-0112** with ADR-0110 and ADR-0111, all three passed in parallel. They
> do not interact: ADR-0110 changes what the imagebuilder's push Job trusts
> (TB-10), ADR-0111 how many adapters a vSphere clone gets (its own TB-4
> table), and ADR-0112 what a Cloud Hypervisor guest reports over vsock (the
> TB-4 and TB-5 sealing rows, the TB-8 vTPM row). A sealing report is per
> guest, not per adapter, so a second vSphere NIC changes nothing it reads;
> nothing changes beyond the stamp. **ADR-0112's acceptance**, 2026-10-28,
> followed a live pool run on a Cloud Hypervisor host. The run found that a
> clone which never reports was marked `FirstBootSealingTimeout` up to one
> long requeue (300 s) late; the provider now re-checks every 30 s while a
> report is due. That tightens the halted-clone row; no actor, identity,
> grant, boundary or accepted risk changes. **ADR-0112** itself, 2026-10-27:
> first-boot sealing on Cloud Hypervisor. An `Immediate` clone there is the
> guest's own reflinked or copied `os.raw` with a fresh swtpm, so sealing
> never writes the cached image, and the guest reports its sealed partitions
> as a `sealed=` line on the ADR-0065 vsock report, parsed by the shared
> provider-SDK parser and gated through the same deadline as libvirt and
> vSphere. The scheduler no longer excludes Cloud Hypervisor. That pass
> widened A-8 and the two sealing-report rows to the vsock channel, rewrote
> the CH vTPM row and the §7 sealing requirement, and extended the
> halted-clone row; no actor, identity, RBAC grant or boundary changed, so §4
> and §5 stand. The pass on 2026-10-30 before these is **ADR-0111**: a cloned vSphere VM now gets
> **one NIC per declared interface**, where it used to get one. Nothing new is
> trusted and no identity, RBAC grant, namespace or boundary is added; what
> changes is that a second interface now *exists*, so a VM can sit on two
> segments. It adds one §2 clause, one A-4 clause, one TB-1 clause, a TB-4 table
> (five rows), §7.28 and two §8 risks, and a line in the §5 diagram. The control
> that matters is old and was already per interface: a tenant picks a
> `VMClass`, it cannot add a NIC (`networkOverrides` has no `networkClass`).
> The pass before it, 2026-10-29, was **ADR-0110**: the registry push Job can
> trust an operator-named CA bundle, for a registry served by a private CA.
> That is a new **trust anchor** in the one Job that holds a registry
> credential. It added one §2 sentence, five TB-10 rows, a clause in §7.13 and
> one §8 risk, and no actor, identity, RBAC grant or boundary. Before that, 2026-10-26, was **ADR-0088's acceptance**: Kairos v4.4.0 shipped first-boot
> sealing and the three kcrypt fixes its *Accept when* named, a raw
> `cloudImage` build was shown to carry the sealing layer onto `COS_OEM`
> (Open question 1), and `--enable-first-boot-sealing` now defaults on, with
> `=false` restoring ADR-0048. It rewrites the §2 controller row, A-12, two
> TB-5 rows, one TB-8 row and §7's sealing requirement, retires the §8
> "pre-release Kairos" risk in favour of a narrower one (an image older than
> v4.4.0 is caught only by the readback) and adds one §8 risk (blocks written
> to `COS_PERSISTENT` before sealing are not guaranteed wiped). No actor,
> identity, RBAC grant or boundary is added, so §4 and §5 are unchanged;
> Cloud Hypervisor stayed excluded until ADR-0112. The pass before that, 2026-10-24, was
> **ADR-0107**: the scheduler places a VM only
> where it fits the capacity a provider publishes on its failure domain
> (memory strictly, with a per-VM overhead; vCPUs up to
> `Provider.spec.scheduling.cpuOvercommitRatio`, default 4), and the Cloud
> Hypervisor provider publishes the smaller of the host's resources and its
> guests' slice budget, read from systemd. A provider's status now steers
> placement, so this pass adds two TB-8 rows (false capacity from a stolen
> host token, bounded to that host; fixed, read-only slice reads), one TB-1
> row (the ratio), §7.27 (one host, one Provider) and three §8 risks
> (placement is not a reservation; capacity is counted per Provider; memory
> outside banlieue's VMs is not deducted). No identity, RBAC grant, boundary
> or diagram change. The 2026-10-23 pass before it is **ADR-0106**: management-cluster nodes on Cloud
> Hypervisor run in `banlieue-nodes.slice`, beside ADR-0105's
> `banlieue-guests.slice`, so `banlieue.slice` holds every VM on a host and an
> operator can bound guests and nodes together with `systemctl set-property`;
> and `banlieue host cloud-hypervisor overhead`, a read-only verb, reports each
> VM's guest CPU against host-side overhead. It updates two §2 rows and the §5
> diagram, adds three TB-9 rows, §7.26 and one §8 risk (a compromised VMM can
> skew the measurement); the polkit tests now refuse `set-property` and `stop`
> on all three slices. No identity, RBAC grant, boundary or cluster object
> changes. It also fixes two citations of a removed reference unit. The
> 2026-10-23 pass before it covers three amendments found by a live vSphere run
> (the third, **ADR-0034's**, is described after the first two):
> **ADR-0044 on vSphere** (once the guest is installed the provider ejects an
> ISO-backed CD-ROM, as `govc device.cdrom.eject` does, and only then can
> `GuestReady` be `True`; recorded as `VSphereMachineStatus.installMediaDetached`)
> and **ADR-0020's** (a `forceUpload` re-import refuses, naming the VMs, before
> destroying anything when a running VM holds the ISO, and a failed import Job
> is retried after 10 minutes). It rewrites the TB-5 prose and three TB-5 rows
> (they were libvirt-only) and adds one TB-5 row. The provider's vCenter role
> now uses `VirtualMachine.Interact.SetCDMedia` / `.DeviceConnection` on the
> provisioning path too (`vsphere-least-privilege.md`). ADR-0034's amendment
> makes the vSphere provider drive a provisioned VM to
> `spec.desiredPowerState`, as the other providers always did, so a VM stopped
> in vCenter is started again; it adds one TB-4 row and one §8 risk. No actor, identity,
> RBAC grant or boundary is added, so §5 is unchanged. Before it, the
> 2026-10-22 pass was **ADR-0105**: Cloud Hypervisor guests and vTPMs
> run in one root-owned, operator-budgeted `banlieue-guests.slice`, so a
> host that also runs other workloads can bound them. It adds a root-owned
> unit file, four installer flags rendered into it, one TB-9 row, one TB-11
> row and one §8 risk (a slice is a ceiling, not a reservation); no actor,
> identity, RBAC grant or boundary is added, so §5 is unchanged. Before it,
> the 2026-10-22 pass was **ADR-0088**: first-boot sealing for `Immediate` images,
> behind `banlieue-controller --enable-first-boot-sealing`. The imagebuilder
> appends a layer writing the sealing policy into `COS_OEM` and a guest
> sealing report; the controller's pairing rule gains the declaration and
> keeps such VMs off Cloud Hypervisor; both providers gate `GuestReady` on the
> report, with a deadline. It adds one VAP
> (`vmimage-first-boot-sealing.yaml`), amends A-6 and A-8, the §2 controller
> and imagebuilder rows, three TB-4 rows and three TB-5 rows, §7.9, and five
> §8 risks (the `Manual` row is narrowed). No component, identity, RBAC grant
> or boundary is added (the report travels the ADR-0043 channels), so §5
> stands. The 2026-10-21 pass is **ADR-0104** (Accepted, implemented):
> a vSphere `Template`-kind `VMImage` (an existing vCenter template banlieue
> adopts and never builds) is looked up per failure domain, in
> `<rootFolder>/<failure-domain>`, and publishes per-zone status like a `Url`
> image. That would have made deleting such an image destroy the
> administrator's own templates, so `finalize` now destroys only templates a
> `Url` source imported, decided on the source kind. This pass adds a TB-4 block
> (four rows: adopted-template deletion, a planted same-named template, forged
> zone status, per-zone lookup load), extends A-3, the vSphere provider in §2 and
> the hypervisor-operator actor, and adds one §8 row (an adopted template is
> trusted by name, not provenance). It also records that the model never
> covered `Template` sources or template destruction on `VMImage` deletion
> (ADR-0028). No component, boundary, identity, RBAC grant or vCenter privilege
> changes (the lookups are the same read calls), so the §5 diagram stands.
> The earlier 2026-10-21 pass is **ADR-0103**: `banlieue bootstrap
> --image-digest` pins every banlieue image it writes (the controller,
> operator, imagebuilder and provider Deployments as well as the seeded
> `ProviderClass`es, which were the only ones pinned before) and refuses a
> malformed digest, and each release's `install.yaml` and clusterctl
> components are rendered after the image push with its multi-arch digest; a
> release with no digest fails. It closes the §8 risk that published
> manifests named the image by tag (that row also wrongly said bootstrap had
> no digest flag), adds two TB-6 rows, widens A-5 and adds §7.25. No
> component, boundary, identity or RBAC grant changes, so §5 stands. The
> 2026-10-15 pass is **ADR-0102**: a provider's liveness is
judged outside it, from the leader-election Lease it renews. banlieue-operator
reads each Provider's Lease (one `get`) into `status.workload.heartbeat`, and
the scheduler places nothing new on a `Stale` provider. The Lease thereby
becomes an input to scheduling, so this pass adds TB-17 (§5 table and
diagram, §6) and asset A-22, updates the operator and controller in §2, and
adds two §8 risks (clock skew, the operator as the evaluator). It also found
and **fixed** a wider grant: every shared ClusterRole but the operator's let
its identity write any Lease in the cluster, so Lease rights are now a
namespaced Role narrowed to each role's own Lease (`banlieue bootstrap`, the
GitOps manifests, and the operator's per-Provider Role, which already was).
The earlier 2026-10-14 pass is **ADR-0101**: every vSphere clone
carries its `VSphereMachine`'s UID in the `extraConfig` key
`banlieue.machine.uid`, outside `guestinfo.*` and set by `CloneVM_Task`
itself, and a create or a deletion with no `status.vmRef` searches the
clone's target folder and destroys only a VM carrying that UID. It widens
A-16 to vSphere, adds a vSphere ownership block to TB-4 (four rows), extends
the hypervisor-operator and sandbox-workload actors, widens the §8 marker
risk to vSphere and adds one §8 row (the guest cannot reach the key, not
yet verified live). No component, boundary, identity, RBAC grant or vCenter
privilege changes, so the §5 diagram stands. The earlier 2026-10-14 pass is
**ADR-0100**: every release
> publishes `banlieue-api` to crates.io at the release's version, from a
> `build.yaml` job of its own that alone reads the new repository secret
> `CARGO_REGISTRY_TOKEN`. It adds one asset (A-21, that token), one actor
> (a Rust consumer of the published types), the crates.io leg of the §5
> TB-6 line, three TB-6 rows, §7.24 for consumers and two §8 risks (a crate
> upload is unsigned and immutable; the token is long-lived until Trusted
> Publishing). No runtime component, CRD, identity or RBAC grant changes:
> the crate carries types, not a client, and a consumer reaches the API
> server under its own RBAC and the same admission policies as `kubectl`.
> The 2026-10-13 pass is
> **ADR-0099**: the management cluster's bootstrap can run a node on Cloud
> Hypervisor (a plain VM under three systemd units, no banlieue component)
> and can add workers and controllers to a running cluster with a
> short-lived token (`join`; a controller copies a running one's `/etc/k0s`),
> so a cluster's nodes can be swapped from libvirt to CH one at a time.
> Amended the same day (Decision 5), CH is now the default VMM: the host
> sudo it needs is the default rather than an opt-in, every command first
> refuses a node whose VMM disagrees with what exists on the host (so a
> libvirt-built cluster is neither doubled nor half-destroyed), and with no
> bridge configured CH nodes join the libvirt network's own bridge, read
> from libvirt (two rows on the CH management-node table, §4, §7.23, §8).
> It adds one component (§2), one asset (A-20, the join tokens), one
> actor (the bootstrap operator with sudo on the host), one boundary (TB-16,
> bootstrap operator → running cluster), a §5 diagram block, rows on TB-9 for
> a management node's VMM, §7.23 and three §8 risks. Found and fixed during the
> pass: the units' runtime directory defaulted to `0755`, and the VMM ran
> without Landlock (now on, with read access to its own tap's sysfs entry
> only). The 2026-10-12 pass is ADR-0097 (banlieue's API ends at
> `VirtualMachineClaim`; `AgentSandbox` moves to mediatore as its CRD), and its
> conclusion is **no boundary change**. `AgentSandbox` and `agent.banlieue.io`
> were never built, so no component, CRD, identity or RBAC grant leaves the
> deployed posture, and the broker stays what §4 already models: an external
> claim consumer bounded by §7.6. The pass updates the broker actor row, one §8
> risk (mediatore's declarative request path) and §9. The 2026-10-13 pass covers the **ADR-0047 Decision 9 amendment**: the
> claim subject policy takes a username prefix per issuer (`issuerPrefixes`),
> falling back to `usernamePrefix`. No component, asset, actor or boundary
> changes. It **narrows** one §8 risk: for every issuer with an
> `issuerPrefixes` line, a claim's `subject.issuer` is now verified (the
> prefix on the caller's username names the issuer that authenticated them)
> rather than only allowlisted. It updates two TB-7 rows and §7.6. It also
> covers **ADR-0098**: the scheduler spreads placement across failure domains
> (least loaded for the VM's pool, then overall, then a rendezvous hash)
> instead of taking the alphabetically first. No component, asset, actor or
> boundary changes; it closes one availability gap (a pool's whole warm set
> on one host) with a TB-1 row. Load is counted only from VMs in the same
> namespace, so placement can be steered only by someone who can already
> create VMs there. A second 2026-10-12
> pass covers the **ADR-0044 amendment**: a libvirt machine with install media
> now boots its disk first and the installer only while the disk is empty,
> fixing a deadlock found live (the guest rebooted into its installer after
> every install). No component, asset, actor, identity or boundary changes.
> It strengthens one TB-5 row (a guest re-entering its still-attached
> installer: the disk now wins whenever it is bootable, before the eject as
> well as after) and changes nothing else. The 2026-10-08 pass is ADR-0091 to ADR-0096 (roadmap 12):
> Prometheus metrics and opt-in OpenTelemetry export, path-aware health,
> opt-in NetworkPolicies, credential rotation without Secret watches, and
> clusterctl packaging with a scoped CAPI aggregate role. It adds no
> component and no namespace. It adds two actors (CAPI's manager, the
> telemetry consumers), one asset (A-19, telemetry), two boundaries (TB-14
> telemetry, TB-15 CAPI controllers → infrastructure objects), rows on TB-2
> and TB-4, §7.21 and §7.22, and four §8 risks. It **closes** two things: the
> CAPI manager no longer inherits the controller's whole role (it had create
> and delete on every `banlieue.io` kind), and the "fixed `200` health
> endpoint" risk, which ADR-0093 retires. A fourth 2026-10-06 pass is ADR-0090 (`VirtualMachine` finalize
> cascade: isolate each backend kind): no new component, actor, asset or
> trust boundary — found live, a stale `ClusterRole` missing a
> `proxmoxmachines` grant made `finalize_vm`'s `?`-chained backend lookups
> abort before the vSphere branch below them ran, so a permission gap on one
> backend blocked deletion for every `VirtualMachine`, any backend. Adds one
> TB-1 row for the fixed availability threat (classified D: the blast radius
> of a lookup failure, now scoped to the affected backend's own VMs by
> `plan_finalize`). The 2026-10-06 pass before it is ADR-0089 (`VSphereMachine`
> custom attributes `Template`/`CreatedAt`/`CreatedBy`): adds A-18, a TB-1 row
> for a forged `banlieue.io/created-by` annotation, and a §7.7 note on the new
> `MutatingAdmissionPolicy`'s higher (1.36+) floor and `failurePolicy:
> Ignore` posture — no new component, actor or trust boundary. A third
> 2026-10-03 pass is driven by evidence
> rather than an ADR: an OIDC issuer (Dex) and workload identity (SPIRE) were
> deployed on a reference management cluster, and every link's cryptography
> was measured by offering one TLS group at a time. It changes no component or
> boundary. It adds TB-7 rows for an issuer that runs **on** the management
> cluster (its signing keys become cluster resources; its default token
> lifetime; any upstream account can log in; the API server's own read access
> to its authentication config), corrects TB-12 (`sslmode=require` encrypts
> but does not authenticate the server; PostgreSQL 17 cannot negotiate
> post-quantum key exchange), adds two TB-13 rows (the default allow-all
> tailnet policy, and a roaming client whose VIP route is not yet approved),
> adds §7.20 (post-quantum key exchange on every link that carries cluster
> secrets) and records three §8 risks: banlieue's own TLS clients use rustls
> with the `ring` provider (ADR-0009), which has no ML-KEM, so **every
> connection banlieue makes is classical** (superseded by ADR-0170 on
> 2026-11-26: the clients now offer X25519MLKEM768 first); classical key exchange on links
> banlieue does not terminate; and no post-quantum signatures anywhere. The
> 2026-10-03 ADR pass covers **ADR-0086**: a
> management cluster with more than one controller puts its API behind a
> keepalived VRRP virtual IP that k0s's reverse proxy balances over every
> controller, and runs without konnectivity. No banlieue component, asset or
> actor changes: keepalived and the proxy run inside k0s, and the VRRP
> password is not a secret by design. It adds one trust boundary (TB-13, the
> LAN to the VIP), with a §5 diagram line, a STRIDE table, a TB-8 row (host
> providers now dial the VIP), §7.19 and two §8 entries (VIP takeover from
> the LAN, and API servers reaching kubelets directly). It removes a
> single-controller availability dependency that was never recorded.
>
> The later 2026-10-03 pass covers **ADR-0087**: a field manager names one
> writer, not a provider class. No component, asset, actor or boundary
> changes, because the writers and what they write are unchanged. What
> changes is that **this document previously asserted something false**: the
> TB-8 row for a stolen host token said the token "writes only its own
> `perProvider` row", which was true of the RBAC and of the intent but not of
> the effect. Every host applied under one class-wide field manager, and
> because a manager owns exactly the field set it last applied, each host's
> single-row apply made the API server delete every other host's row; the
> resulting watch storm was a write loop any host could cause with its own
> legitimate credential, observed at ~170 writes/sec sustained on one object.
> That row is corrected and a new TB-8 row records the threat and its control.
> The `force` on that apply is removed, so a cross-manager claim is now a
> conflict rather than a silent takeover. §§1–5 and 7–10 are unchanged: this
> was never a boundary that was missing, it was a control that did not do what
> the table claimed. The
> second 2026-10-02 pass covers
> **ADR-0085**: the management cluster's bootstrap can keep cluster state in
> an external SQL database through kine (PostgreSQL on the hypervisor host),
> put its nodes on a host bridge, and boot Kairos on Debian. It adds one asset
> (A-17, the datastore, which holds every object including every Secret) and
> one trust boundary (TB-12, controllers to datastore), with a §5 diagram
> line, a STRIDE table, §7.18 and two §8 entries (the datastore as a single
> point of failure, and nodes reachable from the LAN). Etcd, the default,
> was already inside the nodes and moves nothing. No component, actor or
> other boundary changes. The first 2026-10-02 pass covers **ADR-0084**:
> the installer is now `banlieue host cloud-hypervisor <verb>` (alias
> `ch`; it was `banlieue host <verb>`); it no longer installs OS packages
> on any distribution (the host's package manager supplies systemd and
> swtpm; `preflight` checks for them by command); and the VMM, `ch-remote` and
> firmware each come from a URL (github.com by default, or an operator's
> mirror) at the pinned release or any release at or above the client's
> version gate, still verified against a sha256 before anything is
> installed. **No new component, actor or trust boundary**: TB-11 gains a
> second source (a mirror) and, for a non-pinned release without a digest
> flag, a read of GitHub's published digest from `api.github.com`; the §5
> diagram's TB-11 line says so. §2's installer row and A-14 name the new
> digest sources. TB-11 gains five rows (a version or tag escaping
> `/opt/banlieue`, a plain-HTTP mirror, a hostile release document, the
> `[vmm]` rewrite, and a spoofed GitHub digest) and its two pin rows are
> revised. §7.11 gains one bullet, §8 two entries (a non-pinned release
> trusts GitHub's or the operator's digest; a newer VMM runs against types
> checked only against the pinned API), §9 one (the host's OS packages).
>
> **2026-10-01 pass.** Covers **ADR-0083** as revised in
> review: a `VirtualMachine` whose static address another banlieue VM already
> claims on the same `networkClass` is blocked before scheduling
> (`Ready=False reason=DuplicateAddress`, no infra CR), on all four infra
> kinds including Proxmox (ADR-0075); incumbency comes from
> the new, controller-written `status.heldAddresses`, never from
> guest-reported addresses; a blocked VM that is already provisioned keeps
> its infra CR but still has power applied. **No new component, asset, actor
> or trust boundary**, so the §5 diagram is unchanged. The controller reads
> `VirtualMachine`s across every namespace it watches, from its existing
> reflector store. §2's controller row names the check. TB-1 gains three
> rows: the traffic hijack/denial a declared duplicate enabled (controlled),
> cross-namespace disclosure through the condition message (controlled), and
> a forged `status.heldAddresses` (bounded by RBAC). §7 gains 17 (one
> `networkClass` per real segment). §8 gains two residues: first-come address
> squatting and a one-bit "address in use" oracle across namespaces.
> Guest-reported addresses do not feed the check, so it adds no
> guest-influenced residue. §9 gains machines banlieue did not create.
>
> **2026-09-28 pass — Proxmox VE (ADR-0074, ADR-0075).** A third
> hypervisor backend, reached over HTTPS with a **privilege-separated API
> token**. One new component (`banlieue-provider-proxmox`), one new actor (a
> stolen Proxmox token), one new asset (A-16, which of a Proxmox cluster's VMs
> banlieue considers its own), a Proxmox block under TB-4, rows added to
> TB-1, TB-2 and TB-5, hardening item §7.16 and five §8 entries. **No new
> trust boundary**: the provider is an in-cluster pod like the vSphere and
> libvirt ones, so it sits on TB-4. Every section was re-walked; TB-3, TB-6
> to TB-11 are unchanged. What is verified: the client and the provider are
> unit-tested against an in-memory Proxmox that refuses what the real API
> refuses, and the **read-only live tier (`make proxmox-live-test`) passed
> against a real node on 2026-09-28**: token auth, TLS verified against the
> node's own CA, inventory decoding, and the reason phrase on a 401. On 2026-09-30 the **lifecycle tier
> (`make proxmox-lifecycle-test`) passed** too: clone, configure, resize (a
> task), multipart seed upload, start, stop, delete, task polling with real
> UPIDs, and a same-named foreign VM surviving the machine's delete.
>
> A 2026-09-28 pass
> covered **ADR-0043 Decision 8 as amended 2026-09-28** (an unreachable
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
> **2026-10-11 TB-7 update (not a full pass; the stamp above is unchanged).**
> No ADR: `scripts/dev-oidc-k0s.sh` can now add Google and Auth0 as direct
> issuers beside Dex in one structured authentication file. No component,
> asset or boundary changes; TB-7 now has more than one issuer behind it. It
> adds four TB-7 rows (prefixes shared between issuers, unverified emails at
> a self-signup issuer, tokens minted for another application at the same
> provider, a refused authentication file that only fails at the next
> restart), a §7.6 obligation, and a note on the §8 issuer-verification risk.
>
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
   libvirtd (there is no password in the libvirt path by design), and a
   **privilege-separated API token** for Proxmox VE (there is no password or
   ticket path there either, ADR-0074). Those
   credentials are, by construction, more powerful than the Kubernetes cluster
   banlieue runs in — they can create, delete, and read the disks of every VM
   on the backend, including VMs banlieue never created.
2. **Guest bootstrap material.** cloud-config / user-data routinely carries SSH
   authorized keys, cluster join tokens, and registration secrets. banlieue
   moves that material from a Kubernetes Secret into a guest, across several
   intermediate representations, including, for a machine a Cluster API
   `Machine` owns, the join data its bootstrap provider renders (ADR-0129).

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

A host that serves agent sandboxes (ADR-0144) adds two more, both inside the host and both about
networking: TB-19, the sandbox segment (guests that run code nobody reviewed, kept from each other and
from the host), and TB-20, the uplink segment where two gateway VMs meet the host's NAT. The provider's
only new call is one rtnetlink request through the safe `rustix` API, so the `unsafe` count above stays at two.

A host that opts in to x509pop attestation (ADR-0146) gives the provider one more thing worth taking: a **signing
key** (A-24) that turns a guest's public key into a certificate the identity layer accepts as that guest. It is not a
hypervisor credential and not a cluster credential; it is custody of identity for the guests of one host. The
provider holds it in its own process, behind one new boundary, TB-21, where a guest sends one bounded request and
gets one certificate back. banlieue does not strengthen the host's trust (the signer still trusts the host it runs
on); it fixes who made the key, who can read it, and how far a stolen one reaches. The new crate has no `unsafe`
and starts no subprocess.

When an administrator has OpenBao sign a host's attestation CA (ADR-0146 Decisions 7 and 8), banlieue code makes
one call to an external service for the first time outside the hypervisors: the installer, as root, once, logs in
to an OpenBao the administrator names with the host's own ServiceAccount token and posts one certificate request
(TB-22). It holds no PKI credential afterwards and runs no PKI client in a service; the controller and the provider
have no part in it, on purpose, because a controller able to request signing for any host would be one credential
able to mint a CA for every host. What OpenBao enforces (the pinned policy, the login role, uniqueness, deletion)
is the OpenBao repository's; this document claims only what the installer does with the answer.

The Azure provider (ADR-0158 to ADR-0165) is the first backend that is a public cloud API, and it changes the
first fact's shape again: there is **no stored credential at all**. Its pod presents a projected ServiceAccount
token to Entra ID and spends the ARM token it gets back, in memory, inside one resource group, under a custom
role that can create, power and delete VMs and NICs and nothing wider (A-29, TB-4). What an attacker wants
moves from a Secret to whatever can mint that ServiceAccount token, and to the cluster's published
ServiceAccount issuer that Entra trusts; and the data that crosses the boundary now includes user data Azure
keeps with the VM. Ownership on Azure is a set of tags any writer of the resource group can also set, so the
resource group is part of the trust base (§4, §7.36, §8). The two Azure crates contain no `unsafe` and start no
subprocess, so the count above stays at two.

Publishing an image into Azure (ADR-0162) adds a second Azure identity, kept away from the runtime one: a Job in
the build namespace federates as a publisher identity whose role can write images and upload disks in the
gallery's resource group, which the runtime role cannot (A-30, TB-23). Its one new credential is a write SAS, a
bearer URL for one upload disk's bytes, held in the Job's memory for at most an hour and sent only to the cloud's
managed-disk upload domain. The gallery is shared storage in the TB-5 sense: every Azure VM of a `Provider` boots
from it, so what is published there (A-31) is as sensitive as the build itself. The provider orchestrates the Job
and can create it, so the separation holds against a leaked runtime token and not against a compromised provider
process (§8).

## 2. Components

| Component | Identity | Namespace | Scope |
| --- | --- | --- | --- |
| `banlieue-controller` | `banlieue-controller` | `banlieue-system` | Watches `VirtualMachine`, schedules onto a `Provider` (only where the VM fits the failure domain's published capacity, ADR-0107, which lists `VMClass`es once per scheduling pass, and for a new placement its measured free memory and disk, ADR-0128), creates provider infra CRs. Refuses a VM whose static address another VM already claims (ADR-0083), which reads `VirtualMachine`s across every namespace it watches. Serves only the namespaces ADR-0130 resolves: those matching `--namespace-selector`, else `--namespace`, else its own namespace, **never every namespace by default**; in selector mode it holds a read-only list/watch on `Namespace` objects and skips any object outside the matching set (`banlieue-provider-sdk/src/scope.rs`, `Context::skip_unserved`). Also runs the `VirtualMachinePool` loop (ADR-0046) and the `VirtualMachineClaim` loop (ADR-0047): the latter binds a pool member to a subject and destroys it on release. Unless run with `--enable-first-boot-sealing=false` (on by default since ADR-0088 was accepted, 2026-10-26) it accepts a `tpmEnabled` class with an `Immediate` image that declares `spec.firstBootSealing`, copies the declaration onto the infra CR, refuses it without a vTPM or with the flag off, and keeps such VMs off Cloud Hypervisor (ADR-0088): `reconciler/virtualmachine.rs` (`image_class_mismatch`), `reconciler/infra.rs`, `reconciler/scheduler.rs`. For an InfraMachine a Cluster API `Machine` controls, once the API server serves `cluster.x-k8s.io/v1beta2` Machines (it watches the `machines.cluster.x-k8s.io` CRD, by name, so a later Cluster API install needs no restart and an API error stops no other loop), it reads the Machine, reads its bootstrap Secret **by name** where a namespaced Role allows, classifies it, and writes its `value` into `spec.userData` and the one condition `BootstrapDataReady` (ADR-0129; `CloudHypervisorMachine` only until milestone 2), in the namespaces it serves like every other loop: `reconciler/bootstrap_data.rs`, `banlieue-api/src/capi.rs`. Audits its own progress (ADR-0145): a metadata-only list of its `VirtualMachine`s each minute, compared with its cache and a ledger of invoked UIDs; sustained gaps fail `/livez` (`crates/banlieue-provider-sdk/src/audit.rs`). Mirrors `status.attestationSubject` machine to VM to claim (ADR-0143 Decision 7, ADR-0146). Holds no grant on the Firecracker kinds, and does not watch them |
| `banlieue-operator` | `banlieue-operator` | `banlieue-system` | Provider lifecycle (ADR-0012): creates provider Deployments, ServiceAccounts, Roles, RoleBindings. Reads each Provider's leader-election Lease (one `get`) and records its heartbeat in `status.workload` (ADR-0102) Serves only the namespaces ADR-0131 resolves for its `Provider` watch: those matching `--watch-namespace-selector`, else `--watch-namespace`, else its own namespace, **never every namespace by default**; in selector mode it holds a read-only list/watch on `Namespace` objects and skips a `Provider` outside the matching set, except one being deleted (`banlieue-provider-sdk/src/scope.rs`, `Context::skip_unserved`, `reconciler/provider.rs`). |
| `banlieue-provider-vsphere` / `-libvirt` | per-`Provider` SA | `banlieue-system` | Talks to the hypervisor; reconciles infra CRs (`VSphereMachine`, `LibvirtMachine`, ADR-0050) and realises them as real VMs/domains. For a `Template`-kind `VMImage` it looks the adopted template up per zone, read-only, and never destroys it (ADR-0104) On vSphere a clone gets one NIC per declared `VMClass` interface (ADR-0111): the template's own NIC is edited onto the first interface's port group and each further interface is added in the same `CloneVM_Task` (`client/vim.rs`, `build_clone_nic_changes`). Publishes each failure domain's capacity from read-only backend calls (ADR-0128): libvirt `NODE_GET_INFO`, `NODE_GET_MEMORY_STATS`, `STORAGE_POOL_GET_INFO`; vSphere cluster, host and root-resource-pool summaries and datastore summaries (`client/vim.rs::cluster_capacity`). Without a selector it serves one namespace: `--namespace`, else the namespace it runs in, never every namespace (ADR-0131, `scope::provider_namespace`). **Since ADR-0133 the vSphere, libvirt and Proxmox providers serve their machines in the namespaces `Provider.spec.namespaceSelector` selects** (its `--watch-namespace-selector`, rendered by the operator, scope code shared with the controller and operator in `crates/banlieue-provider-sdk/src/scope.rs`), while it reads its own `Provider`, credentials Secret and CA bundle from its **home** namespace only (`Context::home_namespace`): it holds no Secret access in any namespace it serves. (libvirt and Proxmox do the same: `crates/banlieue-provider-{libvirt,proxmox}/src/context.rs`, `src/app.rs`.) It acts on a machine only when `providerRef` resolves to the `Provider` it serves, by name **and** namespace (`serves_machine`). |
| `banlieue-provider-proxmox` | per-`Provider` SA | `banlieue-system` | Talks to Proxmox VE through the first-party `banlieue-proxmox` client (ADR-0074); reconciles `ProxmoxMachine`s (ADR-0075) into full clones of a template VM, verifies the template VMIDs `VMImage`s name, and publishes one failure domain per node. Spawns no Jobs: `deploy/provider-proxmox/rbac/clusterrole.yaml`. Publishes each node's capacity from the `GET /nodes` and `GET /nodes/{node}/storage` fields it already reads (ADR-0128, `reconciler/provider.rs::node_capacity`). Without a selector it serves one namespace: `--namespace`, else the namespace it runs in, never every namespace (ADR-0131, `scope::provider_namespace`); with one it serves the namespaces `Provider.spec.namespaceSelector` selects and reads its credentials from its home namespace only, as the vSphere and libvirt row above describes (ADR-0133, `crates/banlieue-provider-proxmox/src/context.rs`). |
| `banlieue-provider-azure` (ADR-0158, ADR-0164) | per-`Provider` SA (`banlieue-provider-azure-<provider>`), or `banlieue-provider-azure` for a static install; **no Secret and no ConfigMap** in either Role | `banlieue-system`, or the class's `workloadNamespace` | The `banlieue provider azure` role of the one binary (`crates/banlieue/src/cli.rs`), a `Managed` in-cluster workload the operator renders per azure `Provider` (`crates/banlieue-operator/src/workload.rs`, `bootstrap.rs`). Calls Azure Resource Manager and Entra ID over HTTPS through the first-party `banlieue-azure` client (TB-4). Its only cloud credential is a projected ServiceAccount token (audience `api://AzureADTokenExchange`, 3600 s, mounted read-only under `/var/run/secrets/azure/tokens`), exchanged at Entra ID for an ARM token held in memory (ADR-0159 Decision 1, A-29). Reconciles `AzureVirtualMachine`s into one NIC and one VM (its OS disk and NIC are deleted with it) in the `Provider`'s one resource group, every resource tagged with its owner (ADR-0158 Decision 4, A-16); publishes quota headroom per zone as capacity (ADR-0161, `reconciler/provider.rs`); checks that a `Template` source is shaped like a gallery image version ID (`reconciler/image.rs`); runs the orphan sweep (ADR-0158 Decision 5, `reconciler/resources.rs`). For a `Url` source it reconciles the gallery to the declared `VMImage`s (ADR-0162 Decisions 4 and 8): it reads versions with the runtime identity and creates publish and delete **Jobs in the build namespace** as `banlieue-azure-publisher` (`reconciler/{image,gallery,publish_job}.rs`), with `jobs` and `pods` `list` from a `Role` in the build namespace only (`crates/banlieue-operator/src/workload.rs::build_jobs_role`), confined by `deploy/admission/provider-jobs.yaml` (TB-3), and holds the `VMImage` finalizer `banlieue.io/vmimage-azure`. No public IP, no image write of its own, and **no channel into the guest** (ADR-0163). Scope as the Proxmox row: one namespace without a selector, the selected namespaces with one (`SHARED_PROVIDER_BACKENDS`, `crates/banlieue-provider-sdk/src/scope.rs`). The two Azure crates contain no `unsafe` and start no subprocess |
| `banlieue-provider-cloud-hypervisor` | ServiceAccount `banlieue-provider-cloud-hypervisor` (a bound token in a kubeconfig **on the host**); host user `banlieue` | **Not in the cluster**: a systemd service on the KVM host; its Provider lives in `banlieue-system` | Watches its own `Provider`, `CloudHypervisorMachine`s and `VMImage`s over the API; creates taps, disks and seeds, and starts instances of root-owned template units (ADR-0060, ADR-0063, amended 2026-09-27). Creates nothing, not even a guest uid, for a machine Cluster API means until a `Machine` owns it and `BootstrapDataReady` is `True`, and never applies that condition itself (ADR-0129): `reconciler.rs` (`holds_guest` from `banlieue-api`, `status_patch`). Holds `CAP_NET_ADMIN`, `CAP_CHOWN`, `CAP_FOWNER` and nothing else; sees `/dev`, `/sys` and `/proc` read-only (`ReadOnlyPaths=/dev /sys /proc`), and its only device beyond the pseudo-devices `DevicePolicy=closed` always allows (`/dev/null`, `zero`, `full`, `random`, `urandom`, `tty`, `ptmx`, `/dev/pts/*`) is `/dev/net/tun`; not in the `kvm` group. Tested `the_provider_writes_no_device_node_sysfs_or_procfs_and_opens_only_tun` and `the_provider_sets_nothing_that_undoes_its_read_only_paths`: `deploy/provider-cloud-hypervisor/host/banlieue-provider-cloud-hypervisor.service` **Since ADR-0134 a host can serve tenant namespaces** (`crates/banlieue-provider-cloud-hypervisor/src/app.rs`, `reconciler.rs`): it reads its own `Provider` from its home namespace, takes its `namespaceSelector` from it once at startup, and matches a machine by name **and** namespace; the machines behind a selector are reachable only through the opt-in `banlieue-provider-cloud-hypervisor-shared` ClusterRole, which the operator binds for a `Provider` that has one. |
| Cloud Hypervisor VMM, one per guest | Its own host uid **and private gid** (`banlieue-g<uid>`, registered in `/etc/userdb`, or, on a host without userdb, created in the host's own passwd and group databases, ADR-0139), plus `kvm` | The host, as `banlieue-ch@<guest uid>.service`, an instance of a root-owned template | Runs one guest. No capabilities; seccomp and Landlock on; systemd sandbox, `/dev` read-only and its own IPC namespace: `deploy/provider-cloud-hypervisor/host/banlieue-ch@.service` |
| `banlieue-imagebuilder` | `banlieue-imagebuilder` | `banlieue-system` | Drives kairos `OSArtifact` builds; merges cloud-config (ADR-0037) ; for a `VMImage` with `firstBootSealing` appends a banlieue-generated layer after every operator layer: the sealing policy into the installed disk's `COS_OEM` and a boot stage that reports the sealed partitions (ADR-0088, `src/first_boot_sealing.rs`); for a `VMImage` with an `azure` source appends the Azure guest layer last and lists the digest-pinned helper image in the `OSArtifact`'s `bundles`, refusing the build (`AzureGuestImageUnset`) without one (ADR-0171, `src/azure_guest.rs`); reads the artifacts PV and the node it is pinned to (read-only, cluster-scoped) to rebuild an artifact whose node-local volume was lost with a replaced node (ADR-0115) |
| swtpm, one per `tpmEnabled` Cloud Hypervisor guest | The guest's uid and private group | The host, as `banlieue-swtpm@<guest uid>.service` | Emulates the guest's TPM from state in `/var/lib/banlieue/tpm/<guest uid>/` (ADR-0065) — `plan.rs::swtpm_unit` |
| vTPM manufacture, once per guest | Host user `banlieue` | The host, as `banlieue-swtpm-setup@<guest uid>.service` | Runs `swtpm_setup`: creates the TPM and signs its EK and platform certificates with the host's `swtpm_localca` key — `plan.rs::swtpm_setup_unit` |
| Attestation signer, one per opted-in guest (ADR-0146 Decisions 2 and 3) | The provider process (host user `banlieue`); the **socket** is owned by the guest's uid | In the provider, on the host; one Unix socket `<run dir>/vsock.sock_1025` per guest, created before the VMM starts and removed when the guest stops or the machine is deleted | Answers one PEM CSR per connection on the guest's own hybrid vsock: takes only the EC P-256 public key, forces `CA:FALSE`, `digitalSignature` and the subject (the machine's `metadata.uid`), a bounded validity, rate limited per machine, counters only. Absent unless the host config has `[attestation]`: `crates/banlieue-provider-cloud-hypervisor/src/attestation.rs` (`Signers`, `ensure_for_plan`), `crates/banlieue-attestation-ca/src/{signer,serve}.rs` |
| `banlieue-attestation-ca` (library) | None of its own: code in the provider (signer) and in the installer (creation) | Linked into `banlieue-provider-cloud-hypervisor` and `banlieue-host`; no Kubernetes dependency, no subprocess, no FFI | The CA (EC P-256 key and self-signed certificate on disk), the CSR checks, the certificate builder and the request loop, in one place so the `CA:FALSE` rule cannot be forgotten in a second copy (ADR-0146 Decision 3). It knows nothing of machines, classes or the cluster: the caller names the directory, the subject and the socket. The same code serves a Firecracker provider once one exists: `crates/banlieue-attestation-ca/src/`. With ADR-0146 Decision 7 it also makes the pending key and request (`PendingCa`) and `import`s a signed certificate after checking it against that key, and the signer's reply carries the public chain above a CA that an upstream signed; a signer under an expired CA refuses (`crates/banlieue-attestation-ca/src/pending.rs`, `signer.rs`). |
| OpenBao node-attestation PKI mount (ADR-0146 Decisions 7 and 8) | Its own: the OpenBao repository's roles and policies, **not banlieue's** | Outside banlieue and outside every host: an administrator-operated OpenBao, on a dedicated mount whose root key is generated inside it | Signs a host's attestation CA request as an intermediate with path length 0. banlieue holds no credential for it, runs none of it and reaches it from one place, the installer, once per onboarding (TB-22) |
| `banlieue-provider-firecracker` (library, ADR-0143) | None: **nothing links it** (no binary, no other crate) | The workspace only | An attestation CA and per-guest signer written for a Firecracker provider that does not exist. ADR-0146 names `banlieue-attestation-ca` as the one implementation both providers share; before any binary links this crate it is replaced by, or reconciled with, that one, and the pass for that change covers it. No surface while unlinked |
| `banlieue-tls` (library, ADR-0170) | None: code in every binary role and in the tests that open TLS connections | Linked into every TLS client | Builds and installs the one process rustls `CryptoProvider`: aws-lc-rs, key-exchange order X25519MLKEM768, X25519, secp256r1, secp384r1. Holds no key and no credential |
| `banlieue-oci::microvm` (library module, ADR-0142) | None: code in `banlieue-oci` | Linked wherever `banlieue-oci` is; **nothing calls it** | The microvm artifact format: build and parse the manifest, push, and a fail-closed pull that verifies every digest and size and lands in the image area by one atomic rename (`crates/banlieue-oci/src/microvm.rs`). No host pulls a microvm image yet: there is no Firecracker provider |
| Guest attestation helper (ADR-0146 Decision 6) | **Not built.** Inside the guest, as root | The guest image, mediatore's and the sandbox image build's | Would make the guest's key, send the CSR on vsock port 1025, renew at half life, and configure the SPIRE agent. banlieue fixes only the host contract it talks to (port, framing). Until it exists nothing calls the signer in production |
| Azure guest helper `banlieue-azure-guest` (ADR-0171) | **Not a cluster identity**: root inside an Azure guest, as a `boot` stage step (`customdata`) and `banlieue-azure-ready.service` (`ready`) | Every image built for a `VMImage` with an `azure` source, at `/usr/bin/banlieue-azure-guest`, from a digest-pinned bundle unpacked at build time | `customdata`: mounts the local provisioning disk read-only (`nosuid,nodev,noexec`), parses `ovf-env.xml` strictly (256 KiB cap, no DTD, CDATA or entities, one `CustomData`), decodes at most 64 KiB, accepts only a `#cloud-config` document and writes it to `/oem/91_azure_customdata.yaml` 0600 atomically (`src/customdata.rs`, `src/ovf.rs`). `ready`: on an active or passive boot only, goal state then a `Health` `Ready` post to the constant `168.63.129.16:80`, the three echoed identifiers checked against narrow character sets, retried for at most 15 minutes (`src/ready.rs`, `src/goalstate.rs`, `src/http.rs`). No flags, no TLS, no network fetch; does nothing off Azure (DMI asset tag); logs to `/dev/kmsg`, never customData |
| per-zone import Job | `banlieue-import` | `banlieue-imagebuild` | Uploads a built ISO to a datastore, creates a template (ADR-0020) |
| registry push Job | **None** — `automountServiceAccountToken: false` | `banlieue-imagebuild` | Pushes a `Ready` build to the operator's OCI registry for host-resident providers (ADR-0064); reads the artifacts PVC read-only, the push Secret and, if the operator names one, one key of a CA bundle (ADR-0110, a trust anchor, mounted read-only), nothing else — `crates/banlieue-imagebuilder/src/reconciler/push.rs` |
| build digest Job (ADR-0010, amendment 2026-11-26) | **None**: `automountServiceAccountToken: false` | `banlieue-imagebuild` | Measures a `Ready` build's file (`banlieue imagebuilder digest`, streaming SHA-256 with RustCrypto `sha2`, plus SHA-512 when the source declares one) and reports it as its termination message; non-root, read-only root, no privilege escalation, every capability dropped, `RuntimeDefault` seccomp, the artifacts PVC read-only, no network use. Owned by the `OSArtifact`. `crates/banlieue-imagebuilder/src/reconciler/digest.rs` |
| Azure image publish Job (ADR-0162) | `banlieue-azure-publisher`: **no RBAC**, `automountServiceAccountToken: false`; only a projected token (audience `api://AzureADTokenExchange`, 3600 s) for the publisher identity (ADR-0159 Decision 3) | `banlieue-imagebuild` | `banlieue provider azure image-publish` (`crates/banlieue-provider-azure/src/publish.rs`), created by the Azure provider: verifies the build's SHA-256, uploads it as a fixed VHD through a write SAS, snapshots it and creates the gallery version, or deletes a version its owner tagged; reads the artifacts PVC read-only (publish mode only), non-root, read-only root filesystem, no capabilities, `RuntimeDefault` seccomp; reports one JSON line on its termination message (`reconciler/publish_job.rs::build_job`); egress `deploy/network-policies/azure-publish.yaml` |
| pre-lay Job (ADR-0113) | **None**: `automountServiceAccountToken: false` | `banlieue-imagebuild` | Boots a `Ready` raw build once under QEMU/KVM (no network, no TPM) to pre-lay it, for a `VMImage` with `spec.prelay`; **privileged** (loop devices, mounts, `/dev/kvm`, as the namespace's kairos-operator builds), mounts the artifacts PVC read-write to write `<osartifact>-prelaid.raw` beside the untouched build, runs `--prelay-image` (by default the published `ghcr.io/firestoned/banlieue-prelay` at the imagebuilder's version; `banlieue bootstrap imagebuilder --prelay-image-digest` pins it), and reports only a digest: `crates/banlieue-imagebuilder/src/reconciler/prelay.rs` (`build_prelay_job`), `scripts/prelay-kairos-raw.sh` |
| image import unit (Cloud Hypervisor) | Host user `banlieue` | The host, as `banlieue-ch-import@<vmimage uid>.service` | Pulls one image by digest into the storage classes' image caches and exits; sandboxed, writable only in `images/` — `crates/banlieue-provider-cloud-hypervisor/src/vmimage.rs` (`import_unit`), `import.rs` |
| kairos build pod | kairos-operator's SA | `banlieue-imagebuild` | **Privileged** — loop devices, mount, chroot |
| Management-cluster node on Cloud Hypervisor (ADR-0099) | Host user `CH_VM_USER` (default `k0s-vm`, no login, `kvm`), **shared by every such node on the host**; its tap is created by a root oneshot | The host, as `k0s-vm-<name>.service`, with `k0s-vm-<name>-net.service` and `k0s-vm-<name>-swtpm.service`; **not a banlieue object** | Runs one management-cluster node, written by `scripts/bootstrap-k0s-cluster.sh` through sudo. No capabilities, `NoNewPrivileges`, `ProtectSystem=strict`, `DevicePolicy=closed` (`/dev/kvm`, `/dev/net/tun`), Landlock with the tap's sysfs entry read-only, runtime directory `0700`; writes only `CH_VM_DIR/<name>/` (`0750`) and its runtime directory: `render_ch_vm_unit`. VMM and vTPM in `banlieue-nodes.slice` (ADR-0106) |
| `banlieue host cloud-hypervisor` (installer) | **root**, once, at an operator's request; no cluster identity | A Cloud Hypervisor host, as a command, never a service | Installs the VMM and firmware (the pinned release, or one the operator names at or above the client's gate, from github.com or a mirror; ADR-0084), the `banlieue` user, guest uid records, directories, the host config, the EK CA, **for a host that opts in the attestation CA (ADR-0146: created once, never overwritten, rotated only by an explicit flag with a new name)**, the polkit rule and the units (ADR-0067). Installs no OS packages. Its `overhead` verb (ADR-0106) is read-only and runs as any user: it reads `/proc/*/task/*/stat` and cgroup counters and writes nothing. A separate crate no provider depends on: `crates/banlieue-host/`. With `--attestation-ca-csr`, `--attestation-ca-import` or the `--attestation-ca-openbao-*` flags it makes a pending key and request, imports a signed certificate, or has OpenBao sign the request in one run, logging in as the host's own ServiceAccount (TB-22; ADR-0146 Decisions 7 and 8): `crates/banlieue-host/src/{openbao,stages,settings}.rs`. |
| Sandbox segment host rules (ADR-0144 Decision 6) | **root**: `banlieue-sandbox-segment.service` and `/usr/local/sbin/banlieue-sandbox-segment`, installed by the operator, never by the binary (ADR-0067 Decision 7) | A Cloud Hypervisor host that serves sandboxes, a oneshot ordered `Before=` the provider | Loads three iptables chains (`BANLIEUE_SBX_INPUT`, `BANLIEUE_SBX_FORWARD` in `filter`, `BANLIEUE_SBX_NAT` in `nat`) with `--noflush`, keeps one jump to each at position 1, and on stop removes only its own; reads `/etc/banlieue/sandbox-segment.env` (the two bridge names and the uplink range). Reference: `docs/src/guides/cloud-hypervisor-sandbox-segment.md` |
| Egress gateway VMs, two per sandbox host (ADR-0144 Decision 4) | An ordinary guest: its own host uid and private gid, like any other (TB-9) | The same host, placed with `providerSelector` on a label unique to it | NICs on the sandbox bridge (a non-isolating class) and on the uplink bridge; the only route out of the segment. What they enforce is image content, mediatore's (its ADR-0028), as for TB-18 |

## 3. Assets

| ID | Asset | Where it lives | Impact if lost |
| --- | --- | --- | --- |
| A-1 | Hypervisor credentials: vCenter username/password, a libvirt **mTLS client key**, or a Proxmox **API token** (`username` = `user@realm!tokenid`, `tokenValue`) | Secret named by `Provider.spec.connection.credentialsRef` | **Critical**: full virtualization-layer compromise, independent of Kubernetes **Where it lives (ADR-0133): in the provider's home namespace, never in a tenant namespace.** **Who may name it (ADR-0157):** whoever creates its Provider or changes that Provider's connection must hold `get` or `reference` on it; `reference` reads nothing but decides where the credential is presented (§8). A shared provider serves tenant namespaces without a Secret grant there (`deploy/provider-vsphere/rbac/clusterrole.yaml` has no `secrets` or `configmaps` rule, pinned by `bootstrap_tests.rs::serving_tenant_namespaces_grants_no_provider_secret_access`), so a tenant who can read Secrets in its own namespace cannot read it. **An azure `Provider` has no A-1**: admission refuses a `credentialsRef` for it (`deploy/admission/provider-connection.yaml`) and its credential is A-29. |
| A-2 | Guest bootstrap material (cloud-config, SSH keys, join tokens) | (On Azure, also inside the guest after first boot: `/oem/91_azure_customdata.yaml`, 0600, written by `banlieue-azure-guest`, ADR-0171.) Secrets/ConfigMaps, or for an InfraMachine a Cluster API `Machine` owns, that Machine's bootstrap Secret (type `cluster.x-k8s.io/secret`, rendered by a bootstrap provider, usually carrying a **join token**; ADR-0129) → `VSphereMachine.spec.userData` / `LibvirtMachine.spec.userData` / `CloudHypervisorMachine.spec.userData` / `ProxmoxMachine.spec.userData` / `AzureVirtualMachine.spec.userData` → `guestinfo.userdata` or a NoCloud ISO (on Cloud Hypervisor, `seed.iso` in the machine directory on the host; on Proxmox, `banlieue-<machine uid>.iso` on an `iso`-content storage, attached for the machine's life; beside it a generated `network-config` for a static NIC, ADR-0114, which holds addresses, not secrets; on Azure, the VM's `osProfile.customData`, at most 64 KiB, which **Azure stores with the VM** in its own control plane, outside banlieue, for as long as the VM exists, `crates/banlieue-azure/src/vm.rs`) → built image | High: guest compromise, lateral movement into provisioned fleet |
| A-3 | VM image artifacts (ISO / raw disk) | `OSArtifact` PVC, then a vSphere datastore under `banlieue-images/`; on Cloud Hypervisor, the operator's **OCI registry** (a single-layer artifact, addressed by digest) and then each storage class's image cache on the host (`<storage class>/images/sha256-<hex>.raw`, `0750 banlieue`), or a file an admin placed there (ADR-0064); and, for a vSphere `Template` source, the administrator's own template in `<rootFolder>/<failure-domain>`, which banlieue only reads and clones, never writes or destroys (ADR-0104) | **Critical** — a tampered image compromises every VM built from it |
| A-4 | Integrity of the control plane's own decisions | `Provider`, `ProviderClass`, `VMImage`, `VMClass` CRs | High — a forged `Provider` redirects credentials; a label put on a `Namespace` brings its pools, claims and VMs under the controller's hand (ADR-0130); a forged `VMImage` redirects the fleet's boot media; a forged or edited `VMClass` also chooses **which segments a VM joins**, since a VM gets one NIC per interface its class declares (ADR-0111) |
| A-5 | Released binaries, container images (`banlieue`; `banlieue-prelay`, which the privileged pre-lay Job runs, ADR-0113; and `banlieue-azure-guest`, which runs as root in every Azure image banlieue builds, ADR-0171) and install manifests (`install.yaml`, clusterctl components; both pin the image by digest, ADR-0103) | GHCR, GitHub Releases | **Critical**: downstream supply-chain compromise |
| A-9 | **The subject's own credential (a JWT)** — the thing a sandbox is handed so it can act as its subject | **Never in banlieue.** Broker → in-guest agent over mTLS, after attestation (ADR-0049). Not on a disk, not in a CR, not in a hypervisor channel | **Critical** — it *is* the subject's identity. Kept out of banlieue entirely, which is why no banlieue compromise discloses it |
| A-8 | **Guest readiness marker**: a value the guest writes and the provider reads back: `/run/banlieue/phase` on libvirt, `guestinfo.banlieue.phase` on vSphere, and on Cloud Hypervisor the `phase=installed` and (ADR-0112) `sealed=` lines of its vsock report | guest tmpfs → `qemu-guest-agent` → `LibvirtMachine.status.guestInstalled`, or guest `vmware-rpctool` → `config.extraConfig` → `VSphereMachineStatus.guestInstalled` (ADR-0043; both transports verified against a real backend) | Low on its own, but it gates pool membership: a guest that can assert it early gets handed out early. **Not an integrity signal**: see §6/TB-4 and §8 Since ADR-0088 the same channels carry the **sealing report** of a first-boot sealing guest (`/run/banlieue/sealed`, `guestinfo.banlieue.sealed`) into `status.sealedPartitions`, with the same trust: it can only withhold `GuestReady`. |
| A-7 | **Claim bindings**: which subject was given which VM, and when | `VirtualMachineClaim.spec.subject` + `status` (including `status.poolRef`, the pool the member came from, ADR-0132, and the `Bound` message naming the strategy and the member's `provider/failureDomain`, ADR-0135), mirrored onto the member as `banlieue.io/claim-subject-*` annotations (ADR-0047); and `status.attestationSubject`, the bound member's attestation subject, mirrored from its machine through its `VirtualMachine` (ADR-0143 Decision 7, ADR-0146; public, a machine uid; written only by `banlieue-controller`, and cleared when the member has none) | Medium: discloses who was using which sandbox to every reader of the namespace; a *forged* binding makes the record say someone requested a VM they never asked for |
| A-10 | **The consumer's cached Kubernetes credential** — the ID token `kubectl oidc-login` writes to disk after a browser flow | `~/.kube/cache/oidc-login` on the consumer's own machine, outside every boundary below | High — it authenticates as that consumer, so it can create claims *attributed to them*. banlieue has no control here; see §8 |
| A-11 | **The Cloud Hypervisor provider's cluster credential** — a bound ServiceAccount token, renewed by the provider itself at half-life | `/etc/banlieue/credentials/token` on the host, beside a kubeconfig that only points at it; directory `0700 banlieue` (ADR-0060 Decision 5) | High — it is the provider's cluster identity: its own `Provider` and status, every `CloudHypervisorMachine` in the namespace, and **minting further tokens for itself**. **No Secret access** — the operator-built Role (`crates/banlieue-operator/src/workload.rs`) and `deploy/provider-cloud-hypervisor/rbac/clusterrole.yaml` — so it discloses no other credential. On a host that uses the OpenBao route (ADR-0146 Decision 8) the installer also reads it, as root, to log in to OpenBao for one signing (TB-22). |
| A-12 | **Guest disks, seeds and API sockets on a Cloud Hypervisor host** | `<storage class>/<machine uid>/` (`os.raw`, `seed.iso`, `serial.log`, and `install.iso` while a `Deferred` installer is attached) and `/run/banlieue/ch/<guest uid>/api.sock`, each directory `2770 guest-uid:banlieue` | High — a guest's whole disk, its rendered user-data (A-2) in the seed, and control of its VMM. Encrypted at rest only for a `tpmEnabled` machine installed `Deferred` (ADR-0065), which seals to its own vTPM; otherwise plaintext, and `tpmEnabled` with an `Immediate` image is refused: ADR-0048's rule, which ADR-0088 keeps on this backend by never scheduling a first-boot sealing VM here |
| A-13 | **Registry credentials** (ADR-0064) — push: a `kubernetes.io/basic-auth` Secret in `banlieue-imagebuild`; pull: `username`/`password` files in the host's `[registry] credentials_dir` (`0750 root:banlieue`) | Build namespace; each Cloud Hypervisor host | Push: **High** — combined with a `VMImage` status write, it chooses what hosts boot (TB-10). Pull: Medium — reads every pushed image, including cloud-config baked into it (A-2) |
| A-14 | **What makes a Cloud Hypervisor host safe to run guests on**: the VMM and firmware, the template units, the polkit rule, the tmpfiles.d entry (run root, `/dev/net/tun`), the host config, the guest account records (userdb files, or accounts in the host's passwd and group, ADR-0139), the accepted-reduced-hardening list in the host config, and directory modes | `/opt/banlieue/`, `/etc/systemd/system/`, `/etc/polkit-1/rules.d/`, `/etc/tmpfiles.d/`, `/etc/banlieue/`, `/etc/userdb/`, placed by `banlieue host cloud-hypervisor install` (ADR-0067, ADR-0084) | **Critical**: a tampered unit or polkit rule is root on the host, a tampered VMM runs every guest; integrity comes from the banlieue binary (A-5) and a sha256 per file: the pins compiled into it for the pinned release, otherwise the operator's digest flag or GitHub's published digest (ADR-0084) |
| A-16 | **Which Proxmox, vSphere and Azure resources banlieue considers its own**: on Proxmox the `banlieue-machine-uid=<uid>` line in a VM's description (ADR-0075 Decision 4); on vSphere the `extraConfig` key `banlieue.machine.uid` (ADR-0101); on Azure the tags `banlieue-owner-uid`, `banlieue-owner-kind`, `banlieue-provider` (`<namespace>.<name>` of the `Provider`) and `banlieue-owner-namespace` on every VM and NIC (ADR-0158 Decision 4, amended 2026-11-25; `crates/banlieue-azure/src/tags.rs`), which are also what the orphan sweep reads to decide what to delete; and, since ADR-0162, the same owner tags (owner kind `VMImage`, owner UID the `VMImage`'s) on every image definition, version, upload disk and snapshot a publish creates, plus `banlieue-source-sha256` on the version, which decide what a publish adopts and what a delete removes | Each VM's `description` on the Proxmox cluster, or its `config.extraConfig` in vCenter; written by the clone itself on both. On Azure, resource tags in the `Provider`'s resource group, written by the provider at create and **writable by anyone with tag-write there** (§8) | High for **integrity**: it is what stops a machine adopting, reconfiguring and finally destroying a VM it did not create (a same-named VM in another namespace, or an administrator's own). Not a secret |
| A-6 | vTPM identity and sealed disk-encryption keys | vSphere VM, per-clone (ADR-0039/0040); on libvirt, **swtpm state keyed by domain UUID** (ADR-0050); on Cloud Hypervisor, swtpm state in `<storage class>/<machine uid>/tpm/`, owned by the guest's uid, deleted with the machine (ADR-0065) | High — a shared or surviving TPM identity breaks per-VM disk-encryption isolation With an `Immediate` image that declares `firstBootSealing` (ADR-0088) the template carries the sealing **policy** and no key material or TPM state; each clone's key is born on its first boot against its own vTPM, so nothing sealed is copied. |
| A-6a | **vTPM endorsement key certificate**: the public anchor a guest's attestation is checked against | vCenter-issued and read host-side on vSphere; `swtpm_localca`-issued into the vTPM's NVRAM on libvirt, exported by the guest to `/run/banlieue/ek.pem` and mirrored to `VirtualMachineClaim.status` (ADR-0045); on Cloud Hypervisor, written by `swtpm_setup` at manufacture to `<state_root>/ek/<machine uid>/` (`0700 banlieue`) and read **host-side** (ADR-0065). A Cloud Hypervisor guest on an opted-in host may instead attest with `x509pop` (A-24 to A-26, TB-21); the EK path is unchanged and the guest image chooses. **On Azure none is published**: a Trusted Launch vTPM carries an AK certificate, not an EK certificate (ADR-0165), and the controller mirrors an empty list for an `AzureVirtualMachine` (`crates/banlieue-controller/src/reconciler/status_mirror.rs`); the AK publication of ADR-0165 Decision 1 is not built | Low confidentiality: it is a **public key**, deliberately readable by every reader of the claim. Its value is *integrity of binding*: it must name the VM banlieue actually created, or the consumer verifies an attestation from the wrong machine |
| A-17 | **The management cluster's datastore**: every Kubernetes object, Secrets included (A-1 hypervisor credentials, A-13 registry credentials, ServiceAccount signing material references), in plaintext unless API-server encryption at rest is configured | etcd on the controllers by default; with ADR-0085's kine, an external SQL database (PostgreSQL on the hypervisor host), reached over the network | **Critical**: read access is every Secret in the cluster, write access is cluster-admin. With kine it also lives outside every node, so it is backed up, and must be protected, as a database |
| A-18 | **`VSphereMachine` custom attributes** — `Template`, `CreatedAt`, `CreatedBy` (ADR-0089), the fleet-auditing fields `sre-automations`' `cf-node` tool also writes | vCenter `CustomFieldsManager`, per cloned VM; `CreatedBy`'s source is `VirtualMachine`'s `banlieue.io/created-by` annotation, stamped at CREATE by the optional `banlieue-virtualmachine-created-by` `MutatingAdmissionPolicy` | Zero confidentiality — all three are free text, visible to anyone with vCenter read access. **Low integrity**: `CreatedBy` can be forged (TB-1); nothing downstream trusts any of the three for an authorization decision |
| A-19 | **Telemetry**: Prometheus series on each role's `metrics` port, and, only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set, trace spans exported to a collector (ADR-0091, ADR-0092) | The scraper's storage; the operator's collector and wherever it forwards | Low confidentiality: metric labels carry no object names, namespaces or error text (`runner_tests.rs` asserts it); spans carry object names, namespaces and backend ids (node, vmid, moref, domain), never credentials, user-data or Secret contents. Its value is **integrity for operations**: forged or suppressed telemetry hides an outage, it does not change state |
| A-20 | **k0s join tokens, and a joining controller's copy of `/etc/k0s`** (ADR-0099). A worker token joins any machine as a worker; a **controller** token hands over the cluster CA and service-account signing keys, and `/etc/k0s` carries the datastore password and the VRRP secret | Minted on a controller by `join` with `--expiry=15m`, piped over SSH straight to the joining node (never written on the operator's machine), stored `0600 root` at `/etc/k0s/{worker,controller}-token-file` on that node; `/etc/k0s` crosses the same kind of pipe | Worker token: high while valid (a worker runs any pod scheduled to it and reads the Secrets those pods mount). Controller token or `/etc/k0s`: **critical**, the whole cluster and its datastore. A token is worthless after expiry; the file stays on the node, like k0sctl's and the vSphere backend's |
| A-21 | **`CARGO_REGISTRY_TOKEN`**, the crates.io API token that publishes `banlieue-api` (ADR-0100) | A GitHub Actions repository secret, read only by `build.yaml`'s `publish-crate` job on the `release` event; never available to a pull request from a fork | **High**: whoever holds it can publish any new version of `banlieue-api`, and every consumer that resolves a semver range takes it on their next build. It cannot replace a published version (uploads are immutable) and holds no cluster or hypervisor access |
| A-22 | **Provider liveness**: each provider's leader-election Lease, and the `status.workload.heartbeat` banlieue-operator derives from it (ADR-0102) | A `coordination.k8s.io` Lease named after the provider's workload, in its workload namespace; `Provider.status.workload` | Medium for **integrity and availability**: a forged `Alive` hides a dead provider and keeps VMs scheduling onto it; a forged `Stale` stops new VMs reaching a healthy one. Not a secret |
| A-23 | **The isolation of a Cloud Hypervisor sandbox segment** (ADR-0144): each sandbox tap's kernel `isolated` flag, the `isolate_ports` class declarations in the host config, and the host rules' three chains | The kernel's bridge-port flags; `[network_classes]` in `/etc/banlieue/cloud-hypervisor.toml` (root-owned, never the cluster); the iptables chains. Part of A-14's trust base, listed on its own because it is the one asset whose loss is silent | **High**: every sandbox on the host can then reach every other, or the host, or (through the host's forwarding) the host's other networks |
| A-24 | **The attestation CA's private key** (ADR-0146, ADR-0143 Decision 1): the key a Cloud Hypervisor host that opted in signs its guests' certificates with | `/var/lib/banlieue/attestation-ca/ca.key`, `0600` owned by the provider's user, in a `0700` directory (`crates/banlieue-attestation-ca/src/ca.rs`); read once, into the provider process, at start. Never in a Secret, a CR, a status, a log, or the installer's output | **High**: whoever holds it can mint a certificate for any subject under this host's CA name, until that CA is removed from the identity layer's bundle (A-26), and there is no other revocation. Unlike the swtpm CA it is made by banlieue and owned by the provider's user alone, and its name is in every agent ID it signs, so its reach is this host's guests (mediatore ADR-0049 Decision 2; not enforced by banlieue). Under ADR-0146 Decision 7 its certificate may be signed by an upstream PKI; the key is still made on the host and never leaves it. |
| A-25 | **Issued attestation certificates**: what the signer returns to a guest | On the guest's vsock, in the guest; **banlieue never stores or logs one**. Subject CN is the machine's `metadata.uid`, issuer CN the CA's name, `CA:FALSE`, `digitalSignature`, no SAN, validity from `[attestation] certificate_ttl_secs` (default 1 hour, bounds 5 minutes to 7 days) | Low alone (a certificate is public); with the guest's own private key it is that guest's identity for its remaining lifetime. A copy taken out of a guest that is then deleted verifies until it expires: §8 |
| A-26 | **The `hostCa` assertion**: the one trust anchor per host that says which CA to believe for this host's guests | `Provider.spec.attestation.hostCa` (one certificate, inline, or a ConfigMap or Secret the Provider's namespace holds), copied there by an administrator, and into the identity layer's `x509pop` bundle, which is outside banlieue. Never discovered, never verified by banlieue, never written by a host's token (ADR-0143 Decision 2, ADR-0049 Decision 10) | Zero confidentiality (public CA material). **High integrity**, the same class as A-15: a rogue certificate added here, or to the bundle, makes everything that CA signs verify under its name; removing an entry is how a CA is revoked. With Decision 7 the asserted certificate is the host CA alone, signed upstream, and the identity layer's bundle may then hold that mount's root instead of one entry per host: one global trust anchor (§8). |
| A-27 | **The pending attestation CA key and request** (ADR-0146 Decision 7): a key with no certificate yet, and the PKCS#10 request for one | `/var/lib/banlieue/attestation-ca-pending/` (directory `0700`, `ca.key` `0600`, `ca.csr` `0644`), owned by the provider's user | The key is A-24 once a certificate is issued for it; until then it signs nothing. A request that is never signed leaves a key at rest for as long as it stays (§8). The request is public |
| A-28 | **The right to have a host CA signed**: the OpenBao login role bound to one host's ServiceAccount, and the one-use token it yields (ADR-0146 Decision 8) | In OpenBao (the OpenBao repository's onboarding script); on the host only in memory for one run, or in a token file the installer removes after use | **High while it exists**: whoever holds it can obtain a CA the identity layer trusts, under one name. Bounded by pins, a five-minute one-use address-bound token and the role's deletion after the import, **none of which is in this tree** (§8) |
| A-29 | **The Azure provider's cloud identity** (ADR-0159): the projected ServiceAccount token (audience `api://AzureADTokenExchange`, 3600 s), and the ARM access token Entra ID exchanges it for, a bearer token for the identity in `Provider.spec.connection.azure` (`tenantId`, `clientId`) | The projected token: a kubelet-managed volume in the provider pod, mounted read-only and re-read on every exchange (`crates/banlieue-operator/src/workload.rs`, `azure_token_volume`; `crates/banlieue-azure/src/auth.rs`, `FederatedToken`). The ARM token: **in the provider's memory only**, never written to a file, status, log or span; `AccessToken` has a redacting `Debug` (`crates/banlieue-azure/src/token.rs`). No Secret anywhere | **High, bounded by the Azure role**: whoever holds either token acts as the identity until it expires (Entra issues the ARM token for its own lifetime, typically about an hour, and it cannot be recalled), with the custom role's actions on the one resource group it is assigned at (`deploy/provider-azure/role-definition.json`): create, power and delete VMs and NICs, delete disks, read images, quota and SKUs. No role assignment, Key Vault, image write, run-command or extension action. The federated credential trusts the cluster's ServiceAccount **issuer**, so the issuer's signing key is, for this identity, an Azure credential too (§4, §8). Since ADR-0162 the identity also reads gallery image versions (granted at the gallery, `deploy/provider-azure/README.md`) and still writes none: publishing is A-30's |
| A-30 | **The Azure publisher's cloud identity** (ADR-0159 Decision 3, ADR-0162 Decision 1): the publish Job's projected ServiceAccount token (`banlieue-azure-publisher`, audience `api://AzureADTokenExchange`, 3600 s), the ARM token Entra ID exchanges it for (identity `spec.connection.azure.publisher.clientId`), and the **write SAS** ARM returns for an upload disk (`beginGetAccess`, 3600 s, `UPLOAD_ACCESS_SECONDS`, `crates/banlieue-azure/src/gallery.rs`) | The projected token: a kubelet-managed volume in the Job's pod, read-only (`reconciler/publish_job.rs::token_volume`). The ARM token and the SAS: **in the Job's memory only**; `SasUrl`'s `Debug` and `Display` never print it, and the termination line is cut at the SAS signature (`crates/banlieue-azure/src/upload.rs`, `publish.rs::redact`). No Secret | **High, bounded by the publisher role** at the gallery's resource group (`deploy/provider-azure/publisher-role-definition.json`): create and delete image definitions, versions, disks and snapshots there, and open a disk for upload. Never the gallery itself, no VM, network, role assignment or Key Vault action. The SAS writes one upload disk's bytes until it is revoked or expires |
| A-31 | **Published image versions** (ADR-0162): what every Azure VM of a `Provider` boots, and the row that names one (`VMImage.status.perProvider[].resolvedRef`, `sourceDigest`) | The `Provider`'s Compute Gallery (`spec.connection.azure.imageGallery`), in the admin's resource group; the row in `VMImage` status, written by the Azure provider | High for **integrity**: a substituted version runs in every VM booted from it. Not a secret (a version holds the same build as A-3) |
| A-15 | **EK trust-anchor bundle** — the set of CA certificates trusted to issue this backend's vTPM EK certificates | `Provider.spec.attestation.ekTrustBundle` — inline PEM, or a ConfigMap/Secret it names in the Provider's namespace. Admin-supplied, never discovered; resolved by the **broker**, never by any banlieue identity (ADR-0049 Decision 10) | Zero confidentiality — public CA material. **High integrity**: a rogue CA added here makes every EK certificate that CA forges verify, defeating the whole attestation chain; and on libvirt *removing* a host's issuer is the revocation mechanism, so an entry an attacker can re-add is a revocation undone. Since ADR-0146 a second anchor class exists beside it, A-26 (`hostCa`), for guests that attest with `x509pop` instead of the vTPM |

## 4. Actors

| Actor | Assumed capability | Trusted? |
| --- | --- | --- |
| Cluster admin | Full Kubernetes API | Yes — out of scope by policy (`SECURITY.md`) |
| Platform admin | Creates `ProviderClass`, `Provider`, `VMImage`, and **labels a `Namespace` into the served set** (ADR-0130); and, holding `update` on the virtual subresource `providers/namespaceselector` (ADR-0133), **chooses which namespaces a shared `Provider` serves**; and **asserts each opted-in host's attestation CA** in `Provider.spec.attestation.hostCa` and in the identity layer's bundle (A-26, ADR-0146) | **Semi-trusted — must be treated as infrastructure-admin-equivalent** |
| Tenant / VM author | Creates `VirtualMachine` (and pools, claims) in a namespace the platform admin has put in the served set; **cannot serve its own namespace**, because labelling a `Namespace` is cluster-scoped | **Untrusted for confidentiality of A-1/A-2** — see §7 |
| Claim consumer / sandbox broker | Creates `VirtualMachineClaim`s, holding `create` on them in a namespace | **Bounded by admission**: `spec.subject.id` must equal the authenticated username unless the requester is a declared broker (§7.6). A broker is trusted for attribution by definition. The reference broker is mediatore, which creates each claim from an `AgentSandbox` it owns (ADR-0097); to banlieue it is one declared broker, whatever its front door |
| Compromised controller pod | RCE inside one banlieue pod | Untrusted |
| **Sandbox workload** — the AI agent (or anything else) running inside a claimed VM, including one gone hostile via prompt injection | Arbitrary code as root inside its own guest: can write the readiness marker, present or withhold an EK certificate, answer on any port, read the still-attached NoCloud seed | **Untrusted, by design** — the VM boundary is the isolation model (ADR-0047), and every TB-4/TB-5 control involving the guest assumes it is hostile. What it cannot do from inside: escape its vTPM identity (the EK private key is the one thing it cannot substitute, TB-4), reach another member's claim, rewrite its vSphere ownership marker (`vmware-rpctool` reaches only `guestinfo.*`; ADR-0101, not yet verified live, §8), or make banlieue carry a credential to it (A-9) |
| Compromised hypervisor endpoint | Attacker-controlled host reachable at `spec.connection.endpoint` | Untrusted |
| **Azure Resource Manager and Entra ID** (Microsoft's control plane, ADR-0158 to ADR-0165) | Receive A-29 on every call; authorize each call against the custom role; store each VM's `customData` (A-2) and boot-diagnostics serial log with the VM; report the provisioning state that becomes `GuestReady` (ADR-0163) and the quota that becomes capacity (ADR-0161) | **Semi-trusted, as a hypervisor is**: banlieue verifies their TLS against the system roots and bounds what it accepts from them (§6, TB-4 Azure), and cannot verify what they report |
| **Resource-group writer**: any principal in the subscription holding write or tag-write on a `Provider`'s resource group (an Azure administrator, a pipeline, another tool sharing the group) | Can create, retag, power or delete any VM, NIC or disk there, banlieue's included; can put banlieue's ownership tags (A-16) on a resource banlieue did not create | **Trusted, like the hypervisor operator** (§7.36): banlieue cannot tell a tag it wrote from one written by someone else, so a resource tagged as an orphan of this `Provider` is deleted by the sweep (§8). Since ADR-0162 the **gallery's resource group** has writers too: they can replace, retag or delete published versions (A-31), and a version they create under banlieue's deterministic name with a `VMImage`'s tags is adopted as published (§8) |
| **A principal that can obtain the Azure provider's ServiceAccount token**: whoever can create a pod as that ServiceAccount or `serviceaccounts/token` for it in the provider's namespace, the `banlieue-operator` (which renders the Deployment), and whoever holds the cluster's ServiceAccount **signing key** | Mints a token Entra ID accepts for the federated credential, and so becomes the Azure identity (A-29) | **Untrusted**; bounded by the Azure role and the one resource group. No banlieue role grants `serviceaccounts/token` (`deploy/operator/rbac/clusterrole.yaml`, `deploy/controller/rbac/clusterrole.yaml`); pod create in the provider's namespace is the platform admin's to withhold (§7.36). No provider `ClusterRole` grants Jobs: a provider's Job verbs are a `Role` in the build namespace (`crates/banlieue-operator/src/workload.rs::build_jobs_role`), and `deploy/admission/provider-jobs.yaml` refuses a Job by a provider identity that runs as anything but `banlieue-import` or `banlieue-azure-publisher`, so no provider runs a pod as its own ServiceAccount |
| **A principal that can obtain the publisher's ServiceAccount token** (ADR-0162): whoever can create a pod as `banlieue-azure-publisher` in the build namespace, which includes **the Azure provider** (it creates the publish Jobs and chooses their image and arguments) and anyone else with pod or Job create there, and whoever holds the cluster's ServiceAccount signing key | Mints a token Entra ID accepts for every publisher identity whose federated credential trusts that one subject, and so can write and delete images in those galleries (A-30) | **Untrusted**; bounded by the publisher role at each gallery's resource group. Pod create in the build namespace is already node-root-equivalent (TB-3); the Azure provider's reach to it is its build-namespace Job `Role`, which `deploy/admission/provider-jobs.yaml` bounds to the publisher and import identities and a restricted pod |
| **Compromised VMM** (Cloud Hypervisor) | A guest that has escaped into its VMM process: code execution as that guest's host uid, with `kvm` and write access to its own two directories; `/dev` (so `/dev/shm`) read-only and an IPC namespace of its own | **Untrusted.** The unit's sandbox and the per-guest identity are what contain it (TB-9); the provider must never act on anything it can influence without checking |
| **Another tenant of a Cloud Hypervisor host** (container runtime, kubelet networking, a firewall manager) | Sets `ip_forward`, the FORWARD policy and `br_netfilter` on the same kernel, and can reload or reorder the firewall at any time; on the first sandbox host it had already set all three | Outside banlieue's control and not hostile, but its changes can silently displace or bypass the sandbox rules (TB-19, §8) |
| **A guest, or its VMM, at its own certificate signer** (ADR-0146) | Connects to the one per-guest vsock socket (`vsock.sock_1025`) that is its own, sends up to 4096 bytes, chooses its own EC P-256 key, and retries within a rate limit. Cannot choose the subject, the extensions, the validity or the issuer | **Untrusted, by design**: it is the Sandbox workload above at one more door. What it gets is a certificate for *itself* only (TB-21); it never reaches the CA key (A-24), another guest's socket, or the CA directory |
| **A compromised provider process, or root on a Cloud Hypervisor host, as a certificate issuer** (ADR-0146) | Reads the CA key (A-24) and chooses the subject it signs for | **Untrusted for integrity of its own guests' identities, accepted** (§8): already a host-level compromise (it also reads every guest's disk, A-12). Bounded, by mediatore's design, to agent IDs under this host's CA name, so it speaks for this host's guests and no other host's |
| **A PKI operator (an OpenBao administrator)** (ADR-0146 Decisions 7 and 8) | Signs host CAs on a dedicated mount; writes each host's policy and login role, checks name uniqueness and deletes the role after the import | **Trusted for the whole `x509pop` trust root**: whoever can sign on that mount can mint a CA the bundle trusts for any name (§8). Outside banlieue; named here because the bundle is only as good as that mount |
| **A compromised host, as an OpenBao login** (ADR-0146 Decision 8) | Holds the host's ServiceAccount token (A-11); until the role is deleted it can log in again to its own role and obtain a CA under its **own** name only | **Untrusted**; bounded by the pins, the one-use token, the address binding and the role's deletion, all on the OpenBao side and not in this tree (§8) |
| **Stolen Proxmox API token** | Holds A-1 for Proxmox, without the cluster | Untrusted; bounded by the token's **ACLs**, not the user's: it is privilege-separated and holds only the `BanlieueProvider` role on `/vms`, one node, one SDN zone and the ISO storage — it can create and destroy VMs there, and cannot open a console, run commands in a guest, migrate, snapshot, or change permissions (`scripts/bootstrap-proxmox-host.sh`, `ROLE_PRIVS`). Its one storage-deleting privilege, `Datastore.Allocate`, is held **only on `banlieue-seed`**, a storage holding nothing but seed ISOs (`SEED_ROLE_PRIVS`): it cannot delete backups, templates or ISOs elsewhere, nor edit another storage's definition |
| **Stolen host credential** (Cloud Hypervisor) | Holds A-11 — the provider's ServiceAccount token — without the host | Untrusted; bounded by the provider's namespaced RBAC (TB-8) and the token's lifetime |
| **OCI registry** and whoever operates it (ADR-0064) | Stores every pushed build; can read, withhold or delete one. With a private CA (ADR-0110), the CA that signs its certificate is trusted by the push Job, so whoever controls that CA can also read the push credential and the artifact in transit | **Untrusted for integrity** — hosts pull by digest and verify it, so the registry cannot substitute content. **Trusted for confidentiality and availability**: it sees every image in full (§7.13) |
| **Bootstrap operator** running `scripts/bootstrap-k0s-cluster.sh` with Cloud Hypervisor nodes (the default VMM) or `join` (ADR-0099) | Passwordless **sudo on the hypervisor host** (CH nodes; only `DEFAULT_VMM=libvirt` with no CH node avoids it), and an SSH key every management node trusts | Trusted, like the hypervisor operator. `join` asks it to hold no new secret: the token goes controller → node over its SSH sessions |
| **Host operator running `banlieue host cloud-hypervisor install`** | Root on the host, for one command | Trusted, like the hypervisor operator; the installer's controls protect the host **from the provider's user** during that run, not from the operator |
| External contributor | Opens a PR from a fork | Untrusted |
| **Rust consumer of `banlieue-api`** (ADR-0100): a broker such as mediatore, an operator's tooling | Builds against the published types and calls the API server with **its own** credential | Neither more nor less trusted than any API client: the crate carries types only (no client, credential or TLS stack), so it grants nothing that RBAC and admission (TB-1, TB-7) do not |
| **Cluster API's manager** (CAPI core, installed by the platform operator; `clusterctl init --infrastructure banlieue`, ADR-0096), and **KubeadmControlPlane's manager**, which clones control-plane machines from a template itself (ADR-0096, amended 2026-11-14) | Creates, updates and deletes banlieue **infrastructure** objects it owns through `Machine`s, `MachineSet`s and, for KCP, its control plane, through the aggregated `banlieue-capi-infrastructure` ClusterRole, which carries both managers' aggregation labels | Semi-trusted, like any controller the platform operator installs. **Bounded to `infrastructure.banlieue.io`** machines, templates and clusters: nothing in `banlieue.io`, no Secrets, IPAM, Events or Leases (TB-15) |
| **Cluster API Machine author**: whoever creates `Machine`s, `MachineSet`s or `MachineDeployment`s in a namespace that holds banlieue InfraMachines | Can name a bootstrap Secret directly in `spec.bootstrap.dataSecretName`, which banlieue-controller copies into an InfraMachine it can read (ADR-0129) | **Bounded by admission**: may name only a Secret it can itself `get` (`deploy/admission/capi-bootstrap-data-authorization.yaml`, Decision 5). Trusted with join data for its own cluster, as Cluster API trusts it |
| **Telemetry consumers**: the metrics scraper (from a namespace labelled `banlieue.io/monitoring=true`) and, if configured, the OTLP collector | Read A-19; the collector receives spans pushed to it | Trusted for confidentiality of what they receive, untrusted for integrity of anything banlieue does: nothing banlieue decides reads telemetry back (TB-14) |
| **OIDC identity provider** (and any bridge in front of it, e.g. Dex for GitHub) | Mints the ID tokens the API server accepts, and therefore **decides what `request.userInfo.username` is** | **Semi-trusted, and entirely outside banlieue's control.** Every guarantee the claim-subject policy makes is downstream of this actor: banlieue checks `subject.id` against a username it did not derive. Compromise or misconfiguration here makes every claim attribution meaningless — §8 |
| Hypervisor operator | vCenter/libvirt/Proxmox privileges outside Kubernetes; root on a Cloud Hypervisor host | Semi-trusted: **can read datastores, storage pools and Proxmox storages banlieue writes to** (including a Proxmox machine's seed ISO), **can edit or copy the ownership marker on a Proxmox VM, or on a vSphere VM with `VirtualMachine.Config.AdvancedConfig`** (§8), and on libvirt can read swtpm state on the host filesystem. On a Cloud Hypervisor host, root can read every guest's disk, seed and memory, the provider's cluster token (A-11), and the attestation CA key (A-24). Can also **plant or replace the template a `Template`-source image resolves to** in a zone's folder, which banlieue trusts by name (ADR-0104, §8) |

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
                    │             Proxmox VE (HTTPS + API token; ADR-0074)    │
                    │  datastores / storage pools, VMs & domains, vTPM/swtpm  │
                    │  a vSphere VM: one NIC per declared interface (ADR-0111)│
                    │  guest → qemu-guest-agent → EK cert, phase (read-only)  │
                    └────────────────────────────────────────────────────────┘

  TB-4, Azure (ADR-0158 to ADR-0165): a cloud API instead of a hypervisor; no Secret, no A-1
  ┌─ banlieue-system: banlieue-provider-azure (per-Provider SA; Role: no Secret, no ConfigMap) ─
  │ projected SA token (aud api://AzureADTokenExchange, 1 h, read-only mount, re-read per exchange)
  │ ARM token in memory only; no redirects; capped replies; every token-bearing URL, including
  │ an operation URL read back from status.pendingOperation and every nextLink, must sit under
  │ spec.connection.endpoint, or it is not sent
  └──────┬────────────────────────────────────────┬──────────────────────────────
         │ HTTPS :443, SA token                   │ HTTPS :443, ARM token
         │ (egress any address: not IP-pinnable)  │
         ▼                                        ▼
  Entra ID: SA token ──▶ ARM token          Azure Resource Manager, one resource group, custom role:
         ▲ federated credential: issuer +     NIC + VM (tags: owner uid, kind, namespace, provider),
         │ subject (this SA) + audience       customData (A-2), throwaway SSH key, no public IP,
         │                                     boot diagnostics; quota and SKU reads
  cluster SA issuer: discovery + JWKS        orphan sweep: this Provider's tag, owner uid not listed,
  published by the cluster admin              older than 15 min, 3 deletes per pass, home namespace
  (ADR-0159 Decision 2; not banlieue's)       only unless the provider serves tenants
  resource-group writers (tag-write) ── outside banlieue, trusted (§4, §8)
  inside each Azure guest (ADR-0163, ADR-0171), root, gated on the DMI asset tag:
    provisioning disk (UDF, local, read-only) ── ovf-env.xml ──▶ banlieue-azure-guest customdata
        ──▶ /oem/91_azure_customdata.yaml (0600, persists on COS_OEM)
    banlieue-azure-guest ready ── HTTP :80, host-local, no TLS ──▶ WireServer 168.63.129.16
        (goal state in: three identifiers; Health "Ready" out; no credential either way)

  TB-23, Azure image publishing (ADR-0162): a second identity, in the build namespace
  banlieue-provider-azure ── creates Job (Role in this namespace + VAP) ──┐
  ┌─ banlieue-imagebuild (privileged PSA; this pod restricted) ──────────▼──────────────
  │ publish Job as banlieue-azure-publisher: no RBAC, no API token, artifacts PVC read-only
  │ sha256 of the build checked first; projected SA token (aud api://AzureADTokenExchange)
  │ termination line (version id, digest, min disk size; never a token or SAS) ──▶ provider
  └──────┬───────────────────────┬─────────────────────────────────┬──────────────────
         │ HTTPS, SA token       │ HTTPS, ARM token (publisher)    │ HTTPS, write SAS only,
         ▼                       ▼                                 ▼ no bearer, no redirects
  Entra ID (federated       ARM, gallery resource group,      managed-disk upload host, pinned
  credential: issuer +      publisher role: definitions,      suffix per cloud: Put Page into
  publisher SA subject)     versions, disks, snapshots        one upload disk (SAS <= 1 h)
                            tags: owner uid + source sha256 ──▶ version read by the provider
                            (runtime identity: read only) ──▶ VMImage row ──▶ controller boots
                            it only when sourceDigest == the build's checksum
  gallery resource-group writers ── outside banlieue, trusted (§4, §8)

  TB-1 served set (ADR-0130): which tenant namespaces the controller acts on
         platform admin labels a Namespace ──▶ matches --namespace-selector
         controller ──list/watch, read-only──▶ Namespaces matching the selector
         object in a served namespace ──▶ reconciled; otherwise skipped, not deleted
         default with no flag: the controller's own namespace, never every namespace
  TB-1 served set, operator (ADR-0131): which tenant namespaces' Providers get a workload
         operator ──list/watch, read-only──▶ Namespaces matching --watch-namespace-selector
         Provider in a served namespace ──▶ workload rendered; otherwise skipped, not deleted
         provider binaries without a selector: one namespace, never every namespace
  TB-1 shared provider (ADR-0133): a tenant namespace is served without holding a credential
         Provider (platform ns, spec.namespaceSelector) ──operator renders──▶ provider --watch-namespace-selector
         provider pod ──watch──▶ VSphereMachine in each selected tenant namespace (status, finalizer only)
         provider pod ──get, home namespace only──▶ Provider, credentials Secret, CA bundle (A-1)
         controller ──▶ machine in the VM's own namespace, providerRef.namespace set (only the controller may: VAP)
         tenant namespace: no provider, no credential, no verb on machines, no write of providerRef.namespace

  TB-6: GitHub Actions / GHCR ──▶ released images & binaries
        GitHub Actions ──▶ crates.io ──▶ banlieue-api in consumers' builds
        (A-21 token, release job only; ADR-0100)

  TB-12: management cluster controllers (kine) ──▶ SQL datastore on the host
         (A-17; TLS, password from an untracked env file; ADR-0085)

  TB-13: LAN / tailnet clients ──▶ control plane VIP (keepalived, VRRP)
         ──▶ k0s reverse proxy ──▶ any controller's API server
         (TLS passes through; konnectivity off, API servers reach kubelets directly; ADR-0086)

  TB-14: metrics scraper (banlieue.io/monitoring ns) ──▶ :8080 /metrics on every role
         kubelet ──▶ :8081 /livez /readyz;  role ──▶ OTLP collector (opt-in, ADR-0092)

  TB-15: CAPI manager ──▶ API server ──▶ infrastructure.banlieue.io objects only
         (aggregated banlieue-capi-infrastructure ClusterRole, ADR-0096)
         Machine author ──[admission: may name only a Secret it can get]──▶ dataSecretName
         controller ──watch──▶ Machines ──get by name (namespaced Role)──▶ bootstrap Secret
           ──classify──▶ InfraMachine spec.userData + BootstrapDataReady
           ──▶ host provider creates the guest only then (ADR-0129)

  TB-16: bootstrap operator (SSH key) ──▶ controller: k0s token create (15 min)
         ──pipe──▶ joining node: /etc/k0s/<role>-token-file (0600) ──▶ k0s worker
         controller: tar /etc/k0s (no tokens) ──pipe──▶ joining controller ──▶ k0s controller
         (never written on the operator's machine; never writes a controller; ADR-0099)

  TB-17: provider pod / host ──▶ its own Lease only (namespaced, resourceNames)
         operator ──get──▶ that Lease ──▶ Provider.status.workload.heartbeat
         controller scheduler ──▶ skips a Stale Provider (ADR-0102)

  TB-18: sandbox members (one NIC, static, no default route)
           ──▶ sandbox segment (layer 2, no router, no DHCP, no resolver)
           ──mTLS CONNECT──▶ egress gateway VM: sandbox NIC │ egress NIC (ip_forward=0)
           ──▶ routable network, from the gateway's own address (ADR-0109)

  TB-19, TB-20 (ADR-0144): a Cloud Hypervisor host serving sandboxes; bridges and rules are the operator's
  ┌─ one Cloud Hypervisor host, serving sandboxes ───────────────────────────────────────────┐
  │ sandbox A (isolated port)                              sandbox B (isolated port)         │
  │       │  reaches only non-isolated ports (the gateways)      │                           │
  │   ┌───┴──────────────────────────────────────────────────────┴───┐                       │
  │   │ br-sbx: no uplink, no host address, no router        TB-19   │                       │
  │   └───┬──────────────────────────────────────────────────────┬───┘                       │
  │       │ non-isolating class, same bridge                     │                           │
  │   gateway A (NICs: br-sbx, br-up)              gateway B (NICs: br-sbx, br-up)           │
  │       │                                                      │                           │
  │   ┌───┴──────────────────────────────────────────────────────┴───┐                       │
  │   │ br-up: carries the host's own address                TB-20   │                       │
  │   └─────────────────────────────┬────────────────────────────────┘                       │
  │                                 ▼                                                        │
  │   host NAT: masquerade the uplink range out of every interface but br-up                 │
  │                                 │                                                        │
  │                                 ▼  (the host's existing address, no other)               │
  │                         routable network                                                 │
  │                                                                                          │
  │   provider (CAP_NET_ADMIN) ──one rtnetlink request per sandbox tap──▶ isolated on        │
  │   host rules: br-sbx ──INPUT drop, FORWARD drop──▶ host and beyond   (BANLIEUE_SBX_*)    │
  │   another tenant of the host (ip_forward, FORWARD policy, br_netfilter) ── not ours ──   │
  └──────────────────────────────────────────────────────────────────────────────────────────┘

  TB-21 (ADR-0146): a Cloud Hypervisor host that opted in to x509pop attestation
  ┌─ one Cloud Hypervisor host, [attestation] in /etc/banlieue/cloud-hypervisor.toml ─────────┐
  │ guest A: own key, CSR   (the helper is NOT built; mediatore's)         guest B            │
  │       │ vsock port 1025                                                   │               │
  │       ▼                                                                   ▼               │
  │ <run dir A>/vsock.sock_1025 (guest A's uid, 0660)       <run dir B>/vsock.sock_1025       │
  │       │ one request per connection: <= 4096 bytes, 5 s read, 8 slots, 6 per minute        │
  │       ▼                                                                                   │
  │ provider process (user banlieue): signer. Takes the EC P-256 public key only; subject =   │
  │ the machine uid; CA:FALSE; short validity; counters, never a CSR or certificate in a log  │
  │       │ ca.key read once, at start (refused unless 0600, dir 0700, owned by banlieue)     │
  │       ▼                                                                                   │
  │ /var/lib/banlieue/attestation-ca/   ca.key 0600 (A-24)   ca.crt 0644                      │
  │ no guest uid can traverse it; the VMM and swtpm run as the guest's own uid                │
  └───────────────────────────────────────────────────────────────────────────────────────────┘
  administrator ──copies ca.crt──▶ Provider.spec.attestation.hostCa (A-26)  and  the identity layer's
                                   x509pop bundle (outside banlieue; the CA name must be unique there)
  provider ──reads its own Provider only──▶ no hostCa: Ready=False, HostCaMissing, no guest created
  host token ──get, list, watch──▶ its own Provider (never writes spec, so never writes hostCa)
  certificate (CN = machine uid, issuer CN = CA name) ──▶ guest ──x509pop──▶ SPIRE (mediatore's)

  TB-22 (ADR-0146 Decision 8): the installer has OpenBao sign the host CA's request, once
  ┌─ one Cloud Hypervisor host ──────────────────────────────────────────────────────────────┐
  │ install --only attestation (root, once, at an operator's request)                        │
  │   1. key + request ──▶ /var/lib/banlieue/attestation-ca-pending/  (A-27, key 0600)       │
  │   2. reads /etc/banlieue/credentials/token (the host's ServiceAccount token, A-11)       │
  └──────────┬───────────────────────────────────────────────────────────────────────────────┘
             │ HTTPS only, the host's trust store, no skip flag, 30 s, 1 MiB reply cap
             ▼                                                                       TB-22
  OpenBao (administrator-operated, outside banlieue): JWT login (role bound to this host's
  ServiceAccount) ──▶ a 5-minute, one-use, address-bound token ──▶ root/sign-intermediate on a
  dedicated mount, name, path length 0 and ttl pinned by policy        [OpenBao repository; A-28]
             │ certificate + chain: text, unverified
             ▼
  installer: PendingCa::import checks key, name, CA:TRUE, path length 0, keyCertSign, validity and
  chain, then moves the key into attestation-ca/ and writes [attestation]; a refusal installs nothing
  (the controller and the provider have no part in this: no standing PKI credential anywhere)

  KVM host, management nodes on Cloud Hypervisor (ADR-0099; no banlieue component):
    bootstrap operator ──sudo──▶ /etc/systemd/system/k0s-vm-<n>{,-net,-swtpm}.service
    systemd ──▶ k0s-vm-<n>-net    root oneshot: tap kv-<n> owned by k0s-vm ──▶ bridge
            ──▶ k0s-vm-<n>-swtpm  as k0s-vm, state in CH_VM_DIR/<n>/tpm (0700)
            ──▶ k0s-vm-<n>        VMM as k0s-vm (kvm, no caps), sandbox + Landlock,
                                  writes CH_VM_DIR/<n>/ (0750) and /run/k0s-vm-<n>/ (0700)
    (k0s-vm is shared by every management node on the host; banlieue guests'
     uids and 2770 dirs are separate from it ─ TB-9)

  TB-10: push Job (banlieue-imagebuild, no SA token; public roots + one
         operator-named CA bundle key, ADR-0110) ──▶ OCI registry
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
  │              guests in banlieue-guests.slice ⊂ banlieue.slice (ADR-0105/6)   │
  │                                                                             │
  │  banlieue host cloud-hypervisor install (root, once) ──TB-11──▶ /opt, /etc, │
  │     ▲  in banlieue-owned dirs by O_NOFOLLOW   units, polkit, accounts, EK CA │
  │     └── release assets: github.com or an operator mirror, HTTPS, each file   │
  │         sha256-verified (compiled pin, operator flag, or api.github.com)     │
  └─────────────────────────────────────────────────────────────────────────────┘
```

| ID | Boundary | Crossing |
| --- | --- | --- |
| TB-1 | Tenant namespace → `banlieue-system` | A `VirtualMachine` causes privileged work in another namespace. Since ADR-0130 a namespace is a tenant **only if the platform admin has put it in the served set**; since ADR-0131 the same holds for the operator, whose served set decides where `Provider` workloads are rendered |
| TB-2 | Control-plane pod → Kubernetes Secrets | Credential and user-data reads |
| TB-3 | `banlieue-system` (restricted) → `banlieue-imagebuild` (privileged) | Image builds and per-zone imports. The imagebuilder and provider **pods** run in `banlieue-system`; what sits across the boundary is the build inputs (cloud-config Secrets), the import Job and kairos' privileged builder, so both the imagebuilder's cloud-config `Role` (ADR-0041) and the operator-minted import `Role` are cross-namespace bindings. Since ADR-0162 the Azure provider also creates its publish Jobs here. The vSphere, libvirt and Azure providers' Job verbs are, like the import `Role`, cross-namespace bindings: an operator-rendered `Role` in this namespace bound to the provider's ServiceAccount (`crates/banlieue-operator/src/workload.rs::build_jobs_role`), with `deploy/admission/provider-jobs.yaml` as the floor Pod Security does not give here |
| TB-4 | Cluster → hypervisor | Authenticated API calls carrying A-1 (vCenter, libvirtd, Proxmox VE); on Azure, calls to Entra ID carrying the projected ServiceAccount token and to Resource Manager carrying the ARM token (A-29), where the cloud's trust in the cluster's ServiceAccount issuer, published by the cluster admin (ADR-0159 Decision 2), is what makes the token a credential |
| TB-5 | Cluster → shared datastore | ISO/disk artifacts written to storage other people can read |
| TB-6 | Contributor / CI → published artifact | Build and release |
| TB-7 | External identity provider → API server | The assertion of *who the caller is*, on which the whole claim attribution model rests |
| TB-8 | Cloud Hypervisor host ↔ cluster | A process on the hypervisor holds a cluster credential and acts on cluster state; cluster state tells a host what to run |
| TB-9 | Guest VMM → host and other guests | Guest code that escapes into its VMM runs on the host, next to the provider and every other guest |
| TB-10 | Build namespace → OCI registry → Cloud Hypervisor host | A build leaves the cluster for a registry the operator runs, and enters a host that cannot mount cluster storage (ADR-0064) |
| TB-12 | Management cluster controllers → external datastore | With kine (ADR-0085), cluster state leaves the controllers for a database on another host; whoever reaches that database with the credential holds the cluster |
| TB-13 | LAN and tailnet → management cluster control plane VIP | With `API_VIP` (ADR-0086), every client outside the cluster, kube-proxy and joining workers reach the API through an address that VRRP elects among the controllers, an election anything on the LAN can take part in |
| TB-14 | banlieue pods ↔ telemetry consumers | Inbound scrapes of `metrics` and probes of `health`; outbound OTLP spans when configured (ADR-0091 to ADR-0093) |
| TB-15 | CAPI controllers → banlieue infrastructure objects | CAPI's manager creates, owns and deletes infrastructure machines through its aggregated role (ADR-0096); banlieue-controller copies a Machine's bootstrap Secret, a join credential, into the InfraMachine it owns, where anyone who can read that InfraMachine can read it (ADR-0129) |
| TB-16 | Bootstrap operator → running management cluster (`join`) | A join credential (A-20) is minted on a controller and carried to a new node, which then joins a cluster whose controllers the script does not reconfigure; a joining controller also receives a running controller's `/etc/k0s` (ADR-0099) |
| TB-17 | Provider identity → its Lease → operator → scheduler | Since ADR-0102 a Lease a provider writes is read back as an input to placement: whoever can write it can hide a dead provider or starve a live one |
| TB-18 | Sandbox segment → egress gateway → routable network | Untrusted code on the segment reaches the outside only through a gateway VM, the one component with a NIC on both sides (ADR-0109) |
| TB-19 | Cloud Hypervisor sandbox segment → host and other sandboxes | On a host serving sandboxes (ADR-0144), guests that run code nobody reviewed share one bridge with no uplink; what keeps each from its neighbours and from the host is kernel port isolation, which the provider sets, and the host rules, which the operator installs |
| TB-20 | Uplink segment → host NAT → routable network | The two gateway VMs reach the outside only through the host, which masquerades them out of its own address (ADR-0144); the host is the one component that routes between the uplink and everything else it can reach |
| TB-21 | Guest → per-guest vsock signer → attestation CA | On a host that opted in (ADR-0146), a guest that runs code nobody reviewed sends one bounded request to a socket the provider serves, and gets back a certificate the identity layer will accept as that guest. The provider is the one component that holds the signing key; the trust anchor that makes the certificate count (A-26) is asserted by an administrator and lives outside banlieue |
| TB-22 | Installer (root, host) → OpenBao PKI | On the OpenBao route (ADR-0146 Decision 8) the installer makes one HTTPS exchange with an OpenBao an administrator names: a login with the host's ServiceAccount token, one signing of the pending request, a revoke. What OpenBao will sign is decided on the OpenBao side; what the installer will accept back is decided here |
| TB-23 | Build namespace → Azure Compute Gallery and managed-disk upload host | The publish Job (ADR-0162) presents the publisher's projected token to Entra ID, spends the ARM token in the gallery's resource group, and sends the build's bytes with a write SAS to the upload host ARM names; what lands in the gallery is what every Azure VM of a `Provider` boots |
| TB-11 | Installer (root, once) → Cloud Hypervisor host | A root process writes the host's trust base (A-14), in part inside directories the provider's user owns, from artifacts downloaded from upstream or an operator's mirror (ADR-0067, ADR-0084) |

## 6. Threats by boundary

### TB-1 — Tenant namespace → control plane

| Threat | STRIDE | Control |
| --- | --- | --- |
| A `Provider` points at an attacker host and ships real credentials to it | S, I | `banlieue-provider-connection` VAP: absolute URL, `https://` for vsphere/proxmox/azure, no plain `http://`, no `@` userinfo, no `#` fragment (`deploy/admission/provider-connection.yaml`). The Azure clients refuse a non-HTTPS ARM endpoint or Entra authority themselves, whatever admission did (`is_trusted_base`, `crates/banlieue-azure/src/operation.rs`; used by `ArmClient::new` and `FederatedToken::new`) |
| An azure `Provider` is given a stored credential after all, or another class is given Azure identity fields | S, E | The same VAP: an azure `Provider` must carry `spec.connection.azure` and must **not** carry `credentialsRef`, and `spec.connection.azure` is refused on any other class (ADR-0159 Decision 1), so no Secret is ever named for, or read by, an Azure provider |
| A `VMImage` with an azure source declares `installMode: deferred`, which no cloud VM can complete (no install-media path), so every machine fails at create | D | `banlieue-vmimage-cloud-install-mode` VAP refuses the combination at admission (ADR-0162 Decision 7): `deploy/admission/vmimage-cloud-install-mode.yaml` |
| TLS verification silently disabled | T, I | Same VAP: `insecureSkipTLSVerify: true` requires the explicit, auditable annotation `banlieue.io/allow-insecure-tls: "true"` |
| A `Provider` names a Secret its author cannot read (confused deputy), or an existing Provider's credential is re-pointed at another endpoint, CA or `insecureSkipTLSVerify` by someone who could not name it | E, I | `banlieue-provider-credentialsref-authorization` VAP uses the CEL `authorizer`: whoever creates a Provider, or changes anything in its `spec.connection` (structural equality over the whole connection), must be able to `get` the Secret or `reference` it (ADR-0157). A write that leaves the connection unchanged is not checked: it moves no credential. `crates/banlieue-controller/tests/live_credentialsref_policy.rs`: a reader and a referrer admitted, a writer with neither refused on create and on an endpoint, CA or verification change with the credential unchanged, a referrer refused for a Secret it was not granted, a label and a capability change admitted without either. A principal granted `reference` is §8's. |
| A `ProviderClass` mints RBAC or places pods in `kube-system` | E | `banlieue-providerclass-guardrails` VAP: `additionalRules` may not name `secrets`, `*`, `escalate`, `bind`, `impersonate`; system namespaces rejected for `workloadNamespace` |
| Ref-swapping an existing VM onto another class/image | T | `banlieue-virtualmachine-immutable-refs`, `banlieue-provider-immutable-class` VAPs |
| Resource-exhaustion via absurd specs (`numCpus`, disk counts) | D | schemars `range`/`length`/`maxItems` constraints on `VMClass`, `VSphereMachine` and `ProxmoxMachine` (`cores`, `memoryMiB`, `dataDisks` ≤ 30, `nics` ≤ 16); cross-field rules (a seed needs `isoStorage`, unique NIC and disk names, VLAN 1–4094) in `ProxmoxMachineSpec::validate`, called by the controller when it builds the machine — `crates/banlieue-api/src/infrastructure/proxmox_machine.rs` |
| A `VirtualMachine` names user-data its author cannot read (confused deputy) | E, I | `banlieue-virtualmachine-userdata-authorization` VAP (ADR-0042) uses the CEL `authorizer`: the creating principal must itself be able to `get` the Secret / ConfigMap named by `spec.userData`, in the `VirtualMachine`'s own namespace — `deploy/admission/virtualmachine-userdata-authorization.yaml` |
| A tenant's static-address strings (address, gateway, nameservers, domain) add keys to a guest's NoCloud `network-config` (ADR-0114) | T | The one renderer, `banlieue_provider_sdk::cloudinit::network_config` (`crates/banlieue-provider-sdk/src/cloudinit/network.rs`), single-quotes every value and flattens line breaks, so a value stays one scalar (`quotes_in_values_cannot_break_the_document`). The author gains nothing new: the same principal already controls the guest's user-data, which the seed runs as root |
| A consumer takes a **declared** address in status as one the guest answers on (ADR-0114, amendment 2026-11-06) | S, T | A running static machine with no observed address reports its static NICs' declared addresses with `addressSource: Static`, never as an observation: `banlieue_provider_sdk::status::declared_addresses` (`crates/banlieue-provider-sdk/src/status.rs`), applied in libvirt's `converge` (`crates/banlieue-provider-libvirt/src/reconciler/libvirtmachine.rs`) and Cloud Hypervisor's `build_status` (`crates/banlieue-provider-cloud-hypervisor/src/machine.rs`). Any observed address replaces it, a stopped machine reports none, and the guides say `Static` means configured, not proven: `GuestReady` or an observed source is the proof. A tenant cannot use it to show another machine's address: a static address already held by another VM is refused before any infra CR exists (`address_conflict.rs`, ADR-0083, the row below) |
| Rendered user-data is readable from `VSphereMachine.spec` / `LibvirtMachine.spec` | I | **No code control — this is the accepted reflection of ADR-0025.** See §7.1 and §8 |
| **Two claims bind the same warm member**, so one VM is handed to two subjects | I, E | The bind is a JSON merge patch carrying the member's `resourceVersion` **from the list it was picked from**, so a member written since the snapshot is rejected `409` (or `404` if deleted) and the loser re-picks (`crates/banlieue-controller/src/reconciler/claim.rs`: `pick` returns the listed version, `bind` never re-reads the member). Re-reading it before the patch returned the winner's version and let the loser overwrite the label: found 2026-11-16 by `tests/live_claim.rs::two_claims_cannot_bind_the_same_member` against a real API server, timing-dependent and latent before ADR-0132. A member already carrying `banlieue.io/claim` reads as `MemberPhase::Claimed` (`reconciler/pool.rs::member_view`) and `pick_member` filters to `Ready` only (`reconciler/claim_plan.rs`) |
| **One claim binds two warm members**, so one subject drains the pool's warm capacity and holds a sandbox nobody tracks | D, E | Before picking, the reconciler looks in the fresh member list for a member already labelled for this claim and finishes that bind instead — `reconciler/claim.rs::pick`, `reconciler/claim_plan.rs::member_bound_to`. Found live on 2026-09-28 (a reconcile from a stale cached claim); regression test `tests/live_claim.rs::a_stale_copy_of_a_bound_claim_does_not_bind_a_second_member` against a real API server |
| **A released member is recycled to a second subject** | I | Release is always deletion: `claim_plan.rs::next_step` has no transition back to an unclaimed state, and `pool_plan`'s invariants 1–2 keep the pool from reclaiming a labelled member. The VM is the isolation boundary, so reuse is the one outcome the design must exclude (ADR-0047) |
| **A stale-image member is handed to a fresh claim** after the pool's `VMImage` moved on: an old userland, old agent, old patch level | T | Bounded, not prevented: `rank_pick` prefers a member current for its **own** pool's image revision and falls back to a stale one only when nothing fresh is `Ready` (`crates/banlieue-controller/src/reconciler/claim_plan.rs`). The revision ranks before host health and before every selection strategy (ADR-0135 Decision 2; `claim_plan_tests.rs::current_revision_still_beats_host_health`), so no placement preference keeps a stale member in circulation once a fresh one is Ready; `spec.maxIdleSeconds` reaps members that sat unclaimed too long (`reconciler/pool_plan.rs`, invariant 5); the mandatory claim TTL bounds how long a stale member lives once bound; and a pool rollout replaces unclaimed stale members while holding `available ≥ warmReplicas` (ADR-0046). A claim that must never get a stale member is a claim the pool should refuse instead, which is not modelled today |
| **A claim attributes a sandbox to a subject that never requested one** | S, R | `banlieue-virtualmachineclaim-subject-authorization` VAP (ADR-0047 Decision 10): `spec.subject.id` must equal the authenticated username, `subject.issuer` must be in an operator allowlist, and `spec` is immutable so the check cannot be undone by a later patch — `deploy/admission/virtualmachineclaim-subject-authorization.yaml`. Declared brokers are exempt from the id check by design (§7.6) |
| A credential is written into `spec.subject`, which is world-readable in the namespace and copied onto the member | I | Partly controlled: `subject.id` must now equal the authenticated username, so it cannot be an arbitrary string, and `issuer` is allowlisted. Neither stops a determined author from putting a secret in a field shaped like a username — banlieue never reads it as a credential and never forwards it to a guest, but nothing rejects one. See §7.6, §7.10 |
| `delete virtualmachineclaims` destroys running VMs | D | Equivalent to `delete virtualmachines` by design — releasing a claim *is* destroying the sandbox. RBAC is the only control; §7.10 |
| The controller's `patch` on `virtualmachineclaims` is used to rewrite a claim's `spec` (its `poolRef` or `poolSelector`, subject or TTL) rather than only its finalizer | T, E | Bounded: the reconciler's only writes to the claim are `metadata.finalizers` (`banlieue-provider-sdk/src/finalizer.rs`) and status; the claim's `spec` is immutable at admission (`deploy/admission/virtualmachineclaim-subject-authorization.yaml`; setting or clearing either pool field, or `spec.selection`, counts as a change, ADR-0132 Decision 8, ADR-0135 Decision 7; `banlieue-operator/tests/admission_policies.rs::the_claim_policy_freezes_selection_with_optional_access`), so a patch from any principal cannot change it. The grant exists because a finalizer is a merge patch on the object itself (`deploy/controller/rbac/clusterrole.yaml`; `bootstrap_tests.rs::the_controller_may_patch_every_kind_it_finalizes`) |
| A **selector claim is steered to a pool its author did not intend**: someone labels a pool so it matches (one with a stale image, no vTPM, or a weaker readiness gate), or the claim's own selector is broader than meant | T, E | Bounded: pools are matched only in the claim's own namespace (`reconciler/claim.rs::pick` lists with `Api::namespaced`), so the reach is what a `poolRef` there already had (ADR-0132 Decision 3). Relabelling a pool needs `update` on `virtualmachinepools` in that namespace, which already permits rewriting the pool's template and image, strictly more. "Any pool" must be written as `poolSelector: {}`; neither field, or both, is refused by the CRD's CEL rule (`crates/banlieue-api/src/banlieue/virtualmachineclaim.rs`, `deploy/crds/banlieue.io_virtualmachineclaims.yaml`). `status.poolRef` records which pool supplied the VM. A member with no vTPM mirrors no EK certificate, so a consumer that attests (ADR-0049) fails closed on it rather than trusting it |
| The claim admission policy **fails closed on a claim missing a pool field**, so every UPDATE to a selector claim is denied, the controller's finalizer included, and such claims are never reconciled | D | The immutability rule compares `object.spec.?poolRef == oldObject.spec.?poolRef` (and likewise `poolSelector`) with CEL optionals, which are defined for an absent field where `==` on it errors (`deploy/admission/virtualmachineclaim-subject-authorization.yaml`). Regression test `crates/banlieue-controller/tests/live_claim_subject_policy.rs::a_selector_claim_can_be_updated_but_not_retargeted`, which fails against the previous rule with `no such key: poolRef` (2026-11-16, kind) |
| A claim's `Bound` message **discloses where its member runs** (`provider/failureDomain`, ADR-0135 Decision 6) to every reader of the claim, including the name of a shared `Provider` in the platform namespace (ADR-0133) | I | Bounded: the same pair is already on the member's `status.scheduled`, in the same namespace (`crates/banlieue-api/src/banlieue/virtualmachine.rs::ScheduledPlacement`), so only a principal who may read claims but not VMs learns something new, and what it learns is a platform-chosen object name and failure-domain name, never an address or credential (`claim_plan.rs::bound_message`). A site whose `Provider` names are sensitive names them as it would any other cluster object |
| A **`Provider`'s cordon label or heartbeat steers which member a claim gets**: someone cordons a host, or makes its heartbeat `Stale`, to push claims onto other hosts, or fails to, to keep them on a degraded one | T | Bounded: health only reorders Ready members and never leaves a claim `Pending` (ADR-0135 Decisions 2 and 3; `claim_plan_tests.rs::an_unhealthy_host_is_still_used_when_it_is_all_there_is`). Writing `banlieue.io/unschedulable` needs `update` on `providers`, which already stops all placement there (ADR-0124); `status.workload.heartbeat` is written only by the operator's field manager (`crates/banlieue-api/src/banlieue/provider.rs`). The predicates are the scheduler's own (`reconciler/scheduler.rs::is_unschedulable`, `is_stale`), so a claim cannot be steered by a signal the scheduler ignores |
| **Forged subject annotations steer a `Spread` claim**: a member annotated with another subject's issuer and id makes that subject look present in a domain, so its next claim lands elsewhere (or, removed, on a shared host) | T | Bounded: `Spread` only reorders Ready members of the claim's own namespace and is soft (ADR-0135 Decision 5; `claim_plan.rs::subject_held`). Writing a member's annotations needs `patch` on `virtualmachines` there, which already permits deleting the member outright. Issuer and id are matched together, so an id shared across issuers is not one subject (`claim_plan_tests.rs::the_same_id_at_another_issuer_is_not_the_subject`). `Spread` is a fate-sharing convenience, not an isolation control: VM isolation between subjects is the bind-once rule above |
| User-influenced strings (`domainName`, `pool`, disk/volume names) injected into libvirt domain XML | T, E | Every value is escaped on the way in by `esc()` — all five XML entities, uniformly in text *and* attributes, so there is no context-dependent rule to get wrong — `crates/banlieue-provider-libvirt/src/xml/escape.rs`, applied throughout `xml/domain.rs`; both have dedicated `_tests.rs` |
| A rogue CA is slipped into `attestation.ekTrustBundle` (A-15) — on the `Provider` spec, or by editing the ConfigMap/Secret it references — so an attacker-forged EK certificate verifies | S, T | **Bounded, not prevented.** The `banlieue-provider-attestation-ektrustbundle` VAP validates *shape* (exactly one of inline/configMapRef/secretRef — `deploy/admission/provider-attestation-ektrustbundle.yaml`); content cannot be validated by banlieue, which has no idea which CAs are legitimate (ADR-0049 Decision 10 makes that an explicit admin assertion, the same posture as `capabilities.features`). `Provider` writes are platform-admin-only (§7.5), and the referenced object lives in the Provider's own namespace, out of tenant reach. Custody of that object is §7.14 |
| `Provider.spec.scheduling.cpuOvercommitRatio` is set to disable the CPU fit or make it meaningless (ADR-0107) | T, D | The CRD schema bounds it to 1 to 64 (`deploy/crds/banlieue.io_providers.yaml`, from `MIN_`/`MAX_CPU_OVERCOMMIT_RATIO` in `crates/banlieue-api/src/banlieue/provider.rs`, tested `the_crd_bounds_the_cpu_overcommit_ratio`), and `Provider` writes are platform-admin-only (§7.5). Memory has no ratio: it is never overcommitted |
| `Provider.spec.scheduling.storageReservePercent` is set to fill stores to the last byte, or to stop all placement (ADR-0128) | T, D | The CRD schema bounds it to 0 to 50 (`deploy/crds/banlieue.io_providers.yaml`, from `MAX_STORAGE_RESERVE_PERCENT` in `crates/banlieue-api/src/banlieue/provider.rs`, tested `the_crd_bounds_the_storage_reserve`), and `Provider` writes are platform-admin-only (§7.5). `0` is a deliberate, documented choice, not a bypass: the free-space check still applies |
| A principal asks banlieue to **move another tenant's VM**, or retargets a move in flight so it acts on a different VM (ADR-0127) | T, E | A move is namespaced and names its VM by `LocalObjectReference`, so it can only name a VM in its own namespace; `spec.vmRef` is immutable and `spec.target` takes exactly one of `providerName` or `providerSelector`, both by CEL in the CRD (`crates/banlieue-api/src/banlieue/virtualmachinemove.rs`, tested in `virtualmachinemove_tests.rs` and against a kind apiserver). No role in `deploy/` grants `virtualmachinemoves`, and no controller acts on a move yet (API milestone), so today a move has no effect at all. Stage 1 adds the admission checks of ADR-0127 Decision 3 and the controller's grant, and re-walks this row |
| A `VirtualMachine` **declares another VM's address** in `spec.networkOverrides`, by mistake or to intercept traffic meant for it and knock it off the network (ARP contention on the shared segment) | S, T, I, D | **ADR-0083**: `banlieue-controller` blocks the later claimant on the same `networkClass` before scheduling: `Ready=False reason=DuplicateAddress`, and **no infra CR** for a VM never provisioned, so the address never reaches a provider. A VM that **holds** the address (`status.heldAddresses`, written by the controller only when it applies the infra CR) keeps it whatever its age, including while its guest reboots and on vSphere, which reports no guest addresses; a running VM edited onto a held address is blocked and its infra CR is not re-applied. `crates/banlieue-controller/src/reconciler/address_conflict.rs` (`find_duplicate_address`), called from `reconciler/virtualmachine.rs` before `schedule`; unit tests in `address_conflict_tests.rs`, reconcile wiring in `tests/live_duplicate_address.rs` against a real API server. **Banlieue VMs and declared addresses only:** a machine banlieue did not create is not seen (§9), and root in a guest can still configure any address on its NIC, which is the existing no-anti-spoofing residue in §8. **Every declared interface is checked** (ADR-0111): `declared_claims` walks all of `networkOverrides`, so a second NIC's address is held to the same `(networkClass, address)` rule as the first |
| The duplicate-address condition message **discloses another namespace's VM**: its name, namespace, or that it exists | I | The message names a holder as `namespace/name` only when it shares the blocked VM's namespace; otherwise it says "a VirtualMachine in another namespace" (`address_conflict.rs` (`Holder::OtherNamespace`), tested by `collision_across_namespaces_is_detected_but_holder_is_not_named` and `tests/live_duplicate_address.rs::holder_in_another_namespace_blocks_without_being_named`). The residual one-bit oracle ("is this address in use on this `networkClass`?") is §8 |
| A principal forges `status.heldAddresses` to reserve addresses it was never given, or to release one a running guest still uses | T, D | **Bounded by RBAC, not prevented.** Only the controller writes VM status; writing it takes `update`/`patch` on `virtualmachines/status`, which no tenant role needs and which is already infrastructure-admin-equivalent (it can rewrite `status.infrastructureRef` and every mirrored field). The controller rewrites the field from its own spec on every successful apply, so a forgery lasts until the next one. `deploy/controller/rbac/clusterrole.yaml` is the only shipped grant |
| A principal writes a VM's `status.infrastructureRef` to **another VM's machine**, so the controller applies this VM's spec onto it (ADR-0127: a VM now finds its machine by that name, `reconciler/infra.rs`, `machine_name`) | T, D | **Bounded by RBAC, not prevented.** Writing VM status takes `update`/`patch` on `virtualmachines/status`, which only the controller holds (the row above). The apply also carries this VM's controller owner reference, and the API server refuses an object with two controller owner references, so a machine another VM already controls is not taken over. |
| A `VirtualMachine`'s `banlieue.io/created-by` annotation is forged, so the vCenter `CreatedBy` custom attribute later misattributes who built a VM (ADR-0089) | S | **Not prevented on UPDATE, and not prevented at all on a cluster without the policy — both accepted by design (ADR-0089 Decision 2).** On CREATE where `banlieue-virtualmachine-created-by` applies, the `MutatingAdmissionPolicy`'s `ApplyConfiguration` merge always sets the annotation to `request.userInfo.username`, overwriting any value the creator supplied in the same request — a self-set value on CREATE does not survive. But the policy matches `operations: ["CREATE"]` only, so a later `UPDATE` by any principal holding `update` on the `VirtualMachine` can edit or remove it uncorrected; and a cluster that never applies the policy (it needs Kubernetes 1.36+, §7) gets no stamping at all, leaving the annotation fully attacker/author-controlled. Unlike `VirtualMachineClaim.spec.subject` (the claim-attribution row above), `created-by` carries no authorization weight — nothing in banlieue reads it to decide who may do what — so a forged value only misleads an operator reading vCenter's fleet-audit attributes, the same exposure already accepted for the vCenter `annotation` field other tooling writes |
| A permission or connectivity fault on **one** infra backend (vSphere/libvirt/cloud-hypervisor/Proxmox) blocks the `VirtualMachine` deletion cascade for **every** backend, holding every pending `VirtualMachine` delete — including ones with no relationship to the broken backend — `Terminating` indefinitely (ADR-0090) | D | **Fixed, not merely bounded.** Found live: a `ClusterRole` missing `proxmoxmachines` returned `403` from `apis.proxmox.get_opt`, and the old sequential `?`-chained lookup in `finalize_vm` aborted before the vSphere branch below it ran, even though that vSphere lookup had already succeeded — the finalizer stayed on and the backend VM in vCenter was never destroyed. `finalize_vm` now resolves each backend kind independently (`infra_state`) into `InfraState::{Absent,Present,Terminating,Unknown}`, and the pure `plan_finalize` decides per kind: a lookup or delete failure on one kind (`Unknown`) never blocks another kind's `Present` delete, and never lets the finalizer come off either — `crates/banlieue-controller/src/reconciler/virtualmachine.rs`, unit-tested in `virtualmachine_tests.rs` |
| A pool's whole warm set sits on one host, because placement took the first eligible failure domain, so losing that host loses every warm member at once (found live on Cloud Hypervisor, 2026-10-09) | D | **ADR-0098**: `schedule()` orders eligible domains by load of the VM's pool, then total load, then a rendezvous hash, so members spread and refills go where the pool is short (`crates/banlieue-controller/src/reconciler/scheduler.rs`, `placement_load`; tests in `scheduler_tests.rs`). Load comes only from `status.scheduled` of VMs in the same namespace: a principal who can create VMs there can push later placements away from a domain, which is within what that principal can already do. Concurrent members that cannot see each other can still collide; the next placement corrects it |
| A tenant makes its own namespace served, or someone with write access to labels brings a namespace under the controller (ADR-0130) | E, S | Serving is decided by a label on the `Namespace` object, and writing a `Namespace` is cluster-scoped: no namespaced `Role` carries it, and the controller itself has only `get`/`list`/`watch` on `namespaces` (`deploy/controller/rbac/clusterrole.yaml`, tested `the_controller_may_only_read_namespaces`), so it cannot widen its own scope. Select on a label only a platform admin may set, never one a tenant can edit on its own namespace: §7.30 |
| **An unserved namespace is acted on**: a pool, claim or VM there is reconciled although nobody opted it in | E | `Context::skip_unserved` is the first check, straight after the namespace is read, in every reconciler that serves a namespaced kind (`reconciler/{virtualmachine,pool,claim,vsphere_cluster,bootstrap_data}.rs`) and returns before any API call; with no selector the watch itself is namespaced (`banlieue-provider-sdk/src/scope.rs`, `Scope::api`), and with nothing resolvable startup fails instead of widening (`resolve_scope`, tested `no_scope_at_all_is_an_error_not_cluster_wide`). The gate is a *reconcile* filter: the selector-mode watch is still cluster-wide, so it does not narrow what the controller's `ClusterRole` can read (§8) |
| **The operator renders a workload for a `Provider` in a namespace nobody opted in** (ADR-0131) | E | `Context::skip_unserved` is the first check in `reconciler/provider.rs::reconcile`, before the finalizer or any workload is written; with no flag the `Provider` watch itself is namespaced (`Scope::api`), and with nothing resolvable startup fails (`watch_scope`, tested `the_default_scope_is_the_operators_own_namespace`; `a_provider_in_an_unserved_namespace_is_skipped_before_any_write`). The operator `ClusterRole` has only `get`/`list`/`watch` on `namespaces` (`deploy/operator/rbac/clusterrole.yaml`, tested `the_operator_may_only_read_namespaces`), so it cannot widen its own scope |
| **A namespace leaving the operator's selection strands a `Provider`'s workload** (ADR-0131) | D | A `Provider` with a deletion timestamp bypasses the gate (`skip_unserved(.., deleting = true)`, tested `a_deleting_provider_is_never_skipped`), so `cleanup` still removes the workload and the finalizer; un-labelling alone deletes nothing |
| **A provider binary serves every namespace because no flag was given** (ADR-0131) | E | The `Api::all` fallback is gone from the libvirt, vSphere and Proxmox providers: `scope::provider_namespace` resolves `--namespace`, else the namespace the provider runs in, and refuses to start when neither exists (tested `a_provider_with_no_namespace_at_all_refuses_to_widen`). Operator-rendered and `banlieue bootstrap` providers already pass `--namespace` (`workload.rs`, `bootstrap.rs`) |
| The selector watch is broken or unsynced, and the controller serves **nothing** or **everything** | D, E | Neither: the gate over an unsynced or empty store serves nothing (`NamespaceGate::serves`, tested `empty_selection_serves_nothing`), controllers do not start until the first sync (`wait_until_ready` in `scope::spawn_namespace_gate`) so readiness stays false, and a selector the API server rejects fails startup (`check_selector`, tested `a_bad_request_from_the_probe_is_an_invalid_selector`). A missing `namespaces` grant at start is therefore a loud failure, not a silent one. **After** start, a watch that begins failing (grant revoked, apiserver trouble) is only logged, and the last served set stays in force: §8 |
| Un-labelling a namespace **strands** deletions, or is used to stop work and leave resources in an unknown state | D, T | Deletion is never skipped: `skip_unserved` takes a `deleting` argument and lets an object with a `deletionTimestamp` through, so its finalizer runs (tested `deletion_is_never_skipped_in_an_unserved_namespace`). Un-labelling stops reconciliation and writes nothing; status is left as last written, which §8 records. It takes the same privilege as labelling, so it is no new power for a tenant |
| **A tenant forges a placement**: it writes an infrastructure machine (`VSphereMachine`, `LibvirtMachine`, `ProxmoxMachine` or `CloudHypervisorMachine`) whose `providerRef.namespace` names the platform's provider, getting a VM on the shared vCenter without scheduling, class or image checks (ADR-0133) | E, T | Two layers, neither relying on the other. RBAC: tenants hold no verb on machine CRs (§7.31). Admission: `banlieue-machine-provider-ref` and `banlieue-machinetemplate-provider-ref` let only `system:serviceaccount:banlieue-system:banlieue-controller` set or change `providerRef.namespace`, on create and update, for machines and for the templates CAPI stamps machines from — `deploy/admission/machine-provider-ref.yaml`, structure tested offline in `crates/banlieue-operator/tests/admission_policies.rs` (including that the exempt identity is the controller's real ServiceAccount) and the verdicts against a real API server by `make provider-ref-policy-live-test` (`crates/banlieue-controller/tests/live_provider_ref_policy.rs`) |
| A tenant machine **names only the shared provider's name**, hoping the provider takes it (ADR-0133) | S, E | The provider serves a machine only when `providerRef` resolves to the `Provider` it serves in its **home** namespace, by name and namespace; a bare reference resolves in the machine's own namespace, which is not home (`banlieue_provider_sdk::reconciler::serves_machine`, tested `a_bare_provider_ref_in_a_tenant_namespace_is_not_the_shared_providers`; `Context::serves_machine`, `crates/banlieue-provider-vsphere/src/context.rs`) |
| A tenant sets `Provider.spec.namespaceSelector` to widen which namespaces a provider serves, or to make a workload in its own namespace watch other tenants' machines (ADR-0133) | E | `banlieue-provider-namespace-selector-authorization` VAP uses the CEL `authorizer`: setting or changing the field needs `update` on the virtual subresource `providers/namespaceselector`, granted only by the `banlieue-provider-namespaceselector-admin` ClusterRole, which is not aggregated into admin/edit — `deploy/admission/provider-namespace-selector-authorization.yaml`, tested `the_selector_admin_role_grants_exactly_one_virtual_permission`; verdicts by `only_platform_admins_set_a_provider_namespace_selector`. An unchanged selector needs no grant |
| **A tenant who can read Secrets in its own namespace reads the hypervisor credentials** because a provider was placed there (ADR-0133, the reason for it) | I | No provider is placed in a tenant namespace: the shared provider reads its `Provider`, credentials Secret and CA bundle from its home namespace only (`Context::home_namespace`, used by `reconciler/vspheremachine.rs` for the `Provider` get, `read_credentials` and `resolve_ca_bundle`, in both `reconcile` and `finalize`), and its `ClusterRole` has no Secret or ConfigMap rule (`deploy/provider-vsphere/rbac/clusterrole.yaml`, tested `serving_tenant_namespaces_grants_no_provider_secret_access`). The controller reads the tenant's own `userData` Secret in the tenant's namespace through its existing namespaced `Role` (ADR-0025) and renders it inline, so the provider reads no tenant Secret |
| **An empty `namespaceSelector` serves every namespace** (Kubernetes' reading of `{}`) (ADR-0133) | E | It serves nothing: `LabelSelector::is_empty` makes the controller exclude it as a fallback (`provider_lookup.rs::serves_namespace`, tested `an_empty_selector_serves_no_namespace`) and `selector_to_string` renders no flag for it (`scope.rs`, tested `an_empty_selector_renders_nothing`; `reconciler/provider.rs::watch_namespace_selector`), so the provider serves home only. banlieue never defaults to every namespace |
| **A shared provider acts on a namespace nobody opted in** (ADR-0133) | E | `ProviderScope::skip_unserved` is the first check in `vspheremachine::reconcile`, before any API call, with the home namespace always served and everything else only if the label-filtered `Namespace` store holds it (`scope.rs`, tested `without_a_selector_only_home_is_served_whatever_the_gate_says`, `an_unserved_namespace_is_skipped_but_a_deleting_object_never_is`); a machine being deleted bypasses the gate so its finalizer runs. With no selector the watch itself is namespaced to home (`ProviderScope::api`). Startup fails on an invalid selector (`check_selector`). The gate is a *reconcile* filter: the provider's `ClusterRole` is the ceiling (§8) |
| A namespace with its own `Provider` is **silently placed on the shared one** by a selector typo | E | It is not: if the VM's namespace holds any `Provider`, those are the whole candidate set and a miss fails `NoProviderMatched` (`provider_lookup.rs::select_candidates`, tested `a_namespace_with_its_own_provider_never_falls_back`). Only a `Provider` that declares the VM's namespace is ever a fallback, and `--provider-namespace` unset means no fallback at all (`should_try_provider_namespace`) |
| A tenant exhausts a shared provider's capacity, or the capacity fit under-counts because it only sees the VM's own namespace (ADR-0107, ADR-0133) | D | The fit counts the VMs placed on a shared provider across every served namespace (`provider_lookup.rs::placement_peers`, `retain_served`), and can only over-count where another namespace has a same-named `Provider`, the safe direction. One tenant filling a shared provider is not prevented: no per-tenant quota exists (§8) |
| A tenant writes `status.attestationSubject` on its `VirtualMachine` or claim, so a broker trusts another guest's identity for its sandbox (ADR-0143 Decision 7, ADR-0146) | S, T | Only `banlieue-controller` holds `virtualmachines/status` and `virtualmachineclaims/status` (`deploy/controller/rbac/clusterrole.yaml`); banlieue ships no tenant role that writes a status. The controller **replaces** the VM's subject from the machine on every mirror and the hold writes it as `null` when absent, so a withdrawn subject does not linger (`reconciler/status_mirror.rs`, `claim.rs::mirrored_status_fields`, with tests that fail if any of the three mirror points is removed). The machine's own subject is TB-8's row |
| A tenant creates objects to make the controller fail liveness and restart over and over (ADR-0145) | D | **Bounded.** The ledger is written at the start of every invocation, before the reconcile's own logic and whether or not it succeeds (`crates/banlieue-provider-sdk/src/runner.rs`, `note_invoked`), and the controller runs with kube-runtime's default, unbounded concurrency (`app.rs`, `Config::default()`), so an object that is created and delivered is invoked and satisfies the audit; a namespace the gate does not serve is excluded (`audit.rs::build_observed`). What remains is volume: objects created faster than the controller can be invoked on them for 120 s plus 300 s would trip it, and object count is bounded only by quota, which banlieue does not set (§8) |

### TB-2 — Pods → Secrets

The design principle is: **no banlieue identity holds cluster-wide Secret
access**, and each identity reads only the objects it can name.

| Control | Where |
| --- | --- |
| Provider `ClusterRole`s grant **zero** Secret/ConfigMap access | `deploy/provider-{vsphere,libvirt}/rbac/clusterrole.yaml` |
| Per-`Provider` `Role` is `resourceNames`-scoped to exactly that Provider's credentials Secret (+ CA bundle if named) | `crates/banlieue-operator/src/workload.rs` (`build_import_role`, `named_rule`) |
| An empty `resourceNames` list is treated as a bug, not a default — the CA-ConfigMap rule is emitted only when the Provider names one | same |
| `banlieue-controller`'s `ClusterRole` carries no Secret rule at all; its Secret/ConfigMap `get` is a namespaced `Role`, which `banlieue bootstrap operator` emits from the same file (it did not before 2026-10-09, so no `VirtualMachine` with user-data scheduled on a bootstrap install). The Role is un-scoped by name, so it is safe only with ADR-0042's VAP applied (`deploy/admission/`, a separate step) | `deploy/controller/rbac/clusterrole.yaml`, `deploy/controller/rbac/role.yaml`, `crates/banlieue-operator/src/bootstrap.rs` (`build_controller_userdata_role`), tested in `bootstrap_tests.rs` |
| `banlieue-imagebuilder`'s cloud-config Secret access is a namespaced `Role` in the **build** namespace, not a `ClusterRole` rule (ADR-0041) | `deploy/imagebuilder/rbac/role.yaml` |
| `banlieue-imagebuilder`'s `ClusterRole` reads `persistentvolumes` (`get`) and `nodes` (`list`) and writes neither; a unit test pins the exact verbs (ADR-0115) | `deploy/imagebuilder/rbac/clusterrole.yaml`, `crates/banlieue-operator/src/bootstrap_tests.rs` |
| No `ClusterRole` grants Secret access **except `banlieue-operator`'s deliberate `get`** — held only so RBAC's escalation-prevention permits it to delegate that verb into a per-Provider `resourceNames`-scoped `Role`, and never exercised by the operator itself (SEC-007, accepted-with-monitoring) | `deploy/operator/rbac/clusterrole.yaml` |
| That invariant is mechanically **enforced**, not merely checkable: a unit test fails the build if any other `ClusterRole` grants `secrets`/`configmaps` | `crates/banlieue-operator/src/bootstrap_tests.rs` (`only_the_operator_cluster_role_may_grant_secret_access`) |
| The CLI install path emits the same namespaced RBAC as the manifests, so `banlieue bootstrap` and GitOps cannot drift apart | `crates/banlieue-operator/src/bootstrap.rs` (`build_cloud_config_role`) |
| `Credentials` has a hand-written redacting `Debug` | `crates/banlieue-provider-vsphere/src/client/mod.rs` |
| `TlsIdentity` likewise — the libvirt credential *is* the client private key, so `client_key_pem` renders as `<redacted>` and the public CA/cert halves render as byte counts. A regression test asserts the key never reaches `{:?}` | `crates/banlieue-libvirt/src/transport.rs`, `transport_tests.rs` (`tls_identity_debug_redacts_the_private_key`) |
| The Proxmox provider `ClusterRole` grants **no Secret or ConfigMap access, no `create` and no `delete`** on `proxmoxmachines`; the API token and CA bundle are read by name through the operator's per-`Provider` `resourceNames` Role — `deploy/provider-proxmox/rbac/clusterrole.yaml`, `crates/banlieue-operator/src/bootstrap.rs` |
| `ApiToken` and `ClientConfig` have hand-written redacting `Debug`; the token is validated (`user@realm!tokenid`, no whitespace in the secret) before any request, so a mis-pasted value is reported as such and never echoed. Regression tests assert the secret never reaches `{:?}` | `crates/banlieue-proxmox/src/token.rs`, `client.rs` (`token_tests.rs`, `client_tests.rs`) |
| Rotating a credential needs **no Secret `list`/`watch`**: each provider reads its one Secret by name on every reconcile and builds a fresh client from it, so a rotation lands within the ~30 s requeue (ADR-0095). Granting a watch would have meant read access to every Secret in the namespace. Regression tests pin each production client factory as stateless | `crates/banlieue-provider-{vsphere,proxmox,libvirt}/src/client*` (`production_factory_holds_no_client_between_reconciles`) |
| The libvirt provider `ClusterRole` grants **no `create` and no `delete`** on `libvirtmachines` — the controller owns their lifecycle; a compromised provider cannot mint machines the scheduler never placed | `deploy/provider-libvirt/rbac/clusterrole.yaml` |
| The Azure provider reads **no Secret and no ConfigMap at all**: its `ClusterRole` has neither, nor `create` or `delete` on `azurevirtualmachines`, and the operator's per-`Provider` `Role` emits no Secret rule because an azure `Provider` names no `credentialsRef` (admission refuses one, TB-1). Its cloud credential is a projected token the kubelet writes into its own pod (A-29), so neither a Secret nor a watch exists to widen | `deploy/provider-azure/rbac/clusterrole.yaml`; `crates/banlieue-operator/src/workload.rs`; tests `azure_role_grants_no_secret_access`, `azure_pod_projects_an_entra_audience_token_for_one_hour`, `other_backends_get_no_token_volume_mount_or_env` (`workload_tests.rs`), `the_azure_provider_cluster_role_grants_no_secret_or_configmap` (`bootstrap_tests.rs`) |
| `banlieue-controller` reads a Cluster API `Machine`'s bootstrap Secret **by name** through a namespaced `Role`: its own `banlieue-controller-userdata` in `banlieue-system`, and in any other namespace only a `Role` the administrator creates. Its `ClusterRole` gains `get`/`list`/`watch` on `machines.cluster.x-k8s.io` and nothing else in that group, the same verbs on the one CRD `machines.cluster.x-k8s.io` (`resourceNames`, so it reads no other CRD), and no Secret rule; unit tests pin all three (ADR-0129 Decisions 2 and 9) | `deploy/controller/rbac/clusterrole.yaml`, `role.yaml`, `crates/banlieue-operator/src/bootstrap_tests.rs` (`controller_cluster_role_only_reads_capi_machines`, `controller_cluster_role_reads_only_the_capi_machine_crd`, `only_the_operator_cluster_role_may_grant_secret_access`); `scripts/kind-capi-controller.sh` checks the CRD grant on the real API server before the e2e |
| A bootstrap Secret created or fixed after the controller looked is found **without Secret `list`/`watch`**: a machine waiting on one is requeued on a fixed interval, 30 s when it is missing and 5 min when a person must act (ADR-0129 Decision 3) | `crates/banlieue-controller/src/reconciler/bootstrap_data.rs` (`SECRET_NOT_FOUND_REQUEUE`, `NEEDS_A_PERSON_REQUEUE`, `outcome`) |

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
read-only root filesystem and all capabilities dropped. So does the digest
Job that measures every finished build (ADR-0010, amendment 2026-11-26),
under the same Role: no new verb.

**ADR-0162: the Azure provider creates Jobs here too.** The image publish and
delete Jobs run in this namespace, beside the artifacts PVC they read, as
`banlieue-azure-publisher`, which holds no RBAC and mounts no API token
(`deploy/imagebuilder/rbac/serviceaccount-azure-publisher.yaml`); the pod is
non-root with a read-only root filesystem, no capabilities, `RuntimeDefault`
seccomp, and the PVC read-only (`crates/banlieue-provider-azure/src/reconciler/publish_job.rs::build_job`).
The provider's code creates them only in its configured build namespace and
only as that ServiceAccount (`reconciler/image.rs`), and its grant now matches:
see the next paragraph. Anyone who can create a pod here can also run as
`banlieue-azure-publisher` and so act as the publisher identity (A-30, §4).

**Provider Job grants are a `Role` here, and admission bounds them.** The
vSphere (ADR-0020), libvirt (ADR-0011) and Azure (ADR-0162) providers create
Jobs. Their Job verbs are no longer in their cluster-wide `ClusterRole`s
(`deploy/provider-{vsphere,libvirt,azure}/rbac/clusterrole.yaml`), where Job
create was pod create in every namespace as any ServiceAccount there. The
operator renders, per `Provider` of those backends, a `Role` and `RoleBinding`
in this namespace holding exactly the verbs that provider's Job code uses
(libvirt `jobs` get/create/patch; vSphere `jobs`
get/list/watch/create/patch/delete; Azure the same plus `pods` `list`), bound
to the provider's ServiceAccount in its workload namespace
(`crates/banlieue-operator/src/workload.rs::{job_rules, build_jobs_role}`,
applied by `crates/banlieue-operator/src/reconciler/provider.rs`). They carry no
owner reference, which cannot cross namespaces: the `Provider`'s labels select
them for the orphan prune and the finalizer's cleanup, as for the
ClusterRoleBinding. A standalone install gets the same `Role`
(`bootstrap.rs::build_provider_jobs_role`; the static vSphere manifest is
`deploy/provider-vsphere/rbac/role-jobs.yaml`). The per-instance `Role` in the
`Provider`'s namespace carries no `jobs` for any backend
(`bootstrap.rs::backend_additional_rules`): libvirt's used to, which was Job
create beside the operator's ServiceAccount. Because this namespace enforces
PSA `privileged`, `deploy/admission/provider-jobs.yaml` is the floor under
those Jobs: a Job written by any `banlieue-provider-*` ServiceAccount is
admitted only here, only as `banlieue-import` or `banlieue-azure-publisher`,
and only with a restricted pod (non-root, no host namespaces, no privileged
container or privilege escalation, no added capability, no `Unconfined`
seccomp, volumes limited to a PVC, an `emptyDir` or a projected token, no
Secret or ConfigMap by volume or environment). The vSphere import Job gained
the restricted security context it lacked
(`crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs::build_import_job`).
STRIDE: **E** (a compromised provider, or its stolen token, no longer reaches
other namespaces or other ServiceAccounts through Jobs, and no longer reaches
node root here); **T/I** (what it keeps, a restricted Job as the import or
publisher identity, is §8's). Tests: `workload_tests.rs`
(`a_job_running_backend_gets_its_job_verbs_in_the_build_namespace`,
`backends_without_jobs_and_external_providers_get_no_job_role`),
`bootstrap_tests.rs` (`no_provider_cluster_role_grants_jobs_or_pods`,
`the_operator_holds_every_job_rule_it_grants`),
`crates/banlieue-operator/tests/admission_policies.rs`
(`the_provider_jobs_policy_*`).

**ADR-0115: a lost volume is rebuilt, not trusted.** On node-local storage an
artifacts PVC can stay `Bound` and its `OSArtifact` `Ready` after the worker's
disk was replaced, so the build namespace would keep reporting an image whose
file is gone (an availability failure, and a status that lies). The imagebuilder
now reads the bound PV and the node it is pinned to, and deletes and rebuilds an
`OSArtifact` whose node was replaced after the PV was made
(`crates/banlieue-imagebuilder/src/reconciler/vmimage.rs`: `pinned_hostname`,
`volume_lost`, `artifact_gate`). Its identity gains `get` on `persistentvolumes`
and `list` on `nodes` (`deploy/imagebuilder/rbac/clusterrole.yaml`, pinned by
`the_imagebuilder_cluster_role_reads_volumes_and_nodes_and_writes_neither`):
read-only, cluster-scoped, no Secret content, no new write. The delete it
performs is the one SEC-005 already grants. A failed read never deletes
(a missing grant degrades to the old behaviour). STRIDE: **D/T** (a node that
re-registers makes a needless rebuild, bounded, §8); **I** (node and PV names
and timestamps are visible to the identity, the same facts `kubectl get nodes`
shows any cluster reader).

### TB-4 — Cluster → hypervisor

| Threat | Control |
| --- | --- |
| MITM / attacker-presented certificate | banlieue owns the `reqwest` client and honours `connection.caBundle` (ADR-0008); CA source validated by `banlieue-provider-cabundle-source` VAP |
| Recorded hypervisor or cloud API traffic (carrying A-1, A-29) is decrypted later by a quantum-capable adversary (I) | **Partly controlled (ADR-0170).** Every client (reqwest for vCenter, Proxmox and Azure; tokio-rustls for libvirtd's mutual TLS) offers X25519MLKEM768 first through `crates/banlieue-tls`, so an endpoint that accepts it is protected without a banlieue change. An endpoint that does not answers with a classical group: the residual risk is in §8 |
| Hostile or unresponsive endpoint stalls every reconcile | 10 s connect / 120 s request timeouts on the vSphere client; timeouts on libvirt connect, recv, and `Session::send` |
| Malformed libvirt RPC frames | Wire decoder is continuously fuzzed (`crates/banlieue-libvirt/fuzz`, `.github/workflows/fuzz.yaml`, ClusterFuzzLite); ADR-0050's domain `decode_*` halves are pure and unit-tested against captured wire bytes, so they are in that fuzz surface too |
| A compromised provider pod uses its network reach to scan or attack hosts other than its backend | **Opt-in** `deploy/network-policies/` (ADR-0094): default deny, then egress only to DNS, the API server and the provider's own backend port (443 vCenter, 8006 Proxmox, 16514 libvirtd), selected by `app.kubernetes.io/component` so operator-spawned providers in any namespace match; the Azure provider gets 443 for ARM and Entra ID, which cannot be pinned to addresses because both are anycast front ends Microsoft publishes only as service tags (`deploy/network-policies/provider-azure.yaml`), and the Azure publish Job gets DNS and 443 only, no API server and no ingress, for the same reason (`deploy/network-policies/azure-publish.yaml`, applied in the build namespace). Destinations stay "any address" until the site narrows them (§7.21, §8); `crates/banlieue-operator/tests/network_policies.rs` pins the port set against the labels the operator stamps |
| An operator quarantines or holds a VM by powering it off in vCenter, and banlieue starts it again | The provider drives every provisioned vSphere VM to `spec.desiredPowerState` on each pass (ADR-0034 amendment 2026-10-23: `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`, `converge_power_state`), as libvirt, Proxmox and Cloud Hypervisor already do. The supported hold is `desiredPowerState: PoweredOff` on the `VirtualMachine` (the provider then holds it off), or deleting it; both are Kubernetes writes, gated by RBAC on `VirtualMachine`. A vCenter-side power-off is not a hold. Recorded in §8 |
| Backend call tracing (ADR-0092) leaks hypervisor credentials or guest bootstrap material into a span | Every backend span is `skip_all` and records only identifiers (node, vmid, moref, pool, volume, domain): `crates/banlieue-proxmox/src/client.rs`, `crates/banlieue-libvirt/src/procs.rs`, `crates/banlieue-cloud-hypervisor/src/client.rs`, `crates/banlieue-provider-vsphere/src/client/vim.rs`; `banlieue-azure` emits no spans, and the Azure provider's log lines name resource IDs and owner UIDs only. Export is off unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set |
| A deleted VM leaves its sealed-key material behind (swtpm state, UEFI NVRAM varstore) | `domain_undefine` takes **no flags parameter** and unconditionally sends `MANAGED_SAVE\|NVRAM\|TPM` (ADR-0050 Decision 5) — the flag cannot be forgotten at a call site — `crates/banlieue-libvirt/src/procs.rs`, proven against a real libvirtd in `tests/live_libvirtd.rs`. Live since ADR-0050: `LibvirtMachine`'s finalizer calls it on every teardown — `crates/banlieue-provider-libvirt/src/machine_client.rs` (`undefine`), invoked from `reconciler/libvirtmachine.rs::finalize_backend`, which then verifies the domain is actually gone before deleting its volumes |
| Something other than the intended guest answers the broker's mTLS connection and receives the subject's token | S | The consumer attests the guest with **SPIRE's TPM node attestation** (ADR-0049, amended 2026-11-05; mediatore ADR-0018): the guest's agent must answer a credential-activation challenge sealed to its vTPM's EK, and the EK certificate must chain to a hypervisor EK CA, before any SVID is issued, so an answer captured from one sandbox proves nothing about another. banlieue's controls are the anchors it publishes: the bound member's EK certificate on the claim (`status.tpmEndorsementCertificates`, mirrored in `crates/banlieue-controller/src/reconciler/claim.rs`) and the admin-supplied issuer bundle (`Provider.spec.attestation.ekTrustBundle`, A-15). A `tpmEnabled` member is unbindable until it publishes its certificate, on every backend: the gate lives in each reconciler's `GuestReady`, withheld with reason `TpmEndorsementPending`. The vTPM is unique per VM by construction: on vSphere each clone gets a **new** vTPM before its first power-on and the template carries none (ADR-0040, ADR-0088), on libvirt swtpm state is keyed by domain UUID, on Cloud Hypervisor each machine's TPM is manufactured for it (ADR-0065). banlieue mints no nonce: `status.nonce` is retired (never set since v0.4.14). The verification itself is the consumer's, by design (ADR-0049 Decision 2) |
| A compromised or misbehaving backend (libvirtd, vCenter, a Proxmox node) reports **false capacity** to steer placement: inflated, so the scheduler over-places VMs there, or zero, so it places none (ADR-0128) | T, D | **Bounded to that Provider.** The figures land only on that Provider's failure domains, and the backend could already refuse or fail every VM it is given. Inflated figures end at the backend: QEMU or vCenter admission refuses an allocation it cannot make, and the VM reports the failure; deflated ones send VMs elsewhere or leave them `NoFailureDomainMatched` with the shortfall named. The reads are read-only, and libvirt's go through the fuzzed wire decoder with a checked stats count (`decode_node_get_memory_stats_ret`, `NODE_MEMORY_STATS_MAX`); the scheduler's arithmetic saturates rather than wraps (`scheduler.rs::measured_shortfall`), and a VM already placed is never moved by a measured figure, so a lying backend cannot evict running guests |
| A token minted for a different service is presented to the agent and accepted | S | `aud` is the agent's **own configured audience** and is deliberately never read from the claim (ADR-0049 Decision 5) — otherwise whoever wrote the claim chooses the audience |
| A claim names an attacker-controlled issuer, so the agent fetches that attacker's JWKS and every forged token verifies | S, T | The issuer allowlist in `banlieue-virtualmachineclaim-subject-authorization` — added for audit honesty, and load-bearing here: `subject.issuer` is a CR field the agent is asked to trust as a key source (ADR-0049 Decision 7) |
| A guest asserts `GuestReady` while still installing, or a compromised guest asserts it to be handed out sooner | S, T | **Bounded, not prevented.** The marker is guarded on immucore's active/passive sentinels so the *live installer* cannot assert it (`examples/17-cloud-config-guest-phase.yaml`), but a guest that has already been compromised can write anything, on either transport: `vmware-rpctool info-set` is as available to a compromised vSphere guest as writing the marker file is on libvirt. This is why ADR-0043 Decision 9 states the signal is liveness, never integrity: it is not a control against a hostile guest, and the pool hands out fresh, unclaimed VMs. Integrity is ADR-0049's problem (§8) |
| A guest returns a huge or malformed payload to `guest-file-read`, hoping to fault the provider's reconcile loop | D | The read is capped before decoding (`MARKER_READ_MAX`, checked on the base64 *and* the decoded bytes), every parse failure returns "not installed" rather than an error, and the handle is closed on every path so an agent's handle table cannot be exhausted — `crates/banlieue-provider-libvirt/src/guest.rs`, with dedicated tests for oversize, undecodable and garbage input |
| A guest reports **another member's** EK certificate, so a verifier later checks an attestation against the wrong machine | S, T | Two independent checks, and the weaker one runs first. The subject CN must equal `<domain-name>:<domain-uuid>` (both values banlieue itself assigned when it defined the domain) or the certificate is discarded, never published, and the machine reports `GuestReady=False`/`TpmEndorsementMismatch` (`crates/banlieue-provider-libvirt/src/guest.rs`, `ek_cn_matches`). The check that cannot be forged is ADR-0049's: an EK certificate is a **public key**, so a guest presenting one it does not hold the private half of cannot certify an AK under it, and the substitution fails the step it was made to pass |
| A guest reports partitions **sealed** that are not, to be handed out (or kept) as encrypted | S, T | **Bounded, not prevented**, exactly as for `GuestReady` (ADR-0043 Decision 9), on every backend's channel (Cloud Hypervisor's `sealed=` vsock line included, ADR-0112): the report can only *withhold* `GuestReady`, never grant beyond it, and a guest that is already root can write anything. On Cloud Hypervisor a guest can only report its own state: the socket is in its own run directory. For a `Url` build banlieue wrote both the policy and the report stage (`crates/banlieue-imagebuilder/src/first_boot_sealing.rs`), so an uncompromised clone reports what `blkid` sees; an `Asserted` image is the operator's claim (§8) |
| A guest returns a huge or malformed sealing report, hoping to fault the reconcile or smuggle text into status | D, T | Read through `SEALED_READ_MAX` on libvirt (base64 and decoded bytes, handle always closed), and on Cloud Hypervisor within the vsock report's `MAX_REPORT_BYTES` and read timeout (`crates/banlieue-provider-cloud-hypervisor/src/report.rs`, `parse_report`); on every backend only plain labels (`[A-Z0-9_]+`) survive parsing, each once, and the result is used only to compare against the declared list: `crates/banlieue-provider-libvirt/src/guest.rs` (`sealed_from_read`), `crates/banlieue-provider-sdk/src/sealing.rs` (`parse_sealed_report`), `crates/banlieue-provider-vsphere/src/guest.rs` (`probe_sealed`) |
| A `VMImage` author smuggles shell into the sealing report stage through a partition name, which runs as root on every boot of every clone | E, T | Three independent refusals: the CRD schema (`minItems: 1`), the `banlieue-vmimage-first-boot-sealing` VAP (`^[A-Z0-9_]+$`, no system partitions, not with `trustedBoot`: `deploy/admission/vmimage-first-boot-sealing.yaml`), and the builder, which refuses any non-label name whatever admission did (`first_boot_sealing.rs`, `is_plain_label`, tested with `$(id)` and `;`) |
| A guest publishes bytes that are not a certificate at all, into a status field consumers feed to a certificate library | T | Parsed as X.509 before publication, with the PEM label required to be `CERTIFICATE` — a `PRIVATE KEY` block is valid PEM and is refused (`parse_ek_pem_str`, `x509-parser`). Size-capped at `EK_READ_MAX` on the base64 *and* the decoded bytes, like the phase marker. Unit-tested against a real `swtpm_localca` certificate and against junk |
| banlieue's read of the certificate becomes a way to run code inside a sandbox | E | The certificate lives in the vTPM's NVRAM, and the obvious way to fetch it is `tpm2_nvread` over `guest-exec` — **host-to-guest arbitrary code execution**. banlieue does not use `guest-exec` anywhere: the guest exports the certificate to `/run/banlieue/ek.pem` and banlieue reads that file with `guest-file-open` at an explicit `mode: "r"` (ADR-0045 Decision 2). A provider that only ever opens guest files read-only cannot be turned into a remote shell by a compromised controller |
| A half-failed teardown silently leaves domains defined | libvirt 11.3 *fails* undefine on a UEFI domain without `NVRAM` rather than warning; the error is returned, never swallowed, and the live lifecycle test asserts the domain is actually gone — `crates/banlieue-libvirt/tests/live_libvirtd.rs` |
| Credentials leak into logs | No secret is ever logged; redacting `Debug`; provider condition messages are the only verbatim text mirrored to user-facing status |
| A provider workload acts on **another `Provider`'s** state: reads its credentials Secret, overwrites or erases its `VMImage.status.perProvider` row, or (vSphere) blocks or performs the deletion of its zone templates | **ADR-0087, Amendment 2026-11-01.** A workload started with `--provider-name` reconciles only that `Provider`'s rows (`providers_in_scope`, `crates/banlieue-provider-sdk/src/reconciler.rs`) and writes them under its own field manager (`status_field_manager`, `ssa.rs`), on libvirt (`reconciler/vmimage.rs`), Proxmox (`reconciler/image.rs`) and vSphere (`reconciler/vmimage.rs`); its operator-rendered Role grants `get` on its own credentials Secret only, so reaching another's is refused by the API server, not just avoided. vSphere's delete destroys only the workload's own templates, clears its own row's zones, and removes the shared finalizer once no existing `Provider` lists templates (`rows_to_clean`, `with_zones_cleared`, `finalizer_can_go`, tested in `vmimage_finalize_tests.rs`). Found live 2026-11-01 (libvirt, v0.4.9, rolled back); vSphere and Proxmox were latent |

#### vSphere VM ownership (ADR-0024, ADR-0026, ADR-0101)

`status.vmRef` is a cache written at the end of a successful pass; the VM's
own marker is what decides ownership when the cache is empty or wrong. A pass
reads the machine live from the API server and decides from that, never from
the controller's watch cache (ADR-0101, amended 2026-11-04).

| Threat | STRIDE | Control |
| --- | --- | --- |
| A machine adopts or destroys a VM banlieue did not create (an administrator's VM of the same name, another namespace's machine in the same folder) | T, E, D | Ownership is the machine's **UID** in `extraConfig` `banlieue.machine.uid`, written by `CloneVM_Task` in the same `CloneSpec` as the guestinfo keys, so no banlieue VM exists unmarked (A-16). The name lookup runs only when `status.vmRef` is unset and only over the direct children of the clone's own target folder; a VM without a matching UID is never destroyed, and a create that meets one reports `VMNameConflict` instead of cloning. A recorded `vmRef` is still destroyed by moref, as ADR-0026 decided. `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs` (`reclaim_or_refuse`, `finalize_unrecorded`), `client/vim.rs` (`find_vms_named`); tested against a fake that refuses `DuplicateName` per folder, as vCenter does |
| A pass that loses its moref (a fault after the clone, a provider crash before the status write, a machine deleted meanwhile) strands a VM that no object accounts for, still running with the machine's user-data (A-2), or wedges the machine on `DuplicateName` | D, I | A fault in any step after the clone destroys it (`finish_clone`, `roll_back_clone`); a crash leaves a marked VM that the next pass destroys and clones afresh; deletion with no `vmRef` destroys it (`finalize_unrecorded`). If the Provider is already gone and nothing is recorded, the finalizer is released with a warning rather than blocking deletion forever. Same files |
| A pass destroys the VM it has just built, or records a VM that no longer exists, because it decided from a watch-cache snapshot older than its own status write (seen 2026-11-04: every OpenBao VM rolled onto a new image, 13 ms between passes) | T, D | A pass reads the `VSphereMachine` live (`live_base`, `ApiMachineSource`: an uncached `GET`) after the cheap `serves` check and uses that object for the deletion branch, the route (`route`) and the base of every status write, so Decision 3's destroy only runs when the API server itself holds no `vmRef`, and no write regresses a newer one. A live object that is gone, or has another UID, ends the pass. `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`; regression tests `a_second_pass_from_a_stale_cache_does_not_destroy_the_vm_the_first_pass_built` and `a_pass_builds_on_the_live_status_not_the_stale_cached_one` in `vspheremachine_live_tests.rs`. Residual: the live read and the write that follows are not atomic, but the provider runs one pass per object and owns `status`, so nothing else writes it between them. The other providers' create paths have not been checked for the same pattern (ADR-0101 Consequences) |
| A recorded `vmRef` names a VM that is gone, and recovering it by marker adopts a VM that is not the machine's | T, E | Adoption needs the exact `banlieue.machine.uid` match (Decisions 1 and 2), in the machine's own folder, under its own name, and **exactly one** such VM (`find_own_clone`): none reports `BackendMissing`, several report it naming the morefs and adopt none, and a failed lookup is an error that adopts nothing. The adopted moref is written into `status` with every other field kept (`status_adopting`). A user with `VirtualMachine.Config.AdvancedConfig` can plant a UID on a same-named VM, which is the §8 marker risk, now also meaning that VM can be adopted. Same files, same tests |
| A compromised guest forges, copies or erases the marker, to pass its VM off as another machine's or to orphan itself | S, T | The key is outside `guestinfo.*`, the only `extraConfig` namespace a guest reaches through `vmware-rpctool`; the guest's own writes (`guestinfo.banlieue.phase`, A-8) never touch it, and a machine's UID is assigned by the API server, not by whoever writes the `VirtualMachine`. **Not yet verified against a real vCenter** (§8) |
| Two namespaces' machines of the same name, placed in one vCenter folder, collide | D | The second reports `VMNameConflict`; it never adopts or destroys the first one's VM. Making VM names unique across namespaces is undecided (ADR-0101 Consequences), so the denial is bounded, not prevented |


#### vSphere VM network adapters (ADR-0111)

A cloned VM gets one NIC per interface its `VMClass` declares. The guest's
addressing for each reaches it as `guestinfo.network.<i>.*`.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A VM author puts a VM on a segment they should not reach, by declaring another interface | E | A tenant cannot add a NIC. The interface list is the **`VMClass`'s** (cluster-scoped, platform-admin, §7.5), and `VirtualMachine.spec.networkOverrides` carries only a static address keyed by an interface the class already declares: `NetworkInterfaceOverride` has `name` and `static`, no `networkClass` (`crates/banlieue-api/src/banlieue/virtualmachine.rs`). The port groups that exist at all are the ones the Provider advertises in `networkClasses` (§7.17) |
| A second interface's address duplicates another VM's, to intercept its traffic or knock it off the segment | T, D | The ADR-0083 check covers every interface, not the first: `declared_claims` iterates all of `networkOverrides` and keys on `(networkClass, address)` (`crates/banlieue-controller/src/reconciler/address_conflict.rs`). It only means something for a later NIC now that one exists |
| A clone is left half built: the first NIC moved, a later one missing or on the wrong network | T, D | Every declared interface's port group is resolved from the target cluster **before** `CloneVM_Task`, and one that is not reachable stops the clone and names the interface (`ensure_vm`, `reconciler/vspheremachine.rs`; `an_unreachable_later_interface_clones_nothing_and_names_the_port_group`). All NIC changes are in the one `CloneVM_Task` (`build_clone_nic_changes`), so the clone exists with all of them or not at all |
| A guest rewrites its own `guestinfo.network.<i>.*` to confuse its own configuration | T | Nothing reads them back: banlieue writes them once, in the clone's `extraConfig`, and the guest reads them at boot. The keys hold addresses, prefixes and names, never credentials (A-2 is `guestinfo.userdata`, unchanged), and the ownership marker is outside `guestinfo.*` (above) |
| A VM on two segments **forwards between them**, defeating an isolated segment | E | **Not prevented by banlieue**, which cannot see inside a guest. Recorded as an accepted risk in §8 and asked of the operator in §7.28 |

#### vSphere adopted templates (ADR-0020, ADR-0028, ADR-0104)

A `Template`-kind source adopts a template that already exists in vCenter.
banlieue looks it up in each failure domain's own folder and clones from it;
it never builds, writes or destroys it. A `Url` source is the opposite: the
per-zone import Job creates the template, so `VMImage` deletion may destroy it.

| Threat | STRIDE | Control |
| --- | --- | --- |
| Deleting a `VMImage` destroys an **administrator's own** template: per-zone status on a `Template` row would otherwise feed `finalize`, which destroys every template listed in `zones[]` | T, D | Ownership is decided on the **source kind**, not on status: `image_owns_templates` is true only for a vsphere `Url` source, `finalize` skips the vCenter connection and every destroy for anything else, and `destroy_owned_zone_templates` re-checks it before `destroy_zone_templates`, so a `Template` row cannot become destroyable by what `zones[]` contains. The other mutating paths (`--force-create`'s `destroy_if_present`, `--force-upload`'s datastore delete, `import_iso_template`) run only inside the import Job, which `reconcile_for_provider` never creates for a `Template` source; `spec.template.retainOnDelete` and the `forceCreate`/`forceUpload` knobs are ignored for it. `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`; regression tests `vmimage_finalize_tests.rs` (`template_kind_image_never_destroys_*`) against a fake that holds the template in the zone's folder |
| A same-named template planted elsewhere in the datacenter is cloned instead of the intended one (the old lookup took the first match anywhere in the datacenter) | S, T | The lookup is confined to `<rootFolder>/<failure-domain>` of that zone's own datacenter (`template_zone_status`; `client/vim.rs::find_template` roots its view at that folder), so planting one needs write access to that specific folder, and the fake refuses a cross-datacenter folder match as vCenter does (`client/fake.rs`). Whoever can write there is the hypervisor operator (§4); the template is still trusted by name (§8) |
| A forged `status.perProvider[].zones[]` steers VMs into a zone, or marks a missing template ready | T | **Bounded by RBAC, not prevented.** The provider's field manager writes `vmimages/status`, a grant no tenant role carries and already infrastructure-admin-equivalent (the `status.heldAddresses` row in TB-1). The scheduler's per-zone gate (`reconciler/scheduler.rs::image_ready_in_zone`) can only narrow placement: candidates still come from `Provider.status.failureDomains[]`. A clone into a zone whose template is absent fails `ProvisionFailed` (exact-name lookup in that folder), never clones a different template |
| Per-zone lookups multiply the vCenter work of every `VMImage` reconcile | D | One folder-scoped view per failure domain, each narrower than the old datacenter-wide walk; the client's 10 s connect / 120 s request timeouts apply; an error in one zone reports `LookupFailed` on that zone only and leaves the others' status intact — `template_zone_status`. No rate limit beyond the requeue interval |

#### Proxmox VE (ADR-0074, ADR-0075)

The same boundary, with a credential shaped differently: an **API token**
rather than a password or a client certificate, and a protocol whose errors
live in the HTTP reason phrase and whose mutations are asynchronous tasks.

| Threat | STRIDE | Control |
| --- | --- | --- |
| MITM / attacker-presented certificate, so the token is sent to the wrong host | S, I | TLS is verified by default against `connection.caBundle` (the node's own CA, written by the bootstrap script) or the system roots; a bundle that does not parse is a configuration error, never a silent fallback. The endpoint must be `https://` (`banlieue-provider-connection` VAP, which names `proxmox`); `insecureSkipTLSVerify` needs the audited annotation — `crates/banlieue-proxmox/src/client.rs` (`Client::new`), `deploy/admission/provider-connection.yaml` |
| A redirect carries the `Authorization` header to another host | I | Redirects are **never followed** (`redirect::Policy::none()`); a 3xx is an error — `client.rs`, test `redirects_are_not_followed_so_the_token_stays_put` |
| A hostile or wedged endpoint stalls every reconcile | D | 10 s connect and 30 s whole-request timeouts on every call; every task wait is bounded, and a task still running at the deadline is `TaskTimeout`, never a silent return — `client.rs` (`DEFAULT_*_TIMEOUT`), `api.rs` (`wait_task_with`) |
| A hostile endpoint returns an unbounded response body | D | The body is read through a hard cap and refused beyond it — `client.rs` (`MAX_RESPONSE_BYTES`), test `an_oversized_response_is_refused` |
| A stolen or over-privileged token | E | The token is **privilege-separated** and the role holds only what ADR-0074 Decision 4 lists, on `/vms`, one node, one SDN zone and the ISO storage. Absent by design: console, guest exec, migrate, snapshot, backup, `Sys.Modify`, `Permissions.Modify`. **Revocation** is `pveum user token remove` and takes effect immediately. Deleting a seed needs `Datastore.Allocate` — on a shared storage that also deletes backups and templates and edits the storage definition — so it is a separate `BanlieueSeed` role granted on the dedicated `banlieue-seed` storage alone (ADR-0074 Decision 4, amended after the first live run found the missing privilege). The privilege list is a contract: a client change that needs a new privilege must update the role, the script, the guide and this table together — `scripts/bootstrap-proxmox-host.sh` |
| A machine adopts, reconfigures or destroys a VM banlieue did not create (a same-named VM in another namespace, an administrator's own VM) | T, E, D | Ownership is the machine's **UID**, carried in the VM description by the clone itself (A-16), and every adopt and delete checks it; a VM that shares only the name is never touched — `crates/banlieue-provider-proxmox/src/reconciler/proxmoxmachine.rs` (`allocate_vmid`, `finalize_backend`) |
| Two controllers allocate the same VMID | T | Proxmox refuses the second clone onto an existing id; the provider records `status.vmid` **before** cloning, abandons an id whose VM is not its own, and retries — `proxmoxmachine.rs` (`allocate_vmid`) |
| A hostile node, storage or volume name injects a path component or a header | T | Every path segment is percent-encoded, `..` and `/` included (`wire.rs::encode_segment`); an upload filename containing a quote, CR/LF, `/`, `\` or a leading dot is refused before any request (`client.rs::check_filename`) |
| A `Template` source names a VMID that is a running guest, and banlieue clones it | T, I | The provider verifies `template=1` before publishing the image and again before cloning, and fails naming the VMID — `reconciler/image.rs`, `proxmoxmachine.rs` |
| A half-failed delete leaves the VM or its seed ISO behind | I | The finalizer is released only after the VM is destroyed (with its unreferenced disks) **and** the seed volume is deleted; "already gone" is success at each step and every other failure keeps the finalizer — `proxmoxmachine.rs` (`finalize_backend`), tested for orphaned volumes against the fake |
| A Proxmox error message, which the hypervisor authors, is mirrored onto user-facing status | I | Same posture as §8's verbatim condition messages; the message is the reason phrase plus parameter names, and never the request body or the token — `wire.rs::api_message` |
| Proxmox's wire behaviour differs from what the client assumes | — | The first live lifecycle run (2026-09-30) found two drifts the fake had hidden, both fixed with the fake corrected in the same change: `resize` is a **task** on PVE 9 (it was treated as synchronous, so a start could race it), and seed deletion needs `Datastore.Allocate` (above). **Partly verified live (2026-09-28):** the read-only tier passed against a real node — verified TLS against the node CA, token auth, inventory decoding, and Proxmox's reason phrase surfacing on a 401. The lifecycle tier then passed live (2026-09-30): clone, resize, upload, start/stop/delete and UPID polling. **Still unverified live:** the Kubernetes-side reconcile loop (the provider binary in a cluster), guest-agent addresses, EFI/vTPM, and a *failed* task's exit status. The fake refuses what the real API is documented to refuse. Recorded in §8 |

#### Azure (ADR-0158 to ADR-0165)

The same boundary with no stored credential at all. The provider presents its projected
ServiceAccount token to Entra ID, receives an ARM token for the federated identity (A-29), and
spends it on Azure Resource Manager inside one resource group. Entra accepts the first token
only because the cluster admin published the cluster's ServiceAccount issuer (ADR-0159
Decision 2): that trust is outside banlieue, and is what turns a cluster token into a cloud
credential. Implemented in `crates/banlieue-azure` (client) and `crates/banlieue-provider-azure`
(reconcilers).

| Threat | STRIDE | Control |
| --- | --- | --- |
| MITM or an attacker-presented certificate, so a token is sent to the wrong host | S, I | TLS is always verified against the platform's roots: the Azure clients take no CA bundle and have no switch that skips verification. Both refuse a non-HTTPS base (`is_trusted_base`, `crates/banlieue-azure/src/operation.rs`, called by `ArmClient::new` in `client.rs` and `FederatedToken::new` in `auth.rs`), behind the `banlieue-provider-connection` VAP's `https://` rule (TB-1) |
| A redirect carries the `Authorization` header or the ServiceAccount token to another host | I | Redirects are never followed on either client (`redirect::Policy::none()`, `client.rs`, `auth.rs`); a 3xx is an error |
| ARM answers with an operation URL (`Azure-AsyncOperation`, `Location`) or a `nextLink` on another host, so the next request carries the ARM token there | S, I | Every request goes through `ArmClient::send`, which refuses a URL that is not the configured endpoint or a path below it over HTTPS, so a host that merely starts with the endpoint's host is refused (`ensure_under_endpoint`, `operation.rs`); a handle is also checked when it is built from response headers (`OperationHandle::from_headers`). Tests `an_operation_url_off_the_endpoint_is_refused` and `a_next_link_off_the_endpoint_is_refused` (`client_tests.rs`) |
| A tampered `status.pendingOperation` points the provider's next poll somewhere else, or claims an operation finished | S, T, I | Status write on `azurevirtualmachines` is held by the provider, the controller, the operator and CAPI's aggregate role (TB-15), never by a tenant. The stored URL passes the same endpoint check before a token is attached, an unknown stored `kind` is refused (`OperationKind::from_name`), and an unpollable handle is dropped (`poll`, `crates/banlieue-provider-azure/src/reconciler/azurevirtualmachine.rs`). Whether a handle ends as done or dropped, the machine's state is then read from ARM, never inferred from the handle. Within the endpoint a forged handle can only cause a GET the identity could make anyway. Tests `a_stored_handle_that_was_tampered_with_is_never_polled` (`client_tests.rs`), `an_unpollable_stored_operation_is_dropped` (`azurevirtualmachine_tests.rs`) |
| A hostile or wedged endpoint stalls reconciles or returns an unbounded body | D | ARM: 10 s connect, 60 s request, bodies read through a 32 MiB cap, at most 50 pages per listing (`client.rs`, test `a_response_over_the_size_cap_is_refused`); Entra: 10 s connect, 30 s request, 64 KiB reply cap (`auth.rs`). A 429 is honoured through `Retry-After` and the sweep stops on it (`reconciler/resources.rs`) |
| A value from a CR or a `Provider` injects a path segment into an ARM URL | T | Subscription, resource group, location and resource names pass `segment` (`[A-Za-z0-9-_.()]`, never `.` or `..`) before they reach a URL (`client.rs`); VM and NIC names are derived from the machine's UID (`vm_name`, `nic_name`, `crates/banlieue-azure/src/vm.rs`); the subnet and image IDs travel only in request bodies, never in a URL |
| A stolen token, or an identity that can do more than the provider needs | E | No Secret exists to steal (A-29). The ServiceAccount token is audience-bound to `api://AzureADTokenExchange`, lives an hour and is useless to the API server; the ARM token lives in memory with a redacting `Debug` (`token.rs`), and a 401 drops it so the next call exchanges again (`client.rs`, test `a_refused_token_is_invalidated_so_the_next_call_exchanges_again`). The identity's reach is the custom role at one resource group (`deploy/provider-azure/role-definition.json`, assignment in `deploy/provider-azure/README.md`): no role assignment, run-command, extension, public IP, Key Vault, snapshot, image or gallery write. Image writes belong to a separate publisher identity with its own role and federated subject (A-30, TB-23), so a leaked runtime token cannot publish or delete an image; a compromised provider process can, by creating a publish Job (§8). Revocation is removing the federated credential or the role assignment in Azure |
| A machine adopts, reconfigures or destroys a resource banlieue did not create | T, E, D | The VM and NIC are named from the machine's UID and tagged with it (A-16). A NIC with that name but another owner's tag is left alone and reported `ForeignResource`; the finalizer deletes the VM addressed by its UID-derived name, then only resources tagged with the UID, and reports, never deletes, an untagged disk named like the VM (`create` and `finalize`, `azurevirtualmachine.rs`; test `deletion_never_touches_untagged_or_foreign_resources`). Whoever can write in the resource group is trusted with what is in it (§4, §8) |
| The orphan sweep deletes a live machine's resources, or something that is not banlieue's | T, D | A resource is a candidate only if its `banlieue-provider` tag is this `Provider` (`<namespace>.<name>`), its `banlieue-owner-kind` is `AzureVirtualMachine`, its owner UID is not among the machines the provider listed (deleting ones included), it is a VM, NIC or disk, and it was created at least 15 minutes ago; at most 3 deletes per pass, one pass per 10 minutes, and a failed listing skips the pass (`sweep_candidates`, `sweep`, `reconciler/resources.rs`; `run_sweep`, `reconciler/provider.rs`). A provider that serves only its home namespace judges only resources tagged with that namespace, so a tenant machine its listing cannot see is never read as gone (ADR-0158 Decision 5, amended 2026-11-25; `sweep_namespace`). Tests `a_home_only_sweep_never_judges_another_namespaces_resources`, `the_sweep_deletes_only_this_providers_orphans`, `the_sweep_spares_a_resource_inside_the_grace_period`, `the_sweep_is_paced` (`resources_tests.rs`). The tags themselves are not protected from resource-group writers (§8) |
| User data (A-2) leaks on its way to the VM | I | Sent once, base64 in the create body, over verified TLS; capped at 64 KiB before it is sent (`AzureVirtualMachineSpec::validate`, `crates/banlieue-api/src/infrastructure/azure_virtual_machine.rs`; `VmRequest::to_body`, `vm.rs`); never logged. What Azure keeps afterwards is §8's |
| A login path into the guest exists that nobody declared | S, E | No public IP is ever requested (`NicRequest::to_body`, `vm.rs`), and the role cannot create one. The admin user `banlieue` gets a fresh ed25519 public key per create whose private half is never kept, with password login disabled (`throwaway_public_key`, `crates/banlieue-provider-azure/src/sshkey.rs`; `vm.rs`). `provisionVMAgent: false`, and the role holds no run-command or extension action, so neither banlieue nor its identity has a channel into the guest (ADR-0163). Any login the guest offers is the image's and the user data's |
| A guest asserts `GuestReady` before it is ready, or a compromised guest asserts it to be handed out sooner | S, T | **Bounded, not prevented**, as on every backend: `GuestReady` is ARM's provisioning state, which a banlieue image sets by reporting ready itself (ADR-0163 Decision 1, `crates/banlieue-azure/src/types.rs`). It is liveness, never integrity (ADR-0043 Decision 9); banlieue reads nothing from inside the guest |
| A forged or tampered goal state from something posing as WireServer injects into the `Health` document, or makes the guest report for another VM | S, T | WireServer is Azure's host-local address (`168.63.129.16`), reached over plain HTTP by Azure's design; the helper's address is a compile-time constant with no flag or environment override (`crates/banlieue-guest-azure/src/ready.rs`, `cli.rs`, test `nothing_else_parses_not_even_an_address`). It echoes only `Incarnation` (digits), `ContainerId` (hex and `-`) and `InstanceId` (`[A-Za-z0-9._-]`), each at most 128 bytes, and refuses anything else before building XML (`goalstate.rs`, test `a_value_that_could_inject_xml_is_refused`); responses are capped at 1 MiB with 10 s timeouts (`http.rs`). The worst a spoofed WireServer can do is withhold or misdirect the ready report, which only affects `GuestReady` (liveness, as the row above); nothing it returns is executed or stored |
| Hostile `ovf-env.xml` or customData exploits the parser, or customData that is not a cloud-config is applied | T, E | The provisioning disk is Azure's, built from the VM's own `customData`, which only a principal who can create the VM sets (A-2). The helper mounts it read-only with `nosuid,nodev,noexec`, reads at most 256 KiB, refuses a DTD, CDATA, entity references, a second `CustomData`, invalid base64, more than 64 KiB decoded, and anything that is not UTF-8 `#cloud-config` without NUL, writing nothing then (`ovf.rs`, `customdata.rs`; tests `a_doctype_is_refused`, `a_shell_script_is_refused`, `refused_custom_data_writes_nothing_and_keeps_an_existing_file`). The admin password in the same document is never extracted or logged (`only_custom_data_is_extracted_never_the_password`, `the_log_never_carries_custom_data_or_the_password`) |
| The guest reports ready before user data is applied, or from recovery | T | `customdata` runs in the `boot` stage, so a raw image's recovery boot writes it before AuroraBoot's reset and the first active boot applies it from its first stage; `banlieue-azure-ready.service` is ordered after `cos-setup-network.service` (Kairos's last boot stage) with conditions refusing recovery, and the helper refuses a boot that is not active or passive (`azure_guest.rs`, tests `ready_is_a_unit_after_kairos_network_stage_never_a_stage_step`, `the_ready_unit_never_runs_in_recovery`; `ready.rs`, test `recovery_never_reports`). Not yet observed on Azure (§8) |
| A consumer checks an Azure member's attestation against an anchor banlieue published | S | Nothing is published: the controller mirrors no EK certificate for an `AzureVirtualMachine` (`crates/banlieue-controller/src/reconciler/status_mirror.rs`), so a claim on an Azure pool carries none, and EK-based attestation (ADR-0049) has no banlieue anchor there (§8). Trusted Launch is requested only for a `tpmEnabled` class (`vm.rs`, ADR-0165) |
| A `VMImage` author, or a direct writer of an `AzureVirtualMachine`, boots an arbitrary image or joins an arbitrary subnet | T, E | A `Template` source must be shaped like a gallery image version ID (`is_gallery_version_id`, `reconciler/image.rs`), and the controller takes the subnet from the `Provider`'s network class (`build_azure_virtual_machine`, `crates/banlieue-controller/src/reconciler/infra.rs`); both are platform-admin objects (§7.5). ARM then allows only images and subnets the identity was granted read and join on. For a `Url` source the controller boots only the version in the chosen `Provider`'s **ready** `status.perProvider` row, and only when that row's `sourceDigest` equals the build's checksum, so a row left from an earlier build is not booted (`published_azure_version`, `infra.rs`); the provider marks a row ready only after reading the version from Azure and finding it `Succeeded` with this `VMImage`'s owner tag and the digest tag (`classify_version`, `decide_publish`, `crates/banlieue-provider-azure/src/reconciler/gallery.rs`). How the bytes got there is TB-23. banlieue does not verify a `Template` image's content |
| ARM's error text, which Microsoft authors, is mirrored onto user-facing status | I | Truncated to 512 characters (`MAX_MESSAGE_LEN`, `crates/banlieue-azure/src/error.rs`), never the request body or a token; transport errors drop the URL (`without_url`, `client.rs`) |
| Quota read as capacity is wrong or stale, steering placement (ADR-0161) | T, D | Bounded to that `Provider`, as the false-capacity row above: ARM refuses an allocation it cannot make, and the provider reports `ZoneUnavailable` for the controller to place elsewhere (`azurevirtualmachine.rs`) |

### TB-5 — Cluster → shared datastore

Built ISOs are uploaded to `banlieue-images/<vmimage>.iso` on a vSphere
datastore — or to a libvirt **storage pool** — and, under deferred install
(ADR-0040), stay CD-ROM-attached to each clone until its installed guest
announces itself, when the provider ejects them (ADR-0044: libvirt since
2026-09-23, vSphere since 2026-10-23). **Datastore-browse in
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
| A VM presents every outward sign of a sealed disk — vTPM attached, `tpmEnabled: true` on its class, `Ready` — while its disk is plaintext on the datastore or storage pool, readable by any hypervisor operator and by the next tenant of the same host | I | **ADR-0048**, since 2026-09-23: `banlieue-controller` rejects `tpmEnabled: true` paired with an `installMode: Immediate` image (or an image with no `template` block, which is a pre-built and therefore pre-laid disk) — `Ready=False`, `reason=ImageClassMismatch`, and **no infrastructure CR is created**, so the machine never reaches a provider that would build it. `crates/banlieue-controller/src/reconciler/virtualmachine.rs` (`image_class_mismatch`), checked after the class and image resolve and before scheduling. This is the only place the combination is visible: `tpmEnabled` is on the `VMClass` and `installMode` on the `VMImage`, so no `ValidatingAdmissionPolicy` can see both. **ADR-0088** (Accepted 2026-10-26, superseding ADR-0048; on unless `--enable-first-boot-sealing=false`) allows the pairing only when the image declares `firstBootSealing`, refuses the declaration without `tpmEnabled` or with the flag off (`FirstBootSealingDisabled`), and otherwise leaves ADR-0048's refusal in place: same function, unit-tested in `virtualmachine_tests.rs`. |
| A clone of a first-boot sealing image boots **plaintext** (a source image older than Kairos v4.4.0, or an `Asserted` image whose policy is missing from `COS_OEM`) and is used as if sealed. A `Url` build's policy does land there for both artifacts: the `iso` path was verified on vSphere (2026-10-22), and for a raw `cloudImage` AuroraBoot copies the merged cloud-config verbatim to `/oem/90_custom.yaml` (ADR-0088 Open question 1, answered 2026-10-26) | I | `GuestReady` for a sealing machine requires every declared partition in the guest's report, else `False`/`FirstBootSealingUnverified`, and past `timeoutSeconds` `FirstBootSealingTimeout`: `crates/banlieue-provider-sdk/src/sealing.rs` (`sealing_gate`), applied in `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs` and `crates/banlieue-provider-libvirt/src/reconciler/libvirtmachine.rs`, unit-tested on both. A `VirtualMachinePool` binds only on `GuestReady`. **`Ready` is not gated** (§8) |
| An ISO template's install tries to encrypt with no vTPM because an operator layer asked for it, or the policy reaches the install configuration | D | The builder never puts encryption in an ISO's install configuration (the policy is a file written from `after-install-chroot`), and refuses an `Immediate` build whose operator layers set `install.encrypted_partitions` (`FirstBootSealingConflict`) — `first_boot_sealing.rs` (`sealing_layer`) |
| A clone that cannot seal (no vTPM, KMS unreachable, an upstream defect) | D | Fails **closed** in Kairos: the boot halts in the initramfs and never reaches plaintext userspace. banlieue refuses `firstBootSealing` without `tpmEnabled`, and reports the halted clone as `FirstBootSealingTimeout`, on libvirt even though no guest agent answers (`libvirtmachine.rs`, `build_status`) and on Cloud Hypervisor even though nothing was reported (`crates/banlieue-provider-cloud-hypervisor/src/machine.rs`, `build_status`, ADR-0112), re-checked every 30 s while the report is due so the deadline is not overshot (`reconciler.rs`, `sealing_report_due`; verified live 2026-10-28) |
| A **pre-laid** image (ADR-0113) is sealed to the build machine's TPM, so every clone shares one key and no clone's seal is its own | I, E | `scripts/prelay-kairos-raw.sh` boots the image with **no TPM**, with every cloud-config moved off `COS_OEM` except AuroraBoot's `01_reset.yaml` and a throwaway user, so no `kcrypt` section is present at build time; it refuses the result if any partition is `crypto_LUKS` (twice: after the build boot, and before writing the image). Each clone seals its own copy of the plaintext partition with its own vTPM: `make ch-sealing-isolation-e2e` passed against a pre-laid image (2026-10-31) |
| A pre-laid image carries the **build boot's identity or logs** (a machine-id, a journal, `wtmp`) into every clone | I | Found while designing it: the build boot logs into `COS_PERSISTENT` and names a journal directory after its machine-id. The script gives `COS_PERSISTENT` a fresh, empty filesystem after the build boot and refuses the image unless it is empty (`scripts/prelay-kairos-raw.sh`). The build user's password is random, never stored, lives only in the recovery boot's in-memory `/etc`, and its config is deleted from `COS_OEM` |
| An operator's cloud-config stages run **once at build time** and their output is shared by every clone (keys, hostnames, anything a stage generates) | I, T | The build boot runs with only `01_reset.yaml` and banlieue's throwaway user; the image's own configs are restored afterwards, so they run in each clone, never at build. Reset-bound stages (`after-reset*`) then run nowhere, recorded as ADR-0113 Open question 2 |
| A tool reports a pre-laid image good when the reset never ran (empty `COS_STATE`), and clones boot nothing | D | Found live: `01_reset.yaml`'s layout stage creates the partitions before the reset, and QEMU exits 0 on SIGTERM. The script requires `cOS/active.img` on `COS_STATE` and deletes the output (keeping the console) on any failure (`scripts/prelay-kairos-raw.sh`) |
| A host, not the builder, pre-lays (or otherwise rewrites) an image, so what runs is not the artifact that was built and pinned by digest | T | ADR-0113 Decision 3 places pre-laying in the builder. The Cloud Hypervisor provider only reflinks or copies a cached image into a guest's own `os.raw` and never writes the cache (ADR-0064 Decision 4, `crates/banlieue-provider-cloud-hypervisor/src/hostfs.rs`). Pre-laying today is an operator running the script on an image before placing it, the same trust as any operator-built `BackingFile` image |
| A sandbox workload mounts the still-attached install ISO and reads the build-time cloud-config overlay baked into it (`VMImage.spec.cloudConfigs`, `isoOverlay`) | I | **ADR-0044**, since 2026-09-23: the medium is ejected when the ADR-0043 `guestInstalled` marker flips, via `virDomainUpdateDeviceFlags` with `AFFECT_LIVE\|AFFECT_CONFIG` — `crates/banlieue-libvirt/src/procs.rs` (`DEVICE_MODIFY_EJECT`). `GuestReady` is published only **after** the eject, so a `VirtualMachinePool` — whose sole readiness input is that condition — cannot bind a member whose installer is still attached. `converge()` also suppresses the ISO from the domain XML it redefines each pass, or a redefine would restore it while status claimed otherwise. **vSphere** (ADR-0044 amendment, 2026-10-23): one `ReconfigVM_Task` gives every ISO-backed CD-ROM a client-device backing with `connected`, `startConnected` and `allowGuestControl` false — `crates/banlieue-provider-vsphere/src/client/vim.rs` (`build_eject_cdroms_reconfigure_spec`), called by `vspheremachine.rs` (`settle_install_media`) before the status that may publish `GuestReady`; `GuestReady=False`, `InstallMediaAttached`, until it succeeds; the eject is **verified live** (2026-10-23, vCenter 8.0.3, two installed `Deferred` VMs). A guest-side `eject` (Kairos `eject-cd`) is not the control: it cannot clear `startConnected` or `allowGuestControl`, so the ISO would return at the next power-on |
| A guest reboots into its still-attached installer and re-runs the install, re-sealing a fresh disk over the previous tenant's workload | T, D | Same control. Both flags are passed deliberately: a `LIVE`-only eject leaves the medium in the persistent definition, where it returns at the next boot. The rendered `<os>` block also stops offering `<boot dev='cdrom'/>` once detached. Since the ADR-0044 amendment (2026-10-12) the disk is listed before the installer (`crates/banlieue-provider-libvirt/src/xml/domain.rs`), so a bootable disk wins even before the eject; the installer is reached only while the disk is empty or unbootable. On **vSphere** a `Deferred` clone still boots the CD-ROM first (ADR-0044 amendment 2026-10-12), so the window is from install to the first pass that sees `guestInstalled`; after the eject the drive has no medium and is not connected at power-on |
| A guest holds the cdrom tray locked so the eject fails, and is handed out anyway | D | `VIR_DOMAIN_DEVICE_MODIFY_FORCE` is **not** passed. A failed eject leaves `GuestReady` unpublished, so the member never becomes available and `provisioningTimeoutSeconds` (ADR-0046) reaps it as poisoned. Failing toward an unavailable member rather than an exposed one is the intended direction. On **vSphere** the opposite choice (ADR-0044 amendment, Decision 4 revised 2026-10-23): a locked tray makes vCenter ask `msg.cdromdisconnect.locked`, and the provider answers Yes to that question only — `client/vim.rs` (`cdrom_lock_question_answer`, `wait_for_eject_task`). Found live: installed Kairos guests hold the tray locked, and the VMs could not be logged into, so not overriding meant the ISO stayed attached forever. The eject happens only after `guestInstalled`, when the installer is no longer needed. Any other question is left for a human; a failed eject is retried at the long requeue |
| A forced re-import (`forceUpload`) cannot replace an ISO a running VM still has connected, and the zone is left with no template because the destroy ran first | D | Found live 2026-10-22. **ADR-0020 amendment (2026-10-23):** with `forceUpload` the Job resolves the target datastore and lists the VMs on it holding that ISO open *before* the `forceCreate` destroy, and refuses naming them — `crates/banlieue-provider-vsphere/src/import.rs` (`ensure_iso_replaceable`, `iso_delete_failed_message`); `client/vim.rs` (`vms_holding_iso`, `holds_iso`). A failed Job is deleted and recreated once its `Failed` condition is 10 minutes old — `reconciler/vmimage.rs` (`import_retry_due`). The root cause, an installed VM keeping the ISO, is the ADR-0044 control above |
| A Proxmox machine's rendered user-data (A-2) sits in a seed ISO on an `iso` storage that anyone with storage-audit rights on the Proxmox side can read | I | Uploaded as `banlieue-<machine uid>.iso`, so it traces to one machine; deleted with the VM (`finalize_backend`); stays attached for the machine's life like the libvirt and Cloud Hypervisor seeds (§8). Put per-VM secrets in `userData`, not in the shared template |
| An `installMode: Manual` image asserts a deferred install it does not perform, and seals nothing | I | **Not controlled.** `Manual` is ADR-0040's escape hatch for a non-Kairos build and banlieue cannot inspect what such an image does. Recorded in §8 |
| A datastore, pool or host filesystem **fills under running guests**, pausing every guest on it (thin disks growing, new clones landing) | D | ADR-0128: each provider publishes the store's measured `freeGiB` and `totalGiB` per storage class, and the scheduler places a new VM's disks (at full size, overrides applied) only where they fit after `storageReservePercent` (default 10 %) and the disks of VMs scheduled there but not yet provisioned (`scheduler.rs::measured_shortfall`, `in_flight_on`); a store `shared` across failure domains counts in-flight disks Provider-wide. Guests already placed can still grow their thin disks into the reserve: that is what the reserve is for, and §8 records the rest |

### TB-6 — Supply chain

This is the strongest area of the project and is largely already ADR-0006.

| Control | Detail |
| --- | --- |
| Provenance | SLSA build provenance on every release. Its subjects are every installable asset: the binary tarballs, `deploy-manifests.tar.gz`, and the single-file YAMLs (`install.yaml`, `crds.yaml`, `admission-policies.yaml`, `network-policies.yaml`, the clusterctl files), all rendered by `make release-manifests` in `.github/workflows/build.yaml` and listed in `checksums.sha256` |
| SBOM + VEX | Generated per release; VEX statements are reachability- and presence-derived, and the generators **fail closed** on empty or oversized inputs (256 MiB cap) |
| Signing | cosign signatures on published images |
| Base images | Digest-pinned (Chainguard + distroless; Debian 13-slim for the pre-lay image), tracked by Dependabot; the banlieue images run non-root |
| Actions | SHA-pinned, with one documented exception (`slsa-github-generator` must be tag-referenced or it rejects its own ref) |
| Privileged triggers | No `pull_request_target`, no `issue_comment`. `docs.yaml`'s `workflow_run` is hard-gated to same-repository runs, with a defence-in-depth re-check before any checkout — fork SHAs are never checked out privileged, and the default-branch cache cannot be poisoned |
| Untrusted input | Event fields reach `run:` steps only through `env:` indirection, never inline interpolation |
| Auto-merge | Gated on `pull_request.user.login == 'dependabot[bot]'` (the PR author, not `github.actor`); major-version updates are held for human review |
| Scanning | CodeQL, OpenSSF Scorecard, SAST, grype/OSV, `cargo audit`, `cargo deny`, gitleaks; Grype gates on both the banlieue image (`grype`, VEX from `.vex/` and the auto-VEX tools) and the pre-lay image (`grype-prelay`, `make grype-prelay`, VEX from `.vex/prelay/`). The Azure guest image has none by design (ADR-0171, amendment 2026-11-26): `FROM scratch` with one stripped static binary, nothing for Grype to match; its crates are gated by `cargo audit` and `cargo deny` on `Cargo.lock` (§8) |
| Crate publication: token custody (S, E) | `CARGO_REGISTRY_TOKEN` (A-21) is read by one job, `publish-crate` in `.github/workflows/build.yaml`, gated on `github.event_name == 'release'`, holding only `contents: read`; fork pull requests receive no secrets. It goes through the SHA-pinned `firestoned/github-actions/rust/publish-crate` (ADR-0100) |
| Crate publication: what is published (T) | The crate is built from the release's own checkout with the tag's version stamped by `make set-version`, after the `test` job passed; `make crate-package` builds the packaged copy alone against crates.io dependencies on every pull request (`crate-package` job) and again before the upload. Only `banlieue-api` is published; no path dependency or file outside the crate can enter it |
| Crate publication: repudiation and re-runs (R) | One version per release tag, equal to the binary's and the CRDs'; `make crate-published` skips a version already on crates.io, so a re-run cannot fail half-way or publish twice. crates.io's own audit log records each upload against the token |
| Pull-request images (T, S) | Images of a pull request are pushed to GHCR only when its head branch is in this repository (`.github/workflows/build.yaml`, `docker` job: `github.event.pull_request.head.repo.full_name == github.repository`), which needs repository write access, the same trust as a push to main; fork PRs stay build-only. They are Cosign-signed like every pushed image, and tagged only `pr-<N>` and `sha-<short>` (with the variant suffix), never `latest` or a version, so nothing that resolves a release reaches them; release manifests pin a digest (ADR-0103). A PR image is still **unreviewed code under the project's namespace**: pull it only to test that PR |
| Install manifests: image pinning (T) | **ADR-0103.** A moved or overwritten GHCR tag cannot change what a fresh install from a release runs: `install.yaml` and the clusterctl components name every banlieue image `<repository>:<tag>@<digest>` (`install_image` and the seeded `ProviderClass.spec.image.digest` in `crates/banlieue-operator/src/bootstrap.rs`), and the kubelet pulls the digest. The `release-manifests` job in `.github/workflows/build.yaml` runs after `docker` and passes `docker.outputs.digest-chainguard`; `make release-digest-check` (`RELEASE_REQUIRE_DIGEST=true`) fails the release rather than publish an unpinned manifest. Tests: `a_digest_pins_every_installed_workload`, `a_seeded_class_can_be_pinned_to_a_digest` |
| Pre-lay image (T, S) | **ADR-0113, amendment 2026-11-04.** `ghcr.io/firestoned/banlieue-prelay` is what the privileged pre-lay Job runs. It is built from `Dockerfile.prelay` by the `docker-prelay` job in `.github/workflows/build.yaml` under the same push rules and tags as the banlieue image (fork PRs build only; PR tags never `latest` or a version), Cosign-signed by digest, with build provenance and an SBOM on main and releases, and the release's asset upload waits for it. `banlieue bootstrap imagebuilder --prelay-image-digest` pins it (`prelay_install_image` in `crates/banlieue-operator/src/bootstrap.rs`, parsed by `parse_image_digest`; tests `a_prelay_digest_pins_the_prelay_image`, `the_banlieue_digest_does_not_pin_the_prelay_image`); without it the imagebuilder pulls the tag. **Grype gate** (amendment 2026-11-04, *a Grype gate for the pre-lay image*): the `grype-prelay` job runs `make grype-prelay` on every pushed image by digest, uploads every finding as SARIF (`grype-<event>-Prelay`), and fails on a finding at or above `high` with a Debian fix (`--only-fixed --fail-on high`); `upload-release-assets` needs it, so a release whose pre-lay image misses a fix publishes no assets. Its VEX is `.vex/prelay/`, never merged into the banlieue image's. Findings Debian has not fixed ship: §8 |
| Azure guest helper bundle (T, S) | **ADR-0171.** `banlieue-azure-guest` runs as root in every Azure image banlieue builds. It reaches the image only at build time, from the OCI image `--azure-guest-image` names, which the imagebuilder accepts only as `<repository>@sha256:<64 hex>` and refuses to start otherwise (`validate_helper_image`, `crates/banlieue-imagebuilder/src/azure_guest.rs`; `azure_guest_image`, `app.rs`), and `banlieue bootstrap imagebuilder --azure-guest-image` refuses the same at parse time (`parse_pinned_image`, `crates/banlieue-operator/src/bootstrap.rs`); a changed digest rebuilds the image (`bundles_match`, `reconciler/vmimage.rs`), so a new helper never slips into an existing gallery version. The image is `FROM scratch` with one file (`Dockerfile.azure-guest`), built from the workspace (`make azure-guest-binary`, static musl, `cargo deny` like every crate; dependencies `libc`, `base64`, `thiserror`). **Published by CI** (amendment 2026-11-26): the `docker-azure-guest` job in `.github/workflows/build.yaml` compiles the binary with `make azure-guest-binary` (`REMAP_RUSTFLAGS`, no runner path in the binary) and pushes `ghcr.io/firestoned/banlieue-azure-guest` under the banlieue image's push rules and tags (fork PRs build only; PR tags never `latest` or a version), Cosign-signed by digest, with build provenance and an SBOM on main and releases. `upload-release-assets` needs it and runs `make azure-guest-release-ref`, which refuses anything but `<repository>@sha256:<64 hex>` and writes the pushed reference to the `azure-guest-image.txt` asset (in `checksums.sha256`) and the release notes, so the operator pins the digest the release signed rather than resolving a tag. No Grype gate: §8 |
| Crypto provider regression (T, I) | **ADR-0170.** A dependency's default features could bring back a crypto library without ML-KEM and, with it, classical-only key exchange. `deny.toml` bans `ring` (`[bans] deny`), so the `cargo-deny` job in `.github/workflows/build.yaml` fails if anything pulls it in (checked: enabling rcgen's `ring` feature fails `cargo deny check bans`); kube's default features are off in the workspace `Cargo.toml` for the same reason. The provider every binary installs is asserted by `crates/banlieue-tls/src/lib_tests.rs` to offer X25519MLKEM768 first |
| Install manifests: digest input (T, D) | `--image-digest` is parsed by `parse_image_digest`, which accepts only `sha256:` and 64 lowercase hex digits (`banlieue_oci::reference::require_digest`), before anything is applied, so a typo cannot pass the install and then fail every pull (`the_digest_flag_accepts_only_a_sha256_digest`) |

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
| `usernamePrefix` in the policy's ConfigMap disagrees with the API server's `--oidc-username-prefix` | T | **Fails in the safe direction, but silently.** Too short a prefix makes the comparison unsatisfiable and every claim is refused; too long forces authors to store the *prefixed* name in `subject.id`, which is the exact shape ADR-0047 Decision 9 was amended to eliminate and which leaves the in-guest agent a value it cannot compare to any JWT. Neither is a bypass. It is a configuration coupling between two independently-managed objects, so §7.6 now names it. The same holds for each `issuerPrefixes` line (ADR-0047 Decision 9, amended 2026-10-13): a wrong prefix refuses that issuer's claims, never admits another's |
| A stolen cached ID token is used to create claims in the victim's name | S, R | **None in banlieue**, and not specific to claims — a stolen bearer token authenticates as its owner everywhere in Kubernetes. It is called out because the *consequence* here is an audit record that says the victim asked for a sandbox. §8 |
| An issuer the site does not use is named in `spec.subject.issuer` | S, R | The `issuers` allowlist in `banlieue-virtualmachineclaim-subject-authorization`. This is a check on the *claim*, not on the caller — nothing reveals which issuer actually minted the caller's token (§8) |
| The agent is pointed at an attacker's JWKS via `spec.subject.issuer` | S, T | Same allowlist, doing double duty — see TB-4. Load-bearing for verification, not merely for audit tidiness (ADR-0049 Decision 7) |
| The issuer runs **on the management cluster** (e.g. Dex with `storage: kubernetes`), so its signing keys are cluster resources; anyone who can read them mints a token for any username | S, E | **Not a banlieue control.** Operator requirement (§7.20): read on the issuer's storage API group only for `cluster-admin`, and the issuer in its own namespace at Pod Security `restricted`. Residual: a cluster admin can mint any identity, which is consistent with cluster-admin being out of scope (§9); §8 records when to move the issuer off the cluster |
| A stolen cached ID token stays valid for the issuer's default lifetime (24 h for Dex) | S | Operator requirement (§7.20): a short ID-token lifetime with refresh tokens. Observed on a reference deployment: 24 h until configured |
| Any account at the upstream identity provider (e.g. any GitHub user, through Dex) can complete a login | S, E | Authentication is not authorization: nothing is bound to `system:authenticated` beyond the Kubernetes defaults, and `scripts/dev-oidc-k0s.sh grant` (`grant_me`) binds one named identity to one namespaced Role. Verified: a freshly logged-in identity is forbidden from listing namespaces |
| **More than one issuer** is trusted (e.g. GitHub through Dex, Google and Auth0 direct) and two share a username prefix, or one prefix nests inside another, so an account at one issuer authenticates as a same-named identity from another | S, E | The prefix is the only part of `request.userInfo.username` that names the issuer. `scripts/dev-oidc-k0s.sh` (`validate_issuers`) refuses empty, equal, nested and `system:` prefixes before writing the file; defaults are `github:`, `google:`, `auth0:`. Operator requirement for hand-written files: §7.6. On the claim path the policy's `issuerPrefixes` then checks each claim against its own issuer's prefix, so a user from one issuer cannot record a subject under another (`deploy/admission/virtualmachineclaim-subject-authorization.yaml`, tested by `crates/banlieue-controller/tests/live_claim_subject_policy.rs`) |
| A direct issuer that allows self-signup (an Auth0 database connection) issues a token for an email address the holder never proved, and `email` is the username claim | S | `claimValidationRules` requiring `claims.?email_verified.orValue(false) == true` on every direct issuer, written by `authn_google` / `authn_auth0` in `scripts/dev-oidc-k0s.sh`. Stricter than the implicit rule `claim: email` applies, which accepts a token without the claim |
| A validly signed ID token minted for **another application** at the same provider (Google and Auth0 sign every tenant client's tokens with the same keys) is presented to the API server | S | Each `jwt` entry's `audiences` is this cluster's own client ID, so `aud` must name it (`authn_google` / `authn_auth0`) |
| The API server refuses an edited authentication file and keeps serving the previous one, so the change looks applied, and the bad file stops kube-apiserver at its next restart | D | `scripts/dev-oidc-k0s.sh` (`await_reload`) waits until the `hash` label of `apiserver_authentication_config_controller_last_config_info` equals the file's sha256 on each controller, and otherwise restores the previous file it kept as `.prev` |
| The API server cannot read its authentication configuration and crash-loops on every controller | D | `scripts/dev-oidc-k0s.sh` (`attach_controller`) owns the file by the API server's user, because k0s runs kube-apiserver unprivileged **with gid 0 and no supplementary groups** (group ownership does not help), restarts one controller at a time, and restores the previous config if the flag is not in the running process within `RESTART_TIMEOUT_SECS` |

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
| A stolen host token publishes **false capacity** on its own Provider: inflated, so the scheduler over-places VMs there, or zero, so it places none (ADR-0107; since ADR-0128 also the measured `memoryAvailableMiB` and `storage[]`) | T, D | **Bounded to that host.** The same `resourceNames`-scoped Role means a token can only misreport its own Provider, so placement elsewhere is unaffected. Over-placement on that host is still bounded by each VMM's `MemoryMax` and the guests' slice `MemoryMax` (ADR-0105): it ends in guests on that host being OOM-killed, never the host itself. The same token could already withdraw the host by publishing no failure domain |
| The provider's slice reads are turned into reads of something else | I | `systemd.rs::Systemd::guest_slice_budget` loads only the two fixed names `GUEST_SLICE` and `VMS_SLICE` and reads two properties; `LoadUnit` and unit properties need no privilege and change nothing (verified unprivileged on systemd 257), and no cluster value reaches the unit name |
| A stolen host token patches **another host's machines** in the same namespace | T, D | **Not controlled** — machine names are unknowable when the Role is written, so `cloudhypervisormachines` is namespace-wide. §8 |
| A stolen host token writes `spec.moveOut`, `spec.moveIn` or `spec.identity` on **another host's machine** (the row above: the provider's Role allows `patch` on every `cloudhypervisormachines` in its namespace), to pull a guest's disk and memory to a peer it controls (ADR-0127) | T, I, E | **`banlieue-cloudhypervisormachine-move-fields`** (`deploy/admission/cloudhypervisormachine-move-fields.yaml`): only `system:serviceaccount:banlieue-system:banlieue-controller` may set or change the three fields, on CREATE or UPDATE; status writes are a separate resource and untouched. `make move-fields-policy-live-test` proves a host provider's and any other user's writes refused and the controller's admitted, against a real API server (`crates/banlieue-controller/tests/live_move_fields_policy.rs`). `spec.identity` is also set at creation or never and immutable by CEL in the CRD. **Opt-in, like every policy in `deploy/admission/`**: nothing reads the fields yet, and the mover is to refuse every move while the binding is missing or not in `Deny` (ADR-0127 Decision 4, amended 2026-11-14; roadmap 14). |
| The host loses the cluster when the one controller its kubeconfig names goes down | D | With ADR-0086 the host's kubeconfig names the control plane VIP, which moves between controllers; the host still verifies the API server's certificate against the cluster CA (TB-13) |
| A stolen token is renewed by the thief and never expires | S | **Revocable, not self-limiting.** The token can create tokens for its own ServiceAccount (that is how the host renews), so a thief who holds it can too. The control is revocation: bound tokens are tied to the ServiceAccount's UID, so deleting the ServiceAccount invalidates every token ever issued for it at once, and the operator recreates it for a fresh `banlieue bootstrap cloud-hypervisor-host` (§7.11). Token creation appears in the API server's audit log. §8 |
| A host's token outlives the host | S | Bound, 24 h by default, renewed only while the provider runs: a host down for longer than one lifetime comes back with a dead token and must be re-issued one — `crates/banlieue-provider-cloud-hypervisor/src/token.rs` |
| Cluster state chooses a host path or bridge | T, E | Machines name **classes**, never paths: the host resolves a class through its own `/etc/banlieue/cloud-hypervisor.toml`, which is `0640 root:banlieue` and read-only to the provider (`ProtectSystem=strict`) — `crates/banlieue-provider-cloud-hypervisor/src/host_config.rs`, `plan.rs`. An unknown class is refused (`PlanError::UnknownStorageClass`, `UnknownNetworkClass`) |
| Cluster state injects a path through an image name, MAC or machine UID | T, E | `plan.rs` accepts an image only as a plain file name (no `/`, no leading `.`), a MAC as six hex octets, and the machine UID as a canonical UUID before any of them reaches a path, unit name or tap name; tap names are derived, never taken from the spec |
| A machine for another host is realised here | T | `reconciler.rs::is_ours`: only machines whose `providerRef` names this host's Provider, in its namespace, are reconciled |
| Cluster state chooses **where a host downloads what it boots** — a `VMImage` status naming an attacker's registry | T, E | The host pulls only a **digest in the one repository its own config names** (`[registry] repository`); anything else is `ForeignReference` and never fetched — `vmimage.rs::check_reference`, re-checked by the import itself (`import.rs`). The cache file name is derived from the digest, never taken from the cluster. See TB-10 |
| Host paths leak into the cluster through status | I | The `Provider` failure domain and the `VMImage` row carry **class names only**; unit tests assert no `/` in either — `provider_tests.rs`, `vmimage_tests.rs` |
| A stolen host token holds a `VMImage` in `Terminating` by never reporting `Released`, or reports it falsely | D, R | The host token writes only its **own** `perProvider` row (it has `vmimages/status` patch, no `vmimages` write), and since ADR-0087 that is true of the *effect* as well as the intent: it applies under a field manager scoped to its own `Provider`, so the write cannot reach another host's row. Withholding blocks deletion of images that host held (removing that `Provider` releases it); a false `Released` only leaves a file on that host. The finalizer itself is the controller's (`crates/banlieue-controller/src/reconciler/vmimage.rs`, `HOST_CACHE_FINALIZER`), so no host token needed `VMImage` metadata write, which would also reach the spec |
| A stolen host token **erases every other host's** `VMImage` readiness rows, or holds the object in a write loop | T, D | **ADR-0087.** `status.perProvider` is merge-keyed on `providerName`/`providerNamespace` (ADR-0015), and each host applies under `banlieue.io/provider-cloud-hypervisor/<ns>/<provider>` built by `provider_field_manager` in `crates/banlieue-provider-sdk/src/ssa.rs`, via `host_config.rs::field_manager`. A field manager owns exactly the field set it last applied, so under the former class-wide manager one host's single-row apply made the API server delete every other host's row; each host's watch then fired and re-applied, which is a write loop reachable with nothing but a host's own legitimate credential. The single-row apply no longer passes `force`, so claiming a row owned by another manager is a conflict rather than a silent takeover. Pinned by `crates/banlieue-provider-libvirt/tests/e2e_vmimage_ssa.rs` in both directions |
| A cluster author asks for a vTPM or install media the host cannot provide | I | `vtpm` is advertised only when the host config has `[tpm]` and its swtpm binaries, setup configuration and CA certificate exist (`provider.rs::gather_facts`); `plan.rs` refuses `tpmEnabled` on a host without `[tpm]`. A `tpmEnabled` VM with an `Immediate` image is refused before scheduling (ADR-0048), and one whose image declares `firstBootSealing` seals the guest's own reflinked `os.raw` with its fresh swtpm and is `GuestReady` only once its vsock report names every listed partition sealed, or `FirstBootSealingTimeout` past the deadline (ADR-0112; `crates/banlieue-provider-cloud-hypervisor/src/machine.rs`, `build_status`), so none is reported healthy while unsealed |
| **A stolen host token reaches every served tenant namespace** (A-11, ADR-0134): on a host that serves tenants the token can read and patch `CloudHypervisorMachine`s, rendered `userData` included, in every namespace its selector picks, and, because RBAC cannot be scoped by label, in every namespace | E, I | Opt-in, not the default: the machine rules live in a **second** ClusterRole, `banlieue-provider-cloud-hypervisor-shared` (`deploy/provider-cloud-hypervisor/rbac/clusterrole-shared.yaml`), which the operator binds only for a `Provider` with a non-empty `namespaceSelector` and prunes when it is dropped (`workload.rs::wants_shared_cluster_role`, tested `no_selector_means_no_shared_cluster_role_binding`), so a host with no selector keeps the namespaced-only token. The always-bound base role grants no machine access (`bootstrap_tests.rs::the_base_cloud_hypervisor_role_grants_no_machine_access`). The shared role holds **no** Secret or ConfigMap, no `create` or `delete` (`the_shared_cloud_hypervisor_role_is_machines_and_namespaces_only`), and is a subset of what the operator may delegate (`the_operator_holds_everything_the_shared_cloud_hypervisor_role_delegates`). The residual widening is §8 |
| A tenant machine names a host's `Provider` to land a guest on it (ADR-0134) | E | The same two layers as TB-1: no tenant verb on machines, and the admission policy lets only the controller set `providerRef.namespace` for `cloudhypervisormachines` and `cloudhypervisormachinetemplates` (`deploy/admission/machine-provider-ref.yaml`); the host also matches a machine by name **and** namespace (`reconciler.rs::is_ours`, via `banlieue_provider_sdk::reconciler::serves_machine`) |
| Guest uids collide, or an image is evicted from under a tenant's guest, because the host only counted the machines in its own namespace (ADR-0134) | D, T | The uid ledger and the image-eviction check list machines across every served namespace and filter by `is_ours` (`reconciler.rs`, `vmimage.rs::live_references`, both through `Context::machines()`); the guest uid pool is per host, so a count that missed a namespace would hand two tenants' guests one uid, the isolation TB-9 rests on |
| A stolen host token asserts, replaces or withdraws its **own host's CA** (A-26, ADR-0146), or forges `status.attestationSubject` on another host's machine | T, S, D | It cannot write the assertion: the per-Provider Role gives `providers` only `get`, `list`, `watch` by `resourceNames`, and write only on `providers/status` (`crates/banlieue-operator/src/workload.rs::external_rules`), so `spec.attestation.hostCa` is an administrator's edit, never the host's (ADR-0143 Decision 2: a status the host's token writes would be the discovery ADR-0049 rejects). A forged `attestationSubject` is the existing patch-any-machine risk (the row above and §8): at worst it gives that guest a wrong node-entry parent, which is a denial for it; it cannot make a certificate verify, because the parent names the CA of the Provider the VM is on and only that host's key signs (mediatore ADR-0049 Decision 2, which this repository does not enforce) |

### TB-9 — Guest VMM → host and other guests

Assume a guest has escaped into its VMM process. What contains it:

| Threat | STRIDE | Control |
| --- | --- | --- |
| The VMM reads or writes another guest's disk, seed or socket | I, T | Each guest runs as **its own uid and its own private group** (`User=<uid>`, `Group=<uid>`, registered in `/etc/userdb` by the installer, or, on a host without userdb, created as `banlieue-g<uid>` users and same-number private groups with the host's own `useradd` and `groupadd`, ADR-0139: `accounts.rs::register_files`, tested `files_mode_creates_a_user_and_a_private_group_per_uid`); machine and run directories are `2770 guest-uid:banlieue` under `0711` roots, and no guest is ever in the `banlieue` group; a uid is given to one machine at a time, under one lock, even when a pool creates members together — `plan.rs::vmm_unit`, `plan.rs::UidLedger`, `hostfs.rs::prepare_dirs`, `scripts/bootstrap-cloud-hypervisor-host.sh`. Live-verified: another user cannot even list a guest's directory |
| The VMM writes anywhere else on the host | T, E | `ProtectSystem=strict` with `ReadWritePaths=` its own two directories, `ReadOnlyPaths=/dev` (which `strict` leaves writable), `ProtectHome`, `PrivateTmp`, `NoNewPrivileges`; Cloud Hypervisor's own seccomp filters and Landlock, with Landlock widened only by one read-only rule per tap on `/sys/devices/virtual/net/<tap>`: `systemd.rs`, `banlieue-cloud-hypervisor/src/types.rs`; reference unit `deploy/provider-cloud-hypervisor/host/banlieue-ch@.service` |
| Two escaped VMMs (or an escaped VMM and its swtpm) talk to each other **around the per-uid isolation**, through the world-writable `/dev/shm` or `/dev/mqueue`, a POSIX message queue or a SysV IPC object | I, T | Every template sets `ReadOnlyPaths=/dev` (no file can be created in `/dev/shm`) and `PrivateIPC=yes` (POSIX queues bypass `/dev/mqueue`, so the namespace is what separates them, and SysV IPC with them): the four `*@.service` files, pinned by `systemd_tests.rs::every_template_is_sandboxed_and_kept_when_failed`. On a systemd older than 248 `PrivateIPC=` is unknown and the unit would start **without** it, silently: the installer lists the directives the host will not enforce and refuses unless the operator names exactly that set (`hardening.rs::problems`, tested `an_old_host_is_refused_without_the_acceptance`), writes it to the host config (`render.rs::host_config`), and drops the line from the rendered units so the journal carries no `Unknown lvalue` (`hardening.rs::drop_directives`); `ReadOnlyPaths=/dev` still closes the `/dev/shm` file route. The residual (SysV IPC and POSIX queues shared on such a host) is §8. Verified on systemd 257 by booting the firmware in a VMM, and running `swtpm_setup` and `swtpm`, under these settings (ADR-0063, amended 2026-11-16) |
| The VMM opens other devices | E | `DevicePolicy=closed` with only `/dev/kvm` and `/dev/net/tun` allowed, beside the pseudo-devices `closed` always allows |
| The VMM exhausts host memory or PIDs | D | `MemoryMax=` guest memory + 512 MiB, `TasksMax=1024` in the unit's cgroup |
| **All guests together** exhaust a shared host's CPU, memory or PIDs (a pool refill storm, or one escaped VMM spawning load) and starve the host's other workloads | D | Every guest and vTPM runs in `banlieue-guests.slice` (`Slice=` in `banlieue-ch@.service` and `banlieue-swtpm@.service`), whose `AllowedCPUs` (refused on cgroup v1, where it would be accepted and never enforced: `settings.rs::check_cpuset_enforced`, tested `guest_cpus_is_refused_on_cgroup_v1`), `CPUWeight`, `CPUQuota` (ADR-0139), `MemoryMax` and `TasksMax` the operator declares at install. The slice is a root-owned file and polkit never allows `set-property` on it, so neither the provider nor a guest can lift it — `deploy/provider-cloud-hypervisor/host/banlieue-guests.slice`, `banlieue-host/src/settings.rs::GuestSlice`, tested `the_guest_slice_is_installed_with_the_declared_budget`, `guest_and_vtpm_units_run_inside_the_slice`. Unset limits leave that dimension unbounded, and `install` says so. §8 |
| **All VMs on a host together** (guests and management-cluster nodes) starve its other workloads | D | Nodes from `scripts/bootstrap-k0s-cluster.sh` run in `banlieue-nodes.slice` (`Slice=` from `render_ch_vm_unit`/`render_ch_swtpm_unit`, or the `50-slice.conf` drop-in `ensure_ch_slice` adds to an older node), so `banlieue.slice` holds every VM and one `systemctl set-property banlieue.slice` bounds guests and nodes together (§7.26). Nothing ships a limit on the parent or the nodes' slice: by default they are bounded only by the host, and the guests by ADR-0105's declared budget (ADR-0106) |
| The provider lifts a budget on `banlieue.slice` or `banlieue-nodes.slice`, or moves a VM out of its slice | T, E | The polkit rule matches only guest and import instances, so it grants nothing on any slice: `scripts/test-cloud-hypervisor-polkit.js` refuses `set-property` on `banlieue.slice`, `banlieue-guests.slice` and `banlieue-nodes.slice`, and `stop` on `banlieue-guests.slice`. `Slice=` is fixed in root-owned unit files and systemd accepts only resource-control properties at runtime on a unit with a file. Around systemd, through cgroupfs: `/sys` (so `/sys/fs/cgroup`) is read-only to the provider, so its `CAP_CHOWN` cannot re-own a `cgroup.procs` to move a process (row below) |
| VMM-controlled `/proc` data (its thread names, its counters) reaches `banlieue host cloud-hypervisor overhead`, possibly run as root | T, D | `crates/banlieue-host/src/overhead.rs` only parses: `stat` is split after the last `)` and read as numbers, thread names are only compared against two fixed patterns, nothing from `/proc` is printed (VM names come from cgroup directory names, which systemd and root create), and the verb writes nothing. It needs no root; run it unprivileged. Skewed numbers are §8 |
| **The VMM plants a symlink** in its own directory so the provider's privileged `chown`/`chmod`/truncate lands on another guest's disk or the shared image | T, E | **Every privileged change the provider makes inside a guest's directory goes through a file handle, never a path**: disks and seeds are created `O_CREAT\|O_EXCL`, then grown, owned and re-moded through that handle (`fchown`, `fchmod`, `set_len`); directories are opened `O_NOFOLLOW\|O_DIRECTORY` before being re-owned; the seed is read `O_NOFOLLOW` — `hostfs.rs` (`own`, `ensure_os_disk`, `write_seed`, `prepare_dirs`), `sys.rs::clone_file`. Tests plant symlinks and assert their targets are untouched (`hostfs_tests.rs`, `sys_tests.rs`) |
| The VMM replaces its API socket so the provider talks to something else, or so the provider's `CAP_FOWNER` re-modes an arbitrary file | S, E | `hostfs::grant_api_socket` opens the path `O_PATH\|O_NOFOLLOW`, requires a **socket** owned by the guest uid in the `banlieue` group with no bits for others, and changes only that opened inode via `/proc/self/fd`; the VMM client re-checks owner and mode before every connect — `banlieue-cloud-hypervisor/src/socket.rs` |
| The VMM returns a malformed or huge API response to the provider | D, T | Typed decoding into bounded structs; errors are reported on the machine's status, never panics — `banlieue-cloud-hypervisor/src/wire.rs`. **Not fuzzed**, unlike the libvirt decoder. §8 |
| The guest (or its VMM) replaces its **EK certificate** with one it made, carrying the expected CN | S, T | The certificate is written by `swtpm_setup`, run by the provider's user, to `<state_root>/ek/<uid>/` (`0700 banlieue`), outside every directory a guest uid owns, and read from there — `plan.rs` (`TpmPlan::ek_dir`), `hostfs.rs::ek_certificates`. Nothing a guest sends is published as its EK |
| The guest deletes its TPM state to get a TPM **manufactured again**, this time with symlinks planted where `swtpm_setup` (run as the provider's user, which can read the CA key) will write | T, E | Whether a TPM was manufactured is the provider-only EK directory, not the guest-owned state: a machine with EK files is never manufactured again; lost state is an error (`hostfs.rs::tpm_manufactured`, `machine.rs::ensure_tpm`, tested in `machine_tests.rs`) |
| A symlink in the TPM state directory turns the provider's `CAP_CHOWN` onto another file when the state is handed to the guest | T, E | Each entry is opened `O_NOFOLLOW` relative to the directory handle and re-owned through that handle; anything but a regular file is refused — `hostfs.rs::adopt_tpm_state`, tested with a planted symlink in `hostfs_tests.rs` |
| The TPM state is handed to the guest while `swtpm_setup` is still writing it, so setup's final state file lands owned by the provider's user and the guest's swtpm cannot read it: the vTPM enters failure mode and the VM never boots | D | Nothing is adopted and swtpm is not started while the manufacture unit is still running; the EK files alone do not mean manufacture finished (`machine.rs::ensure_tpm`), tested by `machine_tests.rs::swtpm_waits_for_the_manufacture_unit_to_finish` with a fake that now writes the EK files before the unit exits, as `swtpm_setup` does. Found live 2026-10-09 (roadmap 17 phase G) |
| A guest's swtpm reads another guest's TPM state, or the CA key | I, E | swtpm runs as the guest's own uid and private group, writing only its state directory and run directory (`plan.rs::swtpm_unit`); the CA key is `0600 banlieue` in a `0700` directory, and manufacture — the only step that reads it — runs as the provider's user (`plan.rs::swtpm_setup_unit`). The attestation CA key of ADR-0146 has the same shape and the same answer, and is the one the provider itself reads (TB-21) |
| A guest floods or stalls the **vsock report** listener, or sends crafted bytes | D, T | Each connection is read with a 10 s timeout and a 16 KiB cap; only an exact `phase=installed` line counts, everything else is ignored, and nothing is ever sent back or executed — `report.rs` (`parse_report`, `Listeners`), tested in `report_tests.rs`. The listener is a Unix socket in the guest's own run directory, never a network port |
| A guest claims **another** guest's installation, or claims "installed" while its installer is still running | S | A report can only arrive on the socket in the reporting guest's own run directory, so it speaks for itself only. A guest that lies about itself makes only itself `GuestReady` early — the same bound as ADR-0043's marker (§8); with `tpmEnabled`, `GuestReady` also waits for the host-minted EK |
| A symlink planted at the report socket path turns the provider's `CAP_CHOWN` onto another file | T, E | The path is unlinked (a link, never its target) before binding; the bound socket is opened `O_PATH\|O_NOFOLLOW`, checked to be a socket the provider owns, and re-owned through `/proc/self/fd` — `report.rs::hand_to_guest`, tested with a planted symlink |
| The installer survives into the installed system and re-runs on a reboot | T, D | When the installed system reports, `vm.remove-device install` in the same pass, `installMediaDetached` sticky, every later start planned without it — `machine.rs`, `plan.rs::without_install_media`, `reconciler.rs`; `GuestReady` stays `False, InstallMediaAttached` until then |
| A guest's VMM reads the shared image cache, or another guest's installer, or follows a symlink the guest planted where its installer copy goes | I, T | The cache stays `0750 banlieue`, closed to guest uids; each `Deferred` machine gets its own copy in its own directory, `0440` owned by the guest, created `O_EXCL` under a temporary name and re-owned by handle; anything but a regular file at the path is removed, never followed, and the copy is deleted on the pass after the eject — `hostfs.rs::ensure_install_media`, tested with a planted symlink in `hostfs_tests.rs`; verified live by `tests/e2e_deferred.rs` |
| The VMM starts, stops or changes **other** systemd units | E | It holds no D-Bus authority; only the provider's user may manage units, through the polkit rule below — `deploy/provider-cloud-hypervisor/host/60-banlieue-cloud-hypervisor.rules` |
| **The provider's user starts a unit as root, or as a user outside the guest range** (a compromised provider process, or anyone holding its uid) | E | Units are instances of **root-owned templates** that fix `User=`, `ExecStart=` and the sandbox (`deploy/provider-cloud-hypervisor/host/*@.service`); the provider chooses only the instance. polkit allows start/stop/reset-failed only on those templates' instances, with the uid instance **inside the guest range** (so not `banlieue-ch@0`), and `set-property` only on VMM instances; systemd itself refuses a transient unit under a name with a unit file and refuses `User`/`ExecStart` changes through `set-property`. Tested by `scripts/test-cloud-hypervisor-polkit.js` (22 cases) and verified on a host as the `banlieue` user (ADR-0063, amended 2026-09-27). Previously the provider created transient units, which polkit could only check by name, so this was possible |
| **A compromised provider turns its own capabilities on the host's kernel interfaces**: `CAP_CHOWN` re-owns the root cgroup's `cgroup.procs` and moves itself out of its cgroup, out from under its `DevicePolicy=` filter (or moves a guest's VMM out of `banlieue-guests.slice`); re-owns a device node, or a sysfs attribute such as `/sys/power/state` or a PCI device's `remove`, then writes it; `CAP_NET_ADMIN` writes a host-wide `/proc/sys/net` sysctl (`bridge-nf-call-iptables`, `ip_forward`, `rp_filter`) | T, E, D | `ReadOnlyPaths=/dev /sys /proc` in the provider unit: each is a read-only mount, so `chown` and writes fail with `EROFS` whatever the capability, and the provider cannot remount without `CAP_SYS_ADMIN`. Deliberately **not** `ProtectKernelTunables=`/`ProtectControlGroups=`: both imply `MountAPIVFS=yes`, which on systemd 257 mounts `/dev` and `/sys` afresh over `ReadOnlyPaths=` and leaves them writable. Pinned by `systemd_tests.rs` (`the_provider_writes_no_device_node_sysfs_or_procfs_and_opens_only_tun`, `the_provider_sets_nothing_that_undoes_its_read_only_paths`); measured on systemd 257 with the provider's sandbox and capabilities in a transient unit: each write above refused, tap creation, its `/proc` reads and the `/dev/kvm` check unaffected (ADR-0063, amended 2026-11-16) |
| A compromised provider opens `/dev/kvm` or another `kvm`-group device (`/dev/vhost-net`) | E | `DevicePolicy=closed` allows it only `/dev/net/tun`, and the `banlieue` user is **not in the `kvm` group**, so leaving the device filter would gain it nothing there either: `install` never adds it and removes an earlier install's membership (`stages.rs::host`, tested `an_older_install_is_brought_to_least_privilege`); `selftest` fails while it is a member |
| The provider starts with an **empty device allow-list**, so taps fail with `EPERM` until a restart (`DeviceAllow=` resolves `/dev/net/tun` once, at start, and skips a missing node silently) | D | The tmpfiles.d entry creates the node at boot and `install` applies it whenever it changes; the unit's `AssertPathExists=/dev/net/tun` refuses to start without it, saying why; `selftest` checks it: `banlieue-cloud-hypervisor.tmpfiles.conf`, `stages.rs::host`, `stages.rs::selftest` |
| The provider passes crafted arguments through a template's environment file | T, E | Only the two templates that run **as the provider's own user** read one, so nothing is gained; values are refused unless single safe words (no whitespace, quotes, `$`, `\\`), and reach `ExecStart` as `${NAME}` — `systemd.rs::EnvFile::render` |
| A guest spoofs its address, or another guest's, on the bridge | S | **On a plain class, not controlled at L2**: the tap is a plain bridge port. The provider reports only complete neighbour entries for the guest's own MAC on its own bridge (`neigh.rs`), but a guest chooses what IP it answers for. §8. **On a class declared `isolate_ports` (ADR-0144), partly controlled**: an isolated port exchanges frames only with non-isolated ports, so a sandbox cannot answer ARP for, or send to, another sandbox (`sys.rs::ensure_tap_isolated`); what is left, toward the gateways, is TB-19 and §8 |
| A failed VMM loops silently, hiding a fault | R | Failed units are kept (`CollectMode=inactive`), their systemd result is reported as `Ready=False, VmmExited`, then cleared and retried with backoff — `machine.rs`, `systemd.rs::failure` |
| Deleting a machine leaves its disk, seed, tap or unit behind | I | `machine.rs::teardown` stops the unit, deletes the taps and removes both directories, then **verifies** each is gone before the finalizer is released (`hostfs::remove_machine`, `host.rs::remove_taps`) — live-verified repeatedly |

**Management-cluster nodes on Cloud Hypervisor (ADR-0099).** These are not
banlieue guests: `scripts/bootstrap-k0s-cluster.sh` writes their units, and
nothing in banlieue manages them. Assume one node's guest escapes into its
VMM:

| Threat | STRIDE | Control |
| --- | --- | --- |
| The VMM reads or writes a **banlieue guest's** disk, seed, socket or TPM state | I, T | It runs as `CH_VM_USER` (`k0s-vm`), which is neither a guest uid nor in the `banlieue` group, so the `2770 guest-uid:banlieue` directories under `0711` roots are closed to it, as to any other user: `render_ch_vm_unit`, and TB-9 above for the guest side |
| The VMM reads or writes **another management node's** disk or vTPM state | I, T | **Not separated**: every management node on the host runs as the same `CH_VM_USER`. All of them are nodes of one cluster, and a management node already holds what a worker or controller holds; §8 |
| The VMM writes anywhere else on the host | T, E | `ProtectSystem=strict`, `ProtectHome=read-only`, `PrivateTmp`, `NoNewPrivileges`, `ReadWritePaths=` its node directory and `/run/k0s-vm-<name>` only; Cloud Hypervisor's seccomp filters and Landlock, widened only by a read-only rule on its own tap's sysfs entry (found live: without it the VMM cannot read `tun_flags` and refuses to boot): `render_ch_vm_unit`, tested in `scripts/bootstrap-k0s-cluster-test.sh` |
| The VMM opens other devices | E | `DevicePolicy=closed` with only `/dev/kvm` and `/dev/net/tun` |
| A local user drives the VMM through its API or serial socket, or the vTPM through its control socket | E, I | `RuntimeDirectoryMode=0700` and `UMask=0077` on both units: the sockets are reachable by `CH_VM_USER` and root only (found during this pass: the runtime directory had defaulted to `0755`): `render_ch_vm_unit`, `render_ch_swtpm_unit` |
| The VMM reconfigures the bridge or other interfaces | E | It holds no capabilities. The tap is created by a root oneshot, `k0s-vm-<name>-net.service`, owned by `CH_VM_USER`, so opening it needs no `CAP_NET_ADMIN`: `render_ch_net_unit` |
| A management node spoofs another LAN host's address on the bridge | S | Not controlled at L2, as for banlieue guests above and libvirt nodes on `LIBVIRT_BRIDGE` (§8) |
| A VMM that cannot start loops fast enough to take its vTPM unit into systemd's start limit, leaving it down after the fault clears (found live) | D | `RestartSec=5s` on the VMM unit: `render_ch_vm_unit` |
| With CH the default, a command run against a cluster built on libvirt creates a second VM with a running node's name and MAC, or `destroy` reports the cluster removed while every domain still runs | T, D | Every command, `destroy` included, first compares each node's resolved VMM with the host (a libvirt domain, or a CH node's unit, of that name) and refuses on a mismatch, naming `DEFAULT_VMM=libvirt`: `check_vmm_matches_host`, tested in `scripts/bootstrap-k0s-cluster-test.sh` |
| A CH node lands on a network nobody chose | T | `CH_BRIDGE`, else `LIBVIRT_BRIDGE`, else the bridge libvirt itself reports for `LIBVIRT_NETWORK`; refused when none resolves, and the chosen bridge is logged: `resolve_ch_bridge`. A bridge without a host address is refused as before: `check_deps_ch` |

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
| A build's **checksum is a value someone declared rather than the file's**, so a consumer verifies against, and a cloud publisher names a version by, a hash that was never checked against what was built (ADR-0010, amendment 2026-11-26) | T | **Measured, not copied**: once the `OSArtifact` is `Ready` the imagebuilder runs a digest Job over the file and records only what it reports, after checking it is `sha256:` and 64 lowercase hex digits (an optional `sha512:` line, nothing else: `digest.rs::parse_digest_result`; tests `anything_else_in_a_result_is_refused`). A checksum the source declares is compared with the measurement and a mismatch fails the build closed (`ChecksumMismatch`; an unsupported algorithm, `ChecksumUnsupported`, before any Job): `digest.rs::apply_digest`, tests `a_mismatched_declared_checksum_fails_closed`, `an_unusable_declared_checksum_fails_the_build_before_any_job`. Until then the build is `Building` (`DigestPending`) with no `file`, so no import, push or publish starts on an unmeasured file (`while_running_the_build_is_building_with_no_file_and_no_checksum`). A `Ready` that outlived its Job's pod keeps the value only for the same `OSArtifact` uid, re-checked against a declared `sha256:` (`carried_digest`) |
| A compromised or wrong **digest Job** reports a digest for something else, so a substituted file is later verified against its own hash | T, S | It holds no API credential and mounts the PVC read-only, so it cannot change the file it measures; its output is validated as above. Forging its termination message needs pod create in `banlieue-imagebuild`, which is already node-root-equivalent (TB-3) and could replace the file itself. Its code is the banlieue image (`--push-image`, which `banlieue bootstrap imagebuilder` sets to the image it installs, digest included: `InstallRole::args_with`, `crates/banlieue-operator/src/bootstrap.rs`), the same trust as the push Job |
| A compromised or wrong **pre-lay Job** publishes content it should not, or a digest for something else (ADR-0113) | T, S | It holds no API credential; its only output is a termination message the imagebuilder accepts only as `sha256:` and 64 lowercase hex digits (`prelay.rs::parse_prelay_result`), and every consumer verifies the file against that digest. It verifies the built raw file against the source's `checksum` **before** touching it (`PRELAY_VERIFY_CHECKSUM`, `verify_checksum` in `scripts/prelay-kairos-raw.sh`), so SEC-004's chain runs source checksum → builder digest. The Job's code is `--prelay-image`: by default the published, signed `banlieue-prelay` image (TB-6), or the operator's own; the same trust as the build images the namespace already runs |
| The image booted at build time **escapes QEMU** onto the build node, into a privileged container | E | QEMU runs with its seccomp sandbox (`-sandbox on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny`), no network device and no TPM (`scripts/prelay-kairos-raw.sh`); the guest is the image being built, which every clone will run anyway. Residual risk in §8 |
| The pre-lay Job **rewrites the built file**, so what was verified is not what was transformed | T | It writes a new file (`prelaid_file_name`) and refuses an existing output; the built file is read only. Only `Done` moves the artifact to the new file; a failed or refused pre-lay publishes `Failed` with no `file`, so nothing imports a half-made image (`prelay.rs::apply_prelay`, `apply_refusal`, tested in `prelay_tests.rs`) |
| The registry reads images, including cloud-config baked into them (A-2) | I | **Not controlled by banlieue.** Treat the registry as holding A-2 (§7.13); per-VM secrets belong in `VirtualMachine.spec.userData`, delivered per clone, not in a shared image — as TB-5 says for datastores |
| The registry withholds or deletes an image | D | The import unit fails, the row reports `ImportFailed` with systemd's reason, and the next reconcile retries. Images already cached, and running guests, are unaffected |
| An oversized or hostile layer exhausts the host | D | The layer is capped at its manifest-declared size while streaming, and so is the **decompressed** stream: the push records the uncompressed length as a layer annotation (`io.banlieue.disk.size`), covered by the manifest digest the host pins, and the pull refuses to write past it or to finish short of it — `crates/banlieue-oci/src/sparse.rs` (`with_limit`), `client.rs::pull_file`. Superseded pulls are evicted beyond `[registry] keep_unreferenced`, and a deleted image's file is removed (`vmimage.rs::eviction_candidates`, `release`), so steady-state growth is bounded by the images in use. §8 |
| Eviction deletes an image a guest depends on | D, T | Only digest-named pulls are candidates, never admin-placed files; a file is kept while this host's row of a live `VMImage` resolves to it or one of its machines names it (`referenced_files`). A running guest's OS disk is its own file (reflink or sparse copy), so removing the cache file cannot touch it |
| The import process writes outside the image cache | T, E | Its own unit, an instance of the root-owned `banlieue-ch-import@.service`, as the provider's user: `ProtectSystem=strict`, `ReadWritePaths=` only the storage classes' `images/`, `ReadOnlyPaths=/dev`, `PrivateIPC=yes`, `ProtectHome`, `PrivateTmp`, `NoNewPrivileges`, `DevicePolicy=closed` with no devices: the template file, checked by `systemd_tests.rs`. Writes go through a temporary name and a rename; a copy into a second class is created `O_EXCL` (`sys.rs::clone_file`) |
| A bundle naming a **rogue or over-broad CA** makes the push Job trust an impostor registry (a man in the middle reads the push credential and the artifact, A-13 and A-2/A-3) | S, I | **The trust anchor is the operator's choice and nothing else's**: it is named only by the imagebuilder's own configuration (`--registry-ca-bundle-*`, set by whoever edits its ConfigMap and Deployment), never by a `VMImage`, `Provider` or tenant object. The Job mounts one key, read-only, and **adds** the certificates to the public roots (`tls.rs::http_client`). A man in the middle cannot change what a host boots: the host pulls with its own trust store, a digest in its own pinned repository (TB-8). It can capture the push credential, so scope that credential to the one repository (§7.13). §8 |
| A `--registry-ca-bundle-key` that is not a plain file name escapes the mount or names another file | T, E | The key is validated when the flags are resolved, before any Job exists: letters, digits, `.`, `_`, `-`, not `.` or `..`, at most 253 bytes, and only that key is projected as a volume item — `push.rs::CaBundle::new`, tested in `push_tests.rs` and `app_tests.rs` |
| An empty, unreadable or unparsable bundle **silently leaves the Job on the public roots** while the operator believes a CA is pinned | S | Fails closed: a bundle with no certificate, or that cannot be read, fails the push with its path, and the VMImage row reports the failure — `crates/banlieue-oci/src/tls.rs::{http_client, root_certificates}`, `tls_tests.rs` |
| Verification is switched off to get past a certificate error | S, I | There is no such option, in the client or the Job: the only alternative to a CA is `--registry-plain-http`, which is explicit, named for a private network, and unchanged by ADR-0110 — `oci_push.rs`, `app.rs` |
| The CA bundle is tampered with in the build namespace | T | A ConfigMap or Secret in `banlieue-imagebuild`: the same RBAC that protects the push Secret (A-13) and the build namespace's other Secrets (§6 TB-3). Its worst case is the first row; a host is unaffected |
| Registry credentials leak | I | Push: a Secret in `banlieue-imagebuild`, mounted only into the push Job. Pull: files readable by `root:banlieue` only, never a cluster Secret, so ADR-0060's "no Secret reads" holds for the host (`host_config.rs`, bootstrap script) |

### TB-11 — Installer (root, once) → Cloud Hypervisor host

`banlieue host cloud-hypervisor install` runs as root, at an operator's request, and places
everything A-14 names. It is the one piece of banlieue that is root by
design, so its controls are about two things: what it installs is what
banlieue pinned, and a provider's user that was compromised earlier cannot
use a later run as a lever.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A tampered or substituted VMM or firmware download, from upstream or a mirror | T | HTTPS only (rustls), and a **sha256 per file**, every file fetched and verified before any is installed; on a mismatch nothing is written and the previous symlinks stay: `stages.rs::vmm`, tested (`a_pin_mismatch_installs_nothing`, `make ch-host-install-test` step 5). For the pinned release the digest is **compiled in** whatever URL is used, and a digest flag for a pinned file is refused, so a mirror chooses where bytes come from, never what they must be: `release.rs::resolve`, tested (`a_mirror_url_keeps_the_compiled_pins`, `a_digest_flag_for_a_pinned_file_is_refused`). Any other release: §8 |
| The installed VMM is older than the release the client is written against | T | The default is the pinned release (`pins_tests.rs` asserts it equals `spec/PIN` and the client's gate). A chosen `--vmm-version` below the gate's `major.minor` is refused **before any fetch**, by the comparison the provider makes at runtime: `release.rs::check_vmm_version`, tested (`a_vmm_older_than_the_client_gate_is_refused`, `make ch-host-install-test` step 6). The provider's own gate still refuses an older VMM at runtime (ADR-0061 Decision 5). A newer one: §8 |
| A `--vmm-version` or `--firmware-tag` names a path outside `/opt/banlieue` (`../..`), so root writes or links there | T, E | Both become one directory name: letters, digits, `.`, `_`, `-`, not starting with `.`, refused otherwise before anything is fetched: `release.rs::check_dir_name`, tested (`unsafe_or_unparseable_versions_and_tags_are_refused`) |
| A mirror URL downgrades the transport (`http://`, another scheme) | T, I | Refused when the flag is read (`release.rs::check_url`, tested `a_url_that_is_not_https_is_refused`), and the client is `https_only` regardless (`fetch.rs::https_client`). The sha256 check above holds even over a hostile transport |
| A spoofed digest for a non-pinned release, from `api.github.com` | S, T | Read over HTTPS verified against the system trust store, only when no digest flag was given, and only by the installer, never the provider: `fetch.rs::GitHubDigests`. An unreachable or unparseable answer stops the install before any fetch and names the flag to pass (`an_unreachable_github_names_the_digest_flag`, `githubs_release_json_yields_each_assets_digest`). What GitHub itself publishes is trusted: §8 |
| A hostile server makes the installer buffer a release document without bound | D | `fetch.rs::MAX_RELEASE_DOCUMENT_BYTES`, enforced while streaming (`get_capped`) |
| Following a VMM change rewrites more of the host config than `[vmm]`, or a file an admin broke | T | Only `[vmm]` and, since ADR-0139, `[hardening]` are replaced, through the provider's own type, and the result is parsed back before it is written (`render.rs::with_managed`, tested `with_managed_replaces_only_the_vmm_section`, `a_rerun_updates_the_accepted_list_and_nothing_else`); a file that does not parse is left untouched (`the_host_config_is_kept_unless_forced`); the EK CA is not rotated (`the_host_configs_vmm_section_follows_the_release`). The file lives in root-owned `/etc/banlieue/`, so this write gives the provider's user no lever |
| The provider's user (compromised earlier) plants a symlink in a directory it owns — state root, EK CA directory, storage class — so the root installer chowns, chmods or writes another file | T, E | Every ownership and mode change acts on a handle opened `O_NOFOLLOW`; every write is a temporary file created `O_EXCL` beside its target and renamed over it; `mkdir` refuses anything but a directory — `real.rs`, tested with a planted symlink (`a_symlink_planted_where_a_directory_goes_is_refused`). Residual: §8 |
| The installer weakens a directory another package owns (polkit's `rules.d` is `root:polkitd`) | T | Directories the OS or a package owns are created when missing and otherwise never re-moded or re-owned — `stages.rs::shared_dir`, tested; found by a dry run on a host |
| A read-only verb changes a host | T | `preflight` and `status` take `ops::Probe`, which has no mutating methods; `selftest` removes its scratch directory; asserted in unit tests and in the container test (step 3) |
| A reconcile path reaches root installer code, or its subprocesses | E | Crate boundary: no provider crate depends on `banlieue-host` (`boundary_tests.rs`); the subcommand is behind the binary's `host` feature |
| A setting (flag, cloud-init environment) breaks out of the TOML or a unit file it is rendered into | T, E | Class names are `[a-z0-9-]`, values refuse whitespace and quotes; the host config is serialized from the provider's `HostConfig` and parsed back before it is written; a template placeholder left unfilled is an error — `settings.rs`, `render.rs`, tested |
| A guest-slice flag (`--guest-cpus`, `--guest-memory-max` …) injects a directive into the root-owned slice unit | T, E | Each value is parsed against a strict grammar (cpuset list, size with `K`/`M`/`G`/`T` or `1`–`100%`, weight 1–10000, positive count) before it is rendered, and the unit is written by temp-and-rename — `settings.rs::guest_slice`, tested `budget_values_that_could_corrupt_a_unit_are_refused` |
| A hostile server makes the installer buffer without bound | D | Downloads stop at `fetch.rs::MAX_ARTIFACT_BYTES` before the digest check |
| The installer writes the host's identity databases (`files` mode, ADR-0139): a guest account lands on a uid or name another account owns, or a hand edit corrupts `passwd`, `group` or the shadow files | T, E | Accounts are created only with the host's own `groupadd` and `useradd` (they take the locks, keep the shadow files consistent and honour the host's account hooks), as system accounts with no home, no login shell and no password; a guest name or number that belongs to another account is an error, never reused, and `preflight` already refuses any account or group inside the range that is not `banlieue-g`-named: `accounts.rs::register_files`, `accounts.rs::problems`, tested `a_guest_name_with_another_uid_is_a_conflict`, `a_gid_held_by_another_group_is_a_conflict`, `files_mode_uses_groupadd_and_useradd_only`, `files_mode_is_idempotent`. The installer never edits `nsswitch.conf`, PAM or `authselect` state: the preflight only reads it and refuses a host whose `passwd` or `group` has no `files` or `compat` source. Up to `2 × --guest-uid-count` lines are added; `preflight` prints the count. The crate has no uninstall verb, so removal (`userdel`/`groupdel` over the range) is the operator's, by name prefix and range: §8 |
| A flag weakens a named control and nobody can see that it did (`--accept-reduced-hardening`, ADR-0139) | R, T | The flag is not a switch: it must name **exactly** the set the host's systemd cannot enforce (nothing to accept on a host that enforces all; a list that differs is refused), so the acceptance is a recorded, reviewable statement; it is written to the root-owned `[hardening]` section of the host config, kept current on a re-run without `--force`, and a single stage (`--only`) does not bypass the refusal: `hardening.rs::problems`, `stages.rs::install`, tested `a_single_stage_does_not_bypass_the_hardening_guard`, `a_rerun_records_a_new_acceptance_without_force`. Every directive with a minimum systemd version is a named constant, and a test fails if a unit uses a directive that has none recorded (`every_directive_in_every_unit_is_baseline_or_versioned`). Publishing the list in `Provider.status` is not yet implemented: §8 |
| A budget flag is accepted and silently not enforced, so the operator believes a ceiling exists (`--guest-cpus` on cgroup v1, ADR-0139) | R, D | Refused with a message at settings resolution, before anything is rendered, on cgroup v1 and on systemd before 244; `CPUWeight`, `CPUQuota`, `MemoryMax` and `TasksMax` were measured enforced on v1 and stay available. `--guest-cpu-quota` is validated against a strict grammar (a positive integer percentage) like every other slice value: `settings.rs::cpu_quota`, tested `a_cpu_quota_outside_the_grammar_is_refused` |
| An upgraded host keeps running the **old** provider sandbox (a new unit file on disk, the old one loaded), while this document describes the new one | T | `install` restarts a running provider whenever it rewrote the provider unit or binary, and only then (guests keep running, `KillMode=process`): `stages.rs::provider`, tested `a_running_provider_restarts_when_its_unit_or_binary_changes`, and a second identical run restarts nothing (`a_second_install_changes_nothing`). The guide's upgrade path is re-running `install`, not replacing the binary by hand. A running guest keeps the sandbox it started with until its next start, and `install` says so |
| The attestation CA's key is born readable by someone else, or an earlier CA is silently replaced, or a rotation reuses a name for a different key (ADR-0146 Decision 5) | T, I, S | `AttestationCa::create` makes the directory `0700`, chowns the directory and each file to the provider's user **before any key byte is written** (`write_new`: `fchown` then `write_all`), refuses an existing `ca.key` or `ca.crt` (`CaError::AlreadyExists`) and removes a half-made CA; the stage creates nothing when either file is present unless `--rotate-attestation-ca`, which needs a new `--attestation-ca-name` different from the current one and a CA that can still be read, and `--force` regenerates the host config and never rotates (`crates/banlieue-attestation-ca/src/ca.rs`, `crates/banlieue-host/src/stages.rs::attestation_ca`, `settings.rs::attestation_plan`). `create` never touches the directory by path: it opens it `O_DIRECTORY|O_NOFOLLOW`, applies `fchmod`/`fchown` to that handle and creates `ca.key` and `ca.crt` with `openat(O_CREAT|O_EXCL|O_NOFOLLOW)` relative to it, so a symlink the provider's user plants in place of the directory or a file is refused and nothing is written through it; rotation removes the old files with `unlinkat` on the same kind of handle (`AttestationCa::remove_files`, `Host::remove_attestation_ca`). Tested `create_refuses_to_overwrite_an_existing_ca`, `create_with_owner_chowns_directory_and_files`, `create_refuses_a_bad_name_and_creates_nothing`, `create_refuses_a_symlinked_directory_and_touches_nothing_behind_it`, `create_refuses_a_symlinked_key_or_certificate_and_writes_nothing_through_it`, `the_real_removal_never_follows_a_planted_symlink`. Only the directory's last component is `O_NOFOLLOW`; the parent chain is resolved normally (the residual in §8). The key is never printed: the installer prints the certificate's path, its common name and one administrator action (`attestation.rs::admin_action`) |
| A CA with the wrong mode, owner or contents is left in place and the provider is the first to find out | T, E, D | `preflight` and `status` load the CA with the crate's own checks (key `0600`, a directory closed to group and world, no symlinked file, key and certificate matching) and add ownership, which the crate cannot know: the directory and the key must belong to the provider's user (`stages.rs::attestation_state`, `crates/banlieue-host/src/attestation.rs::inspect`). The provider repeats both at start and **does not start** on a failure, naming what to fix (`crates/banlieue-provider-cloud-hypervisor/src/attestation.rs::load_signer`, `check_key_owner`, `app.rs`). The provider checks the key's owner, not the directory's: the directory's owner is checked by the installer only |
| A pending request's key is born readable by someone else, a request replaces another, or an import installs something other than what was asked for (ADR-0146 Decision 7) | T, I, S | `PendingCa::create` makes the directory `0700`, chowns it and each file to the provider's user before the key is written, refuses an existing request and a symlinked directory, and removes half a request; `PendingCa::load` re-checks modes and symlinks; `import` runs every check before it writes, moves the key with `renameat` between handles opened without following links, and a refusal leaves both the request and any live CA as they were: `crates/banlieue-attestation-ca/src/pending.rs`, tested in `pending_tests.rs` and `stages_tests.rs::a_refused_certificate_installs_nothing_and_leaves_the_request`, `rotating_by_csr_keeps_the_live_ca_serving_until_the_import` |
| An unsigned request stops the provider, or a regenerated config names a CA that is not there | D | The CSR and import routes write no `[attestation]` section until the CA is installed, and `render.rs` omits it for them: `stages.rs::attestation_csr`, `attestation_import`; tested `the_csr_route_writes_a_request_and_neither_a_certificate_nor_a_section`, `a_plain_rerun_ignores_a_pending_request` |

### TB-12: Management cluster controllers → external datastore

Only with `K0S_STORAGE_TYPE=kine` (ADR-0085). The datastore is A-17. The
database itself is configured by the operator, not by banlieue, so most
controls here are requirements (§7.18), not code.

| Threat | STRIDE | Control |
| --- | --- | --- |
| The data source's password leaks from the bootstrap host | I | It is read from the operator's untracked env file (`BANLIEUE_ENV_FILE`), never a flag; the generated `k0sctl.yaml` that carries it is created `0600` before it is written: `scripts/bootstrap-k0s-cluster.sh` (`generate_k0sctl_config`). k0sctl then writes it into each controller's root-owned `/etc/k0s/k0s.yaml` |
| A crafted data source breaks out of the YAML string into k0s's config | T | A `KINE_DATASOURCE` containing `"` or `\` is refused: `generate_k0sctl_config` |
| Traffic between controllers and database is read or altered on the LAN | I, T | Operator requirement: `sslmode=verify-full` with a CA the operator controls, and a TLS-only `hostssl` rule on the server (§7.18). `sslmode=require` **encrypts but does not authenticate the server**: a LAN host presenting any certificate can intercept the session, the kine password included. Not enforced by the script |
| Recorded datastore traffic is decrypted later by a quantum-capable adversary ("harvest now, decrypt later") | I | Operator requirement (§7.20): PostgreSQL 18 or later with `ssl_groups` offering `X25519MLKEM768`, TLS 1.3 minimum. **PostgreSQL 17 cannot**: its `ssl_ecdh_curve` takes exactly one classical curve (default `prime256v1`), which a reference deployment confirmed by refusing both X25519 and X25519MLKEM768. Residual: §8 |
| Anyone on the LAN connects to the database | S, E | Operator requirement: the database listens only where the controllers reach it, and allows only the k0s role from the controllers' addresses with SCRAM (§7.18). Residual: §8 |
| The database is lost or corrupted | D | Operator requirement: backups of the database are the cluster's backups (§7.18). Residual: §8 |

### TB-13: LAN and tailnet → management cluster control plane VIP

Only with `API_VIP` (ADR-0086), which the bootstrap script requires for a
cluster with more than one controller. keepalived and the reverse proxy are
k0s components, configured by `scripts/bootstrap-k0s-cluster.sh`
(`render_cplb`, `require_api_vip`).

| Threat | STRIDE | Control |
| --- | --- | --- |
| A LAN host sends VRRP adverts and takes the VIP, then impersonates the API server | S | The proxy passes TLS through without terminating it, and the VIP is only a SAN on the controllers' own certificates: a client that verifies against the cluster CA (every generated kubeconfig, kube-proxy, kubelets, the host providers' kubeconfig) rejects any other server. The VRRP password does not authenticate and is not relied on. Residual (denial of service): §8 |
| The same takeover blackholes the API | D | **Not controlled** beyond VRRP re-election once the impostor stops. §8 |
| A second cluster on the LAN claims the same virtual router ID and fights over the VIP | D | A per-cluster VRRP password, generated once into `$WORKDIR/api-vip-authpass` (`0600`) and identical on every controller, so keepalived ignores the other cluster's adverts; `API_VIP_ROUTER_ID` to separate them outright (§7.19). `api_vip_auth_pass` in the script |
| The VIP collides with an address something else holds | D | Operator requirement: reserve it outside DHCP and every MetalLB pool (§7.19). The script has no default VIP and refuses one without a prefix length: `validate_api_vip` |
| A crafted `API_VIP` or password breaks out of the YAML string into k0s's config, or out of the cloud-init `tailscale up` line | T | `validate_api_vip` accepts only `[0-9A-Fa-f.:]+/<digits>` for the VIP, digits for the router ID, and 1-8 letters or digits for the password |
| One controller's loss takes the API away from clients outside the cluster | D | The VIP moves to a surviving controller, and the proxy balances over the `kubernetes` endpoints, which each API server publishes for itself once control plane load balancing is on. A multi-controller cluster without a VIP is refused unless `NO_API_VIP=true`: `require_api_vip` |
| A tailnet member reaches the API through the advertised `/32` route | S, E | Only the VIP's `/32` is advertised, and only after an admin approves it. Reaching the address grants nothing: the API server authenticates every request. Tailnet ACLs decide who may route to it (§7.19) |
| The tailnet's policy is the default allow-all, so every member device and every tagged device (for example a Kubernetes operator's ingress proxies) reaches the VIP | S, E | Reaching the address grants nothing (the API server authenticates every request), but the reachable population is the whole tailnet. Operator requirement (§7.19): grants by group and tag instead of `src * dst *` |
| The VIP's route is advertised but not yet approved, so a client off the LAN sends API traffic to whatever local network it is on | I, S | The kubeconfig pins the cluster CA: the TLS handshake fails before any client credential is sent (client certificates and bearer tokens both follow server verification). Approve the route before switching kubeconfigs to the VIP (§7.19) |
| With konnectivity off, the API servers dial kubelets (10250) and pods directly | I | The kubelet authenticates and authorizes the API server's client certificate; the nodes were already reachable on the LAN (§8, ADR-0085). Konnectivity stays available (`K0S_DISABLE_KONNECTIVITY=false`) where the controllers cannot reach the nodes |

### TB-14: banlieue pods ↔ telemetry consumers

Every role serves `/metrics` on port 8080 and `/livez` and `/readyz` on 8081,
from a hand-rolled listener in `crates/banlieue-provider-sdk/src/httpd.rs`
(ADR-0091, ADR-0093). Trace export leaves the pod only when an OTLP endpoint
is configured (ADR-0092).

| Threat | STRIDE | Control |
| --- | --- | --- |
| Anyone on the pod network reads `/metrics` | I | Metric labels carry no object name, namespace or error text, only controller kinds, error variants, Provider names and VM phases; `runner_tests.rs` asserts no label carries an object's name. Reach is limited by the opt-in ingress rule: `metrics` only from namespaces labelled `banlieue.io/monitoring=true` (`deploy/network-policies/`, §7.21) |
| Label cardinality explodes the scraper (one series per VM or per error message) | D | Bounded by design (ADR-0091): `banlieue_virtualmachines{phase}` replaces a per-object series; `runner_tests.rs` checks 50 objects share one series |
| A malformed, slow or oversized request ties up the health or metrics listener | D | The request line is capped at 1 KiB with a 5 s read timeout; anything else is a 400 and the connection is closed (`httpd.rs`, `MAX_REQUEST_LINE_BYTES`; `httpd_tests.rs`) |
| A standby falsely reports Ready, or a healthy standby is reported broken and stalls rollouts | T, D | Readiness is "serving or able to take over" (`crates/banlieue-provider-sdk/src/health.rs`, `evaluate`): leader with controllers started, or a standby that read the Lease within one lease duration; leadership is in the body and the `banlieue_leader` gauge (ADR-0093) |
| The process keeps running with no health server after a failed bind | D | A bind failure is fatal (`start_health_server` returns `BootstrapError::Bind`; every role exits non-zero) |
| The audit (ADR-0145) puts object names on the scrape or the probe | I | The gauge `banlieue_audit_unreconciled_objects` is labelled only by controller and reason, both code constants (`REASON_MISSING_FROM_STORE`, `REASON_NEVER_RECONCILED` in `audit.rs`); a stale `/livez` answers `503` with the body `stale` and names no object or controller (`health.rs`). Object names go only to the controller's own log, at most `LOG_SAMPLE` (20) per line |
| The audit's authoritative list loads the API server | D | One metadata-only list per minute, in pages of `LIST_PAGE` (500), and none in the first 180 s after start, when a restarted controller is reconciling everything anyway (`audit.rs`, `AuditConfig`); a failed list skips the pass and leaves the verdict where it was |
| Spans reach a collector the operator did not intend, or carry sensitive content | I | Off unless `OTEL_EXPORTER_OTLP_ENDPOINT`/`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set (`crates/banlieue-provider-sdk/src/bootstrap.rs`, `otlp_enabled`); span attributes exclude credentials, user-data and Secret contents (TB-4). The shipped policies open no egress to a collector; the operator adds that rule knowingly (§7.21) |
| Telemetry is forged or suppressed to hide an outage | T, R | Not controlled, and harmless to state: nothing banlieue decides reads telemetry back. Status on the CRs remains the record (§8) |

### TB-15: CAPI controllers → banlieue infrastructure objects

| Threat | STRIDE | Control |
| --- | --- | --- |
| CAPI's manager, or anything that compromises it, creates or deletes `VirtualMachine`s, pools, claims or IPAM claims through banlieue's RBAC | E | **Closed by ADR-0096.** The aggregation label is off `banlieue-controller`; CAPI aggregates only `banlieue-capi-infrastructure` (`deploy/capi/clusterrole-aggregate.yaml`), which grants verbs on `infrastructure.banlieue.io` machines, templates and clusters and nothing else. Since the 2026-11-14 amendment KubeadmControlPlane's manager aggregates the same role, and nothing more; any further aggregation label fails the build. `bootstrap_tests.rs` (`capi_aggregate_role_grants_nothing_outside_the_infrastructure_group`, `only_the_capi_aggregate_role_is_aggregated_to_the_capi_manager`, `capi_aggregate_role_aggregates_into_core_and_kubeadm_control_plane_only`) and `e2e_capi_contract.rs` (against real CAPI v1.14.3) enforce it |
| A new infrastructure kind is added without the CAPI role, or the role drifts wider than the contract | E, D | `capi_aggregate_role_covers_exactly_the_capi_contract_kinds` derives the expected set from the CRDs carrying the contract label, so either drift fails the build |
| CAPI's aggregate role reaches `FirecrackerMachine`s and their templates before anything acts on them (ADR-0137) | E, D | The kinds carry the contract label, so the drift test above covers them (`deploy/capi/clusterrole-aggregate.yaml`). An object CAPI creates is **inert**: no Firecracker provider exists, and `banlieue-controller` neither watches the kind nor holds a grant on it, so no bootstrap data is copied into one and no guest is created. The crossing is reviewed again when the provider lands |
| CAPI's aggregate role reaches `AzureVirtualMachine`s, their status and their templates (ADR-0164), so CAPI's manager can write `status.pendingOperation` or create a machine that chooses its own subnet and image | T, E | The kinds carry the contract label and are covered by the drift test above (`deploy/capi/clusterrole-aggregate.yaml`). A forged operation handle is confined to the ARM endpoint and never trusted as an outcome (TB-4, Azure), and a machine's subnet and image are honoured by ARM only where the identity holds join and read (TB-4, Azure). A CAPI-owned Azure machine gets no bootstrap data from banlieue, as on libvirt, vSphere and Proxmox (§8) |
| CAPI's manager deletes a banlieue machine it owns, destroying a real VM | D | By design: a `Machine` owns its infrastructure object, and deleting the `Machine` is the request. A machine banlieue's controller created is owned by its `VirtualMachine`, not a CAPI `Machine` |
| The clusterctl components file installs something other than what `banlieue bootstrap` installs | T | One definition: the file is `banlieue bootstrap operator --dry-run`, escaped for envsubst, and `make clusterctl-components` checks that unescaping it gives back the dry-run byte for byte. It ships with the release's provenance (TB-6) |
| A published `install.yaml` installs something other than what `banlieue bootstrap` installs | T | Same single definition: `make install-yaml` writes `banlieue bootstrap operator --image-digest <release digest> --dry-run` for the release tag, unescaped, with only a comment header added. It is a SLSA subject and in `checksums.sha256` (TB-6) |
| A principal who may create `Machine`s, or `MachineSet`/`MachineDeployment` templates, names **another Machine's** bootstrap Secret in `dataSecretName` and reads that Machine's join token back out of its own InfraMachine (ADR-0042's confused deputy, CAPI form) | I, E | The principal must itself be allowed to `get` the Secret it names, checked at admission with the CEL `authorizer` on `Machine` `spec.bootstrap.dataSecretName` and on `MachineSet`/`MachineDeployment` `spec.template.spec.bootstrap.dataSecretName`, when set or changed: `deploy/admission/capi-bootstrap-data-authorization.yaml`. Templates are covered because CAPI's own manager, which may read Secrets, copies them into Machines. Verified against a real API server: `crates/banlieue-operator/tests/e2e_capi_bootstrap_data.rs` (`a_principal_may_only_name_bootstrap_data_it_can_read`) |
| The same principal names another Machine's bootstrap **config** in `spec.bootstrap.configRef` (or a template's), so Cluster API copies that config's `dataSecretName` onto its Machine and banlieue copies the data into its InfraMachine; the controller's ownership check ties the Secret to the named config and cannot tell who named it | I, E | The principal must be allowed to `get` the config it names (resource: the kind lower-cased plus `s`, which fails closed for an irregular plural), checked at admission on the same objects: `deploy/admission/capi-bootstrap-data-authorization.yaml`. Behind it, Cluster API refuses a config another Machine already controls (`SetControllerReference`), so only a config not yet adopted (owned by a MachineSet or control plane, or by nobody) was ever reachable. Both verified against a real API server: `e2e_capi_bootstrap_data.rs` (`a_principal_may_only_name_bootstrap_data_it_can_read`, `an_adopted_bootstrap_config_is_not_shared_with_another_machine`) |
| The bootstrap-data policies, `failurePolicy: Fail`, deny another infrastructure provider's users on a shared management cluster, or turn an authorizer error into an outage of every Machine write | D | Both policies carry a `matchConditions` guard on the `infrastructureRef` group `infrastructure.banlieue.io` (`spec.template.spec.` for templates), so only banlieue Machines are evaluated: `deploy/admission/capi-bootstrap-data-authorization.yaml`, tested with another provider's Machine and MachineDeployment in `a_principal_may_only_name_bootstrap_data_it_can_read` |
| A principal creates a `MachineDeployment` (or `Machine`) whose `infrastructureRef` names **another** provider, so the policies never see it, with another Machine's bootstrap Secret or config named in it, then updates only `infrastructureRef` to a banlieue template: the reference is unchanged, so a check that skips unchanged references never runs | I, E | The unchanged-reference skip applies only when the old object's `infrastructureRef` was a banlieue InfraMachine too (`wasBanlieue`); otherwise the update is checked as if the reference were new: `deploy/admission/capi-bootstrap-data-authorization.yaml`. Verified against a real API server for `dataSecretName` and `configRef`, with a readable Secret admitted to show the refusal is the policy's: `a_principal_may_only_name_bootstrap_data_it_can_read` |
| A principal who may create or update an InfraMachine, but not read Secrets, writes an ownerReference naming **another** `Machine`, so the controller copies that Machine's bootstrap data into an InfraMachine it can read, without touching the admission-checked `dataSecretName` | I, S | The controller does not trust the ownerReference alone: only the **controller** `Machine` reference counts as the owner, and before reading any Secret it requires that `Machine`'s UID to equal the reference's and the Machine's `spec.infrastructureRef` (written by Cluster API at creation) to name this InfraMachine, else it reports `MachineReferenceMismatch` and reads nothing, and the provider keeps holding the machine: `capi.rs` (`machine_confirms_adoption`), `bootstrap_data.rs` (`unconfirmed_adoption`), unit tests |
| An ordinary `VirtualMachine` labelled `cluster.x-k8s.io/cluster-name` (a load balancer for a cluster, a kustomize `commonLabels`) has that label copied onto its machine and waits forever for bootstrap data no Machine sends | D | A machine a `VirtualMachine` controls is never CAPI-intended, whatever its labels; Cluster API could not adopt it anyway: `banlieue-api/src/capi.rs` (`is_capi_intended`), unit tests in `capi_tests.rs`, `bootstrap_data_tests.rs` and the Cloud Hypervisor `reconciler_tests.rs` |
| Cluster API is installed after banlieue-controller starts, so delivery stays off and every CAPI machine is held until a restart; or an API error at startup (a 503 while the API server restarts) stops the whole controller, its `VirtualMachine`, pool and claim loops included | D | The reconciler starts on a watch of the one CRD `machines.cluster.x-k8s.io` (`Established`, `v1beta2` served), not on discovery at startup: a later install starts it, and a watch error is logged and retried with backoff without stopping anything: `reconciler/bootstrap_data.rs` (`wait_for_capi_machines`, `crd_serves_machines`, unit tests), `app.rs`. The held machine's message names the controller not running, the namespace not served and v1beta2 Machines not served as the causes to check |
| A `Machine` names a Secret that is not bootstrap data (a Provider's credentials, a registry token), and the controller copies it into a readable InfraMachine | I | The controller **classifies** before it copies: type `cluster.x-k8s.io/secret`, `cluster.x-k8s.io/cluster-name` equal to the Machine's cluster, a controller ownerReference matching `configRef` when set, non-empty UTF-8 `value`, `format` absent, empty or `cloud-config`; anything else is a `False` condition and no copy: `crates/banlieue-api/src/capi.rs` (`bootstrap_value`), unit tests in `capi_tests.rs`, and `a_secret_that_is_not_bootstrap_data_is_refused` in the e2e |
| A refusal reports the Secret's contents in a condition message | I | Rejection messages name the Secret and the failed check, never its data: `capi_tests.rs` (`a_rejection_message_never_carries_the_value`); the e2e asserts the same on a real refusal |
| A guest boots **before** its bootstrap data arrives, so it never joins, or boots with template `userData` instead; including in the window after CAPI creates the InfraMachine and before a `Machine` adopts it | T, D | Cloud Hypervisor: the provider creates nothing, not even a guest uid, for a machine CAPI means (the `cluster.x-k8s.io/cluster-name` label or a Cluster API owner, both set by CAPI at creation) until a `Machine` owns it and `BootstrapDataReady` is `True`: `banlieue-api/src/capi.rs` (`holds_guest`), called by `crates/banlieue-provider-cloud-hypervisor/src/reconciler.rs`, tested through the CAPI ordering in `reconciler_tests.rs`. The controller stops delivering on the same predicate the provider releases on (`BootstrapTarget::creation_started`: `spec.providerID`, or for Cloud Hypervisor also the guest uid), so the two never disagree about a machine, and a running machine moved or restored without its status is neither held nor delivered to again. libvirt, vSphere and Proxmox do not hold yet, and the controller does not serve them: §8 |
| A provider's whole-status apply, built from a stale snapshot, overwrites the controller's `BootstrapDataReady` and releases or re-imposes the hold wrongly | T | Only the controller writes that condition, under its own field manager, and the Cloud Hypervisor provider drops it from every status it applies, so it never owns the entry: `reconciler.rs` (`status_patch`), `capi.rs` (`retain_provider_conditions`), tested in `reconciler_tests.rs` |
| A later edit of the bootstrap Secret rewrites a machine whose guest is already running | T | Delivery is one-shot: once `BootstrapDataReady` is `True`, or once the provider has set `spec.providerID`, the controller never writes `spec.userData` again: `bootstrap_data.rs` (`gate`), unit tests |

### TB-16: Bootstrap operator → running management cluster (`join`)

Only when the operator runs `join` (ADR-0099). The tokens, and the copied
`/etc/k0s`, are A-20.

| Threat | STRIDE | Control |
| --- | --- | --- |
| Growing a cluster rewrites its controllers with stale settings (the k0sctl config predates a VIP, a disabled component or an API server flag), restarting them misconfigured | T, D | `join` writes nothing to an existing controller and never reads or applies `k0sctl.yaml`; on a controller it only reads: `k0s version`, `k0s token create`, `systemctl cat k0scontroller`, `tar -c /etc/k0s`, `k0s kubectl get node/controlnode`: `join_nodes`, `wait_node_ready`, `wait_controlnode` |
| A joining controller runs with settings the other controllers do not have (another datastore, VIP or API server configuration), splitting the control plane | T, D | It is a copy of a running controller, not of the script's settings: that controller's `/etc/k0s` and its `ExecStart` flags, with only the worker flags following the node's role; a `--single` source and unparseable flags are refused: `join_controller_node`, `controller_install_flags`, tested in `scripts/bootstrap-k0s-cluster-test.sh` |
| The datastore password or VRRP secret in `/etc/k0s` leaks on the way to a joining controller | I | Read by `sudo tar` on the controller and extracted by `sudo tar` on the node, through one pipe between two SSH sessions; nothing is written on the operator's machine; ownership and modes are kept (`--numeric-owner`, `-p`), so `k0s.yaml` stays `0600 root`: `join_controller_node` |
| A join token leaks and joins an attacker's machine, as a worker or, with a controller token, holding the cluster CA key | I, E | Minted per run with `--expiry=15m` (`JOIN_TOKEN_EXPIRY`); carried controller → node in one pipe between two SSH sessions, never written on the operator's machine; stored `umask 077` under `/etc/k0s` on the node: `join_worker_node`, `join_controller_node`. A node that does join is visible in `kubectl get nodes` (and a controller in `kubectl get controlnodes`) and can be removed, but a leaked CA key cannot be revoked short of rotating the cluster CA |
| A node joins at a different k0s version from the cluster | D | The controller's `k0s version` must equal `K0S_VERSION`, otherwise `join` stops before minting a token: `join_nodes` |
| The k0s binary on the new node is tampered with | T | Downloaded for the pinned `K0S_VERSION` from `K0S_BINARY_BASEURL` and checked against the release's `sha256sums.txt`: `stage_k0s_binary`. **Only when that file can be fetched**: if it cannot, the binary is used unverified (§8) |
| Something on the LAN answers as the controller or the new node and receives the token, or hands the node another cluster's token | S, I | **Not controlled by the script**: `ssh_run` connects with `StrictHostKeyChecking=no`, as every step of this script always has. The SSH key authenticates the operator to the node, not the node to the operator (§8) |
| A join runs twice, or after a partial failure | D | A node whose role's service (`k0sworker` or `k0scontroller`) is already active is skipped; `k0s install --force` replaces a half-installed service; a node running k0s in another role is refused rather than reset: `join_worker_node`, `join_controller_node` |
| A reset worker joins as a controller over its old Node object, which keeps the worker's labels, so the control plane runs on a node nothing selects as control plane | T | `join` refuses a controller while a Node of that name exists, and says to delete it: `join_controller_node`, tested in `scripts/bootstrap-k0s-cluster-test.sh` |
| A swap retires a node, and a later `vms` recreates it as an empty VM that then waits for a join | D | Retired nodes are listed in `RETIRED_NODES`, which `vms` does not recreate and the node table skips: `node_retired`, tested in `scripts/bootstrap-k0s-cluster-test.sh` |

### TB-17: Provider identity → its Lease → operator → scheduler

ADR-0102 turns each provider's leader-election Lease into an input to
placement. The boundary is who may write a Lease, and what the operator and
the scheduler do with what they read.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A compromised provider rewrites **another** provider's Lease: renews a dead one so it reads `Alive` and keeps receiving VMs, lets a live one age into `Stale`, or takes over its leadership so the real one exits | S, T, D | No shared ClusterRole but the operator's grants any Lease verb: `deploy/{controller,imagebuilder,provider-vsphere,provider-proxmox,provider-libvirt}/rbac/clusterrole.yaml`, guarded by `bootstrap_tests.rs::no_shared_cluster_role_but_the_operators_grants_leases`. Each role writes its Lease through a namespaced Role whose `get`/`update`/`patch` name only its own Lease: the operator's per-Provider Role (`crates/banlieue-operator/src/workload.rs`, `named_rule`), `banlieue bootstrap`'s (`bootstrap.rs::build_leader_election_role`, with `--leader-election-namespace` set to the install namespace) and the GitOps `deploy/*/rbac/role-leader-election.yaml`, checked equal by `gitops_lease_roles_match_what_bootstrap_builds`. Verified with `kubectl auth can-i` on a real API server: each identity may update its own Lease and no other |
| The same grant reaches **beyond banlieue**: node heartbeats in `kube-node-lease`, control-plane leader Leases in `kube-system` | S, T, E | Removed with the row above: Lease rights are namespaced Roles, so no banlieue identity but the operator can write a Lease outside its own namespace, and none but the operator one it does not name. The operator's cluster-wide Lease grant is §8's "holds the union" |
| A provider `create`s a Lease under another provider's name before that provider ever starts, so an `Absent` provider reads `Alive` | S | Bounded: `create` cannot be narrowed by `resourceNames`, so it stays a namespaced rule. A Lease it created but cannot `update` goes `Stale` after `staleAfter`, and `Absent` schedules exactly like `Alive`, so the forgery gains no placement it would not have had |
| A compromised provider writes `status.workload.heartbeat` on another Provider directly (every provider ClusterRole holds `providers/status` `patch`) | T, D | Bounded, not prevented: the operator owns that field and re-applies it with `force` on every reconcile; the forged write is itself a Provider change that wakes the operator, and the requeue is never longer than `REQUEUE_DEFAULT_SECS` (`reconciler/provider.rs::publish_status`, `heartbeat.rs::requeue_after`). The same identity could already rewrite that Provider's conditions and failure domains; ADR-0102 adds nothing it lacked |
| A provider that is down keeps `Ready=True` and keeps receiving VMs (the 2026-10-13 outage) | D | **ADR-0102**: `crates/banlieue-operator/src/heartbeat.rs` (`evaluate`) records `Stale` once the Lease is `staleAfter` (default 60 s) old; `crates/banlieue-controller/src/reconciler/scheduler.rs` (`is_stale`) skips it, `ProviderNotAlive` when none is left; `ProviderHeartbeatStale` Event and `banlieue_provider_heartbeat` gauge. Live-tested against a real API server |
| `Absent` is used to slip past the filter | D | By design `Absent` (no Lease) is not filtered, so `--no-leader-elect` runs and Providers from before ADR-0102 keep working. It is a different state from `Stale` and visible as such (column, gauge); a provider that was ever running leaves a Lease behind and reads `Stale`, not `Absent`, when it stops |
| Clock skew between a host and the operator misjudges liveness | D | Bounded: a renewal stamped in the future reads `Alive` for at most `staleAfter`, and `staleAfter` is four lease durations. Skew larger than that is §8 |
| Someone labels a Provider `banlieue.io/unschedulable` to stop placement on it | D | **ADR-0124**: the label is a placement filter only (`crates/banlieue-controller/src/reconciler/scheduler.rs`, `is_unschedulable`); setting it takes `patch` on Providers, which already lets its holder change every label a selector matches, so it is no new power. A VM whose every candidate is cordoned is `Scheduled=False`, reason `ProviderUnschedulable`, visible and retried. |
| A cordon is bypassed, so a new VM still lands on a host being drained | T | No per-VM override: every VM passes the filter after its selector, before liveness (ADR-0124 decision 4; `scheduler_tests.rs`: `an_unschedulable_provider_takes_no_new_vm`, `every_candidate_unschedulable_is_provider_unschedulable`, `unschedulable_is_reported_before_liveness`). Only the exact value `"true"` cordons (`only_true_makes_a_provider_unschedulable`). |
| Cordoning a host, its provider going stale, a host joining, a neighbour leaving, or an image revision going not-ready recreates the VMs already placed elsewhere (the reconciler re-runs the scheduler every pass, and drift under the default `migrationPolicy` recreates a VM, losing a local disk) | D, T | A VM's own Provider (its `status.scheduled`) stays a candidate through the cordon and liveness filters, so neither changes its decision; only new placements avoid the Provider (`scheduler.rs`, `is_own`; `scheduler_tests.rs`: `a_placed_vm_keeps_its_provider_when_it_is_cordoned_or_stale`, `the_exemption_is_only_for_the_vms_own_provider`). Its recorded placement, when it survives every filter, is returned before the spread, image readiness is not checked for it, and it never counts against itself for anti-affinity (ADR-0125; `place`, `is_current`, `blocked_topology_values`; `a_new_empty_host_does_not_move_a_placed_vm`, `a_deleted_neighbour_does_not_move_a_placed_vm`, `an_image_not_ready_on_its_host_does_not_move_a_placed_vm`, `required_anti_affinity_never_counts_the_vm_against_itself`); a placement that no longer fits still moves (`a_selector_change_moves_a_placed_vm`). The provider keeps reconciling its VMs: that is `spec.paused`, which the label does not touch (ADR-0124 decision 3; ADR-0102 amended, whose stale filter had this gap). |

### TB-18: Sandbox segment → egress gateway → routable network

ADR-0109 puts sandboxes on a segment with no router and lets them out only
through gateway VMs. banlieue's controls are the topology it provisions; the
segment's isolation is host configuration and the gateways' behaviour is
image content, both recorded where they belong. On a Cloud Hypervisor host the
segment is built by ADR-0144, and what is specific to that host (port isolation,
the host rules, the NAT) is TB-19 and TB-20.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A sandbox reaches the routable network directly, bypassing the gateways | I, T | banlieue gives a member **no default route**: the pool's `addressing` carries no `gateway`, so the stamped `networkOverrides` has none, and the seed's `network-config` renders a route only when one is set (`crates/banlieue-provider-sdk/src/cloudinit/network.rs`, test `a_static_nic_without_a_gateway_has_no_route`); a Kairos member writes only `Address=` (`examples/25-isolated-sandbox-segment.yaml`, its shape enforced by `crates/banlieue-api/tests/examples.rs`, `isolated_segment_example_matches_the_types`: one NIC on the segment, no pool gateway). A guest that adds a route itself finds nothing that routes: the segment has no router, and on libvirt the isolated network rejects anything forwarded off its bridge (host configuration, §7.29; verified live 2026-11-05: rejected packets counted on the bridge) |
| A gateway forwards between its two NICs, so the segment gains a route | I, E | The image author's control (`ip_forward=0`, a listener on the `sandbox` NIC only): §7.28 and the §8 row on two-NIC guests. banlieue puts `egress` first in the gateway class, so the first static interface (the one placeholders and `guestinfo` describe, and the only one with a gateway) is the routable one (`isolated_segment_example_matches_the_types` asserts it) |
| A sandbox attacks a gateway, the one component on both sides | E | Bounded by the gateway's own controls (mutual TLS per claim, per-claim allowlist, no forwarding), which are mediatore's (its ADR-0028), and by the gateway being an ordinary VM: its vTPM, sealing and RBAC are banlieue's as for any VM. §8 records the image's hardening as the image author's |
| A sandbox's address collides with a gateway's `sandbox` address and steals its traffic | S, D | Partly: two static addresses held by banlieue-managed VMs collide only if ADR-0083 lets them, and it does not (`address_conflict.rs`). A gateway's `sandbox` address is set by its image on libvirt and Cloud Hypervisor, which banlieue never sees: §8 |
| A member reaches the gateway's control relay or proxy as another claim | S | Not banlieue's boundary: the gateway authorizes on the caller's claim SVID (mediatore ADR-0027, ADR-0028). banlieue mints no identity for it (ADR-0049, amended) |
| An inbound path into a sandbox appears through a gateway | E | ADR-0082: banlieue injects no SSH key or listener through guest data; a gateway accepts nothing on `egress` and is not a jump host (image content, Decision 8) |

### TB-19: Cloud Hypervisor sandbox segment → host and other sandboxes

ADR-0144 gives a Cloud Hypervisor host a sandbox bridge with no uplink and no
host address. Sandboxes share its broadcast domain, so the controls are the
kernel's port isolation (set by the provider from the host's own config) and the
host rules (an operator recipe the binary does not install, ADR-0067 Decision
7). Threats here are the sandbox workload's (§4), assumed hostile.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A sandbox probes, floods or ARP-spoofs another sandbox on the segment | S, T, D | Kernel port isolation: for a class declared `isolate_ports = true` the provider sets `IFLA_BRPORT_ISOLATED` with one `RTM_SETLINK` after the tap joins its bridge and before it is brought up, so the port never forwards a frame un-isolated, and sets it again on every pass (`crates/banlieue-provider-cloud-hypervisor/src/netlink.rs::set_port_isolated`, `sys.rs::ensure_tap_isolated`). An isolated port exchanges frames only with non-isolated ports, so a sandbox reaches the gateways and nothing else on the segment. A tap that cannot be isolated stops the machine from starting: `host.rs::tap_error` |
| Isolation is configured and the kernel cannot enforce it, so the segment looks isolated and is not | T, I | `preflight` refuses it, per declared class, when the bridge's member port has no `brport/isolated` (`crates/banlieue-host/src/stages.rs::isolation_problems`), and refuses a bridge with no member port, since support cannot then be probed. The installed bridges keep a dummy anchor port for this reason (the recipe) |
| Isolation is inferred, or applies to the wrong class | T | Explicit over implicit: a class written as a bare bridge name never isolates (`host_config.rs::NetworkClass::isolates_ports`), nothing reads a class name, and `--isolate-ports` must name a class declared on the same command, a re-run that contradicts the recorded set is refused (`crates/banlieue-host/src/settings.rs::agree_isolation`). The attribute is read from the host's own config at plan time (`plan.rs`, `NicPlan.isolate`), never from a cluster object |
| A VM names a **non-isolating class on the sandbox bridge** (the gateways' class, `sandbox-gw` in the recipe) and sits un-isolated beside the sandboxes | S, T, E | **Not controlled in the tree.** Isolation is a property of the class, not the guest (ADR-0144 Decision 3), and a machine's class comes from its `VMClass`, which a tenant can name (§7.30). §7.28 and §7.32 ask that only the gateway `VMClass` name that class: §8 |
| A sandbox uses the host as a service or a router: its own address on the segment, the uplink bridge's, or the forwarding another tenant enabled | I, E | The recipe's chains: INPUT from the sandbox bridge is dropped on every host address, and FORWARD drops anything from the sandbox bridge to outside it and anything from outside it into it, with `! -o` and `! -i` so frames bridged inside one bridge (`br_netfilter`) are left to the bridge: `BANLIEUE_SBX_INPUT`, `BANLIEUE_SBX_FORWARD` in `docs/src/guides/cloud-hypervisor-sandbox-segment.md`. The sandbox range is never masqueraded and the sandbox bridge carries no host address by design. Exercised in a privileged container (iptables 1.8.9, kernel 6.8), with another tenant's rules present, not on `br_netfilter` or the RHEL 8 kernel: §8 |
| Another tenant of the host reloads the firewall, inserts a jump above ours, or accepts forwarded traffic before our chain runs | T, D | The recipe owns three chains and never flushes or reorders any other rule (`iptables-restore --noflush`, one atomic commit, so a re-apply leaves no gap); `systemctl reload` moves our jumps back to position 1; the unit is `Before=` the provider so no sandbox starts first. Nothing triggers the reload when the other tenant acts: §8 |
| The host's forwarding and `br_netfilter` setting (`ip_forward=1`, FORWARD ACCEPT, bridged frames through iptables) change under us | T | Not ours to change, so the recipe neither sets nor depends on them: the sandbox bridge's drops hold with forwarding on, and the `! -i`/`! -o` form holds with `br_netfilter` on or off. `preflight` does not check the forwarding state: §8 |
| A sandbox claims another sandbox's address toward a **gateway**, or floods the bridge's address table | S, D | **Partly.** Isolation stops the frames reaching any other sandbox, so a claim made on the segment is heard only by non-isolated ports: the gateways. Static neighbour entries on the gateways and `learning off` on the isolated ports would stop the claim there, and are **not built** (ADR-0144 Decision 7): §8. ADR-0083 blocks two banlieue VMs declaring one address on a class (`crates/banlieue-controller/src/reconciler/address_conflict.rs`), not a guest choosing another |
| A sandbox reaches another sandbox or the host over IPv6 | I | The recipe sets `net.ipv6.conf.<bridge>.disable_ipv6=1` on both bridges, and the segment is isolated at L2 whatever the protocol. `preflight` does not check the sysctl: §8 |
| Two hosts hand out the same sandbox address, so a claim or an audit line names the wrong sandbox | R, S | ADR-0144 Decision 2 makes sandbox addresses unique fleet-wide, carved by the operator from one supernet and recorded (never defaulted); banlieue VMs naming one address on one `networkClass` are blocked by ADR-0083 (`address_conflict.rs`). Nothing checks that two hosts' carve-outs do not overlap: §8 |
| A **compromised provider** clears a sandbox's isolation, or puts a tap on the wrong bridge | T, E | **No control beyond ADR-0063's.** The provider holds `CAP_NET_ADMIN` and is already able to reconfigure every tap on its host; this is a host-level compromise, recorded and not defended here (ADR-0144 Decision 7): §8 |
| A **compromised VMM** (a sandbox that escaped) clears its own isolation | E | The VMM holds no capabilities, `NoNewPrivileges` is set, and `/dev`, `/sys` and `/proc` are read-only to it, with Landlock widened only by a read-only rule on its own tap's sysfs entry (TB-9). Port isolation is set by the provider, never by the guest or its VMM |
| The temporary masquerade of the sandbox range a bring-up test needs is left in place | I, E | A recorded, by-hand step with a verification command that must print nothing, removed before the host serves sandboxes (`docs/src/guides/cloud-hypervisor-sandbox-segment.md`, Step 5; ADR-0144 Decision 8). It is not a mode of the design and nothing enforces its removal: §8 |

### TB-20: Uplink segment → host NAT → routable network

The two gateways sit on both bridges. The host masquerades the uplink bridge's
range out of every interface except the uplink bridge itself, so the source seen
beyond the host is the host's existing address and no routable address is added.
The host is therefore the one component that routes for the segment, and it
must route for nothing else.

| Threat | STRIDE | Control |
| --- | --- | --- |
| The sandbox range is masqueraded or forwarded, so the segment is reachable from outside or a sandbox leaves without a gateway | I, E | Only the uplink range is translated, and only toward an interface that is not the uplink bridge: `BANLIEUE_SBX_NAT` (`-s <uplink range> ! -o <uplink bridge>`); FORWARD drops anything from the sandbox bridge to outside it and anything between the two bridges (`BANLIEUE_SBX_FORWARD`, `docs/src/guides/cloud-hypervisor-sandbox-segment.md`). The one exception is the bring-up masquerade in TB-19 |
| Something outside opens a connection inward through the host, to a gateway or a sandbox | E | FORWARD to the uplink bridge accepts replies only and drops new connections from anywhere but the uplink bridge; FORWARD into the sandbox bridge is dropped; INPUT from the uplink bridge accepts established traffic only (`BANLIEUE_SBX_FORWARD`, `BANLIEUE_SBX_INPUT`). Exercised in a privileged container with an outside stand-in, not on the target kernel: §8 |
| A gateway attacks the host, which is its next hop | E | INPUT from the uplink bridge is dropped except replies to what the host started (`BANLIEUE_SBX_INPUT`). A gateway is also an ordinary guest (own uid, TB-9) |
| A compromised gateway reaches **whatever the host can reach**, as the host's address (the cluster, the host's management network, another tenant's container networks), because the host forwards its traffic out of every other interface | I, E | **Not controlled at the host.** The rule `-i <uplink> ! -o <uplink>` accepts any destination, and its ACCEPT ends FORWARD traversal, so it also bypasses a later DROP of another tenant. What a sandbox may reach is the gateway's per-claim allowlist (mediatore ADR-0028) and the gateway's own hardening (TB-18, §8); the host adds no destination filter: §8 |
| Beyond the host, every sandbox and both gateways look like one source, so an outside party's record cannot say which sandbox acted | R | Attribution is by the gateways' own logs, per claim (mediatore). The host's NAT records nothing, and `conntrack` entries are transient. §8 |
| A sandbox or a compromised gateway exhausts the gateways' CPU or the host's connection-tracking table, starving every other sandbox and the host's other tenants of the NAT path | D | Gateway VMs run in `banlieue-guests.slice` under the host's guest budget (ADR-0105), which bounds their CPU, not their traffic. Neither the gateways nor the host rules rate-limit or cap tracked connections: §8 |
| One gateway (or its VM) fails, or the shared address does not move, and every sandbox on the host loses its way out | D | Two gateways per host, both placed on it (ADR-0144 Decision 4). Failover is the gateway image's concern (mediatore) and is an open question in the ADR; the gateways share the host's failure domain, so two protect against one VM failing and not against the host: §8 |
| A gateway speaks as the other gateway, or as the host, on the uplink bridge | S | **Not controlled at L2**: gateway ports are not isolated (they must reach the host and each other) and the two gateways are one image from one author. Treated as one trust domain with the host's next-hop role: §8 |

### TB-21: Guest → per-guest vsock signer → attestation CA

A Cloud Hypervisor host that opts in (ADR-0146) lets each guest ask for a certificate on its own hybrid
vsock. The provider serves one Unix socket per guest, `<run dir>/vsock.sock_1025`; the guest's helper (not built,
mediatore's) makes its own key and sends a CSR; the signer returns one certificate whose subject is the machine's
`metadata.uid`; SPIRE's `x509pop` attestor accepts it against a bundle of per-host CAs. The signer still trusts the
host it runs on: what this boundary changes is custody (banlieue made the key, the provider's user alone reads it)
and reach (the CA's name is in every agent ID it signs). Threats here are the sandbox workload's (§4), assumed
hostile, plus the host's own.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A guest obtains a certificate for **another guest's** identity, by choosing the subject or by reaching another guest's socket | S, E | The subject is never read from a request: `serve` takes it as a parameter and the provider passes the machine's `metadata.uid` (`crates/banlieue-attestation-ca/src/serve.rs::serve`, `crates/banlieue-provider-cloud-hypervisor/src/attestation.rs::signer_subject`). Each guest has its own socket in its own run directory (`2770 guest-uid:banlieue` under a `0711` root, TB-9), handed to that guest's uid by `report::hand_to_guest` (opened `O_PATH\|O_NOFOLLOW`, checked to be the socket this process bound, re-owned through `/proc/self/fd`), so the peer of a request is that guest by listener ownership and another guest's VMM cannot traverse to it. Tested `subject_comes_from_the_argument_not_the_csr`, `each_guest_has_its_own_signer_and_subject` |
| A guest has the host sign something other than a leaf for its key: a CA certificate, a chosen name or SAN, extra key usages, a longer life, another key type | T, E | Only the EC P-256 public key is taken from the CSR (`signer.rs::p256_key`), the CSR's own signature is verified as proof of possession (`check_self_signature`, `ecdsa-with-SHA256` only), and the public key is **rebuilt** into the certificate, never copied. The CSR's subject and extensions are discarded; `LeafProfile` emits exactly `basicConstraints CA:FALSE` and `keyUsage digitalSignature`, both critical, with no SAN and no extended key usage; the serial is 128 random bits; the validity is the host's `certificate_ttl_secs`, bounded to 5 minutes through 7 days at load (`host_config.rs::validate`). RSA, P-384 and Ed25519 keys and a P-256-shaped key on another curve are refused. Tested `issues_a_certificate_with_exactly_the_specified_fields`, `csr_subject_and_extensions_are_ignored`, `refuses_rsa_p384_and_ed25519_keys`, `refuses_an_ec_key_whose_curve_is_not_p256`, `refuses_a_csr_whose_self_signature_does_not_verify` |
| A guest floods, stalls or feeds the signer a huge or malformed request | D, T | One connection is one request: read at most `MAX_CSR_BYTES` + 1 (4096 + 1) bytes under a 5 s deadline, answer one certificate and close; a refusal gets no reply, only a counter. At most 8 connections are served at once per guest; a ninth is closed unanswered. A per-subject sliding window admits 6 requests per 60 s, checked **before** any parsing, and the size is checked before anything else (`serve.rs`, `signer.rs::sign`, `ratelimit.rs`). The limit is per machine, so one guest cannot spend another's. The signer is pure Rust: no subprocess, so a crafted CSR reaches no external parser. Tested `a_huge_request_is_cut_off_not_buffered`, `a_client_that_never_finishes_times_out_and_does_not_block_others`, `rate_limit_is_per_subject_with_an_injected_clock`. Bounded, not blocked: §8 |
| A CSR or certificate reaches a log, an error, a status or a metric | I, R | Nothing logs a body: `SignError` renders fixed strings, `Signer` and `AttestationCa` `Debug` print the name and never the key, the loop logs only the refusal kind at debug level, and the provider surfaces numeric counters (`Outcomes::snapshot`). Tested `errors_and_signer_debug_never_contain_pem`, `nothing_in_the_counters_or_errors_is_a_pem_body`, `debug_output_shows_the_name_and_no_key_or_pem`. What is **not** kept is a record of which certificates were issued: counters only, lost at restart (§8) |
| A guest or its VMM reads the CA key, or another guest's signer socket | I, E | The directory is `0700` and the key `0600`, owned by the provider's user; guests run as their own uids, in no shared group, so neither is traversable (TB-9); `AttestationCa::load` refuses a key that is not `0600`, a directory open to group or world, a symlinked file, or a key that does not match its certificate (`ca.rs::load`, tested `load_refuses_a_key_that_is_not_0600`, `load_refuses_a_group_or_world_accessible_directory`, `load_refuses_a_symlinked_key`), and the provider refuses a key it does not own and does not start (`attestation.rs::check_key_owner`, `app.rs`) |
| The VMM replaces, removes or plants a link at its own signer socket path | T, D | It owns its run directory, so it can: the effect is bounded to itself, since it holds no CA key and the replacement serves only its own guest's connections. The provider unlinks whatever is at the path (a link, never its target) before binding, and verifies through an `O_PATH\|O_NOFOLLOW` handle that the socket is the one it bound before it re-owns it, so a planted link never turns the provider's `CAP_CHOWN` onto another file (`attestation.rs::Signers::ensure`, `report.rs::hand_to_guest`). A signer whose task died, or whose socket vanished, is started again on the next pass; tested `a_signer_that_died_is_restarted`, `a_provider_restart_brings_the_signer_back`. A VMM that substitutes a listener of its own is not noticed by the provider (it sees a socket); the cost is that guest's own attestation |
| The signer outlives its guest, or answers for a guest that was deleted | S | Started before the VMM and stopped, socket removed, when the guest powers off, the machine is torn down, or it is deleted (`machine.rs`, `attestation.rs::stop_for_plan`; tested `the_signer_stops_when_the_machine_is_deleted`, `the_signer_stops_when_the_guest_is_powered_off`). A `Recreate` makes a new machine and so a new subject; the subject is the infrastructure machine's UID, not the VM's (ADR-0143 Decision 3, `machine.rs` publishes it as `status.attestationSubject`). A certificate already issued stays valid until it expires: §8 |
| A host opts in, the administrator never asserts the CA, and a guest is created whose identity nothing can compute (a late, silent failure) | D, R | **The opt-in gate (ADR-0146 Decision 4):** a host whose config has `[attestation]` reports its `Provider` `Ready=False` with `HostCaMissing` (no `hostCa`) or `HostCaUnreadable` (an inline value that is not exactly one certificate or carries a private key), naming `spec.attestation.hostCa`, and creates no new guest while it holds (`provider.rs::host_ca_hold`, `provider.rs` readiness, `reconciler.rs` before the guest uid is allocated). A host with no `[attestation]` is never held and costs no extra call (`provider_tests.rs::a_host_that_did_not_opt_in_never_reports_a_host_ca_reason`). **Honest limits, §8:** a `configMapRef` or `secretRef` source counts as set and is **not** read (the shared provider role grants no ConfigMap or Secret read), the inline value is judged for shape only, not for being this host's `ca.crt`, and a guest already `Ready=True` is not held when the assertion is later withdrawn |
| The wrong CA is trusted for a host: a rogue certificate in `hostCa` or the bundle, or two hosts sharing a name | S, T, E | **Not controlled by banlieue, by design.** The anchor is admin-asserted and never discovered (ADR-0143 Decision 2): `hostCa` is a `Provider` spec field, so it is guarded like the rest of the `Provider` (§7.5, §7.33), and no host token can write it (TB-8). The CA's name is validated against `[A-Za-z0-9._-]`, at most 64 bytes, and refused rather than replaced (`name.rs::validate_name`); uniqueness across the bundle cannot be checked here: §8 |
| A compromised provider process or host mints certificates, or a stolen CA key is used off the host | S, E | **Accepted, §8.** The signer is the provider (a second process would hold the key beside it for no gain, ADR-0143 Decision 4), so a compromised provider holds the key and chooses the subject. Reach is bounded by the CA's name being in every agent ID it signs (mediatore ADR-0049 Decision 2, not enforced here), and by removal from the bundle as the revocation. The key is readable only by the provider's user and root, loaded once into the process, never written elsewhere |
| A certificate is issued for a guest whose host or CA was replaced, and verifies against the old CA | T | A new CA needs a **new name** (`--rotate-attestation-ca`, refused when the name equals the current one), so an agent ID under the old name does not become an agent ID under the new key; rotation overlap is operational: §8 |
| An expired host CA keeps signing certificates no verifier accepts (ADR-0146 Decision 7) | D, R | `HostCaExpired`: the provider is `Ready=False` and creates no guest once the CA is past its `notAfter`, read from `ca.crt` alone (`provider.rs::{host_ca_expiry_hold, ca_not_after}`; tested `an_opted_in_host_whose_ca_has_expired_is_not_ready`); a signer under an expired CA **refuses** (`signer.rs`, `SignError::CaExpired`; tested `a_signer_refuses_once_its_ca_has_expired_and_still_signs_before`); `preflight` refuses an expired CA and `status` shows the days left (`stages.rs`) |
| The reply now carries certificates above the guest's own (ADR-0146 Decision 7) | I, T | The chain is public certificates only, the host CA and intermediates and never the root (`pending.rs::chain_file_contents`; tested `a_signer_under_an_imported_ca_replies_with_the_leaf_then_the_chain`); a self-signed CA's reply is the leaf alone, unchanged. The framing extension needs mediatore's agreement and no guest speaks it yet: §8 |

### TB-22: Installer (root, host) → OpenBao PKI

When an administrator runs the OpenBao route (ADR-0146 Decision 8), the installer, as root, makes one HTTPS
exchange with an OpenBao the administrator names: it logs in with the host's own ServiceAccount token (A-11) or a
one-use token file, posts the pending request (A-27) to a dedicated PKI mount's `root/sign-intermediate`, revokes
its token and imports what came back. It is the only place banlieue code calls a PKI, it runs once at an
operator's request and never in a reconcile path, and the controller and the provider have no part in it. The
OpenBao side (the pinned policy, the JWT role, the uniqueness check, the role's deletion) is the OpenBao
repository's `scripts/attestation-ca.sh`, **not in this tree**, and was tested there on 2026-11-25 against a throwaway
OpenBao (real JWT logins, the installer's five parameters in its JSON types); it has **not been run on a live server**,
and the audit alert is not built. The rows below cite only controls that are in this tree and mark the rest.

| Threat | STRIDE | Control |
| --- | --- | --- |
| A host, or someone holding its token, gets a CA signed under **another host's name** | S, E | **Mostly not in this tree.** OpenBao's policy pins `common_name` to the host's own name and the JWT role is bound to the host's ServiceAccount (ADR-0146 Decision 8, layers 1 and 2): a §8 row until that script exists and is verified. Here: the installer sends exactly the five parameters the policy lists and the name is the pending request's own, checked when settings resolve (`settings.rs::attestation_plan`; tested `a_pending_request_of_the_same_name_is_reused_and_another_name_is_refused`, `the_openbao_route_needs_a_name`) |
| The installer talks to something that is not OpenBao, over a downgraded or intercepted channel | S, T, I | `OpenBao::new` refuses anything but an `https` origin with no credentials, path, query or fragment; the client uses the host's trust store, has no way to skip verification outside tests, and follows no redirect (`openbao.rs::OpenBao::build`; tested `the_real_client_refuses_http_credentials_in_the_url_a_query_and_a_missing_host`). A host whose trust store was altered would trust the wrong server: §8 |
| A token, the ServiceAccount token, a request or a reply reaches a log, an error, argv or the environment | I | Secrets are read from files (`stages.rs::read_secret_line`), never argv or the environment, and held in `Credential`, whose `Debug` prints `<redacted>`; an error names the step and the HTTP status and never a body (`openbao.rs::post_json`; tested `a_refused_login_is_an_error_that_names_the_status_and_not_the_body`, `a_credential_never_prints_its_secret`); the OpenBao token is revoked after use (`openbao.rs::revoke`) and a token file is removed once used, whether or not the signing worked (`stages.rs::attestation_openbao`; tested `the_token_file_is_removed_even_when_the_signing_fails`) |
| A hostile or broken OpenBao returns a certificate that is not the pending key's, is a CA that may sign further CAs, or carries the wrong name | T, E | The reply is only text for `PendingCa::import`, which checks the key, the name, `CA:TRUE`, path length 0, `keyCertSign`, validity, a ten-year cap and the chain's links before anything is written and leaves the request on refusal (`pending.rs::check_signed`; tested by the `import_rejects_*` tests and `stages_tests.rs::a_hostile_answer_is_refused_by_the_import_and_leaves_the_request`). It cannot stop OpenBao signing the right key under a name the request asked for: that is the policy's pin, above |
| A hostile server hangs the installer or makes it buffer without bound | D | 10 s connect and 30 s request timeouts and a 1 MiB reply cap enforced while streaming (`openbao.rs::{CONNECT_TIMEOUT, REQUEST_TIMEOUT, MAX_RESPONSE_BYTES}`; tested `an_oversized_response_is_refused_before_it_is_all_read`, `nothing_listening_is_an_error_not_a_hang`) |
| A flag value injects into the URL path (`../`, `?`, whitespace) so a login or signing goes to another endpoint | T, E | The mount, the auth mount and the role are `[A-Za-z0-9._-]` segments, none empty, none `.` or `..`, checked when settings resolve and again by the client (`openbao.rs::{valid_path, valid_segment}`; tested `a_mount_or_role_that_could_leave_its_place_is_refused`, `a_credential_with_a_bad_auth_mount_or_role_is_refused_before_any_request`) |
| A failed signing wastes a key, or leaves a half-installed CA | D, T | Any failure leaves the pending request, so the next run signs the same key (tested `a_refused_signing_installs_nothing_and_a_rerun_signs_the_same_request`); nothing is installed until `import` has passed every check (above) |
| A dry run reaches the network or changes the host | T | `--dry-run` makes no call and writes nothing (`stages.rs::attestation_openbao`; tested `a_dry_run_asks_nobody_and_changes_nothing`) |
| Nobody can tell who had a CA signed, or from where | R | OpenBao's audit log records every login and signing (the OpenBao repository's configuration, **not in this tree**: §8). On the host the installer prints each step to the operator's terminal and keeps no log of its own, because no secret may land in one |
| The provider's user, compromised earlier, uses the run as a lever | E | The same handle-relative, no-follow discipline as TB-11 for every file the run touches; the ServiceAccount token is read by root, from a directory the provider's user cannot be tricked into pointing elsewhere because the path is fixed (`paths.rs::CREDENTIALS_DIR`) |

### TB-23: Build namespace → Azure Compute Gallery and upload host

The image publish Job (ADR-0162 Decisions 1 to 8), created by the Azure provider in the build namespace, runs
`banlieue provider azure image-publish` as `banlieue-azure-publisher`. It exchanges its projected token at Entra
ID for the publisher identity (A-30), spends that ARM token in the gallery's resource group, and sends the build's
bytes to the managed disk's upload host with a write SAS ARM hands it; in delete mode it removes one version its
owner tagged. It never calls the Kubernetes API. The provider reads the result back from Azure with its own
read-only identity and from the Job's termination message, and the controller boots what the provider's row
names. Implemented in `crates/banlieue-provider-azure/src/publish.rs` (the Job), `crates/banlieue-azure/src/{gallery,upload}.rs`
(ARM calls and the Put Page writer), `crates/banlieue-diskformat/src/vhd.rs` (the fixed VHD footer) and
`crates/banlieue-provider-azure/src/reconciler/{image,gallery,publish_job}.rs` (orchestration). **Not run against
Azure yet**: every control below is exercised by unit tests over fakes and a local HTTP server.

| Threat | STRIDE | Control |
| --- | --- | --- |
| ARM names an upload URL on another host, so the build and the SAS go there | S, I | The SAS is used only if it is HTTPS, has no userinfo and no explicit port, carries a signature, and its host is the cloud's managed-disk upload suffix or below it (`validate_upload_url`, `crates/banlieue-azure/src/upload.rs`); the suffix is fixed per cloud and chosen from the same pinned endpoint table as ARM's (`upload_host_suffix`, `crates/banlieue-provider-azure/src/cloud.rs`), so a `Provider` cannot widen it. Tests `the_upload_host_is_checked` (`upload_tests.rs`), `a_sas_on_another_host_is_refused_and_nothing_is_written`, `the_real_page_writer_refuses_the_fake_s_sas_on_the_wrong_cloud` (`publish_tests.rs`) |
| The ARM token reaches the upload host, or the SAS follows a redirect | I | The page writer has its own HTTP client: no `Authorization` header and no redirects (`SasPageWriter::new`, `upload.rs`; test `put_page_sends_the_range_headers_and_no_bearer_token`). ARM calls keep TB-4 Azure's controls (endpoint check, no redirects, capped replies) |
| The SAS or a token leaks into a log, an error, the termination message or status | I | `SasUrl`'s `Debug` and `Display` print nothing of it, logs name only its host, transport and service errors drop it (`upload.rs`; tests `sas_never_prints`, `a_transport_error_hides_the_sas`, `a_busy_blob_service_is_retryable_and_the_error_hides_the_sas`); the termination line is cut at any SAS signature and to 1024 bytes (`publish.rs::redact`; test `redaction_drops_a_signature_and_caps_the_length`). Status receives only the version ID, digest, minimum disk size and an error code and message (`publish_job.rs::parse_output`) |
| A corrupted or substituted build is published under the build's digest | T | The digest is the imagebuilder's own measurement of the built file, not a value copied from the source (ADR-0010, amendment 2026-11-26; TB-10 row above, `crates/banlieue-imagebuilder/src/reconciler/digest.rs`), so a `Url` build from an OCI image has one to publish by. The Job hashes the source and refuses a mismatch with `--checksum` **before any network call**, and the version name must be the one the digest maps to (`verify_source`, `publish`; tests `a_checksum_mismatch_makes_no_call_at_all`, `a_version_that_is_not_the_digest_s_name_is_refused_before_any_call`); the stream is hashed again as it is uploaded, and a change leaves the upload disk unused (`publish.rs::upload`). The PVC is mounted read-only (`publish_job.rs::build_job`). What Azure stored is not hashed back (§8) |
| The controller boots a version from an earlier build, or one banlieue did not publish | T | A row is ready only when the provider read the version from Azure as `Succeeded` with this `VMImage`'s owner tag and `banlieue-source-sha256` equal to the build's digest (`classify_version`, `reconciler/gallery.rs`; test `a_version_is_classified_by_state_owner_and_digest`), and a version recording another digest is never adopted (`a_version_recording_another_digest_is_never_adopted`). The controller boots a `Url` source only from that ready row and only when its `sourceDigest` equals the build checksum (`published_azure_version`, `crates/banlieue-controller/src/reconciler/infra.rs`) |
| A forged `status.perProvider` row steers VMs to another version | T, E | `vmimages/status` is written only by banlieue components (the providers, the imagebuilder, the controller), never by a tenant; the controller also requires an ARM resource ID, and ARM lets the runtime identity boot only versions it was granted read on (§7.36 c). A compromised banlieue component that writes `VMImage` status is not prevented: §8's compromised-provider rows |
| A forged termination message sets a row's minimum disk size or error | T | The provider takes only the minimum disk size from it, and only when its version ID matches the one the provider read itself (`image.rs`); the ID, digest and readiness come from its own Azure read. Writing a pod's termination message in the build namespace needs pod create there, which is already node-root-equivalent (TB-3) |
| A publish writes into, or a delete removes, something banlieue does not own | T, D | An image definition is published into only if it carries this `VMImage`'s tags; a version, upload disk or snapshot is adopted or deleted only if tagged with its owner UID; the definition is deleted only when it is ours and empty; delete mode refuses a version tagged for another owner (`ensure_definition`, `unpublish`, `delete_temporaries`, `publish.rs`; tests `a_foreign_definition_is_not_published_into`, `delete_refuses_a_version_owned_by_someone_else`). The provider asks for a delete only for versions it published (rows with a `sourceDigest`), never a `Template` source, and only when owned (`decide_superseded`; test `a_superseded_version_is_deleted_only_when_ours_and_unreferenced`). The gallery is never created or deleted: the publisher role has no gallery write (`deploy/provider-azure/publisher-role-definition.json`). Tags are writable by the gallery group's writers (§8) |
| A superseded or deleted `VMImage`'s version is deleted while a machine still boots it | D | A delete Job is created only when no `AzureVirtualMachine` the provider can list names that version, and a failed list deletes nothing (`referenced_versions`, `delete_unwanted`, `reconciler/image.rs`); the `VMImage` finalizer `banlieue.io/vmimage-azure` holds until its versions are gone. Machines outside the provider's served scope are not seen, which matters only if two scopes share a gallery (§7.36 j) |
| A name from a `VMImage` or `Provider` injects into an ARM URL | T | Definition and version names are checked before any call (`is_valid_image_definition_name`, `is_valid_version_name`, `crates/banlieue-azure/src/gallery.rs`; `target_from`, `publish.rs`; test `bad_names_and_providers_are_refused`); resource group and gallery pass the ARM client's `segment` check (TB-4 Azure) |
| A stolen publisher token or SAS, or an identity that can do more than publishing | E | Every credential lives in the Job's memory, the ARM token for its Entra lifetime and the SAS for at most an hour (`UPLOAD_ACCESS_SECONDS`), revoked by `endGetAccess` once the upload is done (`ensure_uploaded_disk`, `publish.rs`). The publisher role reaches one resource group and only definitions, versions, disks and snapshots there: no gallery, VM, network, role assignment or Key Vault action (`deploy/provider-azure/publisher-role-definition.json`). The runtime role holds no image write (A-29), so a leaked runtime token cannot publish. The Job cannot reach the API server: no token mounted, no RBAC (`deploy/imagebuilder/rbac/serviceaccount-azure-publisher.yaml`), no egress rule for it (`deploy/network-policies/azure-publish.yaml`) |
| The Job escalates on its node or tampers with the build | E, T | Non-root, `readOnlyRootFilesystem`, `allowPrivilegeEscalation: false`, all capabilities dropped, `RuntimeDefault` seccomp, PVC read-only, an `emptyDir` for `/tmp` (`publish_job.rs::build_job`; test `the_pod_is_restricted_and_holds_only_the_projected_token`), in a privileged namespace whose PSA would not stop a privileged pod, so the restriction is the manifest's (TB-3) |
| A wedged endpoint holds a Job, or the identity, for long | D | Per-write connect 10 s and 300 s timeouts with bounded retries (`upload.rs`), 30 min per ARM operation and 90 min for the version create (`OPERATION_DEADLINE`, `VERSION_DEADLINE`, `publish.rs`), a four-hour `activeDeadlineSeconds` and `backoffLimit: 1` on the Job (`publish_job.rs`); a later Job adopts what is still in flight by name (ADR-0162 Decision 3) |
| Nobody can tell who published or deleted an image | R | Azure's activity log records each call against the publisher identity; every resource carries the owner tags and the version its source digest (A-16); the Job logs each step without credentials |

## 7. Deployment hardening requirements

These are properties of the **current** design that operators must enforce
themselves. They are not bugs; they are the trust model, and deploying against
a different assumption is unsafe.

1. **Every infra machine CR is a credential-bearing resource — restrict `get`
   on all of them.** `banlieue-controller` resolves `spec.userData` references
   and inlines the *rendered content* into the infra CR in plaintext
   (ADR-0025/ADR-0038): `VSphereMachine.spec.userData`, and since ADR-0050
   `LibvirtMachine.spec.userData`, since ADR-0062
   `CloudHypervisorMachine.spec.userData`, since ADR-0075
   `ProxmoxMachine.spec.userData` and since ADR-0164
   `AzureVirtualMachine.spec.userData` (which Azure then also keeps as the VM's
   `customData`, §8). All are built by the same
   `build_*_machine` path in `crates/banlieue-controller/src/reconciler/infra.rs`.
   Anyone who can read one of these can read the user-data that produced it,
   SSH keys and join tokens included. This reflection is ADR-0025's accepted
   trade-off (§8), not a bug, but it makes `get vspheremachines`, `get
   libvirtmachines`, `get proxmoxmachines` **or `get azurevirtualmachines`** equivalent to reading every user-data Secret any VM in
   that namespace has referenced. **A new provider inherits this property the
   moment it gains a `userData` field; it is a contract-level consequence, not
   a per-provider one.** Since ADR-0129 the same field on an InfraMachine a
   Cluster API `Machine` owns holds that Machine's **bootstrap data**, a join
   token in practice: `get` on any of the four kinds in a namespace that holds
   CAPI clusters reads every such token there.
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
   The same holds for Cluster API: where CAPI clusters use banlieue machines,
   apply `deploy/admission/capi-bootstrap-data-authorization.yaml` (ADR-0129).
   Without it, anyone who may create a `Machine`, `MachineSet` or
   `MachineDeployment` can name another Machine's bootstrap Secret and read its
   join token from the InfraMachine. A namespace where you grant the controller
   `get` on Secrets for CAPI machines is, like `banlieue-system`, one where it
   may read any Secret by name: keep unrelated Secrets out of it.
3. **Do not co-locate unrelated Secrets in `banlieue-system`.** The controller's
   grant is namespace-wide, not `resourceNames`-scoped; admission bounds who can
   *trigger* a read, not what the controller identity could read if compromised.
4. **Treat `banlieue-imagebuild` RoleBindings as node-root grants** (§TB-3), and
   do not grant pod-create there to anyone who should not hold every Provider's
   hypervisor credentials, or, on Azure, the publisher identity of every gallery
   (A-30). Each vSphere, libvirt and Azure `Provider` gets such a grant, its
   Job `Role` (`crates/banlieue-operator/src/workload.rs::build_jobs_role`):
   apply `deploy/admission/provider-jobs.yaml` (with `deploy/admission/`),
   which keeps those Jobs restricted where Pod Security does not.
5. **`Provider` and `ProviderClass` creation is platform-admin-only.** The VAPs
   bound the damage; they do not make these safe to delegate.
6. **Install the claim subject policy, and audit its ConfigMap.**
   `deploy/admission/virtualmachineclaim-subject-authorization.yaml` is what
   makes `spec.subject` an attribution the API server vouched for rather than
   free text (ADR-0047 Decision 10). It binds `subject.id` to the
   authenticated username, confines `subject.issuer` to an allowlist, and
   makes `spec` immutable so the check cannot be undone by a later patch
   (including `poolRef` and `poolSelector`, ADR-0132; without the policy a
   claim's pool fields can be edited, to no effect, since the pick reads them
   once).

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
     `github:` prefix, so the prefixed form would leave the in-guest agent a
     value it cannot compare (ADR-0047 Decision 9, amended; ADR-0049) — and
     the policy re-applies the prefix at comparison time instead. A mismatch
     fails safe but silently: see §6/TB-7. Re-check it whenever the cluster's
     authentication configuration changes, not only at install.
   - **One username prefix per issuer.** When the API server trusts more
     than one issuer, each `jwt` entry needs its own non-empty prefix, none a
     prefix of another (§6/TB-7). Give the claim subject policy the same
     pairs in `issuerPrefixes` (`<issuer> <prefix>` per line) and list each
     issuer in `issuers`. An issuer without a line falls back to
     `usernamePrefix`, so its users' claims are refused unless that is its
     prefix: it fails closed.

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
   **`virtualmachine-created-by.yaml` is the one exception to both of those
   patterns** (ADR-0089): it is a `MutatingAdmissionPolicy`, needs Kubernetes
   1.36+ (higher than every `ValidatingAdmissionPolicy` floor above), and is
   `failurePolicy: Ignore` on purpose — it stamps an audit-convenience
   annotation, not a security check, so its own failure must never block a
   `VirtualMachine` create. Skipping it has no security consequence (see the
   TB-1 row above); it only means `VSphereMachine`'s `CreatedBy` vCenter
   custom attribute is never populated.
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

   **First-boot sealing is the one exception (ADR-0088, Accepted
   2026-10-26), and it is on by default.** An `Immediate` image may declare
   `firstBootSealing`, so each clone seals `COS_PERSISTENT` to its own vTPM
   on its first boot; `--enable-first-boot-sealing=false` restores ADR-0048
   for every such image. Declare it only on images built from Kairos v4.4.0
   or later, which carries first-boot sealing and its fixes
   (kairos-io/kairos#4924; #5150, #5160 and #5194, closing #5149, #5158 and
   #5193). Keep the operator cloud-config of such images free of
   `install.encrypted_partitions`, and consume such VMs on `GuestReady`, not
   `Ready` (§8). On Cloud Hypervisor (ADR-0112) the image must also send its
   partitions as a `sealed=` line on the vsock report; banlieue's sealing
   layer does, and an `Asserted` image that does not times out rather than
   being reported healthy.

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

    Three consequences to keep in mind when granting:
    - `create virtualmachineclaims` no longer lets the holder record an
      arbitrary `spec.subject`: Decision 10's policy
      (`deploy/admission/virtualmachineclaim-subject-authorization.yaml`)
      requires `subject.id` to be the caller's own username, except for a
      declared broker. A **broker** can still name anyone, so the binding
      record (A-7) is only as trustworthy as the `brokers` list and the
      `create` grant together. The policy must be installed; without it the
      original exposure returns.
    - `delete virtualmachineclaims` destroys running VMs.
    - `spec.subject` is world-readable in the namespace and is copied onto the
      member as an annotation. Put an identifier there, never a token: the
      subject's credential belongs on the consumer's attested channel,
      anchored on the member's EK certificate (ADR-0049).
11. **Cloud Hypervisor hosts are part of the trust base; prepare them with
    `banlieue host cloud-hypervisor install` and keep them to it.** The provider on the host is only as
    contained as the host configuration around it
    (`docs/src/guides/cloud-hypervisor-host-systemd.md`):
    - Install with `banlieue host cloud-hypervisor install` from a banlieue binary you
      verified (A-5: its signature and SBOM), since it is what places A-14
      (ADR-0067; the script is now an SSH wrapper around it), which
      registers per-guest uids and private groups, sets `0711`/`2770`
      directories, and installs the polkit rule and the sandboxed provider
      unit. A host set up by hand without the guest account records (userdb, or
      files-mode accounts) cannot start
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
    - Keep `nsswitch.conf`'s `passwd`/`group` lines including `systemd`
      for `--guest-accounts userdb`, or `files` or `compat` for `files`
      (ADR-0139); `preflight` refuses a host without the source the chosen
      mode needs, and the installer never edits the file.
    - Upgrade the provider by re-running `install` with
      `--provider-binary`, not by copying the binary: only `install`
      rewrites the unit, and it restarts the provider when the unit or
      binary changed. Do not add `banlieue` to the `kvm` group, and do not
      add `ProtectKernelTunables=` or `ProtectControlGroups=` to its unit in
      a drop-in: on systemd 257 either re-mounts `/dev` and `/sys` writable.
    - **Prefer the pinned VMM release.** For another one (ADR-0084), get
      each file's sha256 from the upstream release page on a machine that
      can reach GitHub, and pass it with `--vmm-sha256` /
      `--ch-remote-sha256` / `--firmware-sha256`, rather than trusting a
      mirror or the host's own view of `api.github.com`. Try a new major
      version on one host first.
12. **Recommended audit rule:** alert on any `ClusterRoleBinding` created by the
   `banlieue-operator` identity whose `roleRef` is not `banlieue-provider-*`
   (accepted-risk monitoring for the operator's RBAC-minting capability).
13. **Treat the OCI registry used for Cloud Hypervisor images as holding
    guest bootstrap material** (ADR-0064, TB-10). Images can carry
    cloud-config baked in by `VMImage.spec.cloudConfigs`. Use a private
    repository with access control and TLS (if its certificate is signed by a
    private CA, name that CA to the imagebuilder with
    `--registry-ca-bundle-*`, ADR-0110, and name no more than the one CA);
    give the imagebuilder a push
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
16. **Prepare a Proxmox node with `scripts/bootstrap-proxmox-host.sh`, and
    keep the token privilege-separated.** The script creates the
    `BanlieueProvider` role with exactly the privileges ADR-0074 Decision 4
    lists, a user and a token whose own ACLs are the only permissions it holds
    (privilege separation), and grants them on `/vms`, the node, the SDN zone
    and the image storages, plus a `BanlieueSeed` role on the dedicated
    `banlieue-seed` storage alone. Do not point the `seed-iso` class at a
    shared storage, and do not widen the ACLs to `/` or add
    `VM.Console`, guest-exec or `Permissions.Modify` "to make it work": with
    them, a leaked token stops being "can create and destroy VMs on this node"
    and becomes a way into guests. The token's secret cannot be read back, so
    the Secret in `banlieue-system` is the only copy — treat `get secret` on it
    as `A-1` (§7.2, §7.3). Supply the node's own CA as `caBundle` rather than
    setting `insecureSkipTLSVerify`, and re-issue the certificate when the
    names clients connect by change. Leave the `banlieue-machine-uid=` line in
    a banlieue VM's description alone (§8).
17. **Give each real network segment its own `networkClass`.** The
    duplicate-address check (ADR-0083) keys on `(networkClass, address)`,
    because `networkClass` is the finest network identity the controller has.
    Two `networkClass`es mapped to one segment let two VMs collide
    undetected. One `networkClass` mapped to isolated segments on different
    `Provider`s makes a legitimate reuse of a subnet read as a collision. The
    mapping lives in `Provider.spec.capabilities.networkClasses` and is a
    platform-admin decision (§7.5).

18. **Protect a kine datastore as the cluster itself (A-17, ADR-0085).**
    - Listen only on the address the controllers use, and allow only the
      k0s role from the controllers' addresses: `hostssl` with
      `scram-sha-256`, never `trust` or `md5`. Narrow the rule to the node
      addresses once they are reserved.
    - Require an authenticated TLS session: `sslmode=verify-full` with a CA
      you control. `sslmode=require` encrypts but accepts any server
      certificate. Set `ssl_min_protocol_version = TLSv1.3`.
    - Back the database up, encrypt the backups, and test a restore: they
      are the only copy of the cluster's state, Secrets included.
    - Consider API-server encryption at rest, so a copy of the database or
      a backup does not disclose Secrets in plaintext.
    - Reserve each node's address in DHCP (`VM_MACS` fixes the MACs): a
      node that changes address breaks its k0s install and the API
      server's certificate names.
19. **Treat the control plane VIP as an address you own (TB-13, ADR-0086).**
    - Reserve `API_VIP` outside every DHCP range and MetalLB pool. k0s does
      no address management, and a collision is silent.
    - Give each cluster on a LAN its own `API_VIP_ROUTER_ID`, and keep the
      generated `api-vip-authpass` file with the cluster's other secrets.
    - Keep kubeconfigs verifying the cluster CA. Never set
      `insecure-skip-tls-verify`: certificate verification is what stops a
      host that takes the VIP from impersonating the API.
    - Approve only the VIP's `/32` route on the tailnet, and restrict who may
      reach it with tailnet ACLs.
    - When converting a cluster built before ADR-0086, move
      `/var/lib/k0s/manifests/konnectivity` off every controller and delete
      the objects labelled `k0s.k0sproject.io/stack=konnectivity` (agent
      DaemonSet, its ServiceAccount, the `system:konnectivity-server`
      ClusterRoleBinding): k0s prunes none of them, and the binding keeps
      granting `system:auth-delegator` to an identity nothing uses.
    - Replace a default allow-all tailnet policy with grants by group and
      tag before relying on the tailnet as an access boundary.
20. **Negotiate post-quantum key exchange on every link that carries cluster
    secrets, and an issuer on the cluster is a cluster secret.** Recorded
    traffic and stored backups can be decrypted later; signatures cannot be
    forged retroactively, so key exchange comes first.
    - Verify each endpoint by offering only the hybrid group:
      `openssl s_client -connect <host>:<port> -groups X25519MLKEM768`
      (OpenSSL 3.5 or later; add `-starttls postgres` for the datastore). A
      completed handshake proves the server negotiates it.
    - Datastore (TB-12): PostgreSQL 18 or later, `ssl_groups` including
      `X25519MLKEM768`. PostgreSQL 17 cannot.
    - Backups of the datastore (A-17): encrypt to a hybrid ML-KEM recipient
      (age 1.3 or later `mlkem768x25519`, or GnuPG 2.5 or later composite
      keys), not a classical OpenPGP key.
    - An issuer on the management cluster (TB-7): restrict read on its
      storage API group to `cluster-admin`, give it its own `restricted`
      namespace, and shorten its ID-token lifetime (15 minutes with refresh
      tokens, not Dex's 24 hour default).
    - Observed on a reference deployment (2026-10-03): kube-apiserver,
      kubelet, the SPIRE server, an issuer behind a Go TLS proxy, and
      OpenSSH 10 all negotiate ML-KEM hybrids by default; PostgreSQL 17,
      libvirtd's GnuTLS build, WireGuard and (until ADR-0170) banlieue's own
      clients do not. On 2026-10-04 its datastore moved to PostgreSQL 18 with
      `ssl_groups = 'X25519MLKEM768'` (no classical fallback), TLS 1.3 only
      and `verify-full` against a private CA; kine (Go) and SPIRE reconnected
      under that offer, which shows Go clients negotiate the hybrid.
    - banlieue's own clients (ADR-0170, 2026-11-26) offer X25519MLKEM768
      first through `crates/banlieue-tls`, and negotiate it with any peer that
      accepts it: measured with `crates/banlieue-tls/tests/live_pq_probe.rs`
      (`PQ_PROBE_HOST=<host> cargo test -p banlieue-tls --test live_pq_probe
      -- --ignored --nocapture`), X25519MLKEM768 with Cloudflare's and
      Google's public endpoints, `secp384r1` with Azure Resource Manager and
      the Entra ID token endpoint. Run the probe against your own vCenter,
      Proxmox and API endpoints to see which links are post-quantum.
    - Upgrading the datastore with Debian's `pg_upgradecluster`: it does not
      copy `conf.d/`, so a `listen_addresses` kept there is lost and every
      controller loses the API until it is restored. Copy `conf.d/` first.

21. **Apply the NetworkPolicies, and narrow them (TB-4, TB-14, ADR-0094).**
    `deploy/network-policies/` is opt-in, so nothing is restricted until you
    apply it to every namespace running banlieue pods, a `ProviderClass`
    `workloadNamespace` included. Then replace each "any address" API-server
    and backend rule with your cluster's API CIDR and your hypervisors'
    CIDRs; until you do, egress is restricted by port only, and on vSphere
    port 443 stays open everywhere. Label only your monitoring namespace
    `banlieue.io/monitoring=true`. If you enable trace export, add an egress
    rule to your collector and nothing wider. Cloud Hypervisor providers run
    on the host; use its firewall. The Azure provider's 443 rule cannot be
    narrowed by address (ARM and Entra ID are anycast front ends published
    only as service tags); pin it with an egress proxy or a CNI that matches
    FQDNs if you have one (§7.36). The same holds for the Azure publish Job's
    443 rule (`azure-publish.yaml`), which also reaches the managed-disk upload
    host: apply it in the build namespace.

22. **Point trace export only at a collector you operate (A-19, ADR-0092).**
    Spans name objects, namespaces and backend ids. Leave
    `OTEL_EXPORTER_OTLP_ENDPOINT` unset unless you want them, and use
    `https://` to a collector outside the pod's node.

23. **Grow a running management cluster with `join`, and keep management
    nodes on Cloud Hypervisor apart from banlieue's guests (TB-9, TB-16,
    ADR-0099).**
    - Add workers and controllers to a cluster you have changed since
      bootstrap with `join`, never `config` + `apply`, which push the
      generated configuration back onto every controller. When swapping
      nodes, add the new controller before retiring the old one.
    - Treat a controller `join` as a cluster-admin action: its token carries
      the cluster CA key. Admit the new controller's address to the
      datastore yourself (on PostgreSQL, one `hostssl` line for that
      address), and remove the retired controller's line when it goes.
    - Run `join` from the hypervisor host or another machine on the nodes'
      network, not across an untrusted segment: the script does not check
      SSH host keys (§8).
    - Keep `CH_VM_USER` a dedicated account: not `banlieue`, not a banlieue
      guest uid, in no group but its own and `kvm`.
    - Cloud Hypervisor is the default VMM, and it needs passwordless sudo on
      the host. Set `DEFAULT_VMM=libvirt` in the env file of a cluster built
      on libvirt; the script refuses to run against it otherwise.
    - Leave `CH_VM_DIR` on a filesystem banlieue's storage classes do not
      share a directory with, and keep its parents searchable but not
      writable by `CH_VM_USER`.
    - Reserve each CH node's MAC in DHCP, as for libvirt nodes: the script
      finds a node by its MAC, and a k0s node must not change address.
24. **Build against the `banlieue-api` release that matches the installed
    CRDs (A-21, ADR-0100).** The crate's version is the release's version;
    a consumer pinned to it serializes exactly the schema the API server
    enforces. Commit `Cargo.lock`, so a new upload reaches a build only
    when someone updates it, and enable one k8s-openapi version feature in
    the final binary. The crate grants a consumer nothing: give its
    identity only the RBAC its calls need.
25. **Install banlieue by digest (A-5, ADR-0103).** Apply a release's
    `install.yaml` (already pinned), or pass `--image-digest` to
    `banlieue bootstrap operator`, taking the digest from the release you
    verified (`crane digest ghcr.io/firestoned/banlieue:<tag>`, checked
    against its cosign signature). A GitOps repository rendered with
    `--dry-run` keeps the digest, so an upgrade is a reviewed change of
    digest and every Deployment rolls. A mirror (`--registry`) is pinned to
    the same digest, which the mirror must serve unchanged.

26. **Budget the VMs on a shared host (ADR-0105, ADR-0106).** Declare the
    guests' budget to `install` (`--guest-cpus`, `--guest-cpu-weight`,
    `--guest-memory-max`, `--guest-tasks-max`); change it by re-running
    `install`, never with `set-property` on `banlieue-guests.slice`, whose
    drop-in would silently override the declared value. Where management
    nodes share the host, bound every VM together on the parent:
    `systemctl set-property banlieue.slice CPUWeight=50 MemoryMax=<host RAM
    less what the host needs>`. Size each `MemoryMax` from the VMs under it:
    a cap below their sum makes the kernel kill a VMM.

27. **One backend host is one `Provider` (ADR-0060, ADR-0107).** Capacity is
    counted per `Provider`: two `Provider`s pointing at the same Cloud
    Hypervisor host would each count only their own VMs and together
    over-place it. On a host shared with other workloads or management nodes,
    declare a guests' budget (§7.26), so the published capacity is what guests
    may really have. Set `spec.scheduling.cpuOvercommitRatio` to `1` where
    vCPUs must map to real CPUs. Give each store (filesystem, libvirt pool,
    datastore) **one** storage class: two classes on one store are checked
    separately and can together fill it (ADR-0128). Keep
    `spec.scheduling.storageReservePercent` above `0` on any store guests
    write to: it is the headroom their thin disks grow into.

28. **A VM with two adapters is a bridge until the guest says otherwise (ADR-0111).**
    A `VMClass` that declares an isolated segment beside a routable one gives
    its VMs a path between them. Give such a class only to the VMs that are
    meant to sit on both (a proxy or gateway, ADR-0109), keep IP forwarding off
    in the guest's own configuration (`net.ipv4.ip_forward = 0`, and the IPv6
    equivalent) and test that, and give an isolated segment its own
    `networkClass` (§7.17) so no other class reaches it. banlieue creates the
    NICs; it does not and cannot check what the guest does with them. Rebuild
    an image after the in-guest `configure-network` changes, or its NICs beyond
    the first come up unconfigured.
29. **A sandbox segment routes nowhere (ADR-0109).** On libvirt define it as
    an isolated network, with no `<forward>`, `<dhcp>` or `<dns>`, and give
    the host an address on it only if something on the host must be reached
    from the segment. On vSphere use a port group on a VLAN with no SVI,
    trunked on every host a member can land on, and list a zone only after a
    two-host test passes. Keep the pool's address range clear of the
    gateways' `sandbox` addresses. Check it with the guide's table
    (`docs/src/guides/isolated-sandbox-segment.md`).
30. **Choose the served namespaces with a label only a platform admin can
    set (ADR-0130).** Run `banlieue controller` with
    `--namespace-selector` set to a label such as `banlieue.io/tenant=true`,
    and do not grant `update` or `patch` on `namespaces` to tenants or to
    namespace-scoped admins: that grant is what lets a namespace opt itself in.
    Give the operator the same label with `--watch-namespace-selector` (ADR-0131) and keep the two aligned. Without the flag each serves only its own namespace and logs a warning saying so. A negated selector such as `!foo` serves every namespace without that label, system namespaces included; the controller warns when the selector serves any `kube-*` namespace, but does not refuse. The
    selector is *scope*, not isolation: give each tenant a namespaced `Role`
    (never a `ClusterRole`), and until the follow-up admission and quota
    policies exist treat a tenant as able to name any cluster-scoped `VMClass`,
    `VMImage` and `Provider` selector (§8).

31. **Serve tenants from one shared provider, and lock both doors (ADR-0133).**
    Keep the vCenter credentials in the platform namespace: give the shared
    `Provider` a `spec.namespaceSelector`, set the controller's
    `BANLIEUE_PROVIDER_NAMESPACE`, and never place a provider, its credentials
    Secret or a `Provider` in a tenant namespace. Then (a) grant tenants **no
    verb on `vspheremachines`**, the machines behind their pools: they need to
    see pools, VMs and claims, and `get` on a machine reads its rendered
    `userData` (item 1); (b) apply **both** policies in `deploy/admission/`
    (`machine-provider-ref.yaml`,
    `provider-namespace-selector-authorization.yaml`) and leave them on
    `Deny`; (c) bind `banlieue-provider-namespaceselector-admin` to platform
    admins only, and do not aggregate it into `admin`/`edit`. The first policy
    pins the controller's identity as
    `system:serviceaccount:banlieue-system:banlieue-controller`; if the
    controller runs elsewhere, edit it, or the controller is denied its own
    placements (it fails closed, never open). Cluster API's manager is
    deliberately not exempt. A Cloud Hypervisor host reads its `namespaceSelector` from
    its own `Provider` **once, at startup**: changing it needs a provider restart
    (ADR-0134).

32. **Prepare a sandbox host's segment, and prove it (ADR-0144, TB-19,
    TB-20).** The binary makes no bridge and installs no rules, so a host that
    serves sandboxes is safe only after you have: created the sandbox bridge with
    **no host address** and no uplink, and the uplink bridge; installed the host
    rules and unit from `docs/src/guides/cloud-hypervisor-sandbox-segment.md`
    (and added the other tenant's firewall and container units to the unit's
    `After=`, so a reload of theirs is followed by `systemctl reload` of ours);
    declared the sandbox class `isolate_ports` and a **separate, non-isolating
    class on the same bridge** for the gateways' sandbox-side NICs, and given
    only the gateway `VMClass` that class (§7.28); carved each host's sandbox
    range from one fleet-wide supernet, so no two hosts share an address; and
    run the guide's verification from two test sandboxes, reading the counters.
    `preflight` checks the kernel can isolate, that a class names a bridge, and
    that the three `BANLIEUE_SBX_*` chains exist with a jump to each first in
    its built-in chain (it fails closed when `iptables` cannot be read); it does
    **not** read what is inside those chains, nor the forwarding state or the
    IPv6 sysctl, so do not take a green `preflight` for a safe segment. After editing the
    host rules or another tenant's firewall, re-run the verification. Remove the
    bring-up masquerade (Decision 8) before the first real sandbox, and confirm
    with the command in the guide that it prints nothing.

33. **Opt in to x509pop attestation deliberately, assert the CA yourself, and keep its key to the provider
    (ADR-0146, TB-21, A-24 to A-26).** The default is no opt-in: a host that does not give
    `--attestation-ca-name` (or `--attestation`) has no CA, no signer and no `[attestation]` section, and its
    vTPM path is unchanged. Opt in on a **shared** host whose TPM tooling you cannot vouch for; a host you
    fully control gains little. Then: (a) copy the single `ca.crt` the installer prints into
    `Provider.spec.attestation.hostCa` and into the identity layer's `x509pop` bundle, and make the CA's
    common name **unique across that bundle**, which banlieue cannot check (§8); (b) treat write access to the
    `Provider`, to any ConfigMap or Secret its `hostCa` names, and to the bundle as the root of trust for every
    guest of that host, as §7.14 does for the EK bundle, and alert on changes; prefer an **inline** `hostCa`,
    the only form the provider checks the shape of (a `configMapRef` or `secretRef` counts as set and is not
    read); (c) keep `/var/lib/banlieue/attestation-ca` at `0700` and `ca.key` at `0600`, owned by the provider's
    user, and out of any backup or copy that leaves the host: `status` reports the CA and `preflight` refuses a
    wrong mode or owner, and the provider will not start on one; (d) rotate with `--rotate-attestation-ca` and a
    **new** name, put the new certificate in `hostCa` and the bundle, keep the old one in the bundle until the
    certificates it signed have expired (the TTL, default 1 hour), restart the provider unit so the signer loads
    the new CA, then remove the old one; (e) if the key may have been read, **remove that CA from the
    bundle and from `hostCa` first**, which is the only revocation, then rotate; (f) keep the TTL as short as the
    sandbox's life allows (`--attestation-certificate-ttl`, 5 minutes to 7 days); (g) expect an opted-in host to
    be `Ready=False` until `hostCa` is asserted, on purpose, and do not read a green `Ready` as proof the
    asserted certificate is this host's. **The guest helper is not built**, so do not plan sandbox identity on
    this path until it is and a live run has attested against the identity layer (§8).

34. **Have the CA signed by a PKI only as narrowly as the PKI can enforce, and delete the login role after the
    import (ADR-0146 Decisions 7 and 8, TB-22, A-27, A-28).** The default is a self-signed CA and no PKI call. If you use
    OpenBao: (a) sign on a **dedicated mount** whose root key is generated inside OpenBao, **never the root whose
    CAs sign SVIDs**, because the `x509pop` bundle trusts everything that chains to it; (b) give each host its own
    policy on `root/sign-intermediate` that pins `common_name` to that host's CA name, `max_path_length` to `0`, `ttl`
    and `use_csr_values` to `false` and requires `csr`, and its own JWT role bound to that host's ServiceAccount with
    `token_ttl` of five minutes at most, `token_num_uses` of 1 and `token_bound_cidrs` set to the host's address;
    **verify each against your running OpenBao before relying on it**, because this repository cannot; (c) refuse a
    name already issued on the mount (the issuer CN is in every agent ID, so two hosts with one name lose the
    confinement); (d) **delete the role and policy as soon as the import succeeds**, since a JWT role cannot count
    logins; (e) keep OpenBao's audit log on and alert on a `sign-intermediate` call outside an onboarding; (f)
    give the installer a token **file** owned by root, never argv or the environment, for a host with no cluster
    identity; (g) set the mount's `max-lease-ttl` to the longest CA you accept (the installer caps an import at
    ten years); (h) renew by rotation under a new name before `notAfter` (`status` shows the days left,
    `HostCaExpired` stops new guests). The controller and the provider must not be given a PKI credential to
    automate this: that would be one credential able to mint a CA for every host.
35. **Keep the controller's liveness probe, and treat its restarts as a signal (ADR-0145).**
    The shipped Deployment probes `/livez` every 30 s with a failure threshold of 3
    (`deploy/controller/deployment.yaml`); keep it, because the audit's only recovery is the
    kubelet's restart, and a deployment with no liveness probe gets the gauge and the log but no
    restart. Alert on `banlieue_audit_unreconciled_objects > 0`. A controller restarting about every
    ten minutes with the audit's `ERROR` line in its log is reporting an API server whose watch has
    stopped delivering: repair that server, because the restart only re-rolls which server the
    controller lands on.

36. **Give each Azure `Provider` a resource group of its own, and guard everything that can act as its identity
    (ADR-0158, ADR-0159, ADR-0164; A-29, TB-4).** (a) One resource group per `Provider` per management cluster,
    holding nothing banlieue did not create: the sweep trusts the `banlieue-provider` tag (`<namespace>.<name>`),
    which two clusters with a same-named `Provider` in one group would share, and deletes what it reads as this
    `Provider`'s orphans. (b) Keep write and tag-write on that group to people you would trust with banlieue's VMs;
    a tag is not a signature (§8). (c) Create the custom role from `deploy/provider-azure/role-definition.json`,
    assign it at that group only, and grant subnet join and gallery read separately and narrowly
    (`deploy/provider-azure/README.md`); never Contributor or Owner. (d) Publish the cluster's ServiceAccount
    issuer (discovery document and JWKS) where Entra ID can fetch it, from a location only the cluster admin can
    write (ADR-0159 Decision 2): whoever can change that JWKS, or holds the signing key, can mint tokens Entra
    accepts. (e) Make each federated credential's subject the exact ServiceAccount the operator created
    (`system:serviceaccount:<namespace>:<name>`, read from the cluster, not derived) and its audience
    `api://AzureADTokenExchange`. (f) Withhold pod create, Deployment write and `serviceaccounts/token` in the
    provider's namespace from anyone who should not act as the Azure identity: each can obtain its token. (g) Do
    not put secrets in user data for an Azure VM: Azure keeps `customData` with the VM (§8). (h) Network security
    groups, routes and firewalls on the subnet are yours: banlieue requests no public IP and opens nothing, and
    controls nothing else on the network. (i) For image publishing (ADR-0162), keep the gallery in a resource
    group of its own, not the `Provider`'s: the publisher role creates and deletes disks there. Create the role
    from `deploy/provider-azure/publisher-role-definition.json`, assign it to the publisher identity at that
    group only, and grant the runtime identity only version read on the gallery (`deploy/provider-azure/README.md`).
    (j) Give each `Provider`, or each set of `Provider`s that serve the same namespaces, a gallery of its own:
    version names follow the build's digest, so two `Provider`s sharing a gallery share versions, and one deletes a
    superseded version once no machine **it can see** boots it. (k) Every publisher identity whose federated
    credential trusts `system:serviceaccount:<build namespace>:banlieue-azure-publisher` is reachable by every
    azure `Provider`'s Job and by anyone who can create a pod in the build namespace (§4, §8): federate only
    publisher identities you would let every `Provider` writer use. (l) Set `BANLIEUE_AZURE_GUEST_IMAGE` to the
    `banlieue-azure-guest` image **by digest** (the release's `azure-guest-image.txt`; check it with
    `cosign verify`), from a registry only release automation writes (ADR-0171): it runs as root in every Azure
    image. Keep `168.63.129.16` reachable for root only in a guest's host firewall, and do
    not let a base image ship a second Azure agent.

## 8. Accepted risks

| Risk | Why accepted | Revisit when |
| --- | --- | --- |
| **A principal granted `reference` on a Secret can have banlieue present that credential to an endpoint it chooses** (ADR-0157 Decision 3): `reference` reads nothing, but with Provider write it decides where the credential goes, which for a password or token is disclosure. Nobody holds it by default | It lets a controller that groups Providers (faubourg) name a credential without being able to read it, which is strictly less than the `get` it would otherwise need; the grant is documented beside the policy (`deploy/admission/README.md`) to be made by exact `resourceNames`, or to a grantee whose own admission bounds what it names (faubourg's `faubourg-provider-connection`: `faubourg-` Secrets and ConfigMaps only, never `insecureSkipTLSVerify`) | Kubernetes RBAC can bound a verb by name prefix or label, or a grantee is found holding `reference` without its own bound |
| **The controller's liveness now depends on what the API server delivers** (ADR-0145; TB-1, TB-14): while an API server's watch cache stays blind, the controller is restarted about every nine to ten minutes until its streams land on a healthy server, and a VM created in a blind window waits for that restart. Objects created faster than the controller is invoked on them for about seven minutes would restart it too, and banlieue does not limit object count (the per-tenant limit row below) | The restart is the only recovery banlieue has when its cache stops delivering; before ADR-0145 the fault lasted until a person noticed. The audit cannot be tripped by objects the controller is invoked for, even if every reconcile fails, and excludes namespaces it does not serve | A watch that reconnects to another server after silence lands (ADR-0145, Open), per-tenant quota lands (the ADR-0130 row), or another controller opts in to the audit |
| **The signer still trusts the host it runs on** (ADR-0146; TB-21): it measures nothing about the guest. A compromised provider process or root on the host mints a certificate for any subject under this host's CA name, including subjects of guests that do not exist | The anchor is custody and reach, not measurement: the key is made by banlieue and readable only by the provider's user and root (A-24), and its name is in every agent ID it signs, so the reach is this host's guests, by mediatore's design, and no other host's. A host compromise already reads every guest's disk and memory (A-12, §4, §9), so identity forgery adds a capability to an adversary who already holds the guests themselves | A guest can prove its own measurement to the signer (a TPM quote or similar) before it is signed for, or the identity layer can bind a host CA to measured host state |
| **A CA name that is not unique across the identity layer's bundle lets one host's CA be confused with another's** (A-26; TB-21): the issuer common name is the confinement, and banlieue sees one host's CA only | `validate_name` refuses a name outside `[A-Za-z0-9._-]` or over 64 bytes and never replaces it, rotation refuses the current name, and the installer prints the name and tells the administrator it must be unique (`crates/banlieue-host/src/attestation.rs::admin_action`). banlieue never sees the bundle or another host's CA, so it cannot check; mediatore ADR-0050 is to move uniqueness to the issuer | mediatore ADR-0050 lands, or banlieue gains a place that sees more than one host's asserted CA (for example a check across `Provider`s of the same class) |
| **A stolen CA key mints certificates for that host's subjects until the CA is removed from the bundle**, and the signer is the provider process, so a compromised provider holds the key (A-24; TB-21) | There is no revocation list: removal from `hostCa` and the bundle is the revocation, and certificates are short lived (default 1 hour), so a removed CA stops mattering within one lifetime. The key is `0600` in a `0700` directory, owned by the provider's user, never printed, never in a Secret or CR, and a second process holding it beside the provider would add nothing (ADR-0143 Decision 4) | The key can be sealed to the host (a TPM-held or HSM-held signing key), or the provider's process is split from the signer |
| **Rotating the CA is an operational procedure with an overlap window** (ADR-0146 Decision 5; §7.33): both CAs are trusted between asserting the new one and removing the old, a guest is signed by whichever CA the provider has loaded, and `--rotate-attestation-ca` deletes the old key from disk while the running provider keeps signing under the CA it loaded until it is restarted, after which it signs under the new name, which the identity layer does not trust until its bundle has it (so assert the new CA before restarting the provider) | A name is never reused for a different key, so the old and new CAs cannot be confused; the overlap is the price of not dropping live guests, and the guide states the order. Automating it needs a guest-side renewal story that does not exist yet | The guest helper exists and rotation is seen to drop a guest, or rotation is automated |
| **The certificate lifetime and the signer's rate limit are defaults, not tuned to the sandbox's life** (ADR-0146; ADR-0143 Decision 5): the TTL is 1 hour (5 minutes to 7 days, by `--attestation-certificate-ttl`), the limit is 6 requests per 60 s and 8 connections per guest and is **not configurable**, and a **certificate cannot be revoked**: one copied out of a guest that is then deleted verifies on the identity layer until it expires | A copy needs the guest's private key as well, which never leaves the guest by design; the lifetime bounds the window; and the subject is the machine's UID, which a `Recreate` never reuses | The sandbox's real lifetime and renewal behaviour are measured with the helper, or the TTL is needed shorter than the 5-minute floor, or an abuse case shows a limit that is wrong |
| **No guest-side helper exists, so nothing attests on this path and the signer has been exercised by tests only** (ADR-0146 Decision 6; TB-21): there is no live run of a guest asking, of SPIRE verifying the result, or of renewal | The host contract (vsock port 1025, one PEM CSR, one PEM certificate) is fixed and tested against OpenSSL and a fake host; the helper is mediatore's (ADR-0049) and the sandbox image build's. Until it ships, an opted-in host's only effect is a signer nobody calls and a `Ready=False` until `hostCa` is asserted | mediatore ships the helper and a live run attests a guest against the identity layer, with the controls above in place |
| **A guest that stalls the signer is bounded but not blocked from retrying, and the limiter and counters are in the provider's memory** (TB-21): a guest can hold its own 8 slots for the 5 s read deadline and spend its own 6 requests per minute on garbage, which delays its own renewal for up to a window; there is no host-wide cap on aggregate signing; a provider restart clears the limiter and every counter | Every limit is per guest: its own socket, its own subject, its own slots, so it cannot starve another guest's signer. Signing one request is a single P-256 verify and sign; the aggregate is bounded by guests on the host times the per-guest limit | A host serves enough guests that signing shows in the provider's latency, or the counters show a guest abusing it |
| **The `hostCa` gate is partial** (ADR-0146 Decision 4; TB-21): a `configMapRef` or `secretRef` source counts as set and is never read or checked (the shared provider role grants no ConfigMap or Secret read, `deploy/provider-cloud-hypervisor/rbac/clusterrole-shared.yaml`); an inline source is judged for shape only (`ProviderAttestation::validate`: exactly one certificate, no private key), not for being this host's `ca.crt`, and that shape check is the provider's: the admission policy `banlieue-provider-attestation-hostca` (`deploy/admission/provider-attestation-hostca.yaml`) enforces only that exactly one of `inline`, `configMapRef` and `secretRef` is set, and its CEL has been reviewed but not executed in this tree (an empty `attestation: {}` is likewise caught only by `ProviderAttestation::validate`); a guest already `Ready=True` is **not** held when `hostCa` is withdrawn later, and a machine that is not yet `Ready=True`, a guest mid-boot included, is held and not reconciled until the assertion returns; the hold is re-evaluated on the default requeue, not when the `Provider` changes | banlieue never verifies the anchor (ADR-0143 Decision 2, ADR-0049 Decision 10), and the provider must not gain a Secret read to check a certificate it would not act on. The hold exists to fail early and visibly, not to prove correctness; withdrawing the assertion stops new guests, not live ones, which would otherwise be killed by a configuration edit | The provider may read one named ConfigMap or Secret without widening the shared role, an admission rule for `hostCa` is added, or a held, half-booted guest is seen to matter, or the admission CEL is exercised against a live API server and found to differ |
| **The signer leaves no record of what it issued** (TB-21): counters per machine (issued, refused by reason), nothing per certificate, lost at provider restart; the serial is random and not kept | A per-certificate record is a log of public material, and the identity layer's own audit sees every certificate presented to it. The counters answer "is it being abused", not "what was issued" | An incident review needs which certificates a host issued, or the identity layer's audit proves insufficient |
| **Nothing programs the gateways' neighbour table or turns learning off on the isolated ports** (ADR-0144 Decision 7, "not built"): a sandbox can claim another sandbox's address toward a gateway, or fill the bridge's address table | Port isolation stops every sandbox-to-sandbox frame, so the claim is heard only by the gateways, and a gateway identifies a claim by its mTLS identity and per-claim allowlist, not its source address (mediatore ADR-0028). Who programs the entries (the provider through netlink, or the gateway image from the claim) is not decided | Per-sandbox pinning is decided and built, or a gateway is found to authorize by source address |
| **A VM can name the gateways' non-isolating class and sit un-isolated on the sandbox bridge** (ADR-0144 Decision 3; TB-19): the class that lets a gateway be reached is a name any `VMClass` can carry, and no admission policy restricts which `VMClass` may name it | Isolation is per class by design, and the same gap is already recorded for any isolated `networkClass` (the §8 row on two-NIC guests, §7.28). Only platform admins create `VMClass`es and the class is declared only on sandbox hosts, but a tenant can name any of them (§7.30) | A policy states which `VMClass`es may join a class on an isolating bridge, or a host config can bind a class to a label |
| **A compromised provider can clear a sandbox's isolation**, put a tap on another bridge, or rewrite the host rules' files if it can write them (ADR-0144 Decision 7; TB-19) | Already a host-level compromise: the provider holds `CAP_NET_ADMIN` to own every guest's tap (ADR-0063, and the §8 row on the provider's reach). The rules, the unit and the env file are `root:root`, outside the provider's writable paths | The privileged helper split the §8 provider row waits for, or a periodic check that every sandbox tap reads `isolated on` |
| **`preflight` checks that the host rules exist, not that they are right**, and does not check the forwarding state, `br_netfilter` or the IPv6 sysctl (ADR-0144 Decision 6) | `preflight` reads `iptables -S` and `iptables -t nat -S` and refuses `install` for an isolated class unless the three `BANLIEUE_SBX_*` chains exist and each is jumped to first from INPUT, FORWARD or POSTROUTING, failing closed when the ruleset cannot be read (`crates/banlieue-host/src/stages.rs`, `firewall_problems`). The rules themselves are an operator recipe by decision (ADR-0067 Decision 7), shipped with a verification procedure and the counters to read (`docs/src/guides/cloud-hypervisor-sandbox-segment.md`); §7.32 says a green `preflight` is not a safe segment. The kernel-isolation check also exists (`stages.rs`, `isolation_problems`) | `preflight` verifies what is inside the chains, or the recipe's unit runs a self-check after every apply |
| **The sandbox rules share a kernel firewall with another tenant that changes it** (ADR-0144; TB-19, TB-20): a reload of theirs, a jump inserted above ours, or a change of `ip_forward` or `br_netfilter` can displace or bypass the rules until ours are re-applied, and our ACCEPT for the gateways' outbound traffic bypasses a later DROP of theirs | A host serving sandboxes cannot take the firewall from its other tenants, so the recipe takes only three chains, re-pins its jumps on `reload`, and leaves the rest alone. The bypass of a later DROP is intended (the gateways must get out) and is called out in the guide for the operator to check against the host's policy | Nothing triggers the reload when the other tenant acts (a `PartOf=` drop-in or a path unit does), or a host with no other tenant needs this |
| **The host forwards a compromised gateway's traffic to anything the host can reach** (TB-20), and **egress is attributed only inside the gateways**: beyond the host every sandbox shares the host's address, and the host's NAT records nothing | The gateway's per-claim allowlist is the control (mediatore ADR-0028, ADR-0030), and a gateway is the one component on both bridges by design (TB-18). A host-side destination filter would have to know the host's own networks, which only its operator can; none is in the recipe | The recipe gains an operator-declared deny list of the host's own ranges for the uplink bridge, or a gateway is found to forward outside its allowlist |
| **The NAT path is shared and unmetered**: a hostile sandbox can exhaust a gateway's CPU or the host's connection-tracking table, and conntrack is host-wide, so the host's other tenants lose it too (TB-20) | The gateways are guests in `banlieue-guests.slice` and the host's budget bounds their CPU (ADR-0105); the gateway's per-claim controls bound a claim's reach, not its volume. The cost of two gateways is already in the ADR's capacity figures | A rate limit per claim at the gateway or a `connlimit` rule in the recipe, or the first host that shows contention |
| **Two gateways are a per-host availability dependency that shares the host's failure domain** (ADR-0144 Decision 4): if both fail, or the shared address does not move, every sandbox on that host loses its way out, and a host failure takes sandboxes and gateways together | The gateways exist to remove one VM as a single point, not the host; how the address fails over is the gateway image's concern (mediatore) and an open question in the ADR. A sandbox with no route is a denial, never a bypass | The gateway image decides failover, or gateways are placed across hosts |
| **A sandbox segment's isolation is measured, not proven, on the target**: the host rules were exercised in a privileged container (kernel 6.8, iptables 1.8.9) and not on `br_netfilter`, RHEL 8's kernel 4.18 or iptables 1.8.4, `firewalld`, or after a reboot; the bring-up masquerade is a by-hand step nothing enforces the removal of | The guide records exactly what was and was not verified, and its probes and counters are the procedure to run on the host before it serves sandboxes (§7.32) | A live run on the first sandbox host passes, and the guide's unverified list is empty |
| **On a host older than systemd 248 the guests share one IPC namespace** (SysV IPC, POSIX message queues), because `PrivateIPC=` is unknown there (ADR-0139). Two escaped VMMs on such a host could talk through them, around the per-uid isolation | Only reachable after a VMM escape, and only on a host whose operator named the gap with `--accept-reduced-hardening=PrivateIPC` (the install is refused otherwise). `ReadOnlyPaths=/dev` still closes the `/dev/shm` file route (measured on systemd 239). The alternatives are no RHEL 8 hosts or an unrecorded gap; neither is better | A compensation is decided (`InaccessiblePaths=/dev/mqueue`, or a `SystemCallFilter` denying SysV IPC that the VMM is shown not to need), the list is published in `Provider.status` so a scheduler can avoid such hosts, or the host's systemd is upgraded |
| **`--guest-cpu-quota` is a share of CPU time, not isolation** (ADR-0139): guests are not kept off other workloads' cores and share SMT siblings with them, so cache and SMT contention remain. On cgroup v1 it is the only CPU ceiling | A quota is what the kernel can enforce there; pinning is refused rather than faked. The installer says so when the quota is used without `--guest-cpus` | A host where that matters moves to cgroup v2 and `--guest-cpus` |
| **`files` mode adds up to `2 × --guest-uid-count` system accounts to a host's passwd and group** (ADR-0139), and removing them is manual, as the crate has no uninstall verb | No login, no home, no password: the same accounts userdb gives, in the other database. `preflight` prints the count before anything is written and `--guest-uid-count` lowers it. A directory service that cannot enumerate system accounts is an open question of the ADR | An uninstall verb exists, or a host's directory service is shown to mishandle the range |
| **`banlieue-guests.slice` is a ceiling, not a reservation** (ADR-0105): `AllowedCPUs` confines guests to a CPU set but does not keep other workloads out of it, and nothing stops the host's own processes from using the memory the budget leaves free. Guests and the host also share one kernel | Reserving capacity for foreign workloads is the host owner's decision (their own cpuset or slice); banlieue declares only what *it* may take, and never infers it | `Provider.status` reports the slice budget instead of physical totals (ADR-0105 follow-up), or a multi-tenant host is supported |
| **A VM stopped in vCenter is started again by the vSphere provider** (ADR-0034 amendment 2026-10-23): an operator's out-of-band power-off is reverted within one long requeue, so it cannot be used to quarantine a VM that banlieue still declares `PoweredOn` | The declared state is the source of truth, as for every other provider; observe-only left a `poweroff: true` install stuck off forever. The hold is a `desiredPowerState: PoweredOff` write | A hold mechanism exists that is not a power state (for example a `banlieue.io/hold` annotation the provider honours), or an incident process needs to freeze a VM without a Kubernetes write |
| `banlieue-operator` holds the union of every permission it can delegate | Kubernetes escalation-prevention requires it; granting `escalate`/`bind` instead would be strictly worse. The ceiling is auditable in one file. This includes Lease `get`/`create`/`update`/`patch` **cluster-wide**: it delegates them, narrowed, into every provider's Role, and since ADR-0102 it reads each provider's Lease, so a compromised operator can forge any heartbeat (TB-17) | A provider needs a materially more dangerous permission |
| **Liveness assumes clocks agree to within `staleAfter`** (ADR-0102): a provider writes `renewTime` by its own clock and the operator judges it by its own | Leader election already assumes the same; `staleAfter` defaults to four lease durations, and a future timestamp is capped at one `staleAfter` of grace | A host's clock is found to drift past `staleAfter`, or `staleAfter` is lowered close to the lease duration |
| **The operator is the only evaluator of liveness**: while it is down, every heartbeat stays at its last value, and a provider that dies then keeps `Alive` | Liveness is a status field of the operator's (ADR-0012); a second evaluator would be a second writer. The operator's own `banlieue_leader` gauge is what to alert on, and `docs/src/guides/observability.md` says so | Liveness needs to survive an operator outage (e.g. the scheduler reading the Lease itself) |
| `banlieue-imagebuild` runs `privileged` | kairos' builder genuinely requires loop devices and chroot; isolation is by namespace | kairos supports rootless builds |
| Rendered user-data is visible in `VSphereMachine.spec` **and `LibvirtMachine.spec`** | Single-tenant, single-namespace posture (ADR-0025). ADR-0042 closed the *escalation* (a principal reaching user-data it could not read); the *reflection* to anyone who can already `get` the infra CR is unchanged and deliberate, and ADR-0050 extends it to a second kind rather than introducing a new risk | A second tenant or namespace becomes real — which ADR-0130 now makes possible, so revisit before serving an untrusted tenant; ADR-0025's superseded per-VM Role design is the shape that scales |
| The controller's user-data Role is namespace-wide, not `resourceNames`-scoped | The names a validly admitted `VirtualMachine` may cite are unknowable when the manifest is written; authorization moves to admission, where the requesting identity still exists (ADR-0042). A compromise of the controller identity itself is still bounded only by the namespace | The install stops shipping `deploy/admission/`, or per-VM RBAC becomes tractable |
| A libvirt guest's TPM is **emulated by swtpm on the host**, so a host-root adversary can read the sealed-key material that a physical TPM would protect | This is the libvirt trust model, not a banlieue choice; the hypervisor operator is already semi-trusted (§4) and hypervisor compromise is out of scope (§9). EK trust anchors differ per backend, and since 2026-09-27 that is explicit rather than implicit: `Provider.spec.attestation.ekTrustBundle` (ADR-0049 Decision 10, A-15) names which issuers count, per backend, as an admin assertion | The attestation exchange itself ships (the broker and in-guest agent, outside banlieue), or a libvirt host is no longer operator-trusted |
| **swtpm EK certificates never expire** — the observed `notAfter` is `9999-12-31` — so validity-period checks are not a revocation mechanism on libvirt | Nothing banlieue controls: `swtpm_localca` issues them that way. Expiry would be a weak control regardless, since a sandbox's whole life is measured in minutes. Revocation on libvirt is removing the issuing host's CA from `ekTrustBundle` — a per-host decision an administrator now makes on a field that exists (ADR-0049 Decision 10, landed 2026-09-27) rather than one a certificate makes for them. §7.14 records the corollary: whoever can re-add an entry can undo a revocation | A verifier needs a per-certificate revocation story rather than per-host bundle membership |
| On libvirt the EK certificate is **reported by the guest**, not read from the hypervisor, because swtpm persists no host-side copy | Forced by swtpm's design, not chosen (ADR-0045): the certificate is loaded into the vTPM's NVRAM and the issuing temp directory is deleted, and no libvirt RPC exposes it. It is sound because an EK certificate is a public key — substituting another member's does not yield its private half, so ADR-0049's activation fails — and the subject-CN binding catches the substitution earlier still. The residue is that a guest can *withhold* its certificate, which denies only its own readiness | libvirt or swtpm grows a host-side read, or a member withholding its certificate becomes something worth distinguishing from one that is merely slow |
| `GuestReady` can be asserted by any code running as root inside the guest, so it proves which disk booted only for a guest that has not been compromised, true on both transports (`qemu-guest-agent` file read on libvirt, `config.extraConfig` on vSphere) | It is a *liveness* signal by construction (ADR-0043 Decision 9) and is consumed only to decide when a **fresh, unclaimed** VM joins a warm pool, before any subject has touched it. Treating it as integrity would be the error; the document and the ADR both say so explicitly | The consumer's attestation (SPIRE TPM node attestation against the member's EK, ADR-0049 as amended 2026-11-05) is the integrity signal wherever it runs; this one stays what it is |
| A **broker** both holds subject credentials and decides which attested guest receives them, so its compromise (or that of the consumer's SPIRE server, which runs the attestation) is the design's worst case | Somebody has to hold the credential to deliver it, and somebody has to decide which guest attested; concentrating both in one audited deployment is preferable to spreading either. banlieue is deliberately not that component (ADR-0049 Decision 2), so a controller compromise discloses no subject credential | The broker is split into deliver/verify roles, or hardware-backed key custody becomes available to it |
| A declared **broker** may attribute a sandbox to any identity, so the audit trail is only as honest as the broker is | Handing sandboxes out on behalf of other people is a broker's entire purpose; a broker that could only name itself could not broker. The concentration is explicit, empty by default, and confined to one auditable ConfigMap (§7.6) rather than diffused across everyone holding `create`. mediatore also accepts declarative `AgentSandbox` requests, so the honesty of the trail rests on mediatore's own subject-pinning admission policy as well as on mediatore; that control is mediatore's and is not verified here | A broker is compromised; claims need per-request proof of the subject's consent rather than the broker's assertion; or a broker that accepts declarative requests ships without pinning their subject to the caller |
| **A guest's vTPM can be failed by host CPU contention** on stock Cloud Hypervisor v53.0: the VMM gives swtpm 100 ms per response, then marks the TPM fatally failed for the guest's lifetime. A busy host (another guest, a burst of installs) is enough; on a `Deferred` `tpmEnabled` member it aborts the install, which the pool reaps at `provisioningTimeoutSeconds` (D) | Not banlieue code: it is in the VMM's TPM backend (`tpm/src/emulator.rs`). The cost is bounded by the pool's reaper, and nothing is disclosed or forged: the TPM refuses, it does not answer wrongly. The fix is known and tested (`docs/src/reference/cloud-hypervisor-pool-timings.md`); reported upstream as [cloud-hypervisor#9053](https://github.com/cloud-hypervisor/cloud-hypervisor/issues/9053) | Every Cloud Hypervisor host runs a VMM with the response-deadline fix (upstream release, or a patched VMM installed with `--vmm-url`) |
| **The identity provider is trusted absolutely, and is outside banlieue** | The API server is the only thing that can attest a caller, and it attests whatever the configured issuer asserted. banlieue cannot verify an upstream IdP without becoming an IdP. The claim-subject policy is still worth having: it binds an attribution to *whatever* identity the cluster does authenticate, which is strictly better than free text | banlieue ever needs an attribution stronger than the cluster's own authentication — at which point the answer is per-request proof from the subject (a signed consent, or the attested channel of ADR-0049), not a better check on the caller |
| A consumer's **cached ID token** sits on their laptop (`~/.kube/cache/oidc-login`) and authenticates as them if stolen | Not specific to claims — every Kubernetes bearer token behaves this way, and client-side credential custody is out of scope (§9). Recorded because the claim-specific consequence is distinctive: the audit trail records the *victim* requesting a sandbox, which is exactly the fiction §6/TB-1 exists to prevent. Short token lifetimes are the issuer-side mitigation | Claims carry per-request proof of the subject's intent rather than only the caller's identity |
| `subject.issuer` is allowlisted but never *verified*: the API server does not reveal which issuer minted the caller's token | Nothing in Kubernetes attests it directly, but with one distinct prefix per issuer (§7.6) the username prefix identifies the issuer, and since ADR-0047's 2026-10-13 amendment the policy checks it for every issuer with an `issuerPrefixes` line. For an issuer that falls back to `usernamePrefix` the allowlist remains the strongest check: it stops a claim naming an issuer the site does not use, which is what would make the recorded attribution meaningless. The claim deliberately carries no token to verify (ADR-0047 Decision 9) | The in-guest agent's JWT validation lands (roadmap phase C), at which point the *guest* verifies issuer, audience and `oid` against the claim |
| An `installMode: Manual` image can claim a deferred install it does not perform, so a `tpmEnabled` VM built from it is unencrypted and says nothing | `Manual` exists precisely for builds banlieue does not drive (ADR-0040), so inspecting it is not possible without becoming its build system. ADR-0048 closes the case banlieue *can* see (`Immediate`, and an absent `template`) and fails closed there; `Manual` is a deliberate operator assertion, narrower than the blanket gap it replaced | A backend reports sealed-partition state back to banlieue, making the assertion verifiable rather than trusted Since ADR-0088 an image that **declares** `firstBootSealing` is checked by the guest's sealing report whatever its install mode; a `Manual` image without the declaration remains uncontrolled. |
| **`Ready` does not reflect first-boot sealing**: a clone whose first boot did not seal is `Ready=True` with `GuestReady=False` (ADR-0088 Decision 5 keeps `Ready` unchanged) | `GuestReady` is the readiness input of the consumer that hands VMs out (`VirtualMachinePool`); gating `Ready` would regress every image that never reports, as ADR-0043 found | A consumer other than a pool relies on `Ready` alone for VMs it treats as sealed |
| An **`Asserted`** first-boot sealing image (a `Template` or `BackingFile` source) can carry a forged report stage that says sealed while the disk is plaintext | banlieue did not build the disk and cannot inspect it before boot; the declaration is the operator's claim, the same class as `Manual` (ADR-0088 Consequences). `Url` builds are not affected: banlieue wrote both policy and report | Image provenance can be verified (signed images), or a backend can read the partition table host-side |
| **A sandbox image that bakes in a key that is not TPM-bound** gives every pool member the same identity (ADR-0108) | banlieue ships no sandbox guest software and cannot tell a public certificate from a private key inside a consumer layer, so ADR-0108 Decisions 2 and 3 (no per-VM identity in the image; identity made on first boot onto a sealed partition) are a contract on the image author, stated in the pools guide (*Building a sandbox image*), the same class as `Asserted` and `Manual`. A cloned **TPM-bound** key fails loudly, since it cannot attest against the clone's own EK; only a key that is not TPM-bound can be cloned silently, and only the consumer's identity layer can notice it, by binding each agent's identity to the EK banlieue publishes | The consumer's attestation binds every workload identity to the member's EK, or banlieue gains a way to verify an image's contents |
| **A first-boot sealing image built on a Kairos older than v4.4.0** boots plaintext (no `encrypt-pending`) or cannot unlock after a reboot (kairos-io/kairos#5193). This replaces the pre-acceptance risk "first-boot sealing runs on pre-release Kairos", retired 2026-10-26 when v4.4.0 shipped #4924 and the fixes | The builder never learns the source image's Kairos version, so it cannot refuse an old one (ADR-0088 Amendment 2026-10-22, B). Such a clone never reports its partitions sealed, so it is `GuestReady=False` (`FirstBootSealingUnverified` or `FirstBootSealingTimeout`) and a pool never hands it out (§6/TB-5); the docs and example 23 require v4.4.0 | The builder can read the source's Kairos version (for example `/etc/kairos-release` in the unpacked rootfs) and refuse an older one |
| **Blocks written to `COS_PERSISTENT` before it is sealed are not guaranteed wiped**: on the `iso` path the template install lays the partition out once, and on the raw `cloudImage` path each clone's recovery reset does; first-boot sealing then turns it into a LUKS container, and nothing in banlieue or ADR-0088 asserts the old blocks are overwritten | What is written before sealing is image content (the install or reset), not a per-VM secret: per-VM user-data goes to `COS_OEM` (the row below), and nothing runs from `COS_PERSISTENT` before `encrypt-pending` | A per-VM secret is written to `COS_PERSISTENT` before first-boot sealing, or Kairos documents that sealing wipes the data area |
| **`firstBootSealingLayer` can overclaim on an image changed after its first build**: adding `firstBootSealing` updates the merged cloud-config, but an existing `OSArtifact` is not rebuilt, so status says the layer is in the build while the artifact predates it | The guest readback still withholds `GuestReady` from such a clone, so the overclaim is in provenance, not in what gets handed out; the guides say to recreate the `VMImage` | The `OSArtifact` rebuild is keyed on the merged cloud-config's content |
| **An artifact is rebuilt needlessly when a worker's `Node` object is deleted and re-registered with its disk intact** (ADR-0115): the node's new `creationTimestamp` reads as a replacement | The cost is one rebuild (about eight minutes) per image on that node, and the opposite error, an ISO gone while `Ready` is reported, is what the gate exists to stop. A volume with no node affinity, or a read that fails, is never judged lost | Revisit when storage that survives node loss is the default, or when a probe of the file itself is added |
| First-boot sealing does **not** protect `COS_OEM`, where per-VM user-data (A-2) is persisted in plaintext for the life of the VM | Kairos reads its sealing policy from `COS_OEM` before anything is unlocked, so OEM stays plaintext by design (ADR-0088 Consequences); the example and guides say so | Secrets stop travelling in user-data (short-lived join tokens, fetching secrets at boot from an authenticated source) |
| The NoCloud `cidata` seed stays attached after install, so a guest can read its own rendered user-data (A-2) from inside the sandbox | ADR-0044 Decision 4, deliberate and scoped: cloud-init re-reads its datasource on every boot, so removing the seed risks regressing per-boot modules in a way that needs its own live verification on a reboot — not a first boot. Ejecting the *installer* removes the build-time overlay shared across every VM, which is the broader exposure; what remains is each guest's own material, which that guest's workload could in principle obtain anyway | The seed eject is verified live across a reboot, or user-data delivery stops needing a persistent datasource |
| A stolen host token (A-11) can patch **any `CloudHypervisorMachine`** in its namespace — another host's included — clearing finalizers or rewriting status | Machine names are not known when the Role is written, so `resourceNames` cannot scope them. Providers and their status *are* scoped since External mode (2026-09-26). The token still cannot read Secrets or create or delete machines. Operators bound it by namespace (§7.11) | Per-host machine scoping becomes possible (a label-selector authorization, or one namespace per host) |
| A stolen host token can **renew itself**, so it does not expire on its own | Renewal by TokenRequest is what lets a host run unattended (ADR-0060 Decision 5), and the API cannot tell the host's request from a thief's. Revocation is immediate and total — delete the ServiceAccount (§7.11) — and every renewal is an audited API call | Token requests can be bound to a host attestation (a TPM-bound key), or the API server gains per-token revocation |
| `banlieue-operator` holds `serviceaccounts/token` `create` in its install namespace, so a compromised operator can mint tokens for **other** ServiceAccounts there, the controller's included | RBAC requires a grantor to hold what it grants, and each External provider's Role carries renewal on its own ServiceAccount. Held namespaced, never cluster-wide (`deploy/operator/rbac/role.yaml`; a test fails if the ClusterRole ever gains it). The operator already holds broader grants (SEC-007) | The operator stops needing to delegate token creation — e.g. the provider renews through a narrower API |
| Guest addresses reported on status come from the host's neighbour table, which **the guest influences**; the tap is a plain bridge port with no MAC/IP filtering | Same posture as a libvirt NAT network. Addresses are a convenience for consumers, not an identity — the same reasoning as `GuestReady` (ADR-0043 Decision 9). Static IPAM (ADR-0024) gives an address the guest did not choose | Bridge-level anti-spoofing (nftables `bridge` family) is added, or an address becomes load-bearing for authorization |
| A **compromised provider process** reaches every guest on its host: it can read their disks and reconfigure taps (`CAP_NET_ADMIN`), including clearing a sandbox port's isolation (ADR-0144, TB-19) | Inherent in a host-resident hypervisor manager: something on the host has to own guests' storage and networking, and the alternative (root) is strictly worse. It holds three capabilities and no others, can manage only instances of its root-owned templates for uids in the guest range (so it cannot start anything as root), writes only the run root, the storage classes and its own state directories, and sees `/dev`, `/sys` and `/proc` read-only, so its capabilities do not reach the host's devices, sysfs, sysctls or cgroups (TB-9) | A split into a minimal privileged helper and an unprivileged reconciler becomes worth its complexity |
| Hosts verify an image's **integrity** (digest) but not its **provenance**: whoever can write `VMImage` status and push to the configured repository chooses what hosts boot | Digest pinning already defeats a registry that substitutes content, and the host pins the repository. Signing needs a key-management story of its own, which ADR-0064 defers to its own ADR (cosign/sigstore) | Signature verification on pull lands, or the repository is shared with parties outside the platform team |
| The imagebuilder's identity can **create Jobs in `banlieue-imagebuild`**, a privileged namespace, so its compromise is node-root-equivalent there | It already drove privileged builds through `OSArtifact`s; the push Job, and since ADR-0010's 2026-11-26 amendment the digest Job, must run beside the PVC they read. The Jobs it creates hold no API credential and no privilege | The push can run outside the privileged namespace (e.g. a restricted namespace with a read-only clone of the artifacts volume) |
| The push Job's CA bundle (ADR-0110) is a **trust anchor, not a pin**: any certificate the named CA issues for the registry's host name is accepted, and a CA that issues for other names widens what the Job would trust | The operator names the CA, banlieue adds it to the public roots rather than replacing them, the bundle is reachable only through the imagebuilder's own configuration, and the host never uses it (it pulls by digest from its own repository with its own trust store). The exposure is the push credential and the artifact in transit | A per-registry certificate or public-key pin is added, or the same bundle is offered to anything that reaches a host or a tenant |
| Images in the registry can carry cloud-config (A-2), readable by the registry's operator | The registry is the operator's, like the datastore in TB-5; banlieue cannot encrypt what a host must boot without a key-distribution scheme it does not have | Per-VM secrets move entirely out of shared images, or images are encrypted for their hosts |
| The Cloud Hypervisor API decoder (`banlieue-cloud-hypervisor`) is **not fuzzed** | The peer is a local VMM the provider started, not a network endpoint; decoding is into typed, bounded structs and failure is reported, not fatal. The libvirt decoder is fuzzed because its peer is remote | A fuzz target is added alongside the libvirt one, or the client ever talks to a VMM it did not start |
| A **non-pinned VMM release** is verified against GitHub's published digest or the operator's flag, not a pin reviewed in a banlieue change, so a release **replaced on GitHub itself** (a compromised upstream account) would verify | Only when an operator names a version other than the pinned one, per host and explicitly (ADR-0084 Decision 4). It still catches a corrupted or substituted download from any mirror, and Cloud Hypervisor publishes no signatures to check instead. The pinned release keeps its compiled digests | Cloud Hypervisor signs its releases (then verify the signature), or a fleet needs non-pinned releases routinely enough to pin them in banlieue instead |
| A **newer VMM** than the pinned one runs against client types checked only against the pinned release's API document | The client sends only fields that exist in the pinned schema (`spec_tests.rs`), and the gate refuses older releases, which is where removals would bite. A newer release could still change a field's meaning; the operator chose it, and the guide says to try a new major on one host first | A newer release is pinned in banlieue (vendored spec, digests, tests), or the client learns to read the running VMM's API document |
| With kine, **the management cluster's datastore is one database on one host**: if it is down the API server cannot read or write, and if it is lost so is the cluster's state | A choice the operator makes per cluster (`K0S_STORAGE_TYPE=kine`, ADR-0085), for state outside the VMs and ordinary database backups; etcd remains the default. Running guests are unaffected while the API is down; controllers resume when it returns | The management cluster needs to survive its database host, then a replicated database or etcd |
| **banlieue's TLS clients fall back to classical key exchange with peers that do not accept ML-KEM.** Every client offers X25519MLKEM768 first (ADR-0170, `crates/banlieue-tls`), so the residual exposure is only the peers that answer with a classical group: Azure Resource Manager and the Entra ID token endpoint (both `secp384r1`, measured 2026-11-26 with `crates/banlieue-tls/tests/live_pq_probe.rs`, carrying A-29), and any vCenter, Proxmox VE or libvirtd whose TLS library lacks ML-KEM (carrying A-1). Such a session can be recorded now and decrypted by a future quantum-capable adversary | The fix is on the peer's side, and banlieue upgrades with no change when it lands. Refusing classical groups would cut banlieue off from Azure and from most hypervisors today. Kube API traffic to a Go 1.24+ API server, and any peer that offers the hybrid, is already post-quantum | Entra ID or Resource Manager negotiate ML-KEM (ADR-0159 Decision 5; re-run the probe), a hypervisor distribution ships a TLS library with ML-KEM, and in any case at a review no later than 2027-05-25 |
| **Classical key exchange on links banlieue does not terminate**: the kine datastore on PostgreSQL 17, libvirtd's mutual TLS (GnuTLS builds without ML-KEM negotiate X25519 only), and the WireGuard tailnet (Curve25519, no pre-shared key) | Each is fixed upstream or by the operator, not in banlieue: PostgreSQL 18's `ssl_groups`, a distribution GnuTLS with ML-KEM, Tailscale post-quantum key exchange. Inside the tailnet, API traffic is itself post-quantum TLS where the endpoints support it (§7.20) | The operator moves to PostgreSQL 18; a GnuTLS with ML-KEM reaches the hypervisor distributions; Tailscale ships post-quantum key exchange |
| **No post-quantum signatures anywhere**: the cluster CA, kubelet and libvirt certificates, SPIRE's X.509 CA, OIDC ID tokens (RS256) and public CA certificates are all RSA or ECDSA | Forgery needs a quantum computer at the time of the attack, not a recording, and ML-DSA is not yet supported by Kubernetes, Go's `crypto/x509` issuance paths, SPIRE or JOSE implementations | ML-DSA (FIPS 204) is supported by Kubernetes certificates, SPIRE, the OIDC issuer and the API server's JWT verification |
| **An OIDC issuer running on the management cluster lets a cluster admin mint any identity**, so claim attribution (TB-7) is no stronger than the cluster-admin boundary | Cluster-admin is already out of scope (§9), and a separate identity cluster is more infrastructure than a single-operator site needs | Management-cluster admins and claim subjects become different trust populations; then run the issuer outside the management cluster |
| **Any LAN host can take the control plane VIP** by sending VRRP adverts at a higher priority, and blackhole the API until it stops (ADR-0086) | VRRP's simple password is not authentication, and keepalived offers nothing stronger that k0s exposes. The takeover cannot impersonate the API (TLS is end to end, §6/TB-13); it is the same class of LAN denial of service as ARP spoofing a node's own address, which the bridged nodes already accept | The LAN carries hosts that should not be able to disturb the cluster, then a dedicated VLAN, or unicast VRRP with a host firewall admitting only the controllers |
| With konnectivity disabled, **the API servers reach every kubelet and pod directly** (ADR-0086) | Both backends build flat networks where that reach exists anyway; the kubelet authenticates the API server; konnectivity's alternative pinned every agent to one controller and broke webhooks on the others | A node joins from a network the controllers cannot reach, then `K0S_DISABLE_KONNECTIVITY=false` with node-local load balancing |
| With `LIBVIRT_BRIDGE`, the **management cluster's nodes are on the LAN**, so every LAN host reaches their API server, kubelet and node ports | Needed for host-resident providers and DNS clients to reach the cluster without tailscale (ADR-0085). The API server and kubelet authenticate every request; nothing banlieue adds listens unauthenticated | The LAN carries hosts that should not reach the cluster at all, then a dedicated VLAN or host firewall rules |
| The installer's `O_NOFOLLOW` protects only a path's **last component**: a directory the provider's user owns could be swapped for a symlink between the installer's check and its use, one level up | The window is one run of a root command started by an operator; the directories are created (and a symlink there refused) earlier in the same run; `openat2(RESOLVE_NO_SYMLINKS)` would forbid legitimate symlinks on the path, such as a storage class under a linked mount | A host where the provider's user is untrusted while an install runs; then resolve beneath each banlieue-owned root with `openat2(RESOLVE_BENEATH)` |
| The Proxmox provider's `ClusterRole` can `update`/`patch` **any `ProxmoxMachine` in any namespace**, so a compromised provider pod can clear finalizers or rewrite status on machines placed on other Providers | Machine names are not known when the role is written, so `resourceNames` cannot scope them, exactly as for libvirt. It still cannot read Secrets it is not named for, and cannot create or delete machines (`deploy/provider-proxmox/rbac/clusterrole.yaml`); its **Proxmox** reach is bounded by its own Provider's token ACLs | Per-provider machine scoping becomes possible (a label-selector authorization, or one namespace per Provider) |
| **The ownership marker is not a secret and is not signed**: anyone with `VM.Config.Options` on the Proxmox side can delete the `banlieue-machine-uid=` line (orphaning a VM: banlieue then clones a second) or copy it onto another VM (which banlieue would then treat as its own, and configure, start and finally destroy). On vSphere, anyone with `VirtualMachine.Config.AdvancedConfig` can do the same to `banlieue.machine.uid` (ADR-0101): deleting it orphans an unrecorded clone (the next pass reports `VMNameConflict`), copying it onto a same-named VM in the target folder gets that VM destroyed on the machine's next pass without a `vmRef`, or adopted when the machine's recorded VM is gone (ADR-0101 amendment 2026-11-04, and only when it is the single marked VM) | The party who can do that is the hypervisor operator, already semi-trusted and already able to destroy any VM directly (§4); the marker exists to stop *accidental* collisions between namespaces and with unrelated VMs, which the name could not, not to defend against the hypervisor's own administrators. The token itself holds `VM.Config.Options` on `/vms`, so a compromised provider can edit descriptions too (bounded by §4's stolen-token row); the vSphere provider's account writes `extraConfig` on every clone, so the same holds there. A *tenant* cannot plant a marker ahead of time: a machine's UID is assigned by the API server, not chosen by whoever writes the `VirtualMachine` | A machine's UID becomes something a tag ACL can protect, or a per-VM signed marker is worth its key management |
| **That a vSphere guest cannot reach `banlieue.machine.uid` is documented, not tested** (ADR-0101): the control rests on the guest RPC (`vmware-rpctool info-get` / `info-set`) being confined to `guestinfo.*` | That confinement is how VMware Tools' guestinfo channel is defined, and the impact is low even if it failed: a guest could orphan its own VM on an interrupted pass, or mark it with another machine's UID, which matters only for a VM of that machine's name in that machine's folder | The first ADR-0101 run against a real vCenter: from inside the guest, `vmware-rpctool "info-get banlieue.machine.uid"` must fail and `info-set` must not change the key |
| A vSphere `Template` source trusts **whatever template of that name sits in the zone's folder**: banlieue checks the name and the template flag, not what boots from it, so anyone with write access to `<rootFolder>/<failure-domain>` chooses what every clone there boots (ADR-0104) | The same posture as the Proxmox `Template` row below and ADR-0064's deferred signing; per-zone folders narrow it from the whole datacenter (first match) to one folder, and the party who can write there is the hypervisor operator (§4), who can already destroy any VM directly | Template provenance (a signed image reference recorded on the template) is available, or images are imported by banlieue |
| A `Template` source trusts **whatever the administrator put in that VMID**: banlieue verifies only that it is a template, not what boots from it | Proxmox has no image provenance to check, and banlieue builds none of these templates (the bootstrap script imports a cloud image over HTTPS from its published URL). The same posture as vSphere `Template` sources and as ADR-0064's deferred signing | Template provenance (a signed image reference recorded on the template) is available, or images are imported by banlieue |
| **The Proxmox provider has not run as a deployed controller**: the lifecycle passed live through the reconciler functions (2026-09-30), but not through a cluster (finalizers, SSA status, controller dispatch), and not with EFI, vTPM, guest-agent addresses or a failing task | Each of those is unit-tested against the fake and the API calls under them are now observed live | A kind-cluster e2e runs a `VirtualMachine` to `Ready` on a Proxmox node |
| The Proxmox seed ISO stays attached for the machine's life (as for libvirt and Cloud Hypervisor), so a guest can read its own rendered user-data | Same reasoning as the NoCloud entry above: cloud-init re-reads its datasource on every boot, and ejecting it needs its own live verification across a reboot. What remains is each guest's own material | Detaching the seed is verified live across a reboot on Proxmox, or delivery stops needing a persistent datasource |
| **Address squatting:** anyone who can create a `VirtualMachine` on a `networkClass` can claim an address on it first, and ADR-0083 then blocks everyone after them | First-come-first-served is how the network itself behaves. ADR-0083 makes that visible rather than creating it. The alternative (pre-assigned address ownership) is IPAM, and CAPI IPAM pools (ADR-0033/0053) are where per-tenant address ownership belongs | A `networkClass` is shared between tenants who do not trust each other, or CAPI IPAM lands and static overrides can be restricted to IPAM-issued addresses |
| **Cross-namespace "address in use" oracle:** a tenant can learn whether an address is claimed on a `networkClass` by any VM in any namespace, by declaring it and reading the condition | One bit per probe, never a name (the holder is unnamed across namespaces, §6/TB-1). The same fact is observable on the wire by anyone on the segment, which is where the address actually lives. Consistent with the single-tenant posture (ADR-0025) | A second tenant becomes real on a shared `networkClass` (ADR-0130 makes that possible: revisit before serving an untrusted tenant); then scope the check per tenant or move address ownership into IPAM |
| Health and metrics endpoints bind `0.0.0.0` without authentication, and `/readyz` says whether this replica leads | Probes come from the kubelet, not a pod, so they cannot authenticate; leadership is also readable from the Lease. Metrics carry no object names (TB-14). ADR-0093 retired the old "fixed `200`" entry: the answers are now real | A metric gains a label that names an object, or a non-probe client needs these ports |
| **The NetworkPolicies are opt-in, and their destinations are "any address" until narrowed** | banlieue cannot know a site's API server or hypervisor CIDRs, and a wrong egress rule breaks the product silently; there is no templating layer to fill them in (ADR-0094). Port-level restriction already stops most lateral movement. The Azure provider's 443 rule cannot be narrowed by address at all: ARM and Entra ID are anycast front ends Microsoft publishes only as service tags (`deploy/network-policies/provider-azure.yaml`), and neither can the Azure publish Job's, which also reaches the managed-disk upload hosts (`deploy/network-policies/azure-publish.yaml`) | The operator gains RBAC to write per-workload policies with addresses from the `Provider` (ADR-0094 follow-up); for Azure, when the supported CNIs match FQDNs or banlieue ships an egress proxy |
| **A rotated credential is used for up to ~30 s before banlieue picks it up**, and a revoked one keeps failing reconciles until the Secret is updated | Watching Secrets would grant every provider read access to every Secret in its namespace (ADR-0095). Immediate cut-off belongs on the backend, which stops a credential everywhere at once | A provider caches a client across reconciles (the cache must then key on `resourceVersion`), or Kubernetes can scope a watch to one named Secret |
| **Telemetry is not integrity-protected**: a party on the path or with scraper access can forge or suppress it | Nothing banlieue decides reads telemetry back; the CRs' status is the record (TB-14) | An automated decision (autoscaling, remediation) starts consuming banlieue metrics |
| **Management nodes on Cloud Hypervisor share one host user** (`CH_VM_USER`), so a guest that escapes into one node's VMM can read and write the other management nodes' disks and vTPM state on that host (ADR-0099) | They are nodes of one cluster, and one compromised node already reaches what the cluster's workloads reach; a uid per node, as banlieue gives its guests, would add bookkeeping without separating different trust. banlieue's guests remain separate from it (TB-9) | Nodes of more than one cluster, or of different trust, are put on one host as Cloud Hypervisor nodes (the default VMM) |
| **The bootstrap script does not verify SSH host keys** (`StrictHostKeyChecking=no` in `ssh_run`); with `join` that session also carries a join token, and for a controller the cluster CA key's token and the datastore password (A-20) | Every node is new, or rebuilt, when the script first reaches it, so there is no key to pin yet; a token expires in 15 minutes, though a controller's is enough to take the CA key within them. Running it from the host or the nodes' own network keeps it off untrusted paths (§7.23) | The script records each node's host key at creation (the seed can carry one) and checks it afterwards |
| **`stage_k0s_binary` uses the k0s binary unverified when the release's `sha256sums.txt` cannot be fetched**, which `join` and the vSphere backend both rely on | An air-gapped mirror may publish no checksum file, and failing closed would break those estates; the binary is fetched over HTTPS from `K0S_BINARY_BASEURL` | A pinned digest setting (as `IMAGE_SHA256` does for the installer ISO) is added, or the mirror case is handled explicitly |
| Provider condition messages are mirrored verbatim onto user-facing `VirtualMachine` status | Useful diagnostics; providers are in-tree | A third-party provider ships |
| **A compromised VMM can skew `banlieue host cloud-hypervisor overhead`**: naming a thread `vcpu<N>` (or its process `swtpm`) moves its time between columns, so a VMM's own overhead can be reported as guest time | The verb is a diagnostic an operator reads, not an input to scheduling, billing or any control; the guest's total is unchanged, and the slice's own `cpu.stat` (also reported) is kernel-accounted and cannot be renamed. Telling vCPU threads apart without trusting their names needs the VMM's API (`vm.info`), which the installer crate does not speak | Overhead numbers feed an automated decision (placement, billing, alerts) |
| **Capacity is checked at placement, not reserved** (ADR-0107, ADR-0128): VMs scheduled at the same moment can each fit against the same free memory, and a VM can be placed against capacity a provider has not re-published since another VM landed | The controller reconciles one VM at a time per leader and counts every VM whose `status.scheduled` names the failure domain, so the window is between two concurrent passes, not across them; since ADR-0128 the measured figures also subtract VMs scheduled but not yet provisioned, so a burst of placements cannot all fit against the same measured free space; an over-placement still ends at the guests' slice `MemoryMax`, killing a guest rather than the host. A reservation would need a write-ahead record per placement | Concurrent scheduling (more than one controller replica placing at once) is introduced, or an over-placement is observed |
| **Capacity is counted per `Provider`** (ADR-0107): two `Provider`s on one host each see only their own VMs | ADR-0060 makes one host one Provider, and §7.27 says so; nothing in the CH provider's install or status flow creates a second Provider for a host | Several Providers per host become a supported topology |
| **Memory used outside banlieue's VMs is seen late, and only by new placements** (ADR-0107, revised by ADR-0128): the measured `memoryAvailableMiB` catches the host's own services, management nodes and VMs banlieue did not create, but it is refreshed at the Provider reconcile cadence (30 s) and rounded down to a whole GiB, and a VM already placed is never re-checked against it | A placed VM is part of the measurement, so checking it would count it against itself; the reservation check (ADR-0107) and the guests' budget (ADR-0105, §7.26) still bound what guests may have. Publishing faster or unrounded figures would requeue every VirtualMachine on each change | Measured figures are needed between reconciles, or rebalancing placed VMs becomes a goal (ADR-0125 Decision 4) |
| **The QEMU per-VM overhead is an estimate** (ADR-0128): libvirt and Proxmox publish `memoryOverheadMiBPerVM` = 256 MiB (`banlieue_provider_sdk::capacity::QEMU_MEMORY_OVERHEAD_MIB`), not a measured figure | It errs toward leaving room; the measured `memoryAvailableMiB` catches a QEMU that costs more once its VMs run | A sampled figure exists for QEMU, as ADR-0106 produced for Cloud Hypervisor |
| **Two storage classes on one store are checked separately** (ADR-0128): each sees the whole store's `freeGiB`, so together they can fill it | §7.27 says one class per store; detecting shared backing (one filesystem, one pool) across classes needs a store identity the API does not carry | A Provider is found mapping two classes to one store in practice |
| **vSphere capacity is per cluster** (ADR-0128): DRS picks the host, so a VM larger than any single host passes the scheduler and fails in vCenter | The backend reports the failure on the machine; per-host figures would multiply status size and churn | Large VMs on small hosts become common, or DRS is not in use |
| **A `banlieue-api` upload carries no signature or provenance**, unlike the binary and images (cosign, SLSA, ADR-0006), so a consumer cannot verify which build produced it | crates.io offers none to sign with; the upload comes from the tagged checkout in the release run, and `Cargo.lock` checksums pin what a consumer got the first time | crates.io supports artifact attestations or Sigstore signing |
| **`CARGO_REGISTRY_TOKEN` is a long-lived secret** (A-21): if it leaks, it publishes until revoked | The first upload needs a token; scope it to `publish-new` and `publish-update` and, once the name exists, to `banlieue-api` only, with an expiry (ADR-0100 Decision 6) | The crate exists on crates.io and the publish action accepts Trusted Publishing (GitHub OIDC), so the secret can be deleted |

| **A guest with two NICs can forward between its segments**, and banlieue does not check that it does not (ADR-0111) | banlieue provisions the adapters and cannot inspect a guest; the isolation of a segment is a property of the guest that bridges it. §7.28 asks for forwarding off, and the first consumer (the egress gateways, ADR-0109) writes it in its own cloud-config and tests it | A policy can state which `VMClass`es may join an isolated `networkClass`, or the guest's forwarding state is readable from outside (a status field or a scan) |
| **A gateway's `sandbox` address is image content on libvirt and Cloud Hypervisor** (ADR-0109 Decision 6), so a pool range that overlaps it is not detected (TB-18) | Three gateways are three per-image constants; ADR-0083 sees only addresses banlieue manages. The guide and §7.29 say to keep the range clear | A gateway's `sandbox` address is set through banlieue (as ADR-0111 does on vSphere), so ADR-0083 covers it |
| **The egress gateways' hardening is the image author's control** (ADR-0109): forwarding off, a listener on the `sandbox` NIC only, the per-claim allowlist, no inbound on `egress`. banlieue provisions the VM and cannot inspect what runs in it | The software is consumer image content by design (ADR-0108, ADR-0109 Decision 5); a check banlieue made would see a disk, not a policy. mediatore's ADR-0028 and its image tests hold these controls | banlieue gains a way to verify an image's runtime configuration, or the gateway becomes banlieue code (ADR-0097 rules that out) |
| **A `VMClass` edit does not change a VM that already exists** (ADR-0111, as ADR-0024): the NICs a VM was cloned with are the NICs it keeps, so removing an interface from a class does not detach running VMs from that segment | Reconciling a changed NIC list on a running VM is a reconfigure with its own failure modes (a NIC that is in use, a guest that has configured it) and is out of scope of the clone; the operator who retires a segment must rebuild its VMs | A VM's NIC list is reconciled after creation, or `VMClass` becomes immutable once a VM uses it |
| **The pre-lay Job is privileged and runs a guest under QEMU/KVM on a build node** (ADR-0113): a QEMU escape lands in a privileged container on that node | The build namespace already runs privileged kairos-operator builds (row above), so the namespace boundary is unchanged; QEMU runs seccomp-sandboxed, with no network and no TPM, and the guest is the image being built. `--build-node-selector` keeps it on dedicated build nodes | QEMU can run unprivileged in the Job (non-root, a KVM device plugin instead of `privileged`) |
| **The pre-lay image ships the findings Debian has not fixed** (ADR-0113, amendments 2026-11-04): it is a Debian 13 userland (QEMU, OVMF, e2fsprogs, gdisk, util-linux) run in a privileged Job, and its Grype gate (`make grype-prelay`) fails only on a high or critical finding **with** a fix. Built 2026-11-04, it carried 82 high or critical findings, every one `wont-fix` or `not-fixed` in Debian. A Debian fix is also missed while the cached `apt-get install` layer is reused, until the base is re-pinned or the image rebuilt without the cache | No rebuild can remove an unfixed finding, and a gate on them would be switched off. Every finding is in Code Scanning; the Job holds no API token, QEMU runs seccomp-sandboxed with no network and no TPM, and the guest it boots is the image being built. The image's SBOM is published with each release | A slimmer base (QEMU and OVMF without a full Debian userland), the gate is widened to unfixed findings with curated `.vex/prelay/` statements, or Code Scanning shows an unfixed critical in a package the Job reaches from its input |
| **A `VMImage` deleted after one of its vSphere `Provider`s leaves that `Provider`'s zone templates in vCenter** (ADR-0087, Amendment 2026-11-01) | Nothing can reach that vCenter with a deleted `Provider`'s credentials; before the amendment the delete hung forever instead. The workload logs which `Provider`'s templates it left | An operator-side cleanup for orphaned templates exists, or `Provider` deletion learns to wait for its `VMImage` templates |
| **A Cluster API machine on libvirt, vSphere, Proxmox or Azure gets no bootstrap data from banlieue** (ADR-0129 milestone 2): those providers do not hold creation and still re-apply `BootstrapDataReady` from their status snapshot, so the controller serves `CloudHypervisorMachine` only (`app.rs`) rather than race them; such a guest boots without the data and does not join | Availability, not confidentiality, and the state before ADR-0129 for those backends. Cloud Hypervisor, where this was found, holds and is served; the guide lists which backends receive data | Milestone 2 lands for each of those providers (the hold, the condition filter, and the controller turned on for that kind) |
| **Being allowed to `get` a bootstrap config is treated as being allowed to name it** (ADR-0129 Decision 5): the policy cannot know at admission which Secret a config will render, so a principal with `get` on another Machine's not-yet-adopted config, but not on Secrets, could still name it | Configs hold the same material as the data they render (kubeadm files, join settings), so `get` on them is already a credential-grade grant; Cluster API refuses a config another Machine already controls | A bootstrap provider publishes the data Secret's name before adoption, or `get` on bootstrap configs is routinely granted without `get` on Secrets |
| **A hand-written InfraMachine without the `cluster.x-k8s.io/cluster-name` label is not held before a `Machine` adopts it** (ADR-0129 Decision 1): nothing distinguishes it from a standalone machine until adoption, so a provider may create its guest first | Every InfraMachine Cluster API creates carries the label from creation; the guide tells anyone writing one by hand to add it | Cluster API exposes an earlier, unforgeable signal of intent, or a hand-written CAPI machine is seen booting without its data |
| **In a namespace where the controller may read bootstrap Secrets, it may read any Secret there by name** (ADR-0129 Decision 9): the grant is not `resourceNames`-scoped | Bootstrap Secret names are not knowable when the Role is written. What the controller *copies* is bounded by its classification (TB-15) and who may *name* a Secret by admission; requirement 2 says to keep unrelated Secrets out of such namespaces | Per-Machine Roles become practical (ADR-0025's operator-managed design) or the controller stops needing to read Secrets |
| **A join token that expires before the guest boots is not re-delivered** (ADR-0129 Decision 8) | Cluster API treats bootstrap data as one-shot; re-fetching would rewrite a running machine's seed. A `MachineHealthCheck` replaces a Machine that never becomes a Node | A bootstrap provider supports refreshing a Machine's data in place |
| **Serving a label-selected set of namespaces is scope, not tenant isolation** (ADR-0130): the controller does not restrict which `VMClass`, `VMImage` or `Provider` a tenant's pools may reference, apply a quota, or stop one tenant's objects reading another's through the cluster-wide RBAC the platform admin grants | The decision made was *which namespaces are served*; per-tenant admission policy and quota are a separate ADR, because they need their own design (what a team may reference, how a Provider is shared) rather than a flag | A second team shares a cluster with an untrusted team, or the follow-up ADR on `ValidatingAdmissionPolicy` and `ResourceQuota` per tenant lands |
| **In selector mode the controller's `ClusterRole` stays cluster-wide** (ADR-0130): the gate skips reconciles, it does not narrow what the identity can read or write, so a compromised controller pod reaches every namespace's banlieue objects, served or not. A deployment with no flag serves one namespace but is still bound to the same `ClusterRole` | Kubernetes cannot scope a watch by a namespace's labels, so selector mode needs the cluster-wide verbs; narrowing the default install to a namespaced `Role` is separate bootstrap and manifest work | The default install binds a namespaced `Role` when no selector is set, or a way to bind per-namespace Roles from the selector exists |
| **An object whose namespace leaves the selection keeps its last status** (ADR-0130): it is neither updated nor deleted, so a `VirtualMachine` there can show `Ready` after its backend VM has changed | Deleting on un-label would turn a label edit into destroying a team's machines, and finalizers still run on a delete; the startup log names the resolved scope | A condition on served-ness, written once before the controller stops, is acceptable to tenants |
| **A Namespace watch that fails after start leaves the last served set in force** (ADR-0130): the failure is logged and retried with backoff, not reflected in readiness, so a namespace un-labelled during the outage stays served and one newly labelled is not picked up until the watch recovers | Failing closed would stop every tenant's reconciliation on a transient apiserver blip, which is worse than serving a set that was correct a moment ago; the startup probe still refuses a missing grant outright | Readiness or a metric reflects the Namespace watch's state, or a stale set is given a maximum age |
| **A tenant able to create a `Provider` in a served namespace can cause the operator to run a workload there** (ADR-0131): the operator renders a Deployment, ServiceAccount and Roles for any `Provider` in a served namespace, and the selector decides only *where*, not *who* may ask or which `ProviderClass` they may name | Who may create a `Provider`, and which classes it may reference, is the per-tenant RBAC and admission follow-up (ADR-0130 decision 7); until then the platform admin must not grant tenants `create` on `providers` | The follow-up `ValidatingAdmissionPolicy` ADR lands, or a second team shares a cluster with an untrusted team |
| **The operator's `ClusterRole` stays cluster-wide, and three of its reads are never gated** (ADR-0131): the `ProviderClass` consumer count lists `Provider`s in every namespace, the `Deployment` watch is cluster-wide, and in selector mode the `Namespace` list/watch is cluster-wide. All are read-only; the gate narrows reconciles, not what the identity can read | Kubernetes cannot scope a watch by a namespace's labels; the `ProviderClass` count describes a cluster-scoped object; narrowing the `Deployment` watch to the operator's own label is separate mechanical work | The `Deployment` watch is label-filtered, or the count is limited to served namespaces |
| **An operator and a controller with different selectors leave a `VirtualMachine` with no workload** (ADR-0131): a namespace the controller serves but the operator does not has `Provider`s with no workload, so VMs scheduled onto them never become ready, with no condition saying why | The two binaries do not talk to each other (the CRD-only contract), so neither can check the other's flags; the guide says to keep the selectors aligned | A condition on `Provider` for "namespace not served by the operator", or one shared setting for both |
| **A shared provider's blast radius is every namespace it serves, and its `ClusterRole` is wider than that** (ADR-0133): one `Provider` and one provider pod serve many tenants, so a compromised provider reads and patches the machines (rendered `userData` included, §7.1) of every namespace its selector picks, and because RBAC cannot be scoped by label its `ClusterRole` reaches machines in every namespace, served or not; it has no Secret access outside its home namespace | This is the price of keeping the hypervisor credentials out of every tenant namespace, which the alternative (a provider, its credentials and a Role per tenant) gives up; the selector gate and the `ClusterRole` ceiling are auditable in `scope.rs` and `deploy/provider-vsphere/rbac/clusterrole.yaml`, and the controller and operator already hold the same shape (ADR-0130, ADR-0131) | A team that must not share a provider with another is onboarded (give it its own `Provider` in its own namespace, with the credentials that implies), or a per-served-namespace `RoleBinding` is generated by the operator in place of the `ClusterRoleBinding` |
| **A shared Cloud Hypervisor host's token is wider than a single-namespace host's** (ADR-0134, A-11): the shared ClusterRole reaches machines in every namespace, served or not, and a stolen host credential (which sits on a hypervisor, the least-protected place banlieue's credentials live) now carries that reach; it holds no Secret | Opt-in per host, bounded to machines and read-only Namespaces, and the alternative (the operator generating a `RoleBinding` per served namespace and keeping it in step as namespaces are labelled and un-labelled) needs a reconcile loop and Role-create rights the operator does not otherwise need | A host that serves tenants is judged too exposed, or the operator gains per-namespace binding generation for the cluster-resident providers as well (then do both together) |
| **No per-tenant limit on a shared provider** (ADR-0133): the selector says which namespaces a `Provider` serves, not how much of it each may use, so one tenant's pool can take the provider's whole measured capacity (ADR-0107, ADR-0128) | Scope, not isolation, is ADR-0130 Decision 7's standing position; the fit check still refuses a VM that does not fit | A second team shares a provider with an untrusted team, or per-tenant quota is decided |
| **Capacity peers over-count across namespaces** (ADR-0133): `ScheduledPlacement` records the provider's name, not its namespace, so a VM elsewhere naming a same-named `Provider` of its own is counted against the shared one | It can only refuse a placement that would have fit, never admit one that would not | `ScheduledPlacement` records the provider's namespace (the follow-up named in ADR-0133) |
| **A shared `Provider`'s change reaches VMs at the next requeue in single-namespace mode** (ADR-0133): the controller watches `Provider`s cluster-wide only in selector mode | A multi-tenant install uses selector mode; the requeue bounds it | The controller is run with `--namespace` and `--provider-namespace` together in production |
| **The OpenBao side of ADR-0146 Decision 8 is tested on a throwaway OpenBao, not run on a live one, and its audit alert is not built** (TB-22; A-28): the pinned policy, the JWT role (bound subject, a five-minute, one-use, address-bound token with no default policy), the uniqueness check and the role's deletion are the OpenBao repository's `scripts/attestation-ca.sh`. On 2026-11-25 its tests confirmed `allowed_parameters`, `token_num_uses`, `token_bound_cidrs`, the issued subject following the pinned `common_name`, and the reply's `certificate` and `ca_chain`, and found three things the ADR had wrong (the ACL compares a value **and its JSON type**, so the path length is pinned and sent as the string `"0"`; every parameter must be required, or a missing path length defaults to unlimited; the token needs `token_no_default_policy`). A live server may differ in version or configuration; until `init` and a first `host` run there, no host has been onboarded this way | The installer sends exactly the parameters the policy lists, in the types it pins, and `import` checks the answer, so it is safe to run against any OpenBao; the manual route needs no OpenBao policy; `crates/banlieue-host/src/openbao.rs` | The script is run on the live OpenBao and the first host is onboarded (record the result here), and the audit alert exists |
| **Until the role is deleted, a compromised host can log in again and obtain more CAs under its own name** (TB-22; A-28): a JWT role cannot count logins, so each login yields one signing and each CA lives for the pinned lifetime | Bounded by one signing per login, the pin to the host's own name, the address binding, the script's deletion of the role and the audit log; a second CA under the host's own name gives the host nothing it does not already have, beyond another key to revoke | The role cannot be deleted automatically after the first success, or a host is seen logging in after its import |
| **Whoever can sign on the node-attestation mount can mint a CA the bundle trusts for any name, and its root is one global trust anchor** (A-26; TB-22): on this route the bundle holds that root instead of one entry per host, so its compromise is not bounded to a host | The root key is generated inside OpenBao and never leaves it, on a mount of its own (never the SVID root), every signing is audited and the script checks name uniqueness; a self-signed CA per host has no such concentration and stays the default | The mount's operators change, a second identity layer trusts the same root, or the root's custody is questioned |
| **The signer's reply now carries a chain, which extends mediatore ADR-0049 Decision 6's framing** (TB-21; ADR-0146 Decision 7): the chain is public, and a self-signed CA's reply is unchanged | No guest helper exists, so nothing speaks the protocol and nothing can break; the extension is recorded in the ADR | mediatore accepts or rejects the extension, or the helper is written |
| **A pending key stays at rest until its request is signed or removed** (A-27; TB-11): a request that is never signed leaves a key (`0600`, the provider's user alone) in the pending directory | It signs nothing and is the same custody as A-24; `preflight` and `status` report a pending request. An expiry for abandoned requests is an open point in the ADR | A request is seen left for a long time, or the expiry is built |
| **A CA signed upstream expires on its signer's schedule and nothing renews it** (TB-21; ADR-0146 Decision 7): `HostCaExpired` stops new guests, the signer refuses under an expired CA and `preflight` refuses it, so a fleet whose CA was forgotten stops rather than issues certificates that verify nowhere | Renewal is rotation under a new name; automating it would reuse a name or put the upstream's credential on the host, which Decision 8 avoids | Renewal is automated, or an expired CA is seen to stop a fleet |
| **Name uniqueness is checked by the OpenBao onboarding script, not by banlieue, and not at all on the manual route** (A-26; TB-22): the issuer CN is in every agent ID, so two hosts with one name lose Decision 2's confinement | The script lists what the mount has issued and refuses a duplicate, and an open onboarding is not replaced (tested on a throwaway OpenBao, 2026-11-25; a certificate signed by hand with `no_store` would not be seen); the manual route keeps the administrator's check of ADR-0143; mediatore ADR-0050 moves uniqueness to the issuer | The script is seen to miss a duplicate on the live OpenBao, or a duplicate is found in a bundle |
| **The installer trusts the host's trust store for OpenBao's certificate** (TB-22): a host whose store was altered would trust the wrong server | A wrong server still cannot install a CA that fails `import`'s checks (key, name, path length 0, validity), and the host's trust store belongs to the host's root, who can do more than this; there is no flag to skip verification | An operator wants OpenBao's CA pinned per run (a `--attestation-ca-openbao-ca-file` flag) |
| **Azure ownership tags are not protected from the resource group's writers** (A-16; ADR-0158 Decisions 4 and 5): anyone with tag-write on a `Provider`'s resource group can put that `Provider`'s tags and a UID that no live machine has on a VM, NIC or disk banlieue did not create, and the sweep deletes it after its grace period; removing the tags from a banlieue resource orphans it from the sweep. Two management clusters with a same-named `Provider` (`<namespace>.<name>`) in one resource group would each sweep the other's machines. Since ADR-0162 the same holds in the **gallery's** resource group: a writer there can retag, replace or delete a published version, or create one under banlieue's deterministic name with a `VMImage`'s owner tag and the build's digest tag, which banlieue then adopts and boots (A-31) | Tags are the one ownership marker ARM offers on every resource kind, and the identity's role is assigned at one resource group that banlieue owns (§7.36); a writer there can already delete, power or reconfigure anything in it directly, so the sweep gives them nothing they lacked. The sweep is narrow and paced (provider tag, owner kind, 15 min grace, 3 deletes per pass, home namespace only unless serving tenants) and logs every deletion with the resource and owner UID (`reconciler/resources.rs`) | A resource group must be shared with resources or tools banlieue does not own, or with a second management cluster; or Azure offers an ownership marker only the creating identity can write; or the sweep's tag gains a per-cluster identity |
| **Azure keeps every VM's user data** (A-2): `customData` is stored with the VM in Azure's control plane for the VM's life, outside banlieue's custody, where principals with sufficient rights over the VM in the subscription, and Microsoft, can reach it; banlieue cannot encrypt it, because the guest reads it before any key exists | It is Azure's only first-boot data path that needs no metadata address, which ADR-0163 Decision 2 relies on; a hypervisor seed is the same exposure to the hypervisor operator. The machine CR already holds the same bytes (§7.1), and the 64 KiB cap keeps it to bootstrap, not bulk secrets | banlieue gains a delivery path that leaves nothing in the VM model (a one-time fetch the guest authenticates with its vTPM, for example), or an Azure user must carry long-lived secrets in user data |
| **Whoever can mint the Azure provider's ServiceAccount token is the Azure identity, and an ARM token cannot be recalled** (A-29; ADR-0159): pod create or `serviceaccounts/token` in the provider's namespace, the operator, the cluster's ServiceAccount signing key, and whoever can rewrite the published JWKS all yield a token Entra ID accepts; an ARM token already issued stays valid for its lifetime even after the federated credential is removed | It removes every stored cloud key from the cluster: nothing to leak from etcd, a backup or a Secret read, and nothing to rotate. The holders listed are already platform-level (§7.36 f), no banlieue role grants `serviceaccounts/token`, and the identity's reach is one resource group under a custom role | Entra ID offers continuous access evaluation or a shorter token for workload identities; or the provider's namespace must admit pod creation by people who are not platform admins |
| **An Azure member carries no attestation anchor from banlieue, and its `GuestReady` is ARM's report** (ADR-0163, ADR-0165): no EK or AK certificate is published for an `AzureVirtualMachine`, so a claim on an Azure pool cannot be checked with ADR-0049's EK path against anything banlieue mirrored, and readiness is the provisioning state the guest reported to ARM | A Trusted Launch vTPM has an AK certificate and no EK certificate, so the EK path cannot apply as is; ADR-0165 Decision 1 (publish the AK and its chain) is accepted and not built, and node attestation on Azure is the verifier's choice (Decision 3). Readiness is liveness on every backend (ADR-0043 Decision 9) | ADR-0165 Decision 1 is implemented, or a consumer plans sandbox identity on Azure members |
| **Azure keeps each VM's boot-diagnostics serial log** (ADR-0163 Decision 4): whatever the guest prints to its console during boot (firmware, kernel, cloud-init output) is stored by Azure and readable by principals with sufficient rights over the VM | A VM whose provisioning times out cannot be changed afterwards, so without the log a failed first boot is undiagnosable; reading it takes rights over the VM in the subscription, held by the same principals who can already change or delete the VM | An image's first boot must print sensitive material to the console, or Azure offers boot diagnostics under a separate access policy |
| **customData persists inside the guest on `COS_OEM`** (A-2; ADR-0171 Decision 4): `/oem/91_azure_customdata.yaml`, mode 0600, root, survives reboots and Kairos resets, and `COS_OEM` is never encrypted (first-boot sealing refuses it, ADR-0088), so whoever reads the OS disk offline (a snapshot, a copied VHD, a principal with disk export rights) reads the user data | Kairos applies `/oem` on every boot, which is how user data reaches the first active boot after the recovery reset and every boot after; Azure keeps the same bytes with the VM anyway (row above), and the OS disk is reachable only by principals with rights over the VM | customData carries a long-lived credential, or Kairos offers an encrypted or sealed config location that is readable before the first stage |
| **The ready report is plain HTTP to WireServer, and the Azure guest layer is not yet proven on Azure** (ADR-0163, ADR-0171): the channel is unauthenticated by Azure's design, host-local, and the guest only sends three identifiers it was given; the ordering (recovery writes customData, active boot applies it, report after `cos-setup-network.service`) is unit tested, not observed | Azure offers no other ready transport for a VM without its agent; nothing secret crosses the link and nothing received is executed (TB-4). The spike proved the same exchange from shell on real VMs | The first live Azure run of a banlieue-built image (re-walk TB-4's Azure rows against what Azure did), or the helper reading anything from WireServer beyond the goal state's identifiers |
| **The Azure guest image has no image scan gate, and the musl in its binary is scanned by nothing** (ADR-0171, amendment 2026-11-26): `banlieue-azure-guest` runs as root in every Azure image banlieue builds, and its published image (`docker-azure-guest`) is `FROM scratch` with one stripped static binary, so Grype finds no package to match. Its crates (`libc`, `base64`, `thiserror`) are gated on `Cargo.lock` by `cargo audit` (`security`) and `cargo deny`; the musl C library the Rust toolchain links in statically appears in neither, and is only as current as the toolchain the job installs | A gate over an image with nothing to catalogue always passes and would claim a check that never happens. The binary has no TLS, parses only `ovf-env.xml` (bounded, strict) and WireServer's goal state, and talks only to the provisioning disk and the host-local WireServer (TB-4); musl's fixes reach it with each stable Rust release the job builds with, and the image is rebuilt on every push to main | The binary embeds its dependency list (`cargo auditable`) so a scanner can read it, musl gets an advisory feed a gate can use, or the helper gains a network or parsing surface beyond WireServer and the provisioning disk |
| **A provider that runs Jobs can still run a restricted Job as `banlieue-import` in the build namespace** (the cluster-wide Job grant this row used to record is closed: a `Role` in the build namespace, `crates/banlieue-operator/src/workload.rs::build_jobs_role`, and `deploy/admission/provider-jobs.yaml`, TB-3): `banlieue-import` is one ServiceAccount that every `Provider`'s import `Role` trusts, so code running as a compromised vSphere, libvirt or Azure provider can read every `Provider`'s credentials Secret through such a Job, not only its own | The Job is admitted only in the build namespace, only as the import or publisher identity, and only as a restricted pod, so this is no longer node root or any other namespace; provider code is banlieue's own and `Provider` writes are platform-admin-only (§7.5). A per-`Provider` import ServiceAccount would need the operator to render one per `Provider` in the build namespace | Providers of different trust are installed in one cluster; then an import ServiceAccount per `Provider`, rendered by the operator, with the admission policy pinning each provider identity to its own |
| **The runtime and publisher Azure identities are separate against a leaked token, not against a compromised Azure provider** (ADR-0159 Decision 3, ADR-0162 Decision 1): the provider creates the publish Job and chooses its image and arguments, so code running as the provider can run anything as `banlieue-azure-publisher` and act as every publisher identity federated to that subject; the subject is one ServiceAccount for every azure `Provider`, so a `Provider`'s `publisher.clientId` chooses among them | The split still means a stolen runtime ARM token, or the runtime role's assignment, cannot write or delete an image; the publisher's reach is the publisher role at each gallery's resource group, never a VM, network or role assignment; and `Provider` writes are platform-admin-only (§7.5). A per-`Provider` publisher ServiceAccount would need the operator to render one per `Provider` in the build namespace | Azure `Provider`s are written by more than one team, or two publisher identities must not be usable by each other's `Provider`s; then a publisher ServiceAccount per `Provider`, rendered by the operator, with its own federated subject (§7.36 k) |
| **The published version's digest tag records what the Job read and sent, not what Azure stored** (ADR-0162 Decision 6): the Job verifies the source before the upload and hashes the stream as it sends it, but Azure exposes no digest of a managed disk, and a holder of the write SAS could write pages until the Job revokes it; the SAS is a bearer URL valid for up to an hour, and a Job killed mid-upload leaves it valid until it expires | The SAS never leaves the Job's memory, is sent only to the pinned upload domain over verified TLS with no redirects, and is revoked as soon as the upload completes (TB-23); a writer with that SAS, or with disk write in the gallery's group, is already inside the publisher's trust (§4). Reading the disk back through a read SAS would double the transfer for every publish | Azure reports a content hash for a managed disk or snapshot, or a publish must be verifiable by a party that does not trust the publisher's resource group; then hash the snapshot back through a read SAS before the version is created |
| **Image publishing into Azure has not been run against Azure** (ADR-0162): the Job, the uploader, the VHD footer and the declared-state loop are tested over fakes and a local HTTP server only; the runtime provider path has been run live (2026-11-26) | The ADR's mechanics (direct upload, Put Page, version create times) come from the roadmap 23 spike, and the uploader refuses anything the spike did not show (another host, a missing signature, a redirect). A fake more permissive than Azure would hide a fault, as `rules/testing.md` warns | The first live publish: re-walk TB-23 against what Azure actually returned, fix any fake it contradicts in the same change, and remove this row |

## 9. Out of scope

- Anything requiring cluster-admin as a starting position (`SECURITY.md` policy).
- Compromise of the hypervisor itself, or of vCenter/libvirt/Proxmox authorization —
  including root on a Cloud Hypervisor host, which owns every guest on it.
- Guest-OS hardening after boot; banlieue's responsibility ends at delivering
  the bootstrap material.
- What a Cluster API bootstrap provider (kubeadm, k0smotron) renders into a
  Machine's bootstrap data, and the lifetime of the join token in it: upstream
  provides the bootstrap role (ADR-0096 Decision 6). banlieue's share is which
  Secret it copies, into which machine, and who may name it (ADR-0129, TB-15).
- Address conflicts with machines **banlieue did not create**. ADR-0083
  checks `VirtualMachine`s only; the provider-side lookup that would catch
  another writer on the same segment is deferred until it can be verified
  against a real backend.
- `deploy/kind/` — development-only, not held to production standard.
- The MkDocs documentation toolchain (`docs/`), which ships nothing at runtime.
- The Cloud Hypervisor host's **OS packages** (systemd, polkit, swtpm,
  `ca-certificates`): installed and kept current by the host's own package
  manager or image build, never by banlieue (ADR-0084 Decision 1).
  `preflight` checks that the commands exist, not where they came from.
- The agent layer above `VirtualMachineClaim`: `AgentSandbox`, token
  brokering, the in-guest agent and their attestation. They are mediatore's
  (ADR-0097), covered by mediatore's own threat model; banlieue's share ends at
  the claim and the EK certificate it mirrors.
- A **Cluster API** machine that names a platform `Provider`: the admission policy
  deliberately leaves CAPI's manager un-exempted until that is a decision of its
  own (ADR-0133 Decision 10). A Firecracker provider, which does not exist yet (its
  machine kinds are served and nothing acts on them), is to be built shared-capable from its first commit.
- The host's **other tenants' firewall and forwarding policy** (container runtime, kubelet
  networking, `firewalld`), and what a gateway image does with its two NICs (mediatore ADR-0028):
  banlieue's share on a sandbox host is the recipe that takes three chains and disturbs nothing
  else, the class attribute and the port flag (ADR-0144, TB-19, TB-20).
- What a guest does with its certificate, and the identity layer's side of `x509pop`: the in-guest helper, the
  guest's key custody and renewal, the SPIRE agent's configuration, the server's verification, the bundle's
  assembly and the **uniqueness of each CA's name across it** (mediatore ADR-0049, ADR-0050). banlieue's share on
  an opted-in host is the CA and its key, the signer, `hostCa` as asserted configuration and
  `status.attestationSubject` (ADR-0146, TB-21).
- OpenBao itself and its onboarding script for host CAs: the mount's root and its custody, the pinned policy, the
  JWT role, the uniqueness check, the role's deletion and the audit log (the OpenBao repository). banlieue's share is
  the installer's one exchange and the checks on what comes back (ADR-0146 Decisions 7 and 8, TB-22).
- Azure's own control plane and identity service (Resource Manager, Entra ID, Trusted Launch firmware and the
  vTPM it provides), the publication and custody of the cluster's ServiceAccount issuer (discovery document,
  JWKS and signing key; a cluster-level decision, ADR-0159 Decision 2), and the subnet's network security groups,
  routes and firewalls. banlieue's share is the provider's two HTTPS clients, the federated exchange, the custom
  role it ships, the tags it writes and the sweep that reads them (ADR-0158 to ADR-0165, TB-4). For publishing,
  the Compute Gallery itself, its resource group's other writers, and Azure's storage of the uploaded disk are
  Azure's and the admin's; banlieue's share is the publish Job, its uploader and the publisher role (ADR-0162, TB-23).
- Removing banlieue from a Cloud Hypervisor host (`uninstall`, host drain):
  not built (ADR-0067 Decision 8); `banlieue host cloud-hypervisor status` shows what a
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
change to the tenancy assumptions in §7 (namespace scope and shared
providers, ADR-0130 to ADR-0134).

**Auditing the CALM model counts as a trigger.** TB-7 exists because
`architecture.json` gained an `network-oidc-issuer` node and the wires around
it, and the question "is that actor in the threat model?" answered *no* — for
an actor every claim attribution already depended on. The two documents
describe the same system from different angles, so a component that is new in
one is a prompt to check the other.
