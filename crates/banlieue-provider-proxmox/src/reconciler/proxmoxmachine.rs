// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `ProxmoxMachine` reconciler: a scheduled VM becomes a real Proxmox VM
//! (ADR-0075).
//!
//! ```text
//! allocate VMID  →  clone template  →  configure  →  grow OS disk  →  seed
//!   →  power  →  observe  →  patch status
//! ```
//!
//! The decisions live in [`allocate_vmid`], [`converge`] and
//! [`finalize_backend`], which take `&dyn ProxmoxApi` and no Kubernetes
//! client, so the whole lifecycle is unit-tested against `FakeProxmox`.
//! [`reconcile`] only wires them to the API server.
//!
//! # Ownership
//!
//! A VM belongs to a machine because **its description carries the machine's
//! UID** on a line of its own ([`ownership_marker`]), written atomically by
//! the clone (ADR-0075 Decision 4). The VM's *name* is the machine's name for
//! humans, but it is not an ownership test: names are unique only per
//! namespace and Proxmox permits duplicates and admin-created VMs, so a name
//! match alone could adopt, configure and later destroy a foreign guest.
//! `status.vmid` is a cache written *before* the clone; the marker is what
//! makes a crash between clone and status patch recoverable, and what stops
//! deletion touching a VM that is not ours. Renaming a VM is harmless;
//! deleting its marker line orphans it.
//!
//! # Deletion
//!
//! The finalizer comes off only after the VM and the seed ISO are gone. Every
//! step treats "already gone" as success and every other failure as an error,
//! so a half-removed machine keeps its finalizer and is retried.

use std::sync::Arc;
use std::time::Duration;

use banlieue_api::banlieue::Provider;
use banlieue_api::common::{
    InitializationStatus, MachineAddress, MachineAddressType, PowerState, condition_types,
};
use banlieue_api::infrastructure::{
    ProxmoxAddressSource, ProxmoxMachine, ProxmoxMachineSpec, ProxmoxMachineStatus,
    proxmox_provider_id,
};
use banlieue_provider_sdk::cloudinit::{CIDATA_LABEL, IsoFile, build_iso9660, seed_files};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_PROXMOX;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use banlieue_proxmox::{CloneParams, ClusterVm, ProxmoxApi, Upid, VmConfig};
use kube::{
    ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::config::{
    OS_DISK_KEY, desired_config, is_ours, os_disk_size_gib, ownership_marker, seed_filename,
    seed_volid,
};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::network::{effective_nics, network_config};

/// Finalizer holding a `ProxmoxMachine` until its VM and seed are gone.
pub const MACHINE_FINALIZER: &str = "banlieue.io/proxmoxmachine";

/// How long a full clone may run. A full clone copies every disk, so this is
/// the one genuinely slow task; on a timeout the next reconcile adopts the VM
/// through its ownership marker and carries on.
const CLONE_TASK_TIMEOUT: Duration = Duration::from_secs(1800);
/// Bound for every other task (start, stop, delete, upload).
const TASK_TIMEOUT: Duration = Duration::from_secs(300);
/// `cluster_vms` `type` of a QEMU guest; containers share the VMID space.
const KIND_QEMU: &str = "qemu";
/// Config key that disables the guest agent when removed.
const GIB_SUFFIX: &str = "G";

/// Who a machine is, for ownership and naming.
#[derive(Debug, Clone, Copy)]
pub struct MachineRef<'a> {
    /// `metadata.name`: the Proxmox VM name, for humans. **Not** the ownership
    /// test: names collide across namespaces and with admin-created VMs.
    pub name: &'a str,
    /// `metadata.uid`: the ownership marker in the VM description (see
    /// [`ownership_marker`]); also names the seed ISO and derives stable MACs.
    pub uid: &'a str,
}

