// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pins the shape of the opt-in NetworkPolicy templates in
//! `deploy/network-policies/` (ADR-0094).
//!
//! The policies are plain YAML that nothing else parses, so without this test
//! a renamed component label, a forgotten provider, or an egress rule that
//! quietly opened every port would only show up as a silently broken (or
//! silently open) install. The checks:
//!
//! - a default-deny policy selects every `app.kubernetes.io/name=banlieue` pod;
//! - every component with a Deployment under `deploy/`, every provider the
//!   operator spawns, and every Job banlieue launches has its own allow policy;
//! - those selectors use the label keys and values the operator stamps
//!   (taken from [`banlieue_operator::naming`], not duplicated here);
//! - egress stays on the ports in the ADR's table, per component;
//! - ingress allows only the `metrics` and `health` ports.
//!
//! Hermetic: it reads files from the repository and talks to nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use banlieue_operator::naming::{
    APP_NAME, LABEL_COMPONENT, LABEL_NAME, component, workload_labels_for,
};
use banlieue_operator::workload::{HEALTH_PORT_NAME, METRICS_PORT_NAME};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::networking::v1::{
    NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyPeer, NetworkPolicyPort,
};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use serde::Deserialize;

/// Directory holding the templates, relative to the repository root.
const POLICY_DIR: &str = "deploy/network-policies";

/// The two-line SPDX header every YAML file under `deploy/` starts with.
const SPDX_HEADER: &str =
    "# Copyright (c) 2026 Erick Bourgeois, banlieue\n# SPDX-License-Identifier: Apache-2.0\n";

/// Namespace label the metrics ingress rule selects scrapers by.
const LABEL_MONITORING: &str = "banlieue.io/monitoring";

/// Well-known namespace-name label set by the API server on every namespace.
const LABEL_NAMESPACE_NAME: &str = "kubernetes.io/metadata.name";

/// Label carried by cluster DNS pods.
const LABEL_KUBE_DNS: (&str, &str) = ("k8s-app", "kube-dns");

const TCP: &str = "TCP";
const UDP: &str = "UDP";
const DNS: u16 = 53;
const HTTPS: u16 = 443;
const API_SERVER_ALT: u16 = 6443;
const PROXMOX_API: u16 = 8006;
const LIBVIRT_TLS: u16 = 16514;

/// Backends whose provider pods the operator spawns as Deployments.
/// `cloud-hypervisor` is host-resident (`ProviderDeployment::External`) and
/// has no pod for a NetworkPolicy to select.
const OPERATOR_SPAWNED_BACKENDS: [&str; 3] = ["vsphere", "libvirt", "proxmox"];

