// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A controller audits its own progress (ADR-0145).
//!
//! A controller built on a reflector reconciles what the reflector delivers. If
//! the delivery stops, or the work never reaches the reconcile, objects that
//! exist in the API are simply never touched, and nothing in the process says so:
//! the controller keeps reconciling everything else on its timers, `/livez` stays
//! green, and no log line names the object. This module is the check on the
//! outcome rather than on the mechanism.
//!
//! Three things meet here:
//!
//! - the **API**, listed authoritatively (metadata only) over the controller's
//!   served scope;
//! - the **cache**, the controller's own reflector store;
//! - the **ledger**, the UIDs the controller has been invoked for, written by
//!   [`crate::runner::instrument_reconcile`] on every invocation, including one
//!   that returns early.
//!
//! An object older than a grace that is in the API but not in the cache is
//! `missing_from_store`; one that is cached but whose UID is not in the ledger is
//! `never_reconciled`. [`Tracker`] turns a run of bad passes into [`Verdict::Stale`],
//! which the health server reports as `503` on `/livez` so the kubelet restarts the
//! pod. The decisions are pure functions ([`build_observed`], [`evaluate`],
//! [`Tracker::observe`]); only [`run`] touches the cluster.

use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kube::Resource;
use kube::api::{Api, ListParams};
use kube::core::PartialObjectMeta;
use kube::runtime::reflector::Store;
use serde::de::DeserializeOwned;
use tracing::{error, info, warn};

use crate::health::Readiness;
use crate::metrics::Metrics;
use crate::scope::NamespaceGate;

/// `reason` label and report field: in the API, absent from the controller's cache.
pub const REASON_MISSING_FROM_STORE: &str = "missing_from_store";

/// `reason` label and report field: cached, but the controller was never invoked for it.
pub const REASON_NEVER_RECONCILED: &str = "never_reconciled";

/// How many object names one log line carries at most.
pub const LOG_SAMPLE: usize = 20;

/// Page size of the authoritative listing.
const LIST_PAGE: u32 = 500;

/// The timings of an audit (ADR-0145 decisions 2 and 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditConfig {
    /// Time between two passes.
    pub interval: Duration,
    /// An object younger than this is never reported: its watch event and its first
    /// reconcile are still in flight.
    pub grace: Duration,
    /// No pass runs before the process has been up this long, so a freshly started
    /// controller, which reconciles everything at once, never trips it.
    pub startup_grace: Duration,
    /// How long every pass must have been bad before the controller is stale.
    pub stale_after: Duration,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(60),
            grace: Duration::from_secs(120),
            startup_grace: Duration::from_secs(180),
            stale_after: Duration::from_secs(300),
        }
    }
}

impl AuditConfig {
    /// Whether the process has been up long enough for a pass to mean anything.
    #[must_use]
    pub fn past_startup(&self, process_age: Duration) -> bool {
        process_age >= self.startup_grace
    }
}

/// The UIDs an audited controller has been invoked for. Cheap to clone; clones share
/// state.
///
/// A UID, not a name: an object deleted and recreated under the same name is a new
/// object that the controller has not yet been invoked for.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    seen: Arc<Mutex<HashSet<String>>>,
}

impl Ledger {
    /// A poisoned lock still holds a valid set (every write is one insert or one
    /// retain), so it is used as is.
    fn lock(&self) -> MutexGuard<'_, HashSet<String>> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Note that the controller was invoked for `uid`.
    pub fn record(&self, uid: &str) {
        let mut seen = self.lock();
        if !seen.contains(uid) {
            seen.insert(uid.to_owned());
        }
    }

    /// Whether the controller has been invoked for `uid`.
    #[must_use]
    pub fn contains(&self, uid: &str) -> bool {
        self.lock().contains(uid)
    }

    /// How many UIDs are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no UID is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Drop every UID not in `live`, so deleted objects do not accumulate. Returns how
    /// many were dropped.
    pub fn retain(&self, live: &HashSet<String>) -> usize {
        let mut seen = self.lock();
        let before = seen.len();
        seen.retain(|uid| live.contains(uid));
        before - seen.len()
    }
}

/// One object from the authoritative listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The object's namespace; empty for a cluster-scoped kind.
    pub namespace: String,
    /// The object's name.
    pub name: String,
    /// The object's UID.
    pub uid: String,
    /// Time since the object was created.
    pub age: Duration,
}

