// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `azure_guest.rs` (ADR-0171).

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::ImageSourceKind;

    /// The whole layer, as it lands in the merged Secret. A change here is a
    /// change to every Azure image: review the diff, then update the file.
    const SNAPSHOT: &str = include_str!("azure_guest_layer.snapshot.yaml");

    fn layer() -> Value {
        serde_saphyr::from_str(&azure_guest_layer()).expect("valid YAML")
    }

    fn steps(stage: &str) -> Vec<Value> {
        layer()["stages"][stage]
            .as_array()
            .expect("stage present")
            .clone()
    }

    fn step(stage: &str, name: &str) -> Value {
        steps(stage)
            .into_iter()
            .find(|s| s["name"] == name)
            .unwrap_or_else(|| panic!("step {name} in {stage}"))
    }

    fn source(class: &str, kind: ImageSourceKind) -> ImageSource {
        ImageSource {
            provider_class: class.to_string(),
            kind,
            reference: "ignored".to_string(),
            import_from: None,
            checksum: None,
        }
    }

    // ---- the snapshot ----

    /// The snapshot without its leading licence and comment lines.
    fn snapshot_body() -> String {
        SNAPSHOT
            .lines()
            .skip_while(|l| l.starts_with('#'))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    #[test]
    fn the_layer_matches_its_snapshot() {
        assert_eq!(azure_guest_layer(), snapshot_body());
    }

    // ---- every step is Azure-only ----

    #[test]
    fn the_guard_matches_the_azure_asset_tag() {
        assert!(AZURE_GUARD.contains(AZURE_CHASSIS_ASSET_TAG));
        assert!(AZURE_GUARD.contains("/sys/class/dmi/id/chassis_asset_tag"));
    }

    #[test]
    fn every_step_is_gated_on_azure() {
        let doc = layer();
        let stages = doc["stages"].as_object().expect("stages");
        assert_eq!(
            stages.keys().map(String::as_str).collect::<Vec<_>>(),
            ["boot", "initramfs", "rootfs.before"]
        );
        for (stage, list) in stages {
            for s in list.as_array().expect("list") {
                assert_eq!(s["if"], AZURE_GUARD, "{stage}/{}", s["name"]);
            }
        }
    }

    #[test]
    fn the_layer_sets_no_top_level_key_but_stages() {
        let doc = layer();
        assert_eq!(
            doc.as_object()
                .expect("map")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["stages"],
            "never install, users or kcrypt: those belong to the site"
        );
    }

    // ---- the VF stays unmanaged, in the initrd and in the real root ----

    #[test]
    fn the_vf_file_is_written_for_the_initrd_and_the_root() {
        for (stage, name) in [
            ("rootfs.before", STEP_VF_INITRD),
            ("initramfs", STEP_VF_ROOT),
        ] {
            let s = step(stage, name);
            assert_eq!(s["files"][0]["path"], VF_NETWORK_FILE);
            assert_eq!(s["files"][0]["permissions"], 0o644);
            assert_eq!(s["files"][0]["content"], VF_NETWORK_CONTENT);
        }
        assert!(VF_NETWORK_CONTENT.contains("Driver=mana mlx4_core mlx5_core"));
        assert!(VF_NETWORK_CONTENT.contains("Unmanaged=yes"));
        let file = VF_NETWORK_FILE.rsplit('/').next().expect("name");
        assert!(file < "20-", "sorts before Kairos's catch-all DHCP file");
    }

    #[test]
    fn the_qemu_guest_agent_is_masked_for_this_boot_only() {
        let s = step("initramfs", STEP_NO_QEMU_GA);
        let cmd = s["commands"][0].as_str().expect("command");
        assert!(cmd.contains("systemctl mask --runtime qemu-guest-agent.service"));
    }

    // ---- customData, then ready ----

    #[test]
    fn custom_data_runs_in_boot_in_every_mode() {
        let s = step("boot", STEP_CUSTOM_DATA);
        assert_eq!(s["commands"][0], "/usr/bin/banlieue-azure-guest customdata");
        assert!(
            !s["if"].as_str().expect("guard").contains("recovery"),
            "the recovery boot is the one that writes it before the reset"
        );
    }

    #[test]
    fn ready_is_a_unit_after_kairos_network_stage_never_a_stage_step() {
        let s = step("initramfs", STEP_READY_UNIT);
        assert_eq!(s["files"][0]["path"], READY_UNIT_FILE);
        assert_eq!(s["files"][0]["content"], READY_UNIT_CONTENT);
        let link = s["commands"][0].as_str().expect("command");
        assert!(link.ends_with(&format!("multi-user.target.wants/{READY_UNIT_NAME}")));
        assert!(READY_UNIT_CONTENT.contains("After=cos-setup-network.service\n"));
        assert!(READY_UNIT_CONTENT.contains("ExecStart=/usr/bin/banlieue-azure-guest ready\n"));
        for stage in ["rootfs.before", "initramfs", "boot"] {
            for st in steps(stage) {
                let cmds = st["commands"].to_string();
                assert!(!cmds.contains("guest ready"), "{stage} must not report");
            }
        }
    }

    #[test]
    fn the_ready_unit_never_runs_in_recovery() {
        assert!(READY_UNIT_CONTENT.contains("ConditionPathExists=|/run/cos/active_mode\n"));
        assert!(READY_UNIT_CONTENT.contains("ConditionPathExists=|/run/cos/passive_mode\n"));
        assert!(READY_UNIT_CONTENT.contains("ConditionPathExists=!/run/cos/recovery_mode\n"));
    }

    #[test]
    fn the_helper_path_is_outside_persistent_usr_local() {
        assert!(HELPER_PATH.starts_with("/usr/bin/"));
        assert!(READY_UNIT_CONTENT.contains(HELPER_PATH));
    }

    // ---- when the layer applies ----

    #[test]
    fn any_azure_source_of_any_kind_triggers_the_layer() {
        assert!(has_azure_source(&[source("azure", ImageSourceKind::Url)]));
        assert!(has_azure_source(&[
            source("libvirt", ImageSourceKind::Url),
            source("azure", ImageSourceKind::Template),
        ]));
    }

    #[test]
    fn no_azure_source_no_layer() {
        assert!(!has_azure_source(&[]));
        assert!(!has_azure_source(&[
            source("libvirt", ImageSourceKind::Url),
            source("vsphere", ImageSourceKind::Template),
            source("Azure", ImageSourceKind::Url),
        ]));
    }

    // ---- the helper image is pinned by digest ----

    #[test]
    fn a_digest_pinned_reference_is_accepted() {
        let hex = "0123456789abcdef".repeat(4);
        assert_eq!(
            validate_helper_image(&format!("ghcr.io/x/y@sha256:{hex}")),
            Ok(())
        );
        assert_eq!(
            validate_helper_image(&format!("registry.internal:5000/y:v1@sha256:{hex}")),
            Ok(())
        );
    }

    #[test]
    fn anything_not_pinned_by_a_sha256_digest_is_refused() {
        let hex = "0123456789abcdef".repeat(4);
        for bad in [
            "ghcr.io/x/y:v0.6.0".to_string(),
            "ghcr.io/x/y".to_string(),
            format!("@sha256:{hex}"),
            format!("ghcr.io/x/y@sha512:{hex}"),
            "ghcr.io/x/y@sha256:abc".to_string(),
            format!("ghcr.io/x/y@sha256:{}", hex.to_uppercase()),
            format!("ghcr.io/x y@sha256:{hex}"),
        ] {
            assert!(validate_helper_image(&bad).is_err(), "{bad}");
        }
    }
}
