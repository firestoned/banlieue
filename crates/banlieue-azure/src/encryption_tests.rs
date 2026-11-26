// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `encryption.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    const SUB: &str = "00000000-0000-0000-0000-000000000000";
    const OTHER_SUB: &str = "00000000-0000-0000-0000-0000000000ff";

    fn des(sub: &str) -> String {
        format!(
            "/subscriptions/{sub}/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1"
        )
    }

    #[test]
    fn a_disk_encryption_set_id_parses_into_its_parts() {
        let id = parse_disk_encryption_set_id(&des(SUB)).expect("valid");
        assert_eq!(id.subscription_id, SUB);
        assert_eq!(id.resource_group, "rg-sec");
        assert_eq!(id.name, "des-1");
    }

    /// ARM resource IDs are case-insensitive in their fixed segments.
    #[test]
    fn fixed_segments_are_matched_case_insensitively() {
        let id = format!(
            "/SUBSCRIPTIONS/{SUB}/resourcegroups/rg-sec/PROVIDERS/microsoft.compute/DiskEncryptionSets/des-1"
        );
        assert!(parse_disk_encryption_set_id(&id).is_ok());
    }

    #[test]
    fn anything_but_a_disk_encryption_set_id_is_refused() {
        for bad in [
            String::new(),
            "des-1".to_string(),
            format!("subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.Compute/diskEncryptionSets/des"),
            format!("/subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/kv"),
            format!("/subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.Compute/disks/d"),
            format!("/subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.Compute/diskEncryptionSets/"),
            format!("/subscriptions/{SUB}/resourceGroups//providers/Microsoft.Compute/diskEncryptionSets/des"),
            format!("/subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.Compute/diskEncryptionSets/des/extra"),
            format!("/subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.Compute/diskEncryptionSets/des?x=1"),
            format!("/subscriptions/{SUB}/resourceGroups/r g/providers/Microsoft.Compute/diskEncryptionSets/des"),
            format!("/subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.Compute/diskEncryptionSets/de$"),
            "/subscriptions/not-a-guid/resourceGroups/rg/providers/Microsoft.Compute/diskEncryptionSets/des".to_string(),
        ] {
            let err = parse_disk_encryption_set_id(&bad).expect_err(&bad);
            assert!(
                matches!(err, Error::InvalidRequest { ref code, .. } if code == CODE_INVALID_DISK_ENCRYPTION_SET),
                "{bad}: {err:?}"
            );
        }
    }

    /// Microsoft: the set must be in the subscription of the disks it
    /// encrypts (ADR-0172 Decision 2).
    #[test]
    fn a_set_in_another_subscription_is_refused() {
        assert!(check_disk_encryption_set_id(&des(SUB), SUB).is_ok());
        assert!(check_disk_encryption_set_id(&des(SUB), &SUB.to_ascii_uppercase()).is_ok());
        let err = check_disk_encryption_set_id(&des(OTHER_SUB), SUB).expect_err("refused");
        assert!(err.to_string().contains("subscription"), "{err}");
    }

    #[test]
    fn the_disk_encryption_body_names_the_customer_key_type() {
        let body = disk_encryption_body(&des(SUB));
        assert_eq!(body["type"], ENCRYPTION_AT_REST_WITH_CUSTOMER_KEY);
        assert_eq!(body["diskEncryptionSetId"], des(SUB));
    }
}
