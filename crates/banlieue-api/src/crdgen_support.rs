// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Post-generation fix-ups applied to CRDs by the `crdgen` binary.
//!
//! Gated behind the `crdgen` feature so it is only compiled when generating
//! CRD YAML. Kept in the library (rather than the binary) so it follows the
//! workspace's separate-`_tests.rs` convention without colliding with cargo's
//! `src/bin/*.rs` auto-binary discovery.

use std::collections::BTreeMap;

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceDefinition, JSONSchemaProps, JSONSchemaPropsOrArray, JSONSchemaPropsOrBool,
    JSONSchemaPropsOrStringArray,
};

/// Property name `kube-derive` uses for the spec sub-schema.
const SPEC_PROPERTY: &str = "spec";

/// CAPI v1beta2 contract-discovery label key. Its value enumerates the CRD
/// versions that satisfy the contract.
const CAPI_V1BETA2_LABEL: &str = "cluster.x-k8s.io/v1beta2";

/// API group whose CRDs implement the CAPI infrastructure contracts
/// (InfraMachine / InfraCluster). Only these are labelled.
const INFRA_GROUP: &str = "infrastructure.banlieue.io";

/// Integer formats the apiserver keeps (`type: integer`).
const INTEGER_FORMATS: [&str; 2] = ["int32", "int64"];

/// Number formats the apiserver keeps (`type: number`).
const NUMBER_FORMATS: [&str; 2] = ["float", "double"];

/// String formats the apiserver keeps, without dashes: it compares formats
/// with their dashes removed, so `date-time` matches `datetime`. Mirrors
/// `supportedVersionedFormats` in apiextensions-apiserver's
/// `pkg/apiserver/validation/formats.go` (1.0 set plus the 1.34 names).
const STRING_FORMATS: [&str; 26] = [
    "bsonobjectid",
    "uri",
    "email",
    "hostname",
    "ipv4",
    "ipv6",
    "cidr",
    "mac",
    "uuid",
    "uuid3",
    "uuid4",
    "uuid5",
    "isbn",
    "isbn10",
    "isbn13",
    "creditcard",
    "ssn",
    "hexcolor",
    "rgbcolor",
    "byte",
    "password",
    "date",
    "duration",
    "datetime",
    "k8sshortname",
    "k8slongname",
];

/// Every CRD this crate defines, post-processed by [`prepared`].
///
/// **One list, three consumers.** `crdgen` writes `deploy/crds/`, `crddoc`
/// writes the API reference, and `banlieue-operator`'s bootstrap installs
/// them. Each of those used to carry its own hand-written list, and adding
/// `LibvirtMachine` (ADR-0050) missed two of the three — a CRD that reaches
/// `deploy/crds/` but not the bootstrap installs cleanly under GitOps and
/// fails under `banlieue bootstrap` with "no matches for kind", which is a
/// long way from the cause. A new CRD is added here and nowhere else.
#[must_use]
pub fn all_crds() -> Vec<CustomResourceDefinition> {
    use crate::banlieue::{
        Provider, ProviderClass, VMClass, VMImage, VirtualMachine, VirtualMachineClaim,
        VirtualMachineMove, VirtualMachinePool,
    };
    use crate::infrastructure::{
        AzureVirtualMachine, AzureVirtualMachineTemplate, CloudHypervisorMachine,
        CloudHypervisorMachineTemplate, FirecrackerMachine, FirecrackerMachineTemplate,
        LibvirtMachine, LibvirtMachineTemplate, ProxmoxMachine, ProxmoxMachineTemplate,
        VSphereCluster, VSphereMachine, VSphereMachineTemplate,
    };
    use kube::CustomResourceExt;

    vec![
        prepared(Provider::crd()),
        prepared(ProviderClass::crd()),
        prepared(VirtualMachine::crd()),
        prepared(VirtualMachinePool::crd()),
        prepared(VirtualMachineClaim::crd()),
        prepared(VirtualMachineMove::crd()),
        prepared(VMClass::crd()),
        prepared(VMImage::crd()),
        prepared(AzureVirtualMachine::crd()),
        prepared(AzureVirtualMachineTemplate::crd()),
        prepared(CloudHypervisorMachine::crd()),
        prepared(CloudHypervisorMachineTemplate::crd()),
        prepared(FirecrackerMachine::crd()),
        prepared(FirecrackerMachineTemplate::crd()),
        prepared(LibvirtMachine::crd()),
        prepared(LibvirtMachineTemplate::crd()),
        prepared(ProxmoxMachine::crd()),
        prepared(ProxmoxMachineTemplate::crd()),
        prepared(VSphereCluster::crd()),
        prepared(VSphereMachine::crd()),
        prepared(VSphereMachineTemplate::crd()),
    ]
}

/// Apply post-generation fix-ups to a CRD before serialization.
pub fn prepared(mut crd: CustomResourceDefinition) -> CustomResourceDefinition {
    promote_spec_description(&mut crd);
    add_capi_contract_label(&mut crd);
    strip_unsupported_formats(&mut crd);
    plain_crd_doc_links(&mut crd);
    crd
}

/// Every version's root schema of `crd`, mutably.
fn version_roots(crd: &mut CustomResourceDefinition) -> impl Iterator<Item = &mut JSONSchemaProps> {
    crd.spec.versions.iter_mut().filter_map(|v| {
        v.schema
            .as_mut()
            .and_then(|s| s.open_api_v3_schema.as_mut())
    })
}

