// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `systemd.rs`, and for the template units it starts: what a
//! guest runs, as whom, and in which sandbox lives in those files, so they
//! are what these tests read.

#[cfg(test)]
mod tests {
    use super::super::*;

    const TEMPLATE_VMM: &str =
        include_str!("../../../deploy/provider-cloud-hypervisor/host/banlieue-ch@.service");
    const TEMPLATE_SWTPM: &str =
        include_str!("../../../deploy/provider-cloud-hypervisor/host/banlieue-swtpm@.service");
    const TEMPLATE_SWTPM_SETUP: &str = include_str!(
        "../../../deploy/provider-cloud-hypervisor/host/banlieue-swtpm-setup@.service"
    );
    const TEMPLATE_IMPORT: &str =
        include_str!("../../../deploy/provider-cloud-hypervisor/host/banlieue-ch-import@.service");

    /// Values of `key=` in a unit file, ignoring comments.
    fn values<'a>(unit: &'a str, key: &str) -> Vec<&'a str> {
        unit.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.strip_prefix(&format!("{key}=")))
            .collect()
    }

    fn one<'a>(unit: &'a str, key: &str) -> &'a str {
        match values(unit, key).as_slice() {
            [v] => v,
            other => panic!("{key}: expected one value, got {other:?}"),
        }
    }

    /// ADR-0063 Decision 3, in every template.
    #[test]
    fn every_template_is_sandboxed_and_kept_when_failed() {
        for (name, t) in [
            ("vmm", TEMPLATE_VMM),
            ("swtpm", TEMPLATE_SWTPM),
            ("swtpm-setup", TEMPLATE_SWTPM_SETUP),
            ("import", TEMPLATE_IMPORT),
        ] {
            assert_eq!(one(t, "NoNewPrivileges"), "yes", "{name}");
            assert_eq!(one(t, "ProtectSystem"), "strict", "{name}");
            assert_eq!(one(t, "ProtectHome"), "yes", "{name}");
            assert_eq!(one(t, "PrivateTmp"), "yes", "{name}");
            assert_eq!(one(t, "DevicePolicy"), "closed", "{name}");
            // A failed unit stays loaded so the provider can read why.
            assert_eq!(one(t, "CollectMode"), "inactive", "{name}");
            assert!(!values(t, "ReadWritePaths").is_empty(), "{name}");
        }
    }

    /// The guest's own processes run as the instance: its uid and its
    /// private group. Nothing else can be named, because the instance is a
    /// uid and polkit allows only uids in the guest range.
    #[test]
    fn guest_templates_run_as_the_instance_uid() {
        for t in [TEMPLATE_VMM, TEMPLATE_SWTPM] {
            assert_eq!(one(t, "User"), "%i");
            assert_eq!(one(t, "Group"), "%i");
        }
        assert_eq!(one(TEMPLATE_VMM, "SupplementaryGroups"), "kvm");
        assert_eq!(
            values(TEMPLATE_VMM, "DeviceAllow"),
            vec!["/dev/kvm rw", "/dev/net/tun rw"]
        );
        assert!(values(TEMPLATE_SWTPM, "DeviceAllow").is_empty());
    }

    /// Manufacture and import run as the provider's own user: signing reads
    /// the CA key, which no guest uid may.
    #[test]
    fn helper_templates_run_as_the_provider_user_from_an_environment_file() {
        for t in [TEMPLATE_SWTPM_SETUP, TEMPLATE_IMPORT] {
            assert_eq!(one(t, "User"), "@BANLIEUE_USER@");
            assert_eq!(one(t, "Group"), "@BANLIEUE_USER@");
            assert_eq!(one(t, "Type"), "oneshot");
            assert!(values(t, "EnvironmentFile").len() == 1);
            assert!(values(t, "DeviceAllow").is_empty());
        }
    }

    /// Paths the templates use are the ones the plan computes; a drift here
    /// is a guest that starts and cannot find its socket.
    #[test]
    fn template_paths_match_the_plan() {
        let vmm = one(TEMPLATE_VMM, "ExecStart");
        assert!(
            vmm.ends_with("--api-socket path=@RUN_ROOT@/%i/api.sock"),
            "{vmm}"
        );
        let swtpm = one(TEMPLATE_SWTPM, "ExecStart");
        assert!(
            swtpm.contains("--tpmstate dir=@STATE_ROOT@/tpm/%i"),
            "{swtpm}"
        );
        assert!(swtpm.contains("path=@RUN_ROOT@/%i/swtpm.sock"), "{swtpm}");
        let setup = one(TEMPLATE_SWTPM_SETUP, "ExecStart");
        assert!(setup.contains("--tpmstate @STATE_ROOT@/tpm/%i"), "{setup}");
        assert!(setup.contains("--vmid ${VMID}"), "{setup}");
        assert!(setup.contains("--write-ek-cert-files ${EK_DIR}"), "{setup}");
        assert_eq!(
            one(TEMPLATE_SWTPM_SETUP, "EnvironmentFile"),
            "@STATE_ROOT@/units/swtpm-setup-%i.env"
        );
        let import = one(TEMPLATE_IMPORT, "ExecStart");
        assert!(
            import.contains("import --reference ${REFERENCE} --file ${FILE}"),
            "{import}"
        );
        assert_eq!(
            one(TEMPLATE_IMPORT, "EnvironmentFile"),
            "@STATE_ROOT@/units/import-%i.env"
        );
    }

    #[test]
    fn only_memory_is_set_at_runtime() {
        let u = UnitStart {
            name: "banlieue-ch@2000007.service".into(),
            memory_max: Some(4_831_838_208),
            environment: None,
        };
        let props = u.runtime_properties();
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].0, "MemoryMax");
        assert_eq!(props[0].1.value_signature().to_string(), "t");
        let none = UnitStart {
            memory_max: None,
            ..u
        };
        assert!(none.runtime_properties().is_empty());
    }

    /// Values reach `ExecStart` as `${NAME}`, one word each: nothing that
    /// could split or re-quote them is accepted.
    #[test]
    fn environment_files_accept_only_single_safe_words() {
        let ok = EnvFile {
            path: "/var/lib/banlieue/units/x.env".into(),
            vars: vec![
                (
                    "VMID".into(),
                    "m1:0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e01".into(),
                ),
                ("EK_DIR".into(), "/var/lib/banlieue/ek/0f3c".into()),
            ],
        };
        assert_eq!(
            ok.render().unwrap(),
            "VMID=m1:0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e01\nEK_DIR=/var/lib/banlieue/ek/0f3c\n"
        );
        for bad in ["a b", "a\nX=1", "$(x)", "a\"b", "a'b", "a\\b", "`x`", ""] {
            let e = EnvFile {
                path: "/x".into(),
                vars: vec![("VMID".into(), bad.into())],
            };
            assert!(e.render().is_err(), "{bad:?}");
        }
        let lower = EnvFile {
            path: "/x".into(),
            vars: vec![("vmid".into(), "x".into())],
        };
        assert!(lower.render().is_err());
    }

    #[test]
    fn exit_details_name_the_systemd_step_that_failed() {
        assert_eq!(
            describe_exit("exit-code", 217),
            "exit-code, status 217 (USER: the unit's user is unknown to NSS)"
        );
        assert_eq!(describe_exit("exit-code", 1), "exit-code, status 1");
        assert_eq!(describe_exit("signal", 9), "signal, status 9");
    }

    #[test]
    fn active_states_map() {
        assert!(UnitState::from_active_state("active").is_running());
        assert!(UnitState::from_active_state("activating").is_running());
        assert!(!UnitState::from_active_state("failed").is_running());
        assert_eq!(
            UnitState::from_active_state("inactive"),
            UnitState::Inactive
        );
        assert_eq!(
            UnitState::from_active_state("maintenance"),
            UnitState::Other("maintenance".into())
        );
    }
}
