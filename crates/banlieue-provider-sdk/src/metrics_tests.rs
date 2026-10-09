// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::metrics`]: series names and labels as they
//! appear in the encoded registry, and the `/metrics` router.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;
    use crate::httpd::Status;

    #[test]
    fn records_reconcile_total_and_duration_by_controller_and_result() {
        let metrics = Metrics::new("banlieue-controller");
        metrics.record_reconcile(
            "VirtualMachine",
            ReconcileResult::Success,
            Duration::from_millis(20),
        );
        metrics.record_reconcile(
            "VirtualMachine",
            ReconcileResult::Requeue,
            Duration::from_millis(20),
        );

        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(
                r#"banlieue_reconcile_total{controller="VirtualMachine",result="success"} 1"#
            ),
            "{text}"
        );
        assert!(
            text.contains(
                r#"banlieue_reconcile_total{controller="VirtualMachine",result="requeue"} 1"#
            ),
            "{text}"
        );
        assert!(
            text.contains(
                r#"banlieue_reconcile_duration_seconds_count{controller="VirtualMachine"} 2"#
            ),
            "{text}"
        );
        assert!(text.contains("# UNIT banlieue_reconcile_duration_seconds seconds"));
    }

    #[test]
    fn records_errors_by_variant_kind() {
        let metrics = Metrics::new("banlieue-provider-libvirt");
        metrics.record_error("LibvirtMachine", "Libvirt");
        metrics.record_error("LibvirtMachine", "Libvirt");

        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(
                r#"banlieue_reconcile_errors_total{controller="LibvirtMachine",kind="Libvirt"} 2"#
            ),
            "{text}"
        );
    }

    #[test]
    fn leader_gauge_is_labelled_by_role_and_shared() {
        let metrics = Metrics::new("banlieue-operator");
        let gauge = metrics.leader_gauge();
        gauge.set(1);

        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(r#"banlieue_leader{role="banlieue-operator"} 1"#),
            "{text}"
        );
        assert_eq!(metrics.leader_gauge().get(), 1);
    }

    #[test]
    fn result_labels_are_the_documented_values() {
        assert_eq!(ReconcileResult::Success.as_str(), "success");
        assert_eq!(ReconcileResult::Error.as_str(), "error");
        assert_eq!(ReconcileResult::Requeue.as_str(), "requeue");
    }

    #[test]
    fn encoded_registry_ends_with_eof_marker() {
        let text = Metrics::new("r").encode().expect("encode");
        assert!(text.ends_with("# EOF\n"), "{text}");
    }

    #[test]
    fn get_metrics_is_openmetrics_200() {
        let metrics = Metrics::new("banlieue-controller");
        let _leader = metrics.leader_gauge();
        let response = handle(b"GET /metrics HTTP/1.1\r\n\r\n", &metrics);
        assert_eq!(response.status, Status::Ok);
        assert_eq!(response.content_type, OPENMETRICS_CONTENT_TYPE);
        assert!(response.body.contains("banlieue_leader"));
    }

    #[test]
    fn other_paths_and_methods_are_404() {
        let metrics = Metrics::new("r");
        for raw in [
            &b"GET / HTTP/1.1\r\n"[..],
            b"GET /readyz HTTP/1.1\r\n",
            b"POST /metrics HTTP/1.1\r\n",
        ] {
            assert_eq!(handle(raw, &metrics).status, Status::NotFound);
        }
    }

    #[test]
    fn malformed_request_is_400() {
        let metrics = Metrics::new("r");
        assert_eq!(handle(b"\0\0\0", &metrics).status, Status::BadRequest);
    }

    #[test]
    fn buckets_are_strictly_increasing() {
        assert!(
            RECONCILE_DURATION_BUCKETS_SECS
                .windows(2)
                .all(|w| w[0] < w[1])
        );
    }
}
