// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Fixtures shared by this crate's unit tests. Placeholders only: the
//! documentation account `111122223333`, the IDs the examples use, and the
//! zones of `FakeEc2::ca_central_1`.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    Ec2Connection, InstanceSizeMapping, NetworkClassMapping, Provider, ProviderCapabilities,
    ProviderConnection, ProviderSpec, ScopedTarget, StorageClassMapping,
};
use banlieue_api::common::LocalObjectReference;
use kube::api::ObjectMeta;

/// Region of `FakeEc2::ca_central_1`.
pub const REGION: &str = "ca-central-1";
/// The region's EC2 endpoint.
pub const ENDPOINT: &str = "https://ec2.ca-central-1.amazonaws.com";
/// A role in the documentation account.
pub const ROLE_ARN: &str = "arn:aws:iam::111122223333:role/banlieue-ec2-provider";
/// Provider name.
pub const PROVIDER: &str = "ec2-cc";
/// Provider (home) namespace.
pub const HOME: &str = "banlieue-system";
/// Zone `ca-central-1a`, ID `cac1-az1` in the fake.
pub const ZONE_A: &str = "ca-central-1a";
/// Its zone ID.
pub const ZONE_A_ID: &str = "cac1-az1";
/// Zone `ca-central-1b`.
pub const ZONE_B: &str = "ca-central-1b";
/// Zone `ca-central-1d`.
pub const ZONE_D: &str = "ca-central-1d";
/// A type the fake offers in every zone, with NitroTPM.
pub const TYPE_SMALL: &str = "t3.small";
/// Another type the fake offers in every zone, with NitroTPM.
pub const TYPE_LARGE: &str = "m7i.large";
/// A subnet.
pub const SUBNET_ID: &str = "subnet-0123456789abcdef0";
/// A security group.
pub const SECURITY_GROUP_ID: &str = "sg-0123456789abcdef0";
/// An AMI.
pub const IMAGE_ID: &str = "ami-0123456789abcdef0";
/// An extra tag an account's tag policy asks for.
pub const EXTRA_TAG: (&str, &str) = ("cost-centre", "1234");

/// The `ec2` connection block.
#[must_use]
pub fn ec2() -> Ec2Connection {
    Ec2Connection {
        region: REGION.into(),
        role_arn: ROLE_ARN.into(),
        ..Ec2Connection::default()
    }
}

/// The `ec2` connection block with an extra tag.
#[must_use]
pub fn governed_ec2() -> Ec2Connection {
    Ec2Connection {
        tags: BTreeMap::from([(EXTRA_TAG.0.to_string(), EXTRA_TAG.1.to_string())]),
        ..ec2()
    }
}

/// A connection, with or without its `ec2` block.
#[must_use]
pub fn connection(ec2_block: Option<Ec2Connection>) -> ProviderConnection {
    ProviderConnection {
        endpoint: ENDPOINT.into(),
        credentials_ref: None,
        insecure_skip_tls_verify: false,
        ca_bundle: None,
        azure: None,
        ec2: ec2_block,
    }
}

/// A one-key target map.
#[must_use]
pub fn target(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// An instance type mapping.
#[must_use]
pub fn size(vm_class: &str, instance_type: &str) -> InstanceSizeMapping {
    InstanceSizeMapping {
        vm_class: vm_class.into(),
        size: instance_type.into(),
        per_zone: Vec::new(),
    }
}

/// A storage class with a `volumeType` target.
#[must_use]
pub fn storage(name: &str, volume_type: &str) -> StorageClassMapping {
    StorageClassMapping {
        name: name.into(),
        target: Some(target(&[("volumeType", volume_type)])),
        ..Default::default()
    }
}

/// A network class with a subnet and security group, only in `zones`.
#[must_use]
pub fn network(name: &str, zones: &[&str]) -> NetworkClassMapping {
    NetworkClassMapping {
        name: name.into(),
        target: None,
        per_zone: zones
            .iter()
            .map(|zone| ScopedTarget {
                zone: Some((*zone).into()),
                target: target(&[
                    ("subnetId", SUBNET_ID),
                    ("securityGroupIds", SECURITY_GROUP_ID),
                ]),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// An `ec2` `Provider` with `capabilities`.
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
            provider_class_ref: LocalObjectReference { name: "ec2".into() },
            connection: connection(Some(ec2())),
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

/// Launches an instance in the fake as banlieue would for an `EC2Machine`
/// with `uid` in `namespace`, served by [`PROVIDER`], and describes it
/// until the fake shows it. Returns its instance ID.
pub async fn launch_owned(fake: &banlieue_aws::FakeEc2, uid: &str, namespace: &str) -> String {
    use banlieue_aws::Ec2Api;
    use banlieue_aws::instance::{RunInstancesRequest, VolumeType, client_token_for};
    use banlieue_aws::tags::OwnershipTags;

    let tags = OwnershipTags::new(
        uid,
        crate::reconciler::instances::OWNER_KIND,
        HOME,
        PROVIDER,
    )
    .with_owner_namespace(namespace)
    .resource_tags(Some("vm"), None, &BTreeMap::new())
    .expect("valid tags");
    let request = RunInstancesRequest {
        image_id: IMAGE_ID.into(),
        instance_type: TYPE_SMALL.into(),
        zone_name: ZONE_A.into(),
        subnet_id: SUBNET_ID.into(),
        security_group_ids: vec![SECURITY_GROUP_ID.into()],
        root_device_name: "/dev/xvda".into(),
        root_volume_size_gib: 10,
        volume_type: VolumeType::Gp3,
        iops: None,
        throughput: None,
        kms_key_id: None,
        user_data: Vec::new(),
        client_token: client_token_for(uid, ZONE_A),
        instance_tags: tags.clone(),
        volume_tags: tags.clone(),
        network_interface_tags: tags,
    };
    let id = fake.run_instance(&request).await.expect("launches");
    for _ in 0..=banlieue_aws::fake::DEFAULT_VISIBILITY_DELAY {
        if fake
            .describe_instance(&id)
            .await
            .expect("describes")
            .is_some()
        {
            return id;
        }
    }
    panic!("{id} never became visible");
}
