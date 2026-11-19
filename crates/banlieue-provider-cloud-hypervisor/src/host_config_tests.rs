// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `host_config.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::path::Path;

    /// Exactly what the shell bootstrap wrote before `banlieue host cloud-hypervisor install`
    /// replaced it (ADR-0067), comments included. Hosts prepared then still
    /// carry this file, so it must keep loading.
    const FROM_BOOTSTRAP: &str = r#"# banlieue Cloud Hypervisor host configuration.
# Written by scripts/bootstrap-cloud-hypervisor-host.sh.

[provider]
name = "bar"
namespace = "banlieue-system"
kubeconfig = "/etc/banlieue/kubeconfig"

[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd"

[paths]
run_root = "/run/banlieue/ch"
state_root = "/var/lib/banlieue"

[guests]
uid_base = 2000000
uid_count = 10000

[tpm]
swtpm = "/usr/bin/swtpm"
swtpm_setup = "/usr/bin/swtpm_setup"
setup_config = "/etc/banlieue/swtpm/swtpm_setup.conf"
ek_ca_certificate = "/var/lib/banlieue/swtpm-localca/issuercert.pem"

[storage_classes]
default = "/srv/banlieue/ch"

[network_classes]
default = "br-test"
"#;

    fn parsed() -> HostConfig {
        HostConfig::parse(FROM_BOOTSTRAP).expect("the bootstrap script's output must parse")
    }

    #[test]
    fn the_bootstrap_scripts_output_parses() {
        let c = parsed();
        assert_eq!(c.provider.name, "bar");
        assert_eq!(c.provider.namespace, "banlieue-system");
        assert_eq!(c.vmm.version, "v53.0");
        assert_eq!(
            c.vmm.firmware,
            Path::new("/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd")
        );
        assert_eq!(c.guests.uid_base, 2_000_000);
        assert_eq!(c.guests.uid_count, 10_000);
        assert_eq!(c.paths.run_root, Path::new("/run/banlieue/ch"));
        assert!(c.tpm.is_some());
    }

    #[test]
    fn classes_resolve_by_name() {
        let c = parsed();
        assert_eq!(
            c.storage_path("default"),
            Some(Path::new("/srv/banlieue/ch"))
        );
        assert_eq!(c.bridge("default"), Some("br-test"));
        assert_eq!(c.storage_path("gold"), None);
        assert_eq!(c.bridge("lan"), None);
    }

    /// What the provider publishes on its failure domain: names only, never
    /// the paths or bridges behind them (ADR-0062 Decision 4).
    #[test]
    fn published_class_names_are_names_only() {
        let c = parsed();
        assert_eq!(c.storage_class_names(), vec!["default".to_string()]);
        assert_eq!(c.network_class_names(), vec!["default".to_string()]);
    }

    /// A host without swtpm simply offers no vTPM.
    #[test]
    fn the_tpm_section_is_optional() {
        let without: String = FROM_BOOTSTRAP.split("[tpm]").next().unwrap().to_string()
            + "[storage_classes]\ndefault = \"/srv/banlieue/ch\"\n\n[network_classes]\ndefault = \"br0\"\n";
        let c = HostConfig::parse(&without).unwrap();
        assert!(c.tpm.is_none());
    }

    // ------------------------------------------------------------------
    // Refused
    // ------------------------------------------------------------------

    fn with(from: &str, to: &str) -> Result<HostConfig, HostConfigError> {
        assert!(FROM_BOOTSTRAP.contains(from), "fixture lacks {from:?}");
        HostConfig::parse(&FROM_BOOTSTRAP.replace(from, to))
    }

    /// A relative path would resolve against the provider's working
    /// directory, which nobody reasons about.
    #[test]
    fn a_relative_firmware_path_is_invalid() {
        let e = with(
            "firmware = \"/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd\"",
            "firmware = \"CLOUDHV.fd\"",
        )
        .unwrap_err();
        assert!(matches!(e, HostConfigError::Invalid(_)), "{e:?}");
    }

    #[test]
    fn a_config_that_could_not_be_served_is_refused() {
        for (case, from, to) in [
            // A typo in a key must fail loudly, not be silently ignored.
            ("unknown key", "uid_count = 10000", "uid_cuont = 10000"),
            (
                "relative storage path",
                "default = \"/srv/banlieue/ch\"",
                "default = \"srv/ch\"",
            ),
            ("empty uid range", "uid_count = 10000", "uid_count = 0"),
            (
                "uid range that overflows",
                "uid_base = 2000000",
                "uid_base = 4294967290",
            ),
            // uid 0 is root. A guest must never run as it.
            (
                "uid range including root",
                "uid_base = 2000000",
                "uid_base = 0",
            ),
            ("no storage class", "default = \"/srv/banlieue/ch\"", ""),
            ("no network class", "default = \"br-test\"", ""),
        ] {
            assert!(with(from, to).is_err(), "{case} should be refused");
        }
    }

    /// Linux interface names are at most 15 characters (IFNAMSIZ − 1).
    #[test]
    fn an_impossible_bridge_name_is_refused() {
        assert!(with("\"br-test\"", "\"a-bridge-name-too-long\"").is_err());
        assert!(with("\"br-test\"", "\"br/0\"").is_err());
        assert!(with("\"br-test\"", "\"\"").is_err());
    }

    /// Class names end up in CR fields and failure-domain attributes.
    #[test]
    fn a_class_name_that_is_not_a_dns_label_is_refused() {
        assert!(with("default = \"/srv", "Default = \"/srv").is_err());
        assert!(with("default = \"br-test\"", "\"lan.prod\" = \"br-test\"").is_err());
    }

    #[test]
    fn load_reads_a_file_and_names_it_in_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cloud-hypervisor.toml");
        std::fs::write(&path, FROM_BOOTSTRAP).unwrap();
        assert_eq!(HostConfig::load(&path).unwrap(), parsed());

        let missing = dir.path().join("absent.toml");
        let e = HostConfig::load(&missing).unwrap_err().to_string();
        assert!(e.contains("absent.toml"), "{e}");
    }

    fn with_registry(section: &str) -> String {
        FROM_BOOTSTRAP.replace(
            "[storage_classes]",
            &format!("{section}\n[storage_classes]"),
        )
    }

    /// No `[registry]`: this host serves `BackingFile` images only.
    #[test]
    fn the_registry_section_is_optional() {
        assert!(parsed().registry.is_none());
    }

    /// The host pulls from the one repository its owner named, so a
    /// reference written into the cluster cannot point it elsewhere.
    #[test]
    fn a_registry_section_names_one_repository() {
        let c = HostConfig::parse(&with_registry(
            "[registry]\nrepository = \"registry.internal:5000/banlieue/disks\"\n\
             credentials_dir = \"/etc/banlieue/registry\"\n",
        ))
        .unwrap();
        let r = c.registry.unwrap();
        assert_eq!(r.repository, "registry.internal:5000/banlieue/disks");
        assert_eq!(
            r.credentials_dir.as_deref(),
            Some(Path::new("/etc/banlieue/registry"))
        );
        assert!(!r.plain_http);
        assert_eq!(r.keep_unreferenced, DEFAULT_KEEP_UNREFERENCED);
    }

    #[test]
    fn a_bad_registry_section_is_refused() {
        for bad in [
            "[registry]\nrepository = \"disks\"\n",
            "[registry]\nrepository = \"registry.internal:5000/disks:v1\"\n",
            "[registry]\nrepository = \"registry.internal:5000/disks\"\ncredentials_dir = \"rel\"\n",
            "[registry]\nrepository = \"registry.internal:5000/disks\"\nmirror = true\n",
        ] {
            assert!(HostConfig::parse(&with_registry(bad)).is_err(), "{bad}");
        }
    }

    // ========================================================================
    // field_manager (ADR-0087)
    // ========================================================================

    /// Each host must present a different field manager, because each applies
    /// only its OWN `VMImage.status.perProvider` row. Sharing one made every
    /// host's apply delete the other hosts' rows, which showed up as a
    /// permanent write loop rather than an error.
    #[test]
    fn two_hosts_get_distinct_field_managers() {
        let a = HostConfig::parse(&FROM_BOOTSTRAP.replace(r#"name = "bar""#, r#"name = "host-a""#))
            .expect("loads");
        let b = HostConfig::parse(&FROM_BOOTSTRAP.replace(r#"name = "bar""#, r#"name = "host-b""#))
            .expect("loads");
        assert_ne!(
            a.field_manager(),
            b.field_manager(),
            "two Cloud Hypervisor hosts must not share a field manager"
        );
    }

    /// The manager names this host's Provider, which is what makes
    /// `managedFields` answer "which host wrote this row".
    #[test]
    fn the_field_manager_names_this_hosts_provider() {
        let config = HostConfig::parse(FROM_BOOTSTRAP).expect("loads");
        let manager = config.field_manager();
        assert!(
            manager.contains("bar"),
            "must name the Provider, got {manager}"
        );
        assert!(
            manager.contains("banlieue-system"),
            "must name the namespace, got {manager}"
        );
        assert!(
            manager.starts_with("banlieue.io/provider-cloud-hypervisor"),
            "must stay greppable by class, got {manager}"
        );
    }

    #[test]
    fn the_field_manager_is_stable_across_loads() {
        let once = HostConfig::parse(FROM_BOOTSTRAP)
            .expect("loads")
            .field_manager();
        let twice = HostConfig::parse(FROM_BOOTSTRAP)
            .expect("loads")
            .field_manager();
        assert_eq!(
            once, twice,
            "an unstable manager would orphan every row the last process owned"
        );
    }

    // ------------------------------------------------------------------
    // [hardening] (ADR-0139): what the operator accepted not to have
    // ------------------------------------------------------------------

    /// The section is optional: a host that never needed an acceptance
    /// carries none, and `hardening()` says so.
    #[test]
    fn the_hardening_section_is_optional() {
        let c = parsed();
        assert!(c.hardening.is_none());
        assert!(c.accepted_reduced_hardening().is_empty());
    }

    #[test]
    fn an_accepted_reduction_is_read_back() {
        let text = format!("{FROM_BOOTSTRAP}\n[hardening]\naccepted_reduced = [\"PrivateIPC\"]\n");
        let c = HostConfig::parse(&text).unwrap();
        assert_eq!(c.accepted_reduced_hardening(), ["PrivateIPC".to_string()]);
    }

    #[test]
    fn an_accepted_reduction_survives_a_serialize_round_trip() {
        let mut c = parsed();
        c.hardening = Some(HardeningSection {
            accepted_reduced: vec!["PrivateIPC".into()],
        });
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("[hardening]"), "{text}");
        assert_eq!(HostConfig::parse(&text).unwrap(), c);
    }

    /// No acceptance is no section at all, never an empty one.
    #[test]
    fn no_acceptance_serializes_no_section() {
        let text = toml::to_string(&parsed()).unwrap();
        assert!(!text.contains("hardening"), "{text}");
    }

    #[test]
    fn a_hardening_typo_or_a_bad_name_is_refused() {
        let base = format!("{FROM_BOOTSTRAP}\n[hardening]\n");
        assert!(HostConfig::parse(&format!("{base}accepted_reducd = []\n")).is_err());
        for bad in ["\"\"", "\"Private IPC\"", "\"Private=IPC\"", "\"a\\nb\""] {
            let text = format!("{base}accepted_reduced = [{bad}]\n");
            assert!(HostConfig::parse(&text).is_err(), "{bad} accepted");
        }
    }

    // ------------------------------------------------------------------
    // [attestation] (ADR-0146): the host opts in to a signer for its guests
    // ------------------------------------------------------------------

    const ATTESTATION: &str = "\n[attestation]\nca_dir = \"/var/lib/banlieue/attestation-ca\"\ncertificate_ttl_secs = 3600\n";

    #[test]
    fn the_attestation_section_is_optional() {
        assert!(parsed().attestation.is_none());
    }

    #[test]
    fn an_attestation_section_is_read_back() {
        let c = HostConfig::parse(&format!("{FROM_BOOTSTRAP}{ATTESTATION}")).unwrap();
        let a = c.attestation.expect("opted in");
        assert_eq!(a.ca_dir, Path::new("/var/lib/banlieue/attestation-ca"));
        assert_eq!(a.certificate_ttl_secs, 3600);
        assert_eq!(a.certificate_ttl(), std::time::Duration::from_secs(3600));
    }

    #[test]
    fn an_attestation_section_survives_a_serialize_round_trip() {
        let c = HostConfig::parse(&format!("{FROM_BOOTSTRAP}{ATTESTATION}")).unwrap();
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("[attestation]"), "{text}");
        assert_eq!(HostConfig::parse(&text).unwrap(), c);
    }

    #[test]
    fn no_attestation_serializes_no_section() {
        let text = toml::to_string(&parsed()).unwrap();
        assert!(!text.contains("attestation"), "{text}");
    }

    #[test]
    fn an_unknown_attestation_key_is_refused() {
        let text = format!("{FROM_BOOTSTRAP}{ATTESTATION}ca_name = \"x\"\n");
        assert!(HostConfig::parse(&text).is_err());
    }

    #[test]
    fn both_attestation_keys_are_required() {
        let no_ttl = format!("{FROM_BOOTSTRAP}\n[attestation]\nca_dir = \"/var/lib/ca\"\n");
        assert!(HostConfig::parse(&no_ttl).is_err());
        let no_dir = format!("{FROM_BOOTSTRAP}\n[attestation]\ncertificate_ttl_secs = 3600\n");
        assert!(HostConfig::parse(&no_dir).is_err());
    }

    #[test]
    fn a_relative_ca_dir_is_refused_naming_the_key() {
        let text = format!(
            "{FROM_BOOTSTRAP}\n[attestation]\nca_dir = \"attestation-ca\"\ncertificate_ttl_secs = 3600\n"
        );
        let err = HostConfig::parse(&text).unwrap_err().to_string();
        assert!(err.contains("attestation.ca_dir"), "{err}");
    }

    #[test]
    fn a_ttl_outside_the_signers_bounds_is_refused() {
        let min = banlieue_attestation_ca::MIN_CERTIFICATE_TTL.as_secs();
        let max = banlieue_attestation_ca::MAX_CERTIFICATE_TTL.as_secs();
        for bad in [0, min - 1, max + 1] {
            let text = format!(
                "{FROM_BOOTSTRAP}\n[attestation]\nca_dir = \"/var/lib/ca\"\ncertificate_ttl_secs = {bad}\n"
            );
            let err = HostConfig::parse(&text).unwrap_err().to_string();
            assert!(
                err.contains("attestation.certificate_ttl_secs"),
                "{bad}: {err}"
            );
        }
        for ok in [min, max] {
            let text = format!(
                "{FROM_BOOTSTRAP}\n[attestation]\nca_dir = \"/var/lib/ca\"\ncertificate_ttl_secs = {ok}\n"
            );
            assert!(HostConfig::parse(&text).is_ok(), "{ok} refused");
        }
    }
}
