// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `AzureVirtualMachine` reconciler.
//!
//! `converge` and `finalize` take `&dyn ArmApi`, so these drive the real
//! lifecycle against `FakeArm`, which refuses what ARM refused in the
//! spike. [`ScriptedArm`] wraps it to answer writes with operation handles
//! and to script what polling them returns, which the fake (synchronous by
//! design) never does.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use banlieue_api::common::{PowerState, ProviderReference};
    use banlieue_api::infrastructure::{AzureOperation, AzureVirtualMachineSpec};
    use banlieue_azure::fake::{FakeArm, fake_nic_id};
    use banlieue_azure::types::{ResourceSku, TaggedResource, Usage};
    use banlieue_provider_sdk::status::find_condition;
    use kube::api::ObjectMeta;

    use super::super::*;
    use crate::reconciler::resources::{RESOURCE_TYPE_NIC, RESOURCE_TYPE_VM};
    use crate::test_support::{
        self, IMAGE_ID, LOCATION, PARTIAL_SIZE, PROVIDER, RESOURCE_GROUP, SUBNET_ID, USABLE_SIZE,
    };

    const NAME: &str = "web-1";
    const MACHINE_NAMESPACE: &str = "team-a";
    const UID: &str = "0f3c9a1e-0000-4000-8000-000000000001";
    const OTHER_UID: &str = "0f3c9a1e-0000-4000-8000-000000000002";
    const IMAGE_FLOOR_GB: u32 = 30;
    const OP_URL: &str = "https://management.azure.com/subscriptions/s/providers/Microsoft.Compute/locations/eastus2/operations/op-1";

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

    fn spec() -> AzureVirtualMachineSpec {
        AzureVirtualMachineSpec {
            provider_id: None,
            failure_domain: Some(format!("{PROVIDER}-1")),
            provider_ref: ProviderReference {
                name: PROVIDER.into(),
                namespace: None,
            },
            zone: "1".into(),
            size: USABLE_SIZE.into(),
            image_version_id: IMAGE_ID.into(),
            os_disk_size_gi_b: IMAGE_FLOOR_GB,
            os_disk_sku: "StandardSSD_LRS".into(),
            subnet_id: SUBNET_ID.into(),
            accelerated_networking: true,
            trusted_launch: true,
            user_data: Some("#cloud-config\n".into()),
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn world() -> FakeArm {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fixtures");
        fake.register_image(IMAGE_ID, IMAGE_FLOOR_GB, &["NVMe"]);
        fake
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    async fn pass(
        api: &dyn ArmApi,
        spec: &AzureVirtualMachineSpec,
        prior: Option<&AzureVirtualMachineStatus>,
    ) -> Pass {
        converge(api, &target(), &machine(), spec, prior, 1, now()).await
    }

    async fn listed(api: &dyn ArmApi) -> Vec<TaggedResource> {
        api.list_resources(RESOURCE_GROUP).await.expect("list")
    }

    fn count(resources: &[TaggedResource], kind: &str) -> usize {
        resources
            .iter()
            .filter(|r| r.resource_type.eq_ignore_ascii_case(kind))
            .count()
    }

    fn reason(status: &AzureVirtualMachineStatus, type_: &str) -> (String, String) {
        let c = find_condition(&status.conditions, type_)
            .unwrap_or_else(|| panic!("{type_} condition"));
        (c.status.clone(), c.reason.clone())
    }

    fn pending(action: AzureOperationAction) -> AzureVirtualMachineStatus {
        AzureVirtualMachineStatus {
            pending_operation: Some(AzureOperation {
                action,
                url: OP_URL.into(),
                kind: "AsyncOperation".into(),
            }),
            ..Default::default()
        }
    }

    /// `FakeArm`, answering chosen writes with an operation handle and
    /// polls with a scripted status.
    struct ScriptedArm {
        inner: FakeArm,
        async_writes: bool,
        status: Mutex<OperationStatus>,
        polls: Mutex<u32>,
    }

    impl ScriptedArm {
        fn new(inner: FakeArm, async_writes: bool, status: OperationStatus) -> Self {
            Self {
                inner,
                async_writes,
                status: Mutex::new(status),
                polls: Mutex::new(0),
            }
        }

        fn handle(&self, done: Option<OperationHandle>) -> Option<OperationHandle> {
            if !self.async_writes {
                return done;
            }
            Some(OperationHandle {
                url: OP_URL.into(),
                kind: OperationKind::AsyncOperation,
                retry_after: Some(Duration::from_secs(7)),
            })
        }

        fn polls(&self) -> u32 {
            *self.polls.lock().expect("lock")
        }
    }

    #[async_trait]
    impl ArmApi for ScriptedArm {
        async fn put_nic(
            &self,
            rg: &str,
            name: &str,
            r: &NicRequest,
        ) -> banlieue_azure::Result<Option<OperationHandle>> {
            self.inner
                .put_nic(rg, name, r)
                .await
                .map(|h| self.handle(h))
        }
        async fn nic_private_ip(
            &self,
            rg: &str,
            name: &str,
        ) -> banlieue_azure::Result<Option<String>> {
            self.inner.nic_private_ip(rg, name).await
        }
        async fn delete_nic(
            &self,
            rg: &str,
            name: &str,
        ) -> banlieue_azure::Result<Option<OperationHandle>> {
            self.inner
                .delete_nic(rg, name)
                .await
                .map(|h| self.handle(h))
        }
        async fn put_vm(
            &self,
            rg: &str,
            name: &str,
            r: &VmRequest,
        ) -> banlieue_azure::Result<Option<OperationHandle>> {
            self.inner.put_vm(rg, name, r).await.map(|h| self.handle(h))
        }
        async fn vm_instance_view(
            &self,
            rg: &str,
            name: &str,
        ) -> banlieue_azure::Result<InstanceView> {
            self.inner.vm_instance_view(rg, name).await
        }
        async fn vm_tags(
            &self,
            rg: &str,
            name: &str,
        ) -> banlieue_azure::Result<Option<BTreeMap<String, String>>> {
            self.inner.vm_tags(rg, name).await
        }
        async fn delete_vm(
            &self,
            rg: &str,
            name: &str,
        ) -> banlieue_azure::Result<Option<OperationHandle>> {
            self.inner.delete_vm(rg, name).await.map(|h| self.handle(h))
        }
        async fn power(
            &self,
            rg: &str,
            name: &str,
            a: PowerAction,
        ) -> banlieue_azure::Result<Option<OperationHandle>> {
            self.inner.power(rg, name, a).await.map(|h| self.handle(h))
        }
        async fn delete_disk(
            &self,
            rg: &str,
            name: &str,
        ) -> banlieue_azure::Result<Option<OperationHandle>> {
            self.inner
                .delete_disk(rg, name)
                .await
                .map(|h| self.handle(h))
        }
        async fn operation_status(
            &self,
            _h: &OperationHandle,
        ) -> banlieue_azure::Result<OperationStatus> {
            *self.polls.lock().expect("lock") += 1;
            Ok(self.status.lock().expect("lock").clone())
        }
        async fn resource_skus(&self, l: &str) -> banlieue_azure::Result<Vec<ResourceSku>> {
            self.inner.resource_skus(l).await
        }
        async fn usages(&self, l: &str) -> banlieue_azure::Result<Vec<Usage>> {
            self.inner.usages(l).await
        }
        async fn list_resources(&self, rg: &str) -> banlieue_azure::Result<Vec<TaggedResource>> {
            self.inner.list_resources(rg).await
        }
    }

    // ---- pure decisions ------------------------------------------------

    #[test]
    fn a_stored_operation_round_trips() {
        let handle = OperationHandle {
            url: OP_URL.into(),
            kind: OperationKind::Location,
            retry_after: None,
        };
        let stored = to_operation(AzureOperationAction::DeleteVm, &handle);
        assert_eq!(stored.kind, "Location");
        assert_eq!(to_handle(&stored), Some(handle));
        let edited = AzureOperation {
            kind: "Bogus".into(),
            ..stored
        };
        assert_eq!(to_handle(&edited), None);
    }

    #[test]
    fn vm_ownership_is_the_uid_tag_only() {
        let owner = target().ownership(UID, MACHINE_NAMESPACE);
        assert_eq!(VmOwnership::of(None, &owner), VmOwnership::Absent);
        assert_eq!(
            VmOwnership::of(Some(&owner.to_map()), &owner),
            VmOwnership::Ours
        );
        let other = target().ownership(OTHER_UID, MACHINE_NAMESPACE).to_map();
        assert_eq!(
            VmOwnership::of(Some(&other), &owner),
            VmOwnership::Foreign(Some(OTHER_UID.to_string()))
        );
        assert_eq!(
            VmOwnership::of(Some(&BTreeMap::new()), &owner),
            VmOwnership::Foreign(None)
        );
    }

    #[test]
    fn a_computer_name_is_a_valid_linux_host_name() {
        assert_eq!(computer_name("web-1", "fallback"), "web-1");
        assert_eq!(computer_name("web.prod.1", "fallback"), "web-prod-1");
        assert_eq!(computer_name("12345", "fallback"), "fallback");
        assert_eq!(computer_name("---", "fallback"), "fallback");
        assert_eq!(
            computer_name(&"a".repeat(100), "f").len(),
            MAX_COMPUTER_NAME_LEN
        );
    }

    #[test]
    fn deallocated_reads_as_powered_off() {
        assert_eq!(
            observed_power(Power::Deallocated),
            Some(PowerState::PoweredOff)
        );
        assert_eq!(observed_power(Power::Stopped), Some(PowerState::PoweredOff));
        assert_eq!(observed_power(Power::Running), Some(PowerState::PoweredOn));
        assert_eq!(observed_power(Power::Unknown), None);
    }

    /// ADR-0164 Decision 3: off is deallocate, never power-off.
    #[test]
    fn powered_off_means_deallocate() {
        assert_eq!(
            power_action(&PowerState::PoweredOff, Power::Running),
            Some((PowerAction::Deallocate, AzureOperationAction::Deallocate))
        );
        assert_eq!(
            power_action(&PowerState::PoweredOff, Power::Stopped),
            Some((PowerAction::Deallocate, AzureOperationAction::Deallocate))
        );
        assert_eq!(
            power_action(&PowerState::PoweredOff, Power::Deallocated),
            None
        );
        assert!(!at_desired_power(&PowerState::PoweredOff, Power::Stopped));
    }

    #[test]
    fn powered_on_starts_a_deallocated_vm_and_waits_on_transitions() {
        assert_eq!(
            power_action(&PowerState::PoweredOn, Power::Deallocated),
            Some((PowerAction::Start, AzureOperationAction::Start))
        );
        assert_eq!(power_action(&PowerState::PoweredOn, Power::Starting), None);
        assert_eq!(
            power_action(&PowerState::PoweredOn, Power::Deallocating),
            None
        );
        assert_eq!(power_action(&PowerState::Suspended, Power::Running), None);
    }

    #[test]
    fn the_create_backoff_holds_for_its_generation_only() {
        let mut status = AzureVirtualMachineStatus::default();
        let t0 = now();
        record_create_failure(&mut status, reasons::ZONE_UNAVAILABLE, "no capacity", 1, t0);
        let soon = t0
            .checked_add(jiff::SignedDuration::from_secs(60))
            .expect("ts");
        let later = t0
            .checked_add(jiff::SignedDuration::from_secs(301))
            .expect("ts");
        assert!(
            create_backoff_remaining(&status, 1, soon).is_some_and(|d| d <= CREATE_RETRY_BACKOFF)
        );
        assert_eq!(create_backoff_remaining(&status, 1, later), None);
        assert_eq!(
            create_backoff_remaining(&status, 2, soon),
            None,
            "a new spec retries at once"
        );
    }

    // ---- create and adopt ----------------------------------------------

    /// ADR-0164 Decision 3: the NIC, then the VM, both tagged with the UID.
    #[tokio::test]
    async fn the_first_pass_creates_the_nic_then_the_vm() {
        let fake = world();
        let p = pass(&fake, &spec(), None).await;

        let resources = listed(&fake).await;
        assert_eq!(count(&resources, RESOURCE_TYPE_NIC), 1);
        assert_eq!(count(&resources, RESOURCE_TYPE_VM), 1);
        for r in resources
            .iter()
            .filter(|r| !r.resource_type.ends_with("disks"))
        {
            assert_eq!(r.tags["banlieue-owner-uid"], UID);
            assert_eq!(r.tags["banlieue-owner-kind"], "AzureVirtualMachine");
            assert_eq!(
                r.tags["banlieue-provider"],
                format!("banlieue-system.{PROVIDER}")
            );
        }
        let vm = vm_name(UID);
        assert_eq!(
            p.provider_id.as_deref(),
            Some(target().provider_id(&vm).as_str())
        );
        assert!(
            p.provider_id
                .as_deref()
                .is_some_and(|id| id.starts_with("azure:///subscriptions/"))
        );
        assert_eq!(p.status.vm_name.as_deref(), Some(vm.as_str()));
        assert_eq!(p.status.failure_domain.as_deref(), Some("az-east-1"));
        assert_eq!(p.status.provisioning_state.as_deref(), Some("Creating"));
        assert_eq!(p.status.initialization.provisioned, Some(false));
        assert_eq!(reason(&p.status, VM_CREATED).0, condition_status::TRUE);
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::PROVISIONING
        );
        assert_eq!(
            reason(&p.status, condition_types::GUEST_READY).1,
            reasons::AWAITING_GUEST
        );
        assert_eq!(p.requeue, OBSERVE_POLL);
        assert_eq!(p.status.addresses.len(), 1);
    }

    /// ADR-0158 Decision 4: a create whose status write was lost is the same
    /// VM on the next pass, never a second one.
    #[tokio::test]
    async fn a_retry_after_a_lost_response_creates_nothing_new() {
        let fake = world();
        pass(&fake, &spec(), None).await;
        let before = listed(&fake).await.len();
        let again = pass(&fake, &spec(), None).await;
        assert_eq!(listed(&fake).await.len(), before);
        assert_eq!(reason(&again.status, VM_CREATED).0, condition_status::TRUE);
    }

    /// A NIC created by a pass whose VM create never ran is adopted.
    #[tokio::test]
    async fn an_existing_tagged_nic_is_adopted() {
        let fake = world();
        let owner = target().ownership(UID, MACHINE_NAMESPACE);
        let request = NicRequest {
            location: LOCATION.into(),
            subnet_id: SUBNET_ID.into(),
            accelerated_networking: false,
            tags: owner.to_map(),
        };
        fake.put_nic(RESOURCE_GROUP, &nic_name(UID), &request)
            .await
            .expect("nic");
        pass(&fake, &spec(), None).await;
        let resources = listed(&fake).await;
        assert_eq!(count(&resources, RESOURCE_TYPE_NIC), 1);
        assert_eq!(count(&resources, RESOURCE_TYPE_VM), 1);
    }

    /// The fake refuses a VM whose NIC ID it does not know, so this also
    /// proves the NIC ID is built under the right subscription and group.
    #[test]
    fn the_nic_id_is_the_one_arm_assigns() {
        assert!(fake_nic_id(RESOURCE_GROUP, "n").eq_ignore_ascii_case(&target().nic_id("n")));
    }

    #[tokio::test]
    async fn a_nic_with_our_name_and_another_owner_is_left_alone() {
        let fake = world();
        let request = NicRequest {
            location: LOCATION.into(),
            subnet_id: SUBNET_ID.into(),
            accelerated_networking: false,
            tags: target().ownership(OTHER_UID, MACHINE_NAMESPACE).to_map(),
        };
        fake.put_nic(RESOURCE_GROUP, &nic_name(UID), &request)
            .await
            .expect("nic");
        let p = pass(&fake, &spec(), None).await;
        assert_eq!(reason(&p.status, VM_CREATED).1, reasons::FOREIGN_RESOURCE);
        let resources = listed(&fake).await;
        assert_eq!(count(&resources, RESOURCE_TYPE_VM), 0);
        assert_eq!(resources[0].tags["banlieue-owner-uid"], OTHER_UID);
    }

    /// A VM with our name that exists before this pass and carries our
    /// UID tag is the one a previous pass created: it is adopted.
    #[tokio::test]
    async fn an_existing_vm_tagged_as_ours_is_adopted() {
        let fake = world();
        pass(&fake, &spec(), None).await;
        let again = pass(&fake, &spec(), None).await;
        assert_eq!(reason(&again.status, VM_CREATED).0, condition_status::TRUE);
        assert_eq!(again.provider_id, Some(target().provider_id(&vm_name(UID))));
        assert_eq!(count(&listed(&fake).await, RESOURCE_TYPE_VM), 1);
    }

    /// Our name but another owner's tag (or none): never adopted, never
    /// changed (no power action), reported as `ForeignResource`.
    #[tokio::test]
    async fn a_vm_with_our_name_and_a_foreign_or_missing_tag_is_not_adopted() {
        for tags in [
            target().ownership(OTHER_UID, MACHINE_NAMESPACE).to_map(),
            BTreeMap::new(),
        ] {
            let fake = world();
            pass(&fake, &spec(), None).await;
            fake.guest_reports_ready(RESOURCE_GROUP, &vm_name(UID));
            fake.set_vm_tags(RESOURCE_GROUP, &vm_name(UID), tags);

            let off = AzureVirtualMachineSpec {
                desired_power_state: PowerState::PoweredOff,
                ..spec()
            };
            let p = pass(&fake, &off, None).await;
            assert_eq!(
                reason(&p.status, VM_CREATED),
                (
                    condition_status::FALSE.to_string(),
                    reasons::FOREIGN_RESOURCE.to_string()
                )
            );
            assert_eq!(
                reason(&p.status, condition_types::READY).0,
                condition_status::FALSE
            );
            assert_eq!(p.provider_id, None);
            assert_ne!(p.status.initialization.provisioned, Some(true));
            let view = fake
                .vm_instance_view(RESOURCE_GROUP, &vm_name(UID))
                .await
                .expect("still there");
            assert_eq!(
                view.power,
                Power::Running,
                "a foreign VM is never powered off"
            );
        }
    }

    #[tokio::test]
    async fn an_invalid_spec_makes_no_arm_call() {
        let fake = world();
        let mut s = spec();
        s.desired_power_state = PowerState::Suspended;
        let p = pass(&fake, &s, None).await;
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::INVALID_SPEC
        );
        assert!(listed(&fake).await.is_empty());
        assert_eq!(p.provider_id, None);
    }

    /// ADR-0161 Decision 2: a zone problem is reported for the controller
    /// to place elsewhere, and not retried at once.
    #[tokio::test]
    async fn a_size_restricted_in_the_zone_is_zone_unavailable_and_backs_off() {
        let fake = world();
        let mut s = spec();
        s.size = PARTIAL_SIZE.into();
        s.zone = "2".into();
        let first = pass(&fake, &s, None).await;
        assert_eq!(
            reason(&first.status, VM_CREATED).1,
            reasons::ZONE_UNAVAILABLE
        );
        assert_eq!(
            reason(&first.status, condition_types::READY).1,
            reasons::ZONE_UNAVAILABLE
        );
        assert_eq!(first.requeue, CREATE_RETRY_BACKOFF);

        // Within the back-off nothing is sent, not even the NIC listing.
        let before = listed(&fake).await.len();
        let second = pass(&fake, &s, Some(&first.status)).await;
        assert!(second.requeue <= CREATE_RETRY_BACKOFF && second.requeue > Duration::ZERO);
        assert_eq!(listed(&fake).await.len(), before);

        // The controller re-places it (a new generation): created at once.
        s.zone = "1".into();
        let replaced = converge(
            &fake,
            &target(),
            &machine(),
            &s,
            Some(&first.status),
            2,
            now(),
        )
        .await;
        assert_eq!(
            reason(&replaced.status, VM_CREATED).0,
            condition_status::TRUE
        );
    }

    /// The spike: a refused VM create leaves its NIC behind; the
    /// finalizer then removes it by tag.
    #[tokio::test]
    async fn a_refused_create_leaves_its_nic_which_deletion_removes() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fixtures"); // no image
        let p = pass(&fake, &spec(), None).await;
        assert_eq!(reason(&p.status, VM_CREATED).1, reasons::CREATE_REFUSED);
        assert_eq!(count(&listed(&fake).await, RESOURCE_TYPE_NIC), 1);

        let done = finalize(&fake, &target(), &machine(), Some(&p.status))
            .await
            .expect("finalize");
        assert_eq!(done, Finalized::Done);
        assert!(listed(&fake).await.is_empty());
    }

    // ---- observe -------------------------------------------------------

    /// ADR-0163 Decision 1 and non-negotiable 6.
    #[tokio::test]
    async fn a_guest_ready_vm_is_provisioned_and_guest_installed() {
        let fake = world();
        let first = pass(&fake, &spec(), None).await;
        fake.guest_reports_ready(RESOURCE_GROUP, &vm_name(UID));
        let p = pass(&fake, &spec(), Some(&first.status)).await;
        assert_eq!(p.status.initialization.provisioned, Some(true));
        assert!(p.status.guest_installed);
        assert_eq!(p.status.provisioning_state.as_deref(), Some("Succeeded"));
        assert_eq!(p.status.observed_power_state, Some(PowerState::PoweredOn));
        assert_eq!(
            reason(&p.status, condition_types::GUEST_READY),
            (
                condition_status::TRUE.into(),
                reasons::GUEST_REPORTED_READY.into()
            )
        );
        assert_eq!(
            reason(&p.status, condition_types::READY),
            (condition_status::TRUE.into(), reasons::VM_READY.into())
        );
        assert_eq!(
            p.status.addresses[0].address_type,
            MachineAddressType::InternalIP
        );
        assert_eq!(p.status.observed_generation, Some(1));
        assert_eq!(p.requeue, STEADY_REQUEUE);
    }

    #[tokio::test]
    async fn os_provisioning_timed_out_is_guest_ready_false_with_that_reason() {
        let fake = world();
        let first = pass(&fake, &spec(), None).await;
        fake.fail_provisioning(RESOURCE_GROUP, &vm_name(UID), "OSProvisioningTimedOut");
        let p = pass(&fake, &spec(), Some(&first.status)).await;
        assert_eq!(
            reason(&p.status, condition_types::GUEST_READY),
            (
                condition_status::FALSE.into(),
                "OSProvisioningTimedOut".into()
            )
        );
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::PROVISIONING_FAILED
        );
        assert_eq!(p.status.initialization.provisioned, Some(false));
        assert_eq!(p.status.provisioning_state.as_deref(), Some("Failed"));
    }

    /// `PoweredOff` deallocates; `guestInstalled` stays (sticky).
    #[tokio::test]
    async fn powered_off_deallocates_and_guest_installed_is_sticky() {
        let fake = world();
        let first = pass(&fake, &spec(), None).await;
        fake.guest_reports_ready(RESOURCE_GROUP, &vm_name(UID));
        let ready = pass(&fake, &spec(), Some(&first.status)).await;

        let mut off = spec();
        off.desired_power_state = PowerState::PoweredOff;
        let p = pass(&fake, &off, Some(&ready.status)).await;
        assert_eq!(p.status.observed_power_state, Some(PowerState::PoweredOff));
        let view = fake
            .vm_instance_view(RESOURCE_GROUP, &vm_name(UID))
            .await
            .expect("view");
        assert_eq!(view.power, Power::Deallocated);
        assert!(p.status.guest_installed);
        assert_eq!(p.status.initialization.provisioned, Some(true));
        assert_eq!(
            reason(&p.status, condition_types::READY).0,
            condition_status::TRUE
        );

        let on = pass(&fake, &spec(), Some(&p.status)).await;
        assert_eq!(on.status.observed_power_state, Some(PowerState::PoweredOn));
        assert_eq!(
            reason(&on.status, condition_types::READY).0,
            condition_status::TRUE
        );
    }

    #[tokio::test]
    async fn an_async_power_change_is_stored_and_not_awaited() {
        let scripted = ScriptedArm::new(world(), false, OperationStatus::InProgress);
        let first = pass(&scripted, &spec(), None).await;
        scripted
            .inner
            .guest_reports_ready(RESOURCE_GROUP, &vm_name(UID));
        let scripted = ScriptedArm {
            async_writes: true,
            ..scripted
        };
        let mut off = spec();
        off.desired_power_state = PowerState::PoweredOff;
        let p = pass(&scripted, &off, Some(&first.status)).await;
        let op = p.status.pending_operation.as_ref().expect("pending");
        assert_eq!(op.action, AzureOperationAction::Deallocate);
        assert_eq!(p.requeue, Duration::from_secs(7));
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::AWAITING_POWER_STATE
        );
    }

    // ---- stored operations ---------------------------------------------

    /// ADR-0164 Decision 2: a VM create ARM runs asynchronously is stored,
    /// with the providerID set as soon as the PUT is accepted.
    #[tokio::test]
    async fn an_async_vm_create_is_stored_with_its_provider_id() {
        let scripted = ScriptedArm::new(world(), true, OperationStatus::InProgress);
        // The NIC PUT answers with a handle first.
        let nic = pass(&scripted, &spec(), None).await;
        let op = nic.status.pending_operation.clone().expect("nic op");
        assert_eq!(op.action, AzureOperationAction::CreateNic);
        assert_eq!(op.kind, "AsyncOperation");
        assert_eq!(nic.provider_id, None);

        *scripted.status.lock().expect("lock") = OperationStatus::Succeeded;
        let vm = pass(&scripted, &spec(), Some(&nic.status)).await;
        let op = vm.status.pending_operation.clone().expect("vm op");
        assert_eq!(op.action, AzureOperationAction::CreateVm);
        assert!(vm.provider_id.is_some());
        assert_eq!(
            count(&listed(&scripted).await, RESOURCE_TYPE_NIC),
            1,
            "the NIC was adopted, not re-created"
        );
    }

    #[tokio::test]
    async fn an_operation_in_progress_is_polled_once_and_nothing_else_is_done() {
        let scripted = ScriptedArm::new(world(), false, OperationStatus::InProgress);
        let p = pass(
            &scripted,
            &spec(),
            Some(&pending(AzureOperationAction::CreateVm)),
        )
        .await;
        assert_eq!(scripted.polls(), 1);
        assert!(p.status.pending_operation.is_some());
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::OPERATION_IN_PROGRESS
        );
        assert_eq!(p.requeue, OPERATION_POLL);
        assert!(listed(&scripted).await.is_empty());
        assert!(p.provider_id.is_some(), "a VM create was accepted");
    }

    #[tokio::test]
    async fn a_succeeded_operation_is_cleared_and_the_pass_continues() {
        let scripted = ScriptedArm::new(world(), false, OperationStatus::Succeeded);
        let p = pass(
            &scripted,
            &spec(),
            Some(&pending(AzureOperationAction::CreateNic)),
        )
        .await;
        assert!(p.status.pending_operation.is_none());
        assert_eq!(count(&listed(&scripted).await, RESOURCE_TYPE_VM), 1);
    }

    #[tokio::test]
    async fn a_failed_create_operation_records_the_code_and_backs_off() {
        let scripted = ScriptedArm::new(
            world(),
            false,
            OperationStatus::Failed {
                code: "ZonalAllocationFailed".into(),
                message: "no capacity in zone 1".into(),
            },
        );
        let p = pass(
            &scripted,
            &spec(),
            Some(&pending(AzureOperationAction::CreateVm)),
        )
        .await;
        assert!(p.status.pending_operation.is_none());
        assert_eq!(reason(&p.status, VM_CREATED).1, reasons::ZONE_UNAVAILABLE);
        let message = &find_condition(&p.status.conditions, VM_CREATED)
            .expect("cond")
            .message;
        assert!(message.contains("ZonalAllocationFailed"), "{message}");
        assert_eq!(p.requeue, CREATE_RETRY_BACKOFF);
        assert!(listed(&scripted).await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_power_operation_is_a_ready_condition() {
        let scripted = ScriptedArm::new(
            world(),
            false,
            OperationStatus::Failed {
                code: "OperationNotAllowed".into(),
                message: "no".into(),
            },
        );
        let p = pass(
            &scripted,
            &spec(),
            Some(&pending(AzureOperationAction::Start)),
        )
        .await;
        assert_eq!(
            reason(&p.status, condition_types::READY).1,
            reasons::OPERATION_FAILED
        );
        assert!(p.status.pending_operation.is_none());
        assert_eq!(p.requeue, ERROR_REQUEUE);
    }

    /// Status is user-editable: an operation kind banlieue never writes is
    /// dropped and the VM observed instead.
    #[tokio::test]
    async fn an_unpollable_stored_operation_is_dropped() {
        let scripted = ScriptedArm::new(world(), false, OperationStatus::InProgress);
        let mut prior = pending(AzureOperationAction::CreateVm);
        if let Some(op) = prior.pending_operation.as_mut() {
            op.kind = "Bogus".into();
        }
        let p = pass(&scripted, &spec(), Some(&prior)).await;
        assert_eq!(scripted.polls(), 0);
        assert!(p.status.pending_operation.is_none());
        assert_eq!(count(&listed(&scripted).await, RESOURCE_TYPE_VM), 1);
    }

    // ---- delete --------------------------------------------------------

    /// ADR-0158 Decision 6: the VM takes its NIC and OS disk.
    #[tokio::test]
    async fn deletion_removes_the_vm_and_everything_it_owned() {
        let fake = world();
        let p = pass(&fake, &spec(), None).await;
        assert!(!listed(&fake).await.is_empty());
        let done = finalize(&fake, &target(), &machine(), Some(&p.status))
            .await
            .expect("finalize");
        assert_eq!(done, Finalized::Done);
        assert!(listed(&fake).await.is_empty());
    }

    #[tokio::test]
    async fn deleting_a_machine_whose_resources_are_gone_is_done() {
        let fake = world();
        let done = finalize(&fake, &target(), &machine(), None)
            .await
            .expect("finalize");
        assert_eq!(done, Finalized::Done);
    }

    /// Never touch anything not tagged with this object's UID.
    #[tokio::test]
    async fn deletion_never_touches_untagged_or_foreign_resources() {
        let fake = world();
        for (name, tags) in [
            ("untagged-nic", BTreeMap::new()),
            (
                "other-nic",
                target().ownership(OTHER_UID, MACHINE_NAMESPACE).to_map(),
            ),
            // Our name, someone else's tag.
            (
                nic_name(UID).as_str(),
                target().ownership(OTHER_UID, MACHINE_NAMESPACE).to_map(),
            ),
        ] {
            let request = NicRequest {
                location: LOCATION.into(),
                subnet_id: SUBNET_ID.into(),
                accelerated_networking: false,
                tags,
            };
            fake.put_nic(RESOURCE_GROUP, name, &request)
                .await
                .expect("nic");
        }
        let done = finalize(&fake, &target(), &machine(), None)
            .await
            .expect("finalize");
        assert_eq!(done, Finalized::Done);
        assert_eq!(listed(&fake).await.len(), 3);
    }

    /// A VM with our name that is not tagged as ours is never deleted; the
    /// machine finalizes as if its VM were gone (the proxmox `find_ours`
    /// and ADR-0101 convention), so the finalizer does not block forever.
    /// Our own tagged NIC, held by that VM, is left too: ARM refuses it.
    #[tokio::test]
    async fn deletion_never_deletes_a_vm_with_our_name_and_a_foreign_or_missing_tag() {
        for tags in [
            target().ownership(OTHER_UID, MACHINE_NAMESPACE).to_map(),
            BTreeMap::new(),
        ] {
            let fake = world();
            let p = pass(&fake, &spec(), None).await;
            fake.set_vm_tags(RESOURCE_GROUP, &vm_name(UID), tags.clone());
            let done = finalize(&fake, &target(), &machine(), Some(&p.status))
                .await
                .expect("finalize");
            assert_eq!(done, Finalized::Done);
            let resources = listed(&fake).await;
            assert_eq!(count(&resources, RESOURCE_TYPE_VM), 1, "{tags:?}");
            assert_eq!(
                fake.vm_tags(RESOURCE_GROUP, &vm_name(UID))
                    .await
                    .expect("read"),
                Some(tags)
            );
        }
    }

    #[tokio::test]
    async fn an_async_delete_waits_then_finishes() {
        let world = world();
        pass(&world, &spec(), None).await;
        let scripted = ScriptedArm::new(world, true, OperationStatus::InProgress);
        let Finalized::Waiting(status, after) = finalize(&scripted, &target(), &machine(), None)
            .await
            .expect("finalize")
        else {
            panic!("expected to wait on the VM delete");
        };
        assert_eq!(
            status.pending_operation.as_ref().map(|o| o.action),
            Some(AzureOperationAction::DeleteVm)
        );
        assert_eq!(after, Duration::from_secs(7));

        // Still running: wait again without a second delete.
        let again = finalize(&scripted, &target(), &machine(), Some(status.as_ref()))
            .await
            .expect("finalize");
        assert!(matches!(again, Finalized::Waiting(_, OPERATION_POLL)));

        let scripted = ScriptedArm {
            async_writes: false,
            ..scripted
        };
        *scripted.status.lock().expect("lock") = OperationStatus::Succeeded;
        let done = finalize(&scripted, &target(), &machine(), Some(status.as_ref()))
            .await
            .expect("finalize");
        assert_eq!(done, Finalized::Done);
        assert!(listed(&scripted).await.is_empty());
    }

    #[tokio::test]
    async fn a_throttled_delete_waits_for_the_hint() {
        let fake = world();
        pass(&fake, &spec(), None).await;
        fake.set_delete_burst(0);
        let step = finalize(&fake, &target(), &machine(), None)
            .await
            .expect("finalize");
        assert!(matches!(step, Finalized::Waiting(_, d) if d > Duration::ZERO));
        fake.refill_delete_budget();
        fake.set_delete_burst(u32::MAX);
        assert_eq!(
            finalize(&fake, &target(), &machine(), None)
                .await
                .expect("finalize"),
            Finalized::Done
        );
    }

    // ---- wiring --------------------------------------------------------

    /// Another Provider's machine is skipped before any API call (bug-178):
    /// the offline client would fail any request.
    #[tokio::test]
    async fn a_machine_of_another_provider_is_not_served() {
        let url = "http://127.0.0.1:1".parse().expect("url");
        let client = kube::Client::try_from(kube::Config::new(url)).expect("client");
        let ctx = Context::new(
            client,
            Some("banlieue-system".into()),
            Arc::new(crate::client::StaticClientFactory::new(Arc::new(world()))),
            Some(PROVIDER.into()),
        );
        let mut s = spec();
        s.provider_ref.name = "az-west".into();
        let machine = AzureVirtualMachine {
            metadata: ObjectMeta {
                name: Some(NAME.into()),
                namespace: Some("banlieue-system".into()),
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

    // ----- ADR-0172: a policy-governed subscription -----

    fn governed_target() -> Target {
        let mut provider = test_support::provider(Default::default());
        provider.spec.connection.azure = Some(test_support::governed_azure());
        Target::from_provider(&provider).expect("target")
    }

    async fn governed_pass(api: &dyn ArmApi) -> Pass {
        converge(api, &governed_target(), &machine(), &spec(), None, 1, now()).await
    }

    #[test]
    fn the_target_carries_the_providers_governance_settings() {
        let t = governed_target();
        assert_eq!(
            t.extra_tags
                .get(test_support::EXTRA_TAG.0)
                .map(String::as_str),
            Some(test_support::EXTRA_TAG.1)
        );
        assert_eq!(
            t.disk_encryption_set_id.as_deref(),
            Some(test_support::DES_ID)
        );
        assert!(t.encryption_at_host);
        assert!(target().extra_tags.is_empty());
        assert!(target().disk_encryption_set_id.is_none());
        assert!(!target().encryption_at_host);
    }

    #[test]
    fn a_provider_with_an_invalid_governance_setting_yields_no_target() {
        let mut provider = test_support::provider(Default::default());
        let mut azure = test_support::governed_azure();
        azure
            .tags
            .insert("BANLIEUE-owner-uid".into(), OTHER_UID.into());
        provider.spec.connection.azure = Some(azure);
        assert!(Target::from_provider(&provider).is_err());
    }

    /// The VM and NIC carry the extra tag beside the ownership tags, the OS
    /// disk names the set, and the VM asks for encryption at host.
    #[tokio::test]
    async fn a_governed_provider_tags_and_encrypts_what_it_creates() {
        let fake = world();
        fake.add_disk_encryption_set(test_support::DES_ID, LOCATION);
        fake.register_encryption_at_host();
        let p = governed_pass(&fake).await;
        assert_eq!(reason(&p.status, VM_CREATED).0, condition_status::TRUE);

        let vm = vm_name(UID);
        let tags = fake
            .vm_tags(RESOURCE_GROUP, &vm)
            .await
            .expect("read")
            .expect("vm");
        assert_eq!(
            tags.get(test_support::EXTRA_TAG.0).map(String::as_str),
            Some(test_support::EXTRA_TAG.1)
        );
        assert_eq!(
            tags.get(banlieue_azure::tags::TAG_OWNER_UID)
                .map(String::as_str),
            Some(UID)
        );
        assert_eq!(
            fake.vm_disk_encryption_set(RESOURCE_GROUP, &vm).as_deref(),
            Some(test_support::DES_ID)
        );
        assert!(fake.vm_encryption_at_host(RESOURCE_GROUP, &vm));

        let resources = listed(&fake).await;
        let nic = resources
            .iter()
            .find(|r| r.resource_type.eq_ignore_ascii_case(RESOURCE_TYPE_NIC))
            .expect("nic");
        assert_eq!(
            nic.tags.get(test_support::EXTRA_TAG.0).map(String::as_str),
            Some(test_support::EXTRA_TAG.1)
        );
    }

    /// Encryption at host without the subscription feature is ARM's refusal,
    /// reported as `CreateRefused` with ARM's message, and backed off.
    #[tokio::test]
    async fn encryption_at_host_without_the_feature_is_create_refused() {
        let fake = world();
        fake.add_disk_encryption_set(test_support::DES_ID, LOCATION);
        let p = governed_pass(&fake).await;
        assert_eq!(
            reason(&p.status, VM_CREATED),
            (
                condition_status::FALSE.to_string(),
                reasons::CREATE_REFUSED.to_string()
            )
        );
        let message = find_condition(&p.status.conditions, VM_CREATED)
            .map(|c| c.message.clone())
            .unwrap_or_default();
        assert!(message.contains("EncryptionAtHost"), "{message}");
        assert!(
            fake.vm_tags(RESOURCE_GROUP, &vm_name(UID))
                .await
                .expect("read")
                .is_none()
        );
    }
}