/// Where a machine's VM is, or will be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allocation {
    /// A VM that is ours already exists.
    Existing {
        /// Its VMID.
        vmid: u32,
        /// The node it is on now (a migration may have moved it).
        node: String,
    },
    /// No VM yet: clone onto this VMID. The caller records it in
    /// `status.vmid` **before** cloning.
    Fresh {
        /// The VMID to clone onto.
        vmid: u32,
    },
}

/// What one convergence pass observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The VM's VMID.
    pub vmid: u32,
    /// The node it is on.
    pub node: String,
    /// Its power state after this pass.
    pub power: PowerState,
    /// The VM exists and its config was applied for this spec.
    pub configured: bool,
    /// Addresses, empty while the guest is still coming up.
    pub addresses: Vec<MachineAddress>,
    /// Which source produced `addresses`.
    pub address_source: Option<ProxmoxAddressSource>,
}

/// The VM that belongs to the machine, if any.
///
/// Candidates are the VM holding the `recorded` VMID and every non-template VM
/// named after the machine; each is read and accepted only if its description
/// carries the ownership marker ([`is_ours`]). A candidate without it is a
/// foreign VM and is left completely alone. The node comes from the inventory
/// row, so a migrated VM is found where it is now.
///
/// # Errors
/// [`Error::Proxmox`] if a candidate's config cannot be read (other than
/// "does not exist", which means it vanished since the inventory).
async fn find_ours(
    api: &dyn ProxmoxApi,
    vms: &[ClusterVm],
    m: &MachineRef<'_>,
    recorded: Option<u32>,
) -> Result<Option<ClusterVm>> {
    let mut candidates: Vec<&ClusterVm> = Vec::new();
    candidates.extend(recorded.and_then(|id| vms.iter().find(|v| v.vmid == id)));
    candidates.extend(
        vms.iter()
            .filter(|v| v.name.as_deref() == Some(m.name) && recorded != Some(v.vmid)),
    );
    for vm in candidates.into_iter().filter(|v| !v.template) {
        let Some(config) = ignore_not_found(api.vm_config(&vm.node, vm.vmid).await)? else {
            continue;
        };
        if is_ours(&config, m.uid) {
            return Ok(Some(vm.clone()));
        }
        debug!(
            vmid = vm.vmid,
            "VM has no ownership marker for this machine; leaving it alone"
        );
    }
    Ok(None)
}

/// Decide which VMID this machine uses (ADR-0075 Decision 4).
///
/// 1. A VM that is ours ([`find_ours`]: the recorded VMID or a same-named VM,
///    carrying our marker): adopt it. This is also the crash-after-clone case.
/// 2. `recorded` with no VM at that id yet: the previous pass died before
///    cloning; reuse the id.
/// 3. Otherwise the cluster's next free id. That includes a recorded VMID or a
///    same-named VM that is somebody else's: duplicate names are legal, so we
///    simply clone alongside it.
///
/// # Errors
/// [`Error::Proxmox`] if the inventory, a candidate's config or `nextid`
/// cannot be read.
pub async fn allocate_vmid(
    api: &dyn ProxmoxApi,
    m: &MachineRef<'_>,
    recorded: Option<u32>,
) -> Result<Allocation> {
    let vms = api.cluster_vms().await?;
    if let Some(vm) = find_ours(api, &vms, m, recorded).await? {
        return Ok(Allocation::Existing {
            vmid: vm.vmid,
            node: vm.node,
        });
    }
    if let Some(id) = recorded
        && !vms.iter().any(|v| v.vmid == id)
    {
        return Ok(Allocation::Fresh { vmid: id });
    }
    Ok(Allocation::Fresh {
        vmid: api.next_id().await?.0,
    })
}

/// Whether this pass must (re)apply configuration: until a pass has
/// completed for the current generation. Config is otherwise left alone,
/// because Proxmox normalises what it stores (it fills in MACs, reorders
/// options), so comparing spec against config would never settle.
#[must_use]
pub fn needs_configure(prior: Option<&ProxmoxMachineStatus>, generation: i64) -> bool {
    prior.and_then(|s| s.observed_generation) != Some(generation)
}

