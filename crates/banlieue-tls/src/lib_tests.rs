// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the process crypto provider (ADR-0170).

#[cfg(test)]
mod tests {
    use super::super::*;
    use rustls::NamedGroup;

    #[test]
    fn provider_offers_x25519mlkem768_first() {
        let p = provider();
        assert_eq!(p.kx_groups[0].name(), NamedGroup::X25519MLKEM768);
    }

    #[test]
    fn provider_keeps_classical_fallbacks_after_the_hybrid() {
        let names: Vec<NamedGroup> = provider().kx_groups.iter().map(|g| g.name()).collect();
        assert_eq!(
            names,
            vec![
                NamedGroup::X25519MLKEM768,
                NamedGroup::X25519,
                NamedGroup::secp256r1,
                NamedGroup::secp384r1,
            ]
        );
    }

    #[test]
    fn provider_builds_a_client_config() {
        // rustls rejects an incoherent provider (no usable suite or group).
        let built = rustls::ClientConfig::builder_with_provider(Arc::new(provider()))
            .with_safe_default_protocol_versions();
        assert!(built.is_ok());
    }

    #[test]
    fn installed_default_offers_ml_kem_first_so_it_is_not_ring() {
        // `ring` implements no ML-KEM group, so a ring provider can never
        // satisfy this; deny.toml keeps ring out of the graph as well.
        let installed = install_default_crypto_provider();
        assert_eq!(installed.kx_groups[0].name(), NamedGroup::X25519MLKEM768);
        let global = CryptoProvider::get_default().expect("a provider is installed");
        assert_eq!(global.kx_groups[0].name(), NamedGroup::X25519MLKEM768);
    }

    #[test]
    fn install_is_idempotent() {
        let first = install_default_crypto_provider();
        let second = install_default_crypto_provider();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn rustls_crate_default_order_also_prefers_the_hybrid() {
        // Anything that installs aws-lc-rs's own default (kube-client does in
        // some paths) must agree with ours: `prefer-post-quantum` is on.
        let d = aws_lc_rs::default_provider();
        assert_eq!(d.kx_groups[0].name(), NamedGroup::X25519MLKEM768);
    }
}
