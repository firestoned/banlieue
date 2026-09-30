// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `fake.rs`: every case is a refusal the real API makes.
//! A fake more permissive than Proxmox hides bugs (rules/testing.md, rule 1).

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::{CloneParams, Error, Params, ProxmoxApi};

    fn status_of(e: &Error) -> u16 {
        match e {
            Error::Api { status, .. } => *status,
            other => panic!("expected Api error, got {other}"),
        }
    }

    fn fake() -> FakeProxmox {
        let f = FakeProxmox::single_node("pve1");
        f.add_template("pve1", 9000, "tpl");
        f
    }

    #[tokio::test]
    async fn next_id_starts_at_100_and_skips_used_ids() {
        let f = fake();
        assert_eq!(f.next_id().await.unwrap().0, 100);
        f.add_vm("pve1", 100, "a");
        assert_eq!(f.next_id().await.unwrap().0, 101);
    }

    #[tokio::test]
    async fn clone_creates_a_stopped_vm() {
        let f = fake();
        let u = f
            .clone_vm("pve1", 9000, &CloneParams::new(100).name("a"))
            .await
            .unwrap();
        f.wait_task_with(
            &u,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap();
        let s = f.vm_status("pve1", 100).await.unwrap();
        assert!(!s.is_running());
        assert_eq!(s.name.as_deref(), Some("a"));
        let vms = f.cluster_vms().await.unwrap();
        assert!(vms.iter().any(|v| v.vmid == 100 && !v.template));
    }

    #[tokio::test]
    async fn clone_onto_an_existing_vmid_is_refused() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        let e = f
            .clone_vm("pve1", 9000, &CloneParams::new(100))
            .await
            .unwrap_err();
        assert_eq!(status_of(&e), 500);
        assert!(e.to_string().contains("already exists"), "{e}");
    }

    #[tokio::test]
    async fn clone_from_a_missing_template_is_a_not_found() {
        let f = fake();
        let e = f
            .clone_vm("pve1", 4242, &CloneParams::new(100))
            .await
            .unwrap_err();
        assert!(e.is_not_found(), "{e}");
    }

    #[tokio::test]
    async fn a_linked_clone_of_a_non_template_is_refused() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        let e = f
            .clone_vm("pve1", 100, &CloneParams::new(101).full(false))
            .await
            .unwrap_err();
        assert_eq!(status_of(&e), 500);
        assert!(e.to_string().contains("linked clone"), "{e}");
    }

    #[tokio::test]
    async fn clone_to_an_unknown_node_is_refused() {
        let f = fake();
        let e = f
            .clone_vm("nope", 9000, &CloneParams::new(100))
            .await
            .unwrap_err();
        assert!(e.is_not_found() || status_of(&e) == 500, "{e}");
    }

    #[tokio::test]
    async fn config_of_a_missing_vm_reads_as_the_500_proxmox_uses() {
        let f = fake();
        let e = f.vm_config("pve1", 555).await.unwrap_err();
        assert_eq!(status_of(&e), 500);
        assert!(e.is_not_found());
    }

    #[tokio::test]
    async fn setting_config_on_a_missing_vm_is_refused() {
        let f = fake();
        assert!(
            f.set_vm_config("pve1", 555, &Params::new().set("cores", 1))
                .await
                .unwrap_err()
                .is_not_found()
        );
    }

    #[tokio::test]
    async fn config_merges_and_delete_key_removes() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        f.set_vm_config(
            "pve1",
            100,
            &Params::new().set("cores", 2).set("memory", 1024),
        )
        .await
        .unwrap();
        f.set_vm_config(
            "pve1",
            100,
            &Params::new().set("cores", 4).set("delete", "memory"),
        )
        .await
        .unwrap();
        let c = f.vm_config("pve1", 100).await.unwrap();
        assert_eq!(c.get("cores"), Some("4"));
        assert_eq!(c.get("memory"), None);
    }

    #[tokio::test]
    async fn start_a_running_vm_is_refused() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        f.start_vm("pve1", 100).await.unwrap();
        let e = f.start_vm("pve1", 100).await.unwrap_err();
        assert_eq!(status_of(&e), 500);
        assert!(e.to_string().contains("already running"), "{e}");
    }

    #[tokio::test]
    async fn stop_and_shutdown_power_off() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        f.start_vm("pve1", 100).await.unwrap();
        f.shutdown_vm("pve1", 100).await.unwrap();
        assert!(!f.vm_status("pve1", 100).await.unwrap().is_running());
        f.start_vm("pve1", 100).await.unwrap();
        f.stop_vm("pve1", 100).await.unwrap();
        assert!(!f.vm_status("pve1", 100).await.unwrap().is_running());
    }

    #[tokio::test]
    async fn shutdown_of_a_stopped_vm_is_refused() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        let e = f.shutdown_vm("pve1", 100).await.unwrap_err();
        assert!(e.to_string().contains("not running"), "{e}");
    }

    #[tokio::test]
    async fn deleting_a_running_vm_is_refused() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        f.start_vm("pve1", 100).await.unwrap();
        let e = f.delete_vm("pve1", 100).await.unwrap_err();
        assert!(e.to_string().contains("running"), "{e}");
    }

    #[tokio::test]
    async fn deleting_a_stopped_vm_removes_it() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        f.delete_vm("pve1", 100).await.unwrap();
        assert!(f.vm_status("pve1", 100).await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn deleting_a_missing_vm_is_not_found() {
        let f = fake();
        assert!(f.delete_vm("pve1", 100).await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn resize_cannot_shrink_a_disk() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        f.set_vm_config(
            "pve1",
            100,
            &Params::new().set("scsi0", "local-lvm:vm-100-disk-0,size=20G"),
        )
        .await
        .unwrap();
        let e = f
            .resize_disk("pve1", 100, "scsi0", "10G")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("shrinking"), "{e}");
    }

    #[tokio::test]
    async fn resize_can_grow_a_disk_and_updates_the_config() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        f.set_vm_config(
            "pve1",
            100,
            &Params::new().set("scsi0", "local-lvm:vm-100-disk-0,size=20G"),
        )
        .await
        .unwrap();
        let upid = f.resize_disk("pve1", 100, "scsi0", "40G").await.unwrap();
        // PVE 9 runs a resize as a task, and so does the fake.
        let upid = upid.expect("the fake answers like PVE 9: a task");
        assert_eq!(upid.task_type(), "resize");
        f.wait_task_with(
            &upid,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert!(
            f.vm_config("pve1", 100)
                .await
                .unwrap()
                .get("scsi0")
                .unwrap()
                .contains("size=40G")
        );
    }

    #[tokio::test]
    async fn resize_of_an_absent_disk_is_refused() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        assert!(f.resize_disk("pve1", 100, "scsi9", "1G").await.is_err());
    }

    #[tokio::test]
    async fn upload_needs_a_storage_that_allows_iso() {
        let f = fake();
        let e = f
            .upload_iso("pve1", "local-lvm", "seed.iso", b"x".to_vec())
            .await
            .unwrap_err();
        assert_eq!(status_of(&e), 500);
        assert!(e.to_string().contains("content type"), "{e}");
    }

    #[tokio::test]
    async fn uploaded_isos_are_listed_and_deletable_by_volid() {
        let f = fake();
        f.upload_iso("pve1", "local", "seed-100.iso", b"x".to_vec())
            .await
            .unwrap();
        let vols = f.storage_content("pve1", "local").await.unwrap();
        assert!(vols.iter().any(|v| v.volid == "local:iso/seed-100.iso"));
        f.delete_volume("pve1", "local", "local:iso/seed-100.iso")
            .await
            .unwrap();
        assert!(f.storage_content("pve1", "local").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn uploading_over_an_existing_iso_replaces_it_like_proxmox() {
        let f = fake();
        f.upload_iso("pve1", "local", "seed.iso", b"a".to_vec())
            .await
            .unwrap();
        f.upload_iso("pve1", "local", "seed.iso", b"bb".to_vec())
            .await
            .unwrap();
        let vols = f.storage_content("pve1", "local").await.unwrap();
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].size, Some(2));
    }

    #[tokio::test]
    async fn deleting_a_missing_volume_is_refused() {
        let f = fake();
        assert!(
            f.delete_volume("pve1", "local", "local:iso/none.iso")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn guest_agent_is_unavailable_unless_the_vm_is_running_with_an_agent() {
        let f = fake();
        f.add_vm("pve1", 100, "a");
        let e = f.agent_interfaces("pve1", 100).await.unwrap_err();
        assert!(e.to_string().contains("not running"), "{e}");
        f.start_vm("pve1", 100).await.unwrap();
        let e = f.agent_interfaces("pve1", 100).await.unwrap_err();
        assert!(e.to_string().contains("agent"), "{e}");
        f.set_vm_config("pve1", 100, &Params::new().set("agent", "1"))
            .await
            .unwrap();
        f.set_guest_interfaces(100, vec![]);
        assert!(f.agent_interfaces("pve1", 100).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn inventory_reflects_what_was_configured() {
        let f = fake();
        assert_eq!(f.list_nodes().await.unwrap()[0].node, "pve1");
        assert!(
            f.node_storage("pve1")
                .await
                .unwrap()
                .iter()
                .any(|s| s.usable_for("images"))
        );
        assert!(
            f.node_networks("pve1")
                .await
                .unwrap()
                .iter()
                .any(|n| n.is_bridge())
        );
        assert!(f.node_storage("nope").await.is_err());
        assert_eq!(f.version().await.unwrap().major(), Some(9));
    }

    #[tokio::test]
    async fn a_task_on_another_node_is_unknown() {
        let f = fake();
        let upid = crate::Upid::parse("UPID:pve9:1:2:3:qmstart:100:x@pve!t:").unwrap();
        assert!(f.task_status(&upid).await.is_err());
    }

    #[tokio::test]
    async fn added_nodes_storages_and_bridges_are_reported() {
        let f = fake();
        f.add_node("pve2");
        f.add_storage("nfs", "nfs", "images,iso");
        f.add_bridge("vmbr1");
        let nodes: Vec<String> = f
            .list_nodes()
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.node)
            .collect();
        assert_eq!(nodes, ["pve1", "pve2"]);
        assert!(
            f.node_storage("pve2")
                .await
                .unwrap()
                .iter()
                .any(|s| s.storage == "nfs")
        );
        let nets = f.node_networks("pve2").await.unwrap();
        assert!(nets.iter().any(|n| n.iface == "vmbr1" && n.is_bridge()));
        assert!(nets.iter().any(|n| n.iface == "vmbr0"));
    }

    #[tokio::test]
    async fn adding_a_storage_twice_replaces_it() {
        let f = fake();
        f.add_storage("local-lvm", "lvmthin", "rootdir");
        let s = f.node_storage("pve1").await.unwrap();
        assert_eq!(s.iter().filter(|s| s.storage == "local-lvm").count(), 1);
        assert!(
            !s.iter()
                .find(|s| s.storage == "local-lvm")
                .unwrap()
                .usable_for("images")
        );
    }

    #[tokio::test]
    async fn an_offline_node_reports_offline() {
        let f = fake();
        f.add_node("pve2");
        f.set_node_offline("pve2");
        let nodes = f.list_nodes().await.unwrap();
        assert!(nodes.iter().find(|n| n.node == "pve1").unwrap().is_online());
        assert!(!nodes.iter().find(|n| n.node == "pve2").unwrap().is_online());
    }

    #[tokio::test]
    async fn clone_writes_the_description_into_the_new_vms_config() {
        let f = fake();
        f.clone_vm(
            "pve1",
            9000,
            &CloneParams::new(100).description("banlieue-machine-uid=abc"),
        )
        .await
        .unwrap();
        let c = f.vm_config("pve1", 100).await.unwrap();
        assert_eq!(c.get("description"), Some("banlieue-machine-uid=abc"));
    }

    #[tokio::test]
    async fn clone_without_a_description_does_not_inherit_the_templates() {
        let f = fake();
        f.set_vm_config(
            "pve1",
            9000,
            &Params::new().set("description", "the template"),
        )
        .await
        .unwrap();
        f.clone_vm("pve1", 9000, &CloneParams::new(100))
            .await
            .unwrap();
        assert_eq!(
            f.vm_config("pve1", 100).await.unwrap().get("description"),
            None
        );
    }

    #[tokio::test]
    async fn a_renamed_vm_reports_its_new_name_and_keeps_its_config() {
        let f = fake();
        f.add_vm("pve1", 100, "old");
        f.set_vm_config("pve1", 100, &Params::new().set("cores", 2))
            .await
            .unwrap();
        f.rename_vm(100, "new");
        let vms = f.cluster_vms().await.unwrap();
        assert_eq!(
            vms.iter().find(|v| v.vmid == 100).unwrap().name.as_deref(),
            Some("new")
        );
        assert_eq!(
            f.vm_config("pve1", 100).await.unwrap().get("cores"),
            Some("2")
        );
    }
}
