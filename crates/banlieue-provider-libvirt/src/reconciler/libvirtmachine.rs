// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `LibvirtMachine` reconciler — a scheduled VM becomes a real domain
//! (ADR-0050).
//!
//! The shape is deliberately the same as the vSphere machine reconciler's:
//! converge the backend towards the spec, patch status, requeue. What differs
//! is that libvirt has no clone operation, so "create the disk" and "create
//! the VM" are separate steps here rather than one `CloneVM_Task`.
//!
//! ```text
//! ensure volumes  →  define domain  →  start  →  observe  →  patch status
//! ```
//!
//! Every step is idempotent on its own, because a reconciler is re-entered
//! from the top after any failure: a volume that exists is reused rather than
//! recreated, and `DOMAIN_DEFINE_XML` is an upsert.
//!
//! # Deletion
//!
//! The finalizer is the contract: a `LibvirtMachine` object does not
//! disappear until its domain and every volume banlieue created for it are
//! gone from the host. Teardown order is destroy → undefine → delete volumes,
//! and **no step is allowed to fail silently** — a teardown that reports
//! success while leaving a domain defined is how the next VM of the same name
//! quietly inherits a stale one.

use std::sync::Arc;

use banlieue_api::banlieue::Provider;
use banlieue_api::common::{
    InitializationStatus, MachineAddress, MachineAddressType, PowerState, condition_types,
};
use banlieue_api::infrastructure::{
    LibvirtAddressSource, LibvirtMachine, LibvirtMachineSpec, LibvirtMachineStatus,
};
use banlieue_libvirt::{
    Domain, DomainInterface, DomainState, InterfaceAddressSource, StoragePool, StorageVol,
    qcow2_overlay_volume_xml, qcow2_volume_xml,
};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_LIBVIRT;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use kube::{
    ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::machine_client::LibvirtMachineClient;
use crate::xml::{DomainXmlInput, build_domain_xml};

/// Finalizer holding a `LibvirtMachine` until its domain and volumes are gone.
pub const MACHINE_FINALIZER: &str = "banlieue.io/libvirtmachine";

/// Bytes per GiB, for converting `sizeGiB` to the byte capacity libvirt wants.
const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;

/// Format of the backing image banlieue's own pipeline uploads.
///
/// `banlieue-imagebuilder` produces a raw disk and the import Job uploads it
/// verbatim (ADR-0011), so an overlay over one of our images always sits on
/// `raw`. Declared rather than probed: libvirt does not guess a backing
/// file's format, and an undeclared one is both a security advisory and a
/// hard refusal on current libvirt.
const BANLIEUE_BACKING_FORMAT: &str = "raw";

/// Reconcile one `LibvirtMachine`.
///
/// # Errors
/// [`Error`] on Kubernetes API failure, credential resolution failure, or a
/// libvirt operation that is not simply "already in the desired state".
pub async fn reconcile(machine: Arc<LibvirtMachine>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = machine
        .namespace()
        .ok_or(Error::Missing("LibvirtMachine.metadata.namespace"))?;
    let name = machine.name_any();
    let generation = machine.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!(
        "reconcile",
        kind = "LibvirtMachine",
        namespace = %namespace,
        name = %name,
        generation,
    );
    let _enter = span.enter();

    let api: Api<LibvirtMachine> = Api::namespaced(ctx.client.clone(), &namespace);

    // Connecting is the expensive part of every path below, deletion
    // included, so it happens once here.
    let provider = resolve_provider(&ctx, &namespace, &machine.spec).await?;
    let identity = crate::credentials::resolve(&ctx.client, &namespace, &provider).await?;
    let mut client = ctx
        .libvirt_machine
        .build(&provider.spec.connection, &identity)
        .await?;

    if machine.metadata.deletion_timestamp.is_some() {
        return finalize(&api, client.as_mut(), &machine).await;
    }

    ensure_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER).await?;

    match converge(client.as_mut(), &machine.spec).await {
        Ok(observed) => {
            let status = build_status(&machine, &observed, generation);
            patch_status(&api, &name, &status).await?;
            // A domain with no address yet is still booting, which is the
            // common case for a Deferred install — check back sooner.
            Ok(if observed.addresses.is_empty() {
                requeue_default()
            } else {
                requeue_long()
            })
        }
        Err(e) => {
            warn!(error = %e, "libvirt machine convergence failed");
            let status = failure_status(&machine, &e.to_string(), generation);
            patch_status(&api, &name, &status).await?;
            Ok(requeue_on_error())
        }
    }
}

