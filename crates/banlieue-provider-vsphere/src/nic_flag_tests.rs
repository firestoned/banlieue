// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::nic_flag`] (ADR-0031).

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::{NicAdapter, VMImageTemplateNic};

    use super::super::{parse_nic_flag, serialize_nic_flag};

    #[test]
    fn parse_all_fields() {
        let nic = parse_nic_flag("network=vmnet-prod,adapter=vmxnet3,pciSlot=192").unwrap();
        assert_eq!(
            nic,
            VMImageTemplateNic {
                network: Some("vmnet-prod".to_string()),
                adapter: Some(NicAdapter::Vmxnet3),
                pci_slot: Some(192),
            }
        );
    }

    #[test]
    fn parse_empty_string_is_all_defaults() {
        assert_eq!(parse_nic_flag("").unwrap(), VMImageTemplateNic::default());
    }

    #[test]
    fn parse_partial_fields_leaves_the_rest_unset() {
        let nic = parse_nic_flag("network=vmnet-mgmt").unwrap();
        assert_eq!(nic.network.as_deref(), Some("vmnet-mgmt"));
        assert!(nic.adapter.is_none());
        assert!(nic.pci_slot.is_none());
    }

    #[test]
    fn parse_fields_in_any_order() {
        let nic = parse_nic_flag("pciSlot=193,network=vmnet-mgmt,adapter=e1000").unwrap();
        assert_eq!(
            nic,
            VMImageTemplateNic {
                network: Some("vmnet-mgmt".to_string()),
                adapter: Some(NicAdapter::E1000),
                pci_slot: Some(193),
            }
        );
    }

    #[test]
    fn parse_rejects_unknown_key() {
        let err = parse_nic_flag("bogus=x").unwrap_err();
        assert!(err.contains("bogus"), "{err}");
    }

    #[test]
    fn parse_rejects_invalid_adapter() {
        let err = parse_nic_flag("adapter=not-a-real-adapter").unwrap_err();
        assert!(err.contains("adapter"), "{err}");
    }

    #[test]
    fn parse_rejects_invalid_pci_slot() {
        let err = parse_nic_flag("pciSlot=not-a-number").unwrap_err();
        assert!(err.contains("pciSlot"), "{err}");
    }

    #[test]
    fn parse_rejects_a_malformed_pair_with_no_equals_sign() {
        let err = parse_nic_flag("network").unwrap_err();
        assert!(err.contains("network"), "{err}");
    }

    #[test]
    fn serialize_all_fields_set() {
        let nic = VMImageTemplateNic {
            network: Some("vmnet-prod".to_string()),
            adapter: Some(NicAdapter::Vmxnet3),
            pci_slot: Some(192),
        };
        assert_eq!(
            serialize_nic_flag(&nic),
            "network=vmnet-prod,adapter=vmxnet3,pciSlot=192"
        );
    }

    #[test]
    fn serialize_omits_unset_fields() {
        let nic = VMImageTemplateNic {
            network: Some("vmnet-prod".to_string()),
            adapter: None,
            pci_slot: None,
        };
        assert_eq!(serialize_nic_flag(&nic), "network=vmnet-prod");
    }

    #[test]
    fn serialize_all_unset_is_empty_string() {
        assert_eq!(serialize_nic_flag(&VMImageTemplateNic::default()), "");
    }

    #[test]
    fn round_trips_through_parse() {
        let nic = VMImageTemplateNic {
            network: Some("vmnet-mgmt".to_string()),
            adapter: Some(NicAdapter::E1000e),
            pci_slot: Some(224),
        };
        let flag = serialize_nic_flag(&nic);
        assert_eq!(parse_nic_flag(&flag).unwrap(), nic);
    }
}