fn invalid(what: &'static str, detail: impl Into<String>) -> Error {
    Error::Invalid {
        what,
        detail: detail.into(),
    }
}

/// Wait for `upid`, bounded.
async fn wait(api: &dyn ProxmoxApi, upid: &Upid, timeout: Duration) -> Result<()> {
    api.wait_task(upid, timeout).await?;
    Ok(())
}

/// Bring the cluster in line with `spec` and report what was observed.
///
/// `needs_configure` re-applies configuration on an existing VM; a freshly
/// cloned VM is always configured.
///
/// # Errors
/// [`Error::Invalid`] for a spec or template that cannot work;
/// [`Error::Proxmox`] for a refused call or a task that failed or timed out.
pub async fn converge(
    api: &dyn ProxmoxApi,
    m: &MachineRef<'_>,
    spec: &ProxmoxMachineSpec,
    alloc: &Allocation,
    needs_configure: bool,
) -> Result<Observed> {
    spec.validate()
        .map_err(|d| invalid("ProxmoxMachine.spec", d))?;
    let nics = effective_nics(&spec.nics, m.uid);
    let seed = needs_seed(spec, &nics);
    if seed && spec.iso_storage.is_none() {
        return Err(invalid(
            "ProxmoxMachine.spec.isoStorage",
            "must be set to deliver cloud-init (userData or static addressing)",
        ));
    }

    let (vmid, node, fresh) = match alloc {
        Allocation::Existing { vmid, node } => (*vmid, node.clone(), false),
        Allocation::Fresh { vmid } => {
            clone_from_template(api, m, spec, *vmid).await?;
            (*vmid, spec.node.clone(), true)
        }
    };

    if fresh || needs_configure {
        configure(api, &node, vmid, m, spec, &nics, seed).await?;
    }
    let power = ensure_power(api, &node, vmid, &spec.desired_power_state).await?;
    let (addresses, address_source) = observe_addresses(api, &node, vmid, spec, &power).await;
    Ok(Observed {
        vmid,
        node,
        power,
        configured: true,
        addresses,
        address_source,
    })
}

/// Whether a seed ISO is needed: user-data, or static addressing (which
/// travels in the seed's `network-config`, ADR-0075 Decision 5).
fn needs_seed(
    spec: &ProxmoxMachineSpec,
    nics: &[banlieue_api::infrastructure::ProxmoxNicSpec],
) -> bool {
    spec.needs_seed() || network_config(nics).is_some()
}

async fn clone_from_template(
    api: &dyn ProxmoxApi,
    m: &MachineRef<'_>,
    spec: &ProxmoxMachineSpec,
    vmid: u32,
) -> Result<()> {
    let vms = api.cluster_vms().await?;
    let template = vms
        .iter()
        .find(|v| v.vmid == spec.template_vmid && (v.kind.is_empty() || v.kind == KIND_QEMU))
        .ok_or_else(|| {
            invalid(
                "spec.templateVmid",
                format!("template VMID {} does not exist", spec.template_vmid),
            )
        })?;
    if !template.template {
        return Err(invalid(
            "spec.templateVmid",
            format!(
                "VMID {} is not a template; refusing to clone a live guest",
                spec.template_vmid
            ),
        ));
    }
    // Full clones only: a linked clone would tie this guest's disk to the
    // template's lifetime.
    let mut params = CloneParams::new(vmid)
        .name(m.name)
        // Ownership, set atomically with the clone (see the module docs).
        .description(&ownership_marker(m.uid))
        .storage(&spec.storage)
        .full(true);
    if spec.node != template.node {
        params = params.target(&spec.node);
    }
    if let Some(pool) = &spec.pool {
        params = params.pool(pool);
    }
    info!(vmid, template = spec.template_vmid, node = %spec.node, "cloning template");
    let upid = api
        .clone_vm(&template.node, spec.template_vmid, &params)
        .await?;
    wait(api, &upid, CLONE_TASK_TIMEOUT).await
}

