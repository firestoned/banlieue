// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VSphereMachine` reconciler — clone a VM from its per-zone template
//! (ADR-0024).
//!
//! Create-path only: [`ensure_vm`] resolves `spec`'s names to concrete
//! vCenter morefs, clones from the per-zone template with
//! [`build_guestinfo`]'s `extraConfig`, and drives `spec.desiredPowerState`
//! — but only on first provision (`status.vmRef` unset). Once provisioned,
//! [`reconcile`] skips all of that (template/datastore/network resolution,
//! cloning) forever — update/drift handling and status mirroring beyond
//! power state (addresses, CAPI conditions beyond `Ready`) are deliberately
//! out of scope here, same as the ADR. It does perform exactly one cheap
//! read every pass — `VSphereClient::power_state` — so
//! `status.observedPowerState` reflects a VM manually powered
//! off/suspended out-of-band in vCenter (ADR-0034, a narrow amendment to
//! ADR-0024's "no round-trip" rule, not a reversal of it). The
//! `CloneVM_Task`/`PowerOnVM_Task` calls themselves
//! ([`crate::client::VSphereClient::clone_vm`] /
//! `set_power_state`) are, like every other real vCenter mutation in this
//! crate, verified live rather than unit tested — only the decision logic
//! in this module has a test surface (via [`crate::client::FakeClient`]).

use std::sync::Arc;

use banlieue_api::banlieue::Provider;
use banlieue_api::common::{InitializationStatus, PowerState};
use banlieue_api::infrastructure::{
    VSphereMachine, VSphereMachineSpec, VSphereMachineStatus, VSphereNicSpec,
};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_VSPHERE;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use base64::Engine;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use crate::client::{CloneVmRequest, VSphereClient};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::import::resolve_concrete_datastore;

/// Condition type set on `VSphereMachine.status.conditions`.
mod condition_types {
    pub const READY: &str = "Ready";
}

/// Stable `reason` strings on the conditions. Keep these stable — operators
/// match against them in alerts and tests.
mod reasons {
    pub const RECONCILED: &str = "Reconciled";
    pub const PROVIDER_NOT_FOUND: &str = "ProviderNotFound";
    pub const SECRET_MISSING: &str = "SecretMissing";
    pub const SECRET_INVALID: &str = "SecretInvalid";
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    pub const PROVISION_FAILED: &str = "ProvisionFailed";
    /// ADR-0034: `status.initialization.provisioned == true` but
    /// `status.vmRef` is unset — an inconsistent status a prior bug (or
    /// manual edit) left behind. Detected and reported, never
    /// auto-repaired: recovering it would require re-resolving the VM by
    /// name (the same datacenter/folder round-trip ADR-0024 exists to
    /// avoid), and risks adopting the wrong VM if a same-named one exists
    /// elsewhere.
    pub const BACKEND_REF_MISSING: &str = "BackendRefMissing";
    /// ADR-0034: `status.vmRef` names a moref that no longer exists in
    /// vCenter (deleted out-of-band, or never actually created). Detected
    /// and reported, never auto-recreated — the operator decides whether
    /// that was intentional.
    pub const BACKEND_MISSING: &str = "BackendMissing";
}

/// Finalizer set on every `VSphereMachine` reconciled by this provider
/// (ADR-0026) — mirrors `banlieue-controller`'s own
/// `banlieue.io/virtualmachine` finalizer on the parent `VirtualMachine`.
/// Blocks deletion until the backend VM is confirmed destroyed, completing
/// the two-level cascade that controller's finalizer already assumes exists.
pub const VSPHERE_MACHINE_FINALIZER: &str = "banlieue.io/vspheremachine";

