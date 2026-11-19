// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::finalize_vm`] (ADR-0026: `VSphereMachine`
//! deletion lifecycle).

#[cfg(test)]
mod tests {
    use crate::client::{FakeClient, Inventory};
    use crate::reconciler::test_fixtures::{UID, as_client, base_spec, machine_ref};

    use banlieue_api::infrastructure::VSphereMachineSpec;

    use super::super::{compensate_gone, finalize_unrecorded, finalize_vm, is_gone};
    use crate::error::Error;

    fn spec() -> VSphereMachineSpec {
        let mut spec = base_spec();
        spec.num_cpus = 2;
        spec.memory_mi_b = 2048;
        spec.disks[0].size_gi_b = 20;
        spec.network.clear();
        spec
    }

    fn with_dc() -> FakeClient {
        FakeClient::new(Inventory::builder().with_dc("dc1").build())
    }

    fn api_error(code: u16, reason: &str) -> kube::Error {
        kube::Error::Api(
            kube::core::Status::failure("test", reason)
                .with_code(code)
                .boxed(),
        )
    }

    #[tokio::test]
    async fn destroys_the_backend_vm_when_one_exists() {
        let client = FakeClient::new(Inventory::default());
        finalize_vm(as_client(&client), Some("vm-existing-123"))
            .await
            .unwrap();
        assert_eq!(client.destroyed_vms(), vec!["vm-existing-123".to_string()]);
    }

    #[tokio::test]
    async fn is_a_noop_when_no_vm_was_ever_created() {
        // Create never got past the "resolve refs" stage far enough to
        // clone a VM — status.vmRef was never set. Nothing to destroy, and
        // must not error (or the finalizer would never clear).
        let client = FakeClient::new(Inventory::default());
        finalize_vm(as_client(&client), None).await.unwrap();
        assert!(client.destroyed_vms().is_empty());
    }

    #[test]
    fn a_machine_gone_from_the_api_is_recognised() {
        assert!(is_gone(&Error::Kube(api_error(404, "NotFound"))));
        assert!(is_gone(&Error::Sdk(banlieue_provider_sdk::Error::Kube(
            api_error(404, "NotFound")
        ))));
        assert!(!is_gone(&Error::Kube(api_error(409, "Conflict"))));
        assert!(!is_gone(&Error::Vsphere("session expired".into())));
    }

    /// The machine was deleted while this pass cloned its VM: the VM it
    /// just created is destroyed, since no finalizer will ever see its
    /// moref (bug-177).
    #[tokio::test]
    async fn a_pass_that_finds_its_machine_gone_destroys_the_vm_it_cloned() {
        let client = FakeClient::new(Inventory::default());
        compensate_gone(as_client(&client), Some("vm-just-cloned"))
            .await
            .unwrap();
        assert_eq!(client.destroyed_vms(), vec!["vm-just-cloned".to_string()]);
    }

    /// A pass that failed before cloning has nothing to remove.
    #[tokio::test]
    async fn a_pass_that_cloned_nothing_removes_nothing() {
        let client = FakeClient::new(Inventory::default());
        compensate_gone(as_client(&client), None).await.unwrap();
        assert!(client.destroyed_vms().is_empty());
    }

    // ADR-0101 Decision 4: deletion with no status.vmRef looks for a VM the
    // pass that lost its moref left behind, and destroys only a marked one.

    #[tokio::test]
    async fn deletion_without_a_vm_ref_destroys_our_marked_vm() {
        let client = with_dc();
        let ours = client.add_vm(None, "db-01", Some(UID));
        finalize_unrecorded(as_client(&client), &spec(), &machine_ref())
            .await
            .unwrap();
        assert_eq!(client.destroyed_vms(), vec![ours]);
    }

    #[tokio::test]
    async fn deletion_without_a_vm_ref_never_touches_a_foreign_same_named_vm() {
        for foreign_uid in [None, Some("another-machine-uid")] {
            let client = with_dc();
            client.add_vm(None, "db-01", foreign_uid);
            finalize_unrecorded(as_client(&client), &spec(), &machine_ref())
                .await
                .unwrap();
            assert!(client.destroyed_vms().is_empty(), "{foreign_uid:?}");
        }
    }

    #[tokio::test]
    async fn deletion_without_a_vm_ref_and_nothing_there_succeeds() {
        let client = with_dc();
        finalize_unrecorded(as_client(&client), &spec(), &machine_ref())
            .await
            .unwrap();
        assert!(client.destroyed_vms().is_empty());
    }

    /// A datacenter that no longer exists cannot hold the VM; deletion must
    /// not block on it forever.
    #[tokio::test]
    async fn deletion_without_a_vm_ref_in_a_missing_datacenter_succeeds() {
        let client = FakeClient::new(Inventory::default());
        finalize_unrecorded(as_client(&client), &spec(), &machine_ref())
            .await
            .unwrap();
        assert!(client.destroyed_vms().is_empty());
    }
}
