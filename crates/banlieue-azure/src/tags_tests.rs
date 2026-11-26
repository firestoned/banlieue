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

    /// ADR-0172 Decision 1: Azure folds tag-name case, so an ownership tag
    /// in another case would replace ours on ARM's side. Refused.
    #[test]
    fn an_ownership_tag_in_another_case_is_refused() {
        let hostile = BTreeMap::from([("Banlieue-Owner-Uid".to_string(), "x".to_string())]);
        let err = ownership().merged_with(&hostile).expect_err("refused");
        assert!(matches!(err, Error::InvalidTag(_)), "{err:?}");
    }

    /// The whole `banlieue-` prefix is banlieue's, including tags it sets
    /// only on some resources (the published digest, ADR-0162 Decision 6).
    #[test]
    fn any_name_in_the_banlieue_prefix_is_refused() {
        for name in ["banlieue-source-sha256", "BANLIEUE-anything", "banlieue-"] {
            let extra = BTreeMap::from([(name.to_string(), "x".to_string())]);
            assert!(
                validate_extra_tags(&extra).is_err(),
                "{name} must be refused"
            );
        }
        let fine = BTreeMap::from([("banlieue".to_string(), "x".to_string())]);
        assert!(
            validate_extra_tags(&fine).is_ok(),
            "no dash: not the prefix"
        );
    }

    #[test]
    fn two_names_that_differ_only_in_case_are_refused() {
        let extra = BTreeMap::from([
            ("Env".to_string(), "prod".to_string()),
            ("env".to_string(), "dev".to_string()),
        ]);
        assert!(validate_extra_tags(&extra).is_err());
        assert!(validate_tags(&extra).is_err(), "ARM sees one tag twice");
    }

    #[test]
    fn extra_tags_are_capped_so_every_resource_fits() {
        let at_cap: BTreeMap<String, String> = (0..MAX_EXTRA_TAGS)
            .map(|i| (format!("extra-{i}"), "x".to_string()))
            .collect();
        assert!(validate_extra_tags(&at_cap).is_ok());
        let mut merged = ownership()
            .with_owner_namespace("team-a")
            .merged_with(&at_cap)
            .expect("fits with all four ownership tags");
        merged.insert(TAG_SOURCE_SHA256.to_string(), "00".to_string());
        assert!(validate_tags(&merged).is_ok(), "and with the digest tag");

        let over: BTreeMap<String, String> = (0..=MAX_EXTRA_TAGS)
            .map(|i| (format!("extra-{i}"), "x".to_string()))
            .collect();
        assert!(validate_extra_tags(&over).is_err());
    }

    #[test]
    fn extra_tag_names_and_values_follow_azure_rules() {
        for (name, value) in [
            ("cost/centre", "1"),
            ("", "1"),
            ("ok", &"v".repeat(MAX_TAG_VALUE_LEN + 1)[..]),
            (&"n".repeat(MAX_TAG_NAME_LEN + 1)[..], "1"),
        ] {
            let extra = BTreeMap::from([(name.to_string(), value.to_string())]);
            assert!(
                validate_extra_tags(&extra).is_err(),
                "{name:?} must be refused"
            );
        }
        let fine = BTreeMap::from([("cost-centre".to_string(), String::new())]);
        assert!(
            validate_extra_tags(&fine).is_ok(),
            "an empty value is allowed"
        );
    }

    #[test]
    fn owner_namespace_is_tagged_only_when_set_and_reads_back() {
        let plain = OwnershipTags::new("uid-1", "AzureVirtualMachine", "banlieue-system", "az");
        assert!(!plain.to_map().contains_key(TAG_OWNER_NAMESPACE));

        let scoped = plain.with_owner_namespace("team-a");
        let map = scoped.to_map();
        assert_eq!(
            map.get(TAG_OWNER_NAMESPACE).map(String::as_str),
            Some("team-a")
        );
        assert_eq!(OwnershipTags::from_tags(&map), Some(scoped));
        assert!(validate_tags(&map).is_ok());
    }

    #[test]
    fn the_merge_body_names_the_merge_operation_and_the_tags() {
        let map = ownership().to_map();
        let body = merge_tags_body(&map);
        assert_eq!(body["operation"], TAGS_OPERATION_MERGE);
        assert_eq!(body["properties"]["tags"], serde_json::json!(map));
    }
}
