// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `host_config.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::path::Path;

    /// Exactly what `scripts/bootstrap-cloud-hypervisor-host.sh` writes (its
    /// `host` step, run in a Debian 13 container), comments included. If the
    /// script's output changes shape, this test is where it shows.
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

    /// A typo in a key must fail loudly, not be silently ignored.
    #[test]
    fn an_unknown_key_is_refused() {
        assert!(with("uid_count = 10000", "uid_cuont = 10000").is_err());
    }

    /// A relative path would resolve against the provider's working
    /// directory, which nobody reasons about.
    #[test]
    fn a_relative_path_is_refused() {
        let e = with(
            "firmware = \"/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd\"",
            "firmware = \"CLOUDHV.fd\"",
        )
        .unwrap_err();
        assert!(matches!(e, HostConfigError::Invalid(_)), "{e:?}");
        assert!(with("default = \"/srv/banlieue/ch\"", "default = \"srv/ch\"").is_err());
    }

    #[test]
    fn an_empty_uid_range_is_refused() {
        assert!(with("uid_count = 10000", "uid_count = 0").is_err());
    }

    #[test]
    fn a_uid_range_that_overflows_is_refused() {
        assert!(with("uid_base = 2000000", "uid_base = 4294967290").is_err());
    }

    /// uid 0 is root. A guest must never run as it.
    #[test]
    fn a_uid_range_including_root_is_refused() {
        assert!(with("uid_base = 2000000", "uid_base = 0").is_err());
    }

    #[test]
    fn no_storage_or_network_class_is_refused() {
        assert!(with("default = \"/srv/banlieue/ch\"", "").is_err());
        assert!(with("default = \"br-test\"", "").is_err());
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
}