const GUESTINFO_NETWORK_HOSTNAME: &str = "guestinfo.network.hostname";
const GUESTINFO_NETWORK_IP: &str = "guestinfo.network.ip";
const GUESTINFO_NETWORK_PREFIX: &str = "guestinfo.network.prefix";
const GUESTINFO_NETWORK_GATEWAY: &str = "guestinfo.network.gateway";
const GUESTINFO_NETWORK_DNS: &str = "guestinfo.network.dns";
const GUESTINFO_NETWORK_DOMAIN: &str = "guestinfo.network.domain";
const GUESTINFO_USERDATA: &str = "guestinfo.userdata";
const GUESTINFO_USERDATA_ENCODING: &str = "guestinfo.userdata.encoding";
const GUESTINFO_METADATA: &str = "guestinfo.metadata";

/// Build the `extraConfig` `guestinfo.*` key/value pairs to set on a
/// `CloneVM_Task` (ADR-0024), matching this environment's existing
/// hand-provisioned VM convention exactly: static network config under
/// `guestinfo.network.*`, and a base64 cloud-config under
/// `guestinfo.userdata` / `guestinfo.userdata.encoding`.
///
/// `guestinfo.network.hostname` is unconditional — `vm_name`
/// (the `VirtualMachine`'s own name, same source as the userData
/// placeholder set's `${VM_NAME}`) regardless of DHCP or static network,
/// since hostname is VM identity, not network config. This lets a plain
/// `dhcp`-only node still get a stable hostname without authoring a
/// per-host `userData` cloud-config just to set one.
///
/// The rest of `guestinfo.network.*` is a flat, non-indexed convention — it
/// can only represent one primary static network, not one per NIC — so this
/// uses the *first* NIC with a resolved static [`IpamSource::Static`]
/// override, if any; a plain `dhcp` NIC contributes nothing beyond the
/// hostname. `rendered_userdata` is the already placeholder-substituted
/// cloud-config ([`banlieue_provider_sdk::guestdata::render_placeholders`]);
/// `None` omits both userdata keys entirely.
///
/// `guestinfo.metadata` is also unconditional and independent of
/// `rendered_userdata` — see [`build_guestinfo_metadata`] (ADR-0029).
pub fn build_guestinfo(
    vm_name: &str,
    nics: &[VSphereNicSpec],
    rendered_userdata: Option<&str>,
) -> Vec<(String, String)> {
    let mut out = vec![(GUESTINFO_NETWORK_HOSTNAME.to_string(), vm_name.to_string())];

    let static_cfg = nics
        .iter()
        .find(|n| n.ipam.static_.is_some())
        .and_then(|n| n.ipam.static_.as_ref());

    if let Some(static_cfg) = static_cfg {
        out.push((GUESTINFO_NETWORK_IP.to_string(), static_cfg.address.clone()));
        out.push((
            GUESTINFO_NETWORK_PREFIX.to_string(),
            static_cfg.prefix.to_string(),
        ));
        if let Some(gateway) = &static_cfg.gateway {
            out.push((GUESTINFO_NETWORK_GATEWAY.to_string(), gateway.clone()));
        }
        if !static_cfg.nameservers.is_empty() {
            out.push((
                GUESTINFO_NETWORK_DNS.to_string(),
                static_cfg.nameservers.join(","),
            ));
        }
        if let Some(domain) = &static_cfg.domain {
            out.push((GUESTINFO_NETWORK_DOMAIN.to_string(), domain.clone()));
        }
    }

    if let Some(userdata) = rendered_userdata {
        let encoded = base64::engine::general_purpose::STANDARD.encode(userdata);
        out.push((GUESTINFO_USERDATA.to_string(), encoded));
        out.push((
            GUESTINFO_USERDATA_ENCODING.to_string(),
            "base64".to_string(),
        ));
    }

    let domain = static_cfg.and_then(|s| s.domain.as_deref());
    out.push((
        GUESTINFO_METADATA.to_string(),
        build_guestinfo_metadata(vm_name, domain),
    ));

    out
}

