// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where image-build workloads are allowed to run (ADR-0016 follow-up).
//!
//! ADR-0016 confines privileged build pods to their own namespace, and is
//! explicit that this bounds *admission surface*, not *escape capability* — a
//! privileged container escapes to its node regardless of namespace. The
//! control that actually bounds an escape is scheduling: builds pinned to
//! dedicated nodes that run nothing else of value.
//!
//! **This is for build pods, not import Jobs.** A privileged build pod is
//! placed by *policy* — we choose to confine it. An import Job is placed by the
//! *PVC it mounts*: the scheduler already honours the bound PV's own
//! constraints, so on node-local storage it lands where the volume is, and on
//! network-attached storage it can land anywhere the volume attaches. Giving an
//! import Job a node selector would add a constraint Kubernetes never needed
//! and would be wrong the moment the storage is not node-local.
//!
//! Tolerations are the exception, and they are not a placement decision:
//! they grant permission to land on a node the scheduler has *already* chosen.
//! An import Job needs them only when the volume happens to sit on a tainted
//! node — which is exactly what a dedicated build node is.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Toleration;

/// Where build and import workloads are allowed to run.
///
/// Empty means "no constraint": an operator who has not set up a dedicated
/// build node gets default scheduling rather than a pod that never schedules.
// `Toleration` is not `Eq` (k8s-openapi derives only `PartialEq`), so neither
// is this.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BuildScheduling {
    /// `nodeSelector` applied to build and import pods.
    pub node_selector: BTreeMap<String, String>,
    /// Tolerations applied to build and import pods.
    pub tolerations: Vec<Toleration>,
}

impl BuildScheduling {
    /// Parse both from repeated CLI flags.
    ///
    /// # Errors
    /// The first parse failure, naming the offending flag value.
    pub fn from_flags(selectors: &[String], tolerations: &[String]) -> Result<Self, String> {
        Ok(Self {
            node_selector: parse_node_selector(selectors)?,
            tolerations: parse_tolerations(tolerations)?,
        })
    }

    /// True when no constraint is configured.
    #[must_use]
    pub fn is_unconstrained(&self) -> bool {
        self.node_selector.is_empty() && self.tolerations.is_empty()
    }
}

/// Taint effects Kubernetes recognises.
const VALID_EFFECTS: [&str; 3] = ["NoSchedule", "PreferNoSchedule", "NoExecute"];

/// Parse repeated `key=value` flags into a `nodeSelector` map.
///
/// # Errors
/// A message naming the offending input when an entry is not exactly one
/// `key=value` pair. Rejecting is deliberate: a malformed selector that parsed
/// to "no constraint" would schedule privileged builds anywhere in the cluster,
/// which is the failure this exists to prevent.
pub fn parse_node_selector(entries: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for entry in entries {
        let mut parts = entry.splitn(2, '=');
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            return Err(format!(
                "node selector {entry:?} must be key=value (e.g. banlieue.io/imagebuild=true)"
            ));
        };
        if key.is_empty() || value.contains('=') {
            return Err(format!(
                "node selector {entry:?} must be key=value (e.g. banlieue.io/imagebuild=true)"
            ));
        }
        out.insert(key.to_string(), value.to_string());
    }
    Ok(out)
}

/// Parse repeated `key[=value]:Effect` flags into tolerations.
///
/// `key=value:NoSchedule` tolerates that exact taint; `key:NoSchedule`
/// tolerates the taint whatever its value.
///
/// # Errors
/// A message naming the offending input when the effect is missing or unknown.
/// Kubernetes silently ignores a toleration carrying a bogus effect, leaving
/// the pod unschedulable with nothing to explain why — so it is caught here.
pub fn parse_tolerations(entries: &[String]) -> Result<Vec<Toleration>, String> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some((lhs, effect)) = entry.rsplit_once(':') else {
            return Err(format!(
                "toleration {entry:?} must end in an effect, e.g. \
                 dedicated=imagebuild:NoSchedule"
            ));
        };
        if !VALID_EFFECTS.contains(&effect) {
            return Err(format!(
                "toleration {entry:?} has unknown effect {effect:?}; expected one of {}",
                VALID_EFFECTS.join(", ")
            ));
        }
        // `key=value` is an Equal match; a bare `key` is Exists — NOT Equal
        // against an empty value, which would only tolerate a valueless taint.
        let toleration = match lhs.split_once('=') {
            Some((key, value)) => Toleration {
                key: Some(key.to_string()),
                operator: Some("Equal".to_string()),
                value: Some(value.to_string()),
                effect: Some(effect.to_string()),
                ..Default::default()
            },
            None => Toleration {
                key: Some(lhs.to_string()),
                operator: Some("Exists".to_string()),
                value: None,
                effect: Some(effect.to_string()),
                ..Default::default()
            },
        };
        out.push(toleration);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "scheduling_tests.rs"]
mod scheduling_tests;
