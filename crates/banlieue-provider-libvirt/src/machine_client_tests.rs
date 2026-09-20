// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `machine_client.rs`.
//!
//! The session-backed impl is exercised by `tests/live_libvirtd.rs` against a
//! real daemon — nothing here can prove our reading of the wire format. What
//! *is* testable offline is the decision logic layered on top of it: the
//! not-found mapping and the address-source fallback order.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_libvirt::{
        DomainInterface, DomainIpAddr, InterfaceAddressSource, TransportError, VIR_ERR_NO_DOMAIN,
        VIR_ERR_NO_STORAGE_VOL,
    };

    // ------------------------------------------------------------------
    // not-found mapping
    // ------------------------------------------------------------------

    #[test]
    fn optional_maps_not_found_to_none() {
        let r: banlieue_libvirt::Result<u8> = Err(TransportError::Remote {
            code: VIR_ERR_NO_DOMAIN,
            message: "Domain not found".into(),
        });
        assert_eq!(optional(r).unwrap(), None);
    }

    #[test]
    fn optional_passes_a_value_through() {
        let r: banlieue_libvirt::Result<u8> = Ok(7);
        assert_eq!(optional(r).unwrap(), Some(7));
    }

    /// A permission failure read as "absent" would make the reconciler
    /// recreate an object that already exists — on a storage pool, that is a
    /// second copy of a VM's disk.
    #[test]
    fn optional_propagates_a_real_error() {
        let r: banlieue_libvirt::Result<u8> = Err(TransportError::Remote {
            code: 45,
            message: "access denied".into(),
        });
        assert!(optional(r).is_err());
    }

    #[test]
    fn idempotent_treats_absent_as_success() {
        let r: banlieue_libvirt::Result<()> = Err(TransportError::Remote {
            code: VIR_ERR_NO_STORAGE_VOL,
            message: "Storage volume not found".into(),
        });
        assert!(idempotent(r).is_ok());
    }

    #[test]
    fn idempotent_still_propagates_a_real_error() {
        let r: banlieue_libvirt::Result<()> = Err(TransportError::Remote {
            code: 55,
            message: "operation failed".into(),
        });
        assert!(
            idempotent(r).is_err(),
            "a teardown step must never swallow a real failure"
        );
    }

    // ------------------------------------------------------------------
    // Address source order
    // ------------------------------------------------------------------

    /// Authority descending. The agent knows what the guest configured; a
    /// lease only knows what DHCP offered, which the guest may have ignored;
    /// ARP is an inference. Reporting a lease address the guest never took
    /// sends every consumer to the wrong host, so the order is load-bearing
    /// rather than cosmetic.
    #[test]
    fn address_sources_are_tried_most_authoritative_first() {
        assert_eq!(
            ADDRESS_SOURCE_ORDER,
            [
                InterfaceAddressSource::Agent,
                InterfaceAddressSource::Lease,
                InterfaceAddressSource::Arp,
            ]
        );
    }

    // ------------------------------------------------------------------
    // The fake, and the fallback it exists to exercise
    // ------------------------------------------------------------------

    fn iface(name: &str, addr: &str) -> DomainInterface {
        DomainInterface {
            name: name.to_string(),
            hwaddr: Some("52:54:00:00:00:01".to_string()),
            addrs: vec![DomainIpAddr {
                kind: 0,
                addr: addr.to_string(),
                prefix: 24,
            }],
        }
    }

    #[tokio::test]
    async fn fake_returns_the_first_source_that_has_addresses() {
        let mut c = FakeMachineClient::default();
        // Agent is silent (guest agent not installed), lease answers.
        c.addresses_by_source.insert(
            InterfaceAddressSource::Lease as u32,
            vec![iface("vnet0", "192.0.2.24")],
        );

        let dom = FakeMachineClient::domain("sandbox-01");
        let (ifaces, source) = c.domain_addresses(&dom).await.unwrap().expect("addresses");
        assert_eq!(source, InterfaceAddressSource::Lease);
        assert_eq!(ifaces[0].addrs[0].addr, "192.0.2.24");
    }

    /// When the agent *does* answer it wins, even though the lease also has
    /// something to say — which is the case the ordering exists for.
    #[tokio::test]
    async fn fake_prefers_the_agent_over_a_lease() {
        let mut c = FakeMachineClient::default();
        c.addresses_by_source.insert(
            InterfaceAddressSource::Agent as u32,
            vec![iface("enp1s0", "192.0.2.10")],
        );
        c.addresses_by_source.insert(
            InterfaceAddressSource::Lease as u32,
            vec![iface("vnet0", "192.0.2.99")],
        );

        let dom = FakeMachineClient::domain("sandbox-01");
        let (ifaces, source) = c.domain_addresses(&dom).await.unwrap().expect("addresses");
        assert_eq!(source, InterfaceAddressSource::Agent);
        assert_eq!(ifaces[0].addrs[0].addr, "192.0.2.10");
    }

    /// A guest that has not finished booting has no address from any source.
    /// That is the normal early state, so it is `None`, not an error — a
    /// reconciler must requeue, not fail the machine.
    #[tokio::test]
    async fn fake_reports_no_addresses_as_none_not_an_error() {
        let mut c = FakeMachineClient::default();
        let dom = FakeMachineClient::domain("sandbox-01");
        assert!(c.domain_addresses(&dom).await.unwrap().is_none());
    }

    /// An interface with no addresses does not count as an answer: libvirt
    /// lists the interface as soon as the domain has one, long before the
    /// guest has configured it.
    #[tokio::test]
    async fn fake_ignores_a_source_that_lists_interfaces_without_addresses() {
        let mut c = FakeMachineClient::default();
        c.addresses_by_source.insert(
            InterfaceAddressSource::Agent as u32,
            vec![DomainInterface {
                name: "enp1s0".to_string(),
                hwaddr: None,
                addrs: vec![],
            }],
        );
        c.addresses_by_source.insert(
            InterfaceAddressSource::Arp as u32,
            vec![iface("vnet0", "192.0.2.7")],
        );

        let dom = FakeMachineClient::domain("sandbox-01");
        let (_, source) = c.domain_addresses(&dom).await.unwrap().expect("addresses");
        assert_eq!(source, InterfaceAddressSource::Arp);
    }

    // ------------------------------------------------------------------
    // Lifecycle bookkeeping the reconciler depends on
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn fake_records_the_full_teardown_order() {
        let mut c = FakeMachineClient::default();
        let dom = FakeMachineClient::domain("sandbox-01");
        c.destroy_domain(&dom).await.unwrap();
        c.undefine_domain(&dom).await.unwrap();
        assert_eq!(c.calls, vec!["destroy:sandbox-01", "undefine:sandbox-01"]);
    }

    #[tokio::test]
    async fn fake_lookup_returns_none_until_defined() {
        let mut c = FakeMachineClient::default();
        assert!(c.lookup_domain("sandbox-01").await.unwrap().is_none());
        c.define_domain("<domain type='kvm'><name>sandbox-01</name></domain>")
            .await
            .unwrap();
        assert!(c.lookup_domain("sandbox-01").await.unwrap().is_some());
    }
}
