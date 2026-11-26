// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `azure.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::fs;

    #[test]
    fn asset_tag_with_the_kernels_newline_is_azure() {
        assert!(asset_tag_is_azure(b"7783-7084-3265-9085-8269-3286-77\n"));
    }

    #[test]
    fn asset_tag_padded_with_spaces_or_nuls_is_azure() {
        assert!(asset_tag_is_azure(
            b"  7783-7084-3265-9085-8269-3286-77 \0\0"
        ));
    }

    #[test]
    fn other_asset_tags_are_not_azure() {
        assert!(!asset_tag_is_azure(b""));
        assert!(!asset_tag_is_azure(b"\n"));
        assert!(!asset_tag_is_azure(b"No Asset Tag\n"));
        assert!(!asset_tag_is_azure(b"7783-7084-3265-9085-8269-3286-7\n"));
        assert!(!asset_tag_is_azure(
            b"7783-7084-3265-9085-8269-3286-77-extra\n"
        ));
    }

    #[test]
    fn a_missing_asset_tag_file_is_not_azure() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_azure(&dir.path().join("chassis_asset_tag")));
    }

    #[test]
    fn an_asset_tag_file_naming_azure_is_azure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("chassis_asset_tag");
        fs::write(&path, format!("{AZURE_CHASSIS_ASSET_TAG}\n")).expect("write");
        assert!(is_azure(&path));
    }

    #[test]
    fn boot_mode_reads_the_kairos_markers() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(boot_mode(dir.path()), BootMode::Unknown);
        fs::write(dir.path().join(PASSIVE_MODE_FILE), "").expect("write");
        assert_eq!(boot_mode(dir.path()), BootMode::Passive);
        fs::write(dir.path().join(ACTIVE_MODE_FILE), "").expect("write");
        assert_eq!(boot_mode(dir.path()), BootMode::Active);
    }

    #[test]
    fn recovery_wins_over_any_other_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(ACTIVE_MODE_FILE), "").expect("write");
        fs::write(dir.path().join(RECOVERY_MODE_FILE), "").expect("write");
        assert_eq!(boot_mode(dir.path()), BootMode::Recovery);
    }

    #[test]
    fn only_active_and_passive_are_installed_boots() {
        assert!(BootMode::Active.is_installed());
        assert!(BootMode::Passive.is_installed());
        assert!(!BootMode::Recovery.is_installed());
        assert!(!BootMode::Unknown.is_installed());
    }
}
