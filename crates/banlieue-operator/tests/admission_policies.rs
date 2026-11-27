// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Structural checks on the ADR-0133 admission policies, and on the azure
//! and ec2 rules (ADR-0159, ADR-0162, ADR-0167, ADR-0174) in
//! `provider-connection.yaml`, `provider-ec2-governance.yaml`,
//! `provider-attestation-ektrustbundle.yaml` and
//! `vmimage-cloud-install-mode.yaml`, offline. What the ec2 rules decide is
//! evaluated as CEL by `make provider-connection-policy-test` and
//! `make ec2-governance-policy-test`.
//!
//! What a policy *decides* can only be checked against a real API server (CEL
//! is evaluated there; no CEL evaluator is linked into this workspace); that
//! is `make provider-ref-policy-live-test`. These tests catch the mistakes
//! that need no cluster to see: a binding that names a policy that is not
//! there, a policy that is not set to `Deny`, a controller identity that has
//! drifted from the ServiceAccount the controller actually runs as, an admin
//! role broader than the one virtual permission, a rule that has gone
//! missing, or an allowlist that has drifted from the provider binary's.

use serde_json::Value;

const PROVIDER_REF: &str = include_str!("../../../deploy/admission/machine-provider-ref.yaml");
const SELECTOR_AUTHZ: &str =
    include_str!("../../../deploy/admission/provider-namespace-selector-authorization.yaml");
const CONTROLLER_SA: &str = include_str!("../../../deploy/controller/rbac/serviceaccount.yaml");
const CLAIM_SUBJECT: &str =
    include_str!("../../../deploy/admission/virtualmachineclaim-subject-authorization.yaml");
const PROVIDER_CONNECTION: &str =
    include_str!("../../../deploy/admission/provider-connection.yaml");
const VMIMAGE_CLOUD_INSTALL_MODE: &str =
    include_str!("../../../deploy/admission/vmimage-cloud-install-mode.yaml");
const PROVIDER_JOBS: &str = include_str!("../../../deploy/admission/provider-jobs.yaml");
const PROVIDER_EC2_GOVERNANCE: &str =
    include_str!("../../../deploy/admission/provider-ec2-governance.yaml");
const PROVIDER_EK_TRUST_BUNDLE: &str =
    include_str!("../../../deploy/admission/provider-attestation-ektrustbundle.yaml");
/// The provider binary's cloud table, which the admission allowlist must
/// match string for string.
const AZURE_CLOUD_TABLE: &str = include_str!("../../banlieue-provider-azure/src/cloud.rs");
const VMIMAGE_CRD: &str = include_str!("../../../deploy/crds/banlieue.io_vmimages.yaml");
const PROVIDER_CRD: &str = include_str!("../../../deploy/crds/banlieue.io_providers.yaml");

