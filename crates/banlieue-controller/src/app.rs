// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue controller` entry point
//!
//! This is the library form of the main controller, invoked by the unified
//! `banlieue` binary as `banlieue controller` (see ADR-0004). [`run`] owns the
//! full lifecycle:
//!
//! 1. Initialises structured logging via [`banlieue_provider_sdk::bootstrap`].
//! 2. Builds a [`kube::Client`] via [`banlieue_provider_sdk::client`].
//! 3. Starts the health server on `:health_port` (`/livez`, `/readyz`,
//!    ADR-0093) and the metrics server on `:metrics_port` (`/metrics`,
//!    ADR-0091), both before leader election so a standby answers too.
//! 4. (Unless `--no-leader-elect`) acquires the
//!    `coordination.k8s.io/v1.Lease` named `--leader-election-id`
//!    before any reconciler runs; spawns a background renewer.
//! 5. Runs the [`kube::runtime::Controller`]s for `VirtualMachine` and
//!    `VSphereCluster` (and the pool, claim and image controllers) through
//!    the SDK's instrumented [`run_controller`].

use std::sync::Arc;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{
    Provider, VMClass, VMImage, VirtualMachine, VirtualMachineClaim, VirtualMachinePool,
};
use banlieue_api::infrastructure::{
    AzureVirtualMachine, CloudHypervisorMachine, LibvirtMachine, ProxmoxMachine, VSphereCluster,
    VSphereMachine,
};
use banlieue_provider_sdk::audit::{self, AuditConfig};
use banlieue_provider_sdk::bootstrap::{
    Observability, init_tracing, shutdown_signal, start_observability,
};
use banlieue_provider_sdk::client::build_client_with;
use banlieue_provider_sdk::defaults::{
    DEFAULT_HEALTH_PORT, DEFAULT_LEADER_ELECTION_NAMESPACE, DEFAULT_METRICS_PORT,
};
use banlieue_provider_sdk::health::Election;
use banlieue_provider_sdk::leader::{self, LeaderConfig};
use banlieue_provider_sdk::metrics::Metrics;
use banlieue_provider_sdk::runner::run_controller;
use clap::Args;
use futures::future::BoxFuture;
use futures::{FutureExt as _, StreamExt};
use kube::{
    Api, Client, ResourceExt,
    runtime::{
        Controller, WatchStreamExt, reflector, reflector::ObjectRef, watcher, watcher::Config,
    },
};
use tracing::{info, warn};

use crate::{
    context::Context,
    metrics::{FailureDomainCollector, HeartbeatCollector, VirtualMachineCollector},
    reconciler::bootstrap_data,
    reconciler::claim,
    reconciler::pool,
    reconciler::virtualmachine::{error_policy, reconcile},
    reconciler::vmimage,
    reconciler::vsphere_cluster,
};
use banlieue_provider_sdk::scope::{
    Scope, ScopeChanges, check_selector, resolve_scope, scope_is_implicit, start_gate,
};

const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-controller";

/// This role's name: the OTLP `service.name` (ADR-0092) and the `role`
/// label of `banlieue_leader` (ADR-0091).
pub const ROLE: &str = "banlieue-controller";

/// The `controller` label and audit key of the `VirtualMachine` controller (ADR-0091, ADR-0145).
const VM_CONTROLLER: &str = "VirtualMachine";

/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn"];

