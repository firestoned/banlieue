// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Tests for the CRD Markdown reference generator.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::banlieue::{VMClass, VirtualMachine};
    use crate::crdgen_support::prepared;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
        JSON, JSONSchemaProps, JSONSchemaPropsOrArray, JSONSchemaPropsOrBool,
    };
    use kube::CustomResourceExt;

    fn scalar(type_: &str) -> JSONSchemaProps {
        JSONSchemaProps {
            type_: Some(type_.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn type_label_scalar() {
        assert_eq!(type_label(&scalar("string")), "string");
        assert_eq!(type_label(&scalar("integer")), "integer");
    }

    #[test]
    fn type_label_array_of_scalars() {
        let schema = JSONSchemaProps {
            type_: Some("array".to_string()),
            items: Some(JSONSchemaPropsOrArray::Schema(Box::new(scalar("string")))),
            ..Default::default()
        };
        assert_eq!(type_label(&schema), "string[]");
    }

    #[test]
    fn type_label_map_of_strings() {
        let schema = JSONSchemaProps {
            type_: Some("object".to_string()),
            additional_properties: Some(JSONSchemaPropsOrBool::Schema(Box::new(scalar("string")))),
            ..Default::default()
        };
        assert_eq!(type_label(&schema), "map[string]string");
    }

    #[test]
    fn type_label_object_with_properties() {
        let mut schema = scalar("object");
        schema.properties = Some(Default::default());
        assert_eq!(type_label(&schema), "object");
    }

    #[test]
    fn resolve_object_direct_array_and_none() {
        // Direct object.
        let mut obj = scalar("object");
        obj.properties = Some(Default::default());
        assert_eq!(resolve_object(&obj).map(|(_, s)| s), Some(""));

        // Array of objects → "[]" suffix.
        let mut item = scalar("object");
        item.properties = Some(Default::default());
        let arr = JSONSchemaProps {
            type_: Some("array".to_string()),
            items: Some(JSONSchemaPropsOrArray::Schema(Box::new(item))),
            ..Default::default()
        };
        assert_eq!(resolve_object(&arr).map(|(_, s)| s), Some("[]"));

        // Scalar → no nested object.
        assert!(resolve_object(&scalar("string")).is_none());
    }

    #[test]
    fn cell_description_collapses_and_appends_enum() {
        let schema = JSONSchemaProps {
            description: Some("First line.\n\nSecond paragraph ignored.".to_string()),
            enum_: Some(vec![
                JSON(serde_json::json!("Thin")),
                JSON(serde_json::json!("Thick")),
            ]),
            ..Default::default()
        };
        let cell = cell_description(&schema);
        assert!(cell.starts_with("First line."), "got: {cell}");
        assert!(!cell.contains("Second paragraph"), "only first paragraph");
        assert!(cell.contains("Allowed: `Thin`, `Thick`."), "got: {cell}");
    }

    #[test]
    fn escape_cell_escapes_pipes_and_newlines() {
        assert_eq!(escape_cell("a | b\nc"), "a \\| b c");
    }

    #[test]
    fn heading_anchor_lowercases() {
        assert_eq!(heading_anchor("VirtualMachine"), "virtualmachine");
    }

    #[test]
    fn prose_demotes_headings_to_bold() {
        let input = "Intro line.\n\n# Why create one\n\nBody text.\n## How it is used";
        let rendered = prose(input);
        assert!(rendered.contains("**Why create one**"), "got: {rendered}");
        assert!(rendered.contains("**How it is used**"), "got: {rendered}");
        assert!(!rendered.contains("# Why"), "no raw ATX headings remain");
        assert!(
            rendered.contains("Body text."),
            "non-heading lines preserved"
        );
    }

    #[test]
    fn render_reference_documents_each_crd() {
        let crds = vec![prepared(VMClass::crd()), prepared(VirtualMachine::crd())];
        let md = render_reference(&crds);

        // Page scaffolding + grouped index.
        assert!(md.starts_with("# API Reference"));
        assert!(md.contains("[VMClass](#vmclass)"));
        assert!(md.contains("[VirtualMachine](#virtualmachine)"));

        // Per-CRD heading + the authored root description (promoted to root).
        assert!(md.contains("## VMClass"));
        assert!(md.contains("VMClass — a reusable, cluster-scoped catalog"));

        // Field table + a nested object section.
        assert!(md.contains("| Field | Type | Required | Description |"));
        assert!(md.contains("### `.spec`"));
        assert!(md.contains("`.spec.hardware`"), "nested object section");
        assert!(md.contains("`cpus`"), "leaf field documented");
    }
}