/// The known Azure clouds: (Resource Manager endpoint, Entra ID login host).
const AZURE_CLOUDS: [(&str, &str); 3] = [
    (
        "https://management.azure.com",
        "https://login.microsoftonline.com",
    ),
    (
        "https://management.usgovcloudapi.net",
        "https://login.microsoftonline.us",
    ),
    (
        "https://management.chinacloudapi.cn",
        "https://login.chinacloudapi.cn",
    ),
];

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
    for yaml in [
        PROVIDER_REF,
        SELECTOR_AUTHZ,
        PROVIDER_CONNECTION,
        VMIMAGE_CLOUD_INSTALL_MODE,
        PROVIDER_JOBS,
        PROVIDER_EC2_GOVERNANCE,
        PROVIDER_EK_TRUST_BUNDLE,
    ] {
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
    for yaml in [
        PROVIDER_REF,
        SELECTOR_AUTHZ,
        PROVIDER_CONNECTION,
        VMIMAGE_CLOUD_INSTALL_MODE,
        PROVIDER_JOBS,
        PROVIDER_EC2_GOVERNANCE,
        PROVIDER_EK_TRUST_BUNDLE,
    ] {
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
            "azurevirtualmachines",
            "azurevirtualmachinetemplates",
            "cloudhypervisormachines",
            "cloudhypervisormachinetemplates",
            "ec2machines",
            "ec2machinetemplates",
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

// ---- azure: provider-connection.yaml and vmimage-cloud-install-mode.yaml --

/// The one `ValidatingAdmissionPolicy` in `yaml`.
fn only_policy(yaml: &str) -> Value {
    let all = docs(yaml);
    let policies = of_kind(&all, "ValidatingAdmissionPolicy");
    assert_eq!(policies.len(), 1);
    policies[0].clone()
}

/// `text` with every run of whitespace collapsed to one space, so a rule is
/// matched however the YAML folds it.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every validation of `policy`, as (expression, message or message
/// expression), whitespace collapsed.
fn validations(policy: &Value) -> Vec<(String, String)> {
    policy["spec"]["validations"]
        .as_array()
        .expect("validations")
        .iter()
        .map(|v| {
            let message = v["message"]
                .as_str()
                .or_else(|| v["messageExpression"].as_str())
                .unwrap_or_default();
            (
                squash(v["expression"].as_str().expect("expression")),
                squash(message),
            )
        })
        .collect()
}

/// The one validation whose expression contains every one of `fragments`;
/// it must carry a message naming `field`.
fn rule_with(policy: &Value, fragments: &[&str], field: &str) -> String {
    let found: Vec<(String, String)> = validations(policy)
        .into_iter()
        .filter(|(e, _)| fragments.iter().all(|f| e.contains(f)))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one rule must contain {fragments:?}: {found:#?}"
    );
    let (expression, message) = found.into_iter().next().expect("one rule");
    assert!(
        message.contains(field),
        "the message of {expression:?} must name {field}: {message:?}"
    );
    expression
}

/// Every single-quoted `https://` literal in `expression`.
fn https_literals(expression: &str) -> Vec<String> {
    expression
        .split('\'')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with("https://"))
        .map(str::to_string)
        .collect()
}

/// `url` and `url/`, the two spellings a rule accepts.
fn both_spellings(url: &str) -> [String; 2] {
    [url.to_string(), format!("{url}/")]
}

/// The provider policy matches `providers` on create and update.
#[test]
fn the_provider_connection_policy_checks_every_provider_write() {
    let policy = only_policy(PROVIDER_CONNECTION);
    let rule = &policy["spec"]["matchConstraints"]["resourceRules"][0];
    assert_eq!(rule["apiGroups"], serde_json::json!(["banlieue.io"]));
    assert_eq!(rule["resources"], serde_json::json!(["providers"]));
    assert_eq!(rule["operations"], serde_json::json!(["CREATE", "UPDATE"]));
}

/// azure without `connection.azure` is refused: it names no tenant,
/// client, subscription, region or resource group.
#[test]
fn azure_without_its_connection_block_is_refused() {
    rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &[
            "object.spec.providerClassRef.name != 'azure'",
            "|| has(object.spec.connection.azure)",
        ],
        "spec.connection.azure",
    );
}

/// azure with `credentialsRef` is refused: it reads no Secret (ADR-0159).
#[test]
fn azure_with_a_credentials_ref_is_refused() {
    let expression = rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &[
            "object.spec.providerClassRef.name != 'azure'",
            "!has(object.spec.connection.credentialsRef)",
        ],
        "credentialsRef",
    );
    assert!(!expression.contains("cloud-hypervisor"), "{expression}");
}

/// azure is one of the classes that must speak https, and plain http is
/// refused for every class.
#[test]
fn azure_with_an_http_endpoint_is_refused() {
    let policy = only_policy(PROVIDER_CONNECTION);
    let expression = rule_with(
        &policy,
        &["object.spec.connection.endpoint.startsWith('https://')"],
        "https://",
    );
    assert!(
        expression.contains("['vsphere', 'proxmox', 'azure', 'ec2']"),
        "{expression}"
    );
    rule_with(
        &policy,
        &["!object.spec.connection.endpoint.startsWith('http://')"],
        "plain http",
    );
}

