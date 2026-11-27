// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `fake.rs`: the fake refuses what EC2 refused in the
//! roadmap 24 spike and what the shipped IAM policy denies, and allows what
//! EC2 allows (rules/testing.md, rule 1).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    // `base64::Engine` arrives with the glob below, from `fake.rs`.
    use super::super::*;
    use crate::api::Ec2Api;
    use crate::console::{ReadyState, ready_state};
    use crate::error::Error;
    use crate::instance::{
        EkKeyType, MAX_USER_DATA_BYTES, RunInstancesRequest, VolumeType, client_token_for,
    };
    use crate::tags::{OwnershipTags, TAG_OWNER_UID};
    use crate::types::{InstanceState, InstanceTypeInfo};

    const UID: &str = "0b9c5f0e-7a40-4d27-9e2c-1c3a8f5d2b11";
    const ZONE_A: &str = "ca-central-1a";
    const ZONE_B: &str = "ca-central-1b";
    const ZONE_D: &str = "ca-central-1d";
    const T3: &str = "t3.small";
    const M7I: &str = "m7i.large";
    const NO_TPM_TYPE: &str = "t2.micro";

    // ----- helpers -------------------------------------------------------

    /// A Kubernetes-shaped UID, distinct per `n`.
    fn uid(n: u32) -> String {
        format!("0b9c5f0e-7a40-4d27-9e2c-{n:012x}")
    }

    fn request_for(uid: &str, zone: &str, instance_type: &str) -> RunInstancesRequest {
        let tags = OwnershipTags::new(uid, "EC2Machine", "ns", "p")
            .resource_tags(Some("vm"), None, &BTreeMap::new())
            .expect("valid tags");
        RunInstancesRequest {
            image_id: "ami-0123456789abcdef0".to_string(),
            instance_type: instance_type.to_string(),
            zone_name: zone.to_string(),
            subnet_id: "subnet-0123456789abcdef0".to_string(),
            security_group_ids: vec!["sg-0123456789abcdef0".to_string()],
            root_device_name: "/dev/xvda".to_string(),
            root_volume_size_gib: 10,
            volume_type: VolumeType::Gp3,
            iops: None,
            throughput: None,
            kms_key_id: None,
            user_data: b"#cloud-config\n".to_vec(),
            client_token: client_token_for(uid, zone),
            instance_tags: tags.clone(),
            volume_tags: tags.clone(),
            network_interface_tags: tags,
        }
    }

    fn request() -> RunInstancesRequest {
        request_for(UID, ZONE_A, T3)
    }

    fn params(request: &RunInstancesRequest) -> Vec<(String, String)> {
        request
            .to_params()
            .expect("the builder accepts the request")
    }

    /// `params` with `key` set to `value`, added when absent.
    fn with(mut params: Vec<(String, String)>, key: &str, value: &str) -> Vec<(String, String)> {
        match params.iter_mut().find(|(k, _)| k == key) {
            Some(pair) => pair.1 = value.to_string(),
            None => params.push((key.to_string(), value.to_string())),
        }
        params
    }

    fn without(params: Vec<(String, String)>, key: &str) -> Vec<(String, String)> {
        params.into_iter().filter(|(k, _)| k != key).collect()
    }

    fn fake() -> FakeEc2 {
        FakeEc2::ca_central_1()
    }

    /// Runs `request` and describes it until EC2 shows it.
    async fn launch(fake: &FakeEc2, request: &RunInstancesRequest) -> String {
        let id = fake.run_instance(request).await.expect("launches");
        visible(fake, &id).await;
        id
    }

    async fn visible(fake: &FakeEc2, id: &str) -> Instance {
        for _ in 0..DEFAULT_VISIBILITY_DELAY + 1 {
            if let Some(instance) = fake.describe_instance(id).await.expect("describes") {
                return instance;
            }
        }
        panic!("{id} never became visible");
    }

    async fn state(fake: &FakeEc2, id: &str) -> InstanceState {
        fake.describe_instance(id)
            .await
            .expect("describes")
            .expect("visible")
            .state
    }

    fn code(err: &Error) -> (u16, &str) {
        let fault = err.fault().expect("an AWS fault");
        (fault.status, fault.code.as_str())
    }

    fn no_tpm_type() -> InstanceTypeInfo {
        InstanceTypeInfo {
            instance_type: NO_TPM_TYPE.to_string(),
            default_vcpus: 1,
            architectures: vec!["x86_64".to_string()],
            nitro_tpm_supported: false,
            nitro_tpm_versions: Vec::new(),
            boot_modes: vec!["legacy-bios".to_string()],
        }
    }

    // ----- 1. idempotency ------------------------------------------------

    #[tokio::test]
    async fn the_same_token_and_parameters_return_the_same_instance() {
        let fake = fake();
        let first = fake.run_instance(&request()).await.expect("launches");
        let again = fake.run_instance(&request()).await.expect("retry");
        assert_eq!(first, again);
        assert!(first.starts_with("i-"));
        assert_eq!(fake.instance_count(), 1);
    }

    #[tokio::test]
    async fn the_same_token_with_another_parameter_is_an_idempotent_parameter_mismatch() {
        let fake = fake();
        fake.run_instance(&request()).await.expect("launches");
        let mut other = request();
        other.instance_type = M7I.to_string();
        let err = fake.run_instance(&other).await.expect_err("mismatch");
        assert!(matches!(err, Error::IdempotentParameterMismatch(_)));
        assert_eq!(code(&err), (400, "IdempotentParameterMismatch"));
        assert_eq!(fake.instance_count(), 1);
    }

    #[tokio::test]
    async fn the_same_token_with_other_user_data_is_a_mismatch_too() {
        let fake = fake();
        fake.run_instance(&request()).await.expect("launches");
        let mut other = request();
        other.user_data = b"#cloud-config\nhostname: other\n".to_vec();
        let err = fake.run_instance(&other).await.expect_err("mismatch");
        assert!(matches!(err, Error::IdempotentParameterMismatch(_)));
    }

    #[tokio::test]
    async fn an_idempotent_retry_is_answered_even_once_capacity_ran_out() {
        let fake = fake();
        let first = fake.run_instance(&request()).await.expect("launches");
        fake.set_insufficient_capacity(ZONE_A, T3, true);
        assert_eq!(fake.run_instance(&request()).await.expect("retry"), first);
    }

    #[tokio::test]
    async fn another_zone_is_another_token_and_another_instance() {
        let fake = fake();
        let a = fake.run_instance(&request()).await.expect("zone a");
        let b = fake
            .run_instance(&request_for(UID, ZONE_B, T3))
            .await
            .expect("zone b");
        assert_ne!(a, b);
    }

    // ----- 2. offerings, zones, types ------------------------------------

    #[tokio::test]
    async fn a_type_not_offered_in_the_zone_is_unsupported() {
        let fake = fake();
        fake.set_offered(ZONE_D, T3, false);
        let err = fake
            .run_instance(&request_for(UID, ZONE_D, T3))
            .await
            .expect_err("not offered");
        assert!(matches!(err, Error::NotOffered(_)));
        assert!(err.is_zone_unavailable());
        assert_eq!(code(&err), (400, "Unsupported"));
        assert_eq!(fake.instance_count(), 0);
        fake.run_instance(&request_for(UID, ZONE_A, T3))
            .await
            .expect("offered in another zone");
    }

    #[tokio::test]
    async fn an_unknown_zone_is_an_invalid_parameter_value() {
        let err = fake()
            .run_instance(&request_for(UID, "ca-central-1z", T3))
            .await
            .expect_err("no such zone");
        assert!(matches!(err, Error::InvalidRequest(_)));
        assert_eq!(code(&err), (400, "InvalidParameterValue"));
    }

    #[tokio::test]
    async fn an_unknown_instance_type_is_an_invalid_parameter_value() {
        let err = fake()
            .run_instance(&request_for(UID, ZONE_A, "x9.fake"))
            .await
            .expect_err("no such type");
        assert_eq!(code(&err), (400, "InvalidParameterValue"));
    }

    // ----- 3. capacity ---------------------------------------------------

    #[tokio::test]
    async fn no_capacity_is_insufficient_instance_capacity_in_that_zone_only() {
        let fake = fake();
        fake.set_insufficient_capacity(ZONE_A, T3, true);
        let err = fake
            .run_instance(&request())
            .await
            .expect_err("no capacity");
        assert!(matches!(err, Error::InsufficientCapacity(_)));
        assert!(err.is_zone_unavailable());
        assert_eq!(code(&err), (500, "InsufficientInstanceCapacity"));
        assert_eq!(fake.instance_count(), 0);
        fake.run_instance(&request_for(UID, ZONE_B, T3))
            .await
            .expect("another zone has capacity");
        fake.run_instance(&request_for(&uid(2), ZONE_A, M7I))
            .await
            .expect("another type has capacity");
        fake.set_insufficient_capacity(ZONE_A, T3, false);
        fake.run_instance(&request()).await.expect("capacity back");
    }

    // ----- 4. vCPU quota -------------------------------------------------

    #[tokio::test]
    async fn the_vcpu_quota_refuses_the_instance_that_would_exceed_it() {
        let fake = fake();
        fake.set_vcpu_quota(4);
        fake.run_instance(&request_for(&uid(1), ZONE_A, T3))
            .await
            .expect("2 of 4");
        fake.run_instance(&request_for(&uid(2), ZONE_A, T3))
            .await
            .expect("4 of 4");
        let err = fake
            .run_instance(&request_for(&uid(3), ZONE_A, T3))
            .await
            .expect_err("6 of 4");
        assert!(matches!(err, Error::QuotaExceeded(_)));
        assert_eq!(code(&err), (400, "VcpuLimitExceeded"));
    }

    #[tokio::test]
    async fn a_stopping_instance_still_counts_against_the_vcpu_quota() {
        let fake = fake();
        fake.set_vcpu_quota(2);
        let id = launch(&fake, &request_for(&uid(1), ZONE_A, T3)).await;
        fake.stop_instance(&id).await.expect("stops");
        let err = fake
            .run_instance(&request_for(&uid(2), ZONE_A, T3))
            .await
            .expect_err("stopping counts");
        assert!(matches!(err, Error::QuotaExceeded(_)));
    }

    #[tokio::test]
    async fn stopped_and_terminated_instances_free_vcpu_quota() {
        let fake = fake();
        fake.set_vcpu_quota(2);
        let first = launch(&fake, &request_for(&uid(1), ZONE_A, T3)).await;
        fake.stop_instance(&first).await.expect("stops");
        assert_eq!(state(&fake, &first).await, InstanceState::Stopped);
        let second = launch(&fake, &request_for(&uid(2), ZONE_A, T3)).await;
        fake.terminate_instance(&second).await.expect("terminates");
        assert_eq!(state(&fake, &second).await, InstanceState::Terminated);
        fake.run_instance(&request_for(&uid(3), ZONE_A, T3))
            .await
            .expect("quota freed");
    }

    #[tokio::test]
    async fn starting_a_stopped_instance_beyond_the_quota_is_refused() {
        let fake = fake();
        fake.set_vcpu_quota(2);
        let first = launch(&fake, &request_for(&uid(1), ZONE_A, T3)).await;
        fake.stop_instance(&first).await.expect("stops");
        state(&fake, &first).await;
        launch(&fake, &request_for(&uid(2), ZONE_A, T3)).await;
        let err = fake.start_instance(&first).await.expect_err("over quota");
        assert!(matches!(err, Error::QuotaExceeded(_)));
    }

    // ----- 5. the shipped IAM policy -------------------------------------

    fn assert_unauthorized(fake: &FakeEc2, params: &[(String, String)]) {
        let err = fake
            .run_instance_params(params)
            .expect_err("the policy denies it");
        assert!(matches!(err, Error::Forbidden(_)), "{err}");
        assert_eq!(code(&err), (403, "UnauthorizedOperation"));
        assert_eq!(fake.instance_count(), 0);
    }

    #[test]
    fn the_builder_s_parameters_satisfy_the_policy() {
        let fake = fake();
        fake.run_instance_params(&params(&request()))
            .expect("the policy allows the hardened request");
    }

    #[test]
    fn the_policy_denies_imdsv1() {
        let p = with(params(&request()), "MetadataOptions.HttpTokens", "optional");
        assert_unauthorized(&fake(), &p);
        let p = without(params(&request()), "MetadataOptions.HttpTokens");
        assert_unauthorized(&fake(), &p);
    }

    #[test]
    fn the_policy_denies_a_hop_limit_above_one() {
        let p = with(
            params(&request()),
            "MetadataOptions.HttpPutResponseHopLimit",
            "2",
        );
        assert_unauthorized(&fake(), &p);
        let p = without(
            params(&request()),
            "MetadataOptions.HttpPutResponseHopLimit",
        );
        assert_unauthorized(&fake(), &p);
    }

    #[test]
    fn the_policy_denies_an_unencrypted_root_volume() {
        let p = with(
            params(&request()),
            "BlockDeviceMapping.1.Ebs.Encrypted",
            "false",
        );
        assert_unauthorized(&fake(), &p);
        let p = without(params(&request()), "BlockDeviceMapping.1.Ebs.Encrypted");
        assert_unauthorized(&fake(), &p);
    }

    #[test]
    fn the_policy_denies_a_public_ip() {
        let p = with(
            params(&request()),
            "NetworkInterface.1.AssociatePublicIpAddress",
            "true",
        );
        assert_unauthorized(&fake(), &p);
    }

    #[test]
    fn the_policy_denies_a_launch_without_the_ownership_tag_on_the_instance() {
        let mut untagged = request();
        untagged.instance_tags.remove(TAG_OWNER_UID);
        assert_unauthorized(&fake(), &params(&untagged));
        // On the volume and interface only is not enough.
        let mut no_instance_tags = request();
        no_instance_tags.instance_tags.clear();
        assert_unauthorized(&fake(), &params(&no_instance_tags));
    }

    #[test]
    fn the_policy_denies_any_instance_profile() {
        let p = with(params(&request()), "IamInstanceProfile.Name", "admin");
        assert_unauthorized(&fake(), &p);
        let p = with(
            params(&request()),
            "IamInstanceProfile.Arn",
            "arn:aws:iam::111122223333:instance-profile/x",
        );
        assert_unauthorized(&fake(), &p);
    }

    #[tokio::test]
    async fn a_policy_denial_wins_over_an_idempotent_retry() {
        let fake = fake();
        fake.run_instance(&request()).await.expect("launches");
        let p = with(params(&request()), "MetadataOptions.HttpTokens", "optional");
        let err = fake.run_instance_params(&p).expect_err("denied");
        assert!(matches!(err, Error::Forbidden(_)));
    }

    // ----- 6. user data --------------------------------------------------

    fn with_user_data(raw: &[u8]) -> Vec<(String, String)> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        with(params(&request()), "UserData", &encoded)
    }

    #[test]
    fn user_data_of_exactly_the_limit_is_accepted() {
        fake()
            .run_instance_params(&with_user_data(&vec![b'a'; MAX_USER_DATA_BYTES]))
            .expect("16384 raw bytes fit");
    }

    #[test]
    fn user_data_over_the_limit_is_refused_even_past_the_builder() {
        let fake = fake();
        let err = fake
            .run_instance_params(&with_user_data(&vec![b'a'; MAX_USER_DATA_BYTES + 1]))
            .expect_err("too large");
        assert!(matches!(err, Error::InvalidRequest(_)));
        assert_eq!(code(&err), (400, "InvalidParameterValue"));
        assert_eq!(fake.instance_count(), 0);
    }

    #[test]
    fn user_data_that_is_not_base64_is_refused() {
        let p = with(params(&request()), "UserData", "not base64!");
        let err = fake().run_instance_params(&p).expect_err("bad encoding");
        assert_eq!(code(&err), (400, "InvalidParameterValue"));
    }

    #[tokio::test]
    async fn the_trait_refuses_oversized_user_data_before_sending() {
        let mut big = request();
        big.user_data = vec![b'a'; MAX_USER_DATA_BYTES + 1];
        let err = fake().run_instance(&big).await.expect_err("refused");
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn a_missing_required_parameter_is_missing_parameter() {
        let p = without(params(&request()), "ImageId");
        let err = fake().run_instance_params(&p).expect_err("no AMI");
        assert_eq!(code(&err), (400, "MissingParameter"));
    }

    // ----- 7. eventual consistency ---------------------------------------

    #[tokio::test]
    async fn a_fresh_instance_is_not_found_on_its_first_describe() {
        let fake = fake();
        let id = fake.run_instance(&request()).await.expect("launches");
        assert_eq!(fake.describe_instance(&id).await.expect("describes"), None);
        let instance = fake
            .describe_instance(&id)
            .await
            .expect("describes")
            .expect("visible now");
        assert_eq!(instance.instance_id, id);
    }

    #[tokio::test]
    async fn the_visibility_delay_is_a_knob() {
        let fake = fake();
        fake.set_visibility_delay(3);
        let id = fake.run_instance(&request()).await.expect("launches");
        for _ in 0..3 {
            assert_eq!(fake.describe_instance(&id).await.expect("describes"), None);
        }
        assert!(fake.describe_instance(&id).await.expect("ok").is_some());

        fake.set_visibility_delay(0);
        let at_once = fake
            .run_instance(&request_for(&uid(2), ZONE_A, T3))
            .await
            .expect("launches");
        assert!(
            fake.describe_instance(&at_once)
                .await
                .expect("ok")
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_fresh_instance_is_hidden_from_a_tag_search_too() {
        let fake = fake();
        let id = fake.run_instance(&request()).await.expect("launches");
        let found = fake
            .find_instances_by_tag(TAG_OWNER_UID, UID)
            .await
            .expect("searches");
        assert!(found.is_empty());
        let found = fake
            .find_instances_by_tag(TAG_OWNER_UID, UID)
            .await
            .expect("searches");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].instance_id, id);
    }

    #[tokio::test]
    async fn an_unknown_id_is_not_found_and_a_malformed_one_is_refused() {
        let fake = fake();
        assert_eq!(
            fake.describe_instance("i-0123456789abcdef0")
                .await
                .expect("describes"),
            None
        );
        let err = fake.describe_instance("vm-1").await.expect_err("malformed");
        assert_eq!(code(&err), (400, "InvalidInstanceID.Malformed"));
        let err = fake.describe_instance("").await.expect_err("empty");
        assert!(matches!(err, Error::Invalid(_)));
    }

    // ----- 8. throttling -------------------------------------------------

    #[tokio::test]
    async fn the_next_calls_are_throttled_whatever_the_method() {
        let fake = fake();
        fake.throttle_next(2);
        let err = fake.availability_zones().await.expect_err("throttled");
        assert!(err.is_throttled());
        assert_eq!(code(&err), (503, "RequestLimitExceeded"));
        let err = fake.run_instance(&request()).await.expect_err("throttled");
        assert!(err.is_throttled());
        assert_eq!(fake.instance_count(), 0);
        fake.run_instance(&request()).await.expect("budget spent");
    }

    #[tokio::test]
    async fn every_fault_carries_a_request_id() {
        let fake = fake();
        fake.throttle_next(2);
        let first = fake.availability_zones().await.expect_err("throttled");
        let second = fake.availability_zones().await.expect_err("throttled");
        let first = first.fault().expect("fault").request_id.clone();
        let second = second.fault().expect("fault").request_id.clone();
        assert!(!first.is_empty());
        assert_ne!(first, second);
    }

    // ----- 9. lifecycle --------------------------------------------------

    #[tokio::test]
    async fn a_new_instance_runs_once_visible() {
        let fake = fake();
        let id = fake.run_instance(&request()).await.expect("launches");
        assert_eq!(fake.peek_state(&id), Some(InstanceState::Pending));
        let instance = visible(&fake, &id).await;
        assert_eq!(instance.state, InstanceState::Running);
        assert_eq!(instance.zone_name, ZONE_A);
        assert!(instance.private_ip.is_some());
        assert!(instance.launch_time.is_some());
    }

    #[tokio::test]
    async fn pending_can_be_held_for_some_describes() {
        let fake = fake();
        fake.set_transition_describes(2);
        let id = fake.run_instance(&request()).await.expect("launches");
        assert_eq!(visible(&fake, &id).await.state, InstanceState::Pending);
        assert_eq!(state(&fake, &id).await, InstanceState::Pending);
        assert_eq!(state(&fake, &id).await, InstanceState::Running);
    }

    #[tokio::test]
    async fn stop_then_start() {
        let fake = fake();
        let id = launch(&fake, &request()).await;
        assert_eq!(
            fake.stop_instance(&id).await.expect("stops"),
            InstanceState::Stopping
        );
        assert_eq!(state(&fake, &id).await, InstanceState::Stopped);
        assert_eq!(
            fake.stop_instance(&id)
                .await
                .expect("stopping again is fine"),
            InstanceState::Stopped
        );
        assert_eq!(
            fake.start_instance(&id).await.expect("starts"),
            InstanceState::Pending
        );
        assert_eq!(state(&fake, &id).await, InstanceState::Running);
    }

    #[tokio::test]
    async fn starting_a_running_instance_answers_running_as_ec2_does() {
        let fake = fake();
        let id = launch(&fake, &request()).await;
        let state = fake.start_instance(&id).await.expect("a no-op on EC2");
        assert_eq!(state, InstanceState::Running);
    }

    #[tokio::test]
    async fn stopping_a_pending_instance_is_an_incorrect_state() {
        let fake = fake();
        let id = fake.run_instance(&request()).await.expect("launches");
        let err = fake.stop_instance(&id).await.expect_err("pending");
        assert_eq!(code(&err), (400, "IncorrectInstanceState"));
    }

    #[tokio::test]
    async fn terminate_shuts_down_then_terminates_then_vanishes() {
        let fake = fake();
        fake.set_terminated_describes(2);
        let id = launch(&fake, &request()).await;
        assert_eq!(
            fake.terminate_instance(&id).await.expect("terminates"),
            Some(InstanceState::ShuttingDown)
        );
        assert_eq!(state(&fake, &id).await, InstanceState::Terminated);
        assert_eq!(
            fake.terminate_instance(&id).await.expect("again"),
            Some(InstanceState::Terminated)
        );
        assert_eq!(state(&fake, &id).await, InstanceState::Terminated);
        assert_eq!(fake.describe_instance(&id).await.expect("describes"), None);
        assert_eq!(fake.terminate_instance(&id).await.expect("gone"), None);
        assert_eq!(fake.peek_state(&id), None);
    }

    #[tokio::test]
    async fn terminating_an_unknown_instance_is_already_gone() {
        assert_eq!(
            fake()
                .terminate_instance("i-0123456789abcdef0")
                .await
                .expect("gone"),
            None
        );
    }

    #[tokio::test]
    async fn a_terminated_instance_cannot_be_started_or_stopped() {
        let fake = fake();
        let id = launch(&fake, &request()).await;
        fake.terminate_instance(&id).await.expect("terminates");
        let err = fake.start_instance(&id).await.expect_err("terminating");
        assert_eq!(code(&err), (400, "IncorrectInstanceState"));
        state(&fake, &id).await;
        let err = fake.start_instance(&id).await.expect_err("terminated");
        assert_eq!(code(&err), (400, "IncorrectInstanceState"));
        let err = fake.stop_instance(&id).await.expect_err("terminated");
        assert_eq!(code(&err), (400, "IncorrectInstanceState"));
    }

    #[tokio::test]
    async fn stopping_or_starting_an_unknown_instance_is_not_found() {
        let fake = fake();
        let err = fake
            .stop_instance("i-0123456789abcdef0")
            .await
            .expect_err("unknown");
        assert!(err.is_not_found());
        assert_eq!(code(&err), (400, "InvalidInstanceID.NotFound"));
        let err = fake
            .start_instance("i-0123456789abcdef0")
            .await
            .expect_err("unknown");
        assert!(err.is_not_found());
    }

    // ----- 10. tags ------------------------------------------------------

    #[tokio::test]
    async fn describe_returns_the_instance_tags() {
        let fake = fake();
        let id = fake.run_instance(&request()).await.expect("launches");
        let instance = visible(&fake, &id).await;
        assert_eq!(instance.tags, request().instance_tags);
    }

    #[tokio::test]
    async fn a_tag_search_matches_key_and_value_exactly() {
        let fake = fake();
        let mine = launch(&fake, &request_for(&uid(1), ZONE_A, T3)).await;
        launch(&fake, &request_for(&uid(2), ZONE_A, T3)).await;
        let found = fake
            .find_instances_by_tag(TAG_OWNER_UID, &uid(1))
            .await
            .expect("searches");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].instance_id, mine);
        let prefix = &uid(1)[..8];
        assert!(
            fake.find_instances_by_tag(TAG_OWNER_UID, prefix)
                .await
                .expect("searches")
                .is_empty()
        );
        assert!(
            fake.find_instances_by_tag("banlieue-owner", &uid(1))
                .await
                .expect("searches")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_tag_search_lists_terminated_instances_until_they_vanish() {
        let fake = fake();
        fake.set_terminated_describes(1);
        let id = launch(&fake, &request()).await;
        fake.terminate_instance(&id).await.expect("terminates");
        let found = fake
            .find_instances_by_tag(TAG_OWNER_UID, UID)
            .await
            .expect("searches");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].state, InstanceState::Terminated);
        assert!(
            fake.find_instances_by_tag(TAG_OWNER_UID, UID)
                .await
                .expect("searches")
                .is_empty()
        );
    }

    // ----- 11. console ---------------------------------------------------

    #[tokio::test]
    async fn the_console_is_empty_until_the_guest_reports_ready() {
        let fake = fake();
        let id = launch(&fake, &request()).await;
        assert_eq!(fake.console_output(&id).await.expect("reads"), "");
        fake.guest_reports_ready(&id);
        let text = fake.console_output(&id).await.expect("reads");
        assert_eq!(ready_state(&text, &id), ReadyState::Ready);
        assert!(text.contains(&format!("{READY_MARKER}{id}")));
    }

    #[tokio::test]
    async fn the_guest_can_report_ready_after_some_describes() {
        let fake = fake();
        fake.set_console_ready_after_describes(Some(2));
        let id = launch(&fake, &request()).await;
        assert_eq!(
            ready_state(&fake.console_output(&id).await.expect("reads"), &id),
            ReadyState::NotYet
        );
        state(&fake, &id).await;
        assert_eq!(
            ready_state(&fake.console_output(&id).await.expect("reads"), &id),
            ReadyState::Ready
        );
    }

    #[tokio::test]
    async fn a_restart_boots_a_fresh_console() {
        let fake = fake();
        let id = launch(&fake, &request()).await;
        fake.guest_reports_ready(&id);
        fake.stop_instance(&id).await.expect("stops");
        state(&fake, &id).await;
        fake.start_instance(&id).await.expect("starts");
        assert_eq!(fake.console_output(&id).await.expect("reads"), "");
    }

    #[tokio::test]
    async fn the_console_of_an_unknown_instance_is_not_found() {
        let err = fake()
            .console_output("i-0123456789abcdef0")
            .await
            .expect_err("unknown");
        assert!(err.is_not_found());
    }

    // ----- 12. NitroTPM EK -----------------------------------------------

    #[tokio::test]
    async fn the_ek_is_distinct_per_instance_and_key_type_and_stable_across_restarts() {
        let fake = fake();
        let a = launch(&fake, &request_for(&uid(1), ZONE_A, T3)).await;
        let b = launch(&fake, &request_for(&uid(2), ZONE_A, M7I)).await;
        let a_rsa = fake.tpm_ek_pub(&a, EkKeyType::Rsa2048).await.expect("ek");
        let a_ecc = fake
            .tpm_ek_pub(&a, EkKeyType::EccSecP384)
            .await
            .expect("ek");
        let b_rsa = fake.tpm_ek_pub(&b, EkKeyType::Rsa2048).await.expect("ek");
        assert_eq!(a_rsa.instance_id, a);
        assert_eq!(a_rsa.key_type, EkKeyType::Rsa2048);
        assert_eq!(a_rsa.key_format, "der");
        assert!(!a_rsa.key_value.is_empty());
        assert_ne!(a_rsa.key_value, a_ecc.key_value);
        assert_ne!(a_rsa.key_value, b_rsa.key_value);

        fake.stop_instance(&a).await.expect("stops");
        state(&fake, &a).await;
        fake.start_instance(&a).await.expect("starts");
        state(&fake, &a).await;
        let again = fake.tpm_ek_pub(&a, EkKeyType::Rsa2048).await.expect("ek");
        assert_eq!(again, a_rsa);
    }

    /// ADR-0167 Decision 1: EC2 answered `InstanceTpmEkPub.NotFound` for an
    /// instance that had just reached `running`, and the key on the next
    /// read. A `pending` instance has no key to read either.
    #[tokio::test]
    async fn the_ek_is_not_found_while_pending_and_for_the_configured_reads() {
        let fake = fake();
        fake.set_ek_not_found_reads(1);
        let id = fake.run_instance(&request()).await.expect("launches");
        let err = fake
            .tpm_ek_pub(&id, EkKeyType::Rsa2048)
            .await
            .expect_err("pending");
        assert!(err.is_not_found());
        assert_eq!(code(&err), (400, "InstanceTpmEkPub.NotFound"));

        visible(&fake, &id).await;
        assert_eq!(state(&fake, &id).await, InstanceState::Running);
        let err = fake
            .tpm_ek_pub(&id, EkKeyType::Rsa2048)
            .await
            .expect_err("just running");
        assert!(err.is_not_found());
        fake.tpm_ek_pub(&id, EkKeyType::Rsa2048)
            .await
            .expect("the next read has it");

        fake.stop_instance(&id).await.expect("stops");
        state(&fake, &id).await;
        fake.start_instance(&id).await.expect("starts");
        assert!(
            fake.tpm_ek_pub(&id, EkKeyType::EccSecP384)
                .await
                .expect_err("pending again")
                .is_not_found()
        );
        state(&fake, &id).await;
        assert!(
            fake.tpm_ek_pub(&id, EkKeyType::EccSecP384)
                .await
                .expect_err("a start reads not found again")
                .is_not_found()
        );
        fake.tpm_ek_pub(&id, EkKeyType::EccSecP384)
            .await
            .expect("then it reads");
    }

    #[tokio::test]
    async fn a_type_without_nitro_tpm_is_an_unsupported_operation() {
        let fake = fake();
        fake.add_instance_type(no_tpm_type());
        fake.set_offered(ZONE_A, NO_TPM_TYPE, true);
        let id = launch(&fake, &request_for(UID, ZONE_A, NO_TPM_TYPE)).await;
        let err = fake
            .tpm_ek_pub(&id, EkKeyType::Rsa2048)
            .await
            .expect_err("no NitroTPM");
        assert!(matches!(err, Error::InvalidRequest(_)));
        assert_eq!(code(&err), (400, "UnsupportedOperation"));
    }

    #[tokio::test]
    async fn the_ek_of_an_unknown_instance_is_not_found() {
        let err = fake()
            .tpm_ek_pub("i-0123456789abcdef0", EkKeyType::Rsa2048)
            .await
            .expect_err("unknown");
        assert!(err.is_not_found());
    }

    // ----- 13. IMDS ------------------------------------------------------

    #[tokio::test]
    async fn disabling_the_metadata_endpoint_is_recorded() {
        let fake = fake();
        let id = launch(&fake, &request()).await;
        assert_eq!(fake.imds_enabled(&id), Some(true));
        fake.disable_instance_metadata(&id).await.expect("disables");
        assert_eq!(fake.imds_enabled(&id), Some(false));
        fake.disable_instance_metadata(&id)
            .await
            .expect("disabling twice is fine");
        assert_eq!(fake.imds_enabled("i-0123456789abcdef0"), None);
    }

    #[tokio::test]
    async fn the_metadata_endpoint_of_a_terminated_instance_is_an_incorrect_state() {
        let fake = fake();
        let id = launch(&fake, &request()).await;
        fake.terminate_instance(&id).await.expect("terminates");
        state(&fake, &id).await;
        let err = fake
            .disable_instance_metadata(&id)
            .await
            .expect_err("terminated");
        assert_eq!(code(&err), (400, "IncorrectInstanceState"));
    }

    #[tokio::test]
    async fn the_metadata_endpoint_of_an_unknown_instance_is_not_found() {
        let err = fake()
            .disable_instance_metadata("i-0123456789abcdef0")
            .await
            .expect_err("unknown");
        assert!(err.is_not_found());
        assert_eq!(code(&err), (400, "InvalidInstanceID.NotFound"));
    }

    // ----- 14. the ca-central-1 preset -----------------------------------

    #[tokio::test]
    async fn the_preset_has_the_spike_s_zones() {
        let zones = fake().availability_zones().await.expect("zones");
        let pairs: Vec<(&str, &str)> = zones
            .iter()
            .map(|z| (z.name.as_str(), z.id.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                (ZONE_A, "cac1-az1"),
                (ZONE_B, "cac1-az2"),
                (ZONE_D, "cac1-az4")
            ]
        );
        assert!(zones.iter().all(|z| z.state == "available"));
    }

    #[tokio::test]
    async fn offerings_are_by_zone_id_for_the_asked_types_only() {
        let fake = fake();
        fake.set_offered(ZONE_D, M7I, false);
        let offerings = fake
            .instance_type_offerings(&[M7I.to_string()])
            .await
            .expect("offerings");
        let mut locations: Vec<&str> = offerings.iter().map(|o| o.location.as_str()).collect();
        locations.sort_unstable();
        assert_eq!(locations, ["cac1-az1", "cac1-az2"]);
        assert!(offerings.iter().all(|o| o.instance_type == M7I));
        let none = fake
            .instance_type_offerings(&["x9.fake".to_string()])
            .await
            .expect("an unknown type matches nothing");
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn the_preset_has_the_spike_s_instance_types() {
        let types = fake()
            .instance_types(&[T3.to_string(), M7I.to_string()])
            .await
            .expect("types");
        assert_eq!(types.len(), 2);
        for t in &types {
            assert_eq!(t.default_vcpus, 2);
            assert_eq!(t.architectures, ["x86_64"]);
            assert!(t.nitro_tpm_supported);
            assert_eq!(t.nitro_tpm_versions, ["2.0"]);
            assert!(t.supports_uefi());
        }
    }

    #[tokio::test]
    async fn describing_an_unknown_instance_type_is_invalid_instance_type() {
        let err = fake()
            .instance_types(&["x9.fake".to_string()])
            .await
            .expect_err("unknown");
        assert_eq!(code(&err), (400, "InvalidInstanceType"));
    }

    #[tokio::test]
    async fn the_preset_quota_is_the_raised_sixteen_vcpus() {
        let fake = fake();
        for n in 0..8 {
            fake.run_instance(&request_for(&uid(n), ZONE_A, T3))
                .await
                .expect("within 16 vCPUs");
        }
        let err = fake
            .run_instance(&request_for(&uid(8), ZONE_A, T3))
            .await
            .expect_err("17th and 18th vCPU");
        assert!(matches!(err, Error::QuotaExceeded(_)));
    }

    #[test]
    fn an_empty_fake_knows_no_zones() {
        let fake = FakeEc2::new();
        let err = fake
            .run_instance_params(&params(&request()))
            .expect_err("no zones");
        assert_eq!(code(&err), (400, "InvalidParameterValue"));
    }
}
