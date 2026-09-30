// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `VMImage` reconciler's pure core.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{ImageSource, ImageSourceKind, ProviderSpec};
    use banlieue_api::banlieue::{ProviderCapabilities, ProviderConnection};
    use banlieue_api::common::LocalObjectReference;
    use banlieue_proxmox::FakeProxmox;
    use kube::api::ObjectMeta;

    fn provider() -> Provider {
        Provider {
            metadata: ObjectMeta {
                name: Some("pve-a".into()),
                namespace: Some("banlieue-system".into()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "proxmox".into(),
                },
                connection: ProviderConnection {
                    endpoint: "https://bar.foo.io:8006".into(),
                    credentials_ref: None,
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities::default(),
                paused: false,
                use_content_library: false,
                failure_domain_name_overrides: Vec::new(),
                attestation: None,
            },
            status: None,
        }
    }

    fn source(class: &str, kind: ImageSourceKind, reference: &str) -> ImageSource {
        ImageSource {
            provider_class: class.into(),
            kind,
            reference: reference.into(),
            import_from: None,
            checksum: None,
        }
    }

    #[test]
    fn only_a_proxmox_source_is_ours() {
        let sources = vec![
            source("vsphere", ImageSourceKind::Template, "ubuntu"),
            source("proxmox", ImageSourceKind::Template, "9000"),
        ];
        assert_eq!(find_proxmox_source(&sources).unwrap().reference, "9000");
        assert!(find_proxmox_source(&sources[..1]).is_none());
        assert!(find_proxmox_source(&[]).is_none());
    }

    #[tokio::test]
    async fn a_template_vmid_is_ready_and_published_as_the_resolved_ref() {
        let f = FakeProxmox::single_node("pve1");
        f.add_template("pve1", 9000, "ubuntu-2404");
        let row = row_for(
            &f,
            &provider(),
            &source("proxmox", ImageSourceKind::Template, "9000"),
        )
        .await;
        assert!(row.ready, "{row:?}");
        assert_eq!(row.resolved_ref.as_deref(), Some("9000"));
        assert_eq!(row.provider_name, "pve-a");
        assert_eq!(row.provider_namespace, "banlieue-system");
        assert!(row.zones.is_empty());
    }

    #[tokio::test]
    async fn a_missing_vmid_is_not_ready_and_names_it() {
        let f = FakeProxmox::single_node("pve1");
        let row = row_for(
            &f,
            &provider(),
            &source("proxmox", ImageSourceKind::Template, "9000"),
        )
        .await;
        assert!(!row.ready);
        assert_eq!(row.resolved_ref, None);
        assert_eq!(row.reason.as_deref(), Some(reasons::TEMPLATE_NOT_FOUND));
        assert!(row.message.unwrap().contains("9000"));
    }

    /// A live guest must never be advertised as a clone source.
    #[tokio::test]
    async fn a_vmid_that_is_not_a_template_is_not_ready() {
        let f = FakeProxmox::single_node("pve1");
        f.add_vm("pve1", 9000, "live");
        let row = row_for(
            &f,
            &provider(),
            &source("proxmox", ImageSourceKind::Template, "9000"),
        )
        .await;
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::NOT_A_TEMPLATE));
        assert_eq!(row.resolved_ref, None);
    }

    #[tokio::test]
    async fn a_non_numeric_ref_is_invalid() {
        let f = FakeProxmox::single_node("pve1");
        let row = row_for(
            &f,
            &provider(),
            &source("proxmox", ImageSourceKind::Template, "ubuntu"),
        )
        .await;
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::INVALID_REF));
    }

    #[tokio::test]
    async fn url_and_backing_file_sources_are_unsupported() {
        let f = FakeProxmox::single_node("pve1");
        for kind in [ImageSourceKind::Url, ImageSourceKind::BackingFile] {
            let row = row_for(&f, &provider(), &source("proxmox", kind, "9000")).await;
            assert!(!row.ready);
            assert_eq!(
                row.reason.as_deref(),
                Some(reasons::UNSUPPORTED_SOURCE_KIND)
            );
        }
    }

    #[test]
    fn a_failure_row_never_carries_a_resolved_ref() {
        let row = failure_row(&provider(), reasons::CONNECT_FAILED, "refused".into());
        assert!(!row.ready);
        assert_eq!(row.resolved_ref, None);
        assert_eq!(row.message.as_deref(), Some("refused"));
    }

    #[test]
    fn api_errors_map_to_stable_reasons() {
        use banlieue_proxmox::Error as E;
        assert_eq!(
            api_failure_reason(&E::Api {
                status: 401,
                message: "x".into()
            }),
            reasons::UNAUTHORIZED
        );
        assert_eq!(
            api_failure_reason(&E::Transport("x".into())),
            reasons::CONNECT_FAILED
        );
    }
}
