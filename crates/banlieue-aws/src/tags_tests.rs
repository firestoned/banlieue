// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `tags.rs`.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::*;

    const UID: &str = "0b9c5f0e-7a40-4d27-9e2c-1c3a8f5d2b11";
    const DIGEST: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn ownership() -> OwnershipTags {
        OwnershipTags::new(UID, "EC2Machine", "banlieue-system", "aws-us-east-1")
    }

    fn extra(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn assert_invalid(result: Result<()>) {
        assert!(
            matches!(result, Err(Error::Invalid(_))),
            "expected Error::Invalid, got {result:?}"
        );
    }

    // ---- names: one set on both clouds (ADR-0158) ----

    #[test]
    fn tag_names_match_the_azure_provider() {
        assert_eq!(TAG_OWNER_UID, "banlieue-owner-uid");
        assert_eq!(TAG_OWNER_KIND, "banlieue-owner-kind");
        assert_eq!(TAG_PROVIDER, "banlieue-provider");
        assert_eq!(TAG_OWNER_NAMESPACE, "banlieue-owner-namespace");
        assert_eq!(TAG_SOURCE_SHA256, "banlieue-source-sha256");
        assert_eq!(TAG_NAME, "Name");
    }

    #[test]
    fn ownership_tags_render_as_a_map_ec2_accepts() {
        let map = ownership().to_map();
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(TAG_OWNER_UID).map(String::as_str), Some(UID));
        assert_eq!(
            map.get(TAG_OWNER_KIND).map(String::as_str),
            Some("EC2Machine")
        );
        assert_eq!(
            map.get(TAG_PROVIDER).map(String::as_str),
            Some("banlieue-system.aws-us-east-1")
        );
        assert!(validate_tags(&map).is_ok());
    }

    #[test]
    fn the_owner_namespace_is_added_when_given() {
        let map = ownership().with_owner_namespace("tenant-a").to_map();
        assert_eq!(map.len(), 4);
        assert_eq!(
            map.get(TAG_OWNER_NAMESPACE).map(String::as_str),
            Some("tenant-a")
        );
    }

    #[test]
    fn ownership_reads_back_from_tags() {
        let tags = ownership().with_owner_namespace("tenant-a").to_map();
        let read = OwnershipTags::from_tags(&tags);
        assert_eq!(read, Some(ownership().with_owner_namespace("tenant-a")));
    }

    #[test]
    fn ownership_does_not_read_back_without_all_three() {
        let mut tags = ownership().to_map();
        tags.remove(TAG_PROVIDER);
        assert_eq!(OwnershipTags::from_tags(&tags), None);
    }

    // ---- ownership checks ----

    #[test]
    fn owned_by_is_true_only_for_the_exact_uid() {
        let tags = ownership().to_map();
        assert!(owned_by(&tags, UID));
        assert!(ownership().owns(&tags));
    }

    #[test]
    fn owned_by_is_false_for_another_uid_or_no_uid() {
        let tags = ownership().to_map();
        assert!(!owned_by(&tags, "another-uid"));
        assert!(!owned_by(&tags, ""));
        assert!(!owned_by(&tags, &UID.to_uppercase()));
        assert!(!owned_by(&BTreeMap::new(), UID));
        assert!(!owned_by(&BTreeMap::new(), ""));
    }

    #[test]
    fn owned_by_ignores_a_uid_in_any_other_tag() {
        let tags = extra(&[(TAG_NAME, UID), ("Banlieue-Owner-Uid", UID)]);
        assert!(!owned_by(&tags, UID));
    }

    // ---- key and value rules ----

    #[test]
    fn a_key_of_one_to_128_characters_is_accepted() {
        assert!(validate_tag_key("a").is_ok());
        assert!(validate_tag_key(&"k".repeat(MAX_TAG_KEY_LEN)).is_ok());
    }

    #[test]
    fn an_empty_or_long_key_is_refused() {
        assert_invalid(validate_tag_key(""));
        assert_invalid(validate_tag_key(&"k".repeat(MAX_TAG_KEY_LEN + 1)));
    }

    #[test]
    fn key_length_counts_characters_not_bytes() {
        assert!(validate_tag_key(&"\u{e9}".repeat(MAX_TAG_KEY_LEN)).is_ok());
        assert_invalid(validate_tag_key(&"\u{e9}".repeat(MAX_TAG_KEY_LEN + 1)));
    }

    #[test]
    fn a_key_in_the_aws_prefix_is_refused_in_any_case() {
        assert_invalid(validate_tag_key("aws:cloudformation:stack-name"));
        assert_invalid(validate_tag_key("AWS:foo"));
        assert_invalid(validate_tag_key("Aws:foo"));
        assert_invalid(validate_tag_key("aws:"));
        assert!(validate_tag_key("awsfoo").is_ok());
        assert!(validate_tag_key("my:aws:thing").is_ok());
    }

    #[test]
    fn a_value_of_zero_to_256_characters_is_accepted() {
        assert!(validate_tag_value("").is_ok());
        assert!(validate_tag_value(&"v".repeat(MAX_TAG_VALUE_LEN)).is_ok());
        assert_invalid(validate_tag_value(&"v".repeat(MAX_TAG_VALUE_LEN + 1)));
    }

    #[test]
    fn ec2_documented_characters_are_accepted() {
        assert!(validate_tag_key("Cost Centre+1-2=3.4_5:6/7@8").is_ok());
        assert!(validate_tag_value("Cost Centre+1-2=3.4_5:6/7@8").is_ok());
        assert!(validate_tag_key("\u{e9}quipe").is_ok());
    }

    #[test]
    fn characters_outside_the_documented_set_are_refused() {
        for bad in [
            "a,b", "a;b", "a*b", "a<b", "a%b", "a\tb", "a\nb", "a#b", "a\"b",
        ] {
            assert_invalid(validate_tag_key(bad));
            assert_invalid(validate_tag_value(bad));
        }
    }

    #[test]
    fn at_most_50_tags_per_resource() {
        let full: BTreeMap<String, String> = (0..MAX_TAGS_PER_RESOURCE)
            .map(|i| (format!("k{i}"), String::new()))
            .collect();
        assert!(validate_tags(&full).is_ok());
        let mut over = full;
        over.insert("one-more".to_string(), String::new());
        assert_invalid(validate_tags(&over));
    }

    #[test]
    fn validate_tags_checks_every_key_and_value() {
        assert_invalid(validate_tags(&extra(&[("ok", "v"), ("aws:x", "v")])));
        assert_invalid(validate_tags(&extra(&[("ok", "bad,value")])));
    }

    // ---- configured extra tags (ADR-0172 for EC2) ----

    #[test]
    fn extra_tags_may_not_use_the_banlieue_prefix_in_any_case() {
        for key in [
            "banlieue-owner-uid",
            "Banlieue-Owner-Uid",
            "BANLIEUE-anything",
        ] {
            assert_invalid(validate_extra_tags(&extra(&[(key, "x")])));
        }
        assert!(validate_extra_tags(&extra(&[("banlieue", "x")])).is_ok());
    }

    #[test]
    fn extra_tags_may_not_set_the_name_tag() {
        assert_invalid(validate_extra_tags(&extra(&[(TAG_NAME, "x")])));
    }

    #[test]
    fn extra_tags_must_leave_room_for_banlieues_own() {
        assert_eq!(MAX_EXTRA_TAGS + BANLIEUE_TAG_COUNT, MAX_TAGS_PER_RESOURCE);
        let fits: BTreeMap<String, String> = (0..MAX_EXTRA_TAGS)
            .map(|i| (format!("cost-{i}"), "x".to_string()))
            .collect();
        assert!(validate_extra_tags(&fits).is_ok());
        let mut over = fits;
        over.insert("one-more".to_string(), "x".to_string());
        assert_invalid(validate_extra_tags(&over));
    }

    #[test]
    fn resource_tags_carry_ownership_name_digest_and_extra() {
        let tags = ownership()
            .with_owner_namespace("tenant-a")
            .resource_tags(Some("web-0"), Some(DIGEST), &extra(&[("CostCentre", "42")]))
            .unwrap();
        assert_eq!(tags.len(), BANLIEUE_TAG_COUNT + 1);
        assert_eq!(tags.get(TAG_NAME).map(String::as_str), Some("web-0"));
        assert_eq!(
            tags.get(TAG_SOURCE_SHA256).map(String::as_str),
            Some(DIGEST)
        );
        assert_eq!(tags.get("CostCentre").map(String::as_str), Some("42"));
        assert!(owned_by(&tags, UID));
    }

    #[test]
    fn resource_tags_fit_50_with_every_tag_banlieue_sets() {
        let most: BTreeMap<String, String> = (0..MAX_EXTRA_TAGS)
            .map(|i| (format!("cost-{i}"), "x".to_string()))
            .collect();
        let tags = ownership()
            .with_owner_namespace("tenant-a")
            .resource_tags(Some("web-0"), Some(DIGEST), &most)
            .unwrap();
        assert_eq!(tags.len(), MAX_TAGS_PER_RESOURCE);
    }

    #[test]
    fn resource_tags_refuse_a_forged_ownership_tag() {
        let result = ownership().resource_tags(None, None, &extra(&[(TAG_OWNER_UID, "x")]));
        assert!(matches!(result, Err(Error::Invalid(_))));
    }

    #[test]
    fn resource_tags_refuse_an_invalid_name() {
        let result = ownership().resource_tags(Some("web;0"), None, &BTreeMap::new());
        assert!(matches!(result, Err(Error::Invalid(_))));
    }

    #[test]
    fn resource_tags_refuse_an_invalid_owner_value() {
        let bad = OwnershipTags::new(UID, "EC2Machine", "ns", "name,with,commas");
        let result = bad.resource_tags(None, None, &BTreeMap::new());
        assert!(matches!(result, Err(Error::Invalid(_))));
    }

    // ---- EC2 Query encoding ----

    #[test]
    fn tag_filter_name_is_tag_colon_key() {
        assert_eq!(tag_filter_name(TAG_OWNER_UID), "tag:banlieue-owner-uid");
        assert_eq!(tag_filter_name(TAG_NAME), "tag:Name");
    }

    #[test]
    fn resource_types_render_ec2_strings() {
        assert_eq!(TagResourceType::Instance.as_str(), "instance");
        assert_eq!(TagResourceType::Volume.as_str(), "volume");
        assert_eq!(
            TagResourceType::NetworkInterface.as_str(),
            "network-interface"
        );
        assert_eq!(TagResourceType::Snapshot.as_str(), "snapshot");
        assert_eq!(TagResourceType::Image.as_str(), "image");
        assert_eq!(
            TagResourceType::ImportSnapshotTask.as_str(),
            "import-snapshot-task"
        );
        assert_eq!(TagResourceType::Volume.to_string(), "volume");
    }

    #[test]
    fn tag_specification_params_are_one_based_and_ordered() {
        let tags = extra(&[("b", "2"), ("a", "1")]);
        let params = tag_specification_params(2, TagResourceType::Volume, &tags);
        let expected: Vec<(String, String)> = [
            ("TagSpecification.2.ResourceType", "volume"),
            ("TagSpecification.2.Tag.1.Key", "a"),
            ("TagSpecification.2.Tag.1.Value", "1"),
            ("TagSpecification.2.Tag.2.Key", "b"),
            ("TagSpecification.2.Tag.2.Value", "2"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
        assert_eq!(params, expected);
    }

    #[test]
    fn tag_specification_params_keep_an_empty_value() {
        let params = tag_specification_params(1, TagResourceType::Instance, &extra(&[("k", "")]));
        assert!(params.contains(&("TagSpecification.1.Tag.1.Value".to_string(), String::new())));
    }

    #[test]
    fn tag_specification_params_with_no_tags_name_only_the_type() {
        let params = tag_specification_params(1, TagResourceType::Image, &BTreeMap::new());
        assert_eq!(
            params,
            vec![(
                "TagSpecification.1.ResourceType".to_string(),
                "image".to_string()
            )]
        );
    }
}
