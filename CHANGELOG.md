<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Changelog

All notable changes to banlieue, by release. Generated from the commit
history with [git-cliff](https://git-cliff.org/); do not edit by hand, run
`make changelog`.

## [0.6.1](https://github.com/firestoned/banlieue/releases/tag/v0.6.1) (2026-11-27)

### Features

- **cloud-hypervisor:** isolate sandbox taps with bridge port isolation, with installer and preflight support (ADR-0144) ([#242](https://github.com/firestoned/banlieue/pull/242))
- **azure:** AzureVirtualMachine, banlieue provider azure, controller dispatch (ADR-0164, roadmap 23) ([#247](https://github.com/firestoned/banlieue/pull/247))
- **azure:** per-Provider tags, disk encryption set, encryption at host (ADR-0172, roadmap 23) ([#253](https://github.com/firestoned/banlieue/pull/253))
- **azure:** re-place a machine its zone refused, back-off, sweep metrics (ADR-0173, roadmap 23) ([#254](https://github.com/firestoned/banlieue/pull/254))

### Bug fixes

- **cloud-hypervisor:** every guest has an SMBIOS system UUID, its machine uid ([#246](https://github.com/firestoned/banlieue/pull/246))
- **bootstrap:** pass --push-image and --azure-guest-image to the imagebuilder (ADR-0171, ADR-0110) ([#249](https://github.com/firestoned/banlieue/pull/249))
- **host:** send OpenBao max_path_length as a string; align ADR-0146 docs ([#250](https://github.com/firestoned/banlieue/pull/250))
- **controller:** a migrationPolicy recreate converges instead of deleting forever ([#255](https://github.com/firestoned/banlieue/pull/255))
- **host:** send OpenBao max_path_length as a string; align ADR-0146 docs ([#256](https://github.com/firestoned/banlieue/pull/256))

### Documentation

- **adr:** accept the Firecracker and shared-host ADRs (0137, 0138, 0140, 0141, 0142, 0143) ([#243](https://github.com/firestoned/banlieue/pull/243))
- **security:** threat model pass for ADR-0144 (sandbox segment and port isolation) ([#244](https://github.com/firestoned/banlieue/pull/244))
- **adr:** 0166 sandbox addresses are private and unique across the fleet (Proposed) ([#245](https://github.com/firestoned/banlieue/pull/245))
- **adr:** EC2 provider decisions, ADR-0174, 0167, 0175 and 0162 Decision 9 (roadmap 24) ([#257](https://github.com/firestoned/banlieue/pull/257))

### Build and CI

- strip build-host paths from release and helper binaries ([#248](https://github.com/firestoned/banlieue/pull/248))
- **release:** publish the banlieue-azure-guest image and state its digest (ADR-0171, roadmap 23) ([#251](https://github.com/firestoned/banlieue/pull/251))
- **release:** publish the banlieue-azure-guest image and state its digest (ADR-0171, roadmap 23) ([#252](https://github.com/firestoned/banlieue/pull/252))

## [0.6.0](https://github.com/firestoned/banlieue/releases/tag/v0.6.0) (2026-11-25)

### Features

- feat/adr 0139 firecracker adrs ([#209](https://github.com/firestoned/banlieue/pull/209))
- **controller:** mirror attestationSubject machine to VM to claim; ADR-0143 Decision 7 (HostCaMissing) ([#214](https://github.com/firestoned/banlieue/pull/214))
- **controller:** audit the VirtualMachine controller's progress (ADR-0145) and keep status.scheduledAt while the placement is unchanged ([#215](https://github.com/firestoned/banlieue/pull/215))
- **firecracker:** CALM model, microvm OCI client, attestation signer, policy harnesses (ADR-0137 to ADR-0143) ([#224](https://github.com/firestoned/banlieue/pull/224))
- feat/adr 0146 host ca csr openbao ([#232](https://github.com/firestoned/banlieue/pull/232))
- **admission:** referencing a Provider credential is its own permission (ADR-0157) ([#231](https://github.com/firestoned/banlieue/pull/231))
- **azure:** banlieue-azure ARM client core (ADR-0164, roadmap 23) ([#236](https://github.com/firestoned/banlieue/pull/236))
- **azure:** ARM HTTP client, federated token source, live tier (ADR-0164, roadmap 23) ([#238](https://github.com/firestoned/banlieue/pull/238))

### Bug fixes

- **host:** install honours the host config it keeps (ADR-0067) ([#205](https://github.com/firestoned/banlieue/pull/205))
- **cloud-hypervisor:** provider capacity honours CPUQuota on cgroup v1 hosts (ADR-0138, ADR-0139) ([#207](https://github.com/firestoned/banlieue/pull/207))
- **controller:** finishing a bind keeps the Spread message (ADR-0135) ([#212](https://github.com/firestoned/banlieue/pull/212))
- **ci:** a prerelease gets release notes ([#219](https://github.com/firestoned/banlieue/pull/219))
- **ci:** rebuild the pre-lay apt layer daily (ADR-0113) ([#233](https://github.com/firestoned/banlieue/pull/233))
- **controller,provider-cloud-hypervisor:** a placed tenant VM keeps its host, and a host never reuses a uid it holds ([#234](https://github.com/firestoned/banlieue/pull/234))
- **deps:** kube 5.2 for RUSTSEC-2026-0600, MSRV 1.95 ([#239](https://github.com/firestoned/banlieue/pull/239))
- **ch:** report a lost vTPM state as TpmStateLost and stop the 5 s retry ([#241](https://github.com/firestoned/banlieue/pull/241))

### Performance

- **vsphere:** box vCenter futures so Send proofs stay cached ([#206](https://github.com/firestoned/banlieue/pull/206))

### Documentation

- **adr:** ADR-0136 Cloud Hypervisor guests get whole PCI devices through VFIO (Proposed, roadmap 21) ([#202](https://github.com/firestoned/banlieue/pull/202))
- **adr:** 0144 sandbox segment and egress gateway VMs on a Cloud Hypervisor host (Proposed) ([#208](https://github.com/firestoned/banlieue/pull/208))
- **adr:** accept ADR-0144 and model the sandbox segment in CALM ([#210](https://github.com/firestoned/banlieue/pull/210))
- **roadmap:** 22 to 25, public-cloud providers and cloud images ([#213](https://github.com/firestoned/banlieue/pull/213))
- **adr:** propose ADR-0157, referencing credentials is its own permission ([#216](https://github.com/firestoned/banlieue/pull/216))
- **roadmap:** 22 to 25 reserve ADR-0158 to ADR-0169 ([#217](https://github.com/firestoned/banlieue/pull/217))
- **adr:** 0146 Cloud Hypervisor guests may attest with x509pop (Proposed) ([#218](https://github.com/firestoned/banlieue/pull/218))
- **security:** resolve the threat model conflict markers from #231 ([#235](https://github.com/firestoned/banlieue/pull/235))
- **security:** threat model pass for ADR-0145 and Firecracker code (roadmap 20) ([#237](https://github.com/firestoned/banlieue/pull/237))

## [0.5.10](https://github.com/firestoned/banlieue/releases/tag/v0.5.10) (2026-11-17)

### Features

- **provider:** shared vSphere and Cloud Hypervisor providers serve tenant namespaces (ADR-0133, ADR-0134) ([#203](https://github.com/firestoned/banlieue/pull/203))
- **claim:** a claim chooses its member by strategy and host health (ADR-0135, roadmap 17) ([#204](https://github.com/firestoned/banlieue/pull/204))

## [0.5.9](https://github.com/firestoned/banlieue/releases/tag/v0.5.9) (2026-11-16)

### Features

- **claim:** a VirtualMachineClaim may select its pool by label (ADR-0132, roadmap 17) ([#200](https://github.com/firestoned/banlieue/pull/200))

### Bug fixes

- **cloud-hypervisor:** read-only /sys and /proc for the provider (ADR-0063) ([#201](https://github.com/firestoned/banlieue/pull/201))

### Other

- **cloud-hypervisor:** harden the provider unit (read-only /dev, tun device only) ([#199](https://github.com/firestoned/banlieue/pull/199))

## [0.5.8](https://github.com/firestoned/banlieue/releases/tag/v0.5.8) (2026-11-15)

### Features

- **controller:** serve namespaces by label selector, never all by default (ADR-0129) ([#193](https://github.com/firestoned/banlieue/pull/193))
- **operator,providers:** serve namespaces by label selector, never all by default (ADR-0131) ([#196](https://github.com/firestoned/banlieue/pull/196))
- **admission:** only the controller writes CH machine move fields (ADR-0127, roadmap 14) ([#198](https://github.com/firestoned/banlieue/pull/198))

### Bug fixes

- **crds:** strip schema formats the apiserver does not recognise ([#184](https://github.com/firestoned/banlieue/pull/184))
- **controller:** reconcile on real served-set changes only, warn on implicit and broad scope (ADR-0130) ([#195](https://github.com/firestoned/banlieue/pull/195))
- **controller:** a VM finds its machine through status.infrastructureRef (ADR-0127, roadmap 14) ([#197](https://github.com/firestoned/banlieue/pull/197))

### Other

- tweaks and refinements for install on different setup ([#189](https://github.com/firestoned/banlieue/pull/189))
- adr 123 - capi machine changes ([#192](https://github.com/firestoned/banlieue/pull/192))
- ADR-0129 impl ([#194](https://github.com/firestoned/banlieue/pull/194))

## [0.5.7](https://github.com/firestoned/banlieue/releases/tag/v0.5.7) (2026-11-12)

### Features

- **scheduler:** every provider publishes measured capacity (ADR-0128, roadmap 03) ([#180](https://github.com/firestoned/banlieue/pull/180))
- **api:** VirtualMachineMove and the CH machine move fields (ADR-0127, roadmap 14) ([#182](https://github.com/firestoned/banlieue/pull/182))

### Bug fixes

- **scheduler:** count hardwareOverride CPUs and memory (ADR-0107 amended) ([#183](https://github.com/firestoned/banlieue/pull/183))

### Documentation

- **guides:** capacity-aware scheduling, and how to test it (ADR-0128) ([#181](https://github.com/firestoned/banlieue/pull/181))

## [0.5.6](https://github.com/firestoned/banlieue/releases/tag/v0.5.6) (2026-11-10)

### Bug fixes

- **scheduler:** a placed VM keeps a placement that still fits (ADR-0125) ([#178](https://github.com/firestoned/banlieue/pull/178))

### Documentation

- **adr:** propose moving a CH VM between hosts in a zone (ADR-0127, roadmap 14) ([#179](https://github.com/firestoned/banlieue/pull/179))

## [0.5.5](https://github.com/firestoned/banlieue/releases/tag/v0.5.5) (2026-11-09)

### Features

- **operator:** a Provider status publishes its ServiceAccount name (ADR-0126) ([#177](https://github.com/firestoned/banlieue/pull/177))

## [0.5.4](https://github.com/firestoned/banlieue/releases/tag/v0.5.4) (2026-11-09)

### Bug fixes

- **scheduler:** a VM only cordoned Providers could take says so, naming them (ADR-0124 amended) ([#176](https://github.com/firestoned/banlieue/pull/176))

### Documentation

- probe only the datasource the backend uses (roadmap 18) ([#174](https://github.com/firestoned/banlieue/pull/174))

### Other

- **wolf:** refresh the OpenWolf anatomy index ([#175](https://github.com/firestoned/banlieue/pull/175))

## [0.5.3](https://github.com/firestoned/banlieue/releases/tag/v0.5.3) (2026-11-09)

### Features

- **scheduler:** a Provider labelled unschedulable takes no new VM (ADR-0124) ([#173](https://github.com/firestoned/banlieue/pull/173))

### Documentation

- **roadmap:** libvirt re-measured with the fast guest poll (roadmap 18) ([#172](https://github.com/firestoned/banlieue/pull/172))

## [0.5.2](https://github.com/firestoned/banlieue/releases/tag/v0.5.2) (2026-11-08)

### Features

- **libvirt:** poll a booting guest every 2 s for its first 90 s (ADR-0043 amended) ([#170](https://github.com/firestoned/banlieue/pull/170)) ([#171](https://github.com/firestoned/banlieue/pull/171))

### Documentation

- **adr:** reject ADR-0123, no provider-side duplicate-address probe ([#165](https://github.com/firestoned/banlieue/pull/165))
- **adr:** ADR-0109 allowed request through a gateway has run ([#166](https://github.com/firestoned/banlieue/pull/166))
- **roadmap:** sealed members to GuestReady in 25 s and 21.5 s on XFS (roadmap 18) ([#167](https://github.com/firestoned/banlieue/pull/167))
- **roadmap:** libvirt sealed members and the guest boot profile (roadmap 18) ([#169](https://github.com/firestoned/banlieue/pull/169))

## [0.5.1](https://github.com/firestoned/banlieue/releases/tag/v0.5.1) (2026-11-08)

### Features

- feat/ch reflink attribute adr 0123 ([#160](https://github.com/firestoned/banlieue/pull/160))
- **scripts:** ch-storage-to-xfs.sh moves a dedicated CH storage partition to XFS ([#161](https://github.com/firestoned/banlieue/pull/161))
- **scripts:** ch-storage-swap-to-xfs.sh turns a swap partition into XFS CH storage ([#162](https://github.com/firestoned/banlieue/pull/162))

### Bug fixes

- **libvirt:** hold an ARP-observed address while the domain runs ([#155](https://github.com/firestoned/banlieue/pull/155))
- **scripts:** reload systemd after each fstab edit; all CH hosts on XFS (roadmap 18) ([#163](https://github.com/firestoned/banlieue/pull/163))
- **prelay:** apply Debian security updates at build (zlib CVEs) ([#164](https://github.com/firestoned/banlieue/pull/164))

### Documentation

- **libvirt:** why the ARP source sees nothing on a host with many neighbours ([#156](https://github.com/firestoned/banlieue/pull/156))
- **roadmap:** the firmware 4 to 8 s wait does not reproduce (roadmap 18) ([#158](https://github.com/firestoned/banlieue/pull/158))
- **roadmap:** the firmware 4 to 8 s was the clone writeback (roadmap 18) ([#159](https://github.com/firestoned/banlieue/pull/159))

## [0.5.0](https://github.com/firestoned/banlieue/releases/tag/v0.5.0) (2026-11-07)

### Bug fixes

- **release:** clusterctl metadata gains the 0.5 release series ([#154](https://github.com/firestoned/banlieue/pull/154))

### Documentation

- **roadmap:** add roadmap 20, Firecracker provider for sandbox pools ([#149](https://github.com/firestoned/banlieue/pull/149))

### Tests

- **isolation:** record Kairos's own unlock failure on the console (roadmap 18 phase B) ([#152](https://github.com/firestoned/banlieue/pull/152))

### Build and CI

- crate-package builds banlieue-api against Kubernetes 1.37, as the workspace does ([#153](https://github.com/firestoned/banlieue/pull/153))

## [0.4.14](https://github.com/firestoned/banlieue/releases/tag/v0.4.14) (2026-11-07)

### Features

- **providers:** a static machine reports its declared address when nothing observes one (ADR-0114 amended) ([#144](https://github.com/firestoned/banlieue/pull/144))

### Bug fixes

- **prelay:** re-pin debian:13-slim for apt 3.0.4 (CVE-2026-109518) ([#143](https://github.com/firestoned/banlieue/pull/143))

### Documentation

- **adr:** accept ADR-0108; amend ADR-0049 (SPIRE attestation, status.nonce retired) ([#142](https://github.com/firestoned/banlieue/pull/142))

## [0.4.13](https://github.com/firestoned/banlieue/releases/tag/v0.4.13) (2026-11-05)

### Features

- **imagebuilder:** rebuild an artifact whose node-local volume was lost (ADR-0114) ([#139](https://github.com/firestoned/banlieue/pull/139))

### Bug fixes

- **cloudinit:** every NoCloud seed carries user-data; cloud-init ignored seeds without it (ADR-0054 amended) ([#140](https://github.com/firestoned/banlieue/pull/140))
- **vsphere:** decide from a live read, and recover a lost vmRef by the marker (ADR-0101 amended) ([#141](https://github.com/firestoned/banlieue/pull/141))

## [0.4.12](https://github.com/firestoned/banlieue/releases/tag/v0.4.12) (2026-11-04)

### Bug fixes

- **providers:** static addressing reaches libvirt and Cloud Hypervisor guests through the NoCloud seed (ADR-0114); ADR-0109 amended ([#137](https://github.com/firestoned/banlieue/pull/137))

### Documentation

- Kairos static addressing verified live (ADR-0114 Decision 4) ([#138](https://github.com/firestoned/banlieue/pull/138))

## [0.4.11](https://github.com/firestoned/banlieue/releases/tag/v0.4.11) (2026-11-04)

### Features

- publish the pre-lay image with its own Grype gate; libvirt sealing-isolation e2e (ADR-0113 amended; roadmap 18 phase B) ([#136](https://github.com/firestoned/banlieue/pull/136))

### Bug fixes

- **vsphere:** a by-name VM lookup no longer fails on a vTPM VM ([#134](https://github.com/firestoned/banlieue/pull/134))
- **vsphere:** force-reimport is handled per Provider and cleared by the last (ADR-0087, amended) ([#135](https://github.com/firestoned/banlieue/pull/135))

### Documentation

- **adr:** accept ADR-0113; fix the pre-lay image build and partition nodes in containers ([#133](https://github.com/firestoned/banlieue/pull/133))

### Other

- scrub real host names; add an automated real-infrastructure check ([#132](https://github.com/firestoned/banlieue/pull/132))

## [0.4.10](https://github.com/firestoned/banlieue/releases/tag/v0.4.10) (2026-11-01)

### Bug fixes

- **providers:** a workload for one Provider writes one VMImage row; per-Provider vSphere deletion (ADR-0087, amended) ([#131](https://github.com/firestoned/banlieue/pull/131))

## [0.4.9](https://github.com/firestoned/banlieue/releases/tag/v0.4.9) (2026-11-01)

### Features

- **imagebuilder:** pre-lay Url images (ADR-0113); fix libvirt BackingFile readiness ([#130](https://github.com/firestoned/banlieue/pull/130))

### Documentation

- **adr:** ADR-0113 Open question 1, COS_PERSISTENT does not grow; pre-lay at the class disk size ([#129](https://github.com/firestoned/banlieue/pull/129))

### Other

- roadmap18 phase c ([#128](https://github.com/firestoned/banlieue/pull/128))

## [0.4.8](https://github.com/firestoned/banlieue/releases/tag/v0.4.8) (2026-10-31)

### Features

- **vsphere:** a cloned VM gets one NIC per declared interface (ADR-0111) ([#124](https://github.com/firestoned/banlieue/pull/124))
- **cloud-hypervisor:** first-boot sealing, read back over vsock (ADR-0110, accepted) ([#123](https://github.com/firestoned/banlieue/pull/123))

### Tests

- **cloud-hypervisor:** a sealed disk opens only with its own vTPM; rewrite roadmap 18 ([#127](https://github.com/firestoned/banlieue/pull/127))

## [0.4.7](https://github.com/firestoned/banlieue/releases/tag/v0.4.7) (2026-10-30)

### Features

- **imagebuilder:** the registry push Job trusts an operator-named CA bundle (ADR-0110) ([#122](https://github.com/firestoned/banlieue/pull/122))

### Documentation

- **adr:** accept ADR-0088, first-boot sealing on by default; bump Kairos to v4.4.0
- ADR-0088 verified on libvirt; renumber duplicate ADRs 0100/0103 ([#121](https://github.com/firestoned/banlieue/pull/121))

## [0.4.6](https://github.com/firestoned/banlieue/releases/tag/v0.4.6) (2026-10-26)

### Features

- **vsphere:** resolve Template-kind VMImages per zone; never destroy adopted templates (ADR-0104) ([#114](https://github.com/firestoned/banlieue/pull/114))
- first-boot sealing for Immediate tpmEnabled images, behind --enable-first-boot-sealing (ADR-0088) ([#113](https://github.com/firestoned/banlieue/pull/113))
- **host:** VMs run in banlieue.slice; banlieue host cloud-hypervisor overhead (ADR-0104) ([#115](https://github.com/firestoned/banlieue/pull/115))

### Documentation

- ADR-0104 per-zone vSphere Template images; pin kairos-operator install ([#111](https://github.com/firestoned/banlieue/pull/111))

### Other

- clone-to-TPM-endorsement sequence confirms tpm_endorsement_certificates as the first typed config read on a vTPM-enabled VM ([#112](https://github.com/firestoned/banlieue/pull/112))
- capacity aware scheduling ([#120](https://github.com/firestoned/banlieue/pull/120))

## [0.4.5](https://github.com/firestoned/banlieue/releases/tag/v0.4.5) (2026-10-21)

### Features

- **bootstrap:** --image-digest pins every installed image; releases publish digest-pinned manifests (ADR-0103); internal crates publish = false ([#110](https://github.com/firestoned/banlieue/pull/110))

### Build and CI

- **release:** publish install.yaml, crds.yaml, admission and network policy bundles; cover manifests in SLSA provenance ([#109](https://github.com/firestoned/banlieue/pull/109))

## [0.4.4](https://github.com/firestoned/banlieue/releases/tag/v0.4.4) (2026-10-19)

### Features

- **vsphere:** ownership marker and recovery of a lost clone (ADR-0101) ([#103](https://github.com/firestoned/banlieue/pull/103))
- provider liveness from its Lease (ADR-0102); narrow Lease RBAC to each role's own Lease ([#104](https://github.com/firestoned/banlieue/pull/104))

### Bug fixes

- **deps:** rustls 0.23.46 ([#102](https://github.com/firestoned/banlieue/pull/102))

### Documentation

- roadmap statuses reconciled; publish banlieue-api to crates.io (ADR-0100); fix(vsphere): roll back a clone whose provisioning fails ([#101](https://github.com/firestoned/banlieue/pull/101))
- provider contract gains own-machines, Lease liveness and ownership-marker obligations ([#105](https://github.com/firestoned/banlieue/pull/105))

### Other

- Full live e2e testing of pool claim ([#106](https://github.com/firestoned/banlieue/pull/106))

## [0.4.3](https://github.com/firestoned/banlieue/releases/tag/v0.4.3) (2026-10-13)

### Features

- spread placement across failure domains (ADR-0098); per-issuer claim prefixes (ADR-0047); Deferred libvirt e2e ([#94](https://github.com/firestoned/banlieue/pull/94))
- **bootstrap:** Cloud Hypervisor nodes and a worker-only join (ADR-0099) ([#95](https://github.com/firestoned/banlieue/pull/95))
- **bootstrap:** Cloud Hypervisor nodes and a worker-only join (ADR-0099) ([#97](https://github.com/firestoned/banlieue/pull/97))
- **bootstrap:** Cloud Hypervisor is the default VMM; refuse a node whose VMM disagrees with the host (ADR-0099) ([#100](https://github.com/firestoned/banlieue/pull/100))

### Bug fixes

- **providers:** serve only own Provider's machines; libvirt cleans up a machine deleted mid-create ([#96](https://github.com/firestoned/banlieue/pull/96))
- **api:** banlieue-api depends on kube with only the derive feature ([#98](https://github.com/firestoned/banlieue/pull/98))
- **providers:** vSphere and Proxmox clean up a machine deleted mid-create ([#99](https://github.com/firestoned/banlieue/pull/99))

### Other

- Update anatomy
- Update anatomy

## [0.4.2](https://github.com/firestoned/banlieue/releases/tag/v0.4.2) (2026-10-12)

### Bug fixes

- **ci:** release build really stamps the tag's version into Cargo.toml ([#90](https://github.com/firestoned/banlieue/pull/90))
- **libvirt:** boot the disk before the installer; Deferred GuestReady pools verified on libvirt and Cloud Hypervisor ([#93](https://github.com/firestoned/banlieue/pull/93))

### Documentation

- root-cause the Cloud Hypervisor vTPM fault; pool timing harness (make ch-pool-timings) ([#91](https://github.com/firestoned/banlieue/pull/91))
- **adr:** API ends at VirtualMachineClaim; AgentSandbox moves to mediatore (ADR-0097) ([#92](https://github.com/firestoned/banlieue/pull/92))

## [0.4.1](https://github.com/firestoned/banlieue/releases/tag/v0.4.1) (2026-10-10)

### Features

- **vsphere:** VSphereMachine custom attributes Template/CreatedBy/CreatedAt (ADR-0089) ([#82](https://github.com/firestoned/banlieue/pull/82))
- FINOS readiness (roadmap 12) and docs site close-out (roadmap 08); ADR-0091 to ADR-0096 ([#86](https://github.com/firestoned/banlieue/pull/86))

### Bug fixes

- **scripts:** libvirt pool root picks the roomiest mount again ([#85](https://github.com/firestoned/banlieue/pull/85))
- **cloud-hypervisor,bootstrap:** vTPM adopted mid-manufacture; controller user-data Role and claim patch RBAC; roadmap 17 phase G verified on two hosts ([#89](https://github.com/firestoned/banlieue/pull/89))

### Documentation

- pin manifests, examples and guides to v0.4.0 ([#83](https://github.com/firestoned/banlieue/pull/83))
- AgentSandbox CALM model, ADR-0088 correction, close roadmap 11 ([#84](https://github.com/firestoned/banlieue/pull/84))

### Other

- Update anatomy

## [0.4.0](https://github.com/firestoned/banlieue/releases/tag/v0.4.0) (2026-10-06)

### Features

- **host:** banlieue host cloud-hypervisor: VMM from any URL and version, no packages (ADR-0084) ([#79](https://github.com/firestoned/banlieue/pull/79))
- **bootstrap:** control plane VIP and required tailscale for the management cluster (ADR-0086) ([#80](https://github.com/firestoned/banlieue/pull/80))

### Documentation

- **adr:** propose ADR-0088, first-boot sealing for Immediate images ([#81](https://github.com/firestoned/banlieue/pull/81))

### Other

- Fix the build to prefix versions with "v" and update all docs with v0.3.0 ([#75](https://github.com/firestoned/banlieue/pull/75))

## [0.3.0](https://github.com/firestoned/banlieue/releases/tag/v0.3.0) (2026-10-02)

### Features

- **libvirt:** LibvirtMachine contract — CRD, domain procedures, XML builder (ADR-0050); reconcile roadmaps with the tree ([#46](https://github.com/firestoned/banlieue/pull/46))
- **sandboxes:** roadmap 17 phase A3 and A4 — ADR-0048, ADR-0044 ([#50](https://github.com/firestoned/banlieue/pull/50))
- **vsphere:** ADR-0043 GuestReady via guestinfo, verified live end-to-end ([#51](https://github.com/firestoned/banlieue/pull/51))
- **sandboxes:** roadmap 17 phase A5 — vTPM EK certificate (ADR-0045) ([#53](https://github.com/firestoned/banlieue/pull/53))
- **pool:** MetalLB-style address pool for VirtualMachinePool (ADR-0056) ([#55](https://github.com/firestoned/banlieue/pull/55))
- **cloud-hypervisor:** VMM client crate and controller dispatch ([#61](https://github.com/firestoned/banlieue/pull/61))
- **cloud-hypervisor:** host-resident provider, verified end to end ([#63](https://github.com/firestoned/banlieue/pull/63))
- **host:** banlieue host prepares a Cloud Hypervisor host from the binary (ADR-0067); roadmap 09 done ([#66](https://github.com/firestoned/banlieue/pull/66))
- **pool:** roadmap 17 rollout+sabotage e2e green on libvirt; fix ADR-0043 D8 agent-bootstrap grace ([#67](https://github.com/firestoned/banlieue/pull/67))
- **proxmox:** Proxmox VE provider (ADR-0074, ADR-0075) ([#71](https://github.com/firestoned/banlieue/pull/71))

### Bug fixes

- **cloud-hypervisor:** atomic guest uid allocation, stop waits for its job; pool/claim e2e ([#68](https://github.com/firestoned/banlieue/pull/68))

### Documentation

- **security:** threat model full pass ([#48](https://github.com/firestoned/banlieue/pull/48))
- sandbox-identity entry point linking to mediatore (ADR-0005 topology) ([#62](https://github.com/firestoned/banlieue/pull/62))
- **roadmap:** phase 10, host install as a banlieue host subcommand ([#64](https://github.com/firestoned/banlieue/pull/64))
- **roadmap:** 19, banlieue-managed port groups (VMNetwork) ([#69](https://github.com/firestoned/banlieue/pull/69))

### Other

- Bump distroless/cc-debian13 in the base-images group across 1 directory ([#43](https://github.com/firestoned/banlieue/pull/43))
- Bump the actions group with 3 updates ([#45](https://github.com/firestoned/banlieue/pull/45))
- Bump reqwest from 0.13.4 to 0.13.5 in the rust-dependencies group ([#44](https://github.com/firestoned/banlieue/pull/44))
- Add support for VirtualMachineClaim and re-work all of the e2e tests to be independent so we can run them all in parallel in GH workflows ([#47](https://github.com/firestoned/banlieue/pull/47))
- Add roadmap 18: split-image fast clone (verified base + per-VM sealed volume) ([#54](https://github.com/firestoned/banlieue/pull/54))
- Bump the base-images group across 2 directories with 2 updates ([#56](https://github.com/firestoned/banlieue/pull/56))
- Bump the actions group with 4 updates ([#60](https://github.com/firestoned/banlieue/pull/60))
- Bump the rust-dependencies group with 4 updates ([#59](https://github.com/firestoned/banlieue/pull/59))
- Bump pymdown-extensions from 11.0.2 to 12.0.1 in /docs ([#57](https://github.com/firestoned/banlieue/pull/57))
- Bump mkdocs-git-revision-date-localized-plugin in /docs ([#58](https://github.com/firestoned/banlieue/pull/58))
- Docs/adr 0081 0082 sandbox security ([#73](https://github.com/firestoned/banlieue/pull/73))
- block vm creation with dupe ip - crd / kube only (ADR-0083) ([#72](https://github.com/firestoned/banlieue/pull/72))

## [0.2.0](https://github.com/firestoned/banlieue/releases/tag/v0.2.0) (2026-09-17)

### Other

- Bump chainguard/glibc-dynamic ([#41](https://github.com/firestoned/banlieue/pull/41))
- Add initial threat model ([#39](https://github.com/firestoned/banlieue/pull/39))
- New helper bootstrap scripts for libvirt and vsphere and update docs ([#40](https://github.com/firestoned/banlieue/pull/40))
- Add support for trusted boot in vmimage and thus OSArtifact ([#42](https://github.com/firestoned/banlieue/pull/42))

## [0.1.0](https://github.com/firestoned/banlieue/releases/tag/v0.1.0) (2026-09-08)

### Bug fixes

- **supply-chain:** make base images visible to Dependabot; unstick auto-merge ([#38](https://github.com/firestoned/banlieue/pull/38))

### Other

- Initial commit
- Initial commit of banlieue ([#1](https://github.com/firestoned/banlieue/pull/1))
- Initial controller PR ([#2](https://github.com/firestoned/banlieue/pull/2))
- Massive phase 2 changes that adds support for vSphere controller and a single binay ([#3](https://github.com/firestoned/banlieue/pull/3))
- Add a patch for vim_rs for now, locally and part of the build, as we want to use rustls ([#4](https://github.com/firestoned/banlieue/pull/4))
- vim_rs is now fixed and no patch is required ([#5](https://github.com/firestoned/banlieue/pull/5))
- Add new crate "banlieue-imagebuilder" and a comprehensive script that bootstraps a k0s based cluster in libvirt environment ([#6](https://github.com/firestoned/banlieue/pull/6))
- Add libvirt provider and reconciler and added e2e testing ([#7](https://github.com/firestoned/banlieue/pull/7))
- Add libvirt provider and reconciler and added e2e testing ([#8](https://github.com/firestoned/banlieue/pull/8))
- Add vSphere import and createvm template task job ([#9](https://github.com/firestoned/banlieue/pull/9))
- Implemented fully parameterized template, including fields: cpus, memoryMib, firmware, networkAdapter, nicPciSlot, guestId, plus the earlier network / ([#10](https://github.com/firestoned/banlieue/pull/10))
- Implemented fully parameterized template, including fields: cpus, memoryMib, firmware, networkAdapter, nicPciSlot, guestId, plus the earlier network / ([#10](https://github.com/firestoned/banlieue/pull/10)) ([#11](https://github.com/firestoned/banlieue/pull/11))
- Add support for VM image importing and uploading to vsphere, while auto managing a kairos install ([#20](https://github.com/firestoned/banlieue/pull/20))
- Bump the actions group with 20 updates ([#19](https://github.com/firestoned/banlieue/pull/19))
- Bump mkdocs-git-revision-date-localized-plugin in /docs ([#18](https://github.com/firestoned/banlieue/pull/18))
- Bump mkdocs-material from 9.7.6 to 9.7.7 in /docs ([#15](https://github.com/firestoned/banlieue/pull/15))
- VirtualMachine's controller watches VMClass (filtered by name) and Provider ([#21](https://github.com/firestoned/banlieue/pull/21))
- Add support for VirtualMachine.spec.userData ([#22](https://github.com/firestoned/banlieue/pull/22))
- Add dependabot auto-merge option and group PRs by type ([#30](https://github.com/firestoned/banlieue/pull/30))
- Bump the rust-dependencies group with 2 updates ([#31](https://github.com/firestoned/banlieue/pull/31))
- Add support for TPM enablement ([#28](https://github.com/firestoned/banlieue/pull/28))
- Update cargo deps ([#34](https://github.com/firestoned/banlieue/pull/34))
- Bundle open Dependabot updates (kube 4.2, docs pip, actions) ([#33](https://github.com/firestoned/banlieue/pull/33))
- Fix grype issues ([#35](https://github.com/firestoned/banlieue/pull/35))
- Fix the failing SLSA Provenance / final ([#36](https://github.com/firestoned/banlieue/pull/36))
- Update digest for chainguard and all of the code security openings ([#37](https://github.com/firestoned/banlieue/pull/37))