/// The `azure` block on any other class is refused.
#[test]
fn a_non_azure_provider_with_an_azure_block_is_refused() {
    rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &[
            "object.spec.providerClassRef.name == 'azure'",
            "|| !has(object.spec.connection.azure)",
        ],
        "only valid for azure",
    );
}

/// The variables the token-destination rules read are what they claim to
/// be: the class, the endpoint, and the authority or '' when unset.
#[test]
fn the_azure_variables_read_the_right_fields() {
    let policy = only_policy(PROVIDER_CONNECTION);
    let vars: Vec<(String, String)> = policy["spec"]["variables"]
        .as_array()
        .expect("variables")
        .iter()
        .map(|v| {
            (
                v["name"].as_str().expect("name").to_string(),
                squash(v["expression"].as_str().expect("expression")),
            )
        })
        .collect();
    let get = |n: &str| {
        vars.iter()
            .find(|(name, _)| name == n)
            .map(|(_, e)| e.clone())
            .unwrap_or_else(|| panic!("variable {n}"))
    };
    let crd = squash(PROVIDER_CRD);
    assert!(
        crd.contains("authorityHost:"),
        "the Provider CRD has no authorityHost"
    );
    assert_eq!(
        get("isAzure"),
        "object.spec.providerClassRef.name == 'azure'"
    );
    assert_eq!(get("endpoint"), "object.spec.connection.endpoint");
    assert_eq!(
        get("authority"),
        "has(object.spec.connection.azure) && has(object.spec.connection.azure.authorityHost) ? object.spec.connection.azure.authorityHost : ''"
    );
}

/// The ARM token goes only to a known Resource Manager: the endpoint rule
/// allows exactly the three clouds' endpoints, with and without a trailing
/// slash, and nothing else.
#[test]
fn azure_with_an_endpoint_off_the_allowlist_is_refused() {
    let expression = rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &["!variables.isAzure", "|| variables.endpoint in ["],
        "spec.connection.endpoint",
    );
    let mut got = https_literals(&expression);
    got.sort();
    let mut want: Vec<String> = AZURE_CLOUDS
        .iter()
        .flat_map(|(endpoint, _)| both_spellings(endpoint))
        .collect();
    want.sort();
    assert_eq!(got, want);
}

/// The ServiceAccount token goes only to the login host of the endpoint's
/// own cloud: each endpoint is paired with exactly its authority, and an
/// unset authority is the only other way through.
#[test]
fn azure_with_an_authority_off_the_allowlist_or_unpaired_is_refused() {
    let expression = rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &["!variables.isAzure", "variables.authority in ["],
        "spec.connection.azure.authorityHost",
    );
    let clauses: Vec<&str> = expression.split(" || ").collect();
    assert_eq!(clauses[0], "!variables.isAzure");
    assert_eq!(clauses[1], "variables.authority == ''");
    let pairs = &clauses[2..];
    assert_eq!(pairs.len(), AZURE_CLOUDS.len(), "{clauses:#?}");
    for ((endpoint, authority), clause) in AZURE_CLOUDS.iter().zip(pairs) {
        let [e, e_slash] = both_spellings(endpoint);
        let [a, a_slash] = both_spellings(authority);
        assert_eq!(
            *clause,
            format!(
                "(variables.endpoint in ['{e}', '{e_slash}'] && variables.authority in ['{a}', '{a_slash}'])"
            )
        );
    }
}

