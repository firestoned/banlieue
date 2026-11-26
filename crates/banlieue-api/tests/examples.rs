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

use banlieue_api::banlieue::{ClaimPhase, Provider, VirtualMachineClaim};

/// Repo-relative path to `examples/`, resolved from this crate.
const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");

fn read_example(file: &str) -> serde_json::Value {
    let path = format!("{EXAMPLES}/{file}");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_saphyr::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The documents of a multi-document YAML file: split on `---` lines, with
/// empty and comment-only documents (a license header) dropped.
fn yaml_documents(text: &str) -> Vec<String> {
    let mut docs = vec![String::new()];
    for line in text.lines() {
        if line.trim_end() == "---" {
            docs.push(String::new());
            continue;
        }
        if let Some(doc) = docs.last_mut() {
            doc.push_str(line);
            doc.push('\n');
        }
    }
    docs.into_iter()
        .filter(|d| {
            d.lines()
                .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        })
        .collect()
}

/// Recursively collect `a.b.c` paths of every mapping key.
fn key_paths(value: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map {
                let path = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                out.push(path.clone());
                key_paths(v, &path, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                key_paths(item, prefix, out);
            }
        }
        _ => {}
    }
}

/// Assert that re-serializing `parsed` loses none of `original`'s keys —
/// i.e. that every key the example author wrote is a field the type has.
fn assert_no_dropped_keys<T: serde::Serialize>(original: &serde_json::Value, parsed: &T) {
    let round_tripped = serde_json::to_value(parsed).expect("re-serialize");

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
    let doc = read_example("20-virtualmachineclaim.yaml");
    let claim: VirtualMachineClaim =
        serde_json::from_value(doc.clone()).expect("example must parse as a VirtualMachineClaim");

    assert_eq!(
        claim.spec.pool_ref.as_ref().map(|r| r.name.as_str()),
        Some("sandbox-pool")
    );
    assert!(claim.spec.pool_selector.is_none());
    assert_eq!(claim.spec.ttl_seconds, 900);
    assert!(
        claim.spec.subject.issuer.starts_with("https://"),
        "the example issuer should look like an OIDC issuer URL"
    );

    let spec_doc = doc.get("spec").expect("example has a spec");
    assert_no_dropped_keys(spec_doc, &claim.spec);
}

/// The selector example takes exactly one of the two pool fields, which the
/// CRD's CEL rule enforces on a real API server (ADR-0132).
#[test]
fn virtualmachineclaim_selector_example_matches_the_type() {
    let doc = read_example("27-virtualmachineclaim-pool-selector.yaml");
    let claim: VirtualMachineClaim =
        serde_json::from_value(doc.clone()).expect("example must parse as a VirtualMachineClaim");

    assert!(claim.spec.pool_ref.is_none(), "exactly one pool field");
    let selector = claim.spec.pool_selector.as_ref().expect("a poolSelector");
    assert!(
        !selector.match_labels.is_empty(),
        "the example should select by a label"
    );

    let spec_doc = doc.get("spec").expect("example has a spec");
    assert_no_dropped_keys(spec_doc, &claim.spec);
}

/// The strategy example spreads one subject's sandboxes across failure
/// domains (ADR-0135). A misspelled `selection` would be dropped by serde and
/// the claim would silently rank as `LongestReady`, so the round-trip matters.
#[test]
fn virtualmachineclaim_spread_example_selects_a_strategy() {
    use banlieue_api::banlieue::SelectionStrategy;
    let doc = read_example("29-virtualmachineclaim-spread.yaml");
    let claim: VirtualMachineClaim =
        serde_json::from_value(doc.clone()).expect("example must parse as a VirtualMachineClaim");

    assert_eq!(claim.spec.strategy(), SelectionStrategy::Spread);
    assert!(
        claim.spec.pool_selector.is_some(),
        "spread is about several pools' members"
    );

    let spec_doc = doc.get("spec").expect("example has a spec");
    assert_no_dropped_keys(spec_doc, &claim.spec);
}

