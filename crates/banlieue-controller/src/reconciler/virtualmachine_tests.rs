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
        let mismatch = image_class_mismatch(true, InstallMode::Immediate);
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
        let message = image_class_mismatch(true, InstallMode::Immediate)
            .expect("Immediate + tpmEnabled must be rejected");
        assert!(message.contains("tpmEnabled"), "names the VMClass field");
        assert!(message.contains("installMode"), "names the VMImage field");
        assert!(message.contains("Deferred"), "names the remedy");
    }

    #[test]
    fn tpm_enabled_with_deferred_image_is_accepted() {
        // The sanctioned pairing: each clone installs itself, at its own
        // first boot, with its own already-attached vTPM present.
        assert!(image_class_mismatch(true, InstallMode::Deferred).is_none());
    }

    #[test]
    fn tpm_enabled_with_manual_image_is_accepted() {
        // `Manual` is ADR-0040's documented escape hatch — identical
        // mechanics to Deferred for a build that is not Kairos-driven.
        // banlieue cannot verify what such an image does and must not
        // reject it, or non-Kairos encrypted images have no path at all.
        assert!(image_class_mismatch(true, InstallMode::Manual).is_none());
    }

    #[test]
    fn absent_template_defaults_to_the_rejected_mode() {
        // A VMImage with no `template` is a Template/BackingFile source — a
        // pre-BUILT disk, hence a pre-LAID one, which ADR-0040 says can never
        // be sealed per VM. The reconcile path resolves the absent case with
        // `InstallMode::default()`, so that default must stay `Immediate` or
        // the fail-closed behaviour silently inverts (ADR-0048 Decision 6).
        assert_eq!(InstallMode::default(), InstallMode::Immediate);
        assert!(image_class_mismatch(true, InstallMode::default()).is_some());
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
                image_class_mismatch(false, mode).is_none(),
                "tpmEnabled: false must accept {mode:?}"
            );
        }
    }
}