/// A valid azure Provider (a known endpoint, its own authority or none, an
/// `azure` block, no `credentialsRef`) passes every rule: each rule that
/// names azure is either scoped to it with an escape for the valid shape,
/// or is one of the class-independent URL checks a known endpoint passes.
#[test]
fn a_valid_azure_provider_meets_every_azure_rule() {
    for (endpoint, _) in AZURE_CLOUDS {
        assert!(endpoint.starts_with("https://"));
        assert!(!endpoint.contains('@') && !endpoint.contains('#'));
    }
    for (expression, _) in validations(&only_policy(PROVIDER_CONNECTION)) {
        if !(expression.contains("'azure'") || expression.contains("variables.")) {
            continue;
        }
        let escapes = [
            "object.spec.providerClassRef.name != 'azure' ||",
            "!variables.isAzure ||",
            "object.spec.providerClassRef.name == 'azure' ||",
            "object.spec.providerClassRef.name in ['cloud-hypervisor', 'azure', 'ec2'] ||",
            "!(object.spec.providerClassRef.name in ['vsphere', 'proxmox', 'azure', 'ec2']) ||",
            // The ec2 token-destination rules read variables too; they are
            // scoped to ec2 and do not apply to an azure Provider.
            "!variables.isEc2 ||",
        ];
        assert!(
            escapes.iter().any(|e| expression.starts_with(e)),
            "an azure rule without a recognised scope: {expression}"
        );
    }
}

/// The admission allowlist and the binary's cloud table are the same
/// strings: a cloud added to one and not the other would be refused by
/// whichever lags.
#[test]
fn the_admission_allowlist_matches_the_provider_binary() {
    for (endpoint, authority) in AZURE_CLOUDS {
        for url in [endpoint, authority] {
            assert!(
                AZURE_CLOUD_TABLE.contains(&format!("\"{url}\"")),
                "{url} is in the policy but not crates/banlieue-provider-azure/src/cloud.rs"
            );
        }
    }
    assert!(AZURE_CLOUD_TABLE.contains(&format!(
        "AZURE_CLOUDS: [AzureCloud; {}]",
        AZURE_CLOUDS.len()
    )));
}

/// A VMImage with an azure source and `installMode: deferred` is refused;
/// `immediate` (and every other mode) is not mentioned, so it is accepted.
#[test]
fn a_cloud_vmimage_refuses_deferred_install_only() {
    let policy = only_policy(VMIMAGE_CLOUD_INSTALL_MODE);
    let rule = &policy["spec"]["matchConstraints"]["resourceRules"][0];
    assert_eq!(rule["apiGroups"], serde_json::json!(["banlieue.io"]));
    assert_eq!(rule["resources"], serde_json::json!(["vmimages"]));
    assert_eq!(rule["operations"], serde_json::json!(["CREATE", "UPDATE"]));

    let conditions = policy["spec"]["matchConditions"]
        .as_array()
        .expect("matchConditions");
    assert_eq!(conditions.len(), 1);
    let scope = squash(conditions[0]["expression"].as_str().expect("expression"));
    assert_eq!(
        scope,
        "object.spec.sources.exists(s, s.providerClass in ['azure', 'ec2'])"
    );

    let rules = validations(&policy);
    assert_eq!(rules.len(), 1, "{rules:#?}");
    let (expression, message) = &rules[0];
    assert_eq!(
        expression,
        "!has(object.spec.template) || !has(object.spec.template.installMode) || object.spec.template.installMode != 'deferred'"
    );
    assert!(!expression.contains("immediate"), "{expression}");
    assert!(message.contains("deferred"), "{message}");
}

/// The policy compares against the CRD's own spelling of the mode: a value
/// the enum does not contain would make the rule never fire.
#[test]
fn the_install_modes_the_policy_names_are_the_crd_enum() {
    let crd = squash(VMIMAGE_CRD);
    assert!(
        crd.contains("enum: - immediate - deferred - manual"),
        "the installMode enum changed; re-check vmimage-cloud-install-mode.yaml"
    );
}

