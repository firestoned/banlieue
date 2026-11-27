// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `aws.rs`.
//!
//! Every shape is checked twice over the same cases: by its Rust function and
//! by the regular expression the CRD schema carries, so the two cannot drift.
//! Where the Rust check is deliberately stricter than a regular expression can
//! say (an S3 bucket's reserved prefixes, an IAM path's length), the case
//! records both answers.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::collections::BTreeMap;

    const ACCOUNT: &str = "111122223333";
    const UUID: &str = "1234abcd-12ab-34cd-56ef-1234567890ab";

    /// (input, Rust answer, schema answer); the two agree unless noted.
    fn agree(pattern: &str, rust: fn(&str) -> bool, cases: &[(&str, bool, bool)]) {
        let re = regex::Regex::new(pattern).expect("pattern compiles");
        for (input, want_rust, want_schema) in cases {
            assert_eq!(rust(input), *want_rust, "rust check of {input:?}");
            assert_eq!(
                re.is_match(input),
                *want_schema,
                "schema check of {input:?}"
            );
        }
    }

    fn same(cases: &[(&'static str, bool)]) -> Vec<(&'static str, bool, bool)> {
        cases.iter().map(|(i, ok)| (*i, *ok, *ok)).collect()
    }

    #[test]
    fn regions() {
        agree(
            REGION_PATTERN,
            is_region,
            &same(&[
                ("ca-central-1", true),
                ("us-east-1", true),
                ("ap-southeast-2", true),
                ("us-gov-west-1", true),
                ("cn-north-1", true),
                ("eu-central-12", true),
                ("", false),
                ("ca-central", false),
                ("ca-central-1a", false),
                ("CA-central-1", false),
                ("ca-central-123", false),
                ("us-iso-east-1", false),
                ("c-central-1", false),
                ("ca--1", false),
            ]),
        );
    }

    #[test]
    fn partitions_follow_the_region() {
        assert_eq!(partition_of_region("ca-central-1"), "aws");
        assert_eq!(partition_of_region("us-gov-west-1"), "aws-us-gov");
        assert_eq!(partition_of_region("cn-north-1"), "aws-cn");
        assert_eq!(dns_suffix_of_region("ca-central-1"), "amazonaws.com");
        assert_eq!(dns_suffix_of_region("us-gov-east-1"), "amazonaws.com");
        assert_eq!(dns_suffix_of_region("cn-northwest-1"), "amazonaws.com.cn");
    }

    #[test]
    fn ami_ids() {
        agree(
            AMI_ID_PATTERN,
            is_ami_id,
            &same(&[
                ("ami-12345678", true),
                ("ami-0123456789abcdef0", true),
                ("ami-0123456789ABCDEF0", false),
                ("ami-123456789", false),
                ("ami-", false),
                ("ami-0123456789abcdef", false),
                ("ami-0123456789abcdefg", false),
                ("snap-12345678", false),
                (" ami-12345678", false),
            ]),
        );
    }

    #[test]
    fn subnet_security_group_and_instance_ids() {
        agree(
            SUBNET_ID_PATTERN,
            is_subnet_id,
            &same(&[
                ("subnet-0123456789abcdef0", true),
                ("subnet-12345678", true),
                ("subnet-xyz", false),
                ("sg-12345678", false),
            ]),
        );
        agree(
            SECURITY_GROUP_ID_PATTERN,
            is_security_group_id,
            &same(&[
                ("sg-0123456789abcdef0", true),
                ("sg-12345678", true),
                ("sg-1234567", false),
                ("subnet-12345678", false),
            ]),
        );
        assert!(is_instance_id("i-0123456789abcdef0"));
        assert!(is_instance_id("i-12345678"));
        assert!(!is_instance_id("i-"));
        assert!(!is_instance_id("ami-12345678"));
    }

    #[test]
    fn role_arns() {
        let long_path = format!("arn:aws:iam::{ACCOUNT}:role/{}/name", "p".repeat(510));
        let too_long_path = format!("arn:aws:iam::{ACCOUNT}:role/{}/name", "p".repeat(511));
        let cases: Vec<(String, bool)> = vec![
            (format!("arn:aws:iam::{ACCOUNT}:role/banlieue-ec2"), true),
            (
                format!("arn:aws:iam::{ACCOUNT}:role/banlieue/ec2-provider"),
                true,
            ),
            (format!("arn:aws:iam::{ACCOUNT}:role/a/b/c+=,.@_-"), true),
            (format!("arn:aws-cn:iam::{ACCOUNT}:role/r"), true),
            (format!("arn:aws-us-gov:iam::{ACCOUNT}:role/r"), true),
            (long_path, true),
            (too_long_path, false),
            (
                format!("arn:aws:iam::{ACCOUNT}:role/{}", "n".repeat(64)),
                true,
            ),
            (
                format!("arn:aws:iam::{ACCOUNT}:role/{}", "n".repeat(65)),
                false,
            ),
            (format!("arn:aws:iam::{ACCOUNT}:role/"), false),
            (format!("arn:aws:iam::{ACCOUNT}:role/path/"), false),
            (format!("arn:aws:iam::{ACCOUNT}:user/r"), false),
            ("arn:aws:iam::11112222333:role/r".to_string(), false),
            ("arn:aws:iam::1111222233334:role/r".to_string(), false),
            (format!("arn:aws:sts::{ACCOUNT}:role/r"), false),
            (format!("arn:aws-iso:iam::{ACCOUNT}:role/r"), false),
            (format!("arn:aws:iam:ca-central-1:{ACCOUNT}:role/r"), false),
            (format!("arn:aws:iam::{ACCOUNT}:role/r n"), false),
        ];
        let re = regex::Regex::new(IAM_ROLE_ARN_PATTERN).unwrap();
        for (arn, ok) in &cases {
            assert_eq!(is_iam_role_arn(arn), *ok, "rust check of {arn}");
            assert_eq!(re.is_match(arn), *ok, "schema check of {arn}");
        }
        assert_eq!(
            account_of_role_arn(&format!("arn:aws:iam::{ACCOUNT}:role/x")),
            Some(ACCOUNT)
        );
        assert_eq!(
            partition_of_arn(&format!("arn:aws-cn:iam::{ACCOUNT}:role/x")),
            Some("aws-cn")
        );
        assert_eq!(account_of_role_arn("not-an-arn"), None);
    }

    #[test]
    fn role_names() {
        agree(
            IAM_ROLE_NAME_PATTERN,
            is_iam_role_name,
            &same(&[
                ("vmimport", true),
                ("banlieue-vmimport+=,.@_", true),
                ("", false),
                ("a/b", false),
                ("has space", false),
            ]),
        );
        assert!(is_iam_role_name(&"r".repeat(64)));
        assert!(!is_iam_role_name(&"r".repeat(65)));
    }

    #[test]
    fn bucket_names() {
        agree(
            S3_BUCKET_NAME_PATTERN,
            is_s3_bucket_name,
            &[
                ("banlieue-staging", true, true),
                ("abc", true, true),
                ("a1-2b", true, true),
                ("ab", false, false),
                ("Banlieue", false, false),
                ("-abc", false, false),
                ("abc-", false, false),
                // Dots break the wildcard certificate of virtual-hosted-style
                // HTTPS, so they are refused outright.
                ("bucket.with.dots", false, false),
                ("under_score", false, false),
                // Reserved by S3; the schema cannot say so, the Rust check does.
                ("xn--abc", false, true),
                ("sthree-abc", false, true),
                ("amzn-s3-demo-abc", false, true),
                ("abc-s3alias", false, true),
                ("abc--ol-s3", false, true),
                ("abc--x-s3", false, true),
                ("abc--table-s3", false, true),
            ],
        );
        assert!(is_s3_bucket_name(&"b".repeat(63)));
        assert!(!is_s3_bucket_name(&"b".repeat(64)));
    }

    #[test]
    fn kms_keys() {
        let key_arn = format!("arn:aws:kms:ca-central-1:{ACCOUNT}:key/{UUID}");
        let mrk_arn =
            format!("arn:aws:kms:ca-central-1:{ACCOUNT}:key/mrk-0123456789abcdef0123456789abcdef");
        let alias_arn = format!("arn:aws:kms:ca-central-1:{ACCOUNT}:alias/banlieue-ebs");
        let cases: Vec<(String, bool)> = vec![
            (UUID.to_string(), true),
            ("mrk-0123456789abcdef0123456789abcdef".to_string(), true),
            (key_arn, true),
            (mrk_arn, true),
            (alias_arn, true),
            ("alias/banlieue-ebs".to_string(), true),
            ("alias/aws/ebs".to_string(), true),
            (
                format!("arn:aws-cn:kms:cn-north-1:{ACCOUNT}:key/{UUID}"),
                true,
            ),
            ("alias/".to_string(), false),
            ("alias/has space".to_string(), false),
            ("1234abcd".to_string(), false),
            ("mrk-0123".to_string(), false),
            (
                format!("arn:aws:kms:ca-central-1:{ACCOUNT}:key/not-a-key"),
                false,
            ),
            (format!("arn:aws:kms:ca-central-1:1111:key/{UUID}"), false),
            (format!("arn:aws:iam::{ACCOUNT}:key/{UUID}"), false),
            (format!("arn:aws:kms::{ACCOUNT}:key/{UUID}"), false),
            (String::new(), false),
        ];
        let re = regex::Regex::new(KMS_KEY_PATTERN).unwrap();
        for (key, ok) in &cases {
            assert_eq!(is_kms_key(key), *ok, "rust check of {key}");
            assert_eq!(re.is_match(key), *ok, "schema check of {key}");
        }
    }

    #[test]
    fn service_endpoints_are_the_regional_name_or_a_vpc_endpoint_of_it() {
        let region = "ca-central-1";
        for (url, ok) in [
            ("https://ec2.ca-central-1.amazonaws.com", true),
            ("https://ec2.ca-central-1.amazonaws.com/", true),
            (
                "https://vpce-0123456789abcdef0-abcd1234.ec2.ca-central-1.vpce.amazonaws.com",
                true,
            ),
            (
                "https://vpce-0123456789abcdef0-abcd1234.ec2.ca-central-1.vpce.amazonaws.com/",
                true,
            ),
            ("http://ec2.ca-central-1.amazonaws.com", false),
            ("https://ec2.us-east-1.amazonaws.com", false),
            ("https://ec2.amazonaws.com", false),
            ("https://ec2.ca-central-1.amazonaws.com//", false),
            ("https://ec2.ca-central-1.amazonaws.com/path", false),
            ("https://ec2.ca-central-1.amazonaws.com.example.com", false),
            ("https://ec2.ca-central-1.amazonaws.com.cn", false),
            (
                "https://vpce-0123456789abcdef0-abcd1234.sts.ca-central-1.vpce.amazonaws.com",
                false,
            ),
            (
                "https://vpce-0123-abcd1234.ec2.ca-central-1.vpce.amazonaws.com",
                false,
            ),
            (
                "https://vpce-0123456789abcdef0-ABCD1234.ec2.ca-central-1.vpce.amazonaws.com",
                false,
            ),
            ("https://user@ec2.ca-central-1.amazonaws.com", false),
        ] {
            assert_eq!(is_service_endpoint(url, "ec2", region), ok, "{url}");
        }
        assert!(is_service_endpoint(
            "https://sts.ca-central-1.amazonaws.com",
            "sts",
            region
        ));
        assert!(is_service_endpoint(
            "https://ec2.cn-north-1.amazonaws.com.cn",
            "ec2",
            "cn-north-1"
        ));
        assert!(!is_service_endpoint(
            "https://ec2.cn-north-1.amazonaws.com",
            "ec2",
            "cn-north-1"
        ));
        assert_eq!(
            regional_endpoint("sts", "ca-central-1"),
            "https://sts.ca-central-1.amazonaws.com"
        );
    }

    fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn extra_tags_that_ec2_and_banlieue_accept() {
        assert_eq!(validate_extra_tags(&BTreeMap::new()), Ok(()));
        assert_eq!(
            validate_extra_tags(&tags(&[
                ("cost-centre", "1234"),
                ("team:owner", "platform@bar.foo.io"),
                ("path/like", "a + b = c"),
                ("empty", ""),
                ("banlieue", "no dash so not ours"),
                ("name", "lowercase is another key"),
            ])),
            Ok(())
        );
        let many: BTreeMap<String, String> = (0..MAX_EXTRA_TAGS)
            .map(|i| (format!("t{i}"), "x".to_string()))
            .collect();
        assert_eq!(validate_extra_tags(&many), Ok(()));
        let key = "k".repeat(MAX_TAG_KEY_LEN);
        let value = "v".repeat(MAX_TAG_VALUE_LEN);
        assert_eq!(validate_extra_tags(&tags(&[(&key, &value)])), Ok(()));
    }

    #[test]
    fn extra_tags_that_are_refused() {
        let too_many: BTreeMap<String, String> = (0..=MAX_EXTRA_TAGS)
            .map(|i| (format!("t{i}"), "x".to_string()))
            .collect();
        assert!(
            validate_extra_tags(&too_many)
                .unwrap_err()
                .contains("at most")
        );
        for (key, value, why) in [
            ("banlieue-owner-uid", "x", "banlieue-"),
            ("Banlieue-Owner-Uid", "x", "banlieue-"),
            ("BANLIEUE-SOURCE-SHA256", "x", "banlieue-"),
            ("aws:cloudformation:stack-name", "x", "aws:"),
            ("AWS:x", "x", "aws:"),
            ("Name", "x", "Name"),
            ("", "x", "1 to 128"),
            ("caf\u{e9}", "x", "letters"),
            ("semi;colon", "x", "letters"),
            ("k", "comma,value", "letters"),
            ("k", "tab\tvalue", "letters"),
        ] {
            let err = validate_extra_tags(&tags(&[(key, value)])).unwrap_err();
            assert!(err.contains(why), "{key:?}={value:?}: {err}");
        }
        let long_key = "k".repeat(MAX_TAG_KEY_LEN + 1);
        assert!(
            validate_extra_tags(&tags(&[(&long_key, "x")]))
                .unwrap_err()
                .contains("1 to 128")
        );
        let long_value = "v".repeat(MAX_TAG_VALUE_LEN + 1);
        assert!(
            validate_extra_tags(&tags(&[("k", &long_value)]))
                .unwrap_err()
                .contains("256")
        );
    }

    /// The admission policy spells the tag character set as a regular
    /// expression; it is the same set as the Rust check.
    #[test]
    fn the_tag_character_pattern_is_the_rust_set() {
        let re = regex::Regex::new(TAG_CHARACTERS_PATTERN).unwrap();
        for c in (0u8..=0x7f).map(char::from) {
            let s = c.to_string();
            assert_eq!(re.is_match(&s), is_tag_character(c), "{c:?}");
        }
        assert!(!re.is_match("\u{e9}"));
    }
}
