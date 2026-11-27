// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `EC2Machine` reconciler.
//!
//! `converge` and `finalize` take `&dyn Ec2Api`, so these drive the real
//! lifecycle against `FakeEc2`, which refuses what EC2 refused in the spike
//! and hides a fresh instance from its first describe, as EC2 does.
//! [`ScriptedEc2`] wraps it to put another instance's ready line on the
//! console, which the fake (honest by design) never does.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use banlieue_api::common::{PowerState, ProviderReference};
    use banlieue_aws::FakeEc2;
    use banlieue_aws::fake::DEFAULT_VISIBILITY_DELAY;
    use banlieue_aws::tags::{
        TAG_NAME, TAG_OWNER_KIND, TAG_OWNER_NAMESPACE, TAG_PROVIDER, TagResourceType,
    };
    use banlieue_aws::types::{AvailabilityZone, InstanceTypeInfo, InstanceTypeOffering, TpmEkPub};
    use banlieue_provider_sdk::status::find_condition;
    use kube::api::ObjectMeta;

    use super::super::*;
    use crate::test_support::{
        self, EXTRA_TAG, HOME, IMAGE_ID, PROVIDER, SECURITY_GROUP_ID, SUBNET_ID, TYPE_SMALL,
        ZONE_A, ZONE_A_ID, ZONE_B, launch_owned,
    };

    const NAME: &str = "web-1";
    const MACHINE_NAMESPACE: &str = "team-a";
    const UID: &str = "0f3c9a1e-0000-4000-8000-000000000001";
    const OTHER_UID: &str = "0f3c9a1e-0000-4000-8000-000000000002";
    const ROOT_GIB: u32 = 10;
    const PROVIDER_KMS: &str = "alias/banlieue-ebs";
    const SPEC_KMS: &str = "alias/other";
    const PEM_BEGIN: &str = "-----BEGIN PUBLIC KEY-----";

    fn machine() -> MachineRef<'static> {
        MachineRef {
            name: NAME,
            uid: UID,
            namespace: MACHINE_NAMESPACE,
        }
    }

    fn target() -> Target {
        Target::from_provider(&test_support::provider(Default::default())).expect("target")
    }

    fn spec() -> EC2MachineSpec {
        EC2MachineSpec {
            provider_id: None,
            failure_domain: Some(format!("{PROVIDER}-{ZONE_A}")),
            provider_ref: ProviderReference {
                name: PROVIDER.into(),
                namespace: None,
            },
            zone: ZONE_A.into(),
            instance_type: TYPE_SMALL.into(),
            image_id: IMAGE_ID.into(),
            root_device_name: "/dev/xvda".into(),
            root_volume_gi_b: ROOT_GIB,
            volume_type: Ec2VolumeType::Gp3,
            iops: None,
            throughput: None,
            kms_key_id: None,
            subnet_id: SUBNET_ID.into(),
            security_group_ids: vec![SECURITY_GROUP_ID.into()],
            tpm_enabled: false,
            user_data: Some("#cloud-config\n".into()),
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    fn later(by: Duration) -> jiff::Timestamp {
        now() + by
    }

    async fn pass(
        api: &dyn Ec2Api,
        spec: &EC2MachineSpec,
        prior: Option<&EC2MachineStatus>,
    ) -> Pass {
        converge(api, &target(), &machine(), spec, prior, 1, now()).await
    }

    /// Runs passes from `prior` until the instance is visible and observed.
    async fn until_observed(api: &dyn Ec2Api, spec: &EC2MachineSpec) -> Pass {
        let mut p = pass(api, spec, None).await;
        for _ in 0..=DEFAULT_VISIBILITY_DELAY {
            p = pass(api, spec, Some(&p.status)).await;
        }
        assert!(p.status.instance_state.is_some(), "observed: {p:?}");
        p
    }

    fn reason(status: &EC2MachineStatus, type_: &str) -> (String, String) {
        let c = find_condition(&status.conditions, type_)
            .unwrap_or_else(|| panic!("{type_} condition"));
        (c.status.clone(), c.reason.clone())
    }

    fn id(p: &Pass) -> String {
        p.status.instance_id.clone().expect("an instance ID")
    }

    // ---- create ----------------------------------------------------------

    #[tokio::test]
    async fn the_first_pass_launches_and_does_not_wait() {
        let fake = FakeEc2::ca_central_1();
        let p = pass(&fake, &spec(), None).await;
        let id = id(&p);
        assert!(is_instance_id(&id));
        assert_eq!(fake.instance_count(), 1);
        assert_eq!(p.provider_id, Some(format!("aws:///{ZONE_A}/{id}")));
        assert_eq!(p.requeue, FAST_POLL);
        assert_eq!(reason(&p.status, VM_CREATED).0, condition_status::TRUE);
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::CREATING
        );
        assert_eq!(p.status.create_failures, 0);
        assert_eq!(fake.imds_enabled(&id), Some(true), "IMDS on until ready");
    }

    /// ADR-0174 Decision 2: a fresh ID that describes as not found is not
    /// visible yet, never gone.
    #[tokio::test]
    async fn a_fresh_instance_not_visible_yet_is_creating() {
        let fake = FakeEc2::ca_central_1();
        let first = pass(&fake, &spec(), None).await;
        let p = pass(&fake, &spec(), Some(&first.status)).await;
        assert_eq!(id(&p), id(&first));
        assert!(p.status.instance_state.is_none());
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::CREATING
        );
        assert_eq!(p.requeue, FAST_POLL);
        assert_eq!(fake.instance_count(), 1);
    }

    /// A lost status (or a lost response) makes no second instance: the tag
    /// adopts it, or the client token returns it.
    #[tokio::test]
    async fn a_pass_without_the_stored_id_creates_nothing_new() {
        let fake = FakeEc2::ca_central_1();
        let first = pass(&fake, &spec(), None).await;
        let again = pass(&fake, &spec(), None).await;
        assert_eq!(id(&again), id(&first));
        assert_eq!(fake.instance_count(), 1);
    }

    #[tokio::test]
    async fn an_existing_tagged_instance_is_adopted() {
        let fake = FakeEc2::ca_central_1();
        let existing = launch_owned(&fake, UID, MACHINE_NAMESPACE).await;
        let p = pass(&fake, &spec(), None).await;
        assert_eq!(id(&p), existing);
        assert_eq!(fake.instance_count(), 1);
        assert_eq!(p.status.instance_state.as_deref(), Some("running"));
    }

    #[tokio::test]
    async fn a_malformed_stored_id_is_dropped_and_never_sent() {
        let fake = FakeEc2::ca_central_1();
        let prior = EC2MachineStatus {
            instance_id: Some("not-an-instance".into()),
            ..Default::default()
        };
        let p = pass(&fake, &spec(), Some(&prior)).await;
        assert!(is_instance_id(&id(&p)));
        assert_eq!(fake.instance_count(), 1);
    }

    #[tokio::test]
    async fn the_launch_is_tagged_and_hardened() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        let instance = fake
            .describe_instance(&id(&p))
            .await
            .expect("describes")
            .expect("visible");
        let tags = &instance.tags;
        assert_eq!(tags[TAG_OWNER_UID], UID);
        assert_eq!(tags[TAG_OWNER_KIND], OWNER_KIND);
        assert_eq!(tags[TAG_PROVIDER], format!("{HOME}.{PROVIDER}"));
        assert_eq!(tags[TAG_OWNER_NAMESPACE], MACHINE_NAMESPACE);
        assert_eq!(tags[TAG_NAME], NAME);
    }

    #[test]
    fn the_request_carries_the_token_tags_and_the_providers_key() {
        let mut provider = test_support::provider(Default::default());
        let mut ec2 = test_support::governed_ec2();
        ec2.kms_key_id = Some(PROVIDER_KMS.into());
        provider.spec.connection.ec2 = Some(ec2);
        let target = Target::from_provider(&provider).expect("target");
        let mut s = spec();
        s.kms_key_id = Some(SPEC_KMS.into());
        let request = run_request(&target, &machine(), &s).expect("request");
        assert_eq!(request.client_token, client_token_for(UID, ZONE_A));
        assert_eq!(request.kms_key_id.as_deref(), Some(PROVIDER_KMS));
        assert_eq!(request.instance_tags[EXTRA_TAG.0], EXTRA_TAG.1);
        assert_eq!(request.instance_tags[TAG_OWNER_UID], UID);
        assert_eq!(request.volume_tags, request.instance_tags);
        assert_eq!(request.network_interface_tags, request.instance_tags);
        assert_eq!(request.user_data, b"#cloud-config\n");

        // No key on the Provider: the machine's own applies.
        let plain = run_request(&self::target(), &machine(), &s).expect("request");
        assert_eq!(plain.kms_key_id.as_deref(), Some(SPEC_KMS));
        let params = plain.to_params().expect("params");
        assert!(
            params
                .iter()
                .any(|(_, v)| v == TagResourceType::Volume.as_str())
        );
    }

    /// ADR-0173: a re-placement in another zone is a new request, so it gets
    /// its own token.
    #[test]
    fn the_token_follows_the_zone() {
        let mut b = spec();
        b.zone = ZONE_B.into();
        let a = run_request(&target(), &machine(), &spec()).expect("a");
        let b = run_request(&target(), &machine(), &b).expect("b");
        assert_ne!(a.client_token, b.client_token);
    }

    #[tokio::test]
    async fn an_invalid_spec_makes_no_aws_call() {
        let fake = FakeEc2::ca_central_1();
        let mut s = spec();
        s.desired_power_state = PowerState::Suspended;
        let p = pass(&fake, &s, None).await;
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::INVALID_SPEC
        );
        assert_eq!(p.requeue, STEADY_REQUEUE);
        assert_eq!(fake.instance_count(), 0);
    }

    // ---- refused creates and the back-off --------------------------------

    #[tokio::test]
    async fn a_type_not_offered_in_the_zone_is_zone_unavailable_and_backs_off() {
        let fake = FakeEc2::ca_central_1();
        fake.set_offered(ZONE_A, TYPE_SMALL, false);
        let p = pass(&fake, &spec(), None).await;
        assert_eq!(
            reason(&p.status, VM_CREATED),
            (
                condition_status::FALSE.to_string(),
                reasons::ZONE_UNAVAILABLE.to_string()
            )
        );
        assert_eq!(p.status.create_failures, 1);
        assert_eq!(p.requeue, CREATE_RETRY_BACKOFF);
        assert_eq!(p.status.failure_domain, spec().failure_domain);
        assert!(p.status.instance_id.is_none());

        // Inside the back-off: nothing is sent.
        fake.set_offered(ZONE_A, TYPE_SMALL, true);
        let waiting = pass(&fake, &spec(), Some(&p.status)).await;
        assert_eq!(fake.instance_count(), 0);
        assert!(waiting.requeue <= CREATE_RETRY_BACKOFF);
    }

    #[tokio::test]
    async fn no_capacity_is_zone_unavailable_and_the_wait_doubles() {
        let fake = FakeEc2::ca_central_1();
        fake.set_insufficient_capacity(ZONE_A, TYPE_SMALL, true);
        let first = pass(&fake, &spec(), None).await;
        assert_eq!(
            reason(&first.status, VM_CREATED).1,
            reasons::ZONE_UNAVAILABLE
        );
        let after = later(CREATE_RETRY_BACKOFF + Duration::from_secs(1));
        let second = converge(
            &fake,
            &target(),
            &machine(),
            &spec(),
            Some(&first.status),
            1,
            after,
        )
        .await;
        assert_eq!(second.status.create_failures, 2);
        assert_eq!(second.requeue, create_backoff(2));
    }

    #[tokio::test]
    async fn an_exhausted_quota_is_quota_exceeded() {
        let fake = FakeEc2::ca_central_1();
        fake.set_vcpu_quota(0);
        let p = pass(&fake, &spec(), None).await;
        assert_eq!(reason(&p.status, VM_CREATED).1, reasons::QUOTA_EXCEEDED);
    }

    /// ADR-0173 Decision 5: a new generation retries at once.
    #[tokio::test]
    async fn a_new_generation_is_not_held_by_the_old_back_off() {
        let fake = FakeEc2::ca_central_1();
        fake.set_offered(ZONE_A, TYPE_SMALL, false);
        let p = pass(&fake, &spec(), None).await;
        assert!(create_backoff_remaining(&p.status, 1, now()).is_some());
        assert!(create_backoff_remaining(&p.status, 2, now()).is_none());
    }

    #[test]
    fn the_back_off_doubles_to_an_hour() {
        assert_eq!(create_backoff(0), CREATE_RETRY_BACKOFF);
        assert_eq!(create_backoff(1), CREATE_RETRY_BACKOFF);
        assert_eq!(create_backoff(2), CREATE_RETRY_BACKOFF * 2);
        assert_eq!(create_backoff(3), CREATE_RETRY_BACKOFF * 4);
        assert_eq!(create_backoff(5), CREATE_RETRY_BACKOFF_MAX);
        assert_eq!(create_backoff(u32::MAX), CREATE_RETRY_BACKOFF_MAX);
    }

    #[tokio::test]
    async fn a_throttled_create_waits_and_is_not_a_failure() {
        let fake = FakeEc2::ca_central_1();
        // The tag search before the launch.
        fake.throttle_next(1);
        let p = pass(&fake, &spec(), None).await;
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::THROTTLED
        );
        assert_eq!(p.requeue, THROTTLED_REQUEUE);
        assert_eq!(p.status.create_failures, 0);
        assert!(find_condition(&p.status.conditions, VM_CREATED).is_none());
    }

    #[test]
    fn only_lasting_refusals_count_as_failed_creates() {
        let fault = banlieue_aws::AwsFault::new(400, "x", "y", "");
        assert_eq!(
            create_failure_reason(&AwsError::NotOffered(fault.clone())),
            Some(reasons::ZONE_UNAVAILABLE)
        );
        assert_eq!(
            create_failure_reason(&AwsError::IdempotentParameterMismatch(fault.clone())),
            Some(reasons::CREATE_REFUSED)
        );
        assert_eq!(
            create_failure_reason(&AwsError::Forbidden(fault.clone())),
            Some(reasons::CREATE_REFUSED)
        );
        assert_eq!(
            create_failure_reason(&AwsError::Server(fault.clone())),
            None
        );
        assert_eq!(
            create_failure_reason(&AwsError::Unauthenticated(fault)),
            None
        );
        assert_eq!(
            create_failure_reason(&AwsError::Transport("reset".into())),
            None
        );
    }

    // ---- observe -----------------------------------------------------------

    #[tokio::test]
    async fn a_running_instance_is_provisioned_with_its_addresses_and_zone_id() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        assert_eq!(p.status.instance_state.as_deref(), Some("running"));
        assert_eq!(p.status.initialization.provisioned, Some(true));
        assert_eq!(p.status.zone_id.as_deref(), Some(ZONE_A_ID));
        assert_eq!(p.status.observed_power_state, Some(PowerState::PoweredOn));
        let types: Vec<MachineAddressType> = p
            .status
            .addresses
            .iter()
            .map(|a| a.address_type.clone())
            .collect();
        assert_eq!(
            types,
            [
                MachineAddressType::InternalIP,
                MachineAddressType::InternalDNS
            ]
        );
        assert_eq!(
            reason(&p.status, condition_types::READY).0,
            condition_status::TRUE
        );
        assert_eq!(
            reason(&p.status, condition_types::GUEST_READY).1,
            reasons::AWAITING_GUEST
        );
        assert_eq!(p.status.observed_generation, Some(1));
        assert_eq!(p.requeue, FAST_POLL, "waiting for the guest");
    }

    /// ADR-0175 and ADR-0174 Decision 3: the ready line sets `GuestReady`,
    /// then IMDS goes off, once.
    #[tokio::test]
    async fn the_ready_line_is_guest_ready_and_turns_imds_off() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        let id = id(&p);
        fake.guest_reports_ready(&id);
        let ready = pass(&fake, &spec(), Some(&p.status)).await;
        assert!(ready.status.guest_installed);
        assert_eq!(
            reason(&ready.status, condition_types::GUEST_READY),
            (
                condition_status::TRUE.to_string(),
                reasons::GUEST_REPORTED_READY.to_string()
            )
        );
        assert!(ready.status.imds_disabled);
        assert_eq!(fake.imds_enabled(&id), Some(false));
        assert_eq!(ready.requeue, STEADY_REQUEUE);
    }

    /// `FakeEc2`, with the console replaced by fixed text.
    struct ScriptedEc2 {
        inner: FakeEc2,
        console: Mutex<Option<String>>,
    }

    #[async_trait]
    impl Ec2Api for ScriptedEc2 {
        async fn run_instance(&self, r: &RunInstancesRequest) -> banlieue_aws::Result<String> {
            self.inner.run_instance(r).await
        }
        async fn describe_instance(&self, i: &str) -> banlieue_aws::Result<Option<Instance>> {
            self.inner.describe_instance(i).await
        }
        async fn find_instances_by_tag(
            &self,
            k: &str,
            v: &str,
        ) -> banlieue_aws::Result<Vec<Instance>> {
            self.inner.find_instances_by_tag(k, v).await
        }
        async fn terminate_instance(&self, i: &str) -> banlieue_aws::Result<Option<InstanceState>> {
            self.inner.terminate_instance(i).await
        }
        async fn stop_instance(&self, i: &str) -> banlieue_aws::Result<InstanceState> {
            self.inner.stop_instance(i).await
        }
        async fn start_instance(&self, i: &str) -> banlieue_aws::Result<InstanceState> {
            self.inner.start_instance(i).await
        }
        async fn console_output(&self, i: &str) -> banlieue_aws::Result<String> {
            let scripted = self
                .console
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            match scripted {
                Some(text) => Ok(text),
                None => self.inner.console_output(i).await,
            }
        }
        async fn tpm_ek_pub(&self, i: &str, k: EkKeyType) -> banlieue_aws::Result<TpmEkPub> {
            self.inner.tpm_ek_pub(i, k).await
        }
        async fn disable_instance_metadata(&self, i: &str) -> banlieue_aws::Result<()> {
            self.inner.disable_instance_metadata(i).await
        }
        async fn availability_zones(&self) -> banlieue_aws::Result<Vec<AvailabilityZone>> {
            self.inner.availability_zones().await
        }
        async fn instance_type_offerings(
            &self,
            t: &[String],
        ) -> banlieue_aws::Result<Vec<InstanceTypeOffering>> {
            self.inner.instance_type_offerings(t).await
        }
        async fn instance_types(
            &self,
            t: &[String],
        ) -> banlieue_aws::Result<Vec<InstanceTypeInfo>> {
            self.inner.instance_types(t).await
        }
    }

    #[tokio::test]
    async fn a_ready_line_for_another_instance_is_never_taken() {
        let api = ScriptedEc2 {
            inner: FakeEc2::ca_central_1(),
            console: Mutex::new(Some(
                "[   12.3] banlieue-ec2-guest: GuestReady instance=i-0aaaaaaaaaaaaaaaa\r\n".into(),
            )),
        };
        let p = until_observed(&api, &spec()).await;
        assert!(!p.status.guest_installed);
        assert!(!p.status.imds_disabled);
        assert_eq!(
            reason(&p.status, condition_types::GUEST_READY).1,
            reasons::READY_LINE_MISMATCH
        );
        assert_eq!(api.inner.imds_enabled(&id(&p)), Some(true));
    }

    /// ADR-0167: both EK public keys, as PEM, read again soon while EC2
    /// answers not found right after `running`.
    #[tokio::test]
    async fn a_tpm_machine_publishes_both_ek_public_keys_after_a_not_found() {
        let fake = FakeEc2::ca_central_1();
        fake.set_ek_not_found_reads(1);
        let mut s = spec();
        s.tpm_enabled = true;
        let p = until_observed(&fake, &s).await;
        assert!(p.status.tpm_endorsement_public_keys.is_empty());
        assert_eq!(p.requeue, FAST_POLL);
        let again = pass(&fake, &s, Some(&p.status)).await;
        let keys = &again.status.tpm_endorsement_public_keys;
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|k| k.starts_with(PEM_BEGIN)));
        assert_ne!(keys[0], keys[1]);
    }

    #[tokio::test]
    async fn a_machine_without_tpm_reads_no_ek() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        assert!(p.status.tpm_endorsement_public_keys.is_empty());
    }

    #[tokio::test]
    async fn powered_off_stops_then_powered_on_starts_and_guest_ready_is_sticky() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        fake.guest_reports_ready(&id(&p));
        let ready = pass(&fake, &spec(), Some(&p.status)).await;

        let mut off = spec();
        off.desired_power_state = PowerState::PoweredOff;
        let stopping = pass(&fake, &off, Some(&ready.status)).await;
        assert_eq!(stopping.status.instance_state.as_deref(), Some("stopping"));
        assert_eq!(
            reason(&stopping.status, condition_types::READY).1,
            reasons::AWAITING_POWER_STATE
        );
        let stopped = pass(&fake, &off, Some(&stopping.status)).await;
        assert_eq!(stopped.status.instance_state.as_deref(), Some("stopped"));
        assert_eq!(
            stopped.status.observed_power_state,
            Some(PowerState::PoweredOff)
        );
        assert_eq!(
            reason(&stopped.status, condition_types::READY).0,
            condition_status::TRUE
        );
        assert_eq!(stopped.status.initialization.provisioned, Some(true));

        let starting = pass(&fake, &spec(), Some(&stopped.status)).await;
        assert_eq!(starting.status.instance_state.as_deref(), Some("pending"));
        let running = pass(&fake, &spec(), Some(&starting.status)).await;
        assert_eq!(running.status.instance_state.as_deref(), Some("running"));
        assert!(running.status.guest_installed, "GuestReady is sticky");
        assert_eq!(
            reason(&running.status, condition_types::GUEST_READY).0,
            condition_status::TRUE
        );
    }

    #[tokio::test]
    async fn a_stored_instance_of_another_owner_is_dropped_and_left_alone() {
        let fake = FakeEc2::ca_central_1();
        let foreign = launch_owned(&fake, OTHER_UID, MACHINE_NAMESPACE).await;
        let prior = EC2MachineStatus {
            instance_id: Some(foreign.clone()),
            ..Default::default()
        };
        let p = pass(&fake, &spec(), Some(&prior)).await;
        assert!(p.status.instance_id.is_none());
        assert_eq!(reason(&p.status, VM_CREATED).1, reasons::FOREIGN_RESOURCE);
        assert_eq!(fake.peek_state(&foreign), Some(InstanceState::Running));
        assert_eq!(fake.instance_count(), 1);
    }

    #[tokio::test]
    async fn an_instance_terminated_elsewhere_is_reported_not_replaced() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        fake.terminate_instance(&id(&p)).await.expect("terminates");
        let gone = pass(&fake, &spec(), Some(&p.status)).await;
        assert_eq!(
            reason(&gone.status, condition_types::READY).1,
            reasons::INSTANCE_TERMINATED
        );
        assert!(gone.status.addresses.is_empty());
        // Once EC2 drops it, it stays reported and nothing is launched.
        let mut last = gone;
        for _ in 0..=banlieue_aws::fake::DEFAULT_TERMINATED_DESCRIBES {
            last = pass(&fake, &spec(), Some(&last.status)).await;
        }
        assert_eq!(
            reason(&last.status, condition_types::READY).1,
            reasons::INSTANCE_TERMINATED
        );
        assert_eq!(fake.instance_count(), 0);
    }

    #[tokio::test]
    async fn a_throttled_describe_keeps_what_is_known() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        fake.throttle_next(1);
        let t = pass(&fake, &spec(), Some(&p.status)).await;
        assert_eq!(t.requeue, THROTTLED_REQUEUE);
        assert_eq!(t.status.addresses, p.status.addresses);
        assert_eq!(t.provider_id, p.provider_id);
    }

    // ---- pure helpers ----------------------------------------------------

    #[test]
    fn power_maps_both_ways() {
        assert_eq!(
            observed_power(&InstanceState::Pending),
            Some(PowerState::PoweredOn)
        );
        assert_eq!(
            observed_power(&InstanceState::Stopping),
            Some(PowerState::PoweredOff)
        );
        assert_eq!(observed_power(&InstanceState::Terminated), None);
        assert_eq!(
            power_change(&PowerState::PoweredOff, &InstanceState::Running),
            Some(PowerChange::Stop)
        );
        assert_eq!(
            power_change(&PowerState::PoweredOn, &InstanceState::Stopped),
            Some(PowerChange::Start)
        );
        assert_eq!(
            power_change(&PowerState::PoweredOn, &InstanceState::Stopping),
            None
        );
        assert!(at_desired_power(
            &PowerState::PoweredOff,
            &InstanceState::Stopped
        ));
        assert!(!at_desired_power(
            &PowerState::PoweredOff,
            &InstanceState::Stopping
        ));
    }

    #[test]
    fn the_guest_cadence_is_fast_then_slow() {
        let t = now();
        assert_eq!(guest_poll_interval(None, t), FAST_POLL);
        assert_eq!(guest_poll_interval(Some(t), t), FAST_POLL);
        let old = t - FAST_POLL_WINDOW;
        assert_eq!(guest_poll_interval(Some(old), t), SLOW_POLL);
    }

    // ---- delete --------------------------------------------------------------

    /// Terminating or terminated (the fake settles a terminate on the next
    /// describe, which the same pass's tag search may be).
    fn terminating(fake: &FakeEc2, id: &str) -> bool {
        fake.peek_state(id).is_some_and(|s| s.is_gone())
    }

    async fn finalize_now(api: &dyn Ec2Api, prior: Option<&EC2MachineStatus>) -> Finalized {
        finalize(api, &machine(), prior, now())
            .await
            .expect("finalize")
    }

    #[tokio::test]
    async fn deletion_terminates_the_instance_then_finishes() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        let id = id(&p);
        let Finalized::Waiting(status, after) = finalize_now(&fake, Some(&p.status)).await else {
            panic!("waits for the terminate");
        };
        assert_eq!(after, DELETE_POLL);
        assert!(terminating(&fake, &id));
        assert_eq!(finalize_now(&fake, Some(&status)).await, Finalized::Done);
    }

    /// A machine deleted right after its create: the instance that is not
    /// visible yet is waited for, then terminated, never leaked.
    #[tokio::test]
    async fn deletion_right_after_the_create_waits_for_the_instance() {
        let fake = FakeEc2::ca_central_1();
        fake.set_visibility_delay(2);
        let p = pass(&fake, &spec(), None).await;
        let id = id(&p);
        // Neither the describe nor the tag search shows it yet.
        let Finalized::Waiting(status, _) = finalize_now(&fake, Some(&p.status)).await else {
            panic!("waits for visibility");
        };
        assert_eq!(fake.peek_state(&id), Some(InstanceState::Pending));
        let Finalized::Waiting(status, _) = finalize_now(&fake, Some(&status)).await else {
            panic!("terminating");
        };
        assert!(terminating(&fake, &id));
        assert_eq!(finalize_now(&fake, Some(&status)).await, Finalized::Done);
    }

    /// Past the visibility grace, an instance never seen is gone.
    #[tokio::test]
    async fn an_unseen_instance_past_the_grace_is_gone() {
        let fake = FakeEc2::ca_central_1();
        let prior = EC2MachineStatus {
            instance_id: Some("i-0123456789abcdef0".into()),
            ..Default::default()
        };
        assert_eq!(finalize_now(&fake, Some(&prior)).await, Finalized::Done);
    }

    #[tokio::test]
    async fn deletion_terminates_an_instance_only_the_tag_names() {
        let fake = FakeEc2::ca_central_1();
        let orphan = launch_owned(&fake, UID, MACHINE_NAMESPACE).await;
        assert!(matches!(
            finalize_now(&fake, None).await,
            Finalized::Waiting(..)
        ));
        assert!(terminating(&fake, &orphan));
    }

    #[tokio::test]
    async fn deletion_never_terminates_a_foreign_stored_instance() {
        let fake = FakeEc2::ca_central_1();
        let foreign = launch_owned(&fake, OTHER_UID, MACHINE_NAMESPACE).await;
        let prior = EC2MachineStatus {
            instance_id: Some(foreign.clone()),
            instance_state: Some("running".into()),
            ..Default::default()
        };
        assert_eq!(finalize_now(&fake, Some(&prior)).await, Finalized::Done);
        assert_eq!(fake.peek_state(&foreign), Some(InstanceState::Running));
    }

    #[tokio::test]
    async fn deleting_a_machine_that_never_launched_is_done() {
        let fake = FakeEc2::ca_central_1();
        assert_eq!(finalize_now(&fake, None).await, Finalized::Done);
    }

    #[tokio::test]
    async fn a_throttled_deletion_waits() {
        let fake = FakeEc2::ca_central_1();
        let p = until_observed(&fake, &spec()).await;
        fake.throttle_next(1);
        let Finalized::Waiting(_, after) = finalize_now(&fake, Some(&p.status)).await else {
            panic!("waits");
        };
        assert_eq!(after, THROTTLED_REQUEUE);
        assert_eq!(fake.peek_state(&id(&p)), Some(InstanceState::Running));
    }

    // ---- wiring ----------------------------------------------------------

    /// Another Provider's machine is skipped before any API call (bug-178):
    /// the offline client would fail any request.
    #[tokio::test]
    async fn a_machine_of_another_provider_is_not_served() {
        let url = "http://127.0.0.1:1".parse().expect("url");
        let client = kube::Client::try_from(kube::Config::new(url)).expect("client");
        let ctx = Context::new(
            client,
            Some(HOME.into()),
            Arc::new(crate::client::StaticClientFactory::new(Arc::new(
                FakeEc2::ca_central_1(),
            ))),
            Some(PROVIDER.into()),
        );
        let mut s = spec();
        s.provider_ref.name = "ec2-other".into();
        let machine = EC2Machine {
            metadata: ObjectMeta {
                name: Some(NAME.into()),
                namespace: Some(HOME.into()),
                uid: Some(UID.into()),
                ..Default::default()
            },
            spec: s,
            status: None,
        };
        let action = reconcile(Arc::new(machine), Arc::new(ctx))
            .await
            .expect("skipped");
        assert_eq!(
            format!("{action:?}"),
            format!("{:?}", Action::await_change())
        );
    }
}
