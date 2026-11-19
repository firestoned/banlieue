// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! RBAC and Namespace literals shared by `bootstrap` and `workload`.
//!
//! Both modules build bindings, rules and namespaces; one spelling of each
//! keeps a CLI-installed object identical to one the operator renders.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::api::rbac::v1::{PolicyRule, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

use crate::naming::{APP_NAME, LABEL_NAME};

/// API group of `Role`, `ClusterRole` and their bindings.
const RBAC_API_GROUP: &str = "rbac.authorization.k8s.io";

/// Subject kind of a ServiceAccount.
const SERVICE_ACCOUNT_KIND: &str = "ServiceAccount";

/// Prefix of the Pod Security Admission namespace labels.
const PSA_LABEL_PREFIX: &str = "pod-security.kubernetes.io";

/// Pod Security Standards level `audit` and `warn` always stay at.
const PSA_RESTRICTED: &str = "restricted";

/// A `roleRef` to the `Role` or `ClusterRole` (`kind`) called `name`.
pub(crate) fn role_ref(kind: &str, name: impl Into<String>) -> RoleRef {
    RoleRef {
        api_group: Some(RBAC_API_GROUP.to_string()),
        kind: kind.to_string(),
        name: name.into(),
    }
}

/// A ServiceAccount subject. `namespace` is where the ServiceAccount lives,
/// which is not necessarily the binding's own namespace.
pub(crate) fn sa_subject(name: impl Into<String>, namespace: &str) -> Subject {
    Subject {
        kind: SERVICE_ACCOUNT_KIND.to_string(),
        name: name.into(),
        namespace: Some(namespace.to_string()),
        ..Default::default()
    }
}

/// A policy rule granting `verbs` on one `resource` of one API `group`.
pub(crate) fn rule(group: &str, resource: &str, verbs: &[&str]) -> PolicyRule {
    PolicyRule {
        api_groups: Some(vec![group.to_string()]),
        resources: Some(vec![resource.to_string()]),
        verbs: verbs.iter().map(|v| (*v).to_string()).collect(),
        ..Default::default()
    }
}

/// Build a `resourceNames`-scoped policy rule.
pub(crate) fn named_rule(
    group: &str,
    resource: &str,
    verbs: &[&str],
    names: &[String],
) -> PolicyRule {
    PolicyRule {
        resource_names: Some(names.to_vec()),
        ..rule(group, resource, verbs)
    }
}

/// A banlieue Namespace enforcing the Pod Security Standards level `enforce`.
/// `audit` and `warn` stay at `restricted` whatever is enforced, so a workload
/// below that level is still visible.
pub(crate) fn namespace_with_psa(name: &str, enforce: &str) -> Namespace {
    let mut labels = BTreeMap::from([(LABEL_NAME.to_string(), APP_NAME.to_string())]);
    for (mode, level) in [
        ("enforce", enforce),
        ("audit", PSA_RESTRICTED),
        ("warn", PSA_RESTRICTED),
    ] {
        labels.insert(format!("{PSA_LABEL_PREFIX}/{mode}"), level.to_string());
    }
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "rbac_tests.rs"]
mod rbac_tests;