/// Requeue policy for a failed reconcile.
///
/// Uniform backoff rather than a per-error branch: the failures this
/// reconciler sees are almost all transient (host unreachable, pool not yet
/// refreshed, image import still running), and the ones that are not already
/// surface as a `Ready=False` condition carrying their own message.
pub fn error_policy(_machine: Arc<LibvirtMachine>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "libvirt machine reconcile error policy fired");
    requeue_on_error()
}

/// What one convergence pass observed about the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The domain, once defined.
    pub domain: Domain,
    /// Its run state, as last read.
    pub state: DomainState,
    /// Guest addresses, empty while the guest is still coming up.
    pub addresses: Vec<MachineAddress>,
    /// Which source produced `addresses`, if any did.
    pub address_source: Option<LibvirtAddressSource>,
}

/// Bring the host in line with `spec`, and report what was observed.
async fn converge(
    client: &mut dyn LibvirtMachineClient,
    spec: &LibvirtMachineSpec,
) -> Result<Observed> {
    let pool = client
        .lookup_pool(&spec.pool)
        .await?
        .ok_or_else(|| Error::Invalid {
            what: "spec.pool",
            detail: format!("storage pool {:?} does not exist on this host", spec.pool),
        })?;

    // The source volume: a backing image to overlay, or an installer ISO to
    // attach. Either way it must already be on the host — putting it there is
    // the VMImage reconciler's job, not this one's.
    let source = client
        .lookup_volume(&pool, &spec.boot_source.volume)
        .await?
        .ok_or_else(|| Error::Invalid {
            what: "spec.bootSource.volume",
            detail: format!(
                "volume {:?} is not in pool {:?}; the VMImage for this machine \
                 has not finished importing",
                spec.boot_source.volume, spec.pool
            ),
        })?;

    let (os_disk, extra_disks) = ensure_disks(client, &pool, spec, &source).await?;

    let install_iso = spec
        .boot_source
        .needs_install_cdrom()
        .then(|| source.key.clone());
    let extra_paths: Vec<String> = extra_disks.iter().map(|v| v.key.clone()).collect();

    let xml = build_domain_xml(&DomainXmlInput {
        spec,
        domain_type: DOMAIN_TYPE_KVM,
        os_disk_path: &os_disk.key,
        extra_disk_paths: &extra_paths,
        install_iso_path: install_iso.as_deref(),
        // Rendered only once there is user-data to deliver; see the module
        // note in `xml::domain`.
        cidata_iso_path: None,
        efi_loader_path: None,
        efi_nvram_template_path: None,
    })
    .map_err(|e| Error::Invalid {
        what: "domain XML",
        detail: e.to_string(),
    })?;

    // Define is an upsert, so this converges rather than branching on
    // existence — and a spec change reaches the domain on the next pass.
    let domain = client.define_domain(&xml).await?;
    debug!(domain = %domain.name, "defined domain");

    let mut state = client.domain_state(&domain).await?;
    if spec.desired_power_state == PowerState::PoweredOn && !state.is_running() {
        info!(domain = %domain.name, "starting domain");
        client.start_domain(&domain).await?;
        state = client.domain_state(&domain).await?;
    }
    if spec.desired_power_state == PowerState::PoweredOff && state.is_running() {
        info!(domain = %domain.name, "stopping domain");
        client.destroy_domain(&domain).await?;
        state = client.domain_state(&domain).await?;
    }

    // Addresses only exist once a guest is up, so an empty answer is normal
    // rather than a failure.
    let (addresses, address_source) = match client.domain_addresses(&domain).await? {
        Some((ifaces, source)) => (
            to_machine_addresses(&ifaces),
            Some(to_address_source(source)),
        ),
        None => (Vec::new(), None),
    };

    Ok(Observed {
        domain,
        state,
        addresses,
        address_source,
    })
}

