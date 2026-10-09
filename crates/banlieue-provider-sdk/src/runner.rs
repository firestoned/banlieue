// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The one place every controller is run from (ADR-0091 Decision 2).
//!
//! [`run_controller`] replaces the
//! `Controller::new(..).run(reconcile, error_policy, ctx).for_each(..)` chain
//! each role used to repeat. It wraps the reconcile function so that every
//! reconcile, in every role:
//!
//! - runs inside a `reconcile` span carrying `controller`, `namespace`,
//!   `name`, `resource_version` and `result` (ADR-0092 Decision 4, D-017);
//! - is counted in `banlieue_reconcile_total{controller,result}` and timed in
//!   `banlieue_reconcile_duration_seconds{controller}`;
//! - on error, is counted in `banlieue_reconcile_errors_total{controller,kind}`
//!   with `kind` from the error's [`ErrorKind`].
//!
//! The module's `error_policy` is passed through untouched: the wrapper only
//! observes. A new controller is instrumented by construction.

use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use futures::future::BoxFuture;
use kube::Resource;
use kube::ResourceExt;
use kube::runtime::Controller;
use kube::runtime::controller::Action;
use serde::de::DeserializeOwned;
use tracing::{Instrument, debug, error, field};

use crate::metrics::{ErrorKind, Metrics, ReconcileResult};
use crate::reconciler::{no_requeue, requeue_default, requeue_long};

/// Classify a reconcile's outcome for the `result` label.
///
/// `Ok` with one of the SDK's steady-state actions (the default or long
/// periodic requeue, or await-change) is [`ReconcileResult::Success`]; `Ok`
/// with any other action means the reconciler asked to run again soon, which
/// is [`ReconcileResult::Requeue`]. `Err` is [`ReconcileResult::Error`].
/// `Action` is opaque, so the comparison is by equality with the SDK's own
/// constructors.
pub fn classify<E>(outcome: &Result<Action, E>) -> ReconcileResult {
    let Ok(action) = outcome else {
        return ReconcileResult::Error;
    };
    if *action == requeue_default() || *action == requeue_long() || *action == no_requeue() {
        return ReconcileResult::Success;
    }
    ReconcileResult::Requeue
}

/// Wrap `reconcile` with the span and metrics described in the module docs.
///
/// Exposed separately from [`run_controller`] so it can be tested without a
/// cluster.
///
/// # Arguments
/// * `controller` - the reconciled kind, e.g. `VirtualMachine`. Used as the
///   `controller` label and span field.
/// * `metrics` - the process registry.
/// * `reconcile` - the module's reconcile function.
pub fn instrument_reconcile<K, Ctx, E, F, Fut>(
    controller: &'static str,
    metrics: Metrics,
    mut reconcile: F,
) -> impl FnMut(Arc<K>, Arc<Ctx>) -> BoxFuture<'static, Result<Action, E>>
where
    K: Resource,
    F: FnMut(Arc<K>, Arc<Ctx>) -> Fut,
    Fut: Future<Output = Result<Action, E>> + Send + 'static,
    E: ErrorKind + Send + 'static,
{
    move |obj: Arc<K>, ctx: Arc<Ctx>| {
        let span = tracing::info_span!(
            "reconcile",
            controller,
            namespace = obj.namespace().unwrap_or_default(),
            name = obj.name_any(),
            resource_version = obj.resource_version().unwrap_or_default(),
            result = field::Empty,
        );
        let fut = reconcile(obj, ctx);
        let metrics = metrics.clone();
        let record_span = span.clone();
        Box::pin(
            async move {
                let started = Instant::now();
                let outcome = fut.await;
                let result = classify(&outcome);
                metrics.record_reconcile(controller, result, started.elapsed());
                if let Err(e) = &outcome {
                    metrics.record_error(controller, e.kind());
                }
                record_span.record("result", result.as_str());
                outcome
            }
            .instrument(span),
        )
    }
}

/// Run `controller` to completion with the instrumented reconcile, the
/// module's own `error_policy`, and the shared result-stream logging.
///
/// # Arguments
/// * `controller` - a configured `Controller` (watches, owns already added).
/// * `name` - the reconciled kind, the `controller` label.
/// * `metrics` - the process registry.
/// * `reconcile` / `error_policy` / `ctx` - as for `Controller::run`.
pub async fn run_controller<K, Ctx, E, F, Fut, P>(
    controller: Controller<K>,
    name: &'static str,
    metrics: Metrics,
    reconcile: F,
    error_policy: P,
    ctx: Arc<Ctx>,
) where
    K: Clone + Resource + DeserializeOwned + Debug + Send + Sync + 'static,
    K::DynamicType: Eq + Hash + Clone + Debug + Unpin,
    F: FnMut(Arc<K>, Arc<Ctx>) -> Fut,
    Fut: Future<Output = Result<Action, E>> + Send + 'static,
    E: std::error::Error + ErrorKind + Send + 'static,
    P: Fn(Arc<K>, &E, Arc<Ctx>) -> Action,
{
    controller
        .run(
            instrument_reconcile(name, metrics, reconcile),
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => debug!(controller = name, ?obj, "reconciled"),
                Err(e) => error!(controller = name, error = %e, "reconcile error"),
            }
        })
        .await;
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod runner_tests;