/// Build the base64-encoded `guestinfo.metadata` YAML document — real
/// cloud-init's own VMware GuestInfo datasource schema (`instance-id`,
/// `local-hostname`), not banlieue's flat `guestinfo.network.*` convention
/// above. `instance-id` is the VM's own (already-unique) name; `local-
/// hostname` is the FQDN when `domain` is known, else the plain hostname —
/// `cc_set_hostname` derives both the short hostname and the FQDN from one
/// dotted value, so no separate `fqdn` key exists to set (ADR-0029).
fn build_guestinfo_metadata(vm_name: &str, domain: Option<&str>) -> String {
    let local_hostname = local_hostname(vm_name, domain);
    let yaml = format!("instance-id: {vm_name}\nlocal-hostname: {local_hostname}\n");
    base64::engine::general_purpose::STANDARD.encode(yaml)
}

/// Combine `vm_name` and `domain` into the FQDN `local-hostname` value,
/// without double-appending the domain when `vm_name` is already fully
/// qualified with it. `metadata.name` is a DNS-1123 subdomain and permits
/// dots (confirmed live: a `VirtualMachine` named as a full FQDN applies
/// cleanly), so a VM already named `db-01.example.com` with `domain =
/// Some("example.com")` must render as `db-01.example.com`, not
/// `db-01.example.com.example.com`. Suffix match is case-insensitive — DNS
/// names are case-insensitive.
fn local_hostname(vm_name: &str, domain: Option<&str>) -> String {
    let Some(domain) = domain else {
        return vm_name.to_string();
    };
    let suffix = format!(".{}", domain.to_ascii_lowercase());
    if vm_name.to_ascii_lowercase().ends_with(&suffix) {
        vm_name.to_string()
    } else {
        format!("{vm_name}.{domain}")
    }
}

/// Outcome of [`ensure_vm`] — what the caller should patch onto
/// `VSphereMachine.status`.
#[derive(Debug)]
pub struct ProvisionOutcome {
    /// The VM's moref — the just-created clone's, or the pre-existing one
    /// `existing_vm_ref` named.
    pub vm_ref: String,
    /// `true` when `existing_vm_ref` was already set and nothing further
    /// happened this reconcile (ADR-0024 scopes update/drift handling
    /// after initial provisioning out — see the module doc comment).
    pub already_provisioned: bool,
    /// The power state just set on a fresh clone (`spec.desired_power_state`,
    /// confirmed by `set_power_state`'s own task wait) — `None` when this
    /// outcome came from the `already_provisioned` early return, which
    /// performs no vCenter read of its own (ADR-0034: `reconcile`'s own
    /// separate `refresh_power_state` call is what keeps this current once
    /// provisioned, not this field).
    pub power_state: Option<PowerState>,
}

