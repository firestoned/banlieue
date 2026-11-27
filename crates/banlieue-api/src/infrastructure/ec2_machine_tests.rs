// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `ec2_machine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use kube::CustomResourceExt;

    const AMI: &str = "ami-0123456789abcdef0";
    const SUBNET: &str = "subnet-0123456789abcdef0";
    const SG: &str = "sg-0123456789abcdef0";
    const INSTANCE: &str = "i-0123456789abcdef0";

    fn minimal_spec() -> EC2MachineSpec {
        EC2MachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: ProviderReference {
                name: "ec2-cc".to_string(),
                namespace: None,
            },
            zone: "ca-central-1a".to_string(),
            instance_type: "m7i.large".to_string(),
            image_id: AMI.to_string(),
            root_device_name: DEFAULT_ROOT_DEVICE_NAME.to_string(),
            root_volume_gi_b: 10,
            volume_type: Ec2VolumeType::Gp3,
            iops: None,
            throughput: None,
            kms_key_id: None,
            subnet_id: SUBNET.to_string(),
            security_group_ids: vec![SG.to_string()],
            tpm_enabled: false,
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn refused(spec: &EC2MachineSpec, why: &str) {
        let err = spec.validate().expect_err("refused");
        assert!(err.contains(why), "expected {why:?} in {err:?}");
    }

    // ---- providerID ----------------------------------------------------

    #[test]
    fn provider_id_is_the_aws_cloud_provider_form() {
        assert_eq!(
            ec2_provider_id("ca-central-1a", INSTANCE),
            "aws:///ca-central-1a/i-0123456789abcdef0"
        );
    }

    #[test]
    fn provider_id_round_trips_through_the_parser() {
        let id = ec2_provider_id("us-west-2-lax-1a", INSTANCE);
        assert_eq!(
            parse_ec2_provider_id(&id),
            Some(("us-west-2-lax-1a", INSTANCE))
        );
    }

    #[test]
    fn provider_id_parser_refuses_other_shapes() {
        for bad in [
            "",
            "aws:///",
            "aws:///ca-central-1a",
            "aws:///ca-central-1a/",
            "aws:////i-0123456789abcdef0",
            "aws://ca-central-1a/i-0123456789abcdef0",
            "azure:///ca-central-1a/i-0123456789abcdef0",
            "aws:///ca-central-1a/ami-0123456789abcdef0",
            "aws:///ca-central-1a/i-0123456789abcdef0/extra",
            "aws:///ca/central/i-0123456789abcdef0",
        ] {
            assert_eq!(parse_ec2_provider_id(bad), None, "{bad}");
        }
    }

    // ---- validate ------------------------------------------------------

    #[test]
    fn minimal_spec_is_valid() {
        assert_eq!(minimal_spec().validate(), Ok(()));
    }

    #[test]
    fn user_data_at_the_limit_is_valid_and_one_byte_over_is_refused() {
        let mut spec = minimal_spec();
        spec.user_data = Some("x".repeat(MAX_USER_DATA_BYTES));
        assert_eq!(spec.validate(), Ok(()));
        spec.user_data = Some("x".repeat(MAX_USER_DATA_BYTES + 1));
        refused(&spec, "16384");
    }

    /// The limit is in bytes before encoding: a multi-byte character counts
    /// for each of its bytes.
    #[test]
    fn user_data_is_measured_in_bytes() {
        let mut spec = minimal_spec();
        spec.user_data = Some("\u{e9}".repeat(MAX_USER_DATA_BYTES / 2 + 1));
        refused(&spec, "userData");
    }

    #[test]
    fn suspended_is_refused_and_powered_off_is_valid() {
        let mut spec = minimal_spec();
        spec.desired_power_state = PowerState::Suspended;
        refused(&spec, "Suspended");
        spec.desired_power_state = PowerState::PoweredOff;
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn identifiers_of_the_wrong_shape_are_refused() {
        let mut spec = minimal_spec();
        spec.image_id = "ami-xyz".to_string();
        refused(&spec, "imageId");

        let mut spec = minimal_spec();
        spec.subnet_id = "subnet-xyz".to_string();
        refused(&spec, "subnetId");

        let mut spec = minimal_spec();
        spec.security_group_ids = vec![SG.to_string(), "default".to_string()];
        refused(&spec, "securityGroupIds");

        let mut spec = minimal_spec();
        spec.kms_key_id = Some("not a key".to_string());
        refused(&spec, "kmsKeyId");
    }

    #[test]
    fn at_least_one_security_group_is_required() {
        let mut spec = minimal_spec();
        spec.security_group_ids.clear();
        refused(&spec, "securityGroupIds");
    }

    #[test]
    fn instance_type_and_zone_must_be_set() {
        let mut spec = minimal_spec();
        spec.instance_type = String::new();
        refused(&spec, "instanceType");
        let mut spec = minimal_spec();
        spec.instance_type = "m7i".to_string();
        refused(&spec, "instanceType");
        let mut spec = minimal_spec();
        spec.zone = String::new();
        refused(&spec, "zone");
        let mut spec = minimal_spec();
        spec.zone = "ca-central-1a/x".to_string();
        refused(&spec, "zone");
    }

    #[test]
    fn root_volume_must_be_at_least_one_gib_and_within_ebs() {
        let mut spec = minimal_spec();
        spec.root_volume_gi_b = 0;
        refused(&spec, "rootVolumeGiB");
        spec.root_volume_gi_b = MAX_ROOT_VOLUME_GIB;
        assert_eq!(spec.validate(), Ok(()));
        spec.root_volume_gi_b = MAX_ROOT_VOLUME_GIB + 1;
        refused(&spec, "rootVolumeGiB");
    }

    #[test]
    fn root_device_name_must_be_a_device_path() {
        let mut spec = minimal_spec();
        spec.root_device_name = "/dev/sda1".to_string();
        assert_eq!(spec.validate(), Ok(()));
        for bad in ["xvda", "/dev/", "/dev/../xvda", "/dev/xvda "] {
            spec.root_device_name = bad.to_string();
            refused(&spec, "rootDeviceName");
        }
    }

    #[test]
    fn throughput_only_with_gp3() {
        let mut spec = minimal_spec();
        spec.throughput = Some(250);
        assert_eq!(spec.validate(), Ok(()));
        for other in [Ec2VolumeType::Gp2, Ec2VolumeType::Io1, Ec2VolumeType::Io2] {
            spec.volume_type = other;
            spec.iops = Some(3000);
            refused(&spec, "throughput");
        }
    }

    #[test]
    fn iops_is_required_for_provisioned_iops_volumes() {
        for io in [Ec2VolumeType::Io1, Ec2VolumeType::Io2] {
            let mut spec = minimal_spec();
            spec.volume_type = io;
            refused(&spec, "iops");
            spec.iops = Some(3000);
            assert_eq!(spec.validate(), Ok(()));
        }
    }

    #[test]
    fn iops_is_optional_on_gp3_and_refused_on_gp2() {
        let mut spec = minimal_spec();
        spec.iops = Some(6000);
        assert_eq!(spec.validate(), Ok(()));
        spec.volume_type = Ec2VolumeType::Gp2;
        refused(&spec, "iops");
    }

    #[test]
    fn zero_iops_or_throughput_is_refused() {
        let mut spec = minimal_spec();
        spec.iops = Some(0);
        refused(&spec, "iops");
        let mut spec = minimal_spec();
        spec.throughput = Some(0);
        refused(&spec, "throughput");
    }

    // ---- serde ---------------------------------------------------------

    #[test]
    fn spec_serializes_in_camel_case_and_omits_unset_fields() {
        let value = serde_json::to_value(minimal_spec()).unwrap();
        assert_eq!(value["rootVolumeGiB"], 10);
        assert_eq!(value["imageId"], AMI);
        assert_eq!(value["instanceType"], "m7i.large");
        assert_eq!(value["volumeType"], "gp3");
        assert_eq!(value["securityGroupIds"][0], SG);
        assert_eq!(value["rootDeviceName"], "/dev/xvda");
        for absent in [
            "providerID",
            "failureDomain",
            "iops",
            "throughput",
            "kmsKeyId",
            "tpmEnabled",
            "userData",
        ] {
            assert!(value.get(absent).is_none(), "{absent} in {value}");
        }
    }

    #[test]
    fn defaults_fill_in_the_root_device_and_volume_type() {
        let spec: EC2MachineSpec = serde_json::from_value(serde_json::json!({
            "providerRef": { "name": "ec2-cc" },
            "zone": "ca-central-1a",
            "instanceType": "m7i.large",
            "imageId": AMI,
            "rootVolumeGiB": 10,
            "subnetId": SUBNET,
            "securityGroupIds": [SG],
        }))
        .unwrap();
        assert_eq!(spec.root_device_name, DEFAULT_ROOT_DEVICE_NAME);
        assert_eq!(spec.volume_type, Ec2VolumeType::Gp3);
        assert_eq!(spec.desired_power_state, PowerState::PoweredOn);
        assert!(!spec.tpm_enabled);
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn volume_types_use_the_ec2_spelling() {
        for (t, s) in [
            (Ec2VolumeType::Gp3, "gp3"),
            (Ec2VolumeType::Gp2, "gp2"),
            (Ec2VolumeType::Io1, "io1"),
            (Ec2VolumeType::Io2, "io2"),
        ] {
            assert_eq!(serde_json::to_value(t).unwrap(), s);
            assert_eq!(t.as_str(), s);
        }
    }

    #[test]
    fn status_round_trips_and_omits_unset_fields() {
        let empty = serde_json::to_value(EC2MachineStatus::default()).unwrap();
        for absent in [
            "instanceId",
            "zoneId",
            "instanceState",
            "guestInstalled",
            "imdsDisabled",
            "tpmEndorsementPublicKeys",
            "createFailures",
            "observedPowerState",
            "addresses",
            "conditions",
        ] {
            assert!(empty.get(absent).is_none(), "{absent} in {empty}");
        }

        let status = EC2MachineStatus {
            instance_id: Some(INSTANCE.to_string()),
            zone_id: Some("cac1-az1".to_string()),
            instance_state: Some("running".to_string()),
            guest_installed: true,
            imds_disabled: true,
            tpm_endorsement_public_keys: vec![
                "-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----\n".to_string(),
            ],
            create_failures: 2,
            addresses: vec![
                MachineAddress {
                    address_type: MachineAddressType::InternalIP,
                    address: "10.0.1.15".to_string(),
                },
                MachineAddress {
                    address_type: MachineAddressType::InternalDNS,
                    address: "ip-10-0-1-15.ca-central-1.compute.internal".to_string(),
                },
            ],
            observed_power_state: Some(PowerState::PoweredOn),
            ..Default::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["zoneId"], "cac1-az1");
        assert_eq!(value["imdsDisabled"], true);
        assert_eq!(value["addresses"][1]["type"], "InternalDNS");
        assert!(
            value["tpmEndorsementPublicKeys"][0]
                .as_str()
                .unwrap()
                .starts_with("-----BEGIN PUBLIC KEY-----")
        );
        let back: EC2MachineStatus = serde_json::from_value(value).unwrap();
        assert_eq!(back, status);
    }

    // ---- CRD -----------------------------------------------------------

    #[test]
    fn crd_names_do_not_collide_with_capa() {
        let crd = EC2Machine::crd();
        assert_eq!(crd.spec.names.kind, "EC2Machine");
        assert_eq!(crd.spec.names.plural, "ec2machines");
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert_eq!(crd.spec.names.short_names, Some(vec!["ec2m".to_string()]));
        let template = EC2MachineTemplate::crd();
        assert_eq!(template.spec.names.plural, "ec2machinetemplates");
        assert_eq!(
            template.spec.names.short_names,
            Some(vec!["ec2mt".to_string()])
        );
    }

    #[test]
    fn crd_prints_the_azure_columns() {
        let crd = EC2Machine::crd();
        let columns: Vec<String> = crd.spec.versions[0]
            .additional_printer_columns
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(
            columns,
            [
                "Provider",
                "Zone",
                "InstanceType",
                "Provisioned",
                "State",
                "Power",
                "ProviderID",
                "Age"
            ]
        );
    }

    fn spec_schema() -> serde_json::Value {
        let crd = EC2Machine::crd();
        let schema = crd.spec.versions[0].schema.clone().unwrap();
        let root = serde_json::to_value(schema.open_api_v3_schema.unwrap()).unwrap();
        root["properties"]["spec"].clone()
    }

    /// The shapes the provider checks are also enforced when the object is
    /// stored, from the same patterns.
    #[test]
    fn the_schema_carries_the_identifier_patterns_and_limits() {
        let spec = spec_schema();
        let p = &spec["properties"];
        assert_eq!(p["imageId"]["pattern"], crate::aws::AMI_ID_PATTERN);
        assert_eq!(p["subnetId"]["pattern"], crate::aws::SUBNET_ID_PATTERN);
        assert_eq!(
            p["securityGroupIds"]["items"]["pattern"],
            crate::aws::SECURITY_GROUP_ID_PATTERN
        );
        assert_eq!(p["securityGroupIds"]["minItems"], 1);
        assert_eq!(p["kmsKeyId"]["pattern"], crate::aws::KMS_KEY_PATTERN);
        assert_eq!(p["userData"]["maxLength"], MAX_USER_DATA_BYTES);
        assert_eq!(p["rootVolumeGiB"]["minimum"].as_f64(), Some(1.0));
        assert_eq!(
            p["rootVolumeGiB"]["maximum"].as_f64(),
            Some(f64::from(MAX_ROOT_VOLUME_GIB))
        );
        assert_eq!(
            p["volumeType"]["enum"],
            serde_json::json!(["gp3", "gp2", "io1", "io2"])
        );
        let required: Vec<&str> = spec["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for field in [
            "providerRef",
            "zone",
            "instanceType",
            "imageId",
            "rootVolumeGiB",
            "subnetId",
            "securityGroupIds",
        ] {
            assert!(required.contains(&field), "{field} not required");
        }
    }

    /// The cross-field rules are CEL on the spec, so the API server refuses
    /// them too.
    #[test]
    fn the_schema_carries_the_volume_rules() {
        let spec = spec_schema();
        let rules: Vec<String> = spec["x-kubernetes-validations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["rule"].as_str().unwrap().to_string())
            .collect();
        assert!(rules.iter().any(|r| r.contains("throughput")), "{rules:?}");
        assert!(rules.iter().any(|r| r.contains("'io1'")), "{rules:?}");
        assert!(rules.iter().any(|r| r.contains("'gp2'")), "{rules:?}");
    }
}
