// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Lease-based leader election for banlieue controllers.
//!
//! Uses the standard Kubernetes `coordination.k8s.io/v1.Lease` object as
//! the lock — the same primitive `kube-controller-manager` and CAPI
//! providers use. Only the elected leader runs reconcilers; the loser
//! waits until either the leader exits or the lease expires.
//!
//! The async loop is intentionally thin; all decision logic lives in
//! the pure [`decide_action`] function, which is unit-tested in
//! `leader_tests.rs` without any cluster contact.

use std::time::Duration;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use k8s_openapi::jiff::Timestamp;
use kube::{
    Api, Client,
    api::{Patch, PatchParams, PostParams},
};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

/// Field manager used when patching the Lease.
pub const LEASE_FIELD_MANAGER: &str = "banlieue.io/leader-election";

/// Default lease duration — how long a lease is valid after the last
/// successful renewal. Matches the Kubernetes leader-election default.
pub const DEFAULT_LEASE_DURATION_SECS: u64 = 15;

/// Default renew period — how often the leader patches `renewTime`.
/// Must be strictly less than `lease_duration`.
pub const DEFAULT_RENEW_PERIOD_SECS: u64 = 5;

/// Default retry period — how often a follower polls for the lease.
pub const DEFAULT_RETRY_PERIOD_SECS: u64 = 2;

/// Static lease-spec field: the duration we ask other candidates to
/// honor before forcibly acquiring. Encoded into the Lease for the
/// benefit of other controllers (we re-read our config locally).
const LEASE_SPEC_DURATION_FIELD_SECS: i32 = DEFAULT_LEASE_DURATION_SECS as i32;

/// Configuration for one leader-election loop.
#[derive(Debug, Clone)]
pub struct LeaderConfig {
    /// Namespace the Lease object lives in.
    pub namespace: String,
    /// Lease object name. Must be unique per controller binary.
    pub lease_name: String,
    /// Holder identity written into the Lease. Convention: pod name, or
    /// hostname when running outside a cluster.
    pub identity: String,
    /// How long the lease is valid after the last successful renewal.
    pub lease_duration: Duration,
    /// How often the leader renews while it holds the lease.
    pub renew_period: Duration,
    /// How often followers re-poll for an opportunity.
    pub retry_period: Duration,
}