// ---- ec2: provider-connection.yaml, provider-ec2-governance.yaml, ---------
// ---- provider-attestation-ektrustbundle.yaml ------------------------------

/// ec2 without `connection.ec2` is refused: it names no region or role.
#[test]
fn ec2_without_its_connection_block_is_refused() {
    rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &[
            "object.spec.providerClassRef.name != 'ec2'",
            "|| has(object.spec.connection.ec2)",
        ],
        "spec.connection.ec2",
    );
}

/// ec2 with `credentialsRef` is refused: it reads no Secret (ADR-0159).
#[test]
fn ec2_with_a_credentials_ref_is_refused() {
    rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &[
            "object.spec.providerClassRef.name != 'ec2'",
            "!has(object.spec.connection.credentialsRef)",
        ],
        "credentialsRef",
    );
}

/// The `ec2` block on any other class is refused.
#[test]
fn a_non_ec2_provider_with_an_ec2_block_is_refused() {
    rule_with(
        &only_policy(PROVIDER_CONNECTION),
        &[
            "object.spec.providerClassRef.name == 'ec2'",
            "|| !has(object.spec.connection.ec2)",
        ],
        "only valid for ec2",
    );
}

/// The ec2 variables read the right fields, and the region they splice into
/// a pattern is itself pinned to a safe shape by the CRD schema.
#[test]
fn the_ec2_variables_read_the_right_fields() {
    let policy = only_policy(PROVIDER_CONNECTION);
    let vars: Vec<(String, String)> = policy["spec"]["variables"]
        .as_array()
        .expect("variables")
        .iter()
        .map(|v| {
            (
                v["name"].as_str().expect("name").to_string(),
                squash(v["expression"].as_str().expect("expression")),
            )
        })
        .collect();
    let get = |n: &str| {
        vars.iter()
            .find(|(name, _)| name == n)
            .map(|(_, e)| e.clone())
            .unwrap_or_else(|| panic!("variable {n}"))
    };
    assert_eq!(get("isEc2"), "object.spec.providerClassRef.name == 'ec2'");
    assert_eq!(
        get("region"),
        "has(object.spec.connection.ec2) ? object.spec.connection.ec2.region : ''"
    );
    assert_eq!(
        get("awsSuffix"),
        "variables.region.startsWith('cn-') ? 'amazonaws[.]com[.]cn' : 'amazonaws[.]com'"
    );
    assert_eq!(
        get("sts"),
        "has(object.spec.connection.ec2) && has(object.spec.connection.ec2.stsEndpoint) ? object.spec.connection.ec2.stsEndpoint : ''"
    );
    let crd = squash(PROVIDER_CRD);
    assert!(
        crd.contains("stsEndpoint:"),
        "the Provider CRD has no stsEndpoint"
    );
    assert!(
        crd.contains(banlieue_api::aws::REGION_PATTERN),
        "the Provider CRD does not pin connection.ec2.region's shape"
    );
}

/// The session token goes only to the region's EC2 (or a VPC endpoint of
/// it), and the ServiceAccount token only to the region's STS (or a VPC
/// endpoint of it): anchored, with the service named.
#[test]
fn ec2_token_destinations_are_pinned_to_the_regions_names() {
    let policy = only_policy(PROVIDER_CONNECTION);
    for (fragment, service, field) in [
        (
            "variables.endpoint.matches(",
            "ec2",
            "spec.connection.endpoint",
        ),
        (
            "variables.sts.matches(",
            "sts",
            "spec.connection.ec2.stsEndpoint",
        ),
    ] {
        let expression = rule_with(&policy, &["!variables.isEc2", fragment], field);
        for part in [
            format!("'^https://({service}[.]' + variables.region"),
            format!(
                "'|vpce-[0-9a-f]{{17}}-[0-9a-z]{{8}}[.]{service}[.]' + variables.region + '[.]vpce)[.]'"
            ),
            "+ variables.awsSuffix + '/?$')".to_string(),
        ] {
            assert!(expression.contains(&part), "{part} not in {expression}");
        }
    }
}