async fn configure(
    api: &dyn ProxmoxApi,
    node: &str,
    vmid: u32,
    m: &MachineRef<'_>,
    spec: &ProxmoxMachineSpec,
    nics: &[banlieue_api::infrastructure::ProxmoxNicSpec],
    seed: bool,
) -> Result<()> {
    let existing = api.vm_config(node, vmid).await?;
    let volid = if seed {
        Some(upload_seed(api, node, m, spec, nics).await?)
    } else {
        None
    };
    let params = desired_config(spec, nics, &existing, volid.as_deref());
    api.set_vm_config(node, vmid, &params).await?;
    grow_os_disk(api, node, vmid, spec.os_disk_size_gi_b).await
}

/// Build the NoCloud seed and upload it to `isoStorage`. Uploading the same
/// name replaces the earlier ISO, so a changed `userData` is picked up on the
/// next configure pass without leaving a second volume behind.
async fn upload_seed(
    api: &dyn ProxmoxApi,
    node: &str,
    m: &MachineRef<'_>,
    spec: &ProxmoxMachineSpec,
    nics: &[banlieue_api::infrastructure::ProxmoxNicSpec],
) -> Result<String> {
    let storage = spec
        .iso_storage
        .as_deref()
        .ok_or_else(|| invalid("ProxmoxMachine.spec.isoStorage", "unset"))?;
    let mut files = seed_files(m.name, m.uid, spec.user_data.as_deref());
    if let Some(doc) = network_config(nics) {
        files.push(IsoFile::new("network-config", doc.into_bytes()));
    }
    let iso =
        build_iso9660(CIDATA_LABEL, &files).map_err(|e| invalid("seed ISO", e.to_string()))?;
    let upid = api
        .upload_iso(node, storage, &seed_filename(m.uid), iso)
        .await?;
    wait(api, &upid, TASK_TIMEOUT).await?;
    Ok(seed_volid(storage, m.uid))
}

/// Grow the OS disk to `desired_gib`; never shrink, and do nothing when it is
/// already at least that large. The key is [`OS_DISK_KEY`] (see its note).
async fn grow_os_disk(api: &dyn ProxmoxApi, node: &str, vmid: u32, desired_gib: u32) -> Result<()> {
    let config: VmConfig = api.vm_config(node, vmid).await?;
    let current = os_disk_size_gib(&config).ok_or_else(|| {
        invalid("OS disk", format!("the cloned VM has no sized {OS_DISK_KEY}; is the template's boot disk {OS_DISK_KEY}?"))
    })?;
    if current >= u64::from(desired_gib) {
        return Ok(());
    }
    info!(vmid, from = current, to = desired_gib, "growing OS disk");
    let task = api
        .resize_disk(
            node,
            vmid,
            OS_DISK_KEY,
            &format!("{desired_gib}{GIB_SUFFIX}"),
        )
        .await?;
    // A task on PVE 9 (found live): starting the guest before it finishes
    // would race the resize. Older releases resize synchronously.
    if let Some(upid) = task {
        wait(api, &upid, TASK_TIMEOUT).await?;
    }
    Ok(())
}

async fn ensure_power(
    api: &dyn ProxmoxApi,
    node: &str,
    vmid: u32,
    desired: &PowerState,
) -> Result<PowerState> {
    let running = api.vm_status(node, vmid).await?.is_running();
    match (desired, running) {
        (PowerState::PoweredOn, false) => {
            info!(vmid, "starting VM");
            let upid = api.start_vm(node, vmid).await?;
            wait(api, &upid, TASK_TIMEOUT).await?;
            Ok(PowerState::PoweredOn)
        }
        (PowerState::PoweredOff, true) => {
            info!(vmid, "stopping VM");
            let upid = api.stop_vm(node, vmid).await?;
            wait(api, &upid, TASK_TIMEOUT).await?;
            Ok(PowerState::PoweredOff)
        }
        (_, true) => Ok(PowerState::PoweredOn),
        (_, false) => Ok(PowerState::PoweredOff),
    }
}

