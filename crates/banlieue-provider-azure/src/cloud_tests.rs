// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `cloud.rs`: only the known Azure clouds, each with its own
//! login host, ever receive a token.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn each_known_cloud_is_accepted_with_its_own_authority() {
        for cloud in &AZURE_CLOUDS {
            for endpoint in [
                cloud.arm_endpoint.to_string(),
                format!("{}/", cloud.arm_endpoint),
            ] {
                for authority in [
                    None,
                    Some(cloud.authority_host.to_string()),
                    Some(format!("{}/", cloud.authority_host)),
                ] {
                    let got = resolve_cloud(&endpoint, authority.as_deref())
                        .unwrap_or_else(|e| panic!("{endpoint} {authority:?}: {e}"));
                    assert_eq!(got, cloud);
                }
            }
        }
    }

    #[test]
    fn the_pairs_are_the_documented_ones() {
        let pairs: Vec<(&str, &str)> = AZURE_CLOUDS
            .iter()
            .map(|c| (c.arm_endpoint, c.authority_host))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (
                    "https://management.azure.com",
                    "https://login.microsoftonline.com"
                ),
                (
                    "https://management.usgovcloudapi.net",
                    "https://login.microsoftonline.us"
                ),
                (
                    "https://management.chinacloudapi.cn",
                    "https://login.chinacloudapi.cn"
                ),
            ]
        );
    }

    /// The token scope follows the cloud: a sovereign ARM refuses a token
    /// minted for public ARM.
    #[test]
    fn the_scope_is_the_cloud_s_own_arm() {
        for cloud in &AZURE_CLOUDS {
            assert_eq!(
                cloud.arm_scope(),
                format!("{}/.default", cloud.arm_endpoint)
            );
        }
    }

    #[test]
    fn an_empty_endpoint_is_public_azure() {
        assert_eq!(
            resolve_cloud("", None).expect("public").name,
            PUBLIC_CLOUD_NAME
        );
        assert!(resolve_cloud("", Some(US_GOV_AUTHORITY_HOST)).is_err());
    }

    #[test]
    fn a_mismatched_pair_is_refused() {
        for (endpoint, authority) in [
            (PUBLIC_ARM_ENDPOINT, US_GOV_AUTHORITY_HOST),
            (PUBLIC_ARM_ENDPOINT, CHINA_AUTHORITY_HOST),
            (US_GOV_ARM_ENDPOINT, PUBLIC_AUTHORITY_HOST),
            (CHINA_ARM_ENDPOINT, US_GOV_AUTHORITY_HOST),
        ] {
            let err = resolve_cloud(endpoint, Some(authority)).expect_err(authority);
            assert!(
                matches!(
                    err,
                    Error::Invalid {
                        what: AUTHORITY_FIELD,
                        ..
                    }
                ),
                "{endpoint} {authority}: {err}"
            );
        }
    }

    #[test]
    fn an_endpoint_that_is_not_a_known_arm_is_refused() {
        for endpoint in [
            "https://management.example.com",
            "http://management.azure.com",
            "https://management.azure.com.evil.example",
            "https://management.azure.com@evil.example",
            "https://evil.example@management.azure.com",
            "https://management.azure.com:8443",
            "https://management.azure.com:443",
            "https://management.azure.com/x",
            "https://management.azure.com//",
            "https://management.azure.com?x=1",
            "https://management.azure.com#f",
            "HTTPS://MANAGEMENT.AZURE.COM",
            "management.azure.com",
        ] {
            let err = resolve_cloud(endpoint, None).expect_err(endpoint);
            assert!(
                matches!(
                    err,
                    Error::Invalid {
                        what: ENDPOINT_FIELD,
                        ..
                    }
                ),
                "{endpoint}: {err}"
            );
        }
    }

    #[test]
    fn an_authority_that_is_not_the_cloud_s_login_host_is_refused() {
        for authority in [
            "https://login.example.com",
            "http://login.microsoftonline.com",
            "https://login.microsoftonline.com.evil.example",
            "https://login.microsoftonline.com@evil.example",
            "https://login.microsoftonline.com:8443",
            "https://login.microsoftonline.com/tenant",
        ] {
            let err = resolve_cloud(PUBLIC_ARM_ENDPOINT, Some(authority)).expect_err(authority);
            assert!(
                matches!(
                    err,
                    Error::Invalid {
                        what: AUTHORITY_FIELD,
                        ..
                    }
                ),
                "{authority}: {err}"
            );
        }
    }

    /// An empty or blank authority is "not set": the cloud's own login host.
    #[test]
    fn a_blank_authority_is_the_cloud_s_own() {
        let cloud = resolve_cloud(CHINA_ARM_ENDPOINT, Some("  ")).expect("china");
        assert_eq!(cloud.authority_host, CHINA_AUTHORITY_HOST);
    }
}