/// `<domain type='...'>`. KVM, because a host without it cannot run the
/// workloads banlieue schedules at any useful speed, and silently falling
/// back to emulation would look like a mysterious performance problem rather
/// than a misconfigured host.
const DOMAIN_TYPE_KVM: &str = "kvm";

/// Create this machine's volumes if they do not exist yet.
///
/// Returns the OS disk and the data disks, in `spec.disks` order.
async fn ensure_disks(
    client: &mut dyn LibvirtMachineClient,
    pool: &StoragePool,
    spec: &LibvirtMachineSpec,
    source: &StorageVol,
) -> Result<(StorageVol, Vec<StorageVol>)> {
    let mut disks = spec.disks.iter();
    let os = disks.next().ok_or(Error::Invalid {
        what: "spec.disks",
        detail: "a machine needs at least an OS disk".to_string(),
    })?;

    let os_name = volume_name(&spec.domain_name, &os.name);
    let os_disk = match client.lookup_volume(pool, &os_name).await? {
        // Already there: reuse. Recreating would discard a running VM's disk.
        Some(v) => v,
        None => {
            let capacity = u64::from(os.size_gi_b) * BYTES_PER_GIB;
            let xml = if spec.boot_source.needs_empty_os_disk() {
                // Deferred: the guest installs itself onto an empty disk.
                qcow2_volume_xml(&os_name, capacity)
            } else {
                // Immediate: a copy-on-write overlay over the imported image.
                qcow2_overlay_volume_xml(&os_name, capacity, &source.key, BANLIEUE_BACKING_FORMAT)
            }
            .map_err(Error::from)?;
            let v = client.create_volume(pool, &xml).await?;
            // A pool does not always notice a new file on its own, and the
            // lookup on the next reconcile would then fail for no visible
            // reason.
            client.refresh_pool(pool).await?;
            info!(volume = %os_name, "created OS disk");
            v
        }
    };

    let mut extra = Vec::new();
    for disk in disks {
        let name = volume_name(&spec.domain_name, &disk.name);
        let vol = match client.lookup_volume(pool, &name).await? {
            Some(v) => v,
            None => {
                let xml = qcow2_volume_xml(&name, u64::from(disk.size_gi_b) * BYTES_PER_GIB)
                    .map_err(Error::from)?;
                let v = client.create_volume(pool, &xml).await?;
                client.refresh_pool(pool).await?;
                info!(volume = %name, "created data disk");
                v
            }
        };
        extra.push(vol);
    }

    Ok((os_disk, extra))
}

/// Volume name for one of a machine's disks.
///
/// Derived from the *domain* name, which is already namespace-qualified by
/// the controller, so two namespaces' `db-01` disks cannot collide in a pool
/// the way their CR names would.
#[must_use]
pub fn volume_name(domain_name: &str, disk_name: &str) -> String {
    format!("{domain_name}-{disk_name}.qcow2")
}

/// Map libvirt's run state onto banlieue's backend-neutral one.
///
/// `ShuttingDown` maps to `PoweredOn`: the domain is still executing and
/// still holds its resources. Reporting it as off would make a consumer
/// believe it is safe to delete the disk out from under a running guest.
#[must_use]
pub fn to_power_state(state: DomainState) -> PowerState {
    match state {
        DomainState::Running | DomainState::Blocked | DomainState::ShuttingDown => {
            PowerState::PoweredOn
        }
        DomainState::Paused | DomainState::PmSuspended => PowerState::Suspended,
        // An unknown state is reported as off rather than on, matching
        // `DomainState::is_running`'s own conservatism: the consequence of
        // guessing "on" is a consumer waiting forever for a VM that is not
        // coming up.
        DomainState::ShutOff
        | DomainState::Crashed
        | DomainState::NoState
        | DomainState::Unknown(_) => PowerState::PoweredOff,
    }
}

