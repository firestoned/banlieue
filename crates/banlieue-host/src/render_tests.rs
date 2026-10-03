// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `render.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::fake::FakeHost;
    use crate::settings::{Registry, Settings};

    fn source(a: &crate::pins::Artifact) -> crate::pins::Source {
        crate::pins::Source {
            url: a.url.clone(),
            sha256: a.sha256.clone(),
        }
    }

    fn settings() -> Settings {
        Settings {
            provider_name: "bar".into(),
            namespace: "banlieue-system".into(),
            storage: vec![
                ("default".into(), "/srv/banlieue/ch".into()),
                ("fast".into(), "/nvme/banlieue/ch".into()),
            ],
            network: vec![("default".into(), "virbr0".into())],
            uid_base: 2_000_000,
            uid_count: 1024,
            registry: None,
            allow_virtualized: false,
        }
    }

    /// Every template the installer ships renders completely.
    #[test]
    fn every_template_renders_without_a_placeholder_left() {
        let h = FakeHost::debian();
        let v = values(&settings(), &h);
        for t in UNIT_TEMPLATES
            .iter()
            .chain([&PROVIDER_UNIT, &TMPFILES, &POLKIT_RULE])
        {
            let text = render(t, &v).unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert!(!text.contains("@BANLIEUE_USER@"), "{}", t.name);
        }
    }

    #[test]
    fn values_carry_the_layout_the_provider_expects() {
        let h = FakeHost::debian();
        let v = values(&settings(), &h);
        assert_eq!(v["STORAGE_DIRS"], "/srv/banlieue/ch /nvme/banlieue/ch");
        assert_eq!(
            v["STORAGE_IMAGE_DIRS"],
            "/srv/banlieue/ch/images /nvme/banlieue/ch/images"
        );
        assert_eq!(
            v["PROVIDER_RW_PATHS"],
            "/run/banlieue/ch /srv/banlieue/ch /nvme/banlieue/ch \
             /var/lib/banlieue/ek /var/lib/banlieue/tpm /var/lib/banlieue/units"
        );
        assert_eq!(v["RUN_PARENT"], "/run/banlieue");
        assert_eq!(v["GUEST_UID_BASE"], "2000000");
        let rule = render(&POLKIT_RULE, &v).unwrap();
        assert!(rule.contains("2000000") && rule.contains("1024"));
    }

    /// A unit shipped with `@NAME@` in it would fail on the host; it fails
    /// here instead. A unit name's `@` is not a placeholder.
    #[test]
    fn a_placeholder_nobody_fills_is_an_error() {
        let t = Template {
            name: "x.service",
            text: "Description=banlieue-ch@.service %i@host\nExecStart=@NOT_A_VALUE@\n",
        };
        match render(&t, &BTreeMap::new()) {
            Err(Error::Template { placeholder, .. }) => assert_eq!(placeholder, "@NOT_A_VALUE@"),
            other => panic!("{other:?}"),
        }
        let fine = Template {
            name: "y.service",
            text: "Description=banlieue-ch@.service, an instance per %i@ uid\n",
        };
        assert!(render(&fine, &BTreeMap::new()).is_ok());
    }

    /// The host config goes through the provider's own type and parser.
    #[test]
    fn the_host_config_is_what_the_provider_loads() {
        let h = FakeHost::debian();
        h.install_packages(&["swtpm", "swtpm-tools"]);
        let text = host_config(&settings(), &h, &Release::pinned()).unwrap();
        assert!(text.starts_with("# banlieue Cloud Hypervisor host configuration."));
        let c = HostConfig::parse(&text).unwrap();
        assert_eq!(c.provider.name, "bar");
        assert_eq!(c.vmm.version, crate::pins::VMM_VERSION);
        assert_eq!(
            c.vmm.firmware,
            crate::pins::firmware_path(crate::pins::FIRMWARE_TAG)
        );
        assert_eq!(c.guests.uid_base, 2_000_000);
        assert_eq!(c.storage_path("fast"), Some(Path::new("/nvme/banlieue/ch")));
        assert_eq!(c.bridge("default"), Some("virbr0"));
        let tpm = c.tpm.unwrap();
        assert_eq!(tpm.swtpm, Path::new("/usr/bin/swtpm"));
        assert_eq!(tpm.ek_ca_certificate, Path::new(EK_CA_CERT));
        assert!(c.registry.is_none());
    }

    #[test]
    fn a_registry_setting_writes_the_registry_section() {
        let h = FakeHost::debian();
        let mut s = settings();
        s.registry = Some(Registry {
            repository: "registry.internal:5000/banlieue/disks".into(),
            plain_http: true,
            keep_unreferenced: 3,
        });
        let c = HostConfig::parse(&host_config(&s, &h, &Release::pinned()).unwrap()).unwrap();
        let r = c.registry.unwrap();
        assert_eq!(r.repository, "registry.internal:5000/banlieue/disks");
        assert!(r.plain_http);
        assert_eq!(r.keep_unreferenced, 3);
        assert_eq!(
            r.credentials_dir.as_deref(),
            Some(Path::new(REGISTRY_CREDENTIALS_DIR))
        );
    }

    /// `[vmm]` follows the release; every other key keeps its value, and a
    /// file already naming the release is not rewritten (ADR-0084
    /// Decision 5).
    #[test]
    fn with_vmm_replaces_only_the_vmm_section() {
        let h = FakeHost::debian();
        let pinned = Release::pinned();
        let text = host_config(&settings(), &h, &pinned).unwrap();
        assert_eq!(with_vmm(&text, &pinned).unwrap(), None);

        let newer = Release::new(
            "v54.0",
            "ch-0123456789",
            source(&pinned.artifacts[0]),
            source(&pinned.artifacts[1]),
            source(&pinned.artifacts[2]),
        );
        let updated = with_vmm(&text, &newer).unwrap().unwrap();
        let (before, after) = (
            HostConfig::parse(&text).unwrap(),
            HostConfig::parse(&updated).unwrap(),
        );
        assert_eq!(after.vmm.version, "v54.0");
        assert_eq!(
            after.vmm.firmware,
            Path::new("/opt/banlieue/firmware/ch-0123456789/CLOUDHV.fd")
        );
        assert_eq!(after.vmm.binary, before.vmm.binary);
        assert_eq!(
            HostConfig {
                vmm: before.vmm.clone(),
                ..after
            },
            before
        );
        assert!(updated.starts_with("# banlieue Cloud Hypervisor host configuration."));
        assert!(matches!(
            with_vmm("not = [toml", &newer),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn guest_records_are_a_user_and_its_private_group() {
        let (user, group) = guest_records("banlieue-g2000007", 2_000_007);
        let u: serde_json::Value = serde_json::from_str(&user).unwrap();
        let g: serde_json::Value = serde_json::from_str(&group).unwrap();
        assert_eq!(u["userName"], "banlieue-g2000007");
        assert_eq!(u["uid"], 2_000_007);
        assert_eq!(u["gid"], 2_000_007);
        assert_eq!(u["locked"], true);
        assert_eq!(g["groupName"], "banlieue-g2000007");
        assert_eq!(g["gid"], 2_000_007);
    }

    #[test]
    fn swtpm_is_configured_to_sign_with_the_host_ca() {
        let h = FakeHost::debian();
        h.install_packages(&["swtpm-tools"]);
        let files = swtpm_config(&h);
        let setup = &files
            .iter()
            .find(|(p, _)| p == Path::new(SWTPM_SETUP_CONF))
            .unwrap()
            .1;
        assert!(setup.contains("create_certs_tool = /usr/bin/swtpm_localca"));
        assert!(setup.contains("/etc/banlieue/swtpm/swtpm-localca.conf"));
        let localca = &files[0].1;
        assert!(localca.contains(&format!("issuercert = {EK_CA_CERT}")));
    }
}
