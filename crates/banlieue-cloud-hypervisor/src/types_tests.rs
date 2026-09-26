// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `types.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::path::PathBuf;

    fn plan() -> GuestPlan {
        GuestPlan {
            firmware: PathBuf::from("/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd"),
            boot_vcpus: 2,
            max_vcpus: 4,
            memory_mib: 4096,
            hugepages: false,
            disks: vec![
                PlannedDisk {
                    id: "os".into(),
                    path: PathBuf::from("/srv/banlieue/ch/m/os.raw"),
                    readonly: false,
                },
                PlannedDisk {
                    id: "seed".into(),
                    path: PathBuf::from("/srv/banlieue/ch/m/seed.iso"),
                    readonly: true,
                },
            ],
            nics: vec![PlannedNic {
                id: "eth0".into(),
                tap: "bch0f3c9a1e00".into(),
                mac: "52:54:00:12:34:56".into(),
            }],
            tpm_socket: None,
            serial_file: PathBuf::from("/srv/banlieue/ch/m/serial.log"),
            landlock: true,
        }
    }

    fn config() -> serde_json::Value {
        serde_json::to_value(VmConfigRequest::for_guest(&plan())).unwrap()
    }

    // ------------------------------------------------------------------
    // Invariants the caller cannot opt out of (ADR-0061 Decision 4)
    // ------------------------------------------------------------------

    /// An auto-detected raw image has sector-0 writes disabled (spike
    /// finding), so every disk says `Raw` explicitly.
    #[test]
    fn every_disk_is_explicitly_raw() {
        let c = config();
        let disks = c["disks"].as_array().unwrap();
        assert_eq!(disks.len(), 2);
        for d in disks {
            assert_eq!(d["image_type"], "Raw", "{d}");
        }
    }

    /// Nested virtualization defaults to ON in v53 (spike finding). The plan
    /// has no field for it, and the request always says false.
    #[test]
    fn nested_virtualization_is_always_off() {
        assert_eq!(config()["cpus"]["nested"], false);
    }

    /// A first boot that generates keys with a starved entropy pool looks
    /// like a hang.
    #[test]
    fn every_vm_gets_virtio_rng() {
        assert_eq!(config()["rng"]["src"], "/dev/urandom");
    }

    #[test]
    fn serial_goes_to_the_planned_file_and_the_console_is_off() {
        let c = config();
        assert_eq!(c["serial"]["mode"], "File");
        assert_eq!(c["serial"]["file"], "/srv/banlieue/ch/m/serial.log");
        assert_eq!(c["console"]["mode"], "Off");
    }

    // ------------------------------------------------------------------
    // What the plan carries
    // ------------------------------------------------------------------

    #[test]
    fn firmware_is_the_payload() {
        assert_eq!(
            config()["payload"]["firmware"],
            "/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd"
        );
    }

    #[test]
    fn vcpus_and_memory_are_carried() {
        let c = config();
        assert_eq!(c["cpus"]["boot_vcpus"], 2);
        assert_eq!(c["cpus"]["max_vcpus"], 4);
        assert_eq!(c["memory"]["size"], 4096_u64 * 1024 * 1024);
        assert_eq!(c["memory"]["hugepages"], false);
    }

    /// The VMM rejects `max < boot`. The plan cannot produce it.
    #[test]
    fn max_vcpus_is_never_below_boot() {
        let mut p = plan();
        p.max_vcpus = 1;
        let c = serde_json::to_value(VmConfigRequest::for_guest(&p)).unwrap();
        assert_eq!(c["cpus"]["max_vcpus"], 2);
    }

    /// Disk order is boot order (roadmap 09 Gotchas); the request keeps the
    /// plan's order exactly.
    #[test]
    fn disk_order_is_preserved() {
        let c = config();
        assert_eq!(c["disks"][0]["id"], "os");
        assert_eq!(c["disks"][0]["readonly"], false);
        assert_eq!(c["disks"][1]["id"], "seed");
        assert_eq!(c["disks"][1]["readonly"], true);
    }

    #[test]
    fn nics_carry_tap_mac_and_id() {
        let c = config();
        assert_eq!(c["net"][0]["tap"], "bch0f3c9a1e00");
        assert_eq!(c["net"][0]["mac"], "52:54:00:12:34:56");
        assert_eq!(c["net"][0]["id"], "eth0");
    }

    #[test]
    fn tpm_is_absent_without_a_socket_and_present_with_one() {
        assert!(config().get("tpm").is_none());
        let mut p = plan();
        p.tpm_socket = Some(PathBuf::from("/run/banlieue/ch/m/swtpm.sock"));
        let c = serde_json::to_value(VmConfigRequest::for_guest(&p)).unwrap();
        assert_eq!(c["tpm"]["socket"], "/run/banlieue/ch/m/swtpm.sock");
    }

    #[test]
    fn landlock_follows_the_plan() {
        assert_eq!(config()["landlock_enable"], true);
        let mut p = plan();
        p.landlock = false;
        let c = serde_json::to_value(VmConfigRequest::for_guest(&p)).unwrap();
        assert_eq!(c["landlock_enable"], false);
    }

    // ------------------------------------------------------------------
    // Responses from a real VMM
    // ------------------------------------------------------------------

    #[test]
    fn a_real_vm_info_decodes() {
        let info: VmInfo =
            serde_json::from_str(include_str!("../tests/fixtures/vm-info-created-v53.0.json"))
                .expect("real vm.info must decode");
        assert_eq!(info.state, VmState::Created);
        assert_eq!(info.config.disks.len(), 1);
        assert_eq!(info.config.disks[0].id.as_deref(), Some("os"));
        assert_eq!(info.config.disks[0].image_type, Some(ImageType::Raw));
        assert_eq!(info.config.cpus.as_ref().map(|c| c.nested), Some(false));
    }

    /// Newer VMMs add fields and states. Decoding must not break on either.
    #[test]
    fn unknown_fields_and_states_are_tolerated() {
        let json = serde_json::json!({
            "config": { "payload": {}, "something_new": 1 },
            "state": "Hibernating",
        });
        let info: VmInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.state, VmState::Unknown);
    }
}
