// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Every `examples/*.yaml` must parse into the types it names.
//!
//! `rules/documentation.md` says never to guess a field name in an example,
//! and the way that rule gets broken is not carelessness — it is writing
//! `reference:` where the type says `ref:`, which YAML accepts, serde
//! silently drops, and `kubectl apply --dry-run=client` only catches when a
//! cluster with the CRD installed happens to be reachable. This test makes
//! it a compile-time-ish failure instead.
//!
//! The round-trip is what gives it teeth: serde ignores unknown fields by
//! default, so merely deserializing an example proves nothing about a
//! misspelled key. Re-serializing and comparing key sets does.

use banlieue_api::banlieue::{ClaimPhase, VirtualMachineClaim};

/// Repo-relative path to `examples/`, resolved from this crate.
const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");

fn read_example(file: &str) -> serde_yaml::Value {
    let path = format!("{EXAMPLES}/{file}");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Recursively collect `a.b.c` paths of every mapping key.
fn key_paths(value: &serde_yaml::Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                let Some(key) = k.as_str() else { continue };
                let path = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                out.push(path.clone());
                key_paths(v, &path, out);
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                key_paths(item, prefix, out);
            }
        }
        _ => {}
    }
}

/// Assert that re-serializing `parsed` loses none of `original`'s keys —
/// i.e. that every key the example author wrote is a field the type has.
fn assert_no_dropped_keys<T: serde::Serialize>(original: &serde_yaml::Value, parsed: &T) {
    let round_tripped = serde_yaml::to_value(parsed).expect("re-serialize");

    let mut before = Vec::new();
    key_paths(original, "", &mut before);
    let mut after = Vec::new();
    key_paths(&round_tripped, "", &mut after);

    let dropped: Vec<&String> = before.iter().filter(|k| !after.contains(k)).collect();
    assert!(
        dropped.is_empty(),
        "these keys are in the example but not on the type — misspelled, or \
         invented: {dropped:?}"
    );
}

#[test]
fn virtualmachineclaim_example_matches_the_type() {
    let doc = read_example("19-virtualmachineclaim.yaml");
    let claim: VirtualMachineClaim =
        serde_yaml::from_value(doc.clone()).expect("example must parse as a VirtualMachineClaim");

    assert_eq!(claim.spec.pool_ref.name, "sandbox-pool");
    assert_eq!(claim.spec.ttl_seconds, 900);
    assert!(
        claim.spec.subject.issuer.starts_with("https://"),
        "the example issuer should look like an OIDC issuer URL"
    );

    let spec_doc = doc.get("spec").expect("example has a spec");
    assert_no_dropped_keys(spec_doc, &claim.spec);
}

/// The example's documented status block is what a consumer reads, so the
/// field names in it have to be real ones too.
#[test]
fn claim_status_phases_are_the_documented_four() {
    for (phase, token) in [
        (ClaimPhase::Pending, "Pending"),
        (ClaimPhase::Bound, "Bound"),
        (ClaimPhase::Releasing, "Releasing"),
        (ClaimPhase::Failed, "Failed"),
    ] {
        assert_eq!(serde_json::to_value(phase).unwrap(), token);
    }
}

/// Proves the round-trip check above actually bites: a misspelled key must
/// be reported, not silently ignored. Without this, a broken
/// `assert_no_dropped_keys` would make every example test vacuously pass.
#[test]
#[should_panic(expected = "misspelled, or invented")]
fn a_misspelled_key_is_caught() {
    let mut doc = read_example("19-virtualmachineclaim.yaml");
    let spec = doc.get_mut("spec").unwrap().as_mapping_mut().unwrap();
    let pool_ref = spec.get_mut("poolRef").unwrap().as_mapping_mut().unwrap();
    let name = pool_ref.remove("name").unwrap();
    pool_ref.insert("reference".into(), name);

    let spec_doc = doc.get("spec").unwrap().clone();
    // `poolRef.name` is required, so parse the spec leniently for this check.
    let claim: banlieue_api::banlieue::VirtualMachineClaimSpec = serde_yaml::from_value(
        serde_yaml::from_str(
            r#"
poolRef: { name: sandbox-pool }
subject: { issuer: "https://issuer.example.com", id: "x" }
ttlSeconds: 900
"#,
        )
        .unwrap(),
    )
    .unwrap();
    assert_no_dropped_keys(&spec_doc, &claim);
}
