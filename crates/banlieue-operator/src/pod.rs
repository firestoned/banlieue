// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pod template pieces shared by `bootstrap` and `workload`.
//!
//! A bootstrap-installed role and an operator-spawned provider run the same
//! binary under the same hardening: one definition of the security contexts,
//! ports, probes and environment keeps the two from drifting.

use std::collections::BTreeMap;

use banlieue_provider_sdk::defaults::{DEFAULT_HEALTH_PORT, DEFAULT_METRICS_PORT};
use k8s_openapi::api::core::v1::{
    Capabilities, ContainerPort, EnvVar, EnvVarSource, HTTPGetAction, ObjectFieldSelector,
    PodSecurityContext, Probe, ResourceRequirements, SeccompProfile, SecurityContext,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use crate::workload::{HEALTH_PORT_NAME, METRICS_PORT_NAME};

/// Metrics port every banlieue pod exposes.
pub(crate) const METRICS_PORT: u16 = DEFAULT_METRICS_PORT;

/// Health (livez/readyz) port every banlieue pod exposes.
pub(crate) const HEALTH_PORT: u16 = DEFAULT_HEALTH_PORT;

/// Liveness probe path and timings.
pub(crate) const LIVENESS_PATH: &str = "/livez";
pub(crate) const LIVENESS_INITIAL_DELAY_SECS: i32 = 10;
pub(crate) const LIVENESS_PERIOD_SECS: i32 = 30;

/// Readiness probe path and timings.
pub(crate) const READINESS_PATH: &str = "/readyz";
pub(crate) const READINESS_INITIAL_DELAY_SECS: i32 = 2;
pub(crate) const READINESS_PERIOD_SECS: i32 = 5;

/// Probe failure threshold shared by both probes.
const PROBE_FAILURE_THRESHOLD: i32 = 3;

/// Grace period allowing an in-flight reconcile to finish on SIGTERM.
pub(crate) const TERMINATION_GRACE_PERIOD_SECS: i64 = 30;

/// UID/GID banlieue containers run as (matches the distroless `nonroot` user).
const RUN_AS_NONROOT_UID: i64 = 65532;

/// Seccomp profile applied at pod and container level.
const SECCOMP_RUNTIME_DEFAULT: &str = "RuntimeDefault";

/// CPU/memory requests shared by every banlieue container.
pub(crate) const DEFAULT_CPU_REQUEST: &str = "50m";
pub(crate) const DEFAULT_MEMORY_REQUEST: &str = "128Mi";

/// The `RuntimeDefault` seccomp profile.
fn seccomp_profile() -> SeccompProfile {
    SeccompProfile {
        type_: SECCOMP_RUNTIME_DEFAULT.to_string(),
        ..Default::default()
    }
}

/// Pod-level security context: non-root, fixed UID/GID, default seccomp.
pub(crate) fn pod_security_context() -> PodSecurityContext {
    PodSecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(RUN_AS_NONROOT_UID),
        run_as_group: Some(RUN_AS_NONROOT_UID),
        fs_group: Some(RUN_AS_NONROOT_UID),
        seccomp_profile: Some(seccomp_profile()),
        ..Default::default()
    }
}

/// Container-level security context: no escalation, read-only root, every
/// capability dropped, default seccomp.
pub(crate) fn container_security_context() -> SecurityContext {
    SecurityContext {
        allow_privilege_escalation: Some(false),
        read_only_root_filesystem: Some(true),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        }),
        seccomp_profile: Some(seccomp_profile()),
        ..Default::default()
    }
}

/// The named metrics and health ports, in that order.
pub(crate) fn container_ports() -> Vec<ContainerPort> {
    [
        (METRICS_PORT_NAME, METRICS_PORT),
        (HEALTH_PORT_NAME, HEALTH_PORT),
    ]
    .into_iter()
    .map(|(name, port)| ContainerPort {
        name: Some(name.to_string()),
        container_port: i32::from(port),
        protocol: Some("TCP".to_string()),
        ..Default::default()
    })
    .collect()
}

/// Build an HTTP probe against the health port. `timeout` of `None` leaves
/// `timeoutSeconds` unset.
pub(crate) fn http_probe(
    path: &str,
    initial_delay: i32,
    period: i32,
    timeout: Option<i32>,
) -> Probe {
    Probe {
        http_get: Some(HTTPGetAction {
            path: Some(path.to_string()),
            port: IntOrString::String(HEALTH_PORT_NAME.to_string()),
            ..Default::default()
        }),
        initial_delay_seconds: Some(initial_delay),
        period_seconds: Some(period),
        timeout_seconds: timeout,
        failure_threshold: Some(PROBE_FAILURE_THRESHOLD),
        ..Default::default()
    }
}

/// Environment variables read through the downward API, from
/// `(variable name, field path)` pairs, in the order given.
pub(crate) fn downward_api_env(vars: &[(&str, &str)]) -> Vec<EnvVar> {
    vars.iter()
        .map(|(name, field_path)| EnvVar {
            name: (*name).to_string(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    field_path: (*field_path).to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect()
}

/// CPU and memory requests and limits.
pub(crate) fn resources(
    cpu_request: &str,
    memory_request: &str,
    cpu_limit: &str,
    memory_limit: &str,
) -> ResourceRequirements {
    let pair = |cpu: &str, memory: &str| {
        BTreeMap::from([
            ("cpu".to_string(), Quantity(cpu.to_string())),
            ("memory".to_string(), Quantity(memory.to_string())),
        ])
    };
    ResourceRequirements {
        requests: Some(pair(cpu_request, memory_request)),
        limits: Some(pair(cpu_limit, memory_limit)),
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "pod_tests.rs"]
mod pod_tests;
