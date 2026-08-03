// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn parses_a_full_qemu_tls_uri() {
        let (host, port) = parse_endpoint("qemu+tls://libvirt-host.example/system").unwrap();
        assert_eq!(host, "libvirt-host.example");
        assert_eq!(port, banlieue_libvirt::DEFAULT_TLS_PORT);
    }

    #[test]
    fn parses_an_explicit_port() {
        let (host, port) = parse_endpoint("qemu+tls://libvirt-host.example:16999/system").unwrap();
        assert_eq!(host, "libvirt-host.example");
        assert_eq!(port, 16999);
    }

    #[test]
    fn parses_a_bare_host() {
        let (host, port) = parse_endpoint("libvirt-host.example").unwrap();
        assert_eq!(host, "libvirt-host.example");
        assert_eq!(port, banlieue_libvirt::DEFAULT_TLS_PORT);
    }

    #[test]
    fn rejects_non_tls_schemes_loudly() {
        // Silently treating qemu+ssh:// as TLS would fail far from the cause,
        // and this provider supports mutual TLS only (ADR-0011).
        for bad in [
            "qemu+ssh://user@libvirt-host.example/system",
            "qemu:///system",
            "qemu+tcp://libvirt-host.example/system",
        ] {
            let err = parse_endpoint(bad).unwrap_err();
            assert!(
                matches!(err, Error::Invalid { .. }),
                "{bad} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn rejects_a_non_numeric_port() {
        assert!(parse_endpoint("qemu+tls://host:abc/system").is_err());
    }

    #[tokio::test]
    async fn fake_client_reports_its_inventory() {
        let c = FakeClient::with(&["default", "boot"], &["default"]);
        let pools = c.list_pools().await.unwrap();
        assert_eq!(
            pools.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["default", "boot"]
        );
        assert_eq!(c.list_networks().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn failing_fake_propagates_its_error() {
        let c = FakeClient::failing("connection refused");
        assert!(matches!(c.list_pools().await, Err(Error::Libvirt(_))));
    }
}