/// The `VSphereMachine` create path (ADR-0024): resolve every name in
/// `spec` to a concrete vCenter moref, clone from the per-zone template,
/// and drive `spec.desiredPowerState` — but only on first provision.
///
/// `existing_vm_ref` is `VSphereMachine.status.vmRef` from the last
/// reconcile; when already set, this is a no-op (idempotent — no re-clone,
/// no repeated power-state calls, matching this module's currently
/// create-only scope). `rendered_userdata` is `spec.userData` — already
/// resolved and placeholder-substituted content by the time it reaches
/// here (`banlieue-controller` does that, ADR-0025 — this module never
/// reads a Secret).
pub async fn ensure_vm(
    client: &dyn VSphereClient,
    spec: &VSphereMachineSpec,
    vm_name: &str,
    existing_vm_ref: Option<&str>,
    rendered_userdata: Option<&str>,
) -> Result<ProvisionOutcome> {
    if let Some(vm_ref) = existing_vm_ref {
        return Ok(ProvisionOutcome {
            vm_ref: vm_ref.to_string(),
            already_provisioned: true,
            power_state: None,
        });
    }

    info!(datacenter = %spec.datacenter, "resolving datacenter");
    let dc = client
        .list_datacenters()
        .await?
        .into_iter()
        .find(|d| d.name == spec.datacenter)
        .ok_or_else(|| Error::Vsphere(format!("datacenter {:?} not found", spec.datacenter)))?;

    info!(cluster = %spec.cluster, "resolving cluster");
    let cluster = client
        .list_clusters(&dc)
        .await?
        .into_iter()
        .find(|c| c.name == spec.cluster)
        .ok_or_else(|| {
            Error::Vsphere(format!(
                "cluster {:?} not found in datacenter {:?}",
                spec.cluster, spec.datacenter
            ))
        })?;

    info!(template = %spec.template, "resolving template");
    let template = client
        .find_template(&dc, spec.template_folder.as_deref(), &spec.template)
        .await?
        .ok_or_else(|| {
            Error::Vsphere(format!(
                "template {:?} not found in datacenter {:?}{}",
                spec.template,
                spec.datacenter,
                spec.template_folder
                    .as_deref()
                    .map(|f| format!(" folder {f:?}"))
                    .unwrap_or_default()
            ))
        })?;
    info!(template_moref = %template.moref, "template resolved");

    info!(datastore = %spec.datastore, "resolving datastore");
    let datastores = client.list_datastores(&cluster).await?;
    let datastore_name = resolve_concrete_datastore(&datastores, &spec.datastore)
        .map_err(|e| Error::Vsphere(e.to_string()))?;
    // resolve_concrete_datastore returns the datastore's display name (what
    // the image-import CLI needs for `[name] path`-style datastore paths);
    // CloneVM_Task's relocate spec needs the actual moref instead (found
    // live: passing the name as a ManagedObjectReference value faults with
    // ManagedObjectNotFound, since it's not a real object ID).
    let datastore_moref = datastores
        .iter()
        .find(|d| d.name == datastore_name)
        .map(|d| d.moref.clone())
        .ok_or_else(|| {
            Error::Vsphere(format!(
                "resolved datastore {datastore_name:?} missing its own moref — this is a bug"
            ))
        })?;
    info!(datastore = %datastore_name, datastore_moref = %datastore_moref, "datastore resolved");

    let nic_spec = spec
        .network
        .first()
        .ok_or(Error::Missing("VSphereMachineSpec.network[0]"))?;
    info!(network = %nic_spec.port_group, "resolving network");
    let networks = client.list_networks(&cluster).await?;
    let network = networks
        .into_iter()
        .find(|n| n.name == nic_spec.port_group)
        .ok_or_else(|| {
            Error::Vsphere(format!(
                "network {:?} not reachable from cluster {:?}",
                nic_spec.port_group, spec.cluster
            ))
        })?;
    info!(network = %network.name, network_moref = %network.moref, "network resolved");

    let extra_config = build_guestinfo(vm_name, &spec.network, rendered_userdata);

    info!(vm_name, template_moref = %template.moref, "submitting CloneVM_Task");
    let vm_ref = client
        .clone_vm(&CloneVmRequest {
            datacenter_moref: dc.moref,
            cluster_moref: cluster.moref,
            template_moref: template.moref,
            datastore_moref,
            network: network.name,
            network_moref: network.moref,
            network_distributed: network.distributed,
            num_cpus: spec.num_cpus as i32,
            memory_mib: i64::from(spec.memory_mi_b),
            folder: spec.folder.clone(),
            vm_name: vm_name.to_string(),
            extra_config,
        })
        .await?;
    info!(vm_ref = %vm_ref, "CloneVM_Task complete");

    // CloneVM_Task always clones powered off (ADR-0024's clone spec sets
    // power_on: false). Calling set_power_state(PoweredOff) again is a
    // redundant no-op transition that real vCenter rejects with
    // InvalidPowerState; if that propagated via `?` before this function
    // returns, the caller never learns vm_ref and re-clones every
    // subsequent reconcile, hitting DuplicateName forever (found live
    // testing ADR-0038 userData with desiredPowerState: PoweredOff).
    if spec.desired_power_state == PowerState::PoweredOff {
        info!(vm_ref = %vm_ref, "clone already powered off, skipping redundant power-state task");
    } else {
        info!(vm_ref = %vm_ref, desired_power_state = ?spec.desired_power_state, "setting power state");
        client
            .set_power_state(&vm_ref, spec.desired_power_state.clone())
            .await?;
        info!(vm_ref = %vm_ref, power_state = ?spec.desired_power_state, "power state confirmed");
    }

    Ok(ProvisionOutcome {
        vm_ref,
        power_state: Some(spec.desired_power_state.clone()),
        already_provisioned: false,
    })
}