/// Map the libvirt address source onto the CRD's own enum.
#[must_use]
pub fn to_address_source(source: InterfaceAddressSource) -> LibvirtAddressSource {
    match source {
        InterfaceAddressSource::Agent => LibvirtAddressSource::GuestAgent,
        InterfaceAddressSource::Lease => LibvirtAddressSource::DhcpLease,
        InterfaceAddressSource::Arp => LibvirtAddressSource::ArpTable,
    }
}

/// Convert guest interfaces into CAPI-shaped machine addresses.
///
/// Loopback and link-local addresses are dropped. They are not wrong, they
/// are just never what a consumer means by "the VM's address" — and the
/// guest agent reports `lo` on every single guest, so leaving them in would
/// make `127.0.0.1` the first address of every VM banlieue manages.
#[must_use]
pub fn to_machine_addresses(interfaces: &[DomainInterface]) -> Vec<MachineAddress> {
    interfaces
        .iter()
        .flat_map(|i| i.addrs.iter())
        .filter(|a| is_routable(&a.addr))
        .map(|a| MachineAddress {
            address_type: MachineAddressType::InternalIP,
            address: a.addr.clone(),
        })
        .collect()
}

/// Whether an address is one a consumer could actually connect to.
fn is_routable(addr: &str) -> bool {
    let lower = addr.to_ascii_lowercase();
    // IPv4 loopback is a whole /8, not just 127.0.0.1.
    if lower.starts_with("127.") || lower == "::1" {
        return false;
    }
    // Link-local: 169.254.0.0/16 and fe80::/10. The latter's first hextet
    // runs fe80–febf, so a prefix test on `fe8`/`fe9`/`fea`/`feb` is what
    // actually covers it.
    if lower.starts_with("169.254.") {
        return false;
    }
    if let Some(rest) = lower.strip_prefix("fe")
        && rest.starts_with(['8', '9', 'a', 'b'])
    {
        return false;
    }
    true
}

/// Build the status for a successful convergence.
fn build_status(
    machine: &LibvirtMachine,
    observed: &Observed,
    generation: i64,
) -> LibvirtMachineStatus {
    let mut status = machine.status.clone().unwrap_or_default();

    // `provisioned` is the CAPI contract's "the infrastructure exists" flag,
    // not "the guest is up". A defined, running domain satisfies it; whether
    // anything is listening inside is `GuestReady`'s question (roadmap 70 A2),
    // which is deliberately a separate signal.
    let provisioned = observed.state.is_running();
    status.initialization = InitializationStatus {
        provisioned: Some(provisioned),
    };
    status.domain_uuid = Some(format_uuid(&observed.domain.uuid));
    status.observed_power_state = Some(to_power_state(observed.state));
    status.addresses = observed.addresses.clone();
    status.address_source = observed.address_source.clone();
    status.failure_domain = machine.spec.failure_domain.clone();
    status.tpm_attached = machine.spec.tpm_enabled.then_some(true);
    status.observed_generation = Some(generation);

    let (cond_status, reason, message) = if provisioned {
        (
            condition_status::TRUE,
            "DomainRunning",
            "domain is defined and running".to_string(),
        )
    } else {
        (
            condition_status::FALSE,
            "DomainNotRunning",
            format!("domain is defined but {:?}", observed.state),
        )
    };
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        cond_status,
        reason,
        message,
        generation,
    );
    status
}