/// Whether an address is one a consumer could connect to.
fn routable(addr: &str) -> bool {
    use std::net::IpAddr;
    let Ok(ip) = addr.parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(v4) => !(v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()),
        IpAddr::V6(v6) => {
            let link_local = v6.segments()[0] & LINK_LOCAL_V6_MASK == LINK_LOCAL_V6_PREFIX;
            !(v6.is_loopback() || v6.is_unspecified() || link_local)
        }
    }
}
/// First hextet mask and value of `fe80::/10`.
const LINK_LOCAL_V6_MASK: u16 = 0xffc0;
const LINK_LOCAL_V6_PREFIX: u16 = 0xfe80;

fn internal_ip(address: String) -> MachineAddress {
    MachineAddress {
        address_type: MachineAddressType::InternalIP,
        address,
    }
}

/// Static addresses are known before boot. Otherwise ask the guest agent of a
/// running VM; an agent that is not up yet is "no addresses yet", never a
/// failure.
async fn observe_addresses(
    api: &dyn ProxmoxApi,
    node: &str,
    vmid: u32,
    spec: &ProxmoxMachineSpec,
    power: &PowerState,
) -> (Vec<MachineAddress>, Option<ProxmoxAddressSource>) {
    let statics: Vec<MachineAddress> = spec
        .nics
        .iter()
        .filter_map(|n| n.ipam.static_.as_ref())
        .map(|s| internal_ip(s.address.clone()))
        .collect();
    if !statics.is_empty() {
        return (statics, Some(ProxmoxAddressSource::Static));
    }
    if *power != PowerState::PoweredOn {
        return (Vec::new(), None);
    }
    let interfaces = match api.agent_interfaces(node, vmid).await {
        Ok(i) => i,
        Err(e) => {
            debug!(vmid, error = %e, "guest agent not answering yet");
            return (Vec::new(), None);
        }
    };
    let addresses: Vec<MachineAddress> = interfaces
        .into_iter()
        .flat_map(|i| i.ip_addresses)
        .filter(|a| routable(&a.ip_address))
        .map(|a| internal_ip(a.ip_address))
        .collect();
    let source = (!addresses.is_empty()).then_some(ProxmoxAddressSource::GuestAgent);
    (addresses, source)
}