/// Top-level reconcile entrypoint registered with [`kube::runtime::Controller`].
pub async fn reconcile(machine: Arc<VSphereMachine>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = machine.namespace().ok_or(Error::Missing("namespace"))?;
    let name = machine.name_any();
    let generation = machine.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!(
        "reconcile",
        kind = "VSphereMachine",
        namespace = %namespace,
        name = %name,
        generation,
    );
    let _enter = span.enter();
    info!("reconciling VSphereMachine");

    let api: Api<VSphereMachine> = Api::namespaced(ctx.client.clone(), &namespace);

    // ADR-0026: deletion takes priority over every other branch below,
    // mirroring banlieue-controller's own VirtualMachine reconciler.
    if machine.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &machine, &ctx, &namespace).await;
    }

    ensure_finalizer(&api, machine.as_ref(), VSPHERE_MACHINE_FINALIZER).await?;

    let already_provisioned = machine
        .status
        .as_ref()
        .is_some_and(|s| s.initialization.provisioned == Some(true));

    let existing = machine
        .status
        .as_ref()
        .map(|s| s.conditions.clone())
        .unwrap_or_default();

    let provider_api: Api<Provider> = Api::namespaced(ctx.client.clone(), &namespace);
    let provider = match provider_api.get(&machine.spec.provider_ref.name).await {
        Ok(p) => p,
        Err(kube::Error::Api(e)) if e.code == 404 => {
            warn!(provider = %machine.spec.provider_ref.name, "Provider not found");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
                reasons::PROVIDER_NOT_FOUND,
                format!("Provider {:?} not found", machine.spec.provider_ref.name),
            )
            .await?;
            return Ok(requeue_on_error());
        }
        Err(e) => return Err(Error::Kube(e)),
    };

    let creds = match super::provider::read_credentials(&ctx, &namespace, &provider.spec.connection)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            let reason = match &e {
                Error::Missing(_) => reasons::SECRET_MISSING,
                _ => reasons::SECRET_INVALID,
            };
            warn!(error = %e, "credentials resolution failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
                reason,
                format!("{e}"),
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    let ca_bundle_pem = match crate::reconciler::ca_bundle::resolve_ca_bundle(
        &ctx,
        &namespace,
        &provider.spec.connection.ca_bundle,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "caBundle resolution failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
                reasons::CONNECT_FAILED,
                format!("{e}"),
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    let client = match ctx
        .vsphere
        .build(&provider.spec.connection, &creds, ca_bundle_pem.as_deref())
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "vCenter connect failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
                reasons::CONNECT_FAILED,
                format!("{e}"),
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    // Create-path-only scope (ADR-0024) for everything except power state:
    // once a VM exists, skip template/datastore/network resolution and
    // cloning entirely — that reasoning still holds. Do perform one cheap
    // read (`power_state`, ADR-0034) so `status.observedPowerState` (and
    // the parent VirtualMachine's own Power printcolumn) reflect a VM
    // manually powered off/suspended out-of-band in vCenter, rather than
    // staying frozen at whatever was true at creation forever.
    if already_provisioned {
        return refresh_power_state(
            &ctx,
            &namespace,
            &name,
            generation,
            &machine,
            client.as_ref(),
        )
        .await;
    }

    // ADR-0025: spec.userData is already-resolved, already-rendered content
    // by the time it reaches here — banlieue-controller reads and renders
    // the Secret; the provider never touches a Secret for this.
    let existing_vm_ref = machine.status.as_ref().and_then(|s| s.vm_ref.as_deref());

    match ensure_vm(
        client.as_ref(),
        &machine.spec,
        &name,
        existing_vm_ref,
        machine.spec.user_data.as_deref(),
    )
    .await
    {
        Ok(outcome) => {
            info!(vm_ref = %outcome.vm_ref, "VSphereMachine provisioned");
            patch_status_success(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
                outcome.vm_ref,
                outcome.power_state,
            )
            .await?;
            Ok(requeue_long())
        }
        Err(e) => {
            warn!(error = %e, "provisioning failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
                reasons::PROVISION_FAILED,
                format!("{e}"),
            )
            .await?;
            Ok(requeue_on_error())
        }
    }
}

/// Deletion path (ADR-0026): resolve the same vCenter client the create
/// path does, destroy the backend VM if one was ever cloned, then drop the
/// finalizer. Errors propagate to `error_policy` and leave the finalizer in
/// place — the parent `VirtualMachine`'s own cascade-wait means the whole
/// delete blocks here, the correct conservative behavior for a destructive,
/// irreversible operation.
async fn finalize(
    api: &Api<VSphereMachine>,
    machine: &VSphereMachine,
    ctx: &Context,
    namespace: &str,
) -> Result<Action> {
    info!("finalizing VSphereMachine");
    let vm_ref = machine.status.as_ref().and_then(|s| s.vm_ref.as_deref());

    if vm_ref.is_some() {
        let provider_api: Api<Provider> = Api::namespaced(ctx.client.clone(), namespace);
        let provider = provider_api.get(&machine.spec.provider_ref.name).await?;
        let creds =
            super::provider::read_credentials(ctx, namespace, &provider.spec.connection).await?;
        let ca_bundle_pem = crate::reconciler::ca_bundle::resolve_ca_bundle(
            ctx,
            namespace,
            &provider.spec.connection.ca_bundle,
        )
        .await?;
        let client = ctx
            .vsphere
            .build(&provider.spec.connection, &creds, ca_bundle_pem.as_deref())
            .await?;
        finalize_vm(client.as_ref(), vm_ref).await?;
    }

    remove_finalizer(api, machine, VSPHERE_MACHINE_FINALIZER).await?;
    Ok(requeue_default())
}

/// The finalize half of ADR-0026: destroy the backend VM if one was ever
/// created. Pure enough to unit test via `FakeClient` — mirrors
/// [`ensure_vm`]'s own testability (client trait object injected, no K8s
/// access), same rationale as the module doc comment.
async fn finalize_vm(client: &dyn VSphereClient, vm_ref: Option<&str>) -> Result<()> {
    let Some(vm_ref) = vm_ref else {
        // Create path never got far enough to clone a VM — nothing to destroy.
        return Ok(());
    };
    client.destroy_vm(vm_ref).await
}

/// `error_policy` callback the controller invokes when [`reconcile`] returns
/// `Err`. Short backoff — most errors here are transient (network blips,
/// vCenter session expiry).
pub fn error_policy(_machine: Arc<VSphereMachine>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "reconcile error policy fired");
    requeue_on_error()
}

