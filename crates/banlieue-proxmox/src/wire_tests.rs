// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `wire.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::Error;

    // ---- Params -------------------------------------------------------

    #[test]
    fn params_encode_as_a_form_body_in_insertion_order() {
        let p = Params::new().set("newid", 101).set("name", "vm one");
        assert_eq!(p.encode(), "newid=101&name=vm+one");
    }

    #[test]
    fn params_escape_reserved_characters() {
        let p = Params::new().set("net0", "virtio,bridge=vmbr0&x=y");
        assert_eq!(p.encode(), "net0=virtio%2Cbridge%3Dvmbr0%26x%3Dy");
    }

    #[test]
    fn flags_are_one_and_zero() {
        let p = Params::new().flag("full", true).flag("purge", false);
        assert_eq!(p.encode(), "full=1&purge=0");
    }

    #[test]
    fn set_opt_skips_none() {
        let p = Params::new()
            .set_opt("pool", None::<&str>)
            .set_opt("name", Some("a"));
        assert_eq!(p.encode(), "name=a");
    }

    #[test]
    fn setting_a_key_twice_replaces_it() {
        let p = Params::new().set("cores", 1).set("cores", 4);
        assert_eq!(p.encode(), "cores=4");
    }

    #[test]
    fn empty_params_encode_to_nothing() {
        assert_eq!(Params::new().encode(), "");
        assert!(Params::new().is_empty());
    }

    #[test]
    fn get_returns_what_was_set() {
        let p = Params::new().set("cores", 4);
        assert_eq!(p.get("cores"), Some("4"));
        assert_eq!(p.get("memory"), None);
    }

    // ---- path segments -----------------------------------------------

    #[test]
    fn plain_segments_pass_through() {
        assert_eq!(encode_segment("pve1"), "pve1");
        assert_eq!(encode_segment("local-lvm"), "local-lvm");
    }

    #[test]
    fn a_volid_keeps_its_colon_and_escapes_its_slash() {
        assert_eq!(
            encode_segment("local:iso/seed 1.iso"),
            "local:iso%2Fseed%201.iso"
        );
    }

    #[test]
    fn dots_and_traversal_cannot_form_a_path() {
        assert_eq!(encode_segment("../x"), "..%2Fx");
    }

    // ---- envelope -----------------------------------------------------

    #[test]
    fn data_is_unwrapped_from_the_envelope() {
        let v: Vec<u32> = decode_data(r#"{"data":[1,2,3]}"#).unwrap();
        assert_eq!(v, [1, 2, 3]);
    }

    #[test]
    fn null_data_decodes_as_unit_and_as_none() {
        decode_data::<()>(r#"{"data":null}"#).unwrap();
        assert_eq!(
            decode_data::<Option<u32>>(r#"{"data":null}"#).unwrap(),
            None
        );
    }

    #[test]
    fn a_missing_data_key_is_a_decode_error() {
        let e = decode_data::<u32>(r#"{}"#).unwrap_err();
        assert!(matches!(e, Error::Decode(_)), "{e}");
    }

    #[test]
    fn a_body_that_is_not_json_is_a_decode_error() {
        assert!(matches!(
            decode_data::<u32>("<html>"),
            Err(Error::Decode(_))
        ));
    }

    #[test]
    fn a_wrong_shape_is_a_decode_error() {
        assert!(matches!(
            decode_data::<Vec<u32>>(r#"{"data":"x"}"#),
            Err(Error::Decode(_))
        ));
    }

    // ---- error messages ----------------------------------------------

    #[test]
    fn the_reason_phrase_is_the_message() {
        assert_eq!(
            api_message("Authentication failed!", ""),
            "Authentication failed!"
        );
    }

    #[test]
    fn validation_errors_from_the_body_are_appended_sorted() {
        let body = r#"{"data":null,"errors":{"memory":"value must be >= 16","cores":"value must be an integer"}}"#;
        assert_eq!(
            api_message("Parameter verification failed.", body),
            "Parameter verification failed.: cores: value must be an integer; memory: value must be >= 16"
        );
    }

    #[test]
    fn an_empty_reason_falls_back_to_the_body_message() {
        assert_eq!(api_message("", r#"{"message":"oops"}"#), "oops");
    }

    #[test]
    fn nothing_at_all_still_says_something() {
        assert_eq!(api_message("", ""), "no message from Proxmox");
    }

    #[test]
    fn a_non_json_body_is_ignored_when_a_reason_exists() {
        assert_eq!(api_message("Bad", "<html>"), "Bad");
    }
}
