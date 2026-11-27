// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `instance.rs`.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::*;
    use crate::error::Error;

    fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn request() -> RunInstancesRequest {
        RunInstancesRequest {
            image_id: "ami-0123456789abcdef0".to_string(),
            instance_type: "m6i.large".to_string(),
            zone_name: "ca-central-1a".to_string(),
            subnet_id: "subnet-0123456789abcdef0".to_string(),
            security_group_ids: vec!["sg-0aaa".to_string(), "sg-0bbb".to_string()],
            root_device_name: "/dev/xvda".to_string(),
            root_volume_size_gib: 10,
            volume_type: VolumeType::default(),
            iops: None,
            throughput: None,
            kms_key_id: None,
            user_data: b"#cloud-config\n".to_vec(),
            client_token: client_token_for("6f1c2a9e-0b8d-4c3e-9a51-2f7d8e4b1c0a", "ca-central-1a"),
            instance_tags: tags(&[("Name", "vm-a"), ("banlieue-owner-uid", "u-1")]),
            volume_tags: tags(&[("banlieue-owner-uid", "u-1")]),
            network_interface_tags: tags(&[("banlieue-owner-uid", "u-1")]),
        }
    }

    fn get<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn has(params: &[(String, String)], key: &str, value: &str) -> bool {
        get(params, key) == Some(value)
    }

    const HARDENING: [(&str, &str); 11] = [
        ("MinCount", "1"),
        ("MaxCount", "1"),
        ("NetworkInterface.1.DeviceIndex", "0"),
        ("NetworkInterface.1.AssociatePublicIpAddress", "false"),
        ("NetworkInterface.1.DeleteOnTermination", "true"),
        ("MetadataOptions.HttpTokens", "required"),
        ("MetadataOptions.HttpPutResponseHopLimit", "1"),
        ("MetadataOptions.HttpEndpoint", "enabled"),
        ("BlockDeviceMapping.1.Ebs.Encrypted", "true"),
        ("BlockDeviceMapping.1.Ebs.DeleteOnTermination", "true"),
        ("BlockDeviceMapping.1.DeviceName", "/dev/xvda"),
    ];

    // ----- RunInstances --------------------------------------------------

    #[test]
    fn run_instances_emits_the_documented_query_names() {
        let p = request().to_params().expect("valid");
        assert!(has(&p, "ImageId", "ami-0123456789abcdef0"));
        assert!(has(&p, "InstanceType", "m6i.large"));
        assert!(has(&p, "Placement.AvailabilityZone", "ca-central-1a"));
        assert!(has(
            &p,
            "NetworkInterface.1.SubnetId",
            "subnet-0123456789abcdef0"
        ));
        assert!(has(&p, "NetworkInterface.1.SecurityGroupId.1", "sg-0aaa"));
        assert!(has(&p, "NetworkInterface.1.SecurityGroupId.2", "sg-0bbb"));
        assert!(has(&p, "BlockDeviceMapping.1.Ebs.VolumeSize", "10"));
        assert!(has(&p, "BlockDeviceMapping.1.Ebs.VolumeType", "gp3"));
        assert!(has(
            &p,
            "ClientToken",
            "banlieue-6f1c2a9e-0b8d-4c3e-9a51-2f7d8e4b1c0a-ca-central-1a"
        ));
    }

    #[test]
    fn run_instances_never_sets_a_top_level_subnet_or_security_group() {
        // With a NetworkInterface, EC2 refuses SubnetId/SecurityGroupId at the
        // top level: they belong to the interface.
        let p = request().to_params().expect("valid");
        assert!(get(&p, "SubnetId").is_none());
        assert!(!p.iter().any(|(k, _)| k.starts_with("SecurityGroupId.")));
    }

    #[test]
    fn run_instances_always_carries_the_hardening() {
        let mut with_options = request();
        with_options.kms_key_id = Some("alias/banlieue".to_string());
        with_options.iops = Some(4000);
        with_options.throughput = Some(250);
        with_options.user_data.clear();
        for req in [request(), with_options] {
            let p = req.to_params().expect("valid");
            for (k, v) in HARDENING {
                assert!(has(&p, k, v), "missing {k}={v}");
            }
        }
    }

    #[test]
    fn run_instances_never_names_an_instance_profile() {
        let p = request().to_params().expect("valid");
        assert!(!p.iter().any(|(k, _)| k.contains("IamInstanceProfile")));
    }

    #[test]
    fn run_instances_keys_are_unique() {
        let p = request().to_params().expect("valid");
        let mut keys: Vec<&str> = p.iter().map(|(k, _)| k.as_str()).collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), before);
    }

    #[test]
    fn run_instances_is_deterministic() {
        assert_eq!(request().to_params().ok(), request().to_params().ok());
    }

    #[test]
    fn user_data_is_base64_of_the_raw_bytes() {
        let p = request().to_params().expect("valid");
        let encoded = get(&p, "UserData").expect("user data");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        assert_eq!(decoded, b"#cloud-config\n");
    }

    #[test]
    fn empty_user_data_sends_no_parameter() {
        let mut req = request();
        req.user_data.clear();
        let p = req.to_params().expect("valid");
        assert!(get(&p, "UserData").is_none());
    }

    #[test]
    fn user_data_of_exactly_the_limit_is_accepted() {
        let mut req = request();
        req.user_data = vec![b'a'; MAX_USER_DATA_BYTES];
        assert!(req.to_params().is_ok());
    }

    #[test]
    fn user_data_over_the_limit_before_encoding_is_refused() {
        // 16,384 bytes raw is the limit (seen live, roadmap 24): the base64
        // form is larger, and is not what EC2 counts.
        let mut req = request();
        req.user_data = vec![b'a'; MAX_USER_DATA_BYTES + 1];
        assert!(matches!(req.to_params(), Err(Error::Invalid(_))));
    }

    #[test]
    fn optional_volume_settings_appear_only_when_set() {
        let p = request().to_params().expect("valid");
        assert!(get(&p, "BlockDeviceMapping.1.Ebs.KmsKeyId").is_none());
        assert!(get(&p, "BlockDeviceMapping.1.Ebs.Iops").is_none());
        assert!(get(&p, "BlockDeviceMapping.1.Ebs.Throughput").is_none());

        let mut req = request();
        req.kms_key_id = Some("alias/banlieue".to_string());
        req.iops = Some(4000);
        req.throughput = Some(250);
        let p = req.to_params().expect("valid");
        assert!(has(
            &p,
            "BlockDeviceMapping.1.Ebs.KmsKeyId",
            "alias/banlieue"
        ));
        assert!(has(&p, "BlockDeviceMapping.1.Ebs.Iops", "4000"));
        assert!(has(&p, "BlockDeviceMapping.1.Ebs.Throughput", "250"));
    }

    #[test]
    fn throughput_is_refused_on_a_volume_type_other_than_gp3() {
        let mut req = request();
        req.volume_type = VolumeType::Gp2;
        req.throughput = Some(250);
        assert!(matches!(req.to_params(), Err(Error::Invalid(_))));
    }

    #[test]
    fn provisioned_iops_types_require_iops() {
        for volume_type in [VolumeType::Io1, VolumeType::Io2] {
            let mut req = request();
            req.volume_type = volume_type;
            assert!(matches!(req.to_params(), Err(Error::Invalid(_))));
            req.iops = Some(3000);
            let p = req.to_params().expect("valid");
            assert!(has(
                &p,
                "BlockDeviceMapping.1.Ebs.VolumeType",
                volume_type.as_str()
            ));
        }
    }

    #[test]
    fn tag_specifications_are_instance_volume_interface_in_order() {
        let p = request().to_params().expect("valid");
        assert!(has(&p, "TagSpecification.1.ResourceType", "instance"));
        assert!(has(&p, "TagSpecification.1.Tag.1.Key", "Name"));
        assert!(has(&p, "TagSpecification.1.Tag.1.Value", "vm-a"));
        assert!(has(
            &p,
            "TagSpecification.1.Tag.2.Key",
            "banlieue-owner-uid"
        ));
        assert!(has(&p, "TagSpecification.1.Tag.2.Value", "u-1"));
        assert!(has(&p, "TagSpecification.2.ResourceType", "volume"));
        assert!(has(
            &p,
            "TagSpecification.2.Tag.1.Key",
            "banlieue-owner-uid"
        ));
        assert!(has(
            &p,
            "TagSpecification.3.ResourceType",
            "network-interface"
        ));
        assert!(has(&p, "TagSpecification.3.Tag.1.Value", "u-1"));
        assert!(get(&p, "TagSpecification.4.ResourceType").is_none());
    }

    #[test]
    fn an_empty_tag_set_is_skipped_and_the_numbering_stays_contiguous() {
        let mut req = request();
        req.volume_tags.clear();
        let p = req.to_params().expect("valid");
        assert!(has(&p, "TagSpecification.1.ResourceType", "instance"));
        assert!(has(
            &p,
            "TagSpecification.2.ResourceType",
            "network-interface"
        ));
        assert!(get(&p, "TagSpecification.3.ResourceType").is_none());
    }

    #[test]
    fn empty_required_fields_are_refused() {
        type Blank = fn(&mut RunInstancesRequest);
        let blanks: [(&str, Blank); 7] = [
            ("image", |r| r.image_id.clear()),
            ("type", |r| r.instance_type.clear()),
            ("zone", |r| r.zone_name.clear()),
            ("subnet", |r| r.subnet_id.clear()),
            ("security groups", |r| r.security_group_ids.clear()),
            ("root device", |r| r.root_device_name.clear()),
            ("client token", |r| r.client_token.clear()),
        ];
        for (what, blank) in blanks {
            let mut req = request();
            blank(&mut req);
            assert!(
                matches!(req.to_params(), Err(Error::Invalid(_))),
                "{what} should be required"
            );
        }
    }

    #[test]
    fn an_empty_security_group_id_is_refused() {
        let mut req = request();
        req.security_group_ids.push(String::new());
        assert!(matches!(req.to_params(), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_root_volume_of_zero_is_refused() {
        let mut req = request();
        req.root_volume_size_gib = 0;
        assert!(matches!(req.to_params(), Err(Error::Invalid(_))));
    }

    #[test]
    fn client_token_over_64_characters_is_refused() {
        let mut req = request();
        req.client_token = "a".repeat(MAX_CLIENT_TOKEN_LEN);
        assert!(req.to_params().is_ok());
        req.client_token = "a".repeat(MAX_CLIENT_TOKEN_LEN + 1);
        assert!(matches!(req.to_params(), Err(Error::Invalid(_))));
    }

    #[test]
    fn non_ascii_client_token_is_refused() {
        let mut req = request();
        req.client_token = "banlieue-\u{e9}".to_string();
        assert!(matches!(req.to_params(), Err(Error::Invalid(_))));
    }

    #[test]
    fn to_request_names_the_action() {
        let call = request().to_request().expect("valid");
        assert_eq!(call.action, "RunInstances");
        assert_eq!(call.params, request().to_params().expect("valid"));
    }

    // ----- client token --------------------------------------------------

    #[test]
    fn client_token_is_readable_for_a_kubernetes_uid_and_a_zone() {
        let uid = "6f1c2a9e-0b8d-4c3e-9a51-2f7d8e4b1c0a";
        assert_eq!(
            client_token_for(uid, "ca-central-1a"),
            format!("banlieue-{uid}-ca-central-1a")
        );
    }

    #[test]
    fn client_token_is_deterministic_per_uid_and_zone() {
        assert_eq!(client_token_for("a", "z1"), client_token_for("a", "z1"));
        assert_ne!(client_token_for("a", "z1"), client_token_for("b", "z1"));
    }

    #[test]
    fn a_re_placement_in_another_zone_gets_another_token() {
        // ADR-0173: the same machine re-placed in another zone is a
        // different RunInstances; reusing the token would get
        // IdempotentParameterMismatch from EC2.
        let uid = "6f1c2a9e-0b8d-4c3e-9a51-2f7d8e4b1c0a";
        assert_ne!(
            client_token_for(uid, "ca-central-1a"),
            client_token_for(uid, "ca-central-1b")
        );
    }

    #[test]
    fn uid_and_zone_cannot_be_confused_across_the_separator() {
        assert_ne!(client_token_for("a-b", "c"), client_token_for("a", "b-c"));
    }

    #[test]
    fn client_token_fits_for_any_uid_and_zone() {
        let long = "x".repeat(200);
        let odd = "uid with spaces/\u{e9}";
        let uid = "6f1c2a9e-0b8d-4c3e-9a51-2f7d8e4b1c0a";
        for (u, z) in [
            (long.as_str(), "ca-central-1a"),
            (odd, "ca-central-1a"),
            ("", ""),
            (uid, "us-west-2-lax-1a-plus-a-long-local-zone"),
        ] {
            let token = client_token_for(u, z);
            assert!(token.len() <= MAX_CLIENT_TOKEN_LEN, "{token}");
            assert!(token.is_ascii(), "{token}");
            assert!(token.starts_with("banlieue-"));
        }
        assert_ne!(
            client_token_for(&long, "z"),
            client_token_for(&"x".repeat(201), "z")
        );
    }

    // ----- other actions -------------------------------------------------

    fn pairs(call: &Ec2Request) -> Vec<(&str, &str)> {
        call.params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    #[test]
    fn terminate_stop_start_name_one_instance() {
        let cases = [
            (
                terminate_instances("i-1").expect("ok"),
                "TerminateInstances",
            ),
            (stop_instances("i-1").expect("ok"), "StopInstances"),
            (start_instances("i-1").expect("ok"), "StartInstances"),
        ];
        for (call, action) in cases {
            assert_eq!(call.action, action);
            assert_eq!(pairs(&call), [("InstanceId.1", "i-1")]);
        }
    }

    #[test]
    fn an_empty_instance_id_is_refused_by_every_builder() {
        assert!(terminate_instances("").is_err());
        assert!(stop_instances("").is_err());
        assert!(start_instances("").is_err());
        assert!(describe_instance("").is_err());
        assert!(get_console_output("").is_err());
        assert!(get_instance_tpm_ek_pub("", EkKeyType::Rsa2048).is_err());
        assert!(disable_instance_metadata("").is_err());
    }

    #[test]
    fn describe_instance_by_id() {
        let call = describe_instance("i-1").expect("ok");
        assert_eq!(call.action, "DescribeInstances");
        assert_eq!(pairs(&call), [("InstanceId.1", "i-1")]);
    }

    #[test]
    fn describe_instances_by_tag_filters_on_the_key_and_value() {
        let call = describe_instances_by_tag("banlieue-owner-uid", "u-1").expect("ok");
        assert_eq!(call.action, "DescribeInstances");
        assert_eq!(
            pairs(&call),
            [
                ("Filter.1.Name", "tag:banlieue-owner-uid"),
                ("Filter.1.Value.1", "u-1")
            ]
        );
        assert!(describe_instances_by_tag("", "u-1").is_err());
    }

    #[test]
    fn console_output_asks_for_the_latest() {
        let call = get_console_output("i-1").expect("ok");
        assert_eq!(call.action, "GetConsoleOutput");
        assert_eq!(pairs(&call), [("InstanceId", "i-1"), ("Latest", "true")]);
    }

    #[test]
    fn tpm_ek_pub_asks_for_der_of_the_given_type() {
        for (key_type, wire) in [
            (EkKeyType::Rsa2048, "rsa-2048"),
            (EkKeyType::EccSecP384, "ecc-sec-p384"),
        ] {
            let call = get_instance_tpm_ek_pub("i-1", key_type).expect("ok");
            assert_eq!(call.action, "GetInstanceTpmEkPub");
            assert_eq!(
                pairs(&call),
                [
                    ("InstanceId", "i-1"),
                    ("KeyType", wire),
                    ("KeyFormat", "der")
                ]
            );
            assert_eq!(EkKeyType::parse(wire), Some(key_type));
        }
        assert_eq!(EkKeyType::parse("rsa-4096"), None);
    }

    #[test]
    fn disabling_metadata_sets_only_the_endpoint() {
        let call = disable_instance_metadata("i-1").expect("ok");
        assert_eq!(call.action, "ModifyInstanceMetadataOptions");
        assert_eq!(
            pairs(&call),
            [("InstanceId", "i-1"), ("HttpEndpoint", "disabled")]
        );
    }

    #[test]
    fn describe_availability_zones_takes_no_parameters() {
        let call = describe_availability_zones();
        assert_eq!(call.action, "DescribeAvailabilityZones");
        assert!(call.params.is_empty());
    }

    #[test]
    fn offerings_are_by_zone_id_filtered_by_type() {
        let call = describe_instance_type_offerings(&["m6i.large", "t3.micro"]).expect("ok");
        assert_eq!(call.action, "DescribeInstanceTypeOfferings");
        assert_eq!(
            pairs(&call),
            [
                ("LocationType", "availability-zone-id"),
                ("Filter.1.Name", "instance-type"),
                ("Filter.1.Value.1", "m6i.large"),
                ("Filter.1.Value.2", "t3.micro"),
            ]
        );
        let none: [&str; 0] = [];
        assert!(describe_instance_type_offerings(&none).is_err());
        assert!(describe_instance_type_offerings(&[""]).is_err());
    }

    #[test]
    fn instance_types_are_listed_one_based() {
        let call = describe_instance_types(&["m6i.large".to_string(), "t3.micro".to_string()])
            .expect("ok");
        assert_eq!(call.action, "DescribeInstanceTypes");
        assert_eq!(
            pairs(&call),
            [
                ("InstanceType.1", "m6i.large"),
                ("InstanceType.2", "t3.micro")
            ]
        );
    }

    #[test]
    fn instance_types_are_refused_when_empty_or_over_the_page_limit() {
        let none: [&str; 0] = [];
        assert!(describe_instance_types(&none).is_err());
        let many = vec!["t3.micro"; MAX_DESCRIBE_INSTANCE_TYPES + 1];
        assert!(describe_instance_types(&many).is_err());
        let full = vec!["t3.micro"; MAX_DESCRIBE_INSTANCE_TYPES];
        assert!(describe_instance_types(&full).is_ok());
    }

    // ----- DescribeImages ------------------------------------------------

    #[test]
    fn describe_image_names_one_image() {
        let r = describe_image("ami-0123456789abcdef0").expect("builds");
        assert_eq!(r.action, ACTION_DESCRIBE_IMAGES);
        assert_eq!(
            r.params,
            vec![("ImageId.1".to_string(), "ami-0123456789abcdef0".to_string())]
        );
    }

    #[test]
    fn describe_image_refuses_what_is_not_an_ami_id() {
        for bad in [
            "",
            "ami-",
            "ami-xyz12345",
            "ami-0123456",
            "i-0123456789abcdef0",
            " ami-0123abcd",
        ] {
            assert!(
                matches!(describe_image(bad), Err(Error::Invalid(_))),
                "{bad:?}"
            );
        }
        describe_image("ami-0123abcd").expect("8 hex digits");
    }
}
