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
}
