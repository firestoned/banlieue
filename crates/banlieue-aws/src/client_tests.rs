// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`, against a one-shot local server answering
//! with AWS's published sample responses (`tests/fixtures/ec2/`).

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jiff::civil::DateTime;
    use jiff::tz::TimeZone;

    use super::super::*;
    use crate::auth::StaticSource;
    use crate::retry::{RetryPolicy, SUSTAINED_THROTTLE_COUNT};
    use crate::sigv4::{Credentials, X_AMZ_DATE};
    use crate::test_server::{Recorded, install_provider, ok, respond, serve};

    const REGION: &str = "us-east-1";
    const ACCESS_KEY: &str = "AKIDEXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const SESSION_TOKEN: &str = "session-token-EXAMPLE";
    const INSTANCE: &str = "i-1234567890abcdef0";
    const ZONES: &str = include_str!("../tests/fixtures/ec2/describe-availability-zones.xml");
    const DESCRIBE: &str = include_str!("../tests/fixtures/ec2/describe-instances.xml");
    const RUN: &str = include_str!("../tests/fixtures/ec2/run-instances.xml");
    const TERMINATE: &str = include_str!("../tests/fixtures/ec2/terminate-instances.xml");
    const STOP: &str = include_str!("../tests/fixtures/ec2/stop-instances.xml");
    const CONSOLE: &str = include_str!("../tests/fixtures/ec2/get-console-output.xml");
    const AMZ_DATE_FORMAT: &str = "%Y%m%dT%H%M%SZ";

    fn ec2_error(code: &str) -> String {
        format!(
            "<Response><Errors><Error><Code>{code}</Code><Message>m</Message></Error></Errors>\
             <RequestID>req-1</RequestID></Response>"
        )
    }

    fn credentials(session_token: Option<&str>) -> Credentials {
        Credentials::new(ACCESS_KEY, SECRET, session_token.map(ToString::to_string))
    }

    fn client_with(endpoint: &str, session_token: Option<&str>) -> Ec2Client {
        install_provider();
        Ec2Client::new(
            REGION,
            Arc::new(StaticSource::new(credentials(session_token))),
        )
        .expect("client")
        .with_endpoint(endpoint)
        .expect("endpoint")
    }

    fn client(endpoint: &str) -> Ec2Client {
        client_with(endpoint, Some(SESSION_TOKEN))
    }

    fn form_value(body: &str, key: &str) -> Option<String> {
        body.split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .map(ToString::to_string)
    }

    /// Re-signs what the server saw, at the time it carried, and returns
    /// the `Authorization` that request should have had.
    fn expected_authorization(seen: &Recorded, host: &str, session_token: Option<&str>) -> String {
        let date = seen.header(X_AMZ_DATE).expect("x-amz-date");
        let timestamp = DateTime::strptime(AMZ_DATE_FORMAT, &date)
            .unwrap()
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp();
        let headers = [(
            "content-type".to_string(),
            crate::query::FORM_CONTENT_TYPE.to_string(),
        )];
        let signed = sign(
            &SigningRequest {
                method: "POST",
                host,
                path: "/",
                query: &[],
                headers: &headers,
                payload: Payload::Bytes(&seen.body),
                region: REGION,
                service: EC2_SERVICE,
                timestamp,
            },
            &credentials(session_token),
            &SigningOptions::standard(),
        )
        .unwrap();
        signed
            .headers
            .into_iter()
            .find(|(k, _)| k == crate::sigv4::AUTHORIZATION)
            .unwrap()
            .1
    }

    fn host_of(endpoint: &str) -> String {
        endpoint
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    }

    // ----- the request -----------------------------------------------------

    #[tokio::test]
    async fn a_call_is_a_signed_form_post_with_the_session_token() {
        let (endpoint, server) = serve(vec![ok(ZONES)]).await;
        let zones = client(&endpoint).availability_zones().await.expect("zones");
        assert!(!zones.is_empty());

        let seen = server.await.unwrap();
        assert_eq!(seen.len(), 1);
        let req = &seen[0];
        assert_eq!(req.target(), "POST /");
        let auth = req.header("authorization").expect("authorization");
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential="), "{auth}");
        assert!(auth.contains("/us-east-1/ec2/aws4_request"), "{auth}");
        assert!(
            auth.contains("SignedHeaders=content-type;host;x-amz-date;x-amz-security-token"),
            "{auth}"
        );
        assert!(req.header("x-amz-date").is_some());
        assert_eq!(
            req.header("x-amz-security-token").as_deref(),
            Some(SESSION_TOKEN)
        );
        assert_eq!(
            req.header("content-type").as_deref(),
            Some(crate::query::FORM_CONTENT_TYPE)
        );
        let body = req.body_str();
        assert_eq!(
            form_value(&body, "Action").as_deref(),
            Some("DescribeAvailabilityZones")
        );
        assert_eq!(form_value(&body, "Version").as_deref(), Some("2016-11-15"));
        assert_eq!(
            auth,
            expected_authorization(req, &host_of(&endpoint), Some(SESSION_TOKEN)),
            "the signature covers exactly what was sent"
        );
    }

    #[tokio::test]
    async fn without_a_session_token_no_security_token_is_sent() {
        let (endpoint, server) = serve(vec![ok(ZONES)]).await;
        client_with(&endpoint, None)
            .availability_zones()
            .await
            .expect("zones");
        let seen = server.await.unwrap();
        assert!(seen[0].header("x-amz-security-token").is_none());
        let auth = seen[0].header("authorization").unwrap();
        assert!(
            auth.contains("SignedHeaders=content-type;host;x-amz-date,"),
            "{auth}"
        );
        assert_eq!(
            auth,
            expected_authorization(&seen[0], &host_of(&endpoint), None)
        );
    }

    #[tokio::test]
    async fn the_secret_never_reaches_the_wire() {
        let (endpoint, server) = serve(vec![ok(ZONES)]).await;
        client(&endpoint).availability_zones().await.unwrap();
        let seen = server.await.unwrap();
        assert!(!seen[0].headers.contains(SECRET));
        assert!(!seen[0].body_str().contains(SECRET));
    }

    // ----- retries ---------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_throttled_answer_is_retried_and_then_succeeds() {
        let (endpoint, server) = serve(vec![
            respond(503, &[], &ec2_error("RequestLimitExceeded")),
            ok(ZONES),
        ])
        .await;
        let client = client(&endpoint).without_timeouts().unwrap();
        let zones = client.availability_zones().await.expect("retried");
        assert!(!zones.is_empty());
        assert_eq!(server.await.unwrap().len(), 2);
        assert!(
            !client.throttling_sustained(),
            "one throttle is not sustained"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_retry_budget_ends_with_the_throttled_error() {
        let policy = RetryPolicy::default();
        let attempts = usize::try_from(policy.max_attempts).unwrap();
        let (endpoint, server) = serve(vec![
            respond(503, &[], &ec2_error("RequestLimitExceeded"));
            attempts
        ])
        .await;
        let client = client(&endpoint).without_timeouts().unwrap();
        let err = client.availability_zones().await.unwrap_err();
        assert!(err.is_throttled(), "{err}");
        assert_eq!(server.await.unwrap().len(), attempts);
    }

    #[tokio::test(start_paused = true)]
    async fn many_throttled_answers_are_reported_as_sustained() {
        let policy = RetryPolicy {
            max_attempts: u32::try_from(SUSTAINED_THROTTLE_COUNT).unwrap(),
            ..RetryPolicy::default()
        };
        let (endpoint, _server) = serve(vec![
            respond(503, &[], &ec2_error("RequestLimitExceeded"));
            SUSTAINED_THROTTLE_COUNT
        ])
        .await;
        let client = client(&endpoint)
            .with_retry_policy(policy)
            .without_timeouts()
            .unwrap();
        assert!(!client.throttling_sustained());
        assert!(client.availability_zones().await.is_err());
        assert!(client.throttling_sustained());
    }

    #[tokio::test]
    async fn a_non_throttled_error_is_not_retried() {
        let (endpoint, server) =
            serve(vec![respond(403, &[], &ec2_error("UnauthorizedOperation"))]).await;
        let err = client(&endpoint).availability_zones().await.unwrap_err();
        assert!(matches!(err, Error::Forbidden(_)), "{err}");
        assert_eq!(server.await.unwrap().len(), 1, "exactly one request");
    }

    #[tokio::test]
    async fn a_transport_failure_carries_no_credentials() {
        // Bind, then drop: nothing listens on the port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let err = client(&endpoint).availability_zones().await.unwrap_err();
        assert!(matches!(err, Error::Transport(_)), "{err}");
        let text = err.to_string();
        assert!(!text.contains(SECRET) && !text.contains(SESSION_TOKEN));
    }

    // ----- the trait mapping -----------------------------------------------

    #[tokio::test]
    async fn describe_of_an_unknown_instance_is_none() {
        let (endpoint, server) = serve(vec![respond(
            400,
            &[],
            &ec2_error("InvalidInstanceID.NotFound"),
        )])
        .await;
        assert!(
            client(&endpoint)
                .describe_instance(INSTANCE)
                .await
                .expect("not found is not an error")
                .is_none()
        );
        let seen = server.await.unwrap();
        assert_eq!(
            form_value(&seen[0].body_str(), "InstanceId.1").as_deref(),
            Some(INSTANCE)
        );
    }

    #[tokio::test]
    async fn describe_with_an_empty_reservation_set_is_none() {
        let body = r#"<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
            <requestId>r</requestId><reservationSet/></DescribeInstancesResponse>"#;
        let (endpoint, _server) = serve(vec![ok(body)]).await;
        assert!(
            client(&endpoint)
                .describe_instance(INSTANCE)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn describe_returns_the_instance_asked_for() {
        let id = crate::types::parse_describe_instances(DESCRIBE).unwrap()[0]
            .instance_id
            .clone();
        let (endpoint, _server) = serve(vec![ok(DESCRIBE)]).await;
        let found = client(&endpoint).describe_instance(&id).await.unwrap();
        assert_eq!(found.map(|i| i.instance_id), Some(id));
    }

    #[tokio::test]
    async fn run_instance_returns_the_new_id() {
        let expected = crate::types::parse_run_instances(RUN).unwrap();
        let (endpoint, server) = serve(vec![ok(RUN)]).await;
        let request = crate::instance::RunInstancesRequest {
            image_id: "ami-0123456789abcdef0".to_string(),
            instance_type: "t3.small".to_string(),
            zone_name: "us-east-1a".to_string(),
            subnet_id: "subnet-0123456789abcdef0".to_string(),
            security_group_ids: vec!["sg-0123456789abcdef0".to_string()],
            root_device_name: "/dev/xvda".to_string(),
            root_volume_size_gib: 10,
            volume_type: crate::instance::VolumeType::Gp3,
            iops: None,
            throughput: None,
            kms_key_id: None,
            user_data: Vec::new(),
            client_token: "banlieue-test".to_string(),
            instance_tags: std::collections::BTreeMap::new(),
            volume_tags: std::collections::BTreeMap::new(),
            network_interface_tags: std::collections::BTreeMap::new(),
        };
        let id = client(&endpoint).run_instance(&request).await.unwrap();
        assert_eq!(id, expected);
        let body = server.await.unwrap()[0].body_str();
        assert_eq!(form_value(&body, "Action").as_deref(), Some("RunInstances"));
        assert_eq!(
            form_value(&body, "MetadataOptions.HttpTokens").as_deref(),
            Some("required")
        );
    }

    #[tokio::test]
    async fn terminate_of_a_gone_instance_is_none() {
        let (endpoint, _server) = serve(vec![respond(
            400,
            &[],
            &ec2_error("InvalidInstanceID.NotFound"),
        )])
        .await;
        assert_eq!(
            client(&endpoint)
                .terminate_instance(INSTANCE)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn terminate_and_stop_return_the_new_state() {
        let terminate_id = crate::types::parse_instance_state_changes(TERMINATE).unwrap()[0]
            .instance_id
            .clone();
        let stop_id = crate::types::parse_instance_state_changes(STOP).unwrap()[0]
            .instance_id
            .clone();
        let (endpoint, _server) = serve(vec![ok(TERMINATE), ok(STOP)]).await;
        let client = client(&endpoint);
        assert_eq!(
            client.terminate_instance(&terminate_id).await.unwrap(),
            Some(InstanceState::ShuttingDown)
        );
        assert_eq!(
            client.stop_instance(&stop_id).await.unwrap(),
            InstanceState::Stopping
        );
    }

    #[tokio::test]
    async fn a_state_change_for_another_instance_is_a_decode_error() {
        let (endpoint, _server) = serve(vec![ok(STOP)]).await;
        let err = client(&endpoint)
            .stop_instance("i-0other")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "{err}");
    }

    #[tokio::test]
    async fn the_console_is_decoded() {
        let expected = crate::types::parse_console_output(CONSOLE).unwrap();
        let (endpoint, _server) = serve(vec![ok(CONSOLE)]).await;
        assert_eq!(
            client(&endpoint).console_output(INSTANCE).await.unwrap(),
            expected
        );
    }

    // ----- pagination ------------------------------------------------------

    fn instances_page(id: &str, next: Option<&str>) -> String {
        let next = next.map_or(String::new(), |t| format!("<nextToken>{t}</nextToken>"));
        format!(
            r#"<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
            <requestId>r</requestId><reservationSet><item><instancesSet>
            <item><instanceId>{id}</instanceId><instanceState><name>running</name></instanceState></item>
            </instancesSet></item></reservationSet>{next}</DescribeInstancesResponse>"#
        )
    }

    #[tokio::test]
    async fn find_by_tag_follows_next_token() {
        let (endpoint, server) = serve(vec![
            ok(&instances_page("i-0aaa", Some("page-2"))),
            ok(&instances_page("i-0bbb", None)),
        ])
        .await;
        let found = client(&endpoint)
            .find_instances_by_tag("banlieue-owner-uid", "u-1")
            .await
            .unwrap();
        let ids: Vec<&str> = found.iter().map(|i| i.instance_id.as_str()).collect();
        assert_eq!(ids, ["i-0aaa", "i-0bbb"]);
        let seen = server.await.unwrap();
        assert_eq!(form_value(&seen[0].body_str(), "NextToken"), None);
        assert_eq!(
            form_value(&seen[1].body_str(), "NextToken").as_deref(),
            Some("page-2")
        );
        assert_eq!(
            form_value(&seen[1].body_str(), "Filter.1.Name").as_deref(),
            Some("tag%3Abanlieue-owner-uid")
        );
    }

    #[tokio::test]
    async fn offerings_follow_next_token() {
        let page = |location: &str, next: &str| {
            ok(&format!(
                r#"<DescribeInstanceTypeOfferingsResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
                <requestId>r</requestId><instanceTypeOfferingSet><item>
                <instanceType>t3.small</instanceType><locationType>availability-zone-id</locationType>
                <location>{location}</location></item></instanceTypeOfferingSet>{next}
                </DescribeInstanceTypeOfferingsResponse>"#
            ))
        };
        let (endpoint, server) = serve(vec![
            page("use1-az1", "<nextToken>t2</nextToken>"),
            page("use1-az2", ""),
        ])
        .await;
        let offers = client(&endpoint)
            .instance_type_offerings(&["t3.small".to_string()])
            .await
            .unwrap();
        let locations: Vec<&str> = offers.iter().map(|o| o.location.as_str()).collect();
        assert_eq!(locations, ["use1-az1", "use1-az2"]);
        assert_eq!(server.await.unwrap().len(), 2);
    }

    // ----- construction ----------------------------------------------------

    #[test]
    fn the_endpoint_is_regional() {
        install_provider();
        let client =
            Ec2Client::new(REGION, Arc::new(StaticSource::new(credentials(None)))).unwrap();
        assert_eq!(client.endpoint(), "https://ec2.us-east-1.amazonaws.com/");
        assert_eq!(client.region(), REGION);
        assert_eq!(
            ec2_endpoint("us-gov-west-1").unwrap(),
            "https://ec2.us-gov-west-1.amazonaws.com/"
        );
    }

    #[test]
    fn region_names_are_validated() {
        for good in [
            "us-east-1",
            "ca-central-1",
            "ap-southeast-2",
            "us-gov-west-1",
            "eu-south-2",
        ] {
            assert!(validate_region(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "us-east",
            "us-1",
            "US-east-1",
            "usa-east-1",
            "us--1",
            "us-east-1a",
            "us-east-x",
            "us.east-1",
            "us-east-1/../x",
            &format!("us-{}-1", "a".repeat(MAX_REGION_LEN)),
        ] {
            assert!(validate_region(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn plain_http_is_allowed_only_on_loopback() {
        assert!(check_endpoint("http://127.0.0.1:8080/").is_ok());
        assert!(check_endpoint("http://[::1]:8080/").is_ok());
        assert!(check_endpoint("https://ec2.us-east-1.amazonaws.com/").is_ok());
        assert!(check_endpoint("http://ec2.us-east-1.amazonaws.com/").is_err());
        assert!(
            check_endpoint("http://localhost:8080/").is_err(),
            "names are not trusted"
        );
        assert!(check_endpoint("http://192.0.2.1/").is_err());
        assert!(check_endpoint("https://user:pw@ec2.example.com/").is_err());
        assert!(check_endpoint("https://ec2.example.com/?x=1").is_err());
        assert!(check_endpoint("ftp://127.0.0.1/").is_err());
        assert!(check_endpoint("not a url").is_err());
    }

    #[test]
    fn with_endpoint_refuses_plain_http_off_loopback() {
        install_provider();
        let client =
            Ec2Client::new(REGION, Arc::new(StaticSource::new(credentials(None)))).unwrap();
        assert!(client.with_endpoint("http://203.0.113.5/").is_err());
    }

    #[test]
    fn the_signed_host_carries_a_non_default_port() {
        let url = check_endpoint("http://127.0.0.1:8080/").unwrap();
        assert_eq!(host_header(&url).unwrap(), "127.0.0.1:8080");
        let url = check_endpoint("https://ec2.us-east-1.amazonaws.com/").unwrap();
        assert_eq!(host_header(&url).unwrap(), "ec2.us-east-1.amazonaws.com");
    }

    #[test]
    fn debug_shows_no_credentials() {
        install_provider();
        let client = Ec2Client::new(
            REGION,
            Arc::new(StaticSource::new(credentials(Some(SESSION_TOKEN)))),
        )
        .unwrap();
        let shown = format!("{client:?}");
        assert!(!shown.contains(SECRET) && !shown.contains(SESSION_TOKEN));
    }
}