/// Build the status for a failed convergence, preserving everything already
/// observed. A conditions-only patch from the same field manager would make
/// SSA retract every field this one does not mention.
fn failure_status(machine: &LibvirtMachine, detail: &str, generation: i64) -> LibvirtMachineStatus {
    let mut status = machine.status.clone().unwrap_or_default();
    status.observed_generation = Some(generation);
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

/// Render a raw 16-byte UUID in its canonical 8-4-4-4-12 form.
#[must_use]
pub fn format_uuid(uuid: &[u8; 16]) -> String {
    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Deletion path: clear the backend, then drop the finalizer.
///
/// The finalizer comes off only after [`finalize_backend`] has returned
/// successfully, so a `LibvirtMachine` object cannot disappear while its
/// domain is still on the host.
async fn finalize(
    api: &Api<LibvirtMachine>,
    client: &mut dyn LibvirtMachineClient,
    machine: &LibvirtMachine,
) -> Result<Action> {
    info!("finalizing LibvirtMachine");
    finalize_backend(client, &machine.spec).await?;
    info!("backend cleared; removing finalizer");
    remove_finalizer(api, machine, MACHINE_FINALIZER).await?;
    Ok(requeue_default())
}

/// Remove the domain and every volume banlieue created for this machine.
///
/// Order matters and is not negotiable: a domain must be off before it can be
/// undefined, and undefined before its disks are deleted — deleting a running
/// domain's disk gives the guest I/O errors rather than a clean shutdown.
///
/// Every step is idempotent, so a machine that was never realised finalizes
/// cleanly; that is the common case when a `VirtualMachine` is deleted
/// mid-provision.
///
/// # Errors
/// [`Error`] if any step fails, **including** a teardown that reported
/// success but left the domain defined. Never `|| true` a teardown step: a
/// silently half-removed domain is how the next VM of the same name quietly
/// inherits a stale one.
pub async fn finalize_backend(
    client: &mut dyn LibvirtMachineClient,
    spec: &LibvirtMachineSpec,
) -> Result<()> {
    if let Some(domain) = client.lookup_domain(&spec.domain_name).await? {
        let state = client.domain_state(&domain).await?;
        if state.is_running() {
            info!(domain = %domain.name, "destroying domain");
            client.destroy_domain(&domain).await?;
        }
        info!(domain = %domain.name, "undefining domain");
        // Always with NVRAM and TPM state; see `domain_undefine`.
        client.undefine_domain(&domain).await?;

        // Verify rather than assume.
        if client.lookup_domain(&spec.domain_name).await?.is_some() {
            return Err(Error::Invalid {
                what: "domain teardown",
                detail: format!(
                    "domain {:?} is still defined after undefine",
                    spec.domain_name
                ),
            });
        }
    }

    // Volumes last, and only the ones banlieue named. The boot source is a
    // shared image owned by the VMImage reconciler: deleting it along with one
    // machine would break every other VM using it.
    if let Some(pool) = client.lookup_pool(&spec.pool).await? {
        for disk in &spec.disks {
            let vol_name = volume_name(&spec.domain_name, &disk.name);
            if let Some(vol) = client.lookup_volume(&pool, &vol_name).await? {
                info!(volume = %vol_name, "deleting volume");
                client.delete_volume(&vol).await?;
            }
        }
    }
    Ok(())
}

/// Resolve the `Provider` this machine's `providerRef` names.
async fn resolve_provider(
    ctx: &Context,
    namespace: &str,
    spec: &LibvirtMachineSpec,
) -> Result<Provider> {
    let api: Api<Provider> = Api::namespaced(ctx.client.clone(), namespace);
    Ok(api.get(&spec.provider_ref.name).await?)
}

/// Server-side-apply the machine's status.
async fn patch_status(
    api: &Api<LibvirtMachine>,
    name: &str,
    status: &LibvirtMachineStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "LibvirtMachine",
        "status": status,
    });
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER_PROVIDER_LIBVIRT).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "libvirtmachine_tests.rs"]
mod libvirtmachine_tests;
