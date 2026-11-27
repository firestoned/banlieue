// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `instances.rs`: what the orphan sweep may terminate, and a
//! sweep pass against `FakeEc2`.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    use banlieue_aws::FakeEc2;
    use banlieue_aws::types::InstanceState;

    use super::super::*;
    use crate::test_support::{HOME, PROVIDER, launch_owned};

    const KEY: &str = "banlieue-system.ec2-cc";
    const TENANT: &str = "team-a";
    const UID_LIVE: &str = "0f3c9a1e-0000-4000-8000-000000000001";
    const UID_GONE: &str = "0f3c9a1e-0000-4000-8000-000000000002";
    const HOUR: Duration = Duration::from_secs(3600);

    fn later(by: Duration) -> jiff::Timestamp {
        jiff::Timestamp::now() + by
    }

    fn instance(id: &str, uid: &str, namespace: &str, state: InstanceState) -> Instance {
        Instance {
            instance_id: id.into(),
            state,
            private_ip: None,
            private_dns_name: None,
            zone_name: "ca-central-1a".into(),
            launch_time: Some(jiff::Timestamp::now()),
            tags: BTreeMap::from([
                (TAG_OWNER_UID.to_string(), uid.to_string()),
                (TAG_OWNER_KIND.to_string(), OWNER_KIND.to_string()),
                (TAG_PROVIDER.to_string(), KEY.to_string()),
                (TAG_OWNER_NAMESPACE.to_string(), namespace.to_string()),
            ]),
        }
    }

    fn live() -> BTreeSet<String> {
        BTreeSet::from([UID_LIVE.to_string()])
    }

    #[test]
    fn the_constants_are_named_for_what_they_bound() {
        assert_eq!(OWNER_KIND, "EC2Machine");
        assert!(SWEEP_GRACE > SWEEP_INTERVAL);
        assert_eq!(KEY, format!("{HOME}.{PROVIDER}"));
    }

    #[test]
    fn only_an_old_running_instance_of_a_gone_owner_is_a_candidate() {
        let orphan = instance(
            "i-0000000000000000a",
            UID_GONE,
            HOME,
            InstanceState::Running,
        );
        let alive = instance(
            "i-0000000000000000b",
            UID_LIVE,
            HOME,
            InstanceState::Running,
        );
        let gone = instance(
            "i-0000000000000000c",
            UID_GONE,
            HOME,
            InstanceState::ShuttingDown,
        );
        let mut other_provider = instance(
            "i-0000000000000000d",
            UID_GONE,
            HOME,
            InstanceState::Running,
        );
        other_provider
            .tags
            .insert(TAG_PROVIDER.into(), "banlieue-system.ec2-other".into());
        let mut image = instance(
            "i-0000000000000000e",
            UID_GONE,
            HOME,
            InstanceState::Running,
        );
        image.tags.insert(TAG_OWNER_KIND.into(), "VMImage".into());
        let mut no_launch = instance(
            "i-0000000000000000f",
            UID_GONE,
            HOME,
            InstanceState::Stopped,
        );
        no_launch.launch_time = None;
        let all = [orphan, alive, gone, other_provider, image, no_launch];

        let ids: Vec<&str> = sweep_candidates(&all, KEY, &live(), None, later(HOUR), SWEEP_GRACE)
            .iter()
            .map(|i| i.instance_id.as_str())
            .collect();
        assert_eq!(ids, ["i-0000000000000000a"]);
        assert!(
            sweep_candidates(
                &all,
                KEY,
                &live(),
                None,
                jiff::Timestamp::now(),
                SWEEP_GRACE
            )
            .is_empty(),
            "a fresh instance is inside its grace"
        );
    }

    /// ADR-0158 Decision 4, amended: a home-only provider judges only its
    /// home namespace's instances, and never one without the namespace tag.
    #[test]
    fn a_home_only_sweep_never_judges_another_namespace() {
        let home = instance(
            "i-0000000000000000a",
            UID_GONE,
            HOME,
            InstanceState::Running,
        );
        let tenant = instance(
            "i-0000000000000000b",
            UID_GONE,
            TENANT,
            InstanceState::Running,
        );
        let mut untagged = instance(
            "i-0000000000000000c",
            UID_GONE,
            HOME,
            InstanceState::Running,
        );
        untagged.tags.remove(TAG_OWNER_NAMESPACE);
        let all = [home, tenant, untagged];
        let ids = |ns| -> Vec<String> {
            sweep_candidates(&all, KEY, &live(), ns, later(HOUR), SWEEP_GRACE)
                .iter()
                .map(|i| i.instance_id.clone())
                .collect()
        };
        assert_eq!(ids(Some(HOME)), ["i-0000000000000000a"]);
        assert_eq!(ids(None).len(), 3);
    }

    #[test]
    fn owned_instances_go_by_the_uid_tag_only() {
        let mine = instance(
            "i-0000000000000000b",
            UID_LIVE,
            HOME,
            InstanceState::Running,
        );
        let other = instance(
            "i-0000000000000000a",
            UID_GONE,
            HOME,
            InstanceState::Running,
        );
        let all = [other, mine];
        let owned = owned_instances(&all, UID_LIVE);
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].instance_id, "i-0000000000000000b");
        assert!(owned_instances(&all, "").is_empty());
    }

    #[tokio::test]
    async fn a_sweep_terminates_orphans_and_leaves_live_machines() {
        let fake = FakeEc2::ca_central_1();
        let orphan = launch_owned(&fake, UID_GONE, HOME).await;
        let alive = launch_owned(&fake, UID_LIVE, HOME).await;
        let report = sweep(&fake, KEY, &live(), Some(HOME), later(HOUR))
            .await
            .expect("sweeps");
        assert_eq!(report.found, 1);
        assert_eq!(report.deleted, [orphan.as_str()]);
        assert!(!report.throttled);
        assert_eq!(fake.peek_state(&orphan), Some(InstanceState::ShuttingDown));
        assert_eq!(fake.peek_state(&alive), Some(InstanceState::Running));
    }

    #[tokio::test]
    async fn a_sweep_inside_the_grace_terminates_nothing() {
        let fake = FakeEc2::ca_central_1();
        let orphan = launch_owned(&fake, UID_GONE, HOME).await;
        let report = sweep(&fake, KEY, &live(), None, jiff::Timestamp::now())
            .await
            .expect("sweeps");
        assert_eq!(report, SweepReport::default());
        assert_eq!(fake.peek_state(&orphan), Some(InstanceState::Running));
    }

    #[tokio::test]
    async fn a_sweep_defers_past_its_per_pass_limit() {
        let fake = FakeEc2::ca_central_1();
        let extra = 2;
        for n in 0..MAX_SWEEP_DELETES_PER_PASS + extra {
            let uid = format!("0f3c9a1e-0000-4000-8000-{:012x}", n + 100);
            launch_owned(&fake, &uid, HOME).await;
        }
        let report = sweep(&fake, KEY, &live(), None, later(HOUR))
            .await
            .expect("sweeps");
        assert_eq!(report.found, MAX_SWEEP_DELETES_PER_PASS + extra);
        assert_eq!(report.deleted.len(), MAX_SWEEP_DELETES_PER_PASS);
        assert_eq!(report.deferred, extra);
    }

    #[tokio::test]
    async fn a_throttled_listing_is_an_error_and_terminates_nothing() {
        let fake = FakeEc2::ca_central_1();
        let orphan = launch_owned(&fake, UID_GONE, HOME).await;
        fake.throttle_next(1);
        let err = sweep(&fake, KEY, &live(), None, later(HOUR))
            .await
            .expect_err("throttled");
        assert!(err.is_throttled());
        assert_eq!(fake.peek_state(&orphan), Some(InstanceState::Running));
    }
}