/// SSA-patch `VSphereMachine.status` on successful provisioning.
async fn patch_status_success(
    ctx: &Context,
    namespace: &str,
    name: &str,
    generation: i64,
    existing_conditions: &[Condition],
    vm_ref: String,
    power_state: Option<PowerState>,
) -> Result<()> {
    let mut conditions = existing_conditions.to_vec();
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::TRUE,
        reasons::RECONCILED,
        "VSphereMachine provisioned",
        generation,
    );

    let status = VSphereMachineStatus {
        initialization: InitializationStatus {
            provisioned: Some(true),
        },
        failure_domain: None,
        addresses: Vec::new(),
        vm_ref: Some(vm_ref),
        instance_uuid: None,
        observed_power_state: power_state,
        conditions,
        observed_generation: Some(generation),
    };
    patch_machine_status(ctx, namespace, name, status).await
}

/// ADR-0034: once provisioned, `reconcile` no longer resolves
/// template/datastore/network or clones — that ADR-0024 reasoning still
/// holds. It does perform exactly one cheap read (`VSphereClient::
/// power_state`) so `status.observedPowerState` (and the parent
/// VirtualMachine's own Power printcolumn) reflect a VM manually powered
/// off/suspended out-of-band in vCenter, rather than staying frozen at
/// whatever was true at creation. Only patches when the observed value
/// actually changed, to avoid a no-op status write every `requeue_long`
/// tick.
async fn refresh_power_state(
    ctx: &Context,
    namespace: &str,
    name: &str,
    generation: i64,
    machine: &VSphereMachine,
    client: &dyn VSphereClient,
) -> Result<Action> {
    let Some(current) = machine.status.clone() else {
        warn!("provisioned but status is unset — nothing to refresh");
        return Ok(requeue_long());
    };
    // Self-heal (detect-and-report only, ADR-0034): `provisioned=true` with
    // no `vmRef` is an inconsistent status this reconciler cannot safely
    // repair on its own (recovering it by name would need the same
    // datacenter/folder round-trip ADR-0024 exists to avoid, and risks
    // adopting the wrong VM) — surface it instead of silently doing
    // nothing forever, which is what happened before this fix.
    let Some(vm_ref) = current.vm_ref.clone() else {
        warn!("provisioned but status.vmRef is unset — reporting BackendRefMissing");
        let next = status_reporting_backend_problem(
            current,
            reasons::BACKEND_REF_MISSING,
            "status.initialization.provisioned is true but status.vmRef is unset; this cannot be auto-repaired — recreate this VirtualMachine if the backend VM is actually gone".to_string(),
            generation,
        );
        patch_machine_status(ctx, namespace, name, next).await?;
        return Ok(requeue_long());
    };
    match client.power_state(&vm_ref).await {
        Ok(observed) => {
            let already_reported_healthy = current
                .conditions
                .iter()
                .any(|c| c.type_ == condition_types::READY && c.status == condition_status::TRUE);
            if current.observed_power_state.as_ref() != Some(&observed) || !already_reported_healthy
            {
                info!(vm_ref, power_state = ?observed, "observed power state changed");
                let next = status_with_observed_power_state(current, observed, generation);
                patch_machine_status(ctx, namespace, name, next).await?;
            }
            Ok(requeue_long())
        }
        Err(e) if is_backend_missing_error(&e) => {
            warn!(vm_ref, error = %e, "backend VM no longer exists — reporting BackendMissing");
            let next = status_reporting_backend_problem(
                current,
                reasons::BACKEND_MISSING,
                format!("backend VM {vm_ref:?} no longer exists in vCenter: {e}"),
                generation,
            );
            patch_machine_status(ctx, namespace, name, next).await?;
            Ok(requeue_long())
        }
        Err(e) => Err(e),
    }
}

