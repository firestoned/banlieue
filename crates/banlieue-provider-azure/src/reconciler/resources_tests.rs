// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `resources.rs`: ownership by tag and the orphan sweep,
//! against `FakeArm`.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    use banlieue_azure::fake::FakeArm;
    use banlieue_azure::{NicRequest, OwnershipTags};

    use super::super::*;
    use crate::test_support::{HOME, LOCATION, PROVIDER, RESOURCE_GROUP, SUBNET_ID};

    const DEAD: &str = "11111111-1111-4111-8111-111111111111";
    const LIVE: &str = "22222222-2222-4222-8222-222222222222";
    const AN_HOUR: Duration = Duration::from_secs(3600);

    fn provider_tag() -> String {
        format!("{HOME}.{PROVIDER}")
    }

    fn tags(uid: &str, kind: &str, provider_name: &str) -> BTreeMap<String, String> {
        OwnershipTags::new(uid, kind, HOME, provider_name).to_map()
    }

    fn tags_in(uid: &str, namespace: &str) -> BTreeMap<String, String> {
        OwnershipTags::new(uid, OWNER_KIND, HOME, PROVIDER)
            .with_owner_namespace(namespace)
            .to_map()
    }

    /// A home-only provider lists machines in its home namespace alone, so a
    /// tenant machine's UID is missing from its listing. Its resources must
    /// survive (a provider switched from selector mode to home-only would
    /// otherwise delete live tenant VMs); so must a resource with no
    /// namespace tag. The home namespace's own orphan is still swept.
    #[tokio::test]
    async fn a_home_only_sweep_never_judges_another_namespaces_resources() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fake");
        nic(&fake, "home-orphan", tags_in(DEAD, HOME)).await;
        nic(&fake, "tenant-nic", tags_in(DEAD, "team-a")).await;
        nic(&fake, "no-namespace-tag", tags(DEAD, OWNER_KIND, PROVIDER)).await;

        let report = sweep(
            &fake,
            RESOURCE_GROUP,
            &provider_tag(),
            &BTreeSet::new(),
            Some(HOME),
            later(),
        )
        .await
        .expect("sweep");

        assert_eq!(report.deleted, ["home-orphan"]);
        assert_eq!(
            report.found, 1,
            "another namespace's resources are not found"
        );
        assert_eq!(
            nic_names(&fake).await,
            BTreeSet::from(["tenant-nic".to_string(), "no-namespace-tag".to_string()])
        );
    }

    /// A shared provider lists machines cluster-wide, so every namespace's
    /// orphans are in scope.
    #[tokio::test]
    async fn a_cluster_wide_sweep_covers_every_namespace() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fake");
        nic(&fake, "tenant-orphan", tags_in(DEAD, "team-a")).await;
        nic(&fake, "tenant-live", tags_in(LIVE, "team-a")).await;
        let live = BTreeSet::from([LIVE.to_string()]);

        let report = sweep(&fake, RESOURCE_GROUP, &provider_tag(), &live, None, later())
            .await
            .expect("sweep");

        assert_eq!(report.deleted, ["tenant-orphan"]);
    }

    fn resource(
        name: &str,
        kind: &str,
        tags: BTreeMap<String, String>,
        age: Duration,
    ) -> TaggedResource {
        let created = jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::try_from(age).expect("duration"))
            .expect("timestamp");
        TaggedResource {
            id: format!("/subscriptions/s/resourceGroups/{RESOURCE_GROUP}/providers/{kind}/{name}"),
            name: name.into(),
            resource_type: kind.into(),
            tags,
            created_time: Some(created),
        }
    }

    async fn nic(fake: &FakeArm, name: &str, tags: BTreeMap<String, String>) {
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

    async fn nic_names(fake: &FakeArm) -> BTreeSet<String> {
        fake.list_resources(RESOURCE_GROUP)
            .await
            .expect("list")
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    fn later() -> jiff::Timestamp {
        jiff::Timestamp::now()
            .checked_add(jiff::SignedDuration::try_from(AN_HOUR).expect("duration"))
            .expect("timestamp")
    }

    #[test]
    fn a_kind_is_read_from_the_arm_type_in_any_case() {
        let t = BTreeMap::new();
        assert_eq!(
            OwnedKind::of(&resource(
                "a",
                "microsoft.compute/VIRTUALMACHINES",
                t.clone(),
                AN_HOUR
            )),
            Some(OwnedKind::Vm)
        );
        assert_eq!(
            OwnedKind::of(&resource("a", RESOURCE_TYPE_NIC, t.clone(), AN_HOUR)),
            Some(OwnedKind::Nic)
        );
        assert_eq!(
            OwnedKind::of(&resource("a", RESOURCE_TYPE_DISK, t.clone(), AN_HOUR)),
            Some(OwnedKind::Disk)
        );
        assert_eq!(
            OwnedKind::of(&resource("a", "Microsoft.Compute/galleries", t, AN_HOUR)),
            None
        );
    }

    #[test]
    fn a_delete_is_tracked_as_the_matching_action() {
        assert_eq!(
            OwnedKind::Vm.delete_action(),
            AzureOperationAction::DeleteVm
        );
        assert_eq!(
            OwnedKind::Nic.delete_action(),
            AzureOperationAction::DeleteNic
        );
        assert_eq!(
            OwnedKind::Disk.delete_action(),
            AzureOperationAction::DeleteDisk
        );
    }

    /// Ownership is the UID tag only; the VM comes first.
    #[test]
    fn owned_resources_are_found_by_uid_tag_vm_first() {
        let mine = tags(DEAD, OWNER_KIND, PROVIDER);
        let resources = vec![
            resource("banlieue-x-nic", RESOURCE_TYPE_NIC, mine.clone(), AN_HOUR),
            resource("banlieue-x", RESOURCE_TYPE_VM, mine.clone(), AN_HOUR),
            resource(
                "other-nic",
                RESOURCE_TYPE_NIC,
                tags(LIVE, OWNER_KIND, PROVIDER),
                AN_HOUR,
            ),
            // Named like ours, but untagged: never ours.
            resource(
                "banlieue-x-disk",
                RESOURCE_TYPE_DISK,
                BTreeMap::new(),
                AN_HOUR,
            ),
        ];
        let owned: Vec<&str> = owned_by(&resources, DEAD)
            .iter()
            .map(|(_, r)| r.name.as_str())
            .collect();
        assert_eq!(owned, ["banlieue-x", "banlieue-x-nic"]);
    }

    #[test]
    fn sweep_candidates_are_only_old_tagged_orphans_of_this_provider() {
        let resources = vec![
            resource(
                "orphan-nic",
                RESOURCE_TYPE_NIC,
                tags(DEAD, OWNER_KIND, PROVIDER),
                AN_HOUR,
            ),
            resource(
                "orphan-vm",
                RESOURCE_TYPE_VM,
                tags(DEAD, OWNER_KIND, PROVIDER),
                AN_HOUR,
            ),
            resource(
                "live-nic",
                RESOURCE_TYPE_NIC,
                tags(LIVE, OWNER_KIND, PROVIDER),
                AN_HOUR,
            ),
            resource(
                "young-nic",
                RESOURCE_TYPE_NIC,
                tags(DEAD, OWNER_KIND, PROVIDER),
                Duration::from_secs(60),
            ),
            resource(
                "foreign-nic",
                RESOURCE_TYPE_NIC,
                tags(DEAD, OWNER_KIND, "az-west"),
                AN_HOUR,
            ),
            resource("untagged-nic", RESOURCE_TYPE_NIC, BTreeMap::new(), AN_HOUR),
            // A VMImage's gallery version: another reconciler's to clean up.
            resource(
                "image-version",
                RESOURCE_TYPE_DISK,
                tags(DEAD, "VMImage", PROVIDER),
                AN_HOUR,
            ),
            resource(
                "gallery",
                "Microsoft.Compute/galleries",
                tags(DEAD, OWNER_KIND, PROVIDER),
                AN_HOUR,
            ),
        ];
        let live = BTreeSet::from([LIVE.to_string()]);
        let names: Vec<&str> = sweep_candidates(
            &resources,
            &provider_tag(),
            &live,
            None,
            jiff::Timestamp::now(),
            SWEEP_GRACE,
        )
        .iter()
        .map(|(_, r)| r.name.as_str())
        .collect();
        assert_eq!(names, ["orphan-vm", "orphan-nic"]);
    }

    /// A NIC left behind by a refused VM create, owned by a machine that is
    /// gone, is swept; a live machine's, another Provider's and an untagged
    /// NIC are not touched.
    #[tokio::test]
    async fn the_sweep_deletes_only_this_providers_orphans() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fake");
        nic(&fake, "orphan-nic", tags(DEAD, OWNER_KIND, PROVIDER)).await;
        nic(&fake, "live-nic", tags(LIVE, OWNER_KIND, PROVIDER)).await;
        nic(&fake, "foreign-nic", tags(DEAD, OWNER_KIND, "az-west")).await;
        nic(&fake, "untagged-nic", BTreeMap::new()).await;
        let live = BTreeSet::from([LIVE.to_string()]);

        let report = sweep(&fake, RESOURCE_GROUP, &provider_tag(), &live, None, later())
            .await
            .expect("sweep");

        assert_eq!(report.deleted, ["orphan-nic"]);
        assert!(report.failed.is_empty());
        assert_eq!(
            nic_names(&fake).await,
            BTreeSet::from([
                "live-nic".to_string(),
                "foreign-nic".to_string(),
                "untagged-nic".to_string()
            ])
        );
    }

    /// Within the grace period a create may still be in flight.
    #[tokio::test]
    async fn the_sweep_spares_a_resource_inside_the_grace_period() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fake");
        nic(&fake, "orphan-nic", tags(DEAD, OWNER_KIND, PROVIDER)).await;
        let report = sweep(
            &fake,
            RESOURCE_GROUP,
            &provider_tag(),
            &BTreeSet::new(),
            None,
            jiff::Timestamp::now(),
        )
        .await
        .expect("sweep");
        assert!(report.deleted.is_empty());
        assert!(nic_names(&fake).await.contains("orphan-nic"));
    }

    #[tokio::test]
    async fn the_sweep_is_paced() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fake");
        for i in 0..=MAX_SWEEP_DELETES_PER_PASS {
            nic(
                &fake,
                &format!("orphan-{i}"),
                tags(DEAD, OWNER_KIND, PROVIDER),
            )
            .await;
        }
        let report = sweep(
            &fake,
            RESOURCE_GROUP,
            &provider_tag(),
            &BTreeSet::new(),
            None,
            later(),
        )
        .await
        .expect("sweep");
        assert_eq!(report.deleted.len(), MAX_SWEEP_DELETES_PER_PASS);
        assert_eq!(report.deferred, 1);
        assert_eq!(
            report.found,
            MAX_SWEEP_DELETES_PER_PASS + 1,
            "found counts the deferred orphan too (the sweep metric)"
        );
    }

    /// ARM throttles a burst of deletes; the pass stops and resumes later.
    #[tokio::test]
    async fn a_throttled_sweep_stops_and_says_so() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fake");
        nic(&fake, "orphan-a", tags(DEAD, OWNER_KIND, PROVIDER)).await;
        nic(&fake, "orphan-b", tags(DEAD, OWNER_KIND, PROVIDER)).await;
        fake.set_delete_burst(1);
        let report = sweep(
            &fake,
            RESOURCE_GROUP,
            &provider_tag(),
            &BTreeSet::new(),
            None,
            later(),
        )
        .await
        .expect("sweep");
        assert_eq!(report.deleted, ["orphan-a"]);
        assert!(report.throttled.is_some());
        assert!(nic_names(&fake).await.contains("orphan-b"));
    }

    #[tokio::test]
    async fn deleting_something_already_gone_is_success() {
        let fake = FakeArm::from_fixtures(LOCATION).expect("fake");
        for kind in [OwnedKind::Vm, OwnedKind::Nic, OwnedKind::Disk] {
            assert_eq!(
                delete_owned(&fake, RESOURCE_GROUP, kind, "absent")
                    .await
                    .expect("ok"),
                None
            );
        }
    }
}
