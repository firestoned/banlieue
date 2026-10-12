// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `yaml.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::json;

    fn no_trailing_whitespace(yaml: &str) {
        for (n, line) in yaml.lines().enumerate() {
            assert_eq!(line, line.trim_end(), "line {} ends in whitespace", n + 1);
        }
    }

    #[test]
    fn a_long_single_line_string_stays_on_one_line() {
        let long = "word ".repeat(40).trim_end().to_string();
        let yaml = to_string(&json!({ "description": long })).unwrap();
        assert_eq!(yaml.lines().count(), 1, "{yaml}");
        assert!(!yaml.contains(">-"), "{yaml}");
    }

    #[test]
    fn blank_lines_inside_a_block_scalar_carry_no_indentation() {
        let value = json!({ "spec": { "description": "First paragraph.\n\nSecond paragraph." } });
        let yaml = to_string(&value).unwrap();
        assert!(
            yaml.contains("|-"),
            "multi-line strings stay literal: {yaml}"
        );
        no_trailing_whitespace(&yaml);
        let back: serde_json::Value = serde_saphyr::from_str(&yaml).unwrap();
        assert_eq!(back, value);
    }

    /// A line of spaces that is content, not indentation, must survive.
    #[test]
    fn a_whitespace_only_content_line_is_kept() {
        let value = json!({ "text": "a\n      \nb" });
        let yaml = to_string(&value).unwrap();
        let back: serde_json::Value = serde_saphyr::from_str(&yaml).unwrap();
        assert_eq!(back, value);
    }

    /// Every CRD banlieue generates round-trips unchanged, with no trailing
    /// whitespace: the output `crdgen` and `banlieue bootstrap` write.
    #[test]
    fn every_generated_crd_round_trips_without_trailing_whitespace() {
        for crd in crate::crdgen_support::all_crds() {
            let yaml = to_string(&crd).unwrap();
            no_trailing_whitespace(&yaml);
            let back: serde_json::Value = serde_saphyr::from_str(&yaml).unwrap();
            assert_eq!(back, serde_json::to_value(&crd).unwrap());
        }
    }
}
