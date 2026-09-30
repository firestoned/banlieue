// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `types.rs`. Bodies are shaped like real PVE 9 responses.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::wire::decode_data;

    #[test]
    fn version_decodes_and_reports_its_major() {
        let v: Version =
            decode_data(r#"{"data":{"version":"9.0.3","release":"9.0","repoid":"abc"}}"#).unwrap();
        assert_eq!(v.version, "9.0.3");
        assert_eq!(v.major(), Some(9));
    }

    #[test]
    fn a_version_that_does_not_parse_has_no_major() {
        let v = Version {
            version: "x".into(),
            release: None,
            repoid: None,
        };
        assert_eq!(v.major(), None);
    }

    #[test]
    fn nodes_decode_and_online_is_derived() {
        let n: Vec<Node> = decode_data(
            r#"{"data":[{"node":"pve1","status":"online","maxcpu":8,"maxmem":34359738368},{"node":"pve2","status":"offline"}]}"#,
        )
        .unwrap();
        assert!(n[0].is_online());
        assert!(!n[1].is_online());
        assert_eq!(n[0].maxcpu, Some(8));
    }

    #[test]
    fn next_id_accepts_the_string_proxmox_sends() {
        assert_eq!(decode_data::<VmId>(r#"{"data":"100"}"#).unwrap().0, 100);
    }

    #[test]
    fn next_id_accepts_a_number_too() {
        assert_eq!(decode_data::<VmId>(r#"{"data":101}"#).unwrap().0, 101);
    }

    #[test]
    fn next_id_rejects_garbage() {
        assert!(decode_data::<VmId>(r#"{"data":"abc"}"#).is_err());
    }

    #[test]
    fn cluster_vms_decode_with_template_as_zero_or_one() {
        let v: Vec<ClusterVm> = decode_data(
            r#"{"data":[{"vmid":9000,"node":"pve1","name":"tpl","status":"stopped","type":"qemu","template":1},
                       {"vmid":100,"node":"pve1","status":"running","type":"qemu"}]}"#,
        )
        .unwrap();
        assert!(v[0].template);
        assert!(!v[1].template);
        assert_eq!(v[1].name, None);
    }

    #[test]
    fn storage_flags_default_to_usable_and_ints_become_bools() {
        let s: Vec<Storage> = decode_data(
            r#"{"data":[{"storage":"local-lvm","type":"lvmthin","content":"images,rootdir","active":1,"enabled":1,"shared":0,"total":100,"avail":40},
                       {"storage":"nfs","type":"nfs","content":"iso","shared":1}]}"#,
        )
        .unwrap();
        assert!(s[0].usable_for("images"));
        assert!(!s[0].usable_for("iso"));
        assert!(!s[0].shared);
        assert!(s[1].shared);
        assert!(s[1].usable_for("iso"));
    }

    #[test]
    fn a_disabled_storage_is_not_usable() {
        let s: Storage = serde_json::from_str(
            r#"{"storage":"x","type":"dir","content":"images","enabled":0,"active":1}"#,
        )
        .unwrap();
        assert!(!s.usable_for("images"));
    }

    #[test]
    fn an_inactive_storage_is_not_usable() {
        let s: Storage = serde_json::from_str(
            r#"{"storage":"x","type":"dir","content":"images","enabled":1,"active":0}"#,
        )
        .unwrap();
        assert!(!s.usable_for("images"));
    }

    #[test]
    fn content_matching_is_exact_not_substring() {
        let s: Storage =
            serde_json::from_str(r#"{"storage":"x","type":"dir","content":"vztmpl,images"}"#)
                .unwrap();
        assert!(s.has_content("images"));
        assert!(!s.has_content("image"));
        assert!(!s.has_content("vz"));
    }

    #[test]
    fn network_interfaces_decode_and_identify_bridges() {
        let n: Vec<NetworkIface> = decode_data(
            r#"{"data":[{"iface":"vmbr0","type":"bridge","active":1,"bridge_vlan_aware":1},
                       {"iface":"eth0","type":"eth","active":1}]}"#,
        )
        .unwrap();
        assert!(n[0].is_bridge());
        assert!(n[0].vlan_aware);
        assert!(!n[1].is_bridge());
    }

    #[test]
    fn task_status_running_stopped_ok_and_failed() {
        let run: TaskStatus = serde_json::from_str(r#"{"status":"running"}"#).unwrap();
        assert!(run.is_running());
        let ok: TaskStatus =
            serde_json::from_str(r#"{"status":"stopped","exitstatus":"OK"}"#).unwrap();
        assert!(!ok.is_running());
        assert!(ok.succeeded());
        let bad: TaskStatus =
            serde_json::from_str(r#"{"status":"stopped","exitstatus":"unable to create VM 100"}"#)
                .unwrap();
        assert!(!bad.succeeded());
    }

    #[test]
    fn a_stopped_task_without_an_exit_status_did_not_succeed() {
        let t: TaskStatus = serde_json::from_str(r#"{"status":"stopped"}"#).unwrap();
        assert!(!t.succeeded());
    }

    #[test]
    fn vm_status_running() {
        let s: VmStatus = decode_data(r#"{"data":{"status":"running","vmid":100,"name":"a","qmpstatus":"running","agent":1}}"#).unwrap();
        assert!(s.is_running());
        assert!(s.agent);
        let s: VmStatus = serde_json::from_str(r#"{"status":"stopped"}"#).unwrap();
        assert!(!s.is_running());
    }

    #[test]
    fn guest_interfaces_decode_kebab_case() {
        let r: GuestInterfaces = decode_data(
            r#"{"data":{"result":[{"name":"eth0","hardware-address":"bc:24:11:00:00:01",
               "ip-addresses":[{"ip-address":"192.0.2.10","ip-address-type":"ipv4","prefix":24},
                               {"ip-address":"fe80::1","ip-address-type":"ipv6","prefix":64}]}]}}"#,
        )
        .unwrap();
        let i = &r.result[0];
        assert_eq!(i.hardware_address.as_deref(), Some("bc:24:11:00:00:01"));
        assert_eq!(i.ip_addresses[0].ip_address, "192.0.2.10");
        assert!(i.ip_addresses[0].is_ipv4());
        assert!(!i.ip_addresses[1].is_ipv4());
    }

    #[test]
    fn volumes_decode() {
        let v: Vec<Volume> = decode_data(
            r#"{"data":[{"volid":"local:iso/seed-100.iso","content":"iso","size":374784,"format":"iso"}]}"#,
        )
        .unwrap();
        assert_eq!(v[0].volid, "local:iso/seed-100.iso");
    }

    #[test]
    fn vm_config_values_are_stringified() {
        let c: VmConfig = decode_data(
            r#"{"data":{"cores":4,"memory":"4096","net0":"virtio=BC:24:11:00:00:01,bridge=vmbr0","agent":1}}"#,
        )
        .unwrap();
        assert_eq!(c.get("cores"), Some("4"));
        assert_eq!(c.get("memory"), Some("4096"));
        assert_eq!(c.get("net0"), Some("virtio=BC:24:11:00:00:01,bridge=vmbr0"));
        assert_eq!(c.get("nope"), None);
    }

    #[test]
    fn clone_params_carry_only_what_was_asked() {
        let p = CloneParams::new(101).name("vm-a").full(true).params();
        assert_eq!(p.encode(), "newid=101&name=vm-a&full=1");
        let p = CloneParams::new(102)
            .target("pve2")
            .storage("local-lvm")
            .pool("banlieue")
            .params();
        assert_eq!(
            p.encode(),
            "newid=102&full=1&target=pve2&storage=local-lvm&pool=banlieue"
        );
    }

    #[test]
    fn clone_params_carry_a_description_when_given() {
        let p = CloneParams::new(101)
            .name("vm-a")
            .description("banlieue-machine-uid=abc")
            .params();
        assert_eq!(p.get("description"), Some("banlieue-machine-uid=abc"));
        assert_eq!(CloneParams::new(101).params().get("description"), None);
    }

    #[test]
    fn a_multi_line_description_survives_form_encoding() {
        let p = CloneParams::new(101).description("a\nb").params();
        assert_eq!(p.encode(), "newid=101&full=1&description=a%0Ab");
    }

    #[test]
    fn vm_config_exposes_the_description_key() {
        let c: VmConfig =
            decode_data(r#"{"data":{"description":"x\nbanlieue-machine-uid=abc"}}"#).unwrap();
        assert_eq!(c.get("description"), Some("x\nbanlieue-machine-uid=abc"));
    }
}
