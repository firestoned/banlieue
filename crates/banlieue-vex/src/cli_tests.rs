// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `cli` module.

use super::*;
use crate::auto_vex_presence::build_document;

const TIMESTAMP: &str = "2026-04-22T00:00:00Z";

#[test]
fn load_grype_parses_a_report() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("grype.json");
    std::fs::write(&path, r#"{"matches": []}"#).unwrap();
    assert!(load_grype(&path).unwrap().matches.is_empty());
}

#[test]
fn load_grype_names_the_flag_and_path_of_a_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("absent.json");
    let err = format!("{:#}", load_grype(&path).unwrap_err());
    let expected = format!("failed to read --grype-json {}: ", path.display());
    assert!(err.starts_with(&expected), "{err}");
}

#[test]
fn load_grype_names_the_flag_and_path_of_a_malformed_report() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("grype.json");
    std::fs::write(&path, "not json").unwrap();
    let err = format!("{:#}", load_grype(&path).unwrap_err());
    let expected = format!("failed to parse --grype-json {}: ", path.display());
    assert!(err.starts_with(&expected), "{err}");
}

#[test]
fn timestamp_or_now_keeps_a_given_timestamp() {
    assert_eq!(timestamp_or_now(Some(TIMESTAMP.to_string())), TIMESTAMP);
}

#[test]
fn timestamp_or_now_defaults_to_rfc3339_utc_seconds() {
    let now = timestamp_or_now(None);
    assert_eq!(now.len(), TIMESTAMP.len(), "{now}");
    assert!(now.ends_with('Z'), "{now}");
    assert!(now.parse::<jiff::Timestamp>().is_ok(), "{now}");
}

#[test]
fn write_document_writes_pretty_json_with_a_trailing_newline() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("vex.json");
    let doc = build_document(Vec::new(), "https://example.com/run/1", "author", TIMESTAMP);
    write_document(&doc, Some(&path)).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, serde_json::to_string_pretty(&doc).unwrap() + "\n");
}

#[test]
fn write_document_names_the_flag_and_path_it_could_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("no-such-dir").join("vex.json");
    let doc = build_document(Vec::new(), "https://example.com/run/1", "author", TIMESTAMP);
    let err = format!("{:#}", write_document(&doc, Some(&path)).unwrap_err());
    let expected = format!("failed to write --output {}: ", path.display());
    assert!(err.starts_with(&expected), "{err}");
}