/// Component labels on the Jobs banlieue launches, and where each is stamped:
/// - `registry-push`: `crates/banlieue-imagebuilder/src/reconciler/push.rs`
/// - `libvirt-import`: `crates/banlieue-provider-libvirt/src/reconciler/vmimage.rs`
/// - `vsphere-import`: `crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`
const JOB_COMPONENTS: [&str; 3] = ["registry-push", "libvirt-import", "vsphere-import"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.yaml` file in `dir`, sorted, so failures are reproducible.
fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    files.sort();
    files
}

/// Parse every document in every policy file. Fails loudly when the
/// directory is empty: a test over nothing must not report success.
fn policies() -> Vec<(PathBuf, NetworkPolicy)> {
    let dir = repo_root().join(POLICY_DIR);
    let mut out = Vec::new();
    for path in yaml_files(&dir) {
        let text = fs::read_to_string(&path).expect("readable policy file");
        for doc in serde_yaml::Deserializer::from_str(&text) {
            let value = serde_yaml::Value::deserialize(doc).expect("valid YAML");
            if value.is_null() {
                continue;
            }
            let policy: NetworkPolicy = serde_yaml::from_value(value)
                .unwrap_or_else(|e| panic!("{}: not a NetworkPolicy: {e}", path.display()));
            out.push((path.clone(), policy));
        }
    }
    assert!(
        !out.is_empty(),
        "no NetworkPolicy found in {}",
        dir.display()
    );
    out
}

fn match_labels(policy: &NetworkPolicy) -> BTreeMap<String, String> {
    policy
        .spec
        .as_ref()
        .and_then(|s| s.pod_selector.as_ref())
        .and_then(|sel| sel.match_labels.clone())
        .unwrap_or_default()
}

fn name_of(policy: &NetworkPolicy) -> String {
    policy.metadata.name.clone().unwrap_or_default()
}

fn is_default_deny(policy: &NetworkPolicy) -> bool {
    match_labels(policy) == BTreeMap::from([(LABEL_NAME.to_string(), APP_NAME.to_string())])
}

/// The allow policy whose selector names `component`.
fn policy_for(component: &str) -> Option<NetworkPolicy> {
    policies()
        .into_iter()
        .map(|(_, p)| p)
        .find(|p| match_labels(p).get(LABEL_COMPONENT).map(String::as_str) == Some(component))
}

/// Component labels on the pod templates of every Deployment under `deploy/`.
fn deployed_components() -> BTreeSet<String> {
    let deploy = repo_root().join("deploy");
    let mut components = BTreeSet::new();
    for entry in fs::read_dir(&deploy).expect("deploy/ exists") {
        let manifest = entry
            .expect("directory entry")
            .path()
            .join("deployment.yaml");
        if !manifest.exists() {
            continue;
        }
        let text = fs::read_to_string(&manifest).expect("readable deployment");
        let deployment: Deployment =
            serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
        let labels = deployment
            .spec
            .and_then(|s| s.template.metadata)
            .and_then(|m| m.labels)
            .unwrap_or_default();
        let c = labels
            .get(LABEL_COMPONENT)
            .unwrap_or_else(|| panic!("{} has no component label", manifest.display()));
        components.insert(c.clone());
    }
    assert!(
        !components.is_empty(),
        "no deployment.yaml found under deploy/"
    );
    components
}

fn required_components() -> BTreeSet<String> {
    let mut required = deployed_components();
    required.extend(OPERATOR_SPAWNED_BACKENDS.iter().map(|b| component(b)));
    required.extend(JOB_COMPONENTS.iter().map(ToString::to_string));
    required
}

/// `(protocol, port)` of a numeric NetworkPolicy port.
fn numeric_port(policy: &str, p: &NetworkPolicyPort) -> (String, u16) {
    assert!(
        p.end_port.is_none(),
        "{policy}: port ranges are not allowed"
    );
    let protocol = p.protocol.clone().unwrap_or_else(|| TCP.to_string());
    match &p.port {
        Some(IntOrString::Int(n)) => (
            protocol,
            u16::try_from(*n).unwrap_or_else(|_| panic!("{policy}: port {n} out of range")),
        ),
        other => panic!("{policy}: egress port must be a number, got {other:?}"),
    }
}

fn egress_rules(policy: &NetworkPolicy) -> Vec<NetworkPolicyEgressRule> {
    policy
        .spec
        .as_ref()
        .and_then(|s| s.egress.clone())
        .unwrap_or_default()
}

fn egress_ports(policy: &NetworkPolicy) -> BTreeSet<(String, u16)> {
    let name = name_of(policy);
    egress_rules(policy)
        .iter()
        .flat_map(|r| r.ports.clone().unwrap_or_default())
        .map(|p| numeric_port(&name, &p))
        .collect()
}

fn ports(list: &[(&str, u16)]) -> BTreeSet<(String, u16)> {
    list.iter()
        .map(|(proto, n)| ((*proto).to_string(), *n))
        .collect()
}

/// Egress every component needs, per the ADR-0094 table.
fn expected_egress() -> BTreeMap<String, BTreeSet<(String, u16)>> {
    let dns = [(UDP, DNS), (TCP, DNS)];
    let api = [(TCP, HTTPS), (TCP, API_SERVER_ALT)];
    let base: Vec<(&str, u16)> = dns.iter().chain(api.iter()).copied().collect();
    let with = |extra: &[(&str, u16)]| {
        let mut all = base.clone();
        all.extend_from_slice(extra);
        ports(&all)
    };
    BTreeMap::from([
        ("controller".to_string(), with(&[])),
        ("operator".to_string(), with(&[])),
        ("imagebuilder".to_string(), with(&[])),
        (component("vsphere"), with(&[(TCP, HTTPS)])),
        (component("proxmox"), with(&[(TCP, PROXMOX_API)])),
        (component("libvirt"), with(&[(TCP, LIBVIRT_TLS)])),
        ("vsphere-import".to_string(), with(&[(TCP, HTTPS)])),
        ("libvirt-import".to_string(), with(&[(TCP, LIBVIRT_TLS)])),
        // The push Job runs with automountServiceAccountToken: false and never
        // calls the API server: DNS and the registry only.
        (
            "registry-push".to_string(),
            ports(&[(UDP, DNS), (TCP, DNS), (TCP, HTTPS)]),
        ),
    ])
}

#[test]
fn every_policy_file_carries_the_spdx_header() {
    for path in yaml_files(&repo_root().join(POLICY_DIR)) {
        let text = fs::read_to_string(&path).expect("readable policy file");
        assert!(
            text.starts_with(SPDX_HEADER),
            "{} must start with the SPDX header",
            path.display()
        );
    }
}

#[test]
fn policies_are_namespace_agnostic() {
    // Applied per namespace with `kubectl apply -n <ns>`; a hardcoded
    // namespace would make the same file unusable for a workloadNamespace.
    for (path, p) in policies() {
        assert!(
            p.metadata.namespace.is_none(),
            "{}: {} must not set metadata.namespace",
            path.display(),
            name_of(&p)
        );
    }
}

#[test]
fn default_deny_selects_every_banlieue_pod_in_both_directions() {
    let deny = policies()
        .into_iter()
        .map(|(_, p)| p)
        .find(is_default_deny)
        .expect("a default-deny policy selecting app.kubernetes.io/name=banlieue");
    let spec = deny.spec.expect("spec");
    let types: BTreeSet<String> = spec.policy_types.unwrap_or_default().into_iter().collect();
    assert_eq!(
        types,
        BTreeSet::from(["Ingress".to_string(), "Egress".to_string()])
    );
    assert!(
        spec.ingress.unwrap_or_default().is_empty(),
        "deny allows no ingress"
    );
    assert!(
        spec.egress.unwrap_or_default().is_empty(),
        "deny allows no egress"
    );
}

#[test]
fn every_component_has_an_allow_policy() {
    for c in required_components() {
        assert!(
            policy_for(&c).is_some(),
            "no NetworkPolicy selects component {c}"
        );
    }
}

#[test]
fn every_allow_policy_is_expected() {
    // A policy for a component nobody runs is stale, and probably a typo of
    // one that does.
    let required = required_components();
    for (path, p) in policies() {
        if is_default_deny(&p) {
            continue;
        }
        let c = match_labels(&p).get(LABEL_COMPONENT).cloned();
        assert!(
            c.as_ref().is_some_and(|c| required.contains(c)),
            "{}: {} selects unknown component {c:?}",
            path.display(),
            name_of(&p)
        );
    }
}

#[test]
fn selectors_use_the_operator_label_keys_and_values() {
    for (path, p) in policies() {
        let spec = p.spec.as_ref().expect("spec");
        let selector = spec.pod_selector.as_ref().expect("podSelector");
        assert!(
            selector.match_expressions.is_none(),
            "{}: use matchLabels only",
            path.display()
        );
        let labels = match_labels(&p);
        assert_eq!(
            labels.get(LABEL_NAME).map(String::as_str),
            Some(APP_NAME),
            "{}: selector must pin {LABEL_NAME}={APP_NAME}",
            path.display()
        );
        let keys: BTreeSet<&str> = labels.keys().map(String::as_str).collect();
        let allowed = BTreeSet::from([LABEL_NAME, LABEL_COMPONENT]);
        assert!(
            keys.is_subset(&allowed),
            "{}: selector keys {keys:?} beyond {allowed:?}",
            path.display()
        );
    }
}

#[test]
fn provider_selectors_match_what_the_operator_stamps() {
    for backend in OPERATOR_SPAWNED_BACKENDS {
        let stamped = workload_labels_for("class", "ns", "provider", backend);
        let policy = policy_for(&component(backend))
            .unwrap_or_else(|| panic!("no policy for provider-{backend}"));
        for (k, v) in match_labels(&policy) {
            assert_eq!(
                stamped.get(&k),
                Some(&v),
                "{}: selector {k}={v} is not on operator-spawned {backend} pods",
                name_of(&policy)
            );
        }
    }
}

#[test]
fn egress_ports_match_each_components_needs() {
    for (c, expected) in expected_egress() {
        let policy = policy_for(&c).unwrap_or_else(|| panic!("no policy for {c}"));
        assert_eq!(egress_ports(&policy), expected, "egress ports of {c}");
    }
}

#[test]
fn no_egress_rule_is_port_unrestricted() {
    let allowed = ports(&[
        (UDP, DNS),
        (TCP, DNS),
        (TCP, HTTPS),
        (TCP, API_SERVER_ALT),
        (TCP, PROXMOX_API),
        (TCP, LIBVIRT_TLS),
    ]);
    for (_, p) in policies() {
        let name = name_of(&p);
        for rule in egress_rules(&p) {
            let rule_ports = rule.ports.unwrap_or_default();
            assert!(
                !rule_ports.is_empty(),
                "{name}: an egress rule without ports allows every port"
            );
            for port in &rule_ports {
                let pp = numeric_port(&name, port);
                assert!(
                    allowed.contains(&pp),
                    "{name}: egress {pp:?} not in the ADR table"
                );
            }
        }
    }
}

fn is_kube_dns_peer(peer: &NetworkPolicyPeer) -> bool {
    let ns = peer
        .namespace_selector
        .as_ref()
        .and_then(|s| s.match_labels.as_ref())
        .and_then(|l| l.get(LABEL_NAMESPACE_NAME))
        .map(String::as_str);
    let pod = peer
        .pod_selector
        .as_ref()
        .and_then(|s| s.match_labels.as_ref())
        .and_then(|l| l.get(LABEL_KUBE_DNS.0))
        .map(String::as_str);
    ns == Some("kube-system") && pod == Some(LABEL_KUBE_DNS.1) && peer.ip_block.is_none()
}

#[test]
fn dns_egress_reaches_only_kube_dns() {
    for (_, p) in policies() {
        let name = name_of(&p);
        for rule in egress_rules(&p) {
            let carries_dns = rule
                .ports
                .iter()
                .flatten()
                .any(|port| numeric_port(&name, port).1 == DNS);
            if !carries_dns {
                continue;
            }
            let peers = rule.to.unwrap_or_default();
            assert!(
                !peers.is_empty() && peers.iter().all(is_kube_dns_peer),
                "{name}: DNS egress must target k8s-app=kube-dns in kube-system only"
            );
        }
    }
}

#[test]
fn ingress_allows_only_metrics_and_health() {
    for (_, p) in policies() {
        let name = name_of(&p);
        let rules = p
            .spec
            .as_ref()
            .and_then(|s| s.ingress.clone())
            .unwrap_or_default();
        for rule in rules {
            let rule_ports = rule.ports.clone().unwrap_or_default();
            assert!(
                !rule_ports.is_empty(),
                "{name}: an ingress rule without ports allows every port"
            );
            for port in rule_ports {
                let named = match &port.port {
                    Some(IntOrString::String(s)) => s.clone(),
                    other => panic!("{name}: ingress must use a named port, got {other:?}"),
                };
                if named == HEALTH_PORT_NAME {
                    continue;
                }
                assert_eq!(named, METRICS_PORT_NAME, "{name}: ingress on {named}");
                let from = rule.from.clone().unwrap_or_default();
                let only_monitoring = !from.is_empty()
                    && from.iter().all(|peer| {
                        peer.ip_block.is_none()
                            && peer
                                .namespace_selector
                                .as_ref()
                                .and_then(|s| s.match_labels.as_ref())
                                .and_then(|l| l.get(LABEL_MONITORING))
                                .map(String::as_str)
                                == Some("true")
                    });
                assert!(
                    only_monitoring,
                    "{name}: metrics must be reachable only from {LABEL_MONITORING}=true namespaces"
                );
            }
        }
    }
}

#[test]
fn long_running_components_expose_metrics_and_health() {
    // Jobs serve nothing; every Deployment-backed component does.
    let mut long_running = deployed_components();
    long_running.extend(OPERATOR_SPAWNED_BACKENDS.iter().map(|b| component(b)));
    for c in long_running {
        let policy = policy_for(&c).unwrap_or_else(|| panic!("no policy for {c}"));
        let named: BTreeSet<String> = policy
            .spec
            .and_then(|s| s.ingress)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|r| r.ports.unwrap_or_default())
            .filter_map(|p| match p.port {
                Some(IntOrString::String(s)) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(
            named,
            BTreeSet::from([METRICS_PORT_NAME.to_string(), HEALTH_PORT_NAME.to_string()]),
            "ingress ports of {c}"
        );
    }
}
