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
//! recreated, and a domain that exists is redefined **under its existing
//! UUID**. That last part is not free — `DOMAIN_DEFINE_XML` is not an
//! unconditional upsert, and a define that does not name the UUID already on
//! the host is refused outright.
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

use crate::cloudinit::build_seed_iso;
use banlieue_api::banlieue::Provider;
use banlieue_api::common::{
    InitializationStatus, MachineAddress, MachineAddressType, PowerState, condition_types,
};
use banlieue_api::infrastructure::{
    LibvirtAddressSource, LibvirtMachine, LibvirtMachineSpec, LibvirtMachineStatus,
};
use banlieue_libvirt::{
    Domain, DomainInterface, DomainState, InterfaceAddressSource, StoragePool, StorageVol,
    qcow2_overlay_volume_xml, qcow2_volume_xml, raw_volume_xml,
};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_LIBVIRT;
use banlieue_provider_sdk::status::{condition_status, set_condition};

use crate::guest::{EK_PATH, EkProbe, GuestProbe, MARKER_PATH, expected_ek_cn};
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
use crate::xml::{DomainXmlInput, build_domain_xml, ejected_install_cdrom_xml};

/// Finalizer holding a `LibvirtMachine` until its domain and volumes are gone.
pub const MACHINE_FINALIZER: &str = "banlieue.io/libvirtmachine";

/// Bytes per GiB, for converting `sizeGiB` to the byte capacity libvirt wants.
const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;

/// Format of the backing image banlieue's own pipeline uploads.
///
/// `banlieue-imagebuilder` produces a raw disk and the import Job uploads it
/// verbatim (ADR-0011), so an overlay over one of our own images always sits
/// on `raw`. Also the fallback for an admin-supplied `BackingFile` whose name
/// carries no recognised extension.
const BANLIEUE_BACKING_FORMAT: &str = "raw";

/// Extensions that mean qcow2.
const QCOW2_EXTENSIONS: [&str; 2] = ["qcow2", "qcow"];