impl LeaderConfig {
    /// Sensible defaults matching kube-controller-manager.
    pub fn new(namespace: impl Into<String>, lease_name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            lease_name: lease_name.into(),
            identity: Self::default_identity(),
            lease_duration: Duration::from_secs(DEFAULT_LEASE_DURATION_SECS),
            renew_period: Duration::from_secs(DEFAULT_RENEW_PERIOD_SECS),
            retry_period: Duration::from_secs(DEFAULT_RETRY_PERIOD_SECS),
        }
    }

    /// Validate the configuration before launching the loop. Catches
    /// programmer mistakes (zero durations, renew >= lease) early
    /// instead of producing pathological lease behaviour at runtime.
    ///
    /// # Errors
    /// Returns [`Error::Missing`] with a descriptive label for the
    /// first invalid field encountered.
    pub fn validate(&self) -> Result<()> {
        if self.identity.is_empty() {
            return Err(Error::Missing("LeaderConfig.identity"));
        }
        if self.lease_duration.is_zero() {
            return Err(Error::Missing("LeaderConfig.lease_duration"));
        }
        if self.renew_period.is_zero() {
            return Err(Error::Missing("LeaderConfig.renew_period"));
        }
        if self.retry_period.is_zero() {
            return Err(Error::Missing("LeaderConfig.retry_period"));
        }
        if self.renew_period >= self.lease_duration {
            return Err(Error::Missing(
                "LeaderConfig.renew_period must be < lease_duration",
            ));
        }
        Ok(())
    }

    /// Best-effort identity string for the running process.
    ///
    /// Order of preference:
    /// 1. `POD_NAME` env var (set via the downward API on K8s deployments).
    /// 2. `HOSTNAME` env var.
    /// 3. The literal string `"unknown"` (callers should override this
    ///    via [`LeaderConfig::identity`]).
    pub fn default_identity() -> String {
        std::env::var("POD_NAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

/// The action a candidate should take given the current state of the
/// Lease in the cluster. Pure — derived from `(now, lease, config)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAction {
    /// No live holder. Create or patch the Lease so we hold it.
    AcquireNew,
    /// We are the holder. Patch `renewTime` to extend the lease.
    Renew,
    /// Someone else holds a still-valid lease. Sleep and re-poll.
    Wait,
    /// Previous holder has not renewed within `lease_duration`. Patch
    /// the Lease so we hold it; bump `leaseTransitions`.
    TakeOver,
}

/// Pure decision: given the current time, the most recently observed
/// Lease (if any), and our config, what should we do next?
///
/// Boundary semantics: a lease whose `renewTime + lease_duration`
/// exactly equals `now` is considered *still alive* — strict-less-than
/// would race the holder's renewal cycle and cause spurious takeovers.
pub fn decide_action(now: Timestamp, lease: Option<&Lease>, cfg: &LeaderConfig) -> LeaseAction {
    let Some(lease) = lease else {
        return LeaseAction::AcquireNew;
    };

    let Some(spec) = lease.spec.as_ref() else {
        return LeaseAction::AcquireNew;
    };

    let holder = spec.holder_identity.as_deref().unwrap_or("");
    if holder.is_empty() {
        return LeaseAction::AcquireNew;
    }

    if holder == cfg.identity {
        return LeaseAction::Renew;
    }

    let Some(MicroTime(renew_time)) = spec.renew_time else {
        return LeaseAction::TakeOver;
    };

    // Compare in whole seconds — sub-second resolution adds nothing to a
    // leader-election decision (the lease duration is measured in seconds
    // by Kubernetes itself) and side-steps jiff Span arithmetic.
    let expiry_secs = renew_time.as_second() + cfg.lease_duration.as_secs() as i64;
    if now.as_second() <= expiry_secs {
        LeaseAction::Wait
    } else {
        LeaseAction::TakeOver
    }
}

/// Run the leader-election loop until we successfully acquire the
/// lease. Returns once the lease is held; the caller is responsible for
/// keeping it renewed (via [`renew_forever`]) and surrendering it on
/// shutdown.
///
/// In Phase 1A iteration 4 the controller does the simplest possible
/// thing: `acquire_or_wait` → spawn renewal task → run reconcilers. If
/// renewal ever fails terminally the process exits; the Deployment
/// controller restarts the pod, which re-enters this function.
///
/// # Errors
/// Returns any underlying [`kube::Error`] from a Lease GET / CREATE /
/// PATCH call. Transient errors are logged and retried; persistent
/// errors bubble up.
pub async fn acquire_or_wait(client: Client, cfg: &LeaderConfig) -> Result<()> {
    cfg.validate()?;
    let api: Api<Lease> = Api::namespaced(client, &cfg.namespace);

    info!(
        namespace = %cfg.namespace,
        lease = %cfg.lease_name,
        identity = %cfg.identity,
        "leader election: candidate started"
    );

    loop {
        let current = fetch_lease(&api, &cfg.lease_name).await?;
        let action = decide_action(Timestamp::now(), current.as_ref(), cfg);
        debug!(?action, "leader election step");

        match action {
            LeaseAction::AcquireNew => {
                if current.is_none() {
                    create_owned_lease(&api, cfg).await?;
                } else {
                    patch_take_over(&api, cfg, current.as_ref()).await?;
                }
                info!(identity = %cfg.identity, "leader election: acquired lease");
                return Ok(());
            }
            LeaseAction::Renew => {
                renew_once(&api, cfg).await?;
                info!(identity = %cfg.identity, "leader election: re-renewed existing lease");
                return Ok(());
            }
            LeaseAction::TakeOver => {
                patch_take_over(&api, cfg, current.as_ref()).await?;
                info!(
                    identity = %cfg.identity,
                    "leader election: took over expired lease"
                );
                return Ok(());
            }
            LeaseAction::Wait => {
                sleep(cfg.retry_period).await;
            }
        }
    }
}

/// Renew the lease on `cfg.renew_period`. Returns only on a terminal
/// renewal failure (we no longer hold the lease).
///
/// On a transient error the loop retries on the retry period; on a
/// persistent loss-of-holder the function returns so the caller can
/// exit the process.
pub async fn renew_forever(client: Client, cfg: LeaderConfig) -> Result<()> {
    let api: Api<Lease> = Api::namespaced(client, &cfg.namespace);
    loop {
        sleep(cfg.renew_period).await;
        match renew_once(&api, &cfg).await {
            Ok(()) => {
                debug!(identity = %cfg.identity, "lease renewed");
            }
            Err(Error::Kube(e)) => {
                warn!(error = %e, "lease renew failed, re-evaluating");
                let current = fetch_lease(&api, &cfg.lease_name).await?;
                let action = decide_action(Timestamp::now(), current.as_ref(), &cfg);
                if action != LeaseAction::Renew {
                    error!(
                        ?action,
                        identity = %cfg.identity,
                        "lease lost — caller should exit"
                    );
                    return Err(Error::Missing("leader lease lost"));
                }
            }
            Err(other) => return Err(other),
        }
    }
}

async fn fetch_lease(api: &Api<Lease>, name: &str) -> Result<Option<Lease>> {
    Ok(api.get_opt(name).await?)
}

async fn create_owned_lease(api: &Api<Lease>, cfg: &LeaderConfig) -> Result<()> {
    let now = Timestamp::now();
    let lease = Lease {
        metadata: ObjectMeta {
            name: Some(cfg.lease_name.clone()),
            namespace: Some(cfg.namespace.clone()),
            ..Default::default()
        },
        spec: Some(LeaseSpec {
            acquire_time: Some(MicroTime(now)),
            renew_time: Some(MicroTime(now)),
            holder_identity: Some(cfg.identity.clone()),
            lease_duration_seconds: Some(LEASE_SPEC_DURATION_FIELD_SECS),
            lease_transitions: Some(1),
            ..Default::default()
        }),
    };
    api.create(&PostParams::default(), &lease).await?;
    Ok(())
}

async fn patch_take_over(
    api: &Api<Lease>,
    cfg: &LeaderConfig,
    current: Option<&Lease>,
) -> Result<()> {
    let now = MicroTime(Timestamp::now());
    let prev_transitions = current
        .and_then(|l| l.spec.as_ref())
        .and_then(|s| s.lease_transitions)
        .unwrap_or(0);

    let patch = serde_json::json!({
        "apiVersion": "coordination.k8s.io/v1",
        "kind": "Lease",
        "metadata": {
            "name": cfg.lease_name,
            "namespace": cfg.namespace,
        },
        "spec": {
            "acquireTime": now,
            "renewTime": now,
            "holderIdentity": cfg.identity,
            "leaseDurationSeconds": LEASE_SPEC_DURATION_FIELD_SECS,
            "leaseTransitions": prev_transitions + 1,
        },
    });

    api.patch(
        &cfg.lease_name,
        &PatchParams::apply(LEASE_FIELD_MANAGER).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

async fn renew_once(api: &Api<Lease>, cfg: &LeaderConfig) -> Result<()> {
    let now = MicroTime(Timestamp::now());
    let patch = serde_json::json!({
        "apiVersion": "coordination.k8s.io/v1",
        "kind": "Lease",
        "metadata": {
            "name": cfg.lease_name,
            "namespace": cfg.namespace,
        },
        "spec": {
            "renewTime": now,
            "holderIdentity": cfg.identity,
            "leaseDurationSeconds": LEASE_SPEC_DURATION_FIELD_SECS,
        },
    });
    api.patch(
        &cfg.lease_name,
        &PatchParams::apply(LEASE_FIELD_MANAGER).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "leader_tests.rs"]
mod leader_tests;