/// Command-line arguments for `banlieue controller`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Kubeconfig path, or a `KUBECONFIG`-style list (also read from
    /// `$KUBECONFIG`). When set it wins over every other source, in-cluster
    /// config included; when unset: in-cluster config, then
    /// `~/.kube/config`.
    #[arg(long, env = "KUBECONFIG")]
    pub kubeconfig: Option<String>,

    /// Serve exactly this namespace. Used when `--namespace-selector` is not
    /// set; when neither is, the controller serves the namespace it runs in.
    /// It never defaults to every namespace (ADR-0130).
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Serve every namespace whose labels match this label selector, e.g.
    /// `banlieue.io/tenant=true` or `team in (a,b)`. Takes precedence over
    /// `--namespace`. A platform admin onboards a team by labelling its
    /// namespace; no restart is needed (ADR-0130).
    #[arg(long, env = "BANLIEUE_NAMESPACE_SELECTOR")]
    pub namespace_selector: Option<String>,

    /// The namespace the platform's shared providers run in (ADR-0133). A
    /// `VirtualMachine` whose own namespace holds no `Provider` is scheduled
    /// onto the `Provider`s here that declare its namespace through
    /// `spec.namespaceSelector`. It is the value those providers already have
    /// as `--namespace`. Unset means no fallback; never "every namespace".
    #[arg(long, env = "BANLIEUE_PROVIDER_NAMESPACE")]
    pub provider_namespace: Option<String>,

    /// Health server bind port.
    #[arg(long, env = "BANLIEUE_HEALTH_PORT", default_value_t = DEFAULT_HEALTH_PORT)]
    pub health_port: u16,

    /// Metrics server bind port: Prometheus `GET /metrics` (ADR-0091).
    #[arg(long, env = "BANLIEUE_METRICS_PORT", default_value_t = DEFAULT_METRICS_PORT)]
    pub metrics_port: u16,

    /// Log format: `json` for SIEM-friendly output, `text` for human-readable
    /// (local development).
    #[arg(long, env = "RUST_LOG_FORMAT", default_value = "text")]
    pub log_format: String,

    /// Log level (`error`, `warn`, `info`, `debug`, `trace`). Overrides
    /// `RUST_LOG`; ignored if `RUST_LOG` is unset and this flag is also unset.
    #[arg(long, env = "BANLIEUE_LOG_LEVEL")]
    pub log_level: Option<String>,

    /// Disable leader election. Default is to elect a leader before
    /// running reconcilers, so multiple replicas can run safely.
    #[arg(long, env = "BANLIEUE_NO_LEADER_ELECT", default_value_t = false)]
    pub no_leader_elect: bool,

    /// Honour `VMImage.spec.firstBootSealing` (ADR-0088): allow a
    /// `tpmEnabled` class with an `Immediate` image whose clones seal their
    /// own partitions on first boot. On by default since ADR-0088 was
    /// accepted; `--enable-first-boot-sealing=false` turns it off, and while
    /// off a `VirtualMachine` whose image declares the field is refused
    /// (`FirstBootSealingDisabled`) rather than silently treated as plain.
    /// The bare flag still parses and enables.
    #[arg(
        long,
        env = "BANLIEUE_ENABLE_FIRST_BOOT_SEALING",
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set
    )]
    pub enable_first_boot_sealing: bool,

    /// Namespace the leader-election Lease lives in.
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_NAMESPACE",
        default_value = DEFAULT_LEADER_ELECTION_NAMESPACE,
    )]
    pub leader_election_namespace: String,

    /// Lease object name (the lock).
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_ID",
        default_value = DEFAULT_LEADER_ELECTION_ID,
    )]
    pub leader_election_id: String,

    /// Holder identity to write into the Lease. Falls back to `POD_NAME` /
    /// `HOSTNAME` / "unknown" if unset.
    #[arg(long, env = "BANLIEUE_LEADER_ELECTION_IDENTITY")]
    pub leader_election_identity: Option<String>,
}