/// True when `e` is vCenter's `ManagedObjectNotFound` fault — the backend
/// VM this `VSphereMachine` names no longer exists. Same string-matching
/// convention `destroy_vm` already uses for the identical fault (vim_rs's
/// error type isn't preserved as a structured enum across the client
/// boundary, so string matching is what's available).
fn is_backend_missing_error(e: &Error) -> bool {
    e.to_string()
        .to_lowercase()
        .contains("managedobjectnotfound")
}

/// Report a detected backend inconsistency on `Ready`, preserving every
/// other field of `current` — never a narrow patch (see
/// `status_with_observed_power_state`'s doc comment for why that's unsafe
/// under SSA).
fn status_reporting_backend_problem(
    mut current: VSphereMachineStatus,
    reason: &str,
    message: String,
    generation: i64,
) -> VSphereMachineStatus {
    set_condition(
        &mut current.conditions,
        condition_types::READY,
        condition_status::FALSE,
        reason,
        &message,
        generation,
    );
    current.observed_generation = Some(generation);
    current
}

/// Build the status to (re-)apply after observing a new power state —
/// starting from the *entire current* status, not a narrow
/// `{observedPowerState, observedGeneration}` object. Pure, so this
/// preservation contract is unit-testable without a kube client.
///
/// Found live: a narrower apply from the same field manager that
/// previously applied the full struct (`patch_status_success`) makes the
/// apiserver retract this manager's ownership of every field the narrower
/// payload omits. Since nothing else owns `vmRef`/`conditions`/
/// `initialization`, SSA silently wiped them the moment a narrow patch like
/// that stopped erroring (it had been failing on a stale CRD schema up to
/// that point) — `finalize()` then read `vm_ref` as `None` and skipped
/// `destroy_vm` entirely, orphaning the backend VM in vCenter on delete.
/// The same field manager must always apply the same complete field set.
fn status_with_observed_power_state(
    mut current: VSphereMachineStatus,
    observed: PowerState,
    generation: i64,
) -> VSphereMachineStatus {
    current.observed_power_state = Some(observed);
    current.observed_generation = Some(generation);
    // A successful power_state read means the backend VM demonstrably
    // exists and answered — restore Ready=True/Reconciled here so a
    // BackendMissing/BackendRefMissing condition reported earlier (e.g. a
    // transient vCenter blip) doesn't stay stuck False forever once the
    // problem is gone.
    set_condition(
        &mut current.conditions,
        condition_types::READY,
        condition_status::TRUE,
        reasons::RECONCILED,
        "VSphereMachine provisioned",
        generation,
    );
    current
}

