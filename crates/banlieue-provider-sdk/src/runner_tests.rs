// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::runner`]: outcome classification, and the
//! series the wrapped reconcile records, read back from the encoded registry.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use k8s_openapi::api::core::v1::ConfigMap;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use kube::runtime::controller::Action;

    use super::super::*;
    use crate::metrics::{ErrorKind, Metrics, ReconcileResult};
    use crate::reconciler::{no_requeue, requeue_default, requeue_long, requeue_on_error};

    /// A tenant-chosen object name that must never reach a label.
    const OBJECT_NAME: &str = "tenant-secret-project-vm-0";
    const OBJECT_NAMESPACE: &str = "tenant-a";

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("backend said no to {0}")]
        Backend(String),
    }

    impl ErrorKind for TestError {
        fn kind(&self) -> &'static str {
            match self {
                Self::Backend(_) => "Backend",
            }
        }
    }

    fn object() -> Arc<ConfigMap> {
        Arc::new(ConfigMap {
            metadata: ObjectMeta {
                name: Some(OBJECT_NAME.to_string()),
                namespace: Some(OBJECT_NAMESPACE.to_string()),
                resource_version: Some("42".to_string()),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[test]
    fn steady_state_actions_are_success() {
        for action in [requeue_default(), requeue_long(), no_requeue()] {
            assert_eq!(classify::<TestError>(&Ok(action)), ReconcileResult::Success);
        }
    }

    #[test]
    fn short_requeues_are_requeue() {
        for action in [requeue_on_error(), Action::requeue(Duration::from_secs(2))] {
            assert_eq!(classify::<TestError>(&Ok(action)), ReconcileResult::Requeue);
        }
    }

    #[test]
    fn errors_are_error() {
        assert_eq!(
            classify::<TestError>(&Err(TestError::Backend("x".into()))),
            ReconcileResult::Error
        );
    }

    #[tokio::test]
    async fn wrapped_success_records_total_and_duration() {
        let metrics = Metrics::new("test");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |_obj: Arc<ConfigMap>, _ctx: Arc<()>| async { Ok::<_, TestError>(requeue_default()) },
        );

        let outcome = wrapped(object(), Arc::new(())).await;
        assert!(outcome.is_ok());

        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(r#"banlieue_reconcile_total{controller="ConfigMap",result="success"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"banlieue_reconcile_duration_seconds_count{controller="ConfigMap"} 1"#),
            "{text}"
        );
        assert!(!text.contains("banlieue_reconcile_errors_total{"), "{text}");
    }

    #[tokio::test]
    async fn wrapped_error_records_error_and_kind_and_returns_the_error() {
        let metrics = Metrics::new("test");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |obj: Arc<ConfigMap>, _ctx: Arc<()>| {
                let name = obj.metadata.name.clone().unwrap_or_default();
                async move { Err::<Action, _>(TestError::Backend(name)) }
            },
        );

        let outcome = wrapped(object(), Arc::new(())).await;
        assert!(matches!(outcome, Err(TestError::Backend(_))));

        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(r#"banlieue_reconcile_total{controller="ConfigMap",result="error"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(
                r#"banlieue_reconcile_errors_total{controller="ConfigMap",kind="Backend"} 1"#
            ),
            "{text}"
        );
    }

    #[tokio::test]
    async fn no_label_carries_object_name_namespace_or_error_text() {
        let metrics = Metrics::new("test");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |obj: Arc<ConfigMap>, _ctx: Arc<()>| {
                let name = obj.metadata.name.clone().unwrap_or_default();
                async move { Err::<Action, _>(TestError::Backend(name)) }
            },
        );
        let _ = wrapped(object(), Arc::new(())).await;

        let text = metrics.encode().expect("encode");
        assert!(!text.contains(OBJECT_NAME), "{text}");
        assert!(!text.contains(OBJECT_NAMESPACE), "{text}");
        assert!(!text.contains("backend said no"), "{text}");
    }

    #[tokio::test]
    async fn many_objects_share_one_series_per_controller_and_result() {
        let metrics = Metrics::new("test");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |_obj: Arc<ConfigMap>, _ctx: Arc<()>| async { Ok::<_, TestError>(requeue_default()) },
        );
        const OBJECTS: usize = 50;
        for i in 0..OBJECTS {
            let mut cm = (*object()).clone();
            cm.metadata.name = Some(format!("vm-{i}"));
            let _ = wrapped(Arc::new(cm), Arc::new(())).await;
        }

        let text = metrics.encode().expect("encode");
        let series = text
            .lines()
            .filter(|l| l.starts_with("banlieue_reconcile_total{"))
            .count();
        assert_eq!(series, 1, "{text}");
        assert!(text.contains(&format!(
            r#"banlieue_reconcile_total{{controller="ConfigMap",result="success"}} {OBJECTS}"#
        )));
    }

    // ---- audit ledger (ADR-0145) -------------------------------------------

    fn object_with_uid(uid: &str) -> Arc<ConfigMap> {
        Arc::new(ConfigMap {
            metadata: ObjectMeta {
                name: Some(OBJECT_NAME.to_string()),
                namespace: Some(OBJECT_NAMESPACE.to_string()),
                uid: Some(uid.to_string()),
                resource_version: Some("42".to_string()),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn an_audited_controller_records_every_invocation_in_the_ledger() {
        let metrics = Metrics::new("test");
        metrics.enable_audit("ConfigMap");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |_obj: Arc<ConfigMap>, _ctx: Arc<()>| async { Ok::<_, TestError>(requeue_default()) },
        );

        let _ = wrapped(object_with_uid("uid-1"), Arc::new(())).await;

        assert!(
            metrics
                .ledger("ConfigMap")
                .expect("opted in")
                .contains("uid-1")
        );
    }

    #[tokio::test]
    async fn an_invocation_that_returns_early_still_counts_as_invoked() {
        // The namespace gate's unserved path returns `await_change` before doing anything
        // (ADR-0130): that is "invoked and skipped", which the audit must tell from "never
        // invoked".
        let metrics = Metrics::new("test");
        metrics.enable_audit("ConfigMap");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |_obj: Arc<ConfigMap>, _ctx: Arc<()>| async { Ok::<_, TestError>(no_requeue()) },
        );

        let _ = wrapped(object_with_uid("uid-skip"), Arc::new(())).await;

        assert!(
            metrics
                .ledger("ConfigMap")
                .expect("opted in")
                .contains("uid-skip")
        );
    }

    #[tokio::test]
    async fn an_invocation_that_errors_still_counts_as_invoked() {
        // A failing reconcile must never trip the audit: only "never invoked" does.
        let metrics = Metrics::new("test");
        metrics.enable_audit("ConfigMap");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |_obj: Arc<ConfigMap>, _ctx: Arc<()>| async {
                Err::<Action, _>(TestError::Backend("no".into()))
            },
        );

        let _ = wrapped(object_with_uid("uid-err"), Arc::new(())).await;

        assert!(
            metrics
                .ledger("ConfigMap")
                .expect("opted in")
                .contains("uid-err")
        );
    }

    #[tokio::test]
    async fn an_unaudited_controller_keeps_no_ledger_so_its_memory_does_not_grow() {
        let metrics = Metrics::new("test");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |_obj: Arc<ConfigMap>, _ctx: Arc<()>| async { Ok::<_, TestError>(requeue_default()) },
        );

        let _ = wrapped(object_with_uid("uid-1"), Arc::new(())).await;

        assert!(metrics.ledger("ConfigMap").is_none());
    }

    #[tokio::test]
    async fn an_object_with_no_uid_is_not_recorded_and_does_not_break_the_reconcile() {
        let metrics = Metrics::new("test");
        metrics.enable_audit("ConfigMap");
        let mut wrapped = instrument_reconcile(
            "ConfigMap",
            metrics.clone(),
            |_obj: Arc<ConfigMap>, _ctx: Arc<()>| async { Ok::<_, TestError>(requeue_default()) },
        );

        let outcome = wrapped(object(), Arc::new(())).await;

        assert!(outcome.is_ok());
        assert_eq!(metrics.ledger("ConfigMap").expect("opted in").len(), 0);
    }
}
