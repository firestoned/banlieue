// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` VSphereCluster CRD.
//!
//! This is banlieue's reference implementation of the **CAPI v1beta2
//! InfraCluster contract**. A `clusterv1.Cluster` points its
//! `spec.infrastructureRef` at a `VSphereCluster`; CAPI then spreads the
//! cluster's machines across the failure domains this object advertises in
//! `status.failureDomains`.
//!
//! Unlike CAPV's `VSphereCluster` (bound to a single vCenter), banlieue's
//! `VSphereCluster` aggregates failure domains from **one or more**
//! `Provider`s — so a single Kubernetes cluster can span multiple vCenters
//! (e.g. 2 vCenters × 3 compute clusters = 6 failure domains). See
//! `docs/adr/0002-infracluster-failure-domain-aggregation.md`.
//!
//! It is reconciled by banlieue's **main controller**, not the vSphere
//! provider: aggregation only reads `Provider.status.failureDomains[]` (which
//! the provider already populated by talking to vCenter), so no backend access
//! is required — preserving the CRD-only boundary (non-negotiable #1 / D-003).
//!
//! The CAPI contract label `cluster.x-k8s.io/v1beta2: v1alpha1` is emitted onto
//! the generated CRD by `crdgen` (`crdgen_support::add_capi_contract_label`),
//! since `kube-derive` cannot set CRD-level labels — same treatment as
//! `VSphereMachine`. See ADR-0005.

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "VSphereCluster",
    plural = "vsphereclusters",
    shortname = "vsc",
    namespaced,
    status = "VSphereClusterStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"Endpoint","type":"string","jsonPath":".status.controlPlaneEndpoint.host","priority":1}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// VSphereCluster — banlieue's CAPI v1beta2 InfraCluster for vSphere backends.
///
/// It tells CAPI where a cluster's machines may be placed by aggregating the
/// failure domains of one or more `Provider`s (vCenters) into the CAPI-shaped
/// `status.failureDomains` list. The control-plane endpoint is operator-supplied
/// (a VIP) or filled in by the control-plane provider (e.g. k0smotron).
///
/// # Why it exists
///
/// - **Cluster-side failure-domain spread.** CAPI's control-plane / MachineSet
///   controllers balance machines across the FDs published here. "Spread across
///   all 6 domains" is simply `replicas: 6` over a `VSphereCluster` that
///   advertises 6 FDs.
/// - **Multi-vCenter clusters.** One Kubernetes cluster can span several
///   `Provider`s — a capability beyond single-vCenter CAPV.
///
/// You do not create the resulting VMs by hand; CAPI mints `VSphereMachine`s
/// (the InfraMachine) from a `VSphereMachineTemplate`. This object only
/// advertises *where* they may go.
pub struct VSphereClusterSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract (optional): the cluster's API-server endpoint. Operator-
    /// supplied control-plane VIP, or left unset for a control-plane provider
    /// (e.g. k0smotron) to manage. Mirrored to `status.controlPlaneEndpoint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane_endpoint: Option<ApiEndpoint>,

    // ------------------------------------------------------------------
    // banlieue-specific: which Providers' failure domains to aggregate
    // ------------------------------------------------------------------
    /// Select `Provider`s (in this namespace) to aggregate failure domains
    /// from, by matching their labels. Ignored when `providerRefs` is set.
    #[serde(default, skip_serializing_if = "label_selector_is_empty")]
    pub provider_selector: LabelSelector,

    /// Explicit list of `Provider`s (in this namespace) to aggregate. Takes
    /// precedence over `providerSelector` when non-empty. Declaring the set
    /// explicitly is the preferred, "explicit over implicit" form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_refs: Vec<LocalObjectReference>,

    /// Which aggregated failure domains are eligible to run control-plane
    /// nodes, matched against the Provider FD `labels`. When unset, **all**
    /// aggregated FDs are control-plane eligible. Use this to keep the etcd
    /// quorum to a bounded, odd set of domains while workers spread wider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane_failure_domain_selector: Option<LabelSelector>,

    /// Suspend reconciliation. Equivalent to the `cluster.x-k8s.io/paused`
    /// annotation but in-band.
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
}

// ----------------------------------------------------------------------
// Status — CAPI v1beta2 InfraCluster contract
// ----------------------------------------------------------------------

/// Observed state of a VSphereCluster, shaped to the CAPI v1beta2 InfraCluster
/// status contract.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VSphereClusterStatus {
    /// CAPI contract field: replaces the deprecated v1beta1 `status.ready`.
    /// `provisioned == true` once the failure domains are resolved (and the
    /// control-plane endpoint is known, when one is required).
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): the resolved API-server endpoint,
    /// echoed from `spec.controlPlaneEndpoint` or set by the control-plane
    /// provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane_endpoint: Option<ApiEndpoint>,

    /// CAPI contract field: the failure domains machines may be placed in,
    /// aggregated from the selected `Provider`s. A **list** per v1beta2.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_domains: Vec<ClusterFailureDomain>,

    /// CAPI-compatible conditions. The `Ready` condition is mirrored to the
    /// parent `Cluster`'s `InfrastructureReady`; `Paused` reflects pause state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,

    /// The generation of the spec the controller has reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

/// `skip_serializing_if` predicate for an empty [`LabelSelector`].
#[inline]
fn label_selector_is_empty(s: &LabelSelector) -> bool {
    s.match_labels.is_empty() && s.match_expressions.is_empty()
}

#[cfg(test)]
#[path = "vsphere_cluster_tests.rs"]
mod vsphere_cluster_tests;
