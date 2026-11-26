// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `customdata.rs`: everything but the mount, against a
//! temporary directory standing in for `/dev`, `/oem` and the DMI tag.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::azure::AZURE_CHASSIS_ASSET_TAG;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use std::io;
    use std::os::unix::fs::PermissionsExt;

    const CLOUD_CONFIG: &str = "#cloud-config\nhostname: az-01\n";

    struct Guest {
        _root: tempfile::TempDir,
        paths: Paths,
    }

    fn guest(on_azure: bool, devices: &[&str]) -> Guest {
        let root = tempfile::tempdir().expect("tempdir");
        let dev = root.path().join("dev");
        let oem = root.path().join("oem");
        fs::create_dir_all(&dev).expect("dev");
        fs::create_dir_all(&oem).expect("oem");
        for d in devices {
            fs::write(dev.join(d), "").expect("device");
        }
        let tag = root.path().join("chassis_asset_tag");
        let value = if on_azure {
            AZURE_CHASSIS_ASSET_TAG
        } else {
            "No Asset Tag"
        };
        fs::write(&tag, format!("{value}\n")).expect("tag");
        let paths = Paths {
            asset_tag: tag,
            dev_dir: dev,
            oem_dir: oem,
            mount_dir: root.path().join("mnt"),
        };
        Guest { _root: root, paths }
    }

    fn ovf_env(custom: &str) -> Vec<u8> {
        format!(
            "<?xml version=\"1.0\"?><Environment><wa:ProvisioningSection><LinuxProvisioningConfigurationSet><UserPassword>placeholder-password</UserPassword><CustomData>{custom}</CustomData></LinuxProvisioningConfigurationSet></wa:ProvisioningSection></Environment>"
        )
        .into_bytes()
    }

    fn quiet() -> Logger {
        Logger::to(Box::new(io::sink()))
    }

    fn target(g: &Guest) -> PathBuf {
        g.paths.oem_dir.join(CUSTOM_DATA_FILE_NAME)
    }

    // ---- the device scan ----

    #[test]
    fn candidate_devices_are_sr_n_in_numeric_order() {
        let g = guest(true, &["sr10", "sr2", "sr0", "sda", "sr", "srx", "sr0p1"]);
        let names: Vec<String> = candidate_devices(&g.paths.dev_dir)
            .iter()
            .map(|p| p.file_name().expect("name").to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["sr0", "sr2", "sr10"]);
    }

    #[test]
    fn a_missing_dev_dir_has_no_candidates() {
        assert!(candidate_devices(Path::new("/nonexistent/banlieue-test")).is_empty());
    }

    #[test]
    fn find_ovf_env_skips_unreadable_and_empty_devices() {
        let devices = vec![
            PathBuf::from("/dev/sr0"),
            PathBuf::from("/dev/sr1"),
            PathBuf::from("/dev/sr2"),
        ];
        let mut seen = Vec::new();
        let got = find_ovf_env(
            &devices,
            |d| {
                seen.push(d.to_path_buf());
                match d.to_str() {
                    Some("/dev/sr0") => Err(io::Error::from(io::ErrorKind::InvalidInput)),
                    Some("/dev/sr1") => Ok(None),
                    _ => Ok(Some(b"doc".to_vec())),
                }
            },
            &mut quiet(),
        );
        assert_eq!(got.as_deref(), Some(&b"doc"[..]));
        assert_eq!(seen.len(), 3);
    }

    // ---- read_capped ----

    #[test]
    fn read_capped_refuses_a_file_over_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        fs::write(&path, vec![b'a'; 11]).expect("write");
        assert_eq!(read_capped(&path, 11).expect("fits").len(), 11);
        let err = read_capped(&path, 10).expect_err("over the cap");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // ---- the whole command ----

    #[test]
    fn off_azure_nothing_is_read_or_written() {
        let g = guest(false, &["sr0"]);
        let mut reads = 0;
        let outcome = run_with(
            &g.paths,
            |_| {
                reads += 1;
                Ok(Some(ovf_env(&STANDARD.encode(CLOUD_CONFIG))))
            },
            &mut quiet(),
        )
        .expect("ok");
        assert_eq!(outcome, Outcome::NotAzure);
        assert_eq!(reads, 0);
        assert!(!target(&g).exists());
    }

    #[test]
    fn custom_data_is_written_mode_0600() {
        let g = guest(true, &["sr0"]);
        let outcome = run_with(
            &g.paths,
            |_| Ok(Some(ovf_env(&STANDARD.encode(CLOUD_CONFIG)))),
            &mut quiet(),
        )
        .expect("ok");
        assert_eq!(outcome, Outcome::Written(CLOUD_CONFIG.len()));
        assert_eq!(fs::read_to_string(target(&g)).expect("read"), CLOUD_CONFIG);
        let mode = fs::metadata(target(&g)).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, CUSTOM_DATA_FILE_MODE);
    }

    #[test]
    fn running_twice_is_idempotent() {
        let g = guest(true, &["sr0"]);
        let read = |_: &Path| Ok(Some(ovf_env(&STANDARD.encode(CLOUD_CONFIG))));
        run_with(&g.paths, read, &mut quiet()).expect("first");
        let outcome = run_with(&g.paths, read, &mut quiet()).expect("second");
        assert_eq!(outcome, Outcome::Unchanged);
    }

    #[test]
    fn an_unchanged_file_with_a_wider_mode_is_narrowed() {
        let g = guest(true, &["sr0"]);
        fs::write(target(&g), CLOUD_CONFIG).expect("write");
        fs::set_permissions(target(&g), fs::Permissions::from_mode(0o644)).expect("chmod");
        let outcome = write_custom_data(&g.paths.oem_dir, CLOUD_CONFIG.as_bytes()).expect("ok");
        assert_eq!(outcome, Outcome::Unchanged);
        let mode = fs::metadata(target(&g)).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, CUSTOM_DATA_FILE_MODE);
    }

    #[test]
    fn different_custom_data_replaces_the_file_and_no_temp_file_is_left() {
        let g = guest(true, &["sr0"]);
        fs::write(target(&g), "#cloud-config\nold: 1\n").expect("write");
        fs::write(g.paths.oem_dir.join(".91_azure_customdata.tmp"), "stale").expect("stale");
        let outcome = write_custom_data(&g.paths.oem_dir, CLOUD_CONFIG.as_bytes()).expect("ok");
        assert_eq!(outcome, Outcome::Written(CLOUD_CONFIG.len()));
        assert_eq!(fs::read_to_string(target(&g)).expect("read"), CLOUD_CONFIG);
        let leftovers: Vec<_> = fs::read_dir(&g.paths.oem_dir)
            .expect("dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from(CUSTOM_DATA_FILE_NAME)]);
    }

    #[test]
    fn no_provisioning_disk_writes_nothing() {
        let g = guest(true, &[]);
        let outcome = run_with(&g.paths, |_| Ok(None), &mut quiet()).expect("ok");
        assert_eq!(outcome, Outcome::NoProvisioningDisk);
        assert!(!target(&g).exists());
    }

    #[test]
    fn a_vm_without_custom_data_writes_nothing() {
        let g = guest(true, &["sr0"]);
        let outcome = run_with(&g.paths, |_| Ok(Some(ovf_env(""))), &mut quiet()).expect("ok");
        assert_eq!(outcome, Outcome::NoCustomData);
        assert!(!target(&g).exists());
    }

    #[test]
    fn refused_custom_data_writes_nothing_and_keeps_an_existing_file() {
        let g = guest(true, &["sr0"]);
        fs::write(target(&g), CLOUD_CONFIG).expect("write");
        let script = STANDARD.encode("#!/bin/sh\nreboot\n");
        let err =
            run_with(&g.paths, |_| Ok(Some(ovf_env(&script))), &mut quiet()).expect_err("refused");
        assert!(matches!(
            err,
            CustomDataError::Ovf(OvfError::NotCloudConfig(_))
        ));
        assert_eq!(fs::read_to_string(target(&g)).expect("read"), CLOUD_CONFIG);
    }

    #[test]
    fn the_log_never_carries_custom_data_or_the_password() {
        use std::cell::RefCell;
        use std::rc::Rc;
        #[derive(Clone, Default)]
        struct Shared(Rc<RefCell<Vec<u8>>>);
        impl io::Write for Shared {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let g = guest(true, &["sr0"]);
        let sink = Shared::default();
        let mut log = Logger::to(Box::new(sink.clone()));
        run_with(
            &g.paths,
            |_| Ok(Some(ovf_env(&STANDARD.encode(CLOUD_CONFIG)))),
            &mut log,
        )
        .expect("ok");
        let out = String::from_utf8(sink.0.borrow().clone()).expect("utf8");
        assert!(out.contains("wrote"));
        assert!(!out.contains("az-01"));
        assert!(!out.contains("placeholder-password"));
        assert!(!out.contains(&STANDARD.encode(CLOUD_CONFIG)));
    }
}