/// A valid ec2 Provider passes every rule that names ec2 or reads its
/// variables: each is scoped to ec2 with an escape for the valid shape.
#[test]
fn a_valid_ec2_provider_meets_every_ec2_rule() {
    for (expression, _) in validations(&only_policy(PROVIDER_CONNECTION)) {
        if !(expression.contains("'ec2'") || expression.contains("variables.isEc2")) {
            continue;
        }
        let escapes = [
            "object.spec.providerClassRef.name != 'ec2' ||",
            "!variables.isEc2 ||",
            "object.spec.providerClassRef.name == 'ec2' ||",
            "object.spec.providerClassRef.name in ['cloud-hypervisor', 'azure', 'ec2'] ||",
            "!(object.spec.providerClassRef.name in ['vsphere', 'proxmox', 'azure', 'ec2']) ||",
        ];
        assert!(
            escapes.iter().any(|e| expression.starts_with(e)),
            "an ec2 rule without a recognised scope: {expression}"
        );
    }
}

/// The governance policy applies to providers with an ec2 block, and its
/// cap is the one the CRD schema and the Rust check use.
#[test]
fn the_ec2_governance_policy_checks_ec2_blocks_with_the_rust_limits() {
    let policy = only_policy(PROVIDER_EC2_GOVERNANCE);
    let rule = &policy["spec"]["matchConstraints"]["resourceRules"][0];
    assert_eq!(rule["resources"], serde_json::json!(["providers"]));
    assert_eq!(rule["operations"], serde_json::json!(["CREATE", "UPDATE"]));
    let conditions = policy["spec"]["matchConditions"]
        .as_array()
        .expect("matchConditions");
    assert_eq!(
        squash(conditions[0]["expression"].as_str().expect("expression")),
        "has(object.spec.connection.ec2)"
    );
    rule_with(
        &policy,
        &[&format!(
            "size(object.spec.connection.ec2.tags) <= {}",
            banlieue_api::aws::MAX_EXTRA_TAGS
        )],
        "at most",
    );
    rule_with(&policy, &["(?i)^banlieue-"], "banlieue-");
    rule_with(&policy, &["(?i)^aws:"], "aws:");
    rule_with(&policy, &["'Name' in"], "Name");
    let characters = format!("matches('{}')", banlieue_api::aws::TAG_CHARACTERS_PATTERN);
    rule_with(&policy, &["size(k) <= 128", &characters], "key");
    rule_with(&policy, &["<= 256", &characters], "value");
}

/// An ekTrustBundle on an ec2 Provider is refused (ADR-0167 Decision 2).
#[test]
fn an_ec2_provider_with_an_ek_trust_bundle_is_refused() {
    rule_with(
        &only_policy(PROVIDER_EK_TRUST_BUNDLE),
        &["object.spec.providerClassRef.name != 'ec2'"],
        "ec2",
    );
}

// ---- provider-jobs.yaml: what a provider identity's Jobs may run ----------

/// The ServiceAccounts a provider's Jobs may run as, sorted, as the policy
/// spells the list.
const JOB_ACCOUNTS: [&str; 3] = [
    "banlieue-azure-publisher",
    "banlieue-ec2-publisher",
    "banlieue-import",
];

#[test]
fn the_provider_jobs_policy_matches_job_creates_and_updates_only() {
    let policy = only_policy(PROVIDER_JOBS);
    let rules = policy["spec"]["matchConstraints"]["resourceRules"]
        .as_array()
        .expect("resourceRules");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["apiGroups"], serde_json::json!(["batch"]));
    assert_eq!(rules[0]["resources"], serde_json::json!(["jobs"]));
    assert_eq!(
        rules[0]["operations"],
        serde_json::json!(["CREATE", "UPDATE"])
    );
}