/// Success for "already gone".
fn ignore_not_found<T>(r: banlieue_proxmox::Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.is_not_found() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Remove the VM and the seed ISO banlieue created for this machine.
///
/// The VM is found as in [`allocate_vmid`] (recorded VMID or same name) and
/// touched only if it carries our ownership marker; a VM without it is left
/// alone and our backend is treated as already gone. Order: stop → delete VM →
/// delete seed. Every step is idempotent, so a machine that was never
/// realised finalizes cleanly.
///
/// # Errors
/// [`Error::Proxmox`] for any failure other than "already gone", including a
/// task that failed: the finalizer must stay until the VM is really gone.
pub async fn finalize_backend(
    api: &dyn ProxmoxApi,
    m: &MachineRef<'_>,
    spec: &ProxmoxMachineSpec,
    recorded: Option<u32>,
) -> Result<()> {
    let vms = api.cluster_vms().await?;
    let target = find_ours(api, &vms, m, recorded).await?;

    let mut node = spec.node.clone();
    if let Some(vm) = &target {
        node.clone_from(&vm.node);
        let running =
            ignore_not_found(api.vm_status(&node, vm.vmid).await)?.is_some_and(|s| s.is_running());
        if running {
            info!(vmid = vm.vmid, "stopping VM before delete");
            if let Some(upid) = ignore_not_found(api.stop_vm(&node, vm.vmid).await)? {
                wait(api, &upid, TASK_TIMEOUT).await?;
            }
        }
        info!(vmid = vm.vmid, "deleting VM");
        if let Some(upid) = ignore_not_found(api.delete_vm(&node, vm.vmid).await)? {
            wait(api, &upid, TASK_TIMEOUT).await?;
        }
    }

    let Some(storage) = spec.iso_storage.as_deref() else {
        return Ok(());
    };
    let volid = seed_volid(storage, m.uid);
    let volumes = ignore_not_found(api.storage_content(&node, storage).await)?.unwrap_or_default();
    if volumes.iter().any(|v| v.volid == volid) {
        info!(%volid, "deleting seed ISO");
        if let Some(upid) = ignore_not_found(api.delete_volume(&node, storage, &volid).await)? {
            wait(api, &upid, TASK_TIMEOUT).await?;
        }
    }
    Ok(())
}

/// Fold an observation into the status.
///
/// `provisioned` is true only when the VM exists, is configured **and** has
/// reached the desired power state (non-negotiable 6: status mirrors what the
/// infrastructure actually is). Never advances `observedGeneration` on a
/// failed pass — see [`failure_status`].
#[must_use]
pub fn build_status(
    prior: Option<&ProxmoxMachineStatus>,
    spec: &ProxmoxMachineSpec,
    observed: &Observed,
    generation: i64,
) -> ProxmoxMachineStatus {
    let mut status = prior.cloned().unwrap_or_default();
    let at_desired = match spec.desired_power_state {
        PowerState::PoweredOn => observed.power == PowerState::PoweredOn,
        PowerState::PoweredOff => observed.power == PowerState::PoweredOff,
        PowerState::Suspended => false,
    };
    let provisioned = observed.configured && at_desired;
    status.initialization = InitializationStatus {
        provisioned: Some(provisioned),
    };
    status.vmid = Some(observed.vmid);
    status.node = Some(observed.node.clone());
    status.observed_power_state = Some(observed.power.clone());
    status.addresses.clone_from(&observed.addresses);
    status.address_source.clone_from(&observed.address_source);
    status.failure_domain.clone_from(&spec.failure_domain);
    status.tpm_attached = spec.tpm_enabled.then_some(true);
    status.observed_generation = Some(generation);

    let (cond, reason, message) = if provisioned {
        (
            condition_status::TRUE,
            "VMReady",
            "VM exists, is configured and at its desired power state",
        )
    } else {
        (
            condition_status::FALSE,
            "AwaitingPowerState",
            "VM is configured but not yet at its desired power state",
        )
    };
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        cond,
        reason,
        message.to_string(),
        generation,
    );
    status
}

/// Status after a failed pass: keeps everything known, reports `Ready=False`
/// with the error, and deliberately **does not** advance
/// `observedGeneration`, so the next pass reconfigures ([`needs_configure`]).
#[must_use]
pub fn failure_status(
    prior: Option<&ProxmoxMachineStatus>,
    detail: &str,
    generation: i64,
) -> ProxmoxMachineStatus {
    let mut status = prior.cloned().unwrap_or_default();
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        condition_status::FALSE,
        "ReconcileFailed",
        detail.to_string(),
        generation,
    );
    status
}

/// Come back quickly only while waiting for addresses of a VM that should be
/// running.
#[must_use]
pub fn poll_soon(spec: &ProxmoxMachineSpec, observed: &Observed) -> bool {
    spec.desired_power_state == PowerState::PoweredOn && observed.addresses.is_empty()
}

