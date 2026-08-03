// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the shared CA-bundle resolver.
//!
//! Only [`plan`] is pure; the ConfigMap/Secret arms need a cluster and are
//! covered by each provider's own reconciler tests against a fake API.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::{CABundleSource, KeySelector};

    fn selector(name: &str) -> KeySelector {
        KeySelector {
            name: name.to_string(),
            key: None,
        }
    }

    #[test]
    fn no_source_means_no_bundle() {
        // Distinct from an error: vSphere treats this as "use the system trust
        // roots", libvirt rejects it. The shared layer must not decide.
        assert_eq!(plan(&None).expect("None is valid"), CABundlePlan::None);
    }

    #[test]
    fn inline_pem_is_returned_verbatim() {
        let src = CABundleSource {
            inline: Some("-----BEGIN CERTIFICATE-----".to_string()),
            ..Default::default()
        };
        assert_eq!(
            plan(&Some(src)).expect("inline is valid"),
            CABundlePlan::Inline("-----BEGIN CERTIFICATE-----")
        );
    }

    #[test]
    fn a_config_map_reference_is_classified_without_reading_it() {
        let sel = selector("corp-trust");
        let src = CABundleSource {
            config_map_ref: Some(sel.clone()),
            ..Default::default()
        };
        assert_eq!(
            plan(&Some(src)).expect("configMapRef is valid"),
            CABundlePlan::ConfigMap(&sel)
        );
    }

    #[test]
    fn a_secret_reference_carries_its_explicit_key_through() {
        let sel = KeySelector {
            name: "private-ca".to_string(),
            key: Some("bundle.pem".to_string()),
        };
        let src = CABundleSource {
            secret_ref: Some(sel.clone()),
            ..Default::default()
        };
        assert_eq!(
            plan(&Some(src)).expect("secretRef is valid"),
            CABundlePlan::Secret(&sel)
        );
    }

    #[test]
    fn more_than_one_source_is_rejected() {
        // Controller-side floor under the ValidatingAdmissionPolicy: a bundle
        // with two sources has no defined precedence, so picking one silently
        // would mean two clusters resolving the same spec differently.
        let src = CABundleSource {
            inline: Some("pem".to_string()),
            config_map_ref: Some(selector("ca")),
            ..Default::default()
        };
        let err = plan(&Some(src)).unwrap_err().to_string();
        assert!(err.contains("more than one"), "got: {err}");
        assert!(err.contains("caBundle"), "got: {err}");
    }

    #[test]
    fn an_empty_source_is_rejected() {
        let err = plan(&Some(CABundleSource::default()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("caBundle"), "got: {err}");
    }

    // ---------- secret value validation ----------------------------------

    #[test]
    fn a_pem_secret_value_passes_through_unchanged() {
        let pem = b"-----BEGIN CERTIFICATE-----\nMIIB\n".to_vec();
        assert_eq!(pem_from_secret_value(pem.clone()).expect("valid PEM"), pem);
    }

    #[test]
    fn a_binary_der_secret_value_is_rejected_by_name() {
        // Passing DER to the TLS stack fails with a far less obvious message
        // than saying so here.
        let der = vec![0x30, 0x82, 0x01, 0xff, 0x80];
        let err = pem_from_secret_value(der).unwrap_err().to_string();
        assert!(err.contains("not UTF-8"), "got: {err}");
        assert!(err.contains("secretRef"), "got: {err}");
    }
}
