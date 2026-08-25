// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `cloud_config_merge.rs` (ADR-0037).

#[cfg(test)]
mod tests {
    use super::super::*;

    // ------------------------------------------------------------------
    // Empty / single document
    // ------------------------------------------------------------------

    #[test]
    fn test_merge_empty_sources_returns_cloud_config_header() {
        let result = merge_cloud_configs(&[]).unwrap();
        assert!(
            result.starts_with("#cloud-config\n"),
            "must start with #cloud-config header, got: {result:?}"
        );
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert!(parsed.as_mapping().unwrap().is_empty());
    }

    #[test]
    fn test_merge_single_source_round_trips() {
        let src = "install:\n  auto: true\n  poweroff: true\n";
        let result = merge_cloud_configs(&[src]).unwrap();
        assert!(
            result.starts_with("#cloud-config\n"),
            "must start with #cloud-config header"
        );
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert_eq!(parsed["install"]["auto"], serde_yaml::Value::Bool(true));
        assert_eq!(parsed["install"]["poweroff"], serde_yaml::Value::Bool(true));
    }

    #[test]
    fn test_cloud_config_header_preserved_with_input_header() {
        // Input already has #cloud-config (as a YAML comment); output must
        // still have it even though serde_yaml strips comments.
        let src = "#cloud-config\ninstall:\n  auto: true\n";
        let result = merge_cloud_configs(&[src]).unwrap();
        assert!(
            result.starts_with("#cloud-config\n"),
            "must start with #cloud-config header, got: {result:?}"
        );
    }

    // ------------------------------------------------------------------
    // Map deep-merge: scalar-at-same-key → later wins
    // ------------------------------------------------------------------

    #[test]
    fn test_map_scalar_later_wins() {
        let base = "install:\n  auto: true\n  device: /dev/sda\n";
        let overlay = "install:\n  device: /dev/nvme0n1\n";
        let result = merge_cloud_configs(&[base, overlay]).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert_eq!(parsed["install"]["auto"], serde_yaml::Value::Bool(true));
        assert_eq!(
            parsed["install"]["device"],
            serde_yaml::Value::String("/dev/nvme0n1".to_string())
        );
    }

    // ------------------------------------------------------------------
    // Map deep-merge: new keys added
    // ------------------------------------------------------------------

    #[test]
    fn test_map_new_keys_added() {
        let base = "install:\n  auto: true\n";
        let overlay = "users:\n  - name: admin\n";
        let result = merge_cloud_configs(&[base, overlay]).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert_eq!(parsed["install"]["auto"], serde_yaml::Value::Bool(true));
        assert!(parsed["users"].is_sequence());
    }

    // ------------------------------------------------------------------
    // List concatenation
    // ------------------------------------------------------------------

    #[test]
    fn test_list_concatenation() {
        let base = "users:\n  - name: admin\n    groups: [\"admin\"]\n";
        let overlay = "users:\n  - name: monitor\n    groups: [\"users\"]\n";
        let result = merge_cloud_configs(&[base, overlay]).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        let users = parsed["users"].as_sequence().unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0]["name"], serde_yaml::Value::String("admin".into()));
        assert_eq!(
            users[1]["name"],
            serde_yaml::Value::String("monitor".into())
        );
    }

    // ------------------------------------------------------------------
    // Three-way merge (base + overlay1 + overlay2)
    // ------------------------------------------------------------------

    #[test]
    fn test_three_way_merge() {
        let base = "install:\n  auto: true\n  poweroff: true\nusers:\n  - name: admin\n";
        let overlay1 = "stages:\n  after-install-chroot:\n    - name: strip-identity\n      commands:\n        - truncate -s 0 /etc/machine-id\n";
        let overlay2 = "stages:\n  after-install-chroot:\n    - name: crowdstrike\n      commands:\n        - /opt/cs/install.sh\n";
        let result = merge_cloud_configs(&[base, overlay1, overlay2]).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        // install preserved from base
        assert_eq!(parsed["install"]["auto"], serde_yaml::Value::Bool(true));
        // users preserved from base
        assert!(parsed["users"].is_sequence());
        // stages.after-install-chroot concatenated from overlay1 + overlay2
        let stages = parsed["stages"]["after-install-chroot"]
            .as_sequence()
            .unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(
            stages[0]["name"],
            serde_yaml::Value::String("strip-identity".into())
        );
        assert_eq!(
            stages[1]["name"],
            serde_yaml::Value::String("crowdstrike".into())
        );
    }

    // ------------------------------------------------------------------
    // Type mismatch → hard error
    // ------------------------------------------------------------------

    #[test]
    fn test_type_mismatch_is_error() {
        let base = "users:\n  - name: admin\n";
        let overlay = "users: disabled\n";
        let err = merge_cloud_configs(&[base, overlay]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("merge conflict"),
            "expected type-mismatch error, got: {msg}"
        );
        assert!(msg.contains("users"), "error should name the key: {msg}");
    }

    // ------------------------------------------------------------------
    // Parse error
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_error_reports_source_index() {
        let good = "install:\n  auto: true\n";
        let bad = ":\n  :\n  bad yaml [[[";
        let err = merge_cloud_configs(&[good, bad]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("source index 1"), "got: {msg}");
    }

    // ------------------------------------------------------------------
    // Null overlay replaces base value
    // ------------------------------------------------------------------

    #[test]
    fn test_null_overlay_replaces_base() {
        let base = "install:\n  device: /dev/sda\n";
        let overlay = "install:\n  device: null\n";
        let result = merge_cloud_configs(&[base, overlay]).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert!(parsed["install"]["device"].is_null());
    }

    // ------------------------------------------------------------------
    // Deeply nested map merge
    // ------------------------------------------------------------------

    #[test]
    fn test_deeply_nested_merge() {
        let base = "a:\n  b:\n    c: 1\n    d: 2\n";
        let overlay = "a:\n  b:\n    d: 3\n    e: 4\n";
        let result = merge_cloud_configs(&[base, overlay]).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert_eq!(parsed["a"]["b"]["c"], serde_yaml::Value::Number(1.into()));
        assert_eq!(parsed["a"]["b"]["d"], serde_yaml::Value::Number(3.into()));
        assert_eq!(parsed["a"]["b"]["e"], serde_yaml::Value::Number(4.into()));
    }
}