/// Run the main banlieue controller to completion (until a shutdown signal or a
/// controller stream ends).
///
/// # Arguments
/// * `cli` - parsed `banlieue controller` arguments.
///
/// # Errors
/// Returns an error if logging init, kube client construction, binding the
/// health or metrics port, or leader-lease acquisition fails.
pub async fn run(cli: Cli) -> Result<()> {
    let telemetry = init_tracing(
        ROLE,
        &cli.log_format,
        cli.log_level.as_deref(),
        LOG_DIRECTIVES,
    )
    .context("initialising tracing")?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = ?cli.namespace,
        namespace_selector = ?cli.namespace_selector,
        leader_elect = !cli.no_leader_elect,
        "banlieue-controller starting"
    );

    // The audit's start-up grace counts from here (ADR-0145 decision 2).
    let started = std::time::Instant::now();

    let client = build_client_with(cli.kubeconfig.as_deref().map(std::ffi::OsStr::new))
        .await
        .context("constructing kube client")?;

    // ADR-0130: resolve which namespaces to serve, and never widen to all.
    let scope = resolve_scope(
        cli.namespace_selector.as_deref(),
        cli.namespace.as_deref(),
        Some(client.default_namespace()),
    )
    .context("resolving the namespace scope")?;
    if let Some(selector) = scope.selector() {
        check_selector(&client, selector)
            .await
            .context("validating --namespace-selector")?;
    }
    info!(scope = %scope.describe(), "serving");
    if scope_is_implicit(cli.namespace_selector.as_deref(), cli.namespace.as_deref()) {
        // ADR-0130 narrowed the default from every namespace to this one.
        warn!(
            scope = %scope.describe(),
            "no --namespace-selector or --namespace set: serving only this controller's own \
             namespace; set BANLIEUE_NAMESPACE_SELECTOR to serve tenant namespaces"
        );
    }

    let leader_cfg = (!cli.no_leader_elect).then(|| build_leader_config(&cli));
    let election = leader_cfg
        .as_ref()
        .map_or(Election::Disabled, LeaderConfig::election);
    let Observability { metrics, readiness } =
        start_observability(ROLE, cli.health_port, cli.metrics_port, election)
            .await
            .context("starting health and metrics servers")?;

    leader::elect(client.clone(), leader_cfg, &readiness)
        .await
        .context("acquiring leader lease")?;

    // Selector mode (ADR-0130): the controllers watch every namespace and gate
    // each reconcile on a Namespace reflector the API server has already
    // filtered by the selector. Its events also reconcile everything again, so
    // labelling a namespace in takes effect at once. Single-namespace modes
    // have a narrowed watch and need neither.
    let (gate, scope_changes) = start_gate(&client, scope.selector(), None).await?;

    let vm_api: Api<VirtualMachine> = scope.api(&client);

    // Infra CRs we own (status-mirror feedback). `owns` follows
    // ownerReferences back to the parent VirtualMachine for fast event-driven
    // reconciles when the provider updates infra status.
    let vsphere_api: Api<VSphereMachine> = scope.api(&client);

    // The libvirt counterpart (ADR-0050). Owned the same way and for the same
    // reason: without this watch a LibvirtMachine going Ready would only
    // reach its parent VirtualMachine on the next periodic requeue, so every
    // libvirt VM would sit `InfrastructureReady=False` for up to a full
    // requeue interval after it was in fact running.
    let libvirt_api: Api<LibvirtMachine> = scope.api(&client);

    // And the Cloud Hypervisor counterpart (ADR-0062), for the same reason.
    let cloud_hypervisor_api: Api<CloudHypervisorMachine> = scope.api(&client);

    // And the Proxmox counterpart (ADR-0075), for the same reason.
    let proxmox_api: Api<ProxmoxMachine> = scope.api(&client);

    // And the Azure counterpart (ADR-0164), for the same reason.
    let azure_api: Api<AzureVirtualMachine> = scope.api(&client);

    // VMImage is cluster-scoped; the image watcher requeues every VM
    // referencing an image whose status flipped.
    let image_api: Api<VMImage> = Api::all(client.clone());

    // VMClass is cluster-scoped; the class watcher requeues every VM
    // referencing a class whose spec changed (ADR-0035).
    let class_api: Api<VMClass> = Api::all(client.clone());

    // Provider selectors (`spec.placement.providerSelector`/
    // `failureDomainSelector`) match by label, not by name, so a Provider
    // edit can change *any* VirtualMachine's scheduling decision — unlike
    // VMImage/VMClass, there's no name to filter on. Requeue every VM on
    // any Provider event (ADR-0035); mirrors the VSphereCluster
    // controller's own identical `.watches(provider_api, ...)` below.
    // Provider edits are rare and operator-driven, so requeuing all is
    // cheap. Found live: without this, a Provider label change left an
    // already-scheduled VirtualMachine's placement stale until the next
    // periodic `requeue_default` tick instead of reacting immediately.
    let vm_provider_api: Api<Provider> = scope.api(&client);

    info!("starting VirtualMachine controller");
    let controller =
        Controller::new(vm_api, Config::default()).reconcile_all_on(scope_changes.trigger());
    let vm_store = controller.store();
    // ADR-0145: opt this controller in to the audit before its first reconcile, so the
    // ledger sees every invocation. The audit itself is spawned below.
    metrics.enable_audit(VM_CONTROLLER);
    let vm_store_for_audit = vm_store.clone();
    // The duplicate-address check (ADR-0083) reads every VM from this store
    // rather than listing them from the API on each reconcile.
    let ctx = Arc::new(
        Context::new(client.clone(), scope.namespace().map(str::to_owned))
            .with_namespace_gate(gate.clone())
            .with_vm_store(vm_store.clone())
            .with_first_boot_sealing(cli.enable_first_boot_sealing)
            .with_provider_namespace(cli.provider_namespace.clone()),
    );
    info!(
        provider_namespace = ?ctx.provider_namespace(),
        "provider namespace (ADR-0133): a VM whose namespace holds no Provider falls back to the \
         Providers here that declare it; unset means no fallback"
    );
    let vm_store_for_class = vm_store.clone();
    let vm_store_for_provider = vm_store.clone();
    // `banlieue_virtualmachines{phase}` counts this same store at scrape time.
    metrics.register_collector(Box::new(VirtualMachineCollector::new(vm_store.clone())));

    let vm_controller = controller
        .owns(vsphere_api, Config::default())
        .owns(libvirt_api, Config::default())
        .owns(cloud_hypervisor_api, Config::default())
        .owns(proxmox_api, Config::default())
        .owns(azure_api, Config::default())
        .watches(image_api, Config::default(), move |image: VMImage| {
            // Requeue every VM whose spec.image_ref.name matches this image.
            // VMImage updates are rare (operator-driven template imports), so
            // the linear scan over the store is fine.
            let image_name = image.name_any();
            vm_store
                .state()
                .into_iter()
                .filter(move |vm| vm.spec.image_ref.name == image_name)
                .map(|vm| ObjectRef::from_obj(vm.as_ref()))
                .collect::<Vec<_>>()
        })
        .watches(class_api, Config::default(), move |class: VMClass| {
            // Same rationale as the VMImage watcher above, keyed on
            // spec.class_ref.name instead (ADR-0035).
            let class_name = class.name_any();
            vm_store_for_class
                .state()
                .into_iter()
                .filter(move |vm| vm.spec.class_ref.name == class_name)
                .map(|vm| ObjectRef::from_obj(vm.as_ref()))
                .collect::<Vec<_>>()
        })
        .watches(
            vm_provider_api,
            Config::default(),
            move |_provider: Provider| {
                vm_store_for_provider
                    .state()
                    .into_iter()
                    .map(|vm| ObjectRef::from_obj(vm.as_ref()))
                    .collect::<Vec<_>>()
            },
        );
    let controller_fut = run_controller(
        vm_controller,
        VM_CONTROLLER,
        metrics.clone(),
        reconcile,
        error_policy,
        ctx.clone(),
    );

    // ADR-0145: a controller that stops acting on objects that exist says nothing, so this
    // one audits itself. Every minute it lists the VMs from the API (metadata only), checks
    // each against its own cache and ledger, and after five bad minutes in a row fails
    // `/livez` so the kubelet restarts the pod. A detached task: it must never end the
    // process, and a failed list leaves the verdict where it was.
    tokio::spawn(audit::run(
        VM_CONTROLLER,
        AuditConfig::default(),
        scope.api::<VirtualMachine>(&client),
        vm_store_for_audit,
        gate.clone(),
        metrics.clone(),
        readiness.clone(),
        started,
    ));

    // VSphereCluster (CAPI InfraCluster) controller. Aggregates Provider
    // failure domains into the CAPI status. A Provider's status changing
    // (failure domains added/removed) requeues every VSphereCluster — these
    // events are rare and operator-driven, so requeuing all is cheap.
    let vsc_api: Api<VSphereCluster> = scope.api(&client);
    let provider_api: Api<Provider> = scope.api(&client);

    info!("starting VSphereCluster controller");
    let vsc_controller =
        Controller::new(vsc_api, Config::default()).reconcile_all_on(scope_changes.trigger());
    let vsc_store = vsc_controller.store();

    let vsc_controller = vsc_controller.watches(
        provider_api,
        Config::default(),
        move |_provider: Provider| {
            vsc_store
                .state()
                .into_iter()
                .map(|c| ObjectRef::from_obj(c.as_ref()))
                .collect::<Vec<_>>()
        },
    );
    let vsc_fut = run_controller(
        vsc_controller,
        "VSphereCluster",
        metrics.clone(),
        vsphere_cluster::reconcile,
        vsphere_cluster::error_policy,
        ctx.clone(),
    );

    // `banlieue_provider_failure_domains{provider,kind}` (ADR-0091) reads
    // Provider status from a reflector of its own: the two Provider watches
    // above are triggers whose stores kube-runtime does not expose. Providers
    // are few and operator-created, so the extra watch is cheap.
    let fd_provider_api: Api<Provider> = scope.api(&client);
    let (fd_store, fd_writer) = reflector::store::<Provider>();
    metrics.register_collector(Box::new(FailureDomainCollector::new(fd_store.clone())));
    // ADR-0102's heartbeat gauge reads the same Provider status: one store.
    metrics.register_collector(Box::new(HeartbeatCollector::new(fd_store)));
    let fd_reflector = watcher(fd_provider_api, Config::default())
        .default_backoff()
        .reflect(fd_writer)
        .for_each(|event| async move {
            if let Err(e) = event {
                warn!(error = %e, "Provider metrics watch error; retrying");
            }
        });

    // Aggregate VMImage readiness. Providers each own their perProvider[]
    // entry; only this controller sees the whole list, so only it can say
    // whether the image is ready everywhere (ADR-0015). Pure aggregation —
    // no backend is contacted, so the extra watch is cheap.
    info!("starting VMImage aggregate-readiness controller");
    let image_api: Api<VMImage> = Api::all(client.clone());
    let image_fut = run_controller(
        Controller::new(image_api, Config::default()),
        "VMImage",
        metrics.clone(),
        vmimage::reconcile,
        vmimage::error_policy,
        ctx.clone(),
    );

    // VirtualMachinePool (ADR-0046). Watches its members by label, so a
    // member going Ready — or being claimed — reaches the pool immediately
    // rather than at the next periodic requeue: the whole point of a warm set
    // is that `available` is current. Not `.owns`: a bind re-parents the
    // member to its claim, and an owner-based watch would then tell only the
    // claim that the pool is a member short.
    info!("starting VirtualMachinePool controller");
    let pool_api: Api<VirtualMachinePool> = scope.api(&client);
    let pool_member_api: Api<VirtualMachine> = scope.api(&client);
    let pool_controller = Controller::new(pool_api, Config::default())
        .reconcile_all_on(scope_changes.trigger())
        .watches(pool_member_api, Config::default(), |vm: VirtualMachine| {
            pool::pool_for_member(&vm)
        });
    let pool_fut = run_controller(
        pool_controller,
        "VirtualMachinePool",
        metrics.clone(),
        pool::reconcile,
        pool::error_policy,
        ctx.clone(),
    );

    // VirtualMachineClaim (ADR-0047). Owns its bound member — re-parented
    // from the pool at bind time — so the member going Ready, or being
    // destroyed, reaches the claim without waiting for a periodic requeue.
    info!("starting VirtualMachineClaim controller");
    let claim_api: Api<VirtualMachineClaim> = scope.api(&client);
    let claim_member_api: Api<VirtualMachine> = scope.api(&client);
    let claim_fut = run_controller(
        Controller::new(claim_api, Config::default())
            .reconcile_all_on(scope_changes.trigger())
            .owns(claim_member_api, Config::default()),
        "VirtualMachineClaim",
        metrics.clone(),
        claim::reconcile,
        claim::error_policy,
        ctx.clone(),
    );

    // CAPI bootstrap data (ADR-0129). It runs only for the kinds whose
    // provider holds the guest until the data arrives and leaves the
    // controller's condition alone: Cloud Hypervisor. The libvirt, vSphere and
    // Proxmox providers still apply their whole status, conditions included,
    // from a snapshot, so the controller would fight them over
    // BootstrapDataReady, and their guests could boot before the data
    // arrived; they join with milestone 2. The controller also watches CAPI
    // Machines, so adoption and `dataSecretName` wake the machine they name.
    // It starts once the API server serves v1beta2 Machines, whenever that
    // is: a cluster that installs Cluster API after banlieue needs no
    // restart, and an API error while waiting stops nothing else here.
    let bootstrap_fut: BoxFuture<'static, ()> = {
        let client = client.clone();
        let scope = scope.clone();
        let scope_changes = scope_changes.clone();
        let metrics = metrics.clone();
        let ctx = ctx.clone();
        async move {
            info!(
                crd = bootstrap_data::MACHINE_CRD,
                "CAPI bootstrap-data controllers start once Cluster API v1beta2 Machines are served"
            );
            bootstrap_data::wait_for_capi_machines(&client).await;
            info!(
                kind = %<CloudHypervisorMachine as kube::Resource>::kind(&()),
                "starting CAPI bootstrap-data controllers"
            );
            bootstrap_data_controller::<CloudHypervisorMachine>(
                "CloudHypervisorMachineBootstrapData",
                &client,
                &scope,
                &scope_changes,
                &metrics,
                &ctx,
            )
            .await;
        }
        .boxed()
    };

    readiness.controllers_started();

    tokio::select! {
        () = controller_fut => {
            info!("VirtualMachine controller stream ended");
        }
        () = pool_fut => {
            info!("VirtualMachinePool controller stream ended");
        }
        () = bootstrap_fut => {
            info!("CAPI bootstrap-data controller streams ended");
        }
        () = claim_fut => {
            info!("VirtualMachineClaim controller stream ended");
        }
        () = image_fut => {
            info!("VMImage controller stream ended");
        }
        () = vsc_fut => {
            info!("VSphereCluster controller stream ended");
        }
        () = fd_reflector => {
            info!("Provider metrics watch ended");
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received; releasing controllers");
        }
    }

    telemetry.shutdown();
    Ok(())
}

