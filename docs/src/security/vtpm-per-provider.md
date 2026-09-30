<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# vTPM per Provider

The guarantee: every `tpmEnabled` VM gets a **new** vTPM of its own, with
its own endorsement key, and installs (and seals its disk to that vTPM) at
its own first boot. No vTPM state, identity or disk-encryption material is
copied from a template or from another VM (ADR-0040, ADR-0048, ADR-0081).

This page says how each provider does that today, and marks what is
verified by an automated live test, by a recorded manual run, by unit tests
only, or not at all.

## The controller gate (all providers)

`tpmEnabled: true` on a `VMClass` requires a `VMImage` whose install mode is
not `Immediate` (ADR-0048). An `Immediate` image is installed once into a
template, so every clone would share one sealed disk.

- Enforced in `crates/banlieue-controller/src/reconciler/virtualmachine.rs`
  (`image_class_mismatch`), before scheduling. An image with no install
  template (a `Template` or `BackingFile` source) counts as `Immediate` and
  is rejected.
- `Manual` is accepted. It is an operator's assertion that the image
  installs per VM, and banlieue cannot check it (ADR-0048).
- No admission policy enforces this. Admission cannot see the `VMClass` and
  the `VMImage` together, so the check is in the controller. **Unit tests
  only** (`virtualmachine_tests.rs`).

With `installMode: Deferred`, the image is never installed into a shared
template. Each VM boots the installer on an empty OS disk, and the installer
encrypts that disk to the VM's own vTPM.

## vSphere

| Step | What the code does | Verified by |
| --- | --- | --- |
| Create | Every VM is a clone of a template, cloned **powered off** (`client/vim.rs`, `clone_vm`) | Unit tests against the fake client |
| vTPM | After the clone and before first power-on, `add_tpm_device` reconfigures the VM with a **new** `VirtualTpm` device; vCenter generates its EK certificate (`vspheremachine.rs`, `ensure_vm`) | Unit tests (`vspheremachine_ensure_tests.rs`: attach before power-on) |
| Template | Templates banlieue builds carry no vTPM: the only `VirtualTpm` constructor is the per-clone add | Code reading |
| Deferred | For a non-`Immediate` image, the template is left uninstalled with the installer attached; each clone installs at first boot | One manual live run, one VM, 2026-09-04 (ADR-0040). Not automated |
| EK certificate | Read host-side from the `VirtualTpm` device and published on the infra CR (ADR-0045) | **Not verified live** (ADR-0045) |

!!! warning "Hand-built templates"
    vSphere's default clone behaviour **copies** a source VM's vTPM,
    including its secrets (ADR-0040). banlieue sets no TPM provision policy
    on the clone and does not check whether a template already has a vTPM.
    A template built outside banlieue that carries a vTPM would pass its
    vTPM to every clone, including clones with `tpmEnabled: false`. Use only
    templates banlieue built, or confirm the template has no vTPM device.
    What vCenter does when `add_tpm_device` runs on a clone that already has
    one is **unverified**.

## libvirt

| Step | What the code does | Verified by |
| --- | --- | --- |
| Create | No clone operation. `Deferred`: a new **empty** qcow2 OS disk. `Immediate`: a qcow2 overlay on the imported image (`libvirtmachine.rs`, `ensure_disks`) | Unit tests |
| vTPM | `<tpm model='tpm-crb'><backend type='emulator' version='2.0'/></tpm>` with no carried state (`xml/domain.rs`, `render_tpm`). libvirt generates a new domain UUID on first define, and swtpm keys its state by that UUID | Unit tests. The swtpm-by-UUID state path is from libvirt behaviour and ADR-0045, **not independently verified here** |
| Delete | Undefine always passes `VIR_DOMAIN_UNDEFINE_TPM`, so the swtpm state is removed with the domain (`banlieue-libvirt/src/procs.rs`) | Unit tests |
| EK certificate | Read out of the guest with `guest-file-read`, and rejected unless its subject CN is `<domain-name>:<domain-uuid>` (`guest.rs`, `ek_cn_matches` in `banlieue-provider-sdk/src/ek.rs`) | Live, **one domain**: `make libvirt-ek-live-test` (`tests/live_ek.rs`) |
| Deferred sealing | Installer on an empty disk | **Not verified live** on libvirt |

## Cloud Hypervisor

| Step | What the code does | Verified by |
| --- | --- | --- |
| Create | `Deferred`: an empty OS disk with the installer attached as a second disk (`plan.rs`) | Unit tests |
| vTPM | swtpm state in `state_root/tpm/<host uid>`, **wiped before manufacture** because host uids are reused (`hostfs.rs`, `reset_tpm_state`), then manufactured once per machine with a VMID of `<machine-name>:<uid>` (`plan.rs`, `machine.rs`) | Unit tests; live, **one machine**: `make ch-vtpm-e2e` (`tests/e2e_vtpm.rs`) |
| EK certificate | Written by host-side manufacture and published from the host files, not reported by the guest | Same live test, asserts `ek_cn_matches` |
| Deferred sealing | The guest installs and encrypts at first boot | Live, one machine: `make ch-deferred-e2e` (`tests/e2e_deferred.rs`, checks for LUKS on the disk). Both recorded passing 2026-09-27 (ADR-0065) |
| Host uid uniqueness | How a host uid is allocated uniquely per machine | **Not verified** in this review |

## Proxmox

No provider yet (roadmap 06). ADR-0074 and roadmap 18 plan a fresh
`tpmstate0` per VM at create.

## What no test proves yet

No test on any provider creates **two** VMs from the same template or image
and asserts that their EK certificates differ. The per-VM CN check (libvirt,
Cloud Hypervisor) makes a copied certificate fail for the second VM, which
is strong evidence, but it is not the two-VM test ADR-0081 asks for. That
test is a candidate change in the alignment report.
