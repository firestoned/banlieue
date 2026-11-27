// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Fixtures shared by this crate's unit tests. Placeholders only: zero
//! GUIDs for tenant, client and subscription, `example.com` hosts.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    AzureConnection, InstanceSizeMapping, NetworkClassMapping, Provider, ProviderCapabilities,
    ProviderConnection, ProviderSpec, ScopedTarget, StorageClassMapping, ZonedInstanceSize,
};
use banlieue_api::common::LocalObjectReference;
use kube::api::ObjectMeta;

/// The subscription `FakeArm` puts every resource ID under.
pub const SUBSCRIPTION: &str = "00000000-0000-0000-0000-000000000000";
/// Placeholder tenant.
pub const TENANT: &str = "00000000-0000-0000-0000-000000000001";
/// Placeholder client.
pub const CLIENT: &str = "00000000-0000-0000-0000-000000000002";
/// Region of the captured fixtures.
pub const LOCATION: &str = "eastus2";
/// Resource group.
pub const RESOURCE_GROUP: &str = "rg-banlieue";
/// Provider name.
pub const PROVIDER: &str = "az-east";
/// Provider (home) namespace.
pub const HOME: &str = "banlieue-system";
/// ARM endpoint.
pub const ENDPOINT: &str = "https://management.azure.com";
/// A size the captured catalogue offers, unrestricted, in zones 1 to 4.
pub const USABLE_SIZE: &str = "Standard_D2s_v7";
/// A size this subscription may use in zones 1 and 4 only.
pub const PARTIAL_SIZE: &str = "Standard_D2as_v6";
/// A size the captured catalogue offers in no zone this subscription may use.
pub const RESTRICTED_SIZE: &str = "Standard_D2s_v6";
/// A subnet resource ID.
pub const SUBNET_ID: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-net/providers/Microsoft.Network/virtualNetworks/vnet/subnets/vms";
/// A gallery image version resource ID.
pub const IMAGE_ID: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-img/providers/Microsoft.Compute/galleries/banlieue/images/kairos/versions/1.0.0";

/// The `azure` connection block.
#[must_use]
pub fn azure() -> AzureConnection {
    AzureConnection {
        tenant_id: TENANT.into(),
        client_id: CLIENT.into(),
        subscription_id: SUBSCRIPTION.into(),
        location: LOCATION.into(),
        resource_group: RESOURCE_GROUP.into(),
        authority_host: None,
        image_gallery: None,
        publisher: None,
        ..AzureConnection::default()
    }
}

/// A disk encryption set in [`SUBSCRIPTION`] (ADR-0172).
pub const DES_ID: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1";
/// An extra tag a required-tag policy asks for (ADR-0172).
pub const EXTRA_TAG: (&str, &str) = ("cost-centre", "1234");

/// The `azure` connection block of a policy-governed subscription: an extra
/// tag, a disk encryption set and encryption at host (ADR-0172).
#[must_use]
pub fn governed_azure() -> AzureConnection {
    AzureConnection {
        tags: BTreeMap::from([(EXTRA_TAG.0.to_string(), EXTRA_TAG.1.to_string())]),
        disk_encryption_set_id: Some(DES_ID.into()),
        encryption_at_host: true,
        ..azure()
    }
}

/// A connection, with or without its `azure` block.
#[must_use]
pub fn connection(azure_block: Option<AzureConnection>) -> ProviderConnection {
    ProviderConnection {
        endpoint: ENDPOINT.into(),
        credentials_ref: None,
        insecure_skip_tls_verify: false,
        ca_bundle: None,
        azure: azure_block,
        ec2: None,
    }
}

/// A one-key target map.
#[must_use]
pub fn target(key: &str, value: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(key.to_string(), value.to_string())])
}

/// A size mapping.
#[must_use]
pub fn size(vm_class: &str, size: &str) -> InstanceSizeMapping {
    InstanceSizeMapping {
        vm_class: vm_class.into(),
        size: size.into(),
        per_zone: Vec::new(),
    }
}

/// A size mapping with one per-zone override.
#[must_use]
pub fn size_with_override(
    vm_class: &str,
    size: &str,
    zone: &str,
    zoned: &str,
) -> InstanceSizeMapping {
    InstanceSizeMapping {
        per_zone: vec![ZonedInstanceSize {
            zone: zone.into(),
            size: zoned.into(),
        }],
        ..self::size(vm_class, size)
    }
}

/// A storage class with a `sku` target.
#[must_use]
pub fn storage(name: &str, sku: &str) -> StorageClassMapping {
    StorageClassMapping {
        name: name.into(),
        target: Some(target("sku", sku)),
        ..Default::default()
    }
}

/// A network class with a `subnetId` target, optionally only in `zone`.
#[must_use]
pub fn network(name: &str, zone: Option<&str>) -> NetworkClassMapping {
    let Some(zone) = zone else {
        return NetworkClassMapping {
            name: name.into(),
            target: Some(target("subnetId", SUBNET_ID)),
            ..Default::default()
        };
    };
    NetworkClassMapping {
        name: name.into(),
        target: None,
        per_zone: vec![ScopedTarget {
            zone: Some(zone.into()),
            target: target("subnetId", SUBNET_ID),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// An `azure` `Provider` with `capabilities`.
#[must_use]
pub fn provider(capabilities: ProviderCapabilities) -> Provider {
    Provider {
        metadata: ObjectMeta {
            name: Some(PROVIDER.into()),
            namespace: Some(HOME.into()),
            generation: Some(1),
            ..Default::default()
        },
        spec: ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "azure".into(),
            },
            connection: connection(Some(azure())),
            capabilities,
            paused: false,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
            scheduling: None,
            namespace_selector: None,
        },
        status: None,
    }
}