/// SSA-patch a failure condition onto `VSphereMachine.status.conditions`.
async fn patch_status_failed(
    ctx: &Context,
    namespace: &str,
    name: &str,
    generation: i64,
    existing_conditions: &[Condition],
    reason: &str,
    message: String,
) -> Result<()> {
    let mut conditions = existing_conditions.to_vec();
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::FALSE,
        reason,
        &message,
        generation,
    );

    let patch = json!({
        "apiVersion": VSphereMachine::api_version(&()).to_string(),
        "kind": VSphereMachine::kind(&()).to_string(),
        "metadata": { "name": name, "namespace": namespace },
        "status": {
            "conditions": conditions,
            "observedGeneration": generation,
        },
    });
    apply_status_patch(ctx, namespace, name, patch).await
}

async fn patch_machine_status(
    ctx: &Context,
    namespace: &str,
    name: &str,
    status: VSphereMachineStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": VSphereMachine::api_version(&()).to_string(),
        "kind": VSphereMachine::kind(&()).to_string(),
        "metadata": { "name": name, "namespace": namespace },
        "status": status,
    });
    apply_status_patch(ctx, namespace, name, patch).await
}

async fn apply_status_patch(
    ctx: &Context,
    namespace: &str,
    name: &str,
    patch: serde_json::Value,
) -> Result<()> {
    let api: Api<VSphereMachine> = Api::namespaced(ctx.client.clone(), namespace);
    let params = PatchParams::apply(FIELD_MANAGER_PROVIDER_VSPHERE).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "vspheremachine_tests.rs"]
mod vspheremachine_tests;

#[cfg(test)]
#[path = "vspheremachine_ensure_tests.rs"]
mod vspheremachine_ensure_tests;

#[cfg(test)]
#[path = "vspheremachine_finalize_tests.rs"]
mod vspheremachine_finalize_tests;
