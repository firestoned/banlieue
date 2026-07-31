// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! End-to-end verification of `banlieue bootstrap operator` — the documented
//! install path (ADR-0013/0014).
//!
//! This exists because the rest of the e2e installs via
//! `kubectl apply -R -f deploy/operator/`, which is the *GitOps* path. Those
//! two can drift, and when they do the failure is invisible: bug-110 —
//! bootstrap never installing the shared per-backend ClusterRole — was caught
//! only by accident, because a stale copy of that role happened to linger in a
//! reused cluster. Had the cluster been clean, the manifest-based suite would
//! have stayed green while every real `bootstrap operator` install produced
//! provider pods with zero permissions.
//!
//! So this suite asserts the *installer's own output*, in a cluster where
//! nothing else put those objects there.
//!
//! Run with `make kind-e2e-bootstrap`; `#[ignore]`d so `cargo test` stays
//! hermetic.

use std::time::{Duration, Instant};

use banlieue_api::banlieue::ProviderClass;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{Api, Client};

/// Namespace bootstrap installed into.
fn namespace() -> String {
    std::env::var("BANLIEUE_E2E_NAMESPACE").unwrap_or_else(|_| "banlieue-system".to_string())
}

/// Backends compiled into the binary under test, and therefore expected to have
/// been seeded by `bootstrap operator`.
fn backends() -> Vec<String> {
    std::env::var("BANLIEUE_E2E_BACKENDS")
        .unwrap_or_else(|_| "vsphere,libvirt".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

const CONVERGE_TIMEOUT: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

async fn client() -> Client {
    Client::try_default()
        .await
        .expect("no reachable cluster — run `make kind-e2e-bootstrap`")
}

async fn wait_for<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {}s waiting for: {what}",
            CONVERGE_TIMEOUT.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every CRD the binary implements must be present **and** `Established`.
///
/// Merely existing is not enough: a CRD the apiserver has not established yet
/// will reject CRs of that kind, so an install that returns before this point
/// is not actually usable.
#[tokio::test]
#[ignore = "requires a bootstrap-installed cluster; run `make kind-e2e-bootstrap`"]
async fn bootstrap_installs_every_crd_and_they_become_established() {
    let client = client().await;
    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());

    let expected = [
        "providers.banlieue.io",
        "providerclasses.banlieue.io",
        "vmclasses.banlieue.io",
        "vmimages.banlieue.io",
        "virtualmachines.banlieue.io",
        "vsphereclusters.infrastructure.banlieue.io",
        "vspheremachines.infrastructure.banlieue.io",
        "vspheremachinetemplates.infrastructure.banlieue.io",
    ];

    for name in expected {
        let crd = wait_for(&format!("CRD {name} to exist"), || {
            let api = crds.clone();
            async move { api.get_opt(name).await.ok().flatten() }
        })
        .await;

        let established = crd
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .map(|cs| {
                cs.iter()
                    .any(|c| c.type_ == "Established" && c.status == "True")
            })
            .unwrap_or(false);
        assert!(established, "CRD {name} exists but is not Established");
    }
}

/// Both control-plane roles must be installed and their RBAC wired.
#[tokio::test]
#[ignore = "requires a bootstrap-installed cluster; run `make kind-e2e-bootstrap`"]
async fn bootstrap_installs_the_controller_and_operator_with_their_rbac() {
    let client = client().await;
    let ns = namespace();
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let cluster_roles: Api<ClusterRole> = Api::all(client.clone());
    let bindings: Api<ClusterRoleBinding> = Api::all(client.clone());

    for role in ["banlieue-controller", "banlieue-operator"] {
        wait_for(&format!("Deployment {role}"), || {
            let api = deployments.clone();
            async move { api.get_opt(role).await.ok().flatten() }
        })
        .await;

        assert!(
            cluster_roles.get_opt(role).await.unwrap().is_some(),
            "bootstrap must install the {role} ClusterRole"
        );

        let binding = bindings
            .get_opt(role)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("bootstrap must install the {role} ClusterRoleBinding"));
        let subject = &binding.subjects.as_ref().expect("subjects")[0];
        assert_eq!(
            subject.namespace.as_deref(),
            Some(ns.as_str()),
            "{role} binding subject must follow --namespace"
        );
    }
}

/// **The bug-110 guard.**
///
/// The operator binds `banlieue-provider-<backend>` to every per-instance
/// ServiceAccount but cannot create it — minting the permissions it hands out
/// is the escalation path ADR-0012 refuses. If bootstrap does not install it,
/// every binding the operator writes points at a nonexistent role and the
/// provider pod runs with no permissions, while the install reports success.
#[tokio::test]
#[ignore = "requires a bootstrap-installed cluster; run `make kind-e2e-bootstrap`"]
async fn bootstrap_installs_the_shared_cluster_role_each_backend_binds() {
    let client = client().await;
    let cluster_roles: Api<ClusterRole> = Api::all(client.clone());

    for backend in backends() {
        let name = format!("banlieue-provider-{backend}");
        let role = cluster_roles.get_opt(&name).await.unwrap();
        assert!(
            role.is_some(),
            "bootstrap must install {name} — the operator binds it but cannot create it, \
             so without this every provider pod for the {backend} backend gets no permissions"
        );
        assert!(
            role.unwrap().rules.is_some_and(|r| !r.is_empty()),
            "{name} exists but grants nothing"
        );
    }
}

/// A ProviderClass per compiled-in backend, so registering a backend is just
/// `kubectl apply` of a Provider.
#[tokio::test]
#[ignore = "requires a bootstrap-installed cluster; run `make kind-e2e-bootstrap`"]
async fn bootstrap_seeds_a_provider_class_per_backend() {
    let client = client().await;
    let classes: Api<ProviderClass> = Api::all(client.clone());

    for backend in backends() {
        let class = classes
            .get_opt(&backend)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("bootstrap must seed a ProviderClass named {backend}"));

        assert_eq!(
            class.spec.backend, backend,
            "ProviderClass {backend} must name its own backend"
        );
        assert!(
            !class.spec.image.tag.is_empty(),
            "ProviderClass {backend} has no image tag"
        );
        assert_ne!(
            class.spec.image.tag, "latest",
            "a mutable tag makes the running version unknowable and defeats rollback"
        );
    }
}
