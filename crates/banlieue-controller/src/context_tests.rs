// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::Context`].

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::VirtualMachine;
    use kube::runtime::reflector;
    use kube::{Client, Config};

    use super::super::*;

    /// A client that is never used to make a request: building one does not
    /// connect, so no cluster is needed.
    fn offline_client() -> Client {
        let url = "http://127.0.0.1:1".parse().expect("valid URL");
        Client::try_from(Config::new(url)).expect("client from static config")
    }

    #[tokio::test]
    async fn new_context_has_no_vm_store() {
        let ctx = Context::new(offline_client(), None);
        assert!(
            ctx.vm_store.is_none(),
            "outside the running controller the duplicate-address check must fall back to a LIST"
        );
    }

    #[tokio::test]
    async fn with_vm_store_attaches_the_store_and_keeps_the_scope() {
        let (reader, _writer) = reflector::store::<VirtualMachine>();
        let ctx = Context::new(offline_client(), Some("ns".into())).with_vm_store(reader);
        assert!(ctx.vm_store.is_some());
        assert_eq!(ctx.namespace.as_deref(), Some("ns"));
    }

    /// ADR-0088 Decision 7: first-boot sealing is off unless the operator
    /// turned it on, so a context built the ordinary way refuses every image
    /// that declares it.
    #[tokio::test]
    async fn first_boot_sealing_is_off_by_default() {
        assert!(!Context::new(offline_client(), None).first_boot_sealing);
    }

    #[tokio::test]
    async fn with_first_boot_sealing_sets_the_flag_and_keeps_the_rest() {
        let (reader, _writer) = reflector::store::<VirtualMachine>();
        let ctx = Context::new(offline_client(), Some("ns".into()))
            .with_vm_store(reader)
            .with_first_boot_sealing(true);
        assert!(ctx.first_boot_sealing);
        assert!(ctx.vm_store.is_some());
        assert_eq!(ctx.namespace.as_deref(), Some("ns"));
        assert!(!ctx.with_first_boot_sealing(false).first_boot_sealing);
    }
}
