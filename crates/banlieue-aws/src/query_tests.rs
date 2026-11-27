// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `query.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn pair(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

    #[test]
    fn new_sets_the_action_and_the_pinned_version() {
        let params = QueryParams::new("DescribeInstances");
        assert_eq!(params.get("Action"), Some("DescribeInstances"));
        assert_eq!(params.get("Version"), Some(EC2_API_VERSION));
        assert_eq!(EC2_API_VERSION, "2016-11-15");
        assert_eq!(
            params.encode(),
            "Action=DescribeInstances&Version=2016-11-15"
        );
    }

    #[test]
    fn with_version_sets_another_services_version() {
        let mut params = QueryParams::with_version("AssumeRoleWithWebIdentity", "2011-06-15");
        assert_eq!(params.get("Action"), Some("AssumeRoleWithWebIdentity"));
        assert_eq!(params.get("Version"), Some("2011-06-15"));
        assert!(
            params.push("Version", EC2_API_VERSION).is_err(),
            "the version is still set once"
        );
        assert_eq!(
            params.encode(),
            "Action=AssumeRoleWithWebIdentity&Version=2011-06-15"
        );
    }

    #[test]
    fn the_body_is_sorted_by_key_whatever_the_insertion_order() {
        let mut params = QueryParams::new("RunInstances");
        params.push("MinCount", "1").expect("push");
        params
            .extend(vec![
                pair("InstanceType", "m7i.large"),
                pair("ImageId", "ami-1"),
            ])
            .expect("extend");
        params.push("MaxCount", "1").expect("push");
        assert_eq!(
            params.encode(),
            "Action=RunInstances&ImageId=ami-1&InstanceType=m7i.large&MaxCount=1&MinCount=1&Version=2016-11-15"
        );
    }

    #[test]
    fn keys_and_values_use_sigv4_encoding() {
        let mut params = QueryParams::new("CreateTags");
        params
            .extend(vec![
                pair("Tag.1.Value", "a/b=c+d e"),
                pair("Tag.2.Value", "\u{e9}t\u{e9}"),
                pair("Tag.3.Value", "~-_."),
            ])
            .expect("extend");
        let body = params.encode();
        assert!(body.contains("Tag.1.Value=a%2Fb%3Dc%2Bd%20e"), "{body}");
        assert!(body.contains("Tag.2.Value=%C3%A9t%C3%A9"), "{body}");
        assert!(body.contains("Tag.3.Value=~-_."), "{body}");
        assert!(!body.contains('+'), "{body}");
    }

    #[test]
    fn the_body_sorts_by_encoded_key() {
        let mut params = QueryParams::new("A");
        params.push("Z", "1").expect("push");
        params.push("a b", "2").expect("push");
        params.push("Filter.1.Name", "3").expect("push");
        // '%' sorts before letters once encoded.
        assert_eq!(
            params.encode(),
            "Action=A&Filter.1.Name=3&Version=2016-11-15&Z=1&a%20b=2"
        );
    }

    #[test]
    fn a_duplicate_key_is_refused() {
        let mut params = QueryParams::new("RunInstances");
        params.push("ImageId", "ami-1").expect("push");
        assert!(matches!(
            params.push("ImageId", "ami-2"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            params.push("Action", "Other"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            params.push("Version", "2"),
            Err(Error::Invalid(_))
        ));
        assert_eq!(params.get("ImageId"), Some("ami-1"));
    }

    #[test]
    fn extend_with_a_duplicate_adds_nothing() {
        let mut params = QueryParams::new("RunInstances");
        let before = params.clone();
        let result = params.extend(vec![pair("A", "1"), pair("B", "2"), pair("A", "3")]);
        assert!(matches!(result, Err(Error::Invalid(_))));
        assert_eq!(params, before);

        let result = params.extend(vec![pair("C", "1"), pair("Action", "Other")]);
        assert!(matches!(result, Err(Error::Invalid(_))));
        assert_eq!(params, before);
    }

    #[test]
    fn an_empty_key_is_refused() {
        let mut params = QueryParams::new("RunInstances");
        assert!(matches!(params.push("", "x"), Err(Error::Invalid(_))));
    }

    #[test]
    fn an_empty_value_is_kept() {
        let mut params = QueryParams::new("A");
        params.push("Empty", "").expect("push");
        assert_eq!(params.encode(), "Action=A&Empty=&Version=2016-11-15");
    }

    #[test]
    fn the_content_type_is_form_urlencoded_utf8() {
        assert_eq!(
            FORM_CONTENT_TYPE,
            "application/x-www-form-urlencoded; charset=utf-8"
        );
    }

    #[test]
    fn the_body_matches_what_sigv4_signs_as_a_canonical_query() {
        let mut params = QueryParams::new("DescribeImages");
        params.push("Owner.1", "self").expect("push");
        let pairs: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(params.encode(), crate::sigv4::canonical_query(&pairs));
    }
}