/// What the audit knows about one object at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The object's namespace.
    pub namespace: String,
    /// The object's name.
    pub name: String,
    /// The object's UID.
    pub uid: String,
    /// Time since the object was created.
    pub age: Duration,
    /// Whether the controller's namespace gate serves the object's namespace.
    pub served: bool,
    /// Whether the object is in the controller's cache.
    pub in_store: bool,
    /// Whether the controller has been invoked for the object's UID.
    pub invoked: bool,
}

/// Join the listing with the cache, the ledger and the gate. Pure.
///
/// # Arguments
/// * `listed` - the authoritative listing.
/// * `store_uids` - the UIDs in the controller's cache.
/// * `ledger` - the controller's ledger.
/// * `serves` - whether the controller serves a namespace.
pub fn build_observed(
    listed: &[Listed],
    store_uids: &HashSet<String>,
    ledger: &Ledger,
    serves: impl Fn(&str) -> bool,
) -> Vec<Observed> {
    listed
        .iter()
        .map(|l| Observed {
            namespace: l.namespace.clone(),
            name: l.name.clone(),
            uid: l.uid.clone(),
            age: l.age,
            served: serves(&l.namespace),
            in_store: store_uids.contains(&l.uid),
            invoked: ledger.contains(&l.uid),
        })
        .collect()
}

/// The outcome of one pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// `namespace/name` of objects in the API but not in the cache, sorted.
    pub missing_from_store: Vec<String>,
    /// `namespace/name` of cached objects the controller was never invoked for, sorted.
    pub never_reconciled: Vec<String>,
    /// The age of the oldest object reported.
    pub oldest: Duration,
}

impl Report {
    /// Whether nothing is wrong.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.total() == 0
    }

    /// How many objects are reported (each once).
    #[must_use]
    pub fn total(&self) -> usize {
        self.missing_from_store.len() + self.never_reconciled.len()
    }

    /// At most `limit` names, the cache's misses first, for a log line.
    #[must_use]
    pub fn sample(&self, limit: usize) -> Vec<String> {
        self.missing_from_store
            .iter()
            .chain(&self.never_reconciled)
            .take(limit)
            .cloned()
            .collect()
    }
}

/// Decide what is wrong. Pure.
///
/// An object is ignored if its namespace is not served (not reconciling it is the
/// gate's point) or if it is no older than `grace`. A missing cache entry is reported
/// in preference to a missing invocation: an object that is not cached cannot be
/// invoked, so it is reported once, under the upstream cause.
#[must_use]
pub fn evaluate(observed: &[Observed], grace: Duration) -> Report {
    let mut report = Report::default();
    for o in observed {
        if !o.served || o.age <= grace {
            continue;
        }
        let key = format!("{}/{}", o.namespace, o.name);
        if !o.in_store {
            report.missing_from_store.push(key);
        } else if !o.invoked {
            report.never_reconciled.push(key);
        } else {
            continue;
        }
        report.oldest = report.oldest.max(o.age);
    }
    report.missing_from_store.sort();
    report.never_reconciled.sort();
    report
}

/// What the audit concludes about the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The last pass was clean.
    Healthy,
    /// Bad, but not for long enough to act on.
    Degraded,
    /// Every pass for the stale threshold was bad: fail liveness.
    Stale,
}

/// Turns a run of passes into a [`Verdict`].
#[derive(Debug, Default)]
pub struct Tracker {
    failing_since: Option<Instant>,
}

impl Tracker {
    /// Record one pass. A clean pass resets the window; a bad one opens it if it is not
    /// open, and the controller is stale once the window has lasted `stale_after`.
    pub fn observe(&mut self, clean: bool, now: Instant, stale_after: Duration) -> Verdict {
        if clean {
            self.failing_since = None;
            return Verdict::Healthy;
        }
        let since = *self.failing_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= stale_after {
            Verdict::Stale
        } else {
            Verdict::Degraded
        }
    }
}

/// Seconds since the Unix epoch, now. A clock before the epoch reads as zero.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The authoritative listing, metadata only, every page.
///
/// # Errors
/// Any page failing to list.
async fn list_all<K>(api: &Api<K>) -> Result<Vec<Listed>, kube::Error>
where
    K: Resource + Clone + DeserializeOwned + Debug,
{
    let now = now_secs();
    let mut out = Vec::new();
    let mut params = ListParams::default().limit(LIST_PAGE);
    loop {
        let page = api.list_metadata(&params).await?;
        let next = page.metadata.continue_.clone().filter(|c| !c.is_empty());
        out.extend(page.items.iter().filter_map(|item| listed(item, now)));
        match next {
            Some(token) => params = params.continue_token(&token),
            None => return Ok(out),
        }
    }
}

