// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `tags.rs`.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::*;

    const UID: &str = "0b9c5f0e-7a40-4d27-9e2c-1c3a8f5d2b11";

    fn ownership() -> OwnershipTags {
        OwnershipTags::new(
            UID,
            "AzureVirtualMachine",
            "banlieue-system",
            "azure-eastus2",
        )
    }

    #[test]
    fn ownership_tags_use_dash_names_azure_accepts() {
        let map = ownership().to_map();
        assert_eq!(map.get(TAG_OWNER_UID).map(String::as_str), Some(UID));
        assert_eq!(
            map.get(TAG_OWNER_KIND).map(String::as_str),
            Some("AzureVirtualMachine")
        );
        assert_eq!(
            map.get(TAG_PROVIDER).map(String::as_str),
            Some("banlieue-system.azure-eastus2")
        );
        for name in map.keys() {
            assert!(validate_tag_name(name).is_ok(), "{name} must be valid");
        }
    }

    #[test]
    fn a_tag_name_with_a_slash_is_refused_as_azure_refuses_it() {
        // Seen live: InvalidTagNameCharacters for `banlieue.io/purpose`.
        let err = validate_tag_name("banlieue.io/owner-uid").unwrap_err();
        assert!(err.to_string().contains("banlieue.io/owner-uid"));
    }

    #[test]
    fn every_reserved_character_is_refused() {
        for c in ['<', '>', '%', '&', '\\', '?', '/'] {
            let name = format!("bad{c}name");
            assert!(
                validate_tag_name(&name).is_err(),
                "{name:?} must be refused"
            );
        }
    }

    #[test]
    fn empty_overlong_and_control_character_names_are_refused() {
        assert!(validate_tag_name("").is_err());
        assert!(validate_tag_name(&"a".repeat(MAX_TAG_NAME_LEN + 1)).is_err());
        assert!(validate_tag_name(&"a".repeat(MAX_TAG_NAME_LEN)).is_ok());
        assert!(validate_tag_name("tab\there").is_err());
    }

    #[test]
    fn an_overlong_value_is_refused() {
        assert!(validate_tag_value(&"v".repeat(MAX_TAG_VALUE_LEN)).is_ok());
        assert!(validate_tag_value(&"v".repeat(MAX_TAG_VALUE_LEN + 1)).is_err());
    }

    #[test]
    fn owns_requires_the_exact_uid() {
        let mine = ownership().to_map();
        assert!(ownership().owns(&mine));

        let mut other = mine.clone();
        other.insert(TAG_OWNER_UID.to_string(), "another-uid".to_string());
        assert!(!ownership().owns(&other));

        let untagged = BTreeMap::new();
        assert!(!ownership().owns(&untagged));
    }

    #[test]
    fn from_tags_reads_back_what_to_map_wrote() {
        let parsed = OwnershipTags::from_tags(&ownership().to_map()).expect("parses");
        assert_eq!(parsed, ownership());
    }

    #[test]
    fn from_tags_is_none_without_all_three() {
        let mut map = ownership().to_map();
        map.remove(TAG_PROVIDER);
        assert!(OwnershipTags::from_tags(&map).is_none());
    }

    #[test]
    fn extra_tags_are_merged_but_cannot_override_ownership() {
        let mut extra = BTreeMap::new();
        extra.insert("cost-centre".to_string(), "1234".to_string());
        let merged = ownership().merged_with(&extra).expect("merges");
        assert_eq!(merged.get("cost-centre").map(String::as_str), Some("1234"));
        assert_eq!(merged.get(TAG_OWNER_UID).map(String::as_str), Some(UID));

        let mut hostile = BTreeMap::new();
        hostile.insert(TAG_OWNER_UID.to_string(), "someone-else".to_string());
        assert!(ownership().merged_with(&hostile).is_err());
    }

    #[test]
    fn merged_tags_are_validated_and_capped() {
        let mut bad = BTreeMap::new();
        bad.insert("a/b".to_string(), "x".to_string());
        assert!(ownership().merged_with(&bad).is_err());

        let too_many: BTreeMap<String, String> = (0..MAX_TAGS_PER_RESOURCE)
            .map(|i| (format!("extra-{i}"), "x".to_string()))
            .collect();
        assert!(ownership().merged_with(&too_many).is_err());
    }
}