/// Reconcile one `ProxmoxMachine`.
///
/// # Errors
/// [`Error`] on Kubernetes API failure, credential resolution failure, or a
/// backend failure that is not simply "already in the desired state".
pub async fn reconcile(machine: Arc<ProxmoxMachine>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = machine
        .namespace()
        .ok_or(Error::Missing("ProxmoxMachine.metadata.namespace"))?;
    let name = machine.name_any();
    let uid = machine
        .uid()
        .ok_or(Error::Missing("ProxmoxMachine.metadata.uid"))?;
    let generation = machine.metadata.generation.unwrap_or(0);
    let span =
        tracing::info_span!("reconcile", kind = "ProxmoxMachine", %namespace, %name, generation);
    let _enter = span.enter();

    let api: Api<ProxmoxMachine> = Api::namespaced(ctx.client.clone(), &namespace);
    let providers: Api<Provider> = Api::namespaced(ctx.client.clone(), &namespace);
    let provider = providers.get(&machine.spec.provider_ref.name).await?;
    let creds = crate::credentials::resolve(&ctx.client, &namespace, &provider).await?;
    let proxmox = ctx.proxmox.build(&provider.spec.connection, &creds).await?;
    let m = MachineRef {
        name: &name,
        uid: &uid,
    };
    let recorded = machine.status.as_ref().and_then(|s| s.vmid);

    if machine.metadata.deletion_timestamp.is_some() {
        info!("finalizing ProxmoxMachine");
        finalize_backend(proxmox.as_ref(), &m, &machine.spec, recorded).await?;
        remove_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER).await?;
        return Ok(requeue_default());
    }
    ensure_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER).await?;

    let outcome = async {
        let alloc = allocate_vmid(proxmox.as_ref(), &m, recorded).await?;
        // Record a fresh VMID BEFORE cloning: a crash after the clone is then
        // recovered through the ownership marker, and a crash before it reuses the id.
        if let Allocation::Fresh { vmid } = &alloc
            && recorded != Some(*vmid)
        {
            let mut st = machine.status.clone().unwrap_or_default();
            st.vmid = Some(*vmid);
            st.node = Some(machine.spec.node.clone());
            patch_status(&api, &name, &st).await?;
        }
        let configure = needs_configure(machine.status.as_ref(), generation);
        converge(proxmox.as_ref(), &m, &machine.spec, &alloc, configure).await
    }
    .await;

    match outcome {
        Ok(observed) => {
            let expected = proxmox_provider_id(&machine.spec.provider_ref.name, observed.vmid);
            if machine.spec.provider_id.as_deref() != Some(expected.as_str()) {
                patch_provider_id(&api, &name, &expected).await?;
            }
            let status = build_status(
                machine.status.as_ref(),
                &machine.spec,
                &observed,
                generation,
            );
            patch_status(&api, &name, &status).await?;
            Ok(if poll_soon(&machine.spec, &observed) {
                requeue_default()
            } else {
                requeue_long()
            })
        }
        Err(e) => {
            warn!(error = %e, "proxmox machine convergence failed");
            let status = failure_status(machine.status.as_ref(), &e.to_string(), generation);
            patch_status(&api, &name, &status).await?;
            Ok(requeue_on_error())
        }
    }
}

/// Requeue after a reconcile error.
pub fn error_policy(_m: Arc<ProxmoxMachine>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "proxmox machine reconcile error policy fired");
    requeue_on_error()
}

/// Server-side-apply the machine's status.
async fn patch_status(
    api: &Api<ProxmoxMachine>,
    name: &str,
    status: &ProxmoxMachineStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "ProxmoxMachine",
        "status": status,
    });
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER_PROVIDER_PROXMOX).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

/// CAPI puts `providerID` on spec and the provider sets it, as its own field
/// under the provider's field manager so it never contends with the
/// controller's ownership of the rest of spec.
async fn patch_provider_id(api: &Api<ProxmoxMachine>, name: &str, id: &str) -> Result<()> {
    let patch = json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "ProxmoxMachine",
        "spec": { "providerID": id },
    });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER_PROVIDER_PROXMOX),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "proxmoxmachine_tests.rs"]
mod proxmoxmachine_tests;
