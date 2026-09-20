// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared harness for the kind-based operator e2e suites (ADR-0014).
//!
//! The suites are split across several test binaries so each can be a separate
//! CI job on its own cluster (`make kind-e2e-<suite>`). Everything they have in
//! common — the fixture names, the polling helpers, setup/teardown, and the
//! `Provider` / `ProviderClass` builders — lives here so the split costs no
//! duplication.
//!
//! # The spawned provider pod is expected to be unhealthy
//!
//! Every `Provider` built here points at an endpoint under `.invalid`, which by
//! RFC 2606 can never resolve. Its provider pod will start, fail to reach a
//! backend, and stay NotReady; `status.workload.readyReplicas` will remain `0`.
//!
//! **That is the expected outcome, not a failure.** These suites assert the
//! operator's contract — that the workload is created, correctly shaped, owned,
//! reported, and garbage-collected. Whether a provider can reach a vCenter is
//! the vSphere provider's concern.
//!
//! Do not "fix" a suite by waiting for pod readiness. It will never be ready,
//! and the job would be permanently and unfixably red.

// Each test binary compiles this module in full but uses only the parts it
// needs; without this, every binary warns about the rest.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use banlieue_api::banlieue::{
    ImagePullPolicy, LoggingSpec, Provider, ProviderClass, ProviderClassSpec, ProviderConnection,
    ProviderImage, ProviderSpec,
};
use banlieue_api::common::LocalObjectReference;
use k8s_openapi::api::core::v1::{Namespace, Secret};
use k8s_openapi::api::rbac::v1::ClusterRoleBinding;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Client};

/// Namespace the suites create and work in. Deliberately not
/// `banlieue-system`, so a failed run cannot leave debris in the install.
pub const E2E_NAMESPACE: &str = "banlieue-e2e";

/// Cluster-scoped ProviderClass the suites install.
pub const E2E_CLASS: &str = "e2e-vsphere";

/// Provider under test.
pub const E2E_PROVIDER: &str = "e2e-vc";

/// Secret holding the (fake) backend credentials.
pub const E2E_SECRET: &str = "e2e-vc-creds";

/// Derived name the four NAMESPACED objects share.
pub const E2E_WORKLOAD: &str = "banlieue-provider-e2e-vsphere-e2e-vc";

/// Derived name of the CLUSTER-SCOPED ClusterRoleBinding, which is
/// namespace-qualified: a cluster-scoped object has no namespace to
/// disambiguate it, so two Providers sharing a name and class in different
/// namespaces would otherwise collide on one object.
pub const E2E_CLUSTER_BINDING: &str = "banlieue-provider-e2e-vsphere-banlieue-e2e-e2e-vc";

/// `.invalid` is reserved by RFC 2606 and can never resolve, so the spawned
/// provider fails fast and deterministically instead of hanging on DNS.
pub const UNREACHABLE_ENDPOINT: &str = "https://vcenter.invalid/sdk";

/// Field manager the operator writes under. Must match
/// `banlieue_operator::reconciler::provider::FIELD_MANAGER`.
pub const OPERATOR_FIELD_MANAGER: &str = "banlieue.io/operator";

/// Namespace used by the `workloadNamespace` override case, where workloads are
/// pinned away from their Provider's namespace.
pub const E2E_WORKLOAD_NAMESPACE: &str = "banlieue-e2e-workloads";

/// ProviderClass for the override case.
pub const E2E_PINNED_CLASS: &str = "e2e-vsphere-pinned";

/// Provider for the override case.
pub const E2E_PINNED_PROVIDER: &str = "e2e-vc-pinned";

/// Derived name of the override case's workload (pinned class + pinned provider).
pub const E2E_PINNED_WORKLOAD: &str = "banlieue-provider-e2e-vsphere-pinned-e2e-vc-pinned";

/// Derived name after swapping the DEFAULT provider onto the pinned class —
/// pinned class + default provider name, which is a different object again.
pub const E2E_SWAPPED_WORKLOAD: &str = "banlieue-provider-e2e-vsphere-pinned-e2e-vc";

/// How long to wait for the operator to converge.
pub const CONVERGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Poll interval while waiting.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Budget for a `ProviderClass` edit to reach the workloads that reference it.
///
/// Deliberately well BELOW the operator's 30s periodic requeue: landing inside
/// this window can only happen if the ProviderClass watch mapped the edit back
/// to its Providers. If the watch regressed, the change would still arrive —
/// just on the requeue — and a generous timeout would call that a pass.
pub const WATCH_PROPAGATION_BUDGET: Duration = Duration::from_secs(20);

/// How long to let the operator reconcile before asserting it created nothing.
///
/// The pause cases have no positive event to wait for, so the only honest way
/// to assert absence is to give the operator ample time to have acted.
pub const QUIESCE_WINDOW: Duration = Duration::from_secs(20);

/// Image the spawned provider workload runs. Must already be loaded into the
/// cluster (`make kind-load`); the pod cannot pull from a registry in CI.
pub fn e2e_image() -> String {
    std::env::var("BANLIEUE_E2E_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/firestoned/banlieue:local-dev".to_string())
}

/// Split `repo:tag` into its parts, tolerating a registry `host:port` prefix by
/// only considering a colon that follows the final `/`.
pub fn split_image(image: &str) -> (String, String) {
    let tag_start = image.rfind('/').map_or(0, |slash| slash + 1);
    match image[tag_start..].rfind(':') {
        Some(colon) => (
            image[..tag_start + colon].to_string(),
            image[tag_start + colon + 1..].to_string(),
        ),
        None => (image.to_string(), "local-dev".to_string()),
    }
}

