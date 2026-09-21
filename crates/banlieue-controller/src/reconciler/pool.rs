// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VirtualMachinePool` reconciler (roadmap 17, ADR-0046).
//!
//! Thin on purpose: gather a snapshot, call [`super::pool_plan::plan`], apply
//! it, publish counts. Every decision lives in `pool_plan.rs`; every helper
//! below that does not touch the API server is pure and unit-testable.

use std::net::Ipv4Addr;
use std::sync::Arc;

use banlieue_api::banlieue::NetworkInterfaceOverride;
use banlieue_api::banlieue::{
    LABEL_CLAIM, LABEL_POOL, LABEL_POOL_IMAGE_REVISION, PoolAddressing, PoolReadiness, VMImage,
    VirtualMachine, VirtualMachinePool, VirtualMachinePoolStatus, pool_condition_reasons,
    pool_condition_types,
};
use banlieue_api::common::{StaticIpamConfig, condition_types};
use banlieue_provider_sdk::{
    reconciler::{requeue_default, requeue_long, requeue_on_error},
    status::{condition_status, set_condition},
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, OwnerReference};
use k8s_openapi::jiff::Timestamp;
use kube::{
    Resource, ResourceExt,
    api::{Api, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use super::pool_plan::{AddressRange, MemberPhase, MemberView, NewMember, PoolInputs, plan};
use crate::context::Context;
use crate::error::{Error, Result};

const FIELD_MANAGER: &str = "banlieue.io/pool-controller";

pub async fn reconcile(pool: Arc<VirtualMachinePool>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = pool.namespace().ok_or(Error::Missing("namespace"))?;
    let name = pool.name_any();
    let generation = pool.metadata.generation.unwrap_or(0);

    // A pool being deleted takes its unclaimed members with it through
    // ownerReferences + background GC. Claimed members are re-parented to
    // their claim at bind time (see claim.rs), so they survive the pool.
    if pool.metadata.deletion_timestamp.is_some() {
        return Ok(requeue_long());
    }

    let vm_api: Api<VirtualMachine> = Api::namespaced(ctx.client.clone(), &namespace);
    let image_api: Api<VMImage> = Api::all(ctx.client.clone());

    let image = image_api
        .get(&pool.spec.template.spec.image_ref.name)
        .await?;
    let revision = image_revision(&image);

    let members = vm_api
        .list(&ListParams::default().labels(&format!("{LABEL_POOL}={name}")))
        .await?
        .items;

    let now = Timestamp::now();
    let views: Vec<MemberView> = members
        .iter()
        .map(|vm| member_view(vm, pool.spec.readiness, now))
        .collect();

    let inputs = match pool_inputs(&pool, &revision) {
        Ok(i) => i,
        Err(msg) => {
            warn!(pool = %name, %msg, "invalid pool addressing");
            let existing = existing_conditions(&pool);
            publish(
                &ctx,
                &namespace,
                &name,
                generation,
                &pool,
                existing,
                &views,
                &revision,
                Some((pool_condition_reasons::ADDRESS_RANGE_EXHAUSTED, msg)),
            )
            .await?;
            return Ok(requeue_long());
        }
    };

    let decided = plan(&inputs, &views);

    for (member, reason) in &decided.delete {
        info!(pool = %name, %member, ?reason, "deleting pool member");
        match vm_api.delete(member, &DeleteParams::background()).await {
            Ok(_) => {}
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(e) => return Err(Error::Kube(e)),
        }
    }

    for new in &decided.create {
        let vm = build_member(&pool, &revision, new)?;
        info!(pool = %name, address = ?new.address, "creating pool member");
        vm_api.create(&PostParams::default(), &vm).await?;
    }

    // ADR-0046 Decision 3: a pool waiting on a condition nothing publishes
    // must say so. Checked before the capacity reasons because it explains
    // an empty pool, where those two would not fire at all.
    let member_conditions: Vec<Vec<Condition>> = members
        .iter()
        .map(|m| {
            m.status
                .as_ref()
                .map(|st| st.conditions.clone())
                .unwrap_or_default()
        })
        .collect();
    let want = readiness_condition_type(pool.spec.readiness);
    let blocked = if readiness_signal_absent(want, &member_conditions) {
        Some((
            pool_condition_reasons::READINESS_SIGNAL_ABSENT,
            format!(
                "no member has published the {want:?} condition; \
                 spec.readiness selects a signal nothing is setting"
            ),
        ))
    } else if decided.blocked_on_addresses > 0 {
        Some((
            pool_condition_reasons::ADDRESS_RANGE_EXHAUSTED,
            format!(
                "{} member(s) not created: address range exhausted",
                decided.blocked_on_addresses
            ),
        ))
    } else if decided.blocked_on_capacity > 0 {
        Some((
            pool_condition_reasons::MAX_REPLICAS_REACHED,
            format!(
                "{} member(s) not created: maxReplicas reached",
                decided.blocked_on_capacity
            ),
        ))
    } else {
        None
    };
    let existing = existing_conditions(&pool);
    publish(
        &ctx, &namespace, &name, generation, &pool, existing, &views, &revision, blocked,
    )
    .await?;

    // Member Ready transitions arrive through `.owns(VirtualMachine)`; the
    // periodic requeue exists for the time-based rules (provisioning
    // timeout, idle expiry) that no watch event announces.
    Ok(if decided.create.is_empty() && decided.delete.is_empty() {
        requeue_long()
    } else {
        requeue_default()
    })
}

/// Reason for the `Warm` condition.
///
/// `Filling` is the normal not-yet-warm reason and says the pool is making
/// progress. When the readiness signal is one nothing publishes, it is not:
/// the pool will never warm, and reporting `Filling` buries the diagnosis
/// under a reason that means the opposite. `Warm` is the condition an
/// operator reads to decide whether a pool is usable, so the cause belongs
/// there and not only on `Capacity` (ADR-0046 Decision 3).
#[must_use]
pub fn warm_reason(warm: bool, blocked_reason: Option<&'static str>) -> &'static str {
    if warm {
        return pool_condition_reasons::WARM;
    }
    match blocked_reason {
        Some(r) if r == pool_condition_reasons::READINESS_SIGNAL_ABSENT => r,
        _ => pool_condition_reasons::FILLING,
    }
}

/// The Kubernetes condition type a `readiness` value selects.
///
/// The enum's own wire form is that name (ADR-0046), so this is a mapping
/// rather than a translation — kept as a function so the one place that
/// needs `GuestReady` as a string is findable when ADR-0043 adds the
/// constant.
#[must_use]
pub fn readiness_condition_type(readiness: PoolReadiness) -> &'static str {
    match readiness {
        PoolReadiness::GuestReady => "GuestReady",
        PoolReadiness::InfrastructureReady => condition_types::INFRASTRUCTURE_READY,
    }
}

/// Whether **no** member has ever published the condition the pool is
/// waiting on (ADR-0046 Decision 3).
///
/// This is what makes `readiness` being required safe rather than merely
/// strict. A pool set to `GuestReady` before ADR-0043 exists waits forever
/// for a condition nothing publishes, and without this it reports zero warm
/// members and no error — indistinguishable from a slow install.
///
/// Two things are deliberately *not* absent:
/// - the condition present but `False` — that is a member coming up, the
///   normal state of a filling pool;
/// - a pool with no members at all — every pool starts there, and firing on
///   it would teach operators to ignore the condition.
#[must_use]
pub fn readiness_signal_absent(condition_type: &str, member_conditions: &[Vec<Condition>]) -> bool {
    if member_conditions.is_empty() {
        return false;
    }
    !member_conditions
        .iter()
        .any(|cs| cs.iter().any(|c| c.type_ == condition_type))
}

pub fn error_policy(_pool: Arc<VirtualMachinePool>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "VirtualMachinePool reconcile failed");
    requeue_on_error()
}

/// Stable identifier of the image build a member is created from. A rebuilt
/// image gets a new `OSArtifact`, hence a new UID; images with no managed
/// build fall back to the `VMImage`'s own generation. Either form fits a
/// label value.
#[must_use]
pub fn image_revision(image: &VMImage) -> String {
    image
        .status
        .as_ref()
        .and_then(|s| s.build_artifact.as_ref())
        .and_then(|b| b.os_artifact_uid.clone())
        .unwrap_or_else(|| format!("gen-{}", image.metadata.generation.unwrap_or(0)))
}

/// Project a member `VirtualMachine` onto what the planner needs.
#[must_use]
pub fn member_view(vm: &VirtualMachine, readiness: PoolReadiness, now: Timestamp) -> MemberView {
    let wanted = match readiness {
        PoolReadiness::GuestReady => condition_types::GUEST_READY,
        PoolReadiness::InfrastructureReady => condition_types::INFRASTRUCTURE_READY,
    };
    let ready_since = vm.status.as_ref().and_then(|s| {
        s.conditions
            .iter()
            .find(|c| c.type_ == wanted && c.status == condition_status::TRUE)
            .map(|c| c.last_transition_time.0)
    });

    let phase = if vm.metadata.deletion_timestamp.is_some() {
        MemberPhase::Deleting
    } else if vm.labels().contains_key(LABEL_CLAIM) {
        MemberPhase::Claimed
    } else if ready_since.is_some() {
        MemberPhase::Ready
    } else {
        MemberPhase::Provisioning
    };

    let secs_since =
        |t: Timestamp| -> u64 { u64::try_from(now.as_second() - t.as_second()).unwrap_or(0) };

    MemberView {
        name: vm.name_any(),
        phase,
        age_secs: vm
            .metadata
            .creation_timestamp
            .as_ref()
            .map_or(0, |t| secs_since(t.0)),
        ready_for_secs: ready_since.map_or(0, secs_since),
        image_revision: vm
            .labels()
            .get(LABEL_POOL_IMAGE_REVISION)
            .cloned()
            .unwrap_or_default(),
        address: vm
            .spec
            .network_overrides
            .first()
            .and_then(|o| o.static_.address.parse::<Ipv4Addr>().ok()),
    }
}

fn pool_inputs(
    pool: &VirtualMachinePool,
    revision: &str,
) -> std::result::Result<PoolInputs, String> {
    let address_range = match &pool.spec.addressing {
        None => None,
        Some(a) => Some(AddressRange {
            start: a
                .range_start
                .parse()
                .map_err(|e| format!("addressing.rangeStart {:?}: {e}", a.range_start))?,
            end: a
                .range_end
                .parse()
                .map_err(|e| format!("addressing.rangeEnd {:?}: {e}", a.range_end))?,
        }),
    };
    Ok(PoolInputs {
        warm_replicas: pool.spec.warm_replicas,
        max_replicas: pool.spec.max_replicas,
        max_surge: pool.spec.max_surge,
        provisioning_timeout_secs: pool.spec.provisioning_timeout_seconds,
        max_idle_secs: pool.spec.max_idle_seconds,
        recycle_on_image_change: pool.spec.recycle_on_image_change,
        current_image_revision: revision.to_string(),
        address_range,
    })
}

/// Build one member from the pool's template. `generateName` rather than an
/// index: members are cattle, and an index invites someone to depend on it.
pub fn build_member(
    pool: &VirtualMachinePool,
    revision: &str,
    new: &NewMember,
) -> Result<VirtualMachine> {
    let pool_name = pool.name_any();
    let mut labels = pool.spec.template.labels.clone();
    labels.insert(LABEL_POOL.to_string(), pool_name.clone());
    labels.insert(LABEL_POOL_IMAGE_REVISION.to_string(), revision.to_string());

    let mut spec = pool.spec.template.spec.clone();
    if let (Some(addressing), Some(address)) = (&pool.spec.addressing, new.address) {
        spec.network_overrides
            .retain(|o| o.name != addressing.interface);
        spec.network_overrides
            .insert(0, stamp_address(addressing, address));
    }

    let owner: OwnerReference = pool
        .controller_owner_ref(&())
        .ok_or(Error::Missing("metadata.uid"))?;

    Ok(VirtualMachine {
        metadata: ObjectMeta {
            generate_name: Some(format!("{pool_name}-")),
            namespace: pool.namespace(),
            labels: Some(labels),
            annotations: Some(pool.spec.template.annotations.clone()),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec,
        status: None,
    })
}

fn stamp_address(a: &PoolAddressing, address: Ipv4Addr) -> NetworkInterfaceOverride {
    NetworkInterfaceOverride {
        name: a.interface.clone(),
        static_: StaticIpamConfig {
            address: address.to_string(),
            prefix: a.prefix,
            gateway: a.gateway.clone(),
            nameservers: a.nameservers.clone(),
            domain: a.domain.clone(),
        },
    }
}

fn existing_conditions(pool: &VirtualMachinePool) -> Vec<Condition> {
    pool.status
        .as_ref()
        .map(|s| s.conditions.clone())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
async fn publish(
    ctx: &Context,
    namespace: &str,
    name: &str,
    generation: i64,
    pool: &VirtualMachinePool,
    mut conditions: Vec<Condition>,
    views: &[MemberView],
    revision: &str,
    blocked: Option<(&'static str, String)>,
) -> Result<()> {
    let count = |p: MemberPhase| -> u32 {
        u32::try_from(views.iter().filter(|m| m.phase == p).count()).unwrap_or(u32::MAX)
    };
    let available = count(MemberPhase::Ready);
    let provisioning = count(MemberPhase::Provisioning);
    let claimed = count(MemberPhase::Claimed);

    // `set_condition` only moves lastTransitionTime on a real status flip,
    // same as every other banlieue status writer.
    let warm = available >= pool.spec.warm_replicas;
    set_condition(
        &mut conditions,
        pool_condition_types::WARM,
        if warm {
            condition_status::TRUE
        } else {
            condition_status::FALSE
        },
        warm_reason(warm, blocked.as_ref().map(|(r, _)| *r)),
        format!(
            "{available}/{} warm member(s) available",
            pool.spec.warm_replicas
        ),
        generation,
    );
    match blocked {
        Some((reason, message)) => set_condition(
            &mut conditions,
            pool_condition_types::CAPACITY,
            condition_status::FALSE,
            reason,
            message,
            generation,
        ),
        None => set_condition(
            &mut conditions,
            pool_condition_types::CAPACITY,
            condition_status::TRUE,
            pool_condition_reasons::WARM,
            "no creation blocked",
            generation,
        ),
    }

    let status = VirtualMachinePoolStatus {
        replicas: available + provisioning + claimed,
        available,
        provisioning,
        claimed,
        image_revision: Some(revision.to_string()),
        conditions,
        observed_generation: Some(generation),
    };

    let api: Api<VirtualMachinePool> = Api::namespaced(ctx.client.clone(), namespace);
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachinePool",
            "status": status,
        })),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "pool_tests.rs"]
mod pool_tests;
