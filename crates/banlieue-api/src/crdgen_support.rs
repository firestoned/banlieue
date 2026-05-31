// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Post-generation fix-ups applied to CRDs by the `crdgen` binary.
//!
//! Gated behind the `crdgen` feature so it is only compiled when generating
//! CRD YAML. Kept in the library (rather than the binary) so it follows the
//! workspace's separate-`_tests.rs` convention without colliding with cargo's
//! `src/bin/*.rs` auto-binary discovery.

use std::collections::BTreeMap;

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;

/// Property name `kube-derive` uses for the spec sub-schema.
const SPEC_PROPERTY: &str = "spec";

/// CAPI v1beta2 contract-discovery label key. Its value enumerates the CRD
/// versions that satisfy the contract.
const CAPI_V1BETA2_LABEL: &str = "cluster.x-k8s.io/v1beta2";

/// API group whose CRDs implement the CAPI infrastructure contracts
/// (InfraMachine / InfraCluster). Only these are labelled.
const INFRA_GROUP: &str = "infrastructure.banlieue.io";

/// Apply post-generation fix-ups to a CRD before serialization.
pub fn prepared(mut crd: CustomResourceDefinition) -> CustomResourceDefinition {
    promote_spec_description(&mut crd);
    add_capi_contract_label(&mut crd);
    crd
}

/// Add the CAPI v1beta2 contract-discovery label to `infrastructure.banlieue.io`
/// CRDs.
///
/// CAPI core finds contract-compliant CRDs by the CRD-level label
/// `cluster.x-k8s.io/v1beta2: <served versions>` (e.g. `v1alpha1`). `kube-derive`
/// cannot emit CRD `metadata.labels`, so we add it here rather than in a
/// separate kustomize overlay — keeping the contract label in the generated,
/// single-source-of-truth YAML (ADR-0005).
///
/// Only the `infrastructure.banlieue.io` group is labelled; the user-facing
/// `banlieue.io` group (`Provider`, `VirtualMachine`, …) is not a CAPI contract
/// surface and is left untouched.
pub fn add_capi_contract_label(crd: &mut CustomResourceDefinition) {
    if crd.spec.group != INFRA_GROUP {
        return;
    }
    let versions = served_versions(crd);
    if versions.is_empty() {
        return;
    }
    crd.metadata
        .labels
        .get_or_insert_with(BTreeMap::new)
        .insert(CAPI_V1BETA2_LABEL.to_string(), versions);
}

/// Comma-joined names of the CRD's served versions — the value CAPI expects on
/// the contract label.
fn served_versions(crd: &CustomResourceDefinition) -> String {
    crd.spec
        .versions
        .iter()
        .filter(|v| v.served)
        .map(|v| v.name.clone())
        .collect::<Vec<_>>()
        .join(",")
}

/// Promote the `spec` sub-schema's description to the CRD's root description.
///
/// `kube-derive` hard-codes the root `openAPIV3Schema.description` to
/// "Auto-generated derived type for `<T>` via `CustomResource`" and routes the
/// doc comment on the spec struct to the `spec` property instead. Surfacing the
/// authored description at the root means a bare `kubectl explain <kind>` shows
/// the real "what is this resource" text instead of the boilerplate. Each
/// served version is handled independently; a version whose spec carries no
/// description keeps whatever root description it already had.
pub fn promote_spec_description(crd: &mut CustomResourceDefinition) {
    for version in &mut crd.spec.versions {
        let Some(schema) = version.schema.as_mut() else {
            continue;
        };
        let Some(root) = schema.open_api_v3_schema.as_mut() else {
            continue;
        };
        let spec_description = root
            .properties
            .as_ref()
            .and_then(|props| props.get(SPEC_PROPERTY))
            .and_then(|spec| spec.description.clone());
        if let Some(description) = spec_description {
            root.description = Some(description);
        }
    }
}

#[cfg(test)]
#[path = "crdgen_support_tests.rs"]
mod crdgen_support_tests;
