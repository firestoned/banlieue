// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Structural checks on the ADR-0133 admission policies, offline.
//!
//! What a policy *decides* can only be checked against a real API server (CEL
//! is evaluated there); that is `make provider-ref-policy-live-test`. These
//! tests catch the mistakes that need no cluster to see: a binding that names
//! a policy that is not there, a policy that is not set to `Deny`, a
//! controller identity that has drifted from the ServiceAccount the controller
//! actually runs as, or an admin role broader than the one virtual permission.

use serde_json::Value;

const PROVIDER_REF: &str = include_str!("../../../deploy/admission/machine-provider-ref.yaml");
const SELECTOR_AUTHZ: &str =
    include_str!("../../../deploy/admission/provider-namespace-selector-authorization.yaml");
const CONTROLLER_SA: &str = include_str!("../../../deploy/controller/rbac/serviceaccount.yaml");
const CLAIM_SUBJECT: &str =
    include_str!("../../../deploy/admission/virtualmachineclaim-subject-authorization.yaml");

/// The controller's identity as the API server reports it in `userInfo`.
const CONTROLLER_IDENTITY: &str = "system:serviceaccount:banlieue-system:banlieue-controller";

fn docs(yaml: &str) -> Vec<Value> {
    yaml.split("\n---\n")
        .filter(|d| {
            d.lines()
                .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        })
        .map(|d| serde_saphyr::from_str::<Value>(d).expect("manifest parses"))
        .collect()
}

fn of_kind<'a>(docs: &'a [Value], kind: &str) -> Vec<&'a Value> {
    docs.iter().filter(|d| d["kind"] == kind).collect()
}

fn name(doc: &Value) -> &str {
    doc["metadata"]["name"].as_str().expect("name")
}

#[test]
fn every_binding_names_a_policy_in_the_same_file_and_denies() {
    for yaml in [PROVIDER_REF, SELECTOR_AUTHZ] {
        let all = docs(yaml);
        let policies: Vec<&str> = of_kind(&all, "ValidatingAdmissionPolicy")
            .into_iter()
            .map(name)
            .collect();
        let bindings = of_kind(&all, "ValidatingAdmissionPolicyBinding");
        assert_eq!(policies.len(), bindings.len(), "one binding per policy");
        for b in bindings {
            let target = b["spec"]["policyName"].as_str().expect("policyName");
            assert!(
                policies.contains(&target),
                "{} names a missing policy",
                name(b)
            );
            assert_eq!(
                b["spec"]["validationActions"],
                serde_json::json!(["Deny"]),
                "{} must enforce, not warn or audit",
                name(b)
            );
        }
    }
}

#[test]
fn the_policies_fail_closed() {
    for yaml in [PROVIDER_REF, SELECTOR_AUTHZ] {
        for p in of_kind(&docs(yaml), "ValidatingAdmissionPolicy") {
            assert_eq!(p["spec"]["failurePolicy"], "Fail", "{}", name(p));
        }
    }
}

#[test]
fn the_provider_ref_policies_cover_every_machine_kind_and_its_template_on_create_and_update() {
    let all = docs(PROVIDER_REF);
    let mut resources: Vec<String> = Vec::new();
    for p in of_kind(&all, "ValidatingAdmissionPolicy") {
        let rule = &p["spec"]["matchConstraints"]["resourceRules"][0];
        assert_eq!(
            rule["apiGroups"],
            serde_json::json!(["infrastructure.banlieue.io"])
        );
        let ops: Vec<&str> = rule["operations"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            ops.contains(&"CREATE") && ops.contains(&"UPDATE"),
            "{ops:?}"
        );
        resources.extend(
            rule["resources"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| r.as_str().map(str::to_string)),
        );
    }
    resources.sort();
    assert_eq!(
        resources,
        [
            "cloudhypervisormachines",
            "cloudhypervisormachinetemplates",
            "libvirtmachines",
            "libvirtmachinetemplates",
            "proxmoxmachines",
            "proxmoxmachinetemplates",
            "vspheremachines",
            "vspheremachinetemplates",
        ]
    );
}

/// The status subresource is the provider's report, and must stay writable by
/// the provider: the policy matches the main resource only.
#[test]
fn the_provider_ref_policies_do_not_match_the_status_subresource() {
    for p in of_kind(&docs(PROVIDER_REF), "ValidatingAdmissionPolicy") {
        let rule = &p["spec"]["matchConstraints"]["resourceRules"][0];
        for r in rule["resources"].as_array().unwrap() {
            assert!(!r.as_str().unwrap().contains('/'), "{r}");
        }
    }
}

