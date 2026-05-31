// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Tests for the crdgen post-generation fix-ups.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::banlieue::{Provider, VMClass};
    use crate::infrastructure::{VSphereCluster, VSphereMachine, VSphereMachineTemplate};
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::JSONSchemaProps;
    use kube::CustomResourceExt;
    use std::collections::BTreeMap;

    const AUTOGEN_DESC: &str = "Auto-generated derived type for VMClassSpec via `CustomResource`";

    // Marker prefix authored on the VMClassSpec doc comment.
    const VMCLASS_DESC_PREFIX: &str = "VMClass —";

    /// The authored spec doc comment should replace the kube-derive boilerplate
    /// at the CRD root after `prepared`.
    #[test]
    fn promote_spec_description_replaces_root_boilerplate() {
        // Arrange: raw CRD straight from kube-derive carries the boilerplate.
        let raw = VMClass::crd();
        let raw_root = raw.spec.versions[0]
            .schema
            .as_ref()
            .and_then(|s| s.open_api_v3_schema.as_ref())
            .and_then(|root| root.description.clone())
            .expect("root description present");
        assert_eq!(raw_root, AUTOGEN_DESC, "precondition: boilerplate root");

        // Act.
        let crd = prepared(VMClass::crd());

        // Assert: root now carries the authored spec description.
        let root = crd.spec.versions[0]
            .schema
            .as_ref()
            .and_then(|s| s.open_api_v3_schema.as_ref())
            .and_then(|root| root.description.clone())
            .expect("root description present");
        assert!(
            root.starts_with(VMCLASS_DESC_PREFIX),
            "root description should be the authored spec doc, got: {root}"
        );
        assert_ne!(root, AUTOGEN_DESC, "boilerplate should be gone");
    }

    /// A version whose spec has no description keeps its existing root.
    #[test]
    fn promote_spec_description_noop_without_spec_description() {
        // Arrange: hand-build a CRD whose spec property has no description.
        let mut crd = VMClass::crd();
        let original_root = "original root description".to_string();
        let mut properties = BTreeMap::new();
        properties.insert(SPEC_PROPERTY.to_string(), JSONSchemaProps::default());
        let root = crd.spec.versions[0]
            .schema
            .as_mut()
            .and_then(|s| s.open_api_v3_schema.as_mut())
            .expect("schema present");
        root.description = Some(original_root.clone());
        root.properties = Some(properties);

        // Act.
        promote_spec_description(&mut crd);

        // Assert: root untouched because spec carried no description.
        let after = crd.spec.versions[0]
            .schema
            .as_ref()
            .and_then(|s| s.open_api_v3_schema.as_ref())
            .and_then(|root| root.description.clone())
            .expect("root description present");
        assert_eq!(after, original_root);
    }

    // ----------------------------------------------------------------------
    // add_capi_contract_label (ADR-0005)
    // ----------------------------------------------------------------------

    fn contract_label(
        crd: &k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    ) -> Option<String> {
        crd.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get(CAPI_V1BETA2_LABEL))
            .cloned()
    }

    #[test]
    fn infra_machine_gets_contract_label_with_served_version() {
        let crd = prepared(VSphereMachine::crd());
        assert_eq!(contract_label(&crd).as_deref(), Some("v1alpha1"));
    }

    #[test]
    fn infra_cluster_gets_contract_label() {
        let crd = prepared(VSphereCluster::crd());
        assert_eq!(contract_label(&crd).as_deref(), Some("v1alpha1"));
    }

    #[test]
    fn infra_machine_template_gets_contract_label() {
        // The template CRD has no status but still implements the contract.
        let crd = prepared(VSphereMachineTemplate::crd());
        assert_eq!(contract_label(&crd).as_deref(), Some("v1alpha1"));
    }

    #[test]
    fn user_facing_group_is_not_labelled() {
        // banlieue.io CRDs are not CAPI contract surfaces.
        assert_eq!(contract_label(&prepared(VMClass::crd())), None);
        assert_eq!(contract_label(&prepared(Provider::crd())), None);
    }

    #[test]
    fn label_key_targets_the_v1beta2_contract() {
        // Guard the exact key CAPI looks for.
        assert_eq!(CAPI_V1BETA2_LABEL, "cluster.x-k8s.io/v1beta2");
    }
}
