// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::ProviderConnection;
    use banlieue_proxmox::{ApiToken, FakeProxmox};

    fn connection(endpoint: &str) -> ProviderConnection {
        ProviderConnection {
            endpoint: endpoint.to_string(),
            credentials_ref: None,
            insecure_skip_tls_verify: false,
            ca_bundle: None,
        }
    }

    fn creds() -> Credentials {
        Credentials {
            token: ApiToken::new("banlieue@pve!provider", "s3cret").unwrap(),
            ca_pem: None,
        }
    }

    #[tokio::test]
    async fn the_http_factory_builds_a_client_for_an_https_endpoint() {
        install_default_crypto_provider();
        HttpClientFactory
            .build(&connection("https://bar.foo.io:8006"), &creds())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_http_factory_rejects_an_endpoint_without_a_scheme() {
        install_default_crypto_provider();
        let e = HttpClientFactory
            .build(&connection("bar.foo.io:8006"), &creds())
            .await
            .err()
            .expect("must fail");
        assert!(e.to_string().contains("scheme"), "{e}");
    }

    #[tokio::test]
    async fn the_http_factory_rejects_a_garbage_ca_bundle() {
        install_default_crypto_provider();
        let mut c = creds();
        c.ca_pem = Some("not a certificate".to_string());
        assert!(
            HttpClientFactory
                .build(&connection("https://bar.foo.io:8006"), &c)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_static_factory_hands_back_the_injected_api() {
        let fake = Arc::new(FakeProxmox::single_node("pve1"));
        let f = StaticClientFactory::new(fake);
        let api = f
            .build(&connection("https://unused.invalid"), &creds())
            .await
            .unwrap();
        assert_eq!(api.list_nodes().await.unwrap()[0].node, "pve1");
    }
}
