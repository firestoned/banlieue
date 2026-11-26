// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `Provider` reconciler, against the SKUs and quota
//! captured in the roadmap 23 spike (`FakeArm::from_fixtures`).

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use banlieue_api::banlieue::ProviderCapabilities;
    use banlieue_azure::fake::FakeArm;
    use banlieue_provider_sdk::status::find_condition;

    use super::super::*;
    use crate::test_support::{
        self, LOCATION, PARTIAL_SIZE, PROVIDER, RESOURCE_GROUP, RESTRICTED_SIZE, USABLE_SIZE,
        network, size, size_with_override, storage,
    };

    fn caps(sizes: Vec<banlieue_api::banlieue::InstanceSizeMapping>) -> ProviderCapabilities {
        ProviderCapabilities {
            instance_sizes: sizes,
            storage_classes: vec![storage("standard", "StandardSSD_LRS")],
            network_classes: vec![network("default", None)],
            features: Vec::new(),
        }
    }

    async fn status_for(capabilities: ProviderCapabilities, fake: &FakeArm) -> ProviderStatus {
        probe(fake, &test_support::provider(capabilities), 1)
            .await
            .expect("probe")
    }

    fn fixtures() -> FakeArm {
        FakeArm::from_fixtures(LOCATION).expect("fixtures")
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

    /// ADR-0158 Decision 3: the zones are the ones the subscription can
    /// use for a mapped size, never a fixed list of three.
    #[tokio::test]
    async fn one_failure_domain_per_zone_with_a_usable_mapped_size() {
        let status = status_for(caps(vec![size("small", PARTIAL_SIZE)]), &fixtures()).await;
        assert_eq!(zones(&status), ["1", "4"]);
        assert_eq!(ready(&status).0, condition_status::TRUE);
    }

    #[tokio::test]
    async fn a_failure_domain_names_its_zone_in_labels_and_raw() {
        let status = status_for(caps(vec![size("small", PARTIAL_SIZE)]), &fixtures()).await;
        let fd = &status.failure_domains[0];
        assert_eq!(fd.name, format!("{PROVIDER}-1"));
        assert_eq!(fd.labels[LABEL_NAME], fd.name);
        assert_eq!(fd.labels[KEY_ZONE], "1");
        assert_eq!(fd.attributes.raw[KEY_ZONE], "1");
        assert_eq!(fd.attributes.raw[RAW_KEY_LOCATION], LOCATION);
        assert_eq!(fd.attributes.raw[RAW_KEY_RESOURCE_GROUP], RESOURCE_GROUP);
        assert_eq!(fd.attributes.raw[RAW_KEY_VM_CLASSES], "small");
        assert_eq!(fd.attributes.available_storage_classes, ["standard"]);
        assert_eq!(fd.attributes.available_network_classes, ["default"]);
    }

    #[test]
    fn a_long_failure_domain_name_is_hashed_to_a_label_value() {
        let long = "p".repeat(80);
        let name = failure_domain_name(&long, "1");
        assert!(name.len() <= banlieue_provider_sdk::naming::MAX_NAME_LEN);
        assert_ne!(name, failure_domain_name(&format!("{long}x"), "1"));
    }

    /// ADR-0160 Decision 2: a size restricted everywhere raises a condition.
    #[tokio::test]
    async fn a_size_unusable_in_every_zone_is_a_condition() {
        let status = status_for(
            caps(vec![
                size("small", PARTIAL_SIZE),
                size("big", RESTRICTED_SIZE),
            ]),
            &fixtures(),
        )
        .await;
        let (state, reason, message) = ready(&status);
        assert_eq!(state, condition_status::FALSE);
        assert_eq!(reason, reasons::CAPABILITIES_INCOMPLETE);
        assert!(message.contains(RESTRICTED_SIZE), "{message}");
        assert!(message.contains("restricted"), "{message}");
        assert_eq!(
            zones(&status),
            ["1", "4"],
            "the usable size still publishes"
        );
        assert_eq!(
            status.failure_domains[0].attributes.raw[RAW_KEY_VM_CLASSES],
            "small"
        );
    }

    #[tokio::test]
    async fn a_size_not_offered_in_the_region_is_a_condition() {
        let status = status_for(caps(vec![size("small", "Standard_Nope_v1")]), &fixtures()).await;
        assert!(status.failure_domains.is_empty());
        assert!(ready(&status).2.contains("is not offered"));
    }

    #[tokio::test]
    async fn no_mapped_size_is_a_condition() {
        let status = status_for(caps(Vec::new()), &fixtures()).await;
        assert!(status.failure_domains.is_empty());
        assert!(ready(&status).2.contains("no instanceSizes"));
    }

    /// A per-zone override: zone 2 cannot run PARTIAL_SIZE but its override
    /// can, so zone 2 publishes too.
    #[tokio::test]
    async fn a_per_zone_override_makes_its_zone_usable() {
        let status = status_for(
            caps(vec![size_with_override(
                "small",
                PARTIAL_SIZE,
                "2",
                USABLE_SIZE,
            )]),
            &fixtures(),
        )
        .await;
        assert_eq!(zones(&status), ["1", "2", "4"]);
    }

    #[tokio::test]
    async fn a_network_class_is_listed_only_where_it_has_a_subnet() {
        let mut c = caps(vec![size("small", PARTIAL_SIZE)]);
        c.network_classes = vec![network("zonal", Some("4"))];
        let status = status_for(c, &fixtures()).await;
        let by_zone: Vec<(String, Vec<String>)> = status
            .failure_domains
            .iter()
            .map(|fd| {
                (
                    fd.attributes.raw[KEY_ZONE].clone(),
                    fd.attributes.available_network_classes.clone(),
                )
            })
            .collect();
        assert_eq!(
            by_zone,
            [
                ("1".to_string(), vec![]),
                ("4".to_string(), vec!["zonal".to_string()])
            ]
        );
        assert_eq!(ready(&status).0, condition_status::TRUE);
    }

    #[tokio::test]
    async fn a_storage_class_without_a_sku_is_a_condition() {
        let mut c = caps(vec![size("small", PARTIAL_SIZE)]);
        c.storage_classes[0].target = Some(test_support::target("tier", "gold"));
        let status = status_for(c, &fixtures()).await;
        assert!(
            status.failure_domains[0]
                .attributes
                .available_storage_classes
                .is_empty()
        );
        assert!(ready(&status).2.contains("storageClass standard"));
    }

    /// ADR-0161 Decision 1, captured quota: Dsv7 family 4 - 2 = 2, regional
    /// 4 - 3 = 1, so headroom is the regional 1.
    #[tokio::test]
    async fn capacity_is_the_smaller_of_regional_and_family_headroom() {
        let fake = fixtures();
        fake.use_captured_quota();
        let status = status_for(caps(vec![size("small", USABLE_SIZE)]), &fake).await;
        for fd in &status.failure_domains {
            let capacity = fd.attributes.capacity.as_ref().expect("capacity");
            assert_eq!(capacity.cpus, Some(1));
            assert_eq!(capacity.memory_mi_b, None);
        }
    }

    /// ADR-0161 Decision 3: zero headroom is unschedulable, not unready.
    #[tokio::test]
    async fn zero_headroom_is_not_an_error_state() {
        let fake = fixtures();
        fake.use_captured_quota();
        let mut provider = test_support::provider(caps(vec![size("small", USABLE_SIZE)]));
        provider.spec.capabilities.instance_sizes = vec![size("small", USABLE_SIZE)];
        let skus = fake.resource_skus(LOCATION).await.expect("skus");
        let mut usages = fake.usages(LOCATION).await.expect("usages");
        for u in &mut usages {
            u.current = u.limit;
        }
        let status = compute_status(&provider, &skus, &usages, 1);
        assert_eq!(
            status.failure_domains[0]
                .attributes
                .capacity
                .as_ref()
                .and_then(|c| c.cpus),
            Some(0)
        );
        assert_eq!(ready(&status).0, condition_status::TRUE);
    }

    #[tokio::test]
    async fn generous_quota_publishes_generous_headroom() {
        let status = status_for(caps(vec![size("small", USABLE_SIZE)]), &fixtures()).await;
        let cpus = status.failure_domains[0]
            .attributes
            .capacity
            .as_ref()
            .and_then(|c| c.cpus);
        assert!(cpus.is_some_and(|c| c > 1), "{cpus:?}");
    }

    #[tokio::test]
    async fn a_provider_without_an_azure_block_cannot_be_probed() {
        let mut provider = test_support::provider(caps(vec![size("small", USABLE_SIZE)]));
        provider.spec.connection.azure = None;
        let err = probe(&fixtures(), &provider, 1)
            .await
            .expect_err("no azure block");
        assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);
    }

    #[test]
    fn identity_failures_read_as_unauthorized() {
        for e in [
            banlieue_azure::Error::Unauthorized {
                code: String::new(),
                message: String::new(),
            },
            banlieue_azure::Error::Forbidden {
                code: String::new(),
                message: String::new(),
            },
            banlieue_azure::Error::TokenExchange {
                code: "AADSTS700016".into(),
                message: String::new(),
            },
        ] {
            assert_eq!(failure_reason(&Error::Azure(e)), reasons::UNAUTHORIZED);
        }
        assert_eq!(
            failure_reason(&Error::Azure(banlieue_azure::Error::Transport(
                "refused".into()
            ))),
            reasons::CONNECT_FAILED
        );
    }

    /// Throttling is a condition, not a hard failure: the last failure
    /// domains stay published.
    #[tokio::test]
    async fn throttling_keeps_the_last_failure_domains() {
        let prior = status_for(caps(vec![size("small", PARTIAL_SIZE)]), &fixtures()).await;
        let status = throttled_status(Some(&prior), Duration::from_secs(17), 2);
        assert_eq!(status.failure_domains, prior.failure_domains);
        let (state, reason, message) = ready(&status);
        assert_eq!(state, condition_status::FALSE);
        assert_eq!(reason, reasons::THROTTLED);
        assert!(message.contains("17s"));
        assert_eq!(status.observed_generation, Some(2));
        assert!(find_condition(&status.conditions, condition_types::PROVIDER_REACHABLE).is_some());
    }

    #[test]
    fn throttling_with_no_prior_status_publishes_nothing_stale() {
        let status = throttled_status(None, Duration::from_secs(1), 1);
        assert!(status.failure_domains.is_empty());
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
