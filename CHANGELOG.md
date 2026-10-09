<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Changelog

All notable changes to banlieue, by release. Generated from the commit
history with [git-cliff](https://git-cliff.org/); do not edit by hand, run
`make changelog`.

## Unreleased

### Features

- **vsphere:** VSphereMachine custom attributes Template/CreatedBy/CreatedAt (ADR-0089) ([#82](https://github.com/firestoned/banlieue/pull/82))

### Bug fixes

- **scripts:** libvirt pool root picks the roomiest mount again ([#85](https://github.com/firestoned/banlieue/pull/85))

### Documentation

- pin manifests, examples and guides to v0.4.0 ([#83](https://github.com/firestoned/banlieue/pull/83))
- AgentSandbox CALM model, ADR-0088 correction, close roadmap 11 ([#84](https://github.com/firestoned/banlieue/pull/84))

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

## [0.6.4](https://github.com/firestoned/banlieue/releases/tag/v0.6.4) (2026-09-08)

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


