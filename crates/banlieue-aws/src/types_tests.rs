// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `types.rs`, against AWS's published sample responses (see
//! `tests/fixtures/ec2/README.md`).

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::error::Error;
    use crate::instance::EkKeyType;

    const RUN: &str = include_str!("../tests/fixtures/ec2/run-instances.xml");
    const DESCRIBE: &str = include_str!("../tests/fixtures/ec2/describe-instances.xml");
    const TERMINATE: &str = include_str!("../tests/fixtures/ec2/terminate-instances.xml");
    const STOP: &str = include_str!("../tests/fixtures/ec2/stop-instances.xml");
    const START: &str = include_str!("../tests/fixtures/ec2/start-instances.xml");
    const CONSOLE: &str = include_str!("../tests/fixtures/ec2/get-console-output.xml");
    const EK_RSA: &str = include_str!("../tests/fixtures/ec2/get-instance-tpm-ek-pub-rsa.xml");
    const EK_ECC: &str = include_str!("../tests/fixtures/ec2/get-instance-tpm-ek-pub-ecc.xml");
    const EK_RSA_PEM: &str = include_str!("../tests/fixtures/ec2/ek-rsa-2048.pem");
    const EK_ECC_PEM: &str = include_str!("../tests/fixtures/ec2/ek-ecc-p384.pem");
    const ZONES: &str = include_str!("../tests/fixtures/ec2/describe-availability-zones.xml");
    const OFFERINGS: &str =
        include_str!("../tests/fixtures/ec2/describe-instance-type-offerings.xml");
    const TYPES: &str = include_str!("../tests/fixtures/ec2/describe-instance-types.xml");
    const ERROR_EC2: &str = include_str!("../tests/fixtures/ec2/error-ec2.xml");
    const ERROR_STS: &str = include_str!("../tests/fixtures/ec2/error-sts.xml");
    const ERROR_S3: &str = include_str!("../tests/fixtures/ec2/error-s3.xml");

    // ----- instances -----------------------------------------------------

    #[test]
    fn run_instances_yields_the_instance_id() {
        assert_eq!(
            parse_run_instances(RUN).expect("parses"),
            "i-05e4d39f17ac88e74"
        );
    }

    #[test]
    fn run_instances_without_an_instance_is_a_decode_error() {
        let body = r#"<RunInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
            <requestId>r</requestId><instancesSet/></RunInstancesResponse>"#;
        assert!(matches!(parse_run_instances(body), Err(Error::Decode(_))));
    }

    #[test]
    fn describe_instances_reads_the_facts_the_provider_needs() {
        let instances = parse_describe_instances(DESCRIBE).expect("parses");
        assert_eq!(instances.len(), 1);
        let i = &instances[0];
        assert_eq!(i.instance_id, "i-1234567890abcdef0");
        assert_eq!(i.state, InstanceState::Running);
        assert_eq!(i.private_ip.as_deref(), Some("192.168.1.88"));
        assert_eq!(
            i.private_dns_name.as_deref(),
            Some("ip-192-168-1-88.eu-west-1.compute.internal")
        );
        assert_eq!(i.zone_name, "eu-west-1c");
        assert_eq!(
            i.launch_time,
            Some("2018-05-08T16:46:19Z".parse().expect("timestamp"))
        );
        assert_eq!(i.tags.get("Name").map(String::as_str), Some("Server_1"));
        assert_eq!(i.tags.len(), 1);
    }

    #[test]
    fn describe_instances_flattens_reservations_and_tolerates_missing_fields() {
        let body = r#"<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
            <requestId>r</requestId>
            <reservationSet>
              <item><instancesSet>
                <item><instanceId>i-a</instanceId><instanceState><code>0</code><name>pending</name></instanceState>
                  <placement><availabilityZone>ca-central-1a</availabilityZone></placement>
                  <somethingNew><item>x</item></somethingNew></item>
              </instancesSet></item>
              <item><instancesSet>
                <item><instanceId>i-b</instanceId><instanceState><name>stopped</name></instanceState>
                  <tagSet><item><key>empty</key><value/></item></tagSet></item>
              </instancesSet></item>
            </reservationSet>
            <nextToken/>
          </DescribeInstancesResponse>"#;
        let instances = parse_describe_instances(body).expect("parses");
        let ids: Vec<&str> = instances.iter().map(|i| i.instance_id.as_str()).collect();
        assert_eq!(ids, ["i-a", "i-b"]);
        assert_eq!(instances[0].state, InstanceState::Pending);
        assert!(instances[0].private_ip.is_none());
        assert!(instances[0].launch_time.is_none());
        assert_eq!(instances[1].state, InstanceState::Stopped);
        assert_eq!(instances[1].zone_name, "");
        assert_eq!(instances[1].tags.get("empty").map(String::as_str), Some(""));
    }

    #[test]
    fn describe_instances_with_no_reservations_is_empty() {
        let body = r#"<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
            <requestId>r</requestId><reservationSet/></DescribeInstancesResponse>"#;
        assert!(parse_describe_instances(body).expect("parses").is_empty());
    }

    #[test]
    fn a_bad_launch_time_is_a_decode_error() {
        let body = r#"<DescribeInstancesResponse><reservationSet><item><instancesSet><item>
            <instanceId>i-a</instanceId><launchTime>yesterday</launchTime>
            </item></instancesSet></item></reservationSet></DescribeInstancesResponse>"#;
        assert!(matches!(
            parse_describe_instances(body),
            Err(Error::Decode(_))
        ));
    }

    #[test]
    fn malformed_xml_is_a_decode_error() {
        assert!(matches!(
            parse_describe_instances("<DescribeInstancesResponse><reservationSet>"),
            Err(Error::Decode(_))
        ));
    }

    #[test]
    fn state_changes_report_the_current_state() {
        let cases = [
            (
                TERMINATE,
                InstanceState::ShuttingDown,
                InstanceState::Running,
            ),
            (STOP, InstanceState::Stopping, InstanceState::Running),
            (START, InstanceState::Pending, InstanceState::Stopped),
        ];
        for (body, current, previous) in cases {
            let changes = parse_instance_state_changes(body).expect("parses");
            assert_eq!(
                changes,
                [InstanceStateChange {
                    instance_id: "i-1234567890abcdef0".to_string(),
                    current,
                    previous,
                }]
            );
        }
    }

    #[test]
    fn instance_states_round_trip_and_keep_unknown_names() {
        for name in [
            "pending",
            "running",
            "shutting-down",
            "terminated",
            "stopping",
            "stopped",
        ] {
            assert_eq!(InstanceState::parse(name).as_str(), name);
        }
        assert_eq!(
            InstanceState::parse("hibernating"),
            InstanceState::Other("hibernating".to_string())
        );
        assert!(InstanceState::Terminated.is_gone());
        assert!(InstanceState::ShuttingDown.is_gone());
        assert!(!InstanceState::Stopped.is_gone());
    }

    // ----- console -------------------------------------------------------

    #[test]
    fn console_output_is_decoded_across_wrapped_lines() {
        let text = parse_console_output(CONSOLE).expect("parses");
        assert!(text.starts_with("Linux version 2.6.16-xenU"), "{text}");
        assert!(text.contains("Kernel command line: root=/dev/sda1 ro 4"));
        assert!(text.ends_with("Enabling fast FPU save and restore... done.\n"));
    }

    #[test]
    fn absent_console_output_is_empty() {
        let body = r#"<GetConsoleOutputResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
            <requestId>r</requestId><instanceId>i-a</instanceId>
            <timestamp>2026-11-25T01:12:41.000Z</timestamp></GetConsoleOutputResponse>"#;
        assert_eq!(parse_console_output(body).expect("parses"), "");
        let empty = r#"<GetConsoleOutputResponse><instanceId>i-a</instanceId><output/></GetConsoleOutputResponse>"#;
        assert_eq!(parse_console_output(empty).expect("parses"), "");
    }

    #[test]
    fn console_output_that_is_not_base64_is_a_decode_error() {
        let body =
            r#"<GetConsoleOutputResponse><output>not*base64</output></GetConsoleOutputResponse>"#;
        assert!(matches!(parse_console_output(body), Err(Error::Decode(_))));
    }

    // ----- NitroTPM EK ---------------------------------------------------

    #[test]
    fn tpm_ek_pub_yields_der_and_its_pem_spki() {
        for (body, key_type, pem) in [
            (EK_RSA, EkKeyType::Rsa2048, EK_RSA_PEM),
            (EK_ECC, EkKeyType::EccSecP384, EK_ECC_PEM),
        ] {
            let ek = parse_tpm_ek_pub(body).expect("parses");
            assert_eq!(ek.instance_id, "i-1234567890abcdef0");
            assert_eq!(ek.key_type, key_type);
            assert_eq!(ek.key_format, "der");
            // A DER SubjectPublicKeyInfo is a SEQUENCE.
            assert_eq!(ek.key_value.first(), Some(&0x30));
            assert_eq!(to_pem_spki(&ek.key_value), pem);
        }
    }

    #[test]
    fn pem_lines_are_64_characters() {
        let pem = to_pem_spki(&[0xAB; 200]);
        let body: Vec<&str> = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        assert!(body.iter().rev().skip(1).all(|l| l.len() == 64));
        assert!(body.last().is_some_and(|l| l.len() <= 64));
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
        assert!(pem.ends_with("-----END PUBLIC KEY-----\n"));
    }

    #[test]
    fn an_undocumented_ek_key_type_is_a_decode_error() {
        let body = r#"<GetInstanceTpmEkPubResponse><instanceId>i-a</instanceId>
            <keyType>rsa-4096</keyType><keyFormat>der</keyFormat><keyValue>MA==</keyValue>
            </GetInstanceTpmEkPubResponse>"#;
        assert!(matches!(parse_tpm_ek_pub(body), Err(Error::Decode(_))));
    }

    #[test]
    fn an_empty_ek_is_a_decode_error() {
        let body = r#"<GetInstanceTpmEkPubResponse><instanceId>i-a</instanceId>
            <keyType>rsa-2048</keyType><keyFormat>der</keyFormat><keyValue/>
            </GetInstanceTpmEkPubResponse>"#;
        assert!(matches!(parse_tpm_ek_pub(body), Err(Error::Decode(_))));
    }

    // ----- zones and types -----------------------------------------------

    #[test]
    fn zones_pair_name_with_id() {
        let zones = parse_availability_zones(ZONES).expect("parses");
        assert_eq!(zones.len(), 4);
        assert_eq!(
            zones[0],
            AvailabilityZone {
                name: "us-west-2a".to_string(),
                id: "usw2-az1".to_string(),
                state: "available".to_string(),
            }
        );
        assert_eq!(zones[3].id, "usw2-az4");
    }

    #[test]
    fn offerings_pair_type_with_location() {
        let offerings = parse_instance_type_offerings(OFFERINGS).expect("parses");
        assert_eq!(
            offerings,
            [
                InstanceTypeOffering {
                    instance_type: "m6i.large".to_string(),
                    location: "usw2-az1".to_string(),
                },
                InstanceTypeOffering {
                    instance_type: "m6i.large".to_string(),
                    location: "usw2-az2".to_string(),
                },
                InstanceTypeOffering {
                    instance_type: "t3.micro".to_string(),
                    location: "usw2-az1".to_string(),
                },
            ]
        );
    }

    #[test]
    fn instance_types_read_vcpus_architectures_tpm_and_boot_modes() {
        let types = parse_instance_types(TYPES).expect("parses");
        assert_eq!(types.len(), 2);
        let m6i = &types[0];
        assert_eq!(m6i.instance_type, "m6i.large");
        assert_eq!(m6i.default_vcpus, 2);
        assert_eq!(m6i.architectures, ["x86_64"]);
        assert!(m6i.nitro_tpm_supported);
        assert_eq!(m6i.nitro_tpm_versions, ["2.0"]);
        assert_eq!(m6i.boot_modes, ["legacy-bios", "uefi"]);
        assert!(m6i.supports_uefi());

        let t2 = &types[1];
        assert_eq!(t2.default_vcpus, 1);
        assert_eq!(t2.architectures, ["i386", "x86_64"]);
        assert!(!t2.nitro_tpm_supported);
        assert!(t2.nitro_tpm_versions.is_empty());
        assert!(!t2.supports_uefi());
    }

    // ----- errors --------------------------------------------------------

    #[test]
    fn ec2_error_body_is_classified_with_its_request_id() {
        let err = parse_error(400, ERROR_EC2);
        assert!(err.is_not_found(), "{err}");
        let fault = err.fault().expect("fault");
        assert_eq!(fault.status, 400);
        assert_eq!(fault.code, "InvalidInstanceID.NotFound");
        assert_eq!(fault.message, "The instance ID 'i-1a2b3c4d' does not exist");
        assert_eq!(fault.request_id, "ea966190-f9aa-478e-9ede-example");
    }

    #[test]
    fn sts_error_body_is_classified() {
        let err = parse_error(400, ERROR_STS);
        assert!(matches!(err, Error::Unauthenticated(_)), "{err}");
        let fault = err.fault().expect("fault");
        assert_eq!(fault.code, "InvalidIdentityToken");
        assert_eq!(fault.message, "Incorrect token audience");
        assert_eq!(fault.request_id, "c6104cbe-af31-11e0-8154-cbc7ccf896c7");
    }

    #[test]
    fn s3_error_body_is_classified() {
        let err = parse_error(404, ERROR_S3);
        assert!(err.is_not_found(), "{err}");
        let fault = err.fault().expect("fault");
        assert_eq!(fault.code, "NoSuchKey");
        assert_eq!(fault.request_id, "4442587FB7D0A2F9");
    }

    #[test]
    fn ec2_throttling_is_throttled() {
        let body = "<Response><Errors><Error><Code>RequestLimitExceeded</Code>\
            <Message>Request limit exceeded.</Message></Error></Errors>\
            <RequestID>r-1</RequestID></Response>";
        assert!(parse_error(503, body).is_throttled());
    }

    #[test]
    fn a_body_that_is_not_xml_is_classified_by_status() {
        for (status, body) in [
            (503, "Service Unavailable"),
            (500, ""),
            (403, "<html><body>Forbidden"),
        ] {
            let err = parse_error(status, body);
            let fault = err.fault().expect("fault");
            assert_eq!(fault.code, "", "{body}");
            assert_eq!(fault.status, status);
        }
        assert!(parse_error(503, "Service Unavailable").is_throttled());
        assert!(matches!(parse_error(500, ""), Error::Server(_)));
        assert!(matches!(
            parse_error(403, "<html><body>Forbidden"),
            Error::InvalidRequest(_)
        ));
    }

    #[test]
    fn a_long_error_body_is_cut() {
        let body = "x".repeat(crate::error::MAX_MESSAGE_LEN * 2);
        let err = parse_error(500, &body);
        assert_eq!(
            err.fault().expect("fault").message.len(),
            crate::error::MAX_MESSAGE_LEN
        );
    }
}