/// The match is on provider ServiceAccounts only: every name the operator or
/// bootstrap gives one starts `banlieue-provider-`, in any namespace.
#[test]
fn the_provider_jobs_policy_matches_provider_service_accounts() {
    let policy = only_policy(PROVIDER_JOBS);
    let conditions = policy["spec"]["matchConditions"]
        .as_array()
        .expect("matchConditions");
    assert_eq!(conditions.len(), 1);
    let expression = squash(conditions[0]["expression"].as_str().expect("expression"));
    assert!(
        expression.contains(
            "request.userInfo.username.matches('^system:serviceaccount:[^:]+:banlieue-provider-"
        ),
        "{expression}"
    );
    // The prefix is the one the provider workloads are named with.
    assert!(
        banlieue_provider_sdk::naming::workload_name("vsphere", "p")
            .starts_with("banlieue-provider-")
    );
}

#[test]
fn the_provider_jobs_policy_pins_the_build_namespace() {
    let policy = only_policy(PROVIDER_JOBS);
    let variables = policy["spec"]["variables"].as_array().expect("variables");
    let build = variables
        .iter()
        .find(|v| v["name"] == "buildNamespace")
        .expect("buildNamespace variable");
    assert_eq!(
        build["expression"].as_str().expect("expression").trim(),
        format!(
            "'{}'",
            banlieue_provider_sdk::defaults::DEFAULT_BUILD_NAMESPACE
        )
    );
    rule_with(
        &policy,
        &["object.metadata.namespace == variables.buildNamespace"],
        "build namespace",
    );
}

/// The allowed identities are exactly the three Job ServiceAccounts, sorted;
/// the provider's own identity is never one of them.
#[test]
fn the_provider_jobs_policy_allows_only_the_job_identities() {
    let policy = only_policy(PROVIDER_JOBS);
    let expression = rule_with(&policy, &["serviceAccountName in"], "serviceAccountName");
    let listed = format!(
        "[{}]",
        JOB_ACCOUNTS
            .iter()
            .map(|a| format!("'{a}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(expression.contains(&listed), "{expression}");
    let mut sorted = JOB_ACCOUNTS;
    sorted.sort_unstable();
    assert_eq!(sorted, JOB_ACCOUNTS);
    assert_eq!(
        JOB_ACCOUNTS[JOB_ACCOUNTS.len() - 1],
        banlieue_provider_sdk::defaults::DEFAULT_IMPORT_SERVICE_ACCOUNT
    );
    assert!(!expression.contains("banlieue-provider-"));
}

/// Each restricted-pod rule is present, with a message naming what it checks.
#[test]
fn the_provider_jobs_policy_requires_a_restricted_pod() {
    let policy = only_policy(PROVIDER_JOBS);
    rule_with(
        &policy,
        &["hostNetwork", "hostPID", "hostIPC"],
        "hostNetwork",
    );
    rule_with(&policy, &["runAsNonRoot", "runAsUser == 0"], "runAsNonRoot");
    rule_with(
        &policy,
        &["privileged", "allowPrivilegeEscalation", "capabilities.add"],
        "allowPrivilegeEscalation",
    );
    rule_with(&policy, &["Unconfined"], "seccomp");
    let volumes = rule_with(
        &policy,
        &["persistentVolumeClaim", "emptyDir", "serviceAccountToken"],
        "hostPath",
    );
    assert!(
        !volumes.contains("has(v.hostPath)"),
        "an allowlist, not a denylist"
    );
    rule_with(&policy, &["envFrom", "secretKeyRef"], "Secret");
    // Init containers are checked with the rest.
    let variables = policy["spec"]["variables"].as_array().expect("variables");
    let containers = variables
        .iter()
        .find(|v| v["name"] == "containers")
        .expect("containers variable");
    assert!(
        containers["expression"]
            .as_str()
            .expect("expression")
            .contains("initContainers")
    );
}