/// One listed object, if it has a UID.
fn listed<K: Resource>(item: &PartialObjectMeta<K>, now: i64) -> Option<Listed> {
    let uid = item.metadata.uid.clone()?;
    let age = item
        .metadata
        .creation_timestamp
        .as_ref()
        .map_or(0, |t| u64::try_from(now - t.0.as_second()).unwrap_or(0));
    Some(Listed {
        namespace: item.metadata.namespace.clone().unwrap_or_default(),
        name: item.metadata.name.clone().unwrap_or_default(),
        uid,
        age: Duration::from_secs(age),
    })
}

/// Run the audit for one controller until the process ends (ADR-0145).
///
/// A pass whose listing fails changes nothing: the audit cannot decide, and an
/// unreachable API server is not a stale controller.
///
/// # Arguments
/// * `controller` - the controller's kind, the `controller` label.
/// * `cfg` - the timings.
/// * `api` - the same scope the controller watches.
/// * `store` - the controller's own cache.
/// * `gate` - the controller's namespace gate.
/// * `metrics` - where the gauge and the ledger live.
/// * `readiness` - where staleness is reported to `/livez`.
/// * `started` - when the process started.
#[allow(clippy::too_many_arguments)] // one call site, each argument a distinct handle
pub async fn run<K>(
    controller: &'static str,
    cfg: AuditConfig,
    api: Api<K>,
    store: Store<K>,
    gate: NamespaceGate,
    metrics: Metrics,
    readiness: Readiness,
    started: Instant,
) where
    K: Resource + Clone + DeserializeOwned + Debug + Send + Sync + 'static,
    K::DynamicType: Default + Eq + Hash + Clone,
{
    let Some(ledger) = metrics.ledger(controller) else {
        error!(
            controller,
            "audit started without enable_audit; nothing to audit"
        );
        return;
    };
    let mut tracker = Tracker::default();
    loop {
        tokio::time::sleep(cfg.interval).await;
        if !cfg.past_startup(started.elapsed()) {
            continue;
        }
        let listed = match list_all(&api).await {
            Ok(listed) => listed,
            Err(e) => {
                warn!(controller, error = %e, "audit: authoritative list failed; pass skipped");
                continue;
            }
        };
        let store_uids: HashSet<String> = store
            .state()
            .iter()
            .filter_map(|o| o.meta().uid.clone())
            .collect();
        let observed = build_observed(&listed, &store_uids, &ledger, |ns| {
            ns.is_empty() || gate.serves(ns)
        });
        let report = evaluate(&observed, cfg.grace);

        metrics.set_unreconciled(
            controller,
            REASON_MISSING_FROM_STORE,
            report.missing_from_store.len(),
        );
        metrics.set_unreconciled(
            controller,
            REASON_NEVER_RECONCILED,
            report.never_reconciled.len(),
        );

        let was_bad = tracker.failing_since.is_some();
        let verdict = tracker.observe(report.is_clean(), Instant::now(), cfg.stale_after);
        let live: HashSet<String> = listed.iter().map(|l| l.uid.clone()).collect();
        ledger.retain(&live);

        match verdict {
            Verdict::Healthy => {
                if was_bad {
                    info!(
                        controller,
                        "audit: every object is cached and reconciled again"
                    );
                }
                readiness.clear_stale(controller);
            }
            Verdict::Degraded => {
                warn_report(controller, &report, listed.len(), &store_uids, &ledger);
            }
            Verdict::Stale => {
                error!(
                    controller,
                    missing_from_store = report.missing_from_store.len(),
                    never_reconciled = report.never_reconciled.len(),
                    api = listed.len(),
                    store = store_uids.len(),
                    ledger = ledger.len(),
                    oldest_secs = report.oldest.as_secs(),
                    objects = ?report.sample(LOG_SAMPLE),
                    "audit: the controller has not reconciled objects that exist for the stale \
                     threshold; failing liveness so it is restarted"
                );
                readiness.mark_stale(controller);
            }
        }
    }
}

/// The `WARN` for a bad pass that has not yet lasted the stale threshold.
fn warn_report(
    controller: &'static str,
    report: &Report,
    api: usize,
    store_uids: &HashSet<String>,
    ledger: &Ledger,
) {
    warn!(
        controller,
        missing_from_store = report.missing_from_store.len(),
        never_reconciled = report.never_reconciled.len(),
        api,
        store = store_uids.len(),
        ledger = ledger.len(),
        oldest_secs = report.oldest.as_secs(),
        objects = ?report.sample(LOG_SAMPLE),
        "audit: objects exist that the controller has not reconciled"
    );
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod audit_tests;