/// Call `f` on `schema` and on every schema under it: properties, items,
/// additional properties and items, the combinators, pattern properties,
/// definitions and schema dependencies.
fn for_each_schema_mut(schema: &mut JSONSchemaProps, f: &mut dyn FnMut(&mut JSONSchemaProps)) {
    f(schema);
    let maps = [
        schema.properties.as_mut(),
        schema.pattern_properties.as_mut(),
        schema.definitions.as_mut(),
    ];
    for child in maps.into_iter().flatten().flat_map(|m| m.values_mut()) {
        for_each_schema_mut(child, f);
    }
    let lists = [
        schema.all_of.as_mut(),
        schema.any_of.as_mut(),
        schema.one_of.as_mut(),
    ];
    for child in lists.into_iter().flatten().flat_map(|v| v.iter_mut()) {
        for_each_schema_mut(child, f);
    }
    match schema.items.as_mut() {
        Some(JSONSchemaPropsOrArray::Schema(child)) => for_each_schema_mut(child, f),
        Some(JSONSchemaPropsOrArray::Schemas(children)) => {
            for child in children.iter_mut() {
                for_each_schema_mut(child, f);
            }
        }
        None => {}
    }
    for child in [
        schema.additional_properties.as_mut(),
        schema.additional_items.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        if let JSONSchemaPropsOrBool::Schema(child) = child {
            for_each_schema_mut(child, f);
        }
    }
    if let Some(child) = schema.not.as_mut() {
        for_each_schema_mut(child, f);
    }
    for dependency in schema.dependencies.iter_mut().flat_map(|m| m.values_mut()) {
        if let JSONSchemaPropsOrStringArray::Schema(child) = dependency {
            for_each_schema_mut(child, f);
        }
    }
}

/// Rewrite rustdoc intra-doc links in every description of `crd` as plain
/// code, so `kubectl explain` and the API reference never show `[`X`]`.
pub fn plain_crd_doc_links(crd: &mut CustomResourceDefinition) {
    for root in version_roots(crd) {
        plain_schema_doc_links(root);
    }
}

/// [`plain_crd_doc_links`] for one schema and everything under it.
pub fn plain_schema_doc_links(schema: &mut JSONSchemaProps) {
    for_each_schema_mut(schema, &mut |s| {
        if let Some(description) = s.description.as_mut() {
            *description = plain_doc_links(description);
        }
    });
}

/// Turn rustdoc intra-doc links into inline code.
///
/// The doc comments under `crates/banlieue-api/src/` become CRD
/// descriptions, and rustdoc's link syntax means nothing there: a reader of
/// `kubectl explain` or of the API reference page sees `[`X`]` or a link to a
/// Rust path that goes nowhere. So `[`X`]` and `[`X`](path)` both become
/// `` `X` ``. A link whose target contains `://` is a real URL and is kept.
#[must_use]
pub fn plain_doc_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[`") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + "[`".len()..];
        let Some(code_end) = after_open.find("`]") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let code = &after_open[..code_end];
        let after_close = &after_open[code_end + "`]".len()..];
        if code.contains('`') {
            // Not a single code span: leave the bracket as written.
            out.push_str("[`");
            rest = after_open;
            continue;
        }
        let (target_len, is_url) = match after_close.strip_prefix('(') {
            Some(target) => match target.find(')') {
                Some(end) => (end + "()".len(), target[..end].contains("://")),
                None => (0, false),
            },
            None => (0, false),
        };
        if is_url {
            out.push_str(&rest[start..start + "[`".len() + code_end + "`]".len() + target_len]);
        } else {
            out.push('`');
            out.push_str(code);
            out.push('`');
        }
        rest = &after_close[target_len..];
    }
    out.push_str(rest);
    out
}

/// Remove every `format` the apiserver does not recognise from each
/// version's schema.
///
/// `schemars` names Rust's unsigned integers `uint8` to `uint64`. Kubernetes
/// knows none of them: it drops them from the stored schema, so they
/// validate nothing, and warns on apply (`unrecognized format "uint64"`).
/// The bounds that matter (`minimum: 0`, `maximum`) are separate keywords
/// and stay.
pub fn strip_unsupported_formats(crd: &mut CustomResourceDefinition) {
    for root in version_roots(crd) {
        strip_unsupported_schema_formats(root);
    }
}

/// Whether the apiserver keeps `format` on a `type: string` (or untyped)
/// schema. Dashes are ignored, as the apiserver ignores them.
#[must_use]
pub fn is_supported_string_format(format: &str) -> bool {
    STRING_FORMATS.contains(&format.replace('-', "").as_str())
}

/// [`strip_unsupported_formats`] for one schema and everything under it:
/// properties, items, additional properties and items, the combinators,
/// pattern properties, definitions and schema dependencies.
pub fn strip_unsupported_schema_formats(schema: &mut JSONSchemaProps) {
    for_each_schema_mut(schema, &mut |s| {
        let Some(format) = s.format.as_deref() else {
            return;
        };
        let keep = match s.type_.as_deref() {
            Some("integer") => INTEGER_FORMATS.contains(&format),
            Some("number") => NUMBER_FORMATS.contains(&format),
            None | Some("string") => is_supported_string_format(format),
            Some(_) => false,
        };
        if !keep {
            s.format = None;
        }
    });
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
