// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `first_boot_sealing.rs` (ADR-0088, as amended 2026-10-22).

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::cloud_config_merge::merge_cloud_configs;
    use banlieue_api::banlieue::{BuildArtifactKind, InstallMode};
    use banlieue_api::common::FirstBootSealing;
    use serde_json::Value;

    fn yaml(s: &str) -> Value {
        serde_saphyr::from_str(s).expect("valid YAML")
    }

    fn sealing(partitions: &[&str]) -> FirstBootSealing {
        FirstBootSealing {
            partitions: partitions.iter().map(|p| (*p).to_string()).collect(),
            ..FirstBootSealing::default()
        }
    }

    fn iso_layer(partitions: &[&str]) -> Value {
        yaml(
            &sealing_layer(
                &sealing(partitions),
                &BuildArtifactKind::Iso,
                InstallMode::Immediate,
                "",
            )
            .expect("layer"),
        )
    }

    /// The step that writes the policy into the installed disk's COS_OEM.
    fn policy_step(layer: &Value) -> &Value {
        layer["stages"]["after-install-chroot"]
            .as_array()
            .expect("after-install-chroot stage")
            .iter()
            .find(|s| s["name"] == POLICY_STEP_NAME)
            .expect("policy step")
    }

    // ---- ISO builds: the policy goes to COS_OEM, never to the install ----

    #[test]
    fn iso_layer_writes_the_policy_file_into_oem_after_the_install() {
        let layer = iso_layer(&["COS_PERSISTENT"]);
        let file = &policy_step(&layer)["files"][0];
        assert_eq!(file["path"], POLICY_FILE);
        assert!(
            POLICY_FILE.starts_with("/oem/"),
            "COS_OEM is where encrypt-pending reads"
        );
        let policy = yaml(file["content"].as_str().expect("content"));
        assert_eq!(policy["kcrypt"]["encrypt_on_boot"], true);
        assert_eq!(
            policy["install"]["encrypted_partitions"][0],
            "COS_PERSISTENT"
        );
    }

    /// Kairos's collector silently skips a config file without a
    /// `#cloud-config` header (`HasValidHeader`), so a headerless policy
    /// file is never read and `encrypt-pending` is a no-op. Found live: the
    /// clone booted with COS_PERSISTENT still plaintext.
    #[test]
    fn iso_policy_file_carries_the_cloud_config_header() {
        let layer = iso_layer(&["COS_PERSISTENT"]);
        let content = policy_step(&layer)["files"][0]["content"]
            .as_str()
            .expect("content")
            .to_string();
        assert_eq!(content.lines().next(), Some(CLOUD_CONFIG_HEADER));
    }

    /// The whole point of the amendment: the template installs once with no
    /// vTPM, and the install hook does not read encrypt_on_boot, so the
    /// install configuration must not ask for encryption.
    #[test]
    fn iso_layer_never_puts_encryption_in_the_install_configuration() {
        let layer = iso_layer(&["COS_PERSISTENT"]);
        assert!(layer.get("install").is_none());
        assert!(layer.get("kcrypt").is_none());
    }

    #[test]
    fn iso_layer_carries_every_declared_partition_in_order() {
        let layer = iso_layer(&["COS_PERSISTENT", "COS_DATA"]);
        let policy = yaml(policy_step(&layer)["files"][0]["content"].as_str().unwrap());
        assert_eq!(
            policy["install"]["encrypted_partitions"],
            serde_json::json!(["COS_PERSISTENT", "COS_DATA"])
        );
    }

    // ---- CloudImage builds: no installer, the cloud-config IS the OEM ----

    #[test]
    fn cloud_image_layer_carries_the_policy_keys_directly() {
        let layer = yaml(
            &sealing_layer(
                &sealing(&["COS_PERSISTENT"]),
                &BuildArtifactKind::CloudImage,
                InstallMode::Immediate,
                "",
            )
            .unwrap(),
        );
        assert_eq!(layer["kcrypt"]["encrypt_on_boot"], true);
        assert_eq!(
            layer["install"]["encrypted_partitions"][0],
            "COS_PERSISTENT"
        );
        assert!(layer["stages"].get("after-install-chroot").is_none());
    }

    /// Lists concatenate in banlieue's merge, so a partition the operator
    /// already listed is not listed twice.
    #[test]
    fn cloud_image_layer_does_not_repeat_a_partition_the_operator_listed() {
        let operator = "install:\n  encrypted_partitions:\n    - COS_PERSISTENT\n";
        let layer = sealing_layer(
            &sealing(&["COS_PERSISTENT", "COS_DATA"]),
            &BuildArtifactKind::CloudImage,
            InstallMode::Immediate,
            operator,
        )
        .unwrap();
        let merged = yaml(&merge_cloud_configs(&[operator, &layer]).unwrap());
        assert_eq!(
            merged["install"]["encrypted_partitions"],
            serde_json::json!(["COS_PERSISTENT", "COS_DATA"])
        );
    }

    // ---- the sealing report (Decision 5) ---------------------------------

    fn report_step(layer: &Value) -> &Value {
        layer["stages"]["boot"]
            .as_array()
            .expect("boot stage")
            .iter()
            .find(|s| s["name"] == REPORT_STEP_NAME)
            .expect("report step")
    }

    #[test]
    fn the_report_runs_only_on_the_installed_system() {
        let layer = iso_layer(&["COS_PERSISTENT"]);
        let guard = report_step(&layer)["if"].as_str().unwrap();
        assert!(guard.contains("/run/cos/active_mode"));
        assert!(guard.contains("/run/cos/passive_mode"));
    }

    #[test]
    fn the_report_names_every_partition_and_writes_both_channels() {
        let layer = iso_layer(&["COS_PERSISTENT", "COS_DATA"]);
        let script = report_step(&layer)["commands"][0].as_str().unwrap();
        assert!(script.contains("COS_PERSISTENT"));
        assert!(script.contains("COS_DATA"));
        assert!(script.contains("crypto_LUKS"));
        assert!(script.contains(GUESTINFO_SEALED_KEY));
        assert!(script.contains(RUN_SEALED_PATH));
    }

    #[test]
    fn the_report_accepts_both_luks_label_shapes() {
        // kairos-io/kairos#4403 moved the LUKS container label from `<P>` to
        // `<P>_LUKS`; an image on either side of it must report.
        let layer = iso_layer(&["COS_PERSISTENT"]);
        let script = report_step(&layer)["commands"][0].as_str().unwrap();
        assert!(script.contains("_LUKS"));
    }

    // ---- refusals ---------------------------------------------------------

    #[test]
    fn an_immediate_iso_with_operator_install_encryption_is_a_conflict() {
        let operator = "install:\n  encrypted_partitions:\n    - COS_PERSISTENT\n";
        let err = sealing_layer(
            &sealing(&["COS_PERSISTENT"]),
            &BuildArtifactKind::Iso,
            InstallMode::Immediate,
            operator,
        )
        .unwrap_err();
        assert_eq!(err.reason(), REASON_FIRST_BOOT_SEALING_CONFLICT);
        assert!(err.to_string().contains("encrypted_partitions"));
    }

    /// A Deferred clone installs with its own vTPM present, so install-time
    /// encryption is exactly right there and the policy is a no-op.
    #[test]
    fn a_deferred_iso_may_keep_operator_install_encryption() {
        let operator = "install:\n  encrypted_partitions:\n    - COS_PERSISTENT\n";
        assert!(
            sealing_layer(
                &sealing(&["COS_PERSISTENT"]),
                &BuildArtifactKind::Iso,
                InstallMode::Deferred,
                operator
            )
            .is_ok()
        );
    }

    #[test]
    fn an_empty_operator_encryption_list_is_not_a_conflict() {
        let operator = "install:\n  encrypted_partitions: []\n";
        assert!(
            sealing_layer(
                &sealing(&["COS_PERSISTENT"]),
                &BuildArtifactKind::Iso,
                InstallMode::Immediate,
                operator
            )
            .is_ok()
        );
    }

    /// The names end up in a shell script that runs as root on every boot of
    /// every clone: anything but a plain label is refused, whatever
    /// admission did.
    #[test]
    fn a_partition_that_is_not_a_plain_label_is_refused() {
        for bad in [
            "COS;reboot",
            "cos_persistent",
            "COS PERSISTENT",
            "$(id)",
            "",
        ] {
            let err = sealing_layer(
                &sealing(&[bad]),
                &BuildArtifactKind::Iso,
                InstallMode::Immediate,
                "",
            )
            .unwrap_err();
            assert_eq!(err.reason(), REASON_FIRST_BOOT_SEALING_INVALID, "{bad:?}");
        }
    }

    #[test]
    fn a_system_partition_is_refused() {
        for system in ["COS_OEM", "COS_STATE", "COS_RECOVERY", "COS_GRUB"] {
            let err = sealing_layer(
                &sealing(&[system]),
                &BuildArtifactKind::Iso,
                InstallMode::Immediate,
                "",
            )
            .unwrap_err();
            assert_eq!(err.reason(), REASON_FIRST_BOOT_SEALING_INVALID, "{system}");
        }
    }

    #[test]
    fn an_empty_partition_list_is_refused() {
        let err = sealing_layer(
            &sealing(&[]),
            &BuildArtifactKind::Iso,
            InstallMode::Immediate,
            "",
        )
        .unwrap_err();
        assert_eq!(err.reason(), REASON_FIRST_BOOT_SEALING_INVALID);
    }

    #[test]
    fn unparseable_operator_yaml_is_an_error_not_a_pass() {
        assert!(
            sealing_layer(
                &sealing(&["COS_PERSISTENT"]),
                &BuildArtifactKind::Iso,
                InstallMode::Immediate,
                "install: [unclosed"
            )
            .is_err()
        );
    }

    // ---- merging -----------------------------------------------------------

    /// ADR-0037 makes later layers win, and ADR-0088 needs banlieue's layer
    /// to win: its boot step must run after every operator step.
    #[test]
    fn the_layer_merges_after_the_operator_layers() {
        let operator = "stages:\n  boot:\n    - name: operator-step\n      commands: [\"true\"]\n";
        let layer = sealing_layer(
            &sealing(&["COS_PERSISTENT"]),
            &BuildArtifactKind::Iso,
            InstallMode::Immediate,
            operator,
        )
        .unwrap();
        let merged = yaml(&merge_cloud_configs(&[operator, &layer]).unwrap());
        let boot = merged["stages"]["boot"].as_array().unwrap();
        assert_eq!(boot[0]["name"], "operator-step");
        assert_eq!(boot.last().unwrap()["name"], REPORT_STEP_NAME);
    }
}