/// The shared-provider example declares the namespaces it serves (ADR-0133).
/// A misspelled `namespaceSelector` would be silently dropped by serde and the
/// Provider would serve its own namespace only, so the round-trip matters.
#[test]
fn shared_provider_example_declares_a_namespace_selector() {
    let doc = read_example("28-provider-vsphere-shared.yaml");
    let provider: Provider =
        serde_json::from_value(doc.clone()).expect("example must parse as a Provider");

    let selector = provider
        .spec
        .namespace_selector
        .as_ref()
        .expect("a namespaceSelector");
    assert!(
        !selector.is_empty(),
        "an empty selector serves nothing, so the example must select by a label"
    );
    assert_eq!(
        selector
            .match_labels
            .get("banlieue.io/tenant")
            .map(String::as_str),
        Some("true")
    );

    let spec_doc = doc.get("spec").expect("example has a spec");
    assert_no_dropped_keys(spec_doc, &provider.spec);
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
    let mut doc = read_example("20-virtualmachineclaim.yaml");
    let spec = doc.get_mut("spec").unwrap().as_object_mut().unwrap();
    let pool_ref = spec.get_mut("poolRef").unwrap().as_object_mut().unwrap();
    let name = pool_ref.remove("name").unwrap();
    pool_ref.insert("reference".into(), name);

    let spec_doc = doc.get("spec").unwrap().clone();
    // `poolRef.name` is required, so parse a well-formed spec for this check.
    let claim: banlieue_api::banlieue::VirtualMachineClaimSpec = serde_json::from_value(
        serde_saphyr::from_str(
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

/// Every document in `22-virtualmachine-proxmox.yaml` must parse into the
/// type its `kind` names, and no key may be dropped on the way. The Provider
/// also has to declare the storage class the controller looks up by its
/// well-known name for the seed ISO (ADR-0075 Decision 5).
#[test]
fn proxmox_example_matches_the_types() {
    use banlieue_api::banlieue::{Provider, VMClass, VMImage, VirtualMachine};

    let path = format!("{EXAMPLES}/22-virtualmachine-proxmox.yaml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut seen = Vec::new();

    for text in yaml_documents(&text) {
        let doc: serde_json::Value = serde_saphyr::from_str(&text).expect("valid YAML");
        let kind = doc["kind"].as_str().expect("kind").to_string();
        let spec = doc.get("spec").cloned();
        match kind.as_str() {
            "Provider" => {
                let p: Provider = serde_json::from_value(doc.clone()).expect("Provider");
                assert_no_dropped_keys(&spec.unwrap(), &p.spec);
                let classes = &p.spec.capabilities.storage_classes;
                assert!(
                    classes.iter().any(|c| c.name == "seed-iso"),
                    "the example must declare the `seed-iso` storage class"
                );
                assert!(
                    classes
                        .iter()
                        .all(|c| c.target.as_ref().is_some_and(|t| t.contains_key("storage"))),
                    "Proxmox storage classes name a `storage` target"
                );
            }
            "VMImage" => {
                let i: VMImage = serde_json::from_value(doc.clone()).expect("VMImage");
                assert_no_dropped_keys(&spec.unwrap(), &i.spec);
                assert!(
                    i.spec.sources.iter().any(|s| s.provider_class == "proxmox"),
                    "the image needs a proxmox source"
                );
            }
            "VMClass" => {
                let c: VMClass = serde_json::from_value(doc.clone()).expect("VMClass");
                assert_no_dropped_keys(&spec.unwrap(), &c.spec);
            }
            "VirtualMachine" => {
                let v: VirtualMachine =
                    serde_json::from_value(doc.clone()).expect("VirtualMachine");
                assert_no_dropped_keys(&spec.unwrap(), &v.spec);
            }
            _ => {}
        }
        seen.push(kind);
    }
    for kind in ["Provider", "VMImage", "VMClass", "VirtualMachine"] {
        assert!(seen.iter().any(|k| k == kind), "example lacks a {kind}");
    }
}

/// Every document in `31-virtualmachine-azure.yaml` must parse into the type
/// its `kind` names with no dropped key. The Provider carries no credential
/// (ADR-0159 Decision 1), names its Azure identity, and maps the class the
/// VirtualMachine uses to an instance size (ADR-0160 Decision 1).
#[test]
fn azure_example_matches_the_types() {
    use banlieue_api::banlieue::{Provider, VMClass, VMImage, VirtualMachine};

    let path = format!("{EXAMPLES}/31-virtualmachine-azure.yaml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut seen = Vec::new();
    let mut sized = Vec::new();
    let mut class_names = Vec::new();

    for text in yaml_documents(&text) {
        let doc: serde_json::Value = serde_saphyr::from_str(&text).expect("valid YAML");
        let kind = doc["kind"].as_str().expect("kind").to_string();
        let spec = doc.get("spec").cloned();
        match kind.as_str() {
            "Provider" => {
                let p: Provider = serde_json::from_value(doc.clone()).expect("Provider");
                assert_no_dropped_keys(&spec.unwrap(), &p.spec);
                assert_eq!(p.spec.provider_class_ref.name, "azure");
                assert!(
                    p.spec.connection.credentials_ref.is_none(),
                    "an azure Provider must not name a Secret (ADR-0159 Decision 1)"
                );
                assert!(p.spec.connection.endpoint.starts_with("https://"));
                let azure = p.spec.connection.azure.as_ref().expect("connection.azure");
                assert_eq!(azure.location, "canadacentral");
                assert_eq!(azure.resource_group, "banlieue-vms");
                let caps = &p.spec.capabilities;
                assert!(
                    caps.storage_classes
                        .iter()
                        .all(|c| c.target.as_ref().is_some_and(|t| t.contains_key("sku"))),
                    "Azure storage classes name a managed-disk `sku`"
                );
                assert!(
                    caps.network_classes.iter().all(|c| c
                        .target
                        .as_ref()
                        .is_some_and(|t| t.contains_key("subnetId"))),
                    "Azure network classes name a `subnetId`"
                );
                sized.extend(caps.instance_sizes.iter().map(|s| s.vm_class.clone()));
            }
            "VMImage" => {
                let i: VMImage = serde_json::from_value(doc.clone()).expect("VMImage");
                assert_no_dropped_keys(&spec.unwrap(), &i.spec);
                assert!(
                    i.spec.sources.iter().any(|s| s.provider_class == "azure"
                        && s.reference
                            .contains("/providers/Microsoft.Compute/galleries/")),
                    "the image needs an azure gallery image version source"
                );
            }
            "VMClass" => {
                let c: VMClass = serde_json::from_value(doc.clone()).expect("VMClass");
                assert_no_dropped_keys(&spec.unwrap(), &c.spec);
                class_names.push(c.metadata.name.clone().unwrap_or_default());
            }
            "VirtualMachine" => {
                let v: VirtualMachine =
                    serde_json::from_value(doc.clone()).expect("VirtualMachine");
                assert_no_dropped_keys(&spec.unwrap(), &v.spec);
                assert!(v.spec.user_data.is_some(), "the example shows userData");
            }
            _ => {}
        }
        seen.push(kind);
    }
    for kind in ["Provider", "VMImage", "VMClass", "VirtualMachine"] {
        assert!(seen.iter().any(|k| k == kind), "example lacks a {kind}");
    }
    for class in &class_names {
        assert!(
            sized.contains(class),
            "VMClass {class} has no instance size"
        );
    }
}

/// The Cloud Hypervisor Provider example (ADR-0060, ADR-0107): it must parse
/// with no dropped key, carry no credential (an External provider reads no
/// Secret), map every class to a `hostClass`, and show the scheduling knob
/// inside the range the CRD enforces.
#[test]
fn cloud_hypervisor_provider_example_matches_the_type() {
    use banlieue_api::banlieue::{MAX_CPU_OVERCOMMIT_RATIO, MIN_CPU_OVERCOMMIT_RATIO, Provider};

    let doc = read_example("01-provider-cloud-hypervisor.yaml");
    let p: Provider = serde_json::from_value(doc.clone()).expect("a Provider");
    assert_no_dropped_keys(&doc["spec"], &p.spec);

    assert_eq!(p.spec.provider_class_ref.name, "cloud-hypervisor");
    assert!(
        p.spec.connection.credentials_ref.is_none(),
        "a Cloud Hypervisor Provider must not name a Secret (ADR-0060 Decision 4)"
    );
    let caps = &p.spec.capabilities;
    assert!(!caps.storage_classes.is_empty() && !caps.network_classes.is_empty());
    for target in caps
        .storage_classes
        .iter()
        .map(|c| &c.target)
        .chain(caps.network_classes.iter().map(|c| &c.target))
    {
        assert!(
            target.as_ref().is_some_and(|t| t.contains_key("hostClass")),
            "every class maps to a host class, got {target:?}"
        );
    }
    let ratio = p
        .spec
        .scheduling
        .as_ref()
        .and_then(|s| s.cpu_overcommit_ratio)
        .expect("the example shows spec.scheduling.cpuOvercommitRatio");
    assert!((MIN_CPU_OVERCOMMIT_RATIO..=MAX_CPU_OVERCOMMIT_RATIO).contains(&ratio));
}

/// The isolated sandbox segment (ADR-0109): every document parses with no
/// dropped key, and the example keeps the decisions that make the segment
/// isolated. The gateway class puts `egress` first (static, with the default
/// route, Decision 6) and `sandbox` second on the segment; a sandbox has one
/// NIC, on the segment; the pool draws addresses with **no gateway**
/// (Decision 7), so a member has no default route.
#[test]
fn isolated_segment_example_matches_the_types() {
    use banlieue_api::banlieue::{Provider, VMClass, VirtualMachine, VirtualMachinePool};

    const SEGMENT: &str = "sandbox-segment";
    let path = format!("{EXAMPLES}/25-isolated-sandbox-segment.yaml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut seen = Vec::new();

    for text in yaml_documents(&text) {
        let doc: serde_json::Value = serde_saphyr::from_str(&text).expect("valid YAML");
        let kind = doc["kind"].as_str().expect("kind").to_string();
        let name = doc["metadata"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let spec = doc.get("spec").cloned();
        match kind.as_str() {
            "Provider" => {
                let p: Provider = serde_json::from_value(doc.clone()).expect("Provider");
                assert_no_dropped_keys(&spec.unwrap(), &p.spec);
                let names: Vec<&str> = p
                    .spec
                    .capabilities
                    .network_classes
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect();
                assert!(
                    names.contains(&SEGMENT),
                    "the Provider must offer the segment: {names:?}"
                );
                assert!(
                    names.len() >= 2,
                    "and a routable class for the gateways' egress"
                );
            }
            "VMClass" => {
                let c: VMClass = serde_json::from_value(doc.clone()).expect("VMClass");
                assert_no_dropped_keys(&spec.unwrap(), &c.spec);
                let nics = &c.spec.network.interfaces;
                if name.contains("gateway") {
                    assert_eq!(nics.len(), 2, "a gateway has two NICs");
                    assert_ne!(nics[0].network_class, SEGMENT, "egress first");
                    assert!(
                        nics[0]
                            .ipam
                            .static_
                            .as_ref()
                            .is_some_and(|s| s.gateway.is_some()),
                        "egress is static and carries the default route"
                    );
                    assert_eq!(
                        nics[1].network_class, SEGMENT,
                        "sandbox second, on the segment"
                    );
                } else {
                    assert_eq!(nics.len(), 1, "a sandbox has one NIC");
                    assert_eq!(nics[0].network_class, SEGMENT);
                }
            }
            "VirtualMachine" => {
                let v: VirtualMachine =
                    serde_json::from_value(doc.clone()).expect("VirtualMachine");
                assert_no_dropped_keys(&spec.unwrap(), &v.spec);
            }
            "VirtualMachinePool" => {
                let pool: VirtualMachinePool =
                    serde_json::from_value(doc.clone()).expect("VirtualMachinePool");
                assert_no_dropped_keys(&spec.unwrap(), &pool.spec);
                let addressing = pool.spec.addressing.as_ref().expect("static addressing");
                assert!(
                    addressing.gateway.is_none(),
                    "a sandbox member has no default route (ADR-0109 Decision 7)"
                );
            }
            _ => {}
        }
        seen.push(kind);
    }
    for kind in [
        "Provider",
        "VMClass",
        "VirtualMachine",
        "VirtualMachinePool",
        "ConfigMap",
    ] {
        assert!(seen.iter().any(|k| k == kind), "example lacks a {kind}");
    }
}

/// `26-capi-machinedeployment-cloud-hypervisor.yaml` (ADR-0129): the
/// CloudHypervisorMachineTemplate parses with no dropped key, carries no
/// `userData` (on a CAPI-owned machine the bootstrap data replaces it), and
/// sits in the install namespace with the MachineDeployment, since a Cloud
/// Hypervisor host serves only its Provider's namespace. The MachineDeployment
/// gets its data from a bootstrap config, never a hand-named Secret.
#[test]
fn capi_cloud_hypervisor_example_matches_the_types() {
    use banlieue_api::infrastructure::CloudHypervisorMachineTemplate;

    const NAMESPACE: &str = "banlieue-system";
    let path = format!("{EXAMPLES}/26-capi-machinedeployment-cloud-hypervisor.yaml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut seen = Vec::new();
    for text in yaml_documents(&text) {
        let doc: serde_json::Value = serde_saphyr::from_str(&text).expect("valid YAML");
        let kind = doc["kind"].as_str().expect("kind").to_string();
        assert_eq!(doc["metadata"]["namespace"], NAMESPACE, "{kind}");
        match kind.as_str() {
            "CloudHypervisorMachineTemplate" => {
                let t: CloudHypervisorMachineTemplate =
                    serde_json::from_value(doc.clone()).expect("CloudHypervisorMachineTemplate");
                assert_no_dropped_keys(&doc["spec"], &t.spec);
                assert_eq!(t.spec.template.spec.user_data, None);
            }
            "MachineDeployment" => {
                let bootstrap = &doc["spec"]["template"]["spec"]["bootstrap"];
                assert!(bootstrap.get("configRef").is_some(), "{bootstrap}");
                assert!(bootstrap.get("dataSecretName").is_none(), "{bootstrap}");
            }
            _ => {}
        }
        seen.push(kind);
    }
    for kind in [
        "Cluster",
        "MachineDeployment",
        "CloudHypervisorMachineTemplate",
        "KubeadmConfigTemplate",
    ] {
        assert!(seen.iter().any(|k| k == kind), "example lacks a {kind}");
    }
}
