// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for caBundle classification (`plan`) in `ca_bundle.rs`.
//!
//! The ConfigMap/Secret read paths need a live API and are covered by
//! integration tests; here we exhaustively test the pure classification +
//! "exactly one source" enforcement, which decides everything before any I/O.

#[cfg(test)]
mod tests {
    use super::super::{Plan, plan};
    use banlieue_api::common::{CABundleSource, KeySelector};

    #[test]
    fn plan_none_when_unset() {
        assert_eq!(plan(&None).unwrap(), Plan::None);
    }

    #[test]
    fn plan_inline_returns_pem() {
        let src = Some(CABundleSource {
            inline: Some("PEMDATA".to_string()),
            ..Default::default()
        });
        assert_eq!(plan(&src).unwrap(), Plan::Inline("PEMDATA"));
    }

    #[test]
    fn plan_config_map_ref() {
        let sel = KeySelector {
            name: "corp-trust".to_string(),
            key: None,
        };
        let src = Some(CABundleSource {
            config_map_ref: Some(sel.clone()),
            ..Default::default()
        });
        assert_eq!(plan(&src).unwrap(), Plan::ConfigMap(&sel));
    }

    #[test]
    fn plan_secret_ref() {
        let sel = KeySelector {
            name: "private-ca".to_string(),
            key: Some("bundle.pem".to_string()),
        };
        let src = Some(CABundleSource {
            secret_ref: Some(sel.clone()),
            ..Default::default()
        });
        assert_eq!(plan(&src).unwrap(), Plan::Secret(&sel));
    }

    #[test]
    fn plan_rejects_zero_sources() {
        let src = Some(CABundleSource::default());
        let err = plan(&src).unwrap_err();
        assert!(err.to_string().contains("caBundle"), "got: {err}");
    }

    #[test]
    fn plan_rejects_multiple_sources() {
        let src = Some(CABundleSource {
            inline: Some("PEM".to_string()),
            secret_ref: Some(KeySelector {
                name: "s".to_string(),
                key: None,
            }),
            ..Default::default()
        });
        let err = plan(&src).unwrap_err();
        assert!(err.to_string().contains("more than one"), "got: {err}");
    }
}