/// The exemption is the controller's real identity. If the ServiceAccount is
/// renamed and this string is not, the controller would be denied its own
/// placements and no VM on a shared provider would ever schedule.
#[test]
fn the_exempt_identity_is_the_controllers_service_account() {
    let sa = serde_saphyr::from_str::<Value>(CONTROLLER_SA).expect("serviceaccount parses");
    let identity = format!(
        "system:serviceaccount:{}:{}",
        sa["metadata"]["namespace"].as_str().unwrap(),
        sa["metadata"]["name"].as_str().unwrap()
    );
    assert_eq!(identity, CONTROLLER_IDENTITY);
    for p in of_kind(&docs(PROVIDER_REF), "ValidatingAdmissionPolicy") {
        let cond = p["spec"]["matchConditions"][0]["expression"]
            .as_str()
            .unwrap();
        assert!(
            cond.contains(CONTROLLER_IDENTITY),
            "{} exempts the wrong identity",
            name(p)
        );
        assert!(
            cond.contains("!="),
            "{} must exempt, not require, the controller",
            name(p)
        );
    }
}

/// CAPI's manager is deliberately not exempt (ADR-0133 Decision 10).
#[test]
fn only_the_controller_is_exempt() {
    for p in of_kind(&docs(PROVIDER_REF), "ValidatingAdmissionPolicy") {
        let conds = p["spec"]["matchConditions"].as_array().unwrap();
        assert_eq!(conds.len(), 1, "{}", name(p));
    }
}

#[test]
fn the_selector_policy_checks_the_virtual_subresource_with_the_authorizer() {
    let all = docs(SELECTOR_AUTHZ);
    let policy = of_kind(&all, "ValidatingAdmissionPolicy")[0];
    let expr = policy["spec"]["validations"][0]["expression"]
        .as_str()
        .unwrap();
    assert!(expr.contains("subresource('namespaceselector')"), "{expr}");
    assert!(expr.contains(".check('update').allowed()"), "{expr}");
    assert!(
        expr.contains("variables.unchanged"),
        "an unchanged selector needs no grant"
    );
}

/// Exactly the one virtual permission, and not aggregated into any tenant role.
#[test]
fn the_selector_admin_role_grants_exactly_one_virtual_permission() {
    let all = docs(SELECTOR_AUTHZ);
    let roles = of_kind(&all, "ClusterRole");
    assert_eq!(roles.len(), 1);
    let role = roles[0];
    let rules = role["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["apiGroups"], serde_json::json!(["banlieue.io"]));
    assert_eq!(
        rules[0]["resources"],
        serde_json::json!(["providers/namespaceselector"])
    );
    assert_eq!(rules[0]["verbs"], serde_json::json!(["update"]));
    let labels = role["metadata"]["labels"].as_object().unwrap();
    assert!(
        labels.keys().all(|k| !k.contains("aggregate-to")),
        "the admin role must not be aggregated into admin/edit/view: {labels:?}"
    );
}

/// ADR-0135 Decision 7: `spec.selection` is read once, at pick time, so the
/// claim policy refuses an edit to it on UPDATE, and compares it with
/// optional access (`.?`): the field is usually absent, and comparing an
/// absent field errors in CEL, which this fail-closed policy would turn into
/// a denial of every update to every claim.
#[test]
fn the_claim_policy_freezes_selection_with_optional_access() {
    let all = docs(CLAIM_SUBJECT);
    let policy = of_kind(&all, "ValidatingAdmissionPolicy")
        .into_iter()
        .find(|p| name(p) == "banlieue-virtualmachineclaim-subject-authorization")
        .expect("claim subject policy");
    let expressions: Vec<&str> = policy["spec"]["validations"]
        .as_array()
        .expect("validations")
        .iter()
        .filter_map(|v| v["expression"].as_str())
        .collect();
    assert!(
        expressions.iter().any(|e| {
            e.contains("request.operation != 'UPDATE'")
                && e.contains("object.spec.?selection == oldObject.spec.?selection")
        }),
        "no UPDATE-scoped, optional-access immutability rule for spec.selection: {expressions:#?}"
    );
}
