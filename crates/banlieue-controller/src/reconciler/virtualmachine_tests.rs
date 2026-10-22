// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::virtualmachine`].
//!
//! The async `reconcile` function isn't unit-testable without a fake kube
//! client; those flows are covered by the scheduler / status_mirror /
//! infra suites (each exercising the pure function it owns). The smoke
//! tests below guard the public constants and error-policy decisions.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn finalizer_constant_uses_project_domain() {
        assert!(VM_FINALIZER.starts_with("banlieue.io/"));
    }

    #[test]
    fn finalizer_constant_is_stable_string() {
        // Stable wire value — changing this WILL strand finalizers on
        // existing VMs in production and is therefore a breaking change.
        // Any modification here must come with a documented migration plan.
        assert_eq!(VM_FINALIZER, "banlieue.io/virtualmachine");
    }

    // ---- image_class_mismatch (ADR-0048) --------------------------------
    //
    // The one combination that must be rejected is `tpmEnabled: true` paired
    // with an `Immediate` image: it clones fine, attaches a vTPM, and seals
    // nothing, because an Immediate template's disk was installed before any
    // per-VM vTPM existed (ADR-0040). Every other pairing is legitimate.

    #[test]
    fn tpm_enabled_with_immediate_image_is_rejected() {
        let mismatch = image_class_mismatch(true, InstallMode::Immediate, false, false);
        assert!(
            mismatch.is_some(),
            "tpmEnabled + Immediate attaches a vTPM that nothing ever seals to"
        );
    }

    #[test]
    fn rejection_message_names_both_objects_and_the_remedy() {
        // The condition message is the whole diagnosis: an operator reading
        // `kubectl describe` must be able to fix this without reading code,
        // so it has to name the field that is wrong AND what to set instead.
        let refusal = image_class_mismatch(true, InstallMode::Immediate, false, false)
            .expect("Immediate + tpmEnabled must be rejected");
        assert_eq!(refusal.reason, REASON_IMAGE_CLASS_MISMATCH);
        let message = refusal.message;
        assert!(message.contains("tpmEnabled"), "names the VMClass field");
        assert!(message.contains("installMode"), "names the VMImage field");
        assert!(message.contains("Deferred"), "names the remedy");
        assert!(
            message.contains("firstBootSealing"),
            "names the other remedy (ADR-0088)"
        );
    }

    #[test]
    fn tpm_enabled_with_deferred_image_is_accepted() {
        // The sanctioned pairing: each clone installs itself, at its own
        // first boot, with its own already-attached vTPM present.
        assert!(image_class_mismatch(true, InstallMode::Deferred, false, false).is_none());
    }

    #[test]
    fn tpm_enabled_with_manual_image_is_accepted() {
        // `Manual` is ADR-0040's documented escape hatch — identical
        // mechanics to Deferred for a build that is not Kairos-driven.
        // banlieue cannot verify what such an image does and must not
        // reject it, or non-Kairos encrypted images have no path at all.
        assert!(image_class_mismatch(true, InstallMode::Manual, false, false).is_none());
    }

    #[test]
    fn absent_template_defaults_to_the_rejected_mode() {
        // A VMImage with no `template` is a Template/BackingFile source — a
        // pre-BUILT disk, hence a pre-LAID one, which ADR-0040 says can never
        // be sealed per VM. The reconcile path resolves the absent case with
        // `InstallMode::default()`, so that default must stay `Immediate` or
        // the fail-closed behaviour silently inverts (ADR-0048 Decision 6).
        assert_eq!(InstallMode::default(), InstallMode::Immediate);
        assert!(image_class_mismatch(true, InstallMode::default(), false, false).is_some());
    }

    // ---- image_class_mismatch, first-boot sealing (ADR-0088) -------------
    //
    // ADR-0088 Decision 3's table, plus the Decision 7 flag: the field is
    // inert unless the controller was started with it on, and then the only
    // newly allowed pairing needs the new field.

    #[test]
    fn tpm_enabled_immediate_image_with_first_boot_sealing_is_accepted_when_enabled() {
        // Each clone seals COS_PERSISTENT to its own vTPM on first boot.
        assert!(image_class_mismatch(true, InstallMode::Immediate, true, true).is_none());
    }

    #[test]
    fn first_boot_sealing_without_tpm_enabled_is_rejected() {
        // Every boot of every clone would halt fail closed: no TPM to seal to.
        for mode in [
            InstallMode::Immediate,
            InstallMode::Deferred,
            InstallMode::Manual,
        ] {
            let refusal = image_class_mismatch(false, mode, true, true)
                .unwrap_or_else(|| panic!("sealing without a vTPM must be refused for {mode:?}"));
            assert_eq!(refusal.reason, REASON_IMAGE_CLASS_MISMATCH);
            assert!(refusal.message.contains("tpmEnabled"));
            assert!(refusal.message.contains("firstBootSealing"));
        }
    }

    #[test]
    fn deferred_and_manual_with_first_boot_sealing_are_accepted_when_enabled() {
        // ADR-0088 row three: Deferred/Manual are unchanged by the field;
        // a Deferred install seals at install time and the policy is a no-op.
        for mode in [InstallMode::Deferred, InstallMode::Manual] {
            assert!(image_class_mismatch(true, mode, true, true).is_none());
        }
    }

    #[test]
    fn first_boot_sealing_declared_while_disabled_is_refused_with_its_own_reason() {
        // Decision 7, as amended: with the flag off a declared field is
        // neither honoured nor ignored. The VM is refused, so nothing boots
        // expecting a policy banlieue never agreed to, and the reason says
        // which switch is off.
        for tpm in [true, false] {
            for mode in [
                InstallMode::Immediate,
                InstallMode::Deferred,
                InstallMode::Manual,
            ] {
                let refusal = image_class_mismatch(tpm, mode, true, false)
                    .unwrap_or_else(|| panic!("flag off must refuse tpm={tpm} {mode:?}"));
                assert_eq!(refusal.reason, REASON_FIRST_BOOT_SEALING_DISABLED);
                assert!(refusal.message.contains("--enable-first-boot-sealing"));
            }
        }
    }

    #[test]
    fn the_flag_alone_changes_nothing_for_images_that_do_not_declare_sealing() {
        // "Behaviour changes on upgrade are confined to opted-in VMs":
        // turning the flag on must not change any undeclared pairing.
        for tpm in [true, false] {
            for mode in [
                InstallMode::Immediate,
                InstallMode::Deferred,
                InstallMode::Manual,
            ] {
                assert_eq!(
                    image_class_mismatch(tpm, mode, false, true),
                    image_class_mismatch(tpm, mode, false, false),
                    "tpm={tpm} {mode:?}"
                );
            }
        }
    }

    // ---- spec.paused (in-band pause) ------------------------------------
    //
    // Same convention as `VSphereCluster.spec.paused` (ADR-0002) and
    // `ProviderClass.spec.paused` (ADR-0012): while paused the reconciler
    // does nothing but report `Paused=True` / `Ready=False`; on resume the
    // `Paused` condition disappears rather than flipping to False, exactly
    // like the fresh-built condition list in vsphere_cluster's
    // `build_status`.

    #[test]
    fn paused_status_reports_paused_and_not_ready() {
        let status = paused_status(None, 7);

        let paused = status
            .conditions
            .iter()
            .find(|c| c.type_ == CONDITION_PAUSED)
            .expect("paused status must carry a Paused condition");
        assert_eq!(paused.status, condition_status::TRUE);
        assert_eq!(paused.reason, REASON_PAUSED);

        let ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .expect("paused status must carry a Ready condition");
        assert_eq!(ready.status, condition_status::FALSE);
        assert_eq!(ready.reason, REASON_PAUSED);

        assert_eq!(status.observed_generation, Some(7));
    }

    #[test]
    fn paused_status_preserves_previously_mirrored_fields() {
        // The whole-status SSA rule (`patch_status`'s doc comment): a paused
        // patch that dropped `observedPowerState` / `scheduled` / other
        // mirrored fields would make the apiserver retract this manager's
        // ownership of them, wiping them for as long as the VM stays paused.
        use banlieue_api::common::PowerState;

        let mut current = VirtualMachineStatus {
            observed_power_state: Some(PowerState::PoweredOn),
            ..Default::default()
        };
        set_condition(
            &mut current.conditions,
            condition_types::SCHEDULED,
            condition_status::TRUE,
            "Scheduled",
            "VirtualMachine scheduled successfully",
            3,
        );

        let status = paused_status(Some(&current), 4);

        assert_eq!(status.observed_power_state, Some(PowerState::PoweredOn));
        let scheduled = status
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::SCHEDULED)
            .expect("pre-existing conditions must survive the paused patch");
        assert_eq!(scheduled.status, condition_status::TRUE);
    }

    #[test]
    fn clear_paused_removes_the_condition_on_resume() {
        let mut status = paused_status(None, 2);
        clear_paused(&mut status.conditions);
        assert!(
            !status
                .conditions
                .iter()
                .any(|c| c.type_ == CONDITION_PAUSED),
            "resume must drop the Paused condition entirely"
        );
    }

    #[test]
    fn clear_paused_leaves_other_conditions_alone() {
        let mut status = paused_status(None, 2);
        let before: Vec<String> = status
            .conditions
            .iter()
            .filter(|c| c.type_ != CONDITION_PAUSED)
            .map(|c| c.type_.clone())
            .collect();

        clear_paused(&mut status.conditions);

        // Only Paused is removed; Ready (stale, from the paused patch) is
        // left for the resumed reconcile pass to overwrite with a real value.
        let after: Vec<String> = status.conditions.iter().map(|c| c.type_.clone()).collect();
        assert_eq!(after, before);

        // And on a list that was never paused it is a no-op.
        let mut untouched = VirtualMachineStatus::default();
        clear_paused(&mut untouched.conditions);
        assert!(untouched.conditions.is_empty());
    }

    #[test]
    fn paused_wire_strings_are_stable() {
        // Stable wire values — operators and tests match on these.
        assert_eq!(CONDITION_PAUSED, "Paused");
        assert_eq!(REASON_PAUSED, "Paused");
    }

    #[test]
    fn tpm_disabled_accepts_every_install_mode() {
        // Without a vTPM there is nothing to seal to, so no pairing is a
        // mismatch — including Immediate, which is the common case and must
        // not regress.
        for mode in [
            InstallMode::Immediate,
            InstallMode::Deferred,
            InstallMode::Manual,
        ] {
            assert!(
                image_class_mismatch(false, mode, false, false).is_none(),
                "tpmEnabled: false must accept {mode:?}"
            );
        }
    }

    // ---- plan_finalize (ADR-0090) ---------------------------------------
    //
    // Pure decision step of the deletion cascade: given each backend kind's
    // resolved InfraState, decide what to delete and whether every kind is
    // confirmed gone. `Unknown` (a lookup error other than 404) must behave
    // like neither Absent nor Present — this is the whole fix.

    fn states(
        vsphere: InfraState,
        libvirt: InfraState,
        cloud_hypervisor: InfraState,
        proxmox: InfraState,
    ) -> [(InfraKind, InfraState); 4] {
        [
            (InfraKind::VSphere, vsphere),
            (InfraKind::Libvirt, libvirt),
            (InfraKind::CloudHypervisor, cloud_hypervisor),
            (InfraKind::Proxmox, proxmox),
        ]
    }

    #[test]
    fn all_absent_removes_the_finalizer() {
        let plan = plan_finalize(&states(
            InfraState::Absent,
            InfraState::Absent,
            InfraState::Absent,
            InfraState::Absent,
        ));
        assert!(plan.remove_finalizer);
        assert!(plan.delete.is_empty());
    }

    #[test]
    fn a_present_kind_is_deleted_and_finalizer_stays() {
        let plan = plan_finalize(&states(
            InfraState::Present,
            InfraState::Absent,
            InfraState::Absent,
            InfraState::Absent,
        ));
        assert_eq!(plan.delete, vec![InfraKind::VSphere]);
        assert!(!plan.remove_finalizer);
    }

    #[test]
    fn a_terminating_kind_is_not_deleted_again_and_finalizer_stays() {
        // Already has a deletionTimestamp — its own finalizer is doing the
        // work; issuing another delete is a redundant no-op at best.
        let plan = plan_finalize(&states(
            InfraState::Terminating,
            InfraState::Absent,
            InfraState::Absent,
            InfraState::Absent,
        ));
        assert!(plan.delete.is_empty());
        assert!(!plan.remove_finalizer);
    }

    #[test]
    fn an_unknown_kind_blocks_finalizer_removal_but_is_never_queued_for_delete() {
        // There is nothing safe to act on for a kind whose lookup failed —
        // it is not asked to delete anything, and it is not treated as gone.
        let plan = plan_finalize(&states(
            InfraState::Absent,
            InfraState::Absent,
            InfraState::Absent,
            InfraState::Unknown,
        ));
        assert!(plan.delete.is_empty());
        assert!(!plan.remove_finalizer);
    }

    #[test]
    fn an_unknown_kind_does_not_block_a_present_kinds_delete() {
        // This is the actual bug: a lookup failure on one kind (e.g. an RBAC
        // gap on a backend this VM never used) must not prevent a *different*
        // kind's backend VM from being torn down.
        let plan = plan_finalize(&states(
            InfraState::Present,
            InfraState::Absent,
            InfraState::Absent,
            InfraState::Unknown,
        ));
        assert_eq!(plan.delete, vec![InfraKind::VSphere]);
        assert!(!plan.remove_finalizer);
    }

    #[test]
    fn every_present_kind_is_queued_for_delete_in_one_pass() {
        let plan = plan_finalize(&states(
            InfraState::Present,
            InfraState::Present,
            InfraState::Absent,
            InfraState::Absent,
        ));
        assert_eq!(plan.delete, vec![InfraKind::VSphere, InfraKind::Libvirt]);
        assert!(!plan.remove_finalizer);
    }
}
