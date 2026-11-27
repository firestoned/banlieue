// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `Provider` reconciler, against `FakeEc2`'s
//! `ca-central-1` (three zones, `t3.small` and `m7i.large` offered in all).

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::{InstanceSizeMapping, ProviderCapabilities, ZonedInstanceSize};
    use banlieue_aws::FakeEc2;
    use banlieue_aws::types::InstanceTypeInfo;
    use banlieue_provider_sdk::status::find_condition;

    use super::super::*;
    use crate::test_support::{
        self, ENDPOINT, PROVIDER, REGION, TYPE_LARGE, TYPE_SMALL, ZONE_A, ZONE_A_ID, ZONE_B,
        ZONE_D, network, size, storage,
    };

    const NO_TPM_TYPE: &str = "t2.micro";
    const UNKNOWN_TYPE: &str = "x9.huge";

    fn caps(sizes: Vec<InstanceSizeMapping>) -> ProviderCapabilities {
        ProviderCapabilities {
            instance_sizes: sizes,
            storage_classes: vec![storage("ssd", "gp3")],
            network_classes: vec![network("lan", &[ZONE_A, ZONE_B, ZONE_D])],
            features: Vec::new(),
        }
    }

    async fn status_for(capabilities: ProviderCapabilities, fake: &FakeEc2) -> ProviderStatus {
        probe(fake, &test_support::provider(capabilities), 1)
            .await
            .expect("probe")
    }

    fn zones(status: &ProviderStatus) -> Vec<String> {
        status
            .failure_domains
            .iter()
            .map(|fd| fd.attributes.raw[KEY_ZONE].clone())
            .collect()
    }

    fn ready(status: &ProviderStatus) -> (String, String, String) {
        let c = find_condition(&status.conditions, condition_types::READY).expect("Ready");
        (c.status.clone(), c.reason.clone(), c.message.clone())
    }

    fn no_tpm_type() -> InstanceTypeInfo {
        InstanceTypeInfo {
            instance_type: NO_TPM_TYPE.into(),
            default_vcpus: 1,
            architectures: vec!["x86_64".into()],
            nitro_tpm_supported: false,
            nitro_tpm_versions: Vec::new(),
            boot_modes: vec!["legacy-bios".into()],
        }
    }

    /// ADR-0158 Decision 3: the zones are the ones that offer a mapped type,
    /// never a fixed list of three.
    #[tokio::test]
    async fn one_failure_domain_per_zone_that_offers_a_mapped_type() {
        let fake = FakeEc2::ca_central_1();
        fake.set_offered(ZONE_D, TYPE_SMALL, false);
        let status = status_for(caps(vec![size("small", TYPE_SMALL)]), &fake).await;
        assert_eq!(zones(&status), [ZONE_A, ZONE_B]);
        assert_eq!(ready(&status).0, condition_status::TRUE);
    }

    #[tokio::test]
    async fn a_failure_domain_names_its_zone_and_zone_id() {
        let fake = FakeEc2::ca_central_1();
        let status = status_for(caps(vec![size("small", TYPE_SMALL)]), &fake).await;
        let fd = &status.failure_domains[0];
        assert_eq!(fd.name, format!("{PROVIDER}-{ZONE_A}"));
        assert_eq!(fd.labels[LABEL_NAME], fd.name);
        assert_eq!(fd.labels[KEY_ZONE], ZONE_A);
        assert_eq!(fd.attributes.raw[KEY_ZONE], ZONE_A);
        assert_eq!(fd.attributes.raw[RAW_KEY_ZONE_ID], ZONE_A_ID);
        assert_eq!(fd.attributes.raw[RAW_KEY_REGION], REGION);
        assert_eq!(fd.attributes.raw[RAW_KEY_ENDPOINT], ENDPOINT);
        assert_eq!(fd.attributes.raw[RAW_KEY_VM_CLASSES], "small");
        assert_eq!(fd.attributes.raw[RAW_KEY_TPM_VM_CLASSES], "small");
        assert_eq!(fd.attributes.available_storage_classes, ["ssd"]);
        assert_eq!(fd.attributes.available_network_classes, ["lan"]);
        assert!(fd.attributes.capacity.is_none(), "no quota read yet");
    }

    #[test]
    fn a_long_failure_domain_name_is_hashed_to_a_label_value() {
        let long = "p".repeat(80);
        let name = failure_domain_name(&long, ZONE_A);
        assert!(name.len() <= banlieue_provider_sdk::naming::MAX_NAME_LEN);
    }

    /// ADR-0167 Decision 5: a class whose type has no NitroTPM is offered,
    /// but not as a TPM class.
    #[tokio::test]
    async fn a_type_without_nitro_tpm_is_not_a_tpm_class() {
        let fake = FakeEc2::ca_central_1();
        fake.add_instance_type(no_tpm_type());
        fake.set_offered(ZONE_A, NO_TPM_TYPE, true);
        let status = status_for(
            caps(vec![size("tiny", NO_TPM_TYPE), size("large", TYPE_LARGE)]),
            &fake,
        )
        .await;
        let fd = &status.failure_domains[0];
        assert_eq!(fd.attributes.raw[RAW_KEY_VM_CLASSES], "tiny,large");
        assert_eq!(fd.attributes.raw[RAW_KEY_TPM_VM_CLASSES], "large");
    }

    const ARM_TYPE: &str = "m8g.large";

    fn arm_type() -> InstanceTypeInfo {
        InstanceTypeInfo {
            instance_type: ARM_TYPE.into(),
            default_vcpus: 2,
            architectures: vec!["arm64".into()],
            nitro_tpm_supported: true,
            nitro_tpm_versions: vec!["2.0".into()],
            boot_modes: vec!["uefi".into()],
        }
    }

    /// The scheduler matches a `VMImage`'s architecture against these
    /// (roadmap 24 phase E): each class under the architecture of its type.
    #[tokio::test]
    async fn classes_are_published_by_architecture() {
        let fake = FakeEc2::ca_central_1();
        fake.add_instance_type(arm_type());
        fake.set_offered(ZONE_A, ARM_TYPE, true);
        let status = status_for(
            caps(vec![size("arm", ARM_TYPE), size("large", TYPE_LARGE)]),
            &fake,
        )
        .await;
        let fd = &status.failure_domains[0];
        assert_eq!(fd.attributes.raw[KEY_ZONE], ZONE_A);
        assert_eq!(fd.attributes.raw[RAW_KEY_AMD64_VM_CLASSES], "large");
        assert_eq!(fd.attributes.raw[RAW_KEY_ARM64_VM_CLASSES], "arm");
    }

    /// Both keys are published even when one architecture has no class, so
    /// an empty list means "none", not "unknown".
    #[tokio::test]
    async fn an_architecture_with_no_class_is_published_empty() {
        let fake = FakeEc2::ca_central_1();
        let status = status_for(caps(vec![size("small", TYPE_SMALL)]), &fake).await;
        let raw = &status.failure_domains[0].attributes.raw;
        assert_eq!(raw[RAW_KEY_AMD64_VM_CLASSES], "small");
        assert_eq!(raw[RAW_KEY_ARM64_VM_CLASSES], "");
    }

    #[test]
    fn architecture_lists_need_every_type_described() {
        let region = RegionView::default();
        assert_eq!(architecture_lists(&region, &[("small", TYPE_SMALL)]), None);
    }

    #[test]
    fn architecture_lists_split_by_ec2_architecture_name() {
        let mut amd = no_tpm_type();
        amd.instance_type = TYPE_SMALL.into();
        let region = RegionView {
            types: vec![amd, arm_type()],
            ..RegionView::default()
        };
        let lists = architecture_lists(&region, &[("small", TYPE_SMALL), ("arm", ARM_TYPE)]);
        assert_eq!(lists, Some(("small".to_string(), "arm".to_string())));
    }

    /// A mistyped mapping is a condition, never a probe failure that hides
    /// every zone (EC2 refuses to describe a type it does not know).
    #[tokio::test]
    async fn a_type_offered_nowhere_is_a_condition_not_a_failure() {
        let fake = FakeEc2::ca_central_1();
        let status = status_for(
            caps(vec![size("small", TYPE_SMALL), size("huge", UNKNOWN_TYPE)]),
            &fake,
        )
        .await;
        assert_eq!(status.failure_domains.len(), 3);
        let (state, reason, message) = ready(&status);
        assert_eq!(state, condition_status::FALSE);
        assert_eq!(reason, reasons::CAPABILITIES_INCOMPLETE);
        assert!(message.contains(UNKNOWN_TYPE), "{message}");
    }

    #[tokio::test]
    async fn no_mapped_type_is_a_condition_and_no_zone() {
        let fake = FakeEc2::ca_central_1();
        let status = status_for(caps(Vec::new()), &fake).await;
        assert!(status.failure_domains.is_empty());
        assert_eq!(ready(&status).1, reasons::CAPABILITIES_INCOMPLETE);
    }

    #[tokio::test]
    async fn a_per_zone_override_is_checked_in_its_zone() {
        let fake = FakeEc2::ca_central_1();
        fake.set_offered(ZONE_B, TYPE_SMALL, false);
        let mut mapping = size("small", TYPE_SMALL);
        mapping.per_zone = vec![ZonedInstanceSize {
            zone: ZONE_B.into(),
            size: TYPE_LARGE.into(),
        }];
        let status = status_for(caps(vec![mapping]), &fake).await;
        assert_eq!(zones(&status), [ZONE_A, ZONE_B, ZONE_D]);
    }

    #[tokio::test]
    async fn a_network_class_is_listed_only_where_it_has_a_subnet_and_groups() {
        let fake = FakeEc2::ca_central_1();
        let mut capabilities = caps(vec![size("small", TYPE_SMALL)]);
        capabilities.network_classes = vec![network("lan", &[ZONE_A])];
        let status = status_for(capabilities, &fake).await;
        let listed: Vec<bool> = status
            .failure_domains
            .iter()
            .map(|fd| !fd.attributes.available_network_classes.is_empty())
            .collect();
        assert_eq!(listed, [true, false, false]);
        assert_eq!(ready(&status).0, condition_status::TRUE);
    }

    #[tokio::test]
    async fn a_storage_class_without_a_volume_type_is_a_condition() {
        let fake = FakeEc2::ca_central_1();
        let mut capabilities = caps(vec![size("small", TYPE_SMALL)]);
        capabilities.storage_classes[0].target = None;
        let status = status_for(capabilities, &fake).await;
        assert_eq!(ready(&status).1, reasons::CAPABILITIES_INCOMPLETE);
    }

    #[test]
    fn an_unavailable_zone_publishes_no_failure_domain() {
        let provider = test_support::provider(caps(vec![size("small", TYPE_SMALL)]));
        let region = RegionView {
            zones: vec![
                AvailabilityZone {
                    name: ZONE_A.into(),
                    id: ZONE_A_ID.into(),
                    state: "available".into(),
                },
                AvailabilityZone {
                    name: ZONE_B.into(),
                    id: "cac1-az2".into(),
                    state: "impaired".into(),
                },
            ],
            offerings: vec![
                InstanceTypeOffering {
                    instance_type: TYPE_SMALL.into(),
                    location: ZONE_A_ID.into(),
                },
                InstanceTypeOffering {
                    instance_type: TYPE_SMALL.into(),
                    location: "cac1-az2".into(),
                },
            ],
            types: Vec::new(),
        };
        assert_eq!(zones(&compute_status(&provider, &region, 1)), [ZONE_A]);
    }

    #[test]
    fn mapped_types_are_deduplicated_with_overrides() {
        let mut mapping = size("small", TYPE_SMALL);
        mapping.per_zone = vec![ZonedInstanceSize {
            zone: ZONE_B.into(),
            size: TYPE_LARGE.into(),
        }];
        let types = mapped_types(&[mapping, size("other", TYPE_SMALL)]);
        assert_eq!(types, [TYPE_LARGE, TYPE_SMALL]);
    }

    #[tokio::test]
    async fn a_provider_without_an_ec2_block_cannot_be_probed() {
        let fake = FakeEc2::ca_central_1();
        let mut provider = test_support::provider(caps(vec![size("small", TYPE_SMALL)]));
        provider.spec.connection.ec2 = None;
        let err = probe(&fake, &provider, 1).await.expect_err("no ec2 block");
        assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);
    }

    #[test]
    fn identity_failures_read_as_unauthorized() {
        let fault = banlieue_aws::AwsFault::new(403, "UnauthorizedOperation", "no", "");
        assert_eq!(
            failure_reason(&Error::Aws(banlieue_aws::Error::Forbidden(fault.clone()))),
            reasons::UNAUTHORIZED
        );
        assert_eq!(
            failure_reason(&Error::Aws(banlieue_aws::Error::Unauthenticated(fault))),
            reasons::UNAUTHORIZED
        );
        assert_eq!(
            failure_reason(&Error::Aws(banlieue_aws::Error::Transport("reset".into()))),
            reasons::CONNECT_FAILED
        );
    }

    #[tokio::test]
    async fn throttling_keeps_the_last_failure_domains() {
        let fake = FakeEc2::ca_central_1();
        let fresh = status_for(caps(vec![size("small", TYPE_SMALL)]), &fake).await;
        fake.throttle_next(1);
        let err = probe(&fake, &test_support::provider(Default::default()), 2)
            .await
            .expect_err("throttled");
        assert!(matches!(err, Error::Aws(ref e) if e.is_throttled()));
        let kept = throttled_status(Some(&fresh), 2);
        assert_eq!(kept.failure_domains, fresh.failure_domains);
        assert_eq!(ready(&kept).1, reasons::THROTTLED);
        assert_eq!(kept.observed_generation, Some(2));
    }

    #[test]
    fn throttling_with_no_prior_status_publishes_nothing_stale() {
        assert!(throttled_status(None, 1).failure_domains.is_empty());
    }

    /// The sweep may judge every namespace only when machines were listed
    /// cluster-wide (selector mode); otherwise it is pinned to home.
    #[test]
    fn sweep_namespace_is_none_only_for_a_selector_scope() {
        let home = "banlieue-system";
        assert_eq!(sweep_namespace(None, home), Some(home));
        let home_only = ProviderScope::resolve(Some(home), home, None).expect("scope");
        assert_eq!(sweep_namespace(Some(&home_only), home), Some(home));
        let shared = ProviderScope::resolve(Some(home), home, Some("banlieue.io/tenant=true"))
            .expect("scope");
        assert_eq!(sweep_namespace(Some(&shared), home), None);
    }
}
