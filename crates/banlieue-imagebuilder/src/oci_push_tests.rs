// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `oci_push.rs`. The push itself runs against a real
//! registry in `banlieue-oci`'s live tests.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn annotations_parse_as_key_value_and_values_may_contain_equals() {
        let m = parse_annotations(&[
            "io.banlieue.vmimage=kairos".to_string(),
            "note=a=b".to_string(),
        ])
        .unwrap();
        assert_eq!(m["io.banlieue.vmimage"], "kairos");
        assert_eq!(m["note"], "a=b");
        assert!(parse_annotations(&["novalue".to_string()]).is_err());
        assert!(parse_annotations(&["=v".to_string()]).is_err());
    }
}
