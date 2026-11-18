// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `rbac.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::json;

    #[test]
    fn role_ref_names_the_rbac_group() {
        assert_eq!(
            serde_json::to_value(role_ref("ClusterRole", "r")).unwrap(),
            json!({"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "r"})
        );
    }

    #[test]
    fn sa_subject_carries_the_service_account_namespace() {
        assert_eq!(
            serde_json::to_value(sa_subject("sa", "ns")).unwrap(),
            json!({"kind": "ServiceAccount", "name": "sa", "namespace": "ns"})
        );
    }

    #[test]
    fn rule_is_not_narrowed_by_resource_names() {
        assert_eq!(
            serde_json::to_value(rule("", "secrets", &["get", "list"])).unwrap(),
            json!({"apiGroups": [""], "resources": ["secrets"], "verbs": ["get", "list"]})
        );
    }

    #[test]
    fn named_rule_adds_resource_names_to_the_same_rule() {
        let names = ["mine".to_string()];
        assert_eq!(
            serde_json::to_value(named_rule("batch", "jobs", &["get"], &names)).unwrap(),
            json!({
                "apiGroups": ["batch"],
                "resources": ["jobs"],
                "resourceNames": ["mine"],
                "verbs": ["get"],
            })
        );
    }

    /// Only `enforce` follows the argument: audit and warn stay restricted.
    #[test]
    fn namespace_with_psa_keeps_audit_and_warn_restricted() {
        let namespace = namespace_with_psa("build", "privileged");
        assert_eq!(namespace.metadata.name.as_deref(), Some("build"));
        assert_eq!(
            serde_json::to_value(namespace.metadata.labels).unwrap(),
            json!({
                "app.kubernetes.io/name": "banlieue",
                "pod-security.kubernetes.io/enforce": "privileged",
                "pod-security.kubernetes.io/audit": "restricted",
                "pod-security.kubernetes.io/warn": "restricted",
            })
        );
    }
}
