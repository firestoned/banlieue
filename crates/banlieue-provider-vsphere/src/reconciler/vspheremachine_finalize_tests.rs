// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::finalize_vm`] (ADR-0026: `VSphereMachine`
//! deletion lifecycle).

#[cfg(test)]
mod tests {
    use crate::client::{FakeClient, Inventory, VSphereClient};

    use super::super::finalize_vm;

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
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
}