/// The format to declare for the volume an overlay sits on.
///
/// Taken from the volume's **name**, never from its contents. libvirt refuses
/// to probe a backing file's format for a good reason: a raw image whose
/// first bytes happen to look like a qcow2 header would be reinterpreted as
/// one, and the "backing file" it then names can be any path the daemon can
/// read. banlieue must not reintroduce that by probing either.
///
/// The name is the weaker signal but it is the one the admin controls
/// deliberately when they place the file, and it is what every other tool in
/// this ecosystem uses. banlieue's own imports are always `.raw`, so the
/// fallback is correct for them by construction.
#[must_use]
pub fn backing_format(volume_name: &str) -> &'static str {
    match volume_name.rsplit('.').next() {
        Some(ext) if QCOW2_EXTENSIONS.contains(&ext) => "qcow2",
        _ => BANLIEUE_BACKING_FORMAT,
    }
}

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

    // What the last pass recorded: converge needs it both to decide whether
    // to eject and to keep the installer out of the XML it redefines.
    let already_detached = machine
        .status
        .as_ref()
        .and_then(|s| s.install_media_detached)
        .unwrap_or(false);

    match converge(client.as_mut(), &machine.spec, already_detached).await {
        Ok(observed) => {
            let status = build_status(&machine, &observed, generation);
            patch_status(&api, &name, &status).await?;
            Ok(
                if should_poll_soon(observed.guest, !observed.addresses.is_empty()) {
                    requeue_default()
                } else {
                    requeue_long()
                },
            )
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
    /// What the guest probe found this pass (ADR-0043).
    ///
    /// The raw observation, not the stored one: stickiness is applied in
    /// `build_status`, so this stays a fact about right now.
    pub guest: GuestProbe,
    /// Install-media state after this pass (ADR-0044).
    ///
    /// `None` when the machine never had install media — an `Immediate`
    /// boot source — which must stay distinct from `Some(false)`, "attached
    /// and not yet ejected", because only the latter withholds `GuestReady`.
    pub install_media_detached: Option<bool>,
    /// What the vTPM EK certificate probe found this pass (ADR-0045).
    ///
    /// Always `NotPublished` for a machine with `tpm_enabled: false` — there
    /// is no vTPM, so nothing is ever asked of the guest.
    pub ek: EkProbe,
}

/// Should this pass eject the install medium? (ADR-0044)
///
/// Pure, so the ordering rule is testable without a host. All three inputs
/// are load-bearing:
///
/// - `needs_cdrom` — an `Immediate` machine has no cdrom, and asking libvirt
///   to update a device that does not exist is an error, not a no-op.
/// - `already_detached` — ejecting twice is an error for the same reason.
/// - `guest_installed` — the ADR-0043 marker is the whole point: it is the
///   only signal that distinguishes the *installed* system from the live
///   installer, and ejecting before it fires pulls the ISO out from under a
///   running install.
#[must_use]
pub fn should_eject_install_media(
    needs_cdrom: bool,
    already_detached: bool,
    guest_installed: bool,
) -> bool {
    needs_cdrom && !already_detached && guest_installed
}

/// Bring the host in line with `spec`, and report what was observed.
///
/// Public so `tests/live_machine.rs` can drive it against a real libvirtd
/// without a Kubernetes API server. Nothing else outside this module calls
/// it — `reconcile` is the entry point.
///
/// # Errors
/// [`Error`] if the pool or boot volume is missing, the domain XML is
/// rejected, or any libvirt call fails.
pub async fn converge(
    client: &mut dyn LibvirtMachineClient,
    spec: &LibvirtMachineSpec,
    already_detached: bool,
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

    // Suppressed once the medium has been ejected (ADR-0044). converge
    // redefines the domain on every pass, so leaving it in here would put
    // the installer straight back and undo the eject — silently, and while
    // status still claimed `installMediaDetached: true`.
    let install_iso =
        (spec.boot_source.needs_install_cdrom() && !already_detached).then(|| source.key.clone());
    let extra_paths: Vec<String> = extra_disks.iter().map(|v| v.key.clone()).collect();

    // Look first, and carry the existing UUID into the document.
    // `DOMAIN_DEFINE_XML` is not an unconditional upsert: libvirt matches a
    // domain by UUID, so redefining without naming the one already on the
    // host is refused with "already exists with uuid …". Found live — the
    // first converge succeeded and every one after it failed.
    let existing = client.lookup_domain(&spec.domain_name).await?;
    let existing_uuid = existing.as_ref().map(|d| format_uuid(&d.uuid));

    let xml = build_domain_xml(&DomainXmlInput {
        spec,
        uuid: existing_uuid.as_deref(),
        domain_type: DOMAIN_TYPE_KVM,
        os_disk_path: &os_disk.key,
        extra_disk_paths: &extra_paths,
        install_iso_path: install_iso.as_deref(),
        install_media_detached: already_detached,
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

    // With the UUID in hand this converges rather than branching: a spec
    // change reaches the existing domain on the next pass.
    let mut domain = client.define_domain(&xml).await?;
    debug!(domain = %domain.name, "defined domain");

    // The cloud-init seed, and the second define that attaches it.
    //
    // Two passes are needed because `instance-id` comes from the domain's
    // UUID (ADR-0054 Decision 5) and libvirt assigns that at the first
    // define. Defining again with the CD-ROM attached is cheap and
    // idempotent — by now the UUID is known, so it is a redefine in place.
    let seed = ensure_seed(client, &pool, spec, &domain).await?;
    if let Some(seed_vol) = &seed {
        let xml = build_domain_xml(&DomainXmlInput {
            spec,
            uuid: Some(&format_uuid(&domain.uuid)),
            domain_type: DOMAIN_TYPE_KVM,
            os_disk_path: &os_disk.key,
            extra_disk_paths: &extra_paths,
            install_iso_path: install_iso.as_deref(),
            install_media_detached: already_detached,
            cidata_iso_path: Some(&seed_vol.key),
            efi_loader_path: None,
            efi_nvram_template_path: None,
        })
        .map_err(|e| Error::Invalid {
            what: "domain XML",
            detail: e.to_string(),
        })?;
        domain = client.define_domain(&xml).await?;
        debug!(domain = %domain.name, "attached cloud-init seed");
    }

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

    // Only ask a running domain: the agent cannot answer otherwise, and a
    // pointless round trip per reconcile against every stopped VM adds up.
    let guest = if state.is_running() {
        client.probe_guest(&domain).await
    } else {
        GuestProbe::AgentUnreachable
    };

    // Eject the installer once — and only once — the INSTALLED guest has
    // announced itself (ADR-0044). Ordered before `build_status` publishes
    // `GuestReady`, which is what makes "a bound pool member never has
    // install media attached" structural rather than a race the timing
    // happens to win.
    let install_media_detached = if spec.boot_source.needs_install_cdrom() {
        let mut detached = already_detached;
        if should_eject_install_media(true, already_detached, guest.is_installed()) {
            info!(domain = %domain.name, "ejecting install media");
            client
                .eject_install_media(&domain, &ejected_install_cdrom_xml())
                .await?;
            detached = true;
        }
        Some(detached)
    } else {
        // Never had any. Distinct from Some(false) on purpose: only the
        // latter withholds GuestReady.
        None
    };

    // Read the vTPM EK certificate (ADR-0045). Only for a machine that has
    // a vTPM, and only from a running domain: there is no certificate
    // otherwise, and asking costs a round trip per reconcile against every
    // VM that can never answer.
    let ek = if spec.tpm_enabled && state.is_running() {
        client
            .read_ek_certificate(&domain, &spec.domain_name, &format_uuid(&domain.uuid))
            .await
    } else {
        EkProbe::NotPublished
    };

    Ok(Observed {
        domain,
        state,
        addresses,
        address_source,
        guest,
        install_media_detached,
        ek,
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
                // The format comes from the backing volume's own name, never
                // from the constant: libvirt does not probe a backing file,
                // so declaring raw over a qcow2 image is accepted and the
                // guest then reads the qcow2 header as its partition table.
                qcow2_overlay_volume_xml(
                    &os_name,
                    capacity,
                    &source.key,
                    backing_format(&source.name),
                )
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

/// Create and upload this machine's cloud-init seed, if it has one.
///
/// Returns `None` when there is nothing to deliver — a machine with no
/// `userData` still gets a seed, because the guest wants its hostname and
/// instance-id, but a spec that asked for neither gets no CD-ROM at all.
///
/// Idempotent: an existing seed volume is reused rather than rewritten. The
/// image is deterministic (ADR-0054), so rewriting it every reconcile would
/// churn the host's storage for no change.
async fn ensure_seed(
    client: &mut dyn LibvirtMachineClient,
    pool: &StoragePool,
    spec: &LibvirtMachineSpec,
    domain: &Domain,
) -> Result<Option<StorageVol>> {
    let name = seed_volume_name(&spec.domain_name);
    if let Some(existing) = client.lookup_volume(pool, &name).await? {
        return Ok(Some(existing));
    }

    let image = build_seed_iso(
        &spec.domain_name,
        &format_uuid(&domain.uuid),
        spec.user_data.as_deref(),
    )
    .map_err(|e| Error::Invalid {
        what: "cloud-init seed",
        detail: e.to_string(),
    })?;

    // Raw, not qcow2: this is a filesystem image the guest mounts directly,
    // not a disk banlieue ever grows.
    let xml = raw_volume_xml(&name, u64::try_from(image.len()).unwrap_or(0));
    let vol = client.create_volume(pool, &xml).await?;
    client.upload_volume(&vol, &image).await?;
    client.refresh_pool(pool).await?;
    info!(volume = %name, bytes = image.len(), "uploaded cloud-init seed");
    Ok(Some(vol))
}

/// Volume name for a machine's cloud-init seed.
#[must_use]
pub fn seed_volume_name(domain_name: &str) -> String {
    format!("{domain_name}-cidata.iso")
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
    // anything is listening inside is `GuestReady`'s question (roadmap 17 A2),
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
    status.guest_installed =
        sticky_guest_installed(status.guest_installed, observed.guest.is_installed());
    // Sticky (ADR-0045): the certificate is read from tmpfs, so a guest that
    // stops answering — or reboots before rewriting it — must not retract an
    // anchor a verifier may already be checking a quote against. A mismatch
    // never gets here: it is discarded at the probe.
    if let EkProbe::Published(pem) = &observed.ek
        && !status.tpm_endorsement_certificates.contains(pem)
    {
        status.tpm_endorsement_certificates.push(pem.clone());
    }
    // Sticky for the same reason: an ejected ISO does not come back, and a
    // stopped domain has not become re-armed (ADR-0044).
    status.install_media_detached = sticky_install_media_detached(
        status.install_media_detached,
        observed.install_media_detached,
    );
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

    // GuestReady is additive and independent: `Ready` above does not consult
    // it, because making it do so would regress every Immediate-mode VM
    // whose image was never built to send the marker (ADR-0043 Decision 4).
    //
    // It is published ONLY when this provider can actually evaluate the
    // signal. An unreachable agent means the image cannot send it at all —
    // the same position vSphere is in until its transport lands — and
    // absence is what `pool.rs::readiness_signal_absent` reads to say
    // `ReadinessSignalAbsent`. Publishing a blanket `False` instead makes a
    // pool report `Filling` — "wait a bit" — forever for an image that can
    // never announce, which is the failure ADR-0046 Decision 3 exists to
    // prevent. Observed on a real cluster before this was fixed.
    //
    // ADR-0044 adds one more gate on the TRUE arm: install media must be
    // gone first. Ordering the eject before the condition is what stops a
    // pool binding a member whose installer is still attached — the pool has
    // no other readiness input, so it cannot tell the difference itself.
    //
    // ADR-0045 adds the second gate, for the same structural reason: a
    // `tpmEnabled` member that cannot produce its EK certificate cannot be
    // attested, so binding one would hand a subject a sandbox it can never
    // prove anything about. Machines with no vTPM skip this entirely — they
    // have no certificate to wait for and must not be stranded waiting.
    let installed = status.guest_installed == Some(true);
    let media_pending = status.install_media_detached == Some(false);
    let ek_pending = machine.spec.tpm_enabled && status.tpm_endorsement_certificates.is_empty();
    let ek_mismatch = observed.ek == EkProbe::Mismatch;
    if installed || observed.guest == GuestProbe::NotAnnounced {
        let (guest_status, guest_reason, guest_message) = if installed && media_pending {
            (
                condition_status::FALSE,
                "InstallMediaAttached",
                "the installed guest announced itself, but its install medium \
                 has not been ejected yet (ADR-0044)"
                    .to_string(),
            )
        } else if installed && ek_mismatch {
            (
                condition_status::FALSE,
                "TpmEndorsementMismatch",
                format!(
                    "the guest reported a vTPM endorsement certificate that is not \
                     issued to this domain; expected subject CN {} (ADR-0045)",
                    expected_ek_cn(
                        &machine.spec.domain_name,
                        &format_uuid(&observed.domain.uuid)
                    )
                ),
            )
        } else if installed && ek_pending {
            (
                condition_status::FALSE,
                "TpmEndorsementPending",
                format!(
                    "the installed guest announced itself, but has not exported its \
                     vTPM endorsement certificate to {EK_PATH} yet (ADR-0045)"
                ),
            )
        } else if installed {
            (
                condition_status::TRUE,
                "GuestAnnounced",
                "the installed guest announced itself".to_string(),
            )
        } else {
            (
                condition_status::FALSE,
                "GuestNotAnnounced",
                format!("qemu-guest-agent is answering but reports no {MARKER_PATH} marker"),
            )
        };
        set_condition(
            &mut status.conditions,
            condition_types::GUEST_READY,
            guest_status,
            guest_reason,
            guest_message,
            generation,
        );
    }
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
        let mut owned: Vec<String> = spec
            .disks
            .iter()
            .map(|d| volume_name(&spec.domain_name, &d.name))
            .collect();
        // The seed is this machine's too, and nothing else references it.
        owned.push(seed_volume_name(&spec.domain_name));
        for vol_name in owned {
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

/// Whether to come back at the short interval rather than the long one.
///
/// Fast only while the answer is expected to change soon (ADR-0043
/// Decision 8): a domain still coming up, or one whose agent is answering
/// but which has not announced yet — a Deferred install in progress.
///
/// An **unreachable agent is not a reason to poll fast**, which is the
/// correction Decision 8 needed. An `Immediate` image has no phase stage
/// and usually no guest agent, so "poll until installed" would poll every
/// 30s forever, per VM, for a signal that is never coming.
#[must_use]
pub fn should_poll_soon(guest: GuestProbe, has_addresses: bool) -> bool {
    if !has_addresses {
        return true;
    }
    guest == GuestProbe::NotAnnounced
}

/// Fold a fresh guest observation into the stored one, stickily.
///
/// Once `Some(true)`, it stays: the marker lives in the guest's `/run` and
/// so does not survive a power cycle, but a VM that was stopped has not
/// become uninstalled (ADR-0043 Decision 5). `None` means nothing has
/// looked yet, which is the expected state for the whole of a `Deferred`
/// image's install.
#[must_use]
pub fn sticky_install_media_detached(
    previous: Option<bool>,
    observed: Option<bool>,
) -> Option<bool> {
    match (previous, observed) {
        // Once ejected, always ejected.
        (Some(true), _) => Some(true),
        (_, o) => o,
    }
}

/// Sticky `guestInstalled` (ADR-0043).
pub fn sticky_guest_installed(previous: Option<bool>, observed: bool) -> Option<bool> {
    if previous == Some(true) {
        return Some(true);
    }
    Some(observed)
}

#[cfg(test)]
#[path = "libvirtmachine_tests.rs"]
mod libvirtmachine_tests;