/// Connect using the ambient kubeconfig (the kind context in CI).
pub async fn client() -> Client {
    Client::try_default()
        .await
        .expect("no reachable cluster — run `make kind-e2e`, or point KUBECONFIG at one")
}

/// Poll `check` until it returns `Some`, or fail with `what` after the timeout.
pub async fn wait_for<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {}s waiting for: {what}",
            CONVERGE_TIMEOUT.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll until `check` reports the resource is gone.
pub async fn wait_until_gone<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    wait_for(what, || {
        let fut = check();
        async move { if fut.await { Some(()) } else { None } }
    })
    .await;
}

/// Create the namespace, ProviderClass and Secret every suite needs, removing
/// any leftovers from a previous run first so the suites are re-runnable.
pub async fn setup(client: &Client) {
    teardown(client).await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let ns = Namespace {
        metadata: ObjectMeta {
            name: Some(E2E_NAMESPACE.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    // A namespace terminating from a previous run needs to finish first.
    wait_for("the e2e namespace to be creatable", || {
        let api = namespaces.clone();
        let ns = ns.clone();
        async move { api.create(&PostParams::default(), &ns).await.ok() }
    })
    .await;

    let classes: Api<ProviderClass> = Api::all(client.clone());
    let class = ProviderClass::new(
        E2E_CLASS,
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: {
                let (repository, tag) = split_image(&e2e_image());
                ProviderImage {
                    repository,
                    tag,
                    digest: None,
                    // The image is side-loaded into the node, never pulled.
                    pull_policy: Some(ImagePullPolicy::IfNotPresent),
                    pull_secrets: Vec::new(),
                }
            },
            workload_namespace: None,
            replicas: None,
            resources: None,
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        },
    );
    classes
        .create(&PostParams::default(), &class)
        .await
        .expect("creating the e2e ProviderClass");

    let secrets: Api<Secret> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(E2E_SECRET.to_string()),
            namespace: Some(E2E_NAMESPACE.to_string()),
            ..Default::default()
        },
        string_data: Some(BTreeMap::from([
            ("username".to_string(), "e2e@vsphere.local".to_string()),
            ("password".to_string(), "not-a-real-password".to_string()),
        ])),
        ..Default::default()
    };
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .expect("creating the e2e credentials Secret");
}

/// Best-effort removal of everything `setup` creates.
pub async fn teardown(client: &Client) {
    let classes: Api<ProviderClass> = Api::all(client.clone());
    let _ = classes.delete(E2E_CLASS, &DeleteParams::default()).await;

    let bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    let _ = bindings
        .delete(E2E_WORKLOAD, &DeleteParams::default())
        .await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let _ = namespaces
        .delete(E2E_NAMESPACE, &DeleteParams::default())
        .await;
}

/// Remove everything the `workloadNamespace` override case creates.
pub async fn teardown_pinned(client: &Client) {
    let classes: Api<ProviderClass> = Api::all(client.clone());
    let _ = classes
        .delete(E2E_PINNED_CLASS, &DeleteParams::default())
        .await;

    let bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    let _ = bindings
        .delete_collection(
            &DeleteParams::default(),
            &kube::api::ListParams::default()
                .labels(&format!("banlieue.io/provider-namespace={E2E_NAMESPACE}")),
        )
        .await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let _ = namespaces
        .delete(E2E_WORKLOAD_NAMESPACE, &DeleteParams::default())
        .await;
}

/// Build the Provider under test.
pub fn provider(paused: bool) -> Provider {
    let mut p = Provider::new(
        E2E_PROVIDER,
        ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: E2E_CLASS.to_string(),
            },
            connection: ProviderConnection {
                endpoint: UNREACHABLE_ENDPOINT.to_string(),
                credentials_ref: LocalObjectReference {
                    name: E2E_SECRET.to_string(),
                },
                insecure_skip_tls_verify: true,
                ca_bundle: None,
            },
            capabilities: Default::default(),
            paused,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
        },
    );
    p.metadata.namespace = Some(E2E_NAMESPACE.to_string());
    p
}

/// Build the `workloadNamespace`-pinned class used by the override and swap
/// suites.
pub fn pinned_class() -> ProviderClass {
    let (repository, tag) = split_image(&e2e_image());
    ProviderClass::new(
        E2E_PINNED_CLASS,
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: ProviderImage {
                repository,
                tag,
                digest: None,
                pull_policy: Some(ImagePullPolicy::IfNotPresent),
                pull_secrets: Vec::new(),
            },
            workload_namespace: Some(E2E_WORKLOAD_NAMESPACE.to_string()),
            replicas: None,
            resources: None,
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        },
    )
}

/// Create `E2E_WORKLOAD_NAMESPACE`, waiting out a termination from a prior run.
///
/// The operator deliberately does not create the pinned workload namespace —
/// that is an install concern, not a reconcile one — so the suites that use it
/// have to.
pub async fn create_workload_namespace(client: &Client) {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    wait_for("the pinned workload namespace to be creatable", || {
        let api = namespaces.clone();
        async move {
            api.create(
                &PostParams::default(),
                &Namespace {
                    metadata: ObjectMeta {
                        name: Some(E2E_WORKLOAD_NAMESPACE.to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .ok()
        }
    })
    .await;
}
