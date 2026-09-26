// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `hostfs.rs`, against a temporary directory. The guest uid
//! and group are this process's own, since changing ownership to another uid
//! needs `CAP_CHOWN`; the end-to-end run covers that.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::host_config::HostConfig;
    use crate::plan::{MachinePlan, plan_machine};
    use crate::sys;
    use banlieue_api::common::{IpamSpec, LocalObjectReference, PowerState};
    use banlieue_api::infrastructure::{
        ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec, ChNicSpec,
        CloudHypervisorMachineSpec,
    };
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    const UID: &str = "0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e01";

    const MACHINE_NAME: &str = "m1";
    /// A small "image" and a disk size a little larger than it, in GiB.
    const IMAGE: &[u8] = b"kairos-image-bytes";
    const DISK_GIB: u32 = 1;

    struct Host {
        _root: tempfile::TempDir,
        plan: MachinePlan,
    }

    fn host() -> Host {
        host_with(false)
    }

    /// With `tpm`, the machine asks for a vTPM and the host has `[tpm]`.
    fn host_with(tpm: bool) -> Host {
        host_booting(tpm, ChBootSourceKind::Image)
    }

    /// A `Deferred` machine: the image is its installer.
    fn deferred_host() -> Host {
        host_booting(false, ChBootSourceKind::InstallMedia)
    }

    fn host_booting(tpm: bool, kind: ChBootSourceKind) -> Host {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let storage = root.path().join("storage");
        let run = root.path().join("run");
        std::fs::create_dir_all(storage.join("images")).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(storage.join("images").join("kairos.raw"), IMAGE).unwrap();
        let me = sys::effective_uid();
        let config = HostConfig::parse(&format!(
            r#"
[provider]
name = "ch-a"
namespace = "banlieue-system"
kubeconfig = "/etc/banlieue/kubeconfig"
[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/f/CLOUDHV.fd"
[paths]
run_root = "{run}"
state_root = "{state}"
[guests]
uid_base = {me}
uid_count = 1
[storage_classes]
fast = "{storage}"
[network_classes]
lan = "br0"
"#,
            run = run.display(),
            storage = storage.display(),
            state = state.display(),
        ))
        .unwrap();
        let config = HostConfig {
            tpm: tpm.then(|| crate::host_config::TpmSection {
                swtpm: "/usr/bin/swtpm".into(),
                swtpm_setup: "/usr/bin/swtpm_setup".into(),
                setup_config: "/etc/banlieue/swtpm/swtpm_setup.conf".into(),
                ek_ca_certificate: state.join("swtpm-localca/issuercert.pem"),
            }),
            ..config
        };
        let spec = CloudHypervisorMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "ch-a".into(),
            },
            cpus: ChCpuSpec { boot: 1, max: None },
            memory: ChMemorySpec {
                size_mi_b: 512,
                hugepages: false,
            },
            storage_class: "fast".into(),
            boot_source: ChBootSource {
                kind,
                image: "kairos.raw".into(),
            },
            os_disk_size_gi_b: DISK_GIB,
            nics: vec![ChNicSpec {
                name: "eth0".into(),
                network_class: "lan".into(),
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            tpm_enabled: tpm,
            user_data: Some("#cloud-config\n".into()),
            desired_power_state: PowerState::PoweredOn,
        };
        let plan = plan_machine(UID, MACHINE_NAME, "ch-a", &spec, &config, me).unwrap();
        Host { _root: root, plan }
    }

    fn mode(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn directories_are_created_owned_and_shared_with_the_provider_group() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        for d in [&h.plan.machine_dir, &h.plan.run_dir] {
            let m = std::fs::metadata(d).unwrap();
            assert_eq!(m.uid(), h.plan.host_uid);
            assert_eq!(m.gid(), sys::effective_gid());
            // setgid: what the VMM creates inside (the API socket, the
            // serial log) is in the provider's group, not the guest's.
            assert_eq!(mode(d), 0o2770, "{}", d.display());
        }
        // Idempotent.
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
    }

    #[test]
    fn the_os_disk_is_cloned_grown_and_owned() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        let out = ensure_os_disk(&h.plan, sys::effective_gid()).unwrap();
        assert!(matches!(out, DiskOutcome::Created(_)));
        let m = std::fs::metadata(&h.plan.os_disk).unwrap();
        assert_eq!(m.len(), h.plan.os_disk_bytes, "grown to the requested size");
        assert_eq!(mode(&h.plan.os_disk), 0o660);
        let head = std::fs::read(&h.plan.os_disk).unwrap();
        assert_eq!(&head[..IMAGE.len()], IMAGE, "image content preserved");
        assert!(!h.plan.os_disk.with_extension("raw.tmp").exists());
    }

    /// An existing disk may hold an installed guest: never replace it.
    #[test]
    fn an_existing_os_disk_is_left_alone() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        std::fs::write(&h.plan.os_disk, b"installed guest").unwrap();
        assert_eq!(
            ensure_os_disk(&h.plan, sys::effective_gid()).unwrap(),
            DiskOutcome::Existing
        );
        assert_eq!(std::fs::read(&h.plan.os_disk).unwrap(), b"installed guest");
    }

    #[test]
    fn a_missing_image_names_the_image() {
        let mut h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        h.plan.image = h.plan.image.with_file_name("absent.raw");
        let e = ensure_os_disk(&h.plan, sys::effective_gid()).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        assert!(e.to_string().contains("absent.raw"), "{e}");
    }

    /// A disk is never shrunk below its image.
    #[test]
    fn an_image_larger_than_the_disk_is_refused() {
        let mut h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        h.plan.os_disk_bytes = 4;
        let e = ensure_os_disk(&h.plan, sys::effective_gid()).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!h.plan.os_disk.exists());
    }

    #[test]
    fn the_seed_is_written_read_only_and_rewritten_only_when_it_changes() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        write_seed(&h.plan, b"seed-v1", sys::effective_gid()).unwrap();
        assert_eq!(std::fs::read(&h.plan.seed).unwrap(), b"seed-v1");
        assert_eq!(mode(&h.plan.seed), 0o440);
        let before = std::fs::metadata(&h.plan.seed).unwrap().ino();
        write_seed(&h.plan, b"seed-v1", sys::effective_gid()).unwrap();
        assert_eq!(
            std::fs::metadata(&h.plan.seed).unwrap().ino(),
            before,
            "unchanged"
        );
        write_seed(&h.plan, b"seed-v2", sys::effective_gid()).unwrap();
        assert_eq!(std::fs::read(&h.plan.seed).unwrap(), b"seed-v2");
    }

    #[test]
    fn removal_takes_everything_and_verifies_it() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        ensure_os_disk(&h.plan, sys::effective_gid()).unwrap();
        write_seed(&h.plan, b"seed", sys::effective_gid()).unwrap();
        std::fs::write(h.plan.run_dir.join("api.sock"), b"").unwrap();
        assert!(machine_files_exist(&h.plan));
        remove_machine(&h.plan).unwrap();
        assert!(!machine_files_exist(&h.plan));
        // Absent is success.
        remove_machine(&h.plan).unwrap();
        // The image cache is untouched.
        assert!(h.plan.image.exists());
    }

    #[test]
    fn a_missing_storage_root_is_an_error_not_a_silent_mkdir_p() {
        let mut h = host();
        h.plan.machine_dir = h.plan.machine_dir.join("nested/too/deep");
        assert!(prepare_dirs(&h.plan, sys::effective_gid()).is_err());
    }

    // ------------------------------------------------------------------
    // The VMM's API socket (v53 forces umask 0077: it is always 0700)
    // ------------------------------------------------------------------

    fn socket_at(
        dir: &std::path::Path,
        mode: u32,
    ) -> (std::path::PathBuf, std::os::unix::net::UnixListener) {
        let path = dir.join("api.sock");
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        (path, l)
    }

    #[test]
    fn a_missing_socket_is_not_ready_yet() {
        let d = tempfile::tempdir().unwrap();
        let ready = grant_api_socket(
            &d.path().join("api.sock"),
            sys::effective_uid(),
            sys::effective_gid(),
        );
        assert!(!ready.unwrap());
    }

    #[test]
    fn the_vmms_0700_socket_is_opened_to_the_provider_group_only() {
        let d = tempfile::tempdir().unwrap();
        let (path, _l) = socket_at(d.path(), 0o700);
        assert!(grant_api_socket(&path, sys::effective_uid(), sys::effective_gid()).unwrap());
        assert_eq!(mode(&path), 0o660);
        // Idempotent.
        assert!(grant_api_socket(&path, sys::effective_uid(), sys::effective_gid()).unwrap());
    }

    /// Not the guest's, or not in the provider's group: someone else's
    /// socket. Refused, and left untouched.
    #[test]
    fn a_socket_with_the_wrong_owner_or_group_is_refused_untouched() {
        let d = tempfile::tempdir().unwrap();
        let (path, _l) = socket_at(d.path(), 0o700);
        let me = sys::effective_uid();
        let gid = sys::effective_gid();
        assert!(grant_api_socket(&path, me.wrapping_add(1), gid).is_err());
        assert!(grant_api_socket(&path, me, gid.wrapping_add(1)).is_err());
        assert_eq!(mode(&path), 0o700);
    }

    #[test]
    fn a_socket_open_to_others_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (path, _l) = socket_at(d.path(), 0o707);
        assert!(grant_api_socket(&path, sys::effective_uid(), sys::effective_gid()).is_err());
    }

    /// The provider can chmod any file (CAP_FOWNER), and the guest owns the
    /// directory, so a symlink planted as api.sock must never be followed.
    #[test]
    fn a_symlink_in_place_of_the_socket_is_refused_and_its_target_untouched() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("precious");
        std::fs::write(&target, b"x").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.path().join("api.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(grant_api_socket(&link, sys::effective_uid(), sys::effective_gid()).is_err());
        assert_eq!(mode(&target), 0o600);
    }

    #[test]
    fn a_regular_file_in_place_of_the_socket_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("api.sock");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(grant_api_socket(&path, sys::effective_uid(), sys::effective_gid()).is_err());
        assert_eq!(mode(&path), 0o600);
    }

    // ------------------------------------------------------------------
    // Guest-owned directories: never act on a path the guest can swap
    // ------------------------------------------------------------------

    /// A symlink planted where the provider builds the OS disk must not
    /// lead it to chown, chmod or truncate the link's target.
    #[test]
    fn a_symlink_planted_at_the_disk_temp_path_does_not_touch_its_target() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        let victim = h.plan.machine_dir.parent().unwrap().join("victim.raw");
        std::fs::write(&victim, b"another guest's disk").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut tmp = h.plan.os_disk.as_os_str().to_owned();
        tmp.push(".tmp");
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();

        ensure_os_disk(&h.plan, sys::effective_gid()).unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), b"another guest's disk");
        assert_eq!(mode(&victim), 0o600);
        assert!(
            std::fs::symlink_metadata(&h.plan.os_disk)
                .unwrap()
                .is_file()
        );
    }

    /// A seed replaced by a symlink to identical bytes must not count as
    /// "unchanged": the provider reads it without following links and puts
    /// a real file back.
    #[test]
    fn a_symlinked_seed_is_replaced_by_a_real_file() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        let iso = b"seed-bytes".to_vec();
        let elsewhere = h.plan.machine_dir.parent().unwrap().join("elsewhere.iso");
        std::fs::write(&elsewhere, &iso).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &h.plan.seed).unwrap();

        write_seed(&h.plan, &iso, sys::effective_gid()).unwrap();

        let m = std::fs::symlink_metadata(&h.plan.seed).unwrap();
        assert!(m.is_file(), "seed must be a regular file, not a link");
        assert_eq!(std::fs::read(&h.plan.seed).unwrap(), iso);
        assert_eq!(std::fs::read(&elsewhere).unwrap(), iso, "target untouched");
    }

    /// A machine directory that is a symlink is refused, not followed and
    /// re-owned.
    #[test]
    fn a_symlinked_machine_directory_is_refused() {
        let h = host();
        let elsewhere = h.plan.machine_dir.parent().unwrap().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &h.plan.machine_dir).unwrap();

        assert!(prepare_dirs(&h.plan, sys::effective_gid()).is_err());
        assert_eq!(mode(&elsewhere), 0o700);
    }

    // ------------------------------------------------------------------
    // vTPM (ADR-0065)
    // ------------------------------------------------------------------

    /// Before manufacture the state directory belongs to the provider, who
    /// runs `swtpm_setup`; the EK directory is provider-only, outside the
    /// guest's machine directory.
    #[test]
    fn tpm_directories_are_prepared_for_manufacture() {
        let h = host_with(true);
        let t = h.plan.tpm.clone().unwrap();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        prepare_tpm(&h.plan, sys::effective_uid(), sys::effective_gid()).unwrap();
        reset_tpm_state(&h.plan, sys::effective_uid(), sys::effective_gid()).unwrap();
        assert!(t.state_dir.is_dir());
        assert_eq!(mode(&t.ek_dir), 0o700);
        assert!(!t.ek_dir.starts_with(&h.plan.machine_dir));
        assert!(!tpm_manufactured(&h.plan));
        // Idempotent.
        prepare_tpm(&h.plan, sys::effective_uid(), sys::effective_gid()).unwrap();
    }

    /// Manufactured once the host has written EK certificates; they are
    /// published as PEM, in a stable order.
    #[test]
    fn ek_files_mark_manufacture_and_are_read_as_pem() {
        let h = host_with(true);
        let t = h.plan.tpm.clone().unwrap();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        prepare_tpm(&h.plan, sys::effective_uid(), sys::effective_gid()).unwrap();
        std::fs::write(t.ek_dir.join("ek-secp384r1.crt"), b"\x30\x02ec").unwrap();
        std::fs::write(t.ek_dir.join("ek-rsa2048.crt"), b"\x30\x02rs").unwrap();
        std::fs::write(t.ek_dir.join("notes.txt"), b"ignored").unwrap();
        assert!(tpm_manufactured(&h.plan));
        let pems = ek_certificates(&h.plan).unwrap();
        assert_eq!(pems.len(), 2);
        assert!(
            pems.iter()
                .all(|p| p.starts_with("-----BEGIN CERTIFICATE-----"))
        );
        assert_eq!(
            pems[0],
            banlieue_provider_sdk::pem::der_to_pem(b"\x30\x02rs")
        );
    }

    /// Handing the state to the guest acts on each file through a handle
    /// opened without following links: a symlink planted in the state
    /// directory is refused and its target untouched.
    #[test]
    fn adopting_tpm_state_refuses_a_planted_symlink() {
        let h = host_with(true);
        let t = h.plan.tpm.clone().unwrap();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        prepare_tpm(&h.plan, sys::effective_uid(), sys::effective_gid()).unwrap();
        reset_tpm_state(&h.plan, sys::effective_uid(), sys::effective_gid()).unwrap();
        std::fs::write(t.state_dir.join("tpm2-00.permall"), b"state").unwrap();
        adopt_tpm_state(&h.plan, sys::effective_gid()).unwrap();
        assert_eq!(mode(&t.state_dir.join("tpm2-00.permall")), 0o660);

        let victim = h.plan.machine_dir.parent().unwrap().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&victim, t.state_dir.join("evil")).unwrap();
        assert!(adopt_tpm_state(&h.plan, sys::effective_gid()).is_err());
        assert_eq!(mode(&victim), 0o600);
    }

    #[test]
    fn removal_takes_the_ek_directory_too() {
        let h = host_with(true);
        let t = h.plan.tpm.clone().unwrap();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        prepare_tpm(&h.plan, sys::effective_uid(), sys::effective_gid()).unwrap();
        assert!(machine_files_exist(&h.plan));
        remove_machine(&h.plan).unwrap();
        assert!(!t.ek_dir.exists());
        assert!(!machine_files_exist(&h.plan));
    }

    /// ADR-0065 Decision 3: `Deferred` starts from an empty, sparse OS disk
    /// of the requested size; the image (the installer) is never copied.
    #[test]
    fn a_deferred_os_disk_is_created_empty_and_sparse() {
        let mut h = host();
        h.plan.empty_os_disk = true;
        std::fs::remove_file(&h.plan.image).unwrap();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        assert_eq!(
            ensure_os_disk(&h.plan, sys::effective_gid()).unwrap(),
            DiskOutcome::CreatedEmpty
        );
        let m = std::fs::metadata(&h.plan.os_disk).unwrap();
        assert_eq!(m.len(), h.plan.os_disk_bytes);
        assert!(m.blocks() * 512 < 1024 * 1024, "sparse");
        assert_eq!(mode(&h.plan.os_disk), 0o660);
        assert_eq!(
            ensure_os_disk(&h.plan, sys::effective_gid()).unwrap(),
            DiskOutcome::Existing
        );
    }

    /// ADR-0065 Decision 3, amended: the installer is cloned into the
    /// machine's own directory, read-only and owned by the guest, because
    /// the guest's VMM cannot read the provider's cache.
    #[test]
    fn the_installer_is_staged_read_only_in_the_machine_directory() {
        let h = deferred_host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        ensure_install_media(&h.plan, sys::effective_gid()).unwrap();
        let m = std::fs::symlink_metadata(&h.plan.install_media).unwrap();
        assert!(m.is_file());
        assert_eq!(m.uid(), h.plan.host_uid);
        assert_eq!(mode(&h.plan.install_media), 0o440);
        assert_eq!(std::fs::read(&h.plan.install_media).unwrap(), IMAGE);
        assert!(!tmp_path(&h.plan.install_media).exists());
        // Idempotent: a staged installer is not copied again.
        ensure_install_media(&h.plan, sys::effective_gid()).unwrap();
        assert_eq!(std::fs::read(&h.plan.install_media).unwrap(), IMAGE);
    }

    /// Decision 4: once the installer is out of the plan (ejected), its
    /// copy is deleted, including one a crash after the eject left behind.
    #[test]
    fn an_ejected_installer_is_deleted() {
        let h = deferred_host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        ensure_install_media(&h.plan, sys::effective_gid()).unwrap();
        let ejected = h.plan.clone().without_install_media();
        ensure_install_media(&ejected, sys::effective_gid()).unwrap();
        assert!(std::fs::symlink_metadata(&h.plan.install_media).is_err());
        // Nothing to do, and no error, when it is already gone.
        ensure_install_media(&ejected, sys::effective_gid()).unwrap();
    }

    /// An `Immediate` machine never gets an installer copy.
    #[test]
    fn an_image_machine_stages_no_installer() {
        let h = host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        ensure_install_media(&h.plan, sys::effective_gid()).unwrap();
        assert!(std::fs::symlink_metadata(&h.plan.install_media).is_err());
    }

    /// The machine directory is the guest's: a symlink planted where the
    /// installer goes is replaced by a real copy, and its target untouched.
    #[test]
    fn a_symlink_in_place_of_the_installer_is_replaced() {
        let h = deferred_host();
        prepare_dirs(&h.plan, sys::effective_gid()).unwrap();
        let victim = h.plan.machine_dir.parent().unwrap().join("victim.iso");
        std::fs::write(&victim, b"another guest's installer").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&victim, &h.plan.install_media).unwrap();

        ensure_install_media(&h.plan, sys::effective_gid()).unwrap();

        assert!(
            std::fs::symlink_metadata(&h.plan.install_media)
                .unwrap()
                .is_file()
        );
        assert_eq!(std::fs::read(&h.plan.install_media).unwrap(), IMAGE);
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"another guest's installer"
        );
        assert_eq!(mode(&victim), 0o600);
    }

    /// Environment files for the template units: written whole (temporary
    /// name, rename), `0600`, into a provider-only directory it creates.
    #[test]
    fn environment_files_are_written_privately_and_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("units").join("import-x.env");
        write_env_file(&path, "FILE=a.raw\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "FILE=a.raw\n");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        write_env_file(&path, "FILE=b.raw\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "FILE=b.raw\n");
        assert!(!dir.path().join("units").join("import-x.env.tmp").exists());
    }
}
