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

    // ---- strip_unsupported_formats ----

    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
        JSONSchemaPropsOrArray, JSONSchemaPropsOrBool,
    };

    fn typed(type_: &str, format: &str) -> JSONSchemaProps {
        JSONSchemaProps {
            type_: Some(type_.into()),
            format: Some(format.into()),
            ..JSONSchemaProps::default()
        }
    }

    fn format_after(type_: &str, format: &str) -> Option<String> {
        let mut schema = typed(type_, format);
        strip_unsupported_schema_formats(&mut schema);
        schema.format
    }

    /// Integers keep only `int32` and `int64`: the apiserver drops schemars'
    /// unsigned formats, and warns about them on apply.
    #[test]
    fn integer_formats_keep_only_int32_and_int64() {
        for kept in ["int32", "int64"] {
            assert_eq!(format_after("integer", kept).as_deref(), Some(kept));
        }
        for dropped in ["uint8", "uint16", "uint32", "uint64", "uint", "float"] {
            assert_eq!(format_after("integer", dropped), None, "{dropped}");
        }
    }

    #[test]
    fn number_formats_keep_only_float_and_double() {
        for kept in ["float", "double"] {
            assert_eq!(format_after("number", kept).as_deref(), Some(kept));
        }
        assert_eq!(format_after("number", "int64"), None);
    }

    /// Strings keep the formats Kubernetes knows, spelled with or without
    /// dashes, and lose the rest.
    #[test]
    fn string_formats_keep_the_kubernetes_set() {
        for kept in [
            "date-time",
            "datetime",
            "uuid",
            "byte",
            "ipv4",
            "k8s-short-name",
        ] {
            assert_eq!(format_after("string", kept).as_deref(), Some(kept));
        }
        for dropped in ["uint32", "json", "int64"] {
            assert_eq!(format_after("string", dropped), None, "{dropped}");
        }
    }

    #[test]
    fn other_types_lose_any_format() {
        assert_eq!(format_after("boolean", "int32"), None);
        assert_eq!(format_after("object", "date-time"), None);
    }

    /// Nested schemas are cleaned too, and a *property* named `format` is a
    /// field, not the keyword: it stays.
    #[test]
    fn nested_schemas_are_cleaned_and_a_property_named_format_stays() {
        let mut root = JSONSchemaProps {
            type_: Some("object".into()),
            properties: Some(BTreeMap::from([
                ("format".to_string(), typed("string", "date-time")),
                (
                    "list".to_string(),
                    JSONSchemaProps {
                        type_: Some("array".into()),
                        items: Some(JSONSchemaPropsOrArray::Schema(Box::new(typed(
                            "integer", "uint64",
                        )))),
                        ..JSONSchemaProps::default()
                    },
                ),
                (
                    "map".to_string(),
                    JSONSchemaProps {
                        type_: Some("object".into()),
                        additional_properties: Some(JSONSchemaPropsOrBool::Schema(Box::new(
                            typed("integer", "uint32"),
                        ))),
                        ..JSONSchemaProps::default()
                    },
                ),
                (
                    "either".to_string(),
                    JSONSchemaProps {
                        any_of: Some(vec![typed("integer", "uint8"), typed("string", "uuid")]),
                        ..JSONSchemaProps::default()
                    },
                ),
            ])),
            ..JSONSchemaProps::default()
        };
        strip_unsupported_schema_formats(&mut root);
        let props = root.properties.as_ref().unwrap();
        assert!(props.contains_key("format"), "a field named format stays");
        assert_eq!(props["format"].format.as_deref(), Some("date-time"));
        let Some(JSONSchemaPropsOrArray::Schema(item)) = &props["list"].items else {
            panic!("items");
        };
        assert_eq!(item.format, None);
        let Some(JSONSchemaPropsOrBool::Schema(value)) = &props["map"].additional_properties else {
            panic!("additionalProperties");
        };
        assert_eq!(value.format, None);
        let any = props["either"].any_of.as_ref().unwrap();
        assert_eq!(any[0].format, None);
        assert_eq!(any[1].format.as_deref(), Some("uuid"));
    }

    /// Every format a shipped CRD declares, collected recursively.
    fn formats(schema: &JSONSchemaProps, out: &mut Vec<(String, String)>) {
        if let Some(f) = &schema.format {
            out.push((schema.type_.clone().unwrap_or_default(), f.clone()));
        }
        let children = schema
            .properties
            .iter()
            .flat_map(|m| m.values())
            .chain(schema.items.iter().flat_map(|i| match i {
                JSONSchemaPropsOrArray::Schema(s) => std::slice::from_ref(s.as_ref()),
                JSONSchemaPropsOrArray::Schemas(v) => v.as_slice(),
            }))
            .chain(schema.additional_properties.iter().filter_map(|a| match a {
                JSONSchemaPropsOrBool::Schema(s) => Some(s.as_ref()),
                JSONSchemaPropsOrBool::Bool(_) => None,
            }))
            .chain(schema.any_of.iter().flatten())
            .chain(schema.all_of.iter().flatten())
            .chain(schema.one_of.iter().flatten());
        for child in children {
            formats(child, out);
        }
    }

    /// The regression guard: no CRD banlieue installs carries a format the
    /// apiserver would drop and warn about (`unrecognized format "uint64"`
    /// on `banlieue bootstrap` against a real cluster).
    #[test]
    fn no_shipped_crd_carries_an_unsupported_format() {
        let mut seen = Vec::new();
        for crd in all_crds() {
            for version in &crd.spec.versions {
                let root = version
                    .schema
                    .as_ref()
                    .and_then(|s| s.open_api_v3_schema.as_ref())
                    .expect("schema");
                let mut found = Vec::new();
                formats(root, &mut found);
                for (type_, format) in found {
                    let ok = match type_.as_str() {
                        "integer" => ["int32", "int64"].contains(&format.as_str()),
                        "number" => ["float", "double"].contains(&format.as_str()),
                        "string" | "" => is_supported_string_format(&format),
                        _ => false,
                    };
                    if !ok {
                        seen.push(format!("{}: {type_}/{format}", crd.spec.names.kind));
                    }
                }
            }
        }
        assert!(seen.is_empty(), "unsupported formats: {seen:?}");
    }
}
