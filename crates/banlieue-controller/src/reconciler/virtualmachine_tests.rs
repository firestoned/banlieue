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
