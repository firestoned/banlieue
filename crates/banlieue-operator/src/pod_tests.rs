// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `pod.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::json;

    #[test]
    fn pod_security_context_is_nonroot_with_default_seccomp() {
        assert_eq!(
            serde_json::to_value(pod_security_context()).unwrap(),
            json!({
                "runAsNonRoot": true,
                "runAsUser": 65532,
                "runAsGroup": 65532,
                "fsGroup": 65532,
                "seccompProfile": {"type": "RuntimeDefault"},
            })
        );
    }

    #[test]
    fn container_security_context_drops_everything() {
        assert_eq!(
            serde_json::to_value(container_security_context()).unwrap(),
            json!({
                "allowPrivilegeEscalation": false,
                "readOnlyRootFilesystem": true,
                "capabilities": {"drop": ["ALL"]},
                "seccompProfile": {"type": "RuntimeDefault"},
            })
        );
    }

    #[test]
    fn container_ports_are_metrics_then_health() {
        assert_eq!(
            serde_json::to_value(container_ports()).unwrap(),
            json!([
                {"name": "metrics", "containerPort": 8080, "protocol": "TCP"},
                {"name": "health", "containerPort": 8081, "protocol": "TCP"},
            ])
        );
    }

    #[test]
    fn http_probe_sets_a_timeout_only_when_given_one() {
        assert_eq!(
            serde_json::to_value(http_probe("/livez", 10, 30, None)).unwrap(),
            json!({
                "httpGet": {"path": "/livez", "port": "health"},
                "initialDelaySeconds": 10,
                "periodSeconds": 30,
                "failureThreshold": 3,
            })
        );
        assert_eq!(
            http_probe("/readyz", 2, 5, Some(2)).timeout_seconds,
            Some(2)
        );
    }

    #[test]
    fn downward_api_env_keeps_the_order_given() {
        assert_eq!(
            serde_json::to_value(downward_api_env(&[
                ("POD_NAME", "metadata.name"),
                ("POD_NAMESPACE", "metadata.namespace"),
            ]))
            .unwrap(),
            json!([
                {"name": "POD_NAME", "valueFrom": {"fieldRef": {"fieldPath": "metadata.name"}}},
                {"name": "POD_NAMESPACE", "valueFrom": {"fieldRef": {"fieldPath": "metadata.namespace"}}},
            ])
        );
    }

    #[test]
    fn resources_separates_requests_from_limits() {
        assert_eq!(
            serde_json::to_value(resources("50m", "128Mi", "500m", "256Mi")).unwrap(),
            json!({
                "requests": {"cpu": "50m", "memory": "128Mi"},
                "limits": {"cpu": "500m", "memory": "256Mi"},
            })
        );
    }
}
