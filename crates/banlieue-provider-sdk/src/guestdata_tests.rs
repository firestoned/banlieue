// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::guestdata`].

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::StaticIpamConfig;

    fn static_cfg() -> StaticIpamConfig {
        StaticIpamConfig {
            address: "10.0.0.90".to_string(),
            prefix: 24,
            gateway: Some("10.0.0.1".to_string()),
            nameservers: vec!["10.0.1.53".to_string(), "10.0.1.54".to_string()],
            domain: Some("k8s.example.internal".to_string()),
        }
    }

    #[test]
    fn substitutes_vm_name() {
        let ctx = GuestDataContext::from_static("bar01", None);
        assert_eq!(
            render_placeholders("hostname: ${VM_NAME}", &ctx),
            "hostname: bar01"
        );
    }

    #[test]
    fn substitutes_vm_name_multiple_times() {
        let ctx = GuestDataContext::from_static("bar01", None);
        assert_eq!(
            render_placeholders("${VM_NAME}-${VM_NAME}", &ctx),
            "bar01-bar01"
        );
    }

    #[test]
    fn substitutes_fqdn_with_domain() {
        let cfg = static_cfg();
        let ctx = GuestDataContext::from_static("bar01", Some(&cfg));
        assert_eq!(
            render_placeholders("fqdn: ${FQDN}", &ctx),
            "fqdn: bar01.k8s.example.internal"
        );
    }

    #[test]
    fn fqdn_falls_back_to_trailing_dot_with_no_domain() {
        let ctx = GuestDataContext::from_static("bar01", None);
        assert_eq!(render_placeholders("${FQDN}", &ctx), "bar01.");
    }

    #[test]
    fn fqdn_does_not_double_append_domain_when_vm_name_is_already_fully_qualified() {
        // metadata.name is a DNS-1123 subdomain and permits dots (confirmed
        // live: a VirtualMachine named as a full FQDN applies cleanly) — a
        // VM already named "bar01.k8s.example.internal" must not render as
        // "bar01.k8s.example.internal.k8s.example.internal".
        let cfg = static_cfg();
        let ctx = GuestDataContext::from_static("bar01.k8s.example.internal", Some(&cfg));
        assert_eq!(
            render_placeholders("${FQDN}", &ctx),
            "bar01.k8s.example.internal"
        );
    }

    #[test]
    fn fqdn_domain_suffix_match_is_case_insensitive() {
        let cfg = static_cfg();
        let ctx = GuestDataContext::from_static("bar01.K8S.EXAMPLE.INTERNAL", Some(&cfg));
        assert_eq!(
            render_placeholders("${FQDN}", &ctx),
            "bar01.K8S.EXAMPLE.INTERNAL"
        );
    }

    #[test]
    fn fqdn_still_appends_when_vm_name_only_partially_overlaps_domain() {
        // "bar01.example.internal" is not suffixed by ".k8s.example.internal"
        // — a partial/different domain must still get the full domain
        // appended, not be mistaken for already-qualified.
        let cfg = static_cfg();
        let ctx = GuestDataContext::from_static("bar01.example.internal", Some(&cfg));
        assert_eq!(
            render_placeholders("${FQDN}", &ctx),
            "bar01.example.internal.k8s.example.internal"
        );
    }

    #[test]
    fn substitutes_all_static_network_placeholders() {
        let cfg = static_cfg();
        let ctx = GuestDataContext::from_static("bar01", Some(&cfg));
        let raw = "${IP}/${PREFIX} via ${GATEWAY} dns ${DNS} domain ${DOMAIN}";
        assert_eq!(
            render_placeholders(raw, &ctx),
            "10.0.0.90/24 via 10.0.0.1 dns 10.0.1.53,10.0.1.54 domain k8s.example.internal"
        );
    }

    #[test]
    fn network_placeholders_are_empty_when_no_static_override() {
        let ctx = GuestDataContext::from_static("bar01", None);
        let raw = "${IP}|${PREFIX}|${GATEWAY}|${DNS}|${DOMAIN}";
        assert_eq!(render_placeholders(raw, &ctx), "||||");
    }

    #[test]
    fn gateway_is_empty_when_static_config_omits_it() {
        let cfg = StaticIpamConfig {
            address: "10.0.0.90".to_string(),
            prefix: 24,
            gateway: None,
            nameservers: Vec::new(),
            domain: None,
        };
        let ctx = GuestDataContext::from_static("bar01", Some(&cfg));
        assert_eq!(render_placeholders("${GATEWAY}", &ctx), "");
        assert_eq!(render_placeholders("${DNS}", &ctx), "");
    }

    #[test]
    fn unknown_placeholder_is_left_untouched() {
        // Deliberately not a general templating engine (ADR-0024): only the
        // fixed, documented set is substituted.
        let ctx = GuestDataContext::from_static("bar01", None);
        assert_eq!(
            render_placeholders("${NOT_A_REAL_PLACEHOLDER}", &ctx),
            "${NOT_A_REAL_PLACEHOLDER}"
        );
    }

    #[test]
    fn raw_without_any_placeholder_is_unchanged() {
        let ctx = GuestDataContext::from_static("bar01", None);
        let raw = "#cloud-config\ninstall:\n  auto: true\n";
        assert_eq!(render_placeholders(raw, &ctx), raw);
    }
}