/// One CAPI bootstrap-data controller for InfraMachine kind `K` (ADR-0129):
/// the kind itself, plus CAPI `Machine`s mapped to the InfraMachine their
/// `spec.infrastructureRef` names.
fn bootstrap_data_controller<K>(
    name: &'static str,
    client: &Client,
    scope: &Scope,
    scope_changes: &ScopeChanges,
    metrics: &Metrics,
    ctx: &Arc<Context>,
) -> BoxFuture<'static, ()>
where
    K: kube::Resource<DynamicType = (), Scope = k8s_openapi::NamespaceResourceScope>
        + banlieue_api::capi::BootstrapTarget
        + Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned
        + serde::Serialize
        + Send
        + Sync
        + 'static,
{
    // Same scope as every other controller here (ADR-0130): one namespace, or
    // every namespace gated per reconcile in selector mode.
    let kinds: Api<K> = scope.api(client);
    let machines = scope.api_with(client, &bootstrap_data::machine_resource());
    let controller = Controller::new(kinds, Config::default())
        .reconcile_all_on(scope_changes.trigger())
        .watches_with(
            machines,
            bootstrap_data::machine_resource(),
            Config::default(),
            |machine| bootstrap_data::infra_machine_for::<K>(&machine),
        );
    run_controller(
        controller,
        name,
        metrics.clone(),
        bootstrap_data::reconcile::<K>,
        bootstrap_data::error_policy::<K>,
        ctx.clone(),
    )
    .boxed()
}

/// Build a [`LeaderConfig`] from parsed CLI flags, filling the holder identity
/// from `--leader-election-identity` or the `POD_NAME` / `HOSTNAME` fallback.
fn build_leader_config(cli: &Cli) -> LeaderConfig {
    LeaderConfig::new(&cli.leader_election_namespace, &cli.leader_election_id)
        .with_identity(cli.leader_election_identity.clone())
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;
