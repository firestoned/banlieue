// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `credentials.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use k8s_openapi::ByteString;
    use std::collections::BTreeMap;

    fn data(pairs: &[(&str, &str)]) -> BTreeMap<String, ByteString> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), ByteString(v.as_bytes().to_vec())))
            .collect()
    }

    #[test]
    fn a_username_and_token_value_build_credentials() {
        let c = credentials_from_secret(
            &data(&[
                ("username", "banlieue@pve!provider"),
                ("tokenValue", "s3cret\n"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(c.token.id(), "banlieue@pve!provider");
        assert_eq!(
            c.token.authorization(),
            "PVEAPIToken=banlieue@pve!provider=s3cret"
        );
        assert_eq!(c.ca_pem, None);
    }

    #[test]
    fn the_ca_bundle_is_carried_as_text() {
        let c = credentials_from_secret(
            &data(&[("username", "a@pve!t"), ("tokenValue", "s")]),
            Some(b"-----BEGIN CERTIFICATE-----\n".to_vec()),
        )
        .unwrap();
        assert!(c.ca_pem.unwrap().starts_with("-----BEGIN"));
    }

    #[test]
    fn a_missing_username_is_named() {
        let e = credentials_from_secret(&data(&[("tokenValue", "s")]), None).unwrap_err();
        assert!(e.to_string().contains("username"), "{e}");
    }

    #[test]
    fn a_missing_token_value_is_named() {
        let e = credentials_from_secret(&data(&[("username", "a@pve!t")]), None).unwrap_err();
        assert!(e.to_string().contains("tokenValue"), "{e}");
    }

    /// ADR-0074 Decision 3: a password Secret is not a credential here, and
    /// must fail as a token error rather than be tried as one.
    #[test]
    fn a_password_style_username_is_rejected_at_the_token_check() {
        let e = credentials_from_secret(
            &data(&[("username", "root@pam"), ("tokenValue", "hunter2")]),
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("token"), "{e}");
    }

    #[test]
    fn a_non_utf8_ca_bundle_is_rejected() {
        let e = credentials_from_secret(
            &data(&[("username", "a@pve!t"), ("tokenValue", "s")]),
            Some(vec![0xff, 0xfe]),
        )
        .unwrap_err();
        assert!(e.to_string().contains("caBundle"), "{e}");
    }
}
