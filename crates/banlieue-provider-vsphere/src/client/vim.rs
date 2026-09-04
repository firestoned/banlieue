// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Production `VSphereClient` implementation backed by `vim_rs`.
//!
//! Phase 1B iteration 1 surface: connect (basic-auth + optional insecure TLS),
//! list datacenters, list clusters per datacenter. Iteration 2 grows it with
//! datastores, networks, and the VSphereMachine VM-lifecycle calls.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use banlieue_api::banlieue::{DiskController, InstallMode, NicAdapter, ProviderConnection};
use banlieue_api::common::{DiskProvisioning, Firmware, PowerState};
use tracing::{debug, info};
use vim_rs::core::client::{Client, ClientBuilder};
use vim_rs::mo::cluster_compute_resource::ClusterComputeResource;
use vim_rs::mo::container_view::ContainerView;
use vim_rs::mo::datacenter::Datacenter as VimDatacenter;
use vim_rs::mo::datastore::Datastore as VimDatastore;
use vim_rs::mo::distributed_virtual_portgroup::DistributedVirtualPortgroup;
use vim_rs::mo::distributed_virtual_switch::DistributedVirtualSwitch;
use vim_rs::mo::file_manager::FileManager;
use vim_rs::mo::folder::Folder;
use vim_rs::mo::network::Network as VimNetwork;
use vim_rs::mo::storage_pod::StoragePod as VimStoragePod;
use vim_rs::mo::task::Task;
use vim_rs::mo::view_manager::ViewManager;
use vim_rs::mo::virtual_machine::VirtualMachine as VimVirtualMachine;
use vim_rs::types::boxed_types::ValueElements;
use vim_rs::types::enums::{
    MoTypesEnum, TaskInfoStateEnum, VirtualDeviceConfigSpecFileOperationEnum,
    VirtualDeviceConfigSpecOperationEnum, VirtualMachinePowerStateEnum, VirtualScsiSharingEnum,
};
use vim_rs::types::structs::{
    DistributedVirtualSwitchPortConnection, ManagedObjectReference, OptionValue,
    ParaVirtualScsiController, VirtualBusLogicController, VirtualCdrom, VirtualCdromIsoBackingInfo,
    VirtualController, VirtualDevice, VirtualDeviceConfigSpec, VirtualDeviceConnectInfo,
    VirtualDeviceDeviceBackingInfo, VirtualDeviceFileBackingInfo, VirtualDevicePciBusSlotInfo,
    VirtualDisk, VirtualDiskFlatVer2BackingInfo, VirtualE1000, VirtualE1000E, VirtualEthernetCard,
    VirtualEthernetCardDistributedVirtualPortBackingInfo, VirtualEthernetCardNetworkBackingInfo,
    VirtualIdeController, VirtualLsiLogicController, VirtualLsiLogicSasController,
    VirtualMachineBootOptions, VirtualMachineBootOptionsBootableCdromDevice,
    VirtualMachineBootOptionsBootableDiskDevice, VirtualMachineBootOptionsBootableEthernetDevice,
    VirtualMachineCloneSpec, VirtualMachineConfigSpec, VirtualMachineFileInfo,
    VirtualMachineRelocateSpec, VirtualScsiController, VirtualTpm, VirtualVmxnet, VirtualVmxnet2,
    VirtualVmxnet3,
};
use vim_rs::types::traits::VirtualDeviceBackingInfoTrait;
use vim_rs::types::vim_any::VimAny;

use crate::error::{Error, Result};

use super::{
    Cluster, Credentials, Datacenter, Datastore, Network, Template, VSphereClient,
    VSphereClientFactory,
};

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

const MO_TYPE_DATACENTER: &str = "Datacenter";
const MO_TYPE_CLUSTER: &str = "ClusterComputeResource";
const MO_TYPE_VIRTUAL_MACHINE: &str = "VirtualMachine";

/// Deadline for establishing the TCP+TLS connection to vCenter.
///
/// Security review 2026-07-31 (SEC-012): without it, a firewalled or
/// black-holed endpoint stalls the connect — and the reconcile with it —
/// indefinitely. 10s is generous for a reachable vCenter.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on one complete HTTP request, response body included.
///
/// Deliberately independent of `--vsphere-task-timeout-secs` (default 600s):
/// that flag bounds a whole vCenter *task* (clone, power, reconfigure) once
/// iter 2+ wires task polling, while this bounds a single SOAP call. The
/// calls this client makes today — login, container views, property reads —
/// answer in well under a second on a healthy vCenter, so 120s leaves ample
/// headroom for slow PropertyCollector responses on large inventories while
/// still failing a stalled endpoint (SEC-012). When vim_rs grows task
/// long-polling, that path must carry its own deadline (driven by the task
/// flag), not this per-request one.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Total-request ceiling for a **datastore ISO upload**, which moves a
/// multi-gigabyte file and cannot share the 120s SOAP timeout. Generous but
/// bounded so a genuinely stuck transfer still fails rather than hanging
/// forever; the connect timeout stays at [`HTTP_CONNECT_TIMEOUT`].
const DATASTORE_UPLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

// --- CreateVM_Task template shape (ADR-0020) ---------------------------------
/// Poll interval while waiting for a vCenter task (CreateVM / Destroy).
const TASK_POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Max task-poll attempts before giving up (≈10 min at the interval above).
const TASK_POLL_MAX_ATTEMPTS: u32 = 200;

// --- Install + generalize before MarkAsTemplate (ADR-0021) ------------------
/// Poll interval while waiting for the golden-template build VM to power
/// itself off once the cloud-config's unattended Kairos install completes.
const INSTALL_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Default bound (seconds) on the unattended-install wait when
/// `VMImage.spec.template.installTimeoutSeconds` is unset or non-positive.
/// Mirrors the default documented on `VMImageTemplate::install_timeout_seconds`.
const DEFAULT_INSTALL_TIMEOUT_SECS: u32 = 1800;
/// vCenter firmware tokens (`VirtualMachineConfigSpec.firmware`). vCenter takes
/// only these two; secure boot is layered on `efi` via `boot_options`.
const FIRMWARE_BIOS: &str = "bios";
const FIRMWARE_EFI: &str = "efi";
const DISK_MODE_PERSISTENT: &str = "persistent";
// Negative device keys are the vSphere convention for devices being added in a
// single CreateVM/Reconfigure spec; controllers are referenced by these keys.
const KEY_SCSI: i32 = -1000;
const KEY_DISK: i32 = -1001;
const KEY_IDE: i32 = -1002;
const KEY_CDROM: i32 = -1003;
const KEY_NIC: i32 = -1004;
const KEY_TPM: i32 = -1005;
const KIB_PER_GIB: i64 = 1024 * 1024;

/// Factory that talks to a real vCenter via vim_rs.
#[derive(Default, Clone)]
pub struct VimClientFactory;

impl VimClientFactory {
    pub fn new() -> Self {
        Self
    }
}

/// Install the rustls **ring** crypto provider as the process default.
///
/// reqwest 0.13 is built with `rustls-no-provider` (ADR-0009): it resolves the
/// TLS provider via `CryptoProvider::get_default()` and **panics with
/// `"No provider set"`** on first TLS use if none was installed. We install ring
/// — the same provider kube/hyper-rustls already use — so the whole process
/// shares one crypto stack (no aws-lc-rs, no OpenSSL).
///
/// Idempotent and safe to call from multiple roles: `install_default` returns
/// `Err` if a provider is already set, which we deliberately ignore. Call once
/// at startup, before any TLS (kube client or vCenter) is constructed.
pub fn install_default_crypto_provider() {
    // Ignore the error: a second install (or one already done by another role)
    // is a no-op for our purposes — we only need *a* ring provider to be active.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Parse a PEM bundle into reqwest root certificates.
///
/// Uses `Certificate::from_pem_bundle` (not `from_pem`) so a `caBundle` holding
/// a chain / multiple concatenated CAs contributes every certificate, not just
/// the first. Pure and side-effect-free so it is unit-testable without a vCenter.
///
/// Rejects a bundle that yields **zero** certificates. `from_pem_bundle` returns
/// `Ok([])` for input containing no PEM blocks (e.g. garbage, or an
/// accidentally-empty ConfigMap value); accepting it would silently fall back to
/// the system trust roots while the operator believes they pinned a CA. Failing
/// closed surfaces the misconfiguration on `Provider.status` instead.
///
/// # Errors
/// Returns [`Error::Vsphere`] if the PEM cannot be parsed or contains no
/// certificates.
/// Reduce a `Provider.spec.connection.endpoint` to the bare `host[:port]`
/// vim_rs 0.5 expects as its `server_address`.
///
/// vim_rs builds every request URL as `https://{server_address}/api/...` (and
/// the SOAP path likewise), so `server_address` must be a host or `host:port`,
/// never a full URL. The banlieue CRD documents `endpoint` as a full URL
/// (`https://vcenter/sdk`), so the scheme and any path are stripped here.
/// Without this the connect target came out as
/// `https://https://vcenter/sdk/api/vcenter/system?action=hello` and every
/// request failed with a connect error.
pub(crate) fn server_address(endpoint: &str) -> &str {
    // Strip an optional `scheme://` prefix, then everything from the first
    // `/`, `?`, or `#` — leaving `host` or `host:port` (IPv6 literals keep
    // their brackets, having no interior delimiter).
    let after_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_scheme, rest)| rest);
    after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme)
}

fn root_certs_from_pem(pem: &str) -> Result<Vec<reqwest::Certificate>> {
    let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
        .map_err(|e| Error::Vsphere(format!("caBundle: invalid PEM: {e}")))?;
    if certs.is_empty() {
        return Err(Error::Vsphere(
            "caBundle: no certificates found in PEM (expected at least one BEGIN CERTIFICATE block)"
                .to_string(),
        ));
    }
    Ok(certs)
}

/// Build the reqwest client banlieue injects into vim_rs (BYOC, ADR-0008).
///
/// banlieue owns the transport so it owns TLS trust: a resolved `ca_bundle_pem`
/// is added as root certificate(s); `insecure` disables verification entirely.
/// The two are independent — a CA bundle does not imply skipping verification —
/// but `insecure` is the bigger hammer and is applied regardless.
///
/// Every request carries [`HTTP_CONNECT_TIMEOUT`] / [`HTTP_REQUEST_TIMEOUT`]
/// (SEC-012): a hostile or stalled vCenter must fail the call, not hang the
/// reconcile forever.
///
/// # Errors
/// Returns [`Error::Vsphere`] if the PEM is invalid or the client fails to build.
pub(crate) fn build_http_client(
    ca_bundle_pem: Option<&str>,
    insecure: bool,
) -> Result<reqwest::Client> {
    build_http_client_with_timeouts(
        ca_bundle_pem,
        insecure,
        HTTP_CONNECT_TIMEOUT,
        HTTP_REQUEST_TIMEOUT,
    )
}

/// As [`build_http_client`], but with the [`DATASTORE_UPLOAD_TIMEOUT`] ceiling
/// instead of the SOAP one — for streaming a multi-gigabyte ISO to a datastore.
pub(crate) fn build_upload_http_client(
    ca_bundle_pem: Option<&str>,
    insecure: bool,
) -> Result<reqwest::Client> {
    build_http_client_with_timeouts(
        ca_bundle_pem,
        insecure,
        HTTP_CONNECT_TIMEOUT,
        DATASTORE_UPLOAD_TIMEOUT,
    )
}

/// As [`build_http_client`], with explicit timeouts. Split out so tests can
/// prove the deadline fires without waiting out the production values.
fn build_http_client_with_timeouts(
    ca_bundle_pem: Option<&str>,
    insecure: bool,
    connect_timeout: Duration,
    request_timeout: Duration,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(format!("{APP_NAME}/{APP_VERSION}"))
        .connect_timeout(connect_timeout)
        .timeout(request_timeout);
    if let Some(pem) = ca_bundle_pem {
        for cert in root_certs_from_pem(pem)? {
            builder = builder.add_root_certificate(cert);
        }
    }
    if insecure {
        builder = builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
    }
    builder
        .build()
        .map_err(|e| Error::Vsphere(format!("build http client: {e}")))
}

#[async_trait]
impl VSphereClientFactory for VimClientFactory {
    async fn build(
        &self,
        connection: &ProviderConnection,
        creds: &Credentials,
        ca_bundle_pem: Option<&str>,
    ) -> Result<Box<dyn VSphereClient>> {
        debug!(
            endpoint = %connection.endpoint,
            ca_bundle = ca_bundle_pem.is_some(),
            insecure = connection.insecure_skip_tls_verify,
            "vim_rs ClientBuilder::new (BYOC)"
        );
        // BYOC (ADR-0008/0009): banlieue builds the reqwest client and hands it to
        // vim_rs, so it owns CA trust / insecure policy end to end. In vim_rs 0.5
        // with `default-client` off, the client is a required 2nd argument to
        // `ClientBuilder::new` (there is no `.http_client()` setter); vim_rs never
        // constructs a reqwest client of its own.
        let http = build_http_client(ca_bundle_pem, connection.insecure_skip_tls_verify)?;
        let client = ClientBuilder::new(server_address(&connection.endpoint), http)
            .basic_authn(&creds.username, &creds.password)
            .build()
            .await
            .map_err(|e| Error::Vsphere(format!("connect: {e}")))?;
        Ok(Box::new(VimClientImpl { client }))
    }
}

/// Real vim_rs-backed client. Holds an `Arc<Client>` from the builder; the
/// `Drop` impl logs out automatically when the last `Arc` is dropped.
pub struct VimClientImpl {
    client: Arc<Client>,
}

#[async_trait]
impl VSphereClient for VimClientImpl {
    async fn list_datacenters(&self) -> Result<Vec<Datacenter>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);

        let view_ref = vm
            .create_container_view(
                &sc.root_folder,
                Some(&[MO_TYPE_DATACENTER.to_string()]),
                true,
            )
            .await
            .map_err(|e| Error::Vsphere(format!("create_container_view(Datacenter): {e}")))?;
        let view = ContainerView::new(self.client.clone(), &view_ref.value);

        let morefs = view
            .view()
            .await
            .map_err(|e| Error::Vsphere(format!("ContainerView.view: {e}")))?
            .unwrap_or_default();

        // Destroy the view eagerly so vCenter doesn't accumulate ghost views.
        // Ignore destroy errors — they're not fatal for the caller.
        let _ = view.destroy_view().await;

        let mut out = Vec::with_capacity(morefs.len());
        for moref in morefs {
            let dc = VimDatacenter::new(self.client.clone(), &moref.value);
            let name = dc
                .name()
                .await
                .map_err(|e| Error::Vsphere(format!("Datacenter.name({}): {e}", moref.value)))?;
            out.push(Datacenter {
                name,
                moref: moref.value,
            });
        }
        Ok(out)
    }

    async fn list_clusters(&self, dc: &Datacenter) -> Result<Vec<Cluster>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);

        // Scope the container view to the Datacenter so we only see its clusters.
        let dc_moref = ManagedObjectReference {
            r#type: vim_rs::types::enums::MoTypesEnum::Datacenter,
            value: dc.moref.clone(),
        };
        let view_ref = vm
            .create_container_view(&dc_moref, Some(&[MO_TYPE_CLUSTER.to_string()]), true)
            .await
            .map_err(|e| Error::Vsphere(format!("create_container_view(Cluster): {e}")))?;
        let view = ContainerView::new(self.client.clone(), &view_ref.value);

        let morefs = view
            .view()
            .await
            .map_err(|e| Error::Vsphere(format!("ContainerView.view: {e}")))?
            .unwrap_or_default();
        let _ = view.destroy_view().await;

        let mut out = Vec::with_capacity(morefs.len());
        for moref in morefs {
            let cluster = ClusterComputeResource::new(self.client.clone(), &moref.value);
            let name = cluster
                .name()
                .await
                .map_err(|e| Error::Vsphere(format!("Cluster.name({}): {e}", moref.value)))?;
            out.push(Cluster {
                name,
                moref: moref.value,
                datacenter_moref: dc.moref.clone(),
            });
        }
        Ok(out)
    }

    async fn find_template(
        &self,
        dc: &Datacenter,
        folder: Option<&str>,
        name: &str,
    ) -> Result<Option<Template>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);

        // Root the view at the requested folder (per-zone `Url`-kind
        // import), or the whole datacenter (`Template`-kind, no per-zone
        // folder). A missing folder means no template can be in it.
        let root_moref = match folder {
            Some(path) if !path.is_empty() => {
                let dc_client = VimDatacenter::new(self.client.clone(), &dc.moref);
                let vm_folder = dc_client.vm_folder().await.map_err(|e| {
                    Error::Vsphere(format!("Datacenter.vmFolder({}): {e}", dc.moref))
                })?;
                match self.find_folder(&vm_folder.value, path).await? {
                    Some(m) => m,
                    None => return Ok(None),
                }
            }
            _ => dc.moref.clone(),
        };
        let root_type = if folder.is_some_and(|p| !p.is_empty()) {
            vim_rs::types::enums::MoTypesEnum::Folder
        } else {
            vim_rs::types::enums::MoTypesEnum::Datacenter
        };
        let root_ref = ManagedObjectReference {
            r#type: root_type,
            value: root_moref,
        };
        let view_ref = vm
            .create_container_view(
                &root_ref,
                Some(&[MO_TYPE_VIRTUAL_MACHINE.to_string()]),
                true,
            )
            .await
            .map_err(|e| Error::Vsphere(format!("create_container_view(VirtualMachine): {e}")))?;
        let view = ContainerView::new(self.client.clone(), &view_ref.value);

        let morefs = view
            .view()
            .await
            .map_err(|e| Error::Vsphere(format!("ContainerView.view: {e}")))?
            .unwrap_or_default();
        let _ = view.destroy_view().await;

        // Filter to templates and match by name. vCenter inventories can have
        // thousands of VMs; we ask each per-VM rather than fetching all configs
        // up front because PropertyCollector batching is iteration-2b territory.
        // The common case (handful of templates per DC) is fine without it.
        for moref in morefs {
            let vmm = VimVirtualMachine::new(self.client.clone(), &moref.value);
            let cfg = match vmm.config().await {
                Ok(Some(c)) => c,
                Ok(None) => continue,
                Err(e) => {
                    return Err(Error::Vsphere(format!(
                        "VirtualMachine.config({}): {e}",
                        moref.value
                    )));
                }
            };
            if !cfg.template {
                continue;
            }
            if cfg.name != name {
                continue;
            }
            return Ok(Some(Template {
                name: cfg.name,
                moref: moref.value,
                datacenter_moref: dc.moref.clone(),
            }));
        }
        Ok(None)
    }

    async fn list_datastores(&self, cluster: &Cluster) -> Result<Vec<Datastore>> {
        // A cluster's reachable datastores come from its own `datastore`
        // association, not the folder tree — so read the property directly
        // rather than a container view (ADR-0019).
        let ccr = ClusterComputeResource::new(self.client.clone(), &cluster.moref);
        let morefs = ccr
            .datastore()
            .await
            .map_err(|e| Error::Vsphere(format!("Cluster.datastore({}): {e}", cluster.moref)))?
            .unwrap_or_default();

        let mut out = Vec::with_capacity(morefs.len());
        for moref in morefs {
            let ds = VimDatastore::new(self.client.clone(), &moref.value);
            let name = ds
                .name()
                .await
                .map_err(|e| Error::Vsphere(format!("Datastore.name({}): {e}", moref.value)))?;
            // A datastore in an SDRS datastore cluster has a StoragePod parent;
            // record that name so `target.datastoreCluster` can be matched.
            let datastore_cluster = match ds.parent().await {
                Ok(Some(parent)) if parent.r#type == MoTypesEnum::StoragePod => {
                    let pod = VimStoragePod::new(self.client.clone(), &parent.value);
                    pod.name().await.ok()
                }
                _ => None,
            };
            // Free space (summary.freeSpace) — best-effort; None if the summary
            // read fails, so datastore-cluster selection can still fall back to
            // a deterministic choice.
            let free_space_bytes = ds.summary().await.ok().map(|s| s.free_space);
            out.push(Datastore {
                name,
                moref: moref.value,
                datastore_cluster,
                free_space_bytes,
            });
        }
        Ok(out)
    }

    async fn list_networks(&self, cluster: &Cluster) -> Result<Vec<Network>> {
        let ccr = ClusterComputeResource::new(self.client.clone(), &cluster.moref);
        let morefs = ccr
            .network()
            .await
            .map_err(|e| Error::Vsphere(format!("Cluster.network({}): {e}", cluster.moref)))?
            .unwrap_or_default();

        let mut out = Vec::with_capacity(morefs.len());
        for moref in morefs {
            // The MOR type is the authoritative distributed-vs-standard signal.
            let distributed = moref.r#type == MoTypesEnum::DistributedVirtualPortgroup;
            let net = VimNetwork::new(self.client.clone(), &moref.value);
            let name = net
                .name()
                .await
                .map_err(|e| Error::Vsphere(format!("Network.name({}): {e}", moref.value)))?;
            out.push(Network {
                name,
                moref: moref.value,
                distributed,
            });
        }
        Ok(out)
    }

    async fn import_iso_template(&self, req: &crate::client::IsoImportRequest) -> Result<String> {
        // ADR-0020: the ISO is already uploaded (`req.iso_datastore_path`). Here
        // we `CreateVM_Task` an empty EFI VM (pvscsi + blank disk + IDE CD-ROM
        // backed by the ISO) in the cluster's resource pool, then
        // `MarkAsTemplate` — mirroring `~/dev/vm-build/bin/create-kairos-template.sh`.
        // NB: the NIC is a follow-up (DVS port backing needs the port-group key
        // + switch UUID); a disk+ISO template still boots the installer.
        let resolved = format!("[{}] {}", req.datastore, req.template_name);

        // Resource pool (cluster) + VM folder (datacenter) for the create.
        let ccr = ClusterComputeResource::new(self.client.clone(), &req.cluster_moref);
        let pool = ccr
            .resource_pool()
            .await
            .map_err(|e| {
                Error::Vsphere(format!("Cluster.resourcePool({}): {e}", req.cluster_moref))
            })?
            .ok_or(Error::Missing("ClusterComputeResource.resourcePool"))?;
        let dc = VimDatacenter::new(self.client.clone(), &req.datacenter_moref);
        let vm_folder = dc.vm_folder().await.map_err(|e| {
            Error::Vsphere(format!(
                "Datacenter.vmFolder({}): {e}",
                req.datacenter_moref
            ))
        })?;

        // Resolve (creating if missing) the zone's own template folder before
        // any lookup-by-name: every zone shares the same template display
        // name, so the idempotency/destroy check below must be scoped to
        // *this* folder, never the whole datacenter (found live: a
        // datacenter-wide search let one zone's forceCreate destroy a
        // different zone's in-flight VM).
        let target_folder = match req.folder.as_deref() {
            Some(path) if !path.is_empty() => self.ensure_folder(&vm_folder.value, path).await?,
            _ => vm_folder.value.clone(),
        };

        // Idempotency + forceCreate: an existing VM/template of this name in
        // this zone's folder is a no-op unless forceCreate, which destroys
        // it first.
        if let Some(existing) = self
            .find_vm_moref_by_name(&target_folder, &req.template_name)
            .await?
        {
            if !req.force_create {
                info!(name = %req.template_name, "template already exists; skipping create (set forceCreate to replace)");
                return Ok(resolved);
            }
            info!(name = %req.template_name, "forceCreate: destroying existing VM/template before recreate");
            let vmm = VimVirtualMachine::new(self.client.clone(), &existing);
            let task = vmm
                .destroy_task()
                .await
                .map_err(|e| Error::Vsphere(format!("Destroy_Task({existing}): {e}")))?;
            self.wait_for_task(&task.value, "destroy existing template")
                .await?;
        }

        // NIC backings: distributed vDS port (portgroupKey + switchUuid) for
        // a dvPortGroup, else a standard device-name backing — one per
        // declared NIC (ADR-0031).
        let mut nic_backings = Vec::with_capacity(req.nics.len());
        for nic in &req.nics {
            nic_backings.push(
                self.build_nic_backing(&nic.network, &nic.network_moref, nic.network_distributed)
                    .await?,
            );
        }

        let config = build_template_config_spec(req, nic_backings);
        let folder = Folder::new(self.client.clone(), &target_folder);
        let task = folder
            .create_vm_task(&config, &pool, None)
            .await
            .map_err(|e| Error::Vsphere(format!("CreateVM_Task({}): {e}", req.template_name)))?;
        self.wait_for_task(&task.value, "create VM").await?;

        // Find the just-created VM. When auto-managed (ADR-0021), install
        // Kairos unattended and generalize it before marking as a template;
        // otherwise fall straight through to MarkAsTemplate (ADR-0020's
        // original behavior) for a build that isn't Kairos-driven or whose
        // install/generalize is managed some other way.
        let vm_moref = self
            .find_vm_moref_by_name(&target_folder, &req.template_name)
            .await?
            .ok_or(Error::Vsphere(
                "created VM not found after CreateVM_Task".to_string(),
            ))?;
        let vmm = VimVirtualMachine::new(self.client.clone(), &vm_moref);

        let created_cfg = vmm
            .config()
            .await
            .map_err(|e| Error::Vsphere(format!("VirtualMachine.config({vm_moref}): {e}")))?
            .ok_or_else(|| Error::Vsphere(format!("{vm_moref}: no config after CreateVM_Task")))?;
        let created_devices = created_cfg.hardware.device.unwrap_or_default();

        // NIC PCI-slot placement is governed entirely by the
        // `ethernetN.pciSlotNumber` ExtraConfig entries, pinned below in a
        // *separate* post-create ReconfigVM_Task — never by the structured
        // `VirtualDevice.slotInfo` object, and never inline in the
        // CreateVM_Task above (`build_template_config_spec` deliberately
        // sets neither). Two earlier attempts at this same fix didn't
        // stick, both found live: (1) pinning `slotInfo` in a separate
        // post-create reconfigure — vCenter silently reassigns it — and
        // additionally clobbered the ExtraConfig value in the same call;
        // (2) setting the ExtraConfig entry *inline* in the initial
        // CreateVM_Task, alongside the `device_change` that creates the
        // NIC itself — the freshly created template's own config still
        // read back the auto-assigned slot, not the requested one, even
        // with nothing else touching it afterward. Only a wholly separate
        // ReconfigVM_Task, with no `device_change` at all, run after
        // CreateVM_Task has already committed — exactly
        // `create-kairos-template.sh`'s own two-step `govc vm.create` /
        // `govc vm.change -e` sequence — actually sticks.
        let created_nic_keys = find_all_nic_keys(&created_devices);
        if created_nic_keys.len() != req.nics.len() {
            return Err(Error::Vsphere(format!(
                "{vm_moref}: expected {} NIC device(s) after CreateVM_Task, found {}",
                req.nics.len(),
                created_nic_keys.len()
            )));
        }
        let pci_slots: Vec<i32> = req.nics.iter().map(|nic| nic.pci_slot).collect();
        let pci_slot_spec = build_nic_pci_slot_extra_config_reconfigure_spec(&pci_slots);
        let pci_slot_task = vmm.reconfig_vm_task(&pci_slot_spec).await.map_err(|e| {
            Error::Vsphere(format!(
                "ReconfigVM_Task({vm_moref}) [pin NIC PCI slot(s)]: {e}"
            ))
        })?;
        self.wait_for_task(&pci_slot_task.value, "pin NIC PCI slot(s)")
            .await?;

        if req.install_mode == InstallMode::Immediate {
            info!(vm_moref = %vm_moref, "entering auto-manage install phase");

            // Resolve real device keys and apply the boot-order reconfigure
            // that the initial CreateVM_Task spec deliberately omits (see
            // build_boot_order_reconfigure_spec): connect the CD-ROM and set
            // boot order to [cdrom, disk, ethernet], mirroring
            // create-vm.sh's govc device.connect + device.boot -order.
            let cdrom_placement = find_cdrom_placement(&created_devices).ok_or_else(|| {
                Error::Vsphere(format!("{vm_moref}: no CD-ROM device after CreateVM_Task"))
            })?;
            let disk_key = find_disk_key(&created_devices).ok_or_else(|| {
                Error::Vsphere(format!("{vm_moref}: no disk device after CreateVM_Task"))
            })?;
            // Boot order only needs the primary (first) NIC — the one
            // that's the guest's boot-eligible ethernet device, same as
            // the single-NIC behavior before ADR-0031.
            let (boot_nic_key, ..) = find_first_nic_key(&created_devices).ok_or_else(|| {
                Error::Vsphere(format!("{vm_moref}: no NIC device after CreateVM_Task"))
            })?;
            let boot_spec = build_boot_order_reconfigure_spec(
                cdrom_placement,
                &req.iso_datastore_path,
                disk_key,
                boot_nic_key,
            );
            let boot_task = vmm.reconfig_vm_task(&boot_spec).await.map_err(|e| {
                Error::Vsphere(format!("ReconfigVM_Task({vm_moref}) [boot order]: {e}"))
            })?;
            self.wait_for_task(&boot_task.value, "set boot order")
                .await?;

            // Power on and confirm it actually started before settling in for
            // the (long) install wait.
            let power_on_task = vmm
                .power_on_vm_task(None)
                .await
                .map_err(|e| Error::Vsphere(format!("PowerOnVM_Task({vm_moref}): {e}")))?;
            self.wait_for_task(&power_on_task.value, "power on").await?;
            let runtime = vmm
                .runtime()
                .await
                .map_err(|e| Error::Vsphere(format!("VirtualMachine.runtime({vm_moref}): {e}")))?;
            if !matches!(runtime.power_state, VirtualMachinePowerStateEnum::PoweredOn) {
                return Err(Error::Vsphere(format!(
                    "{vm_moref} did not report poweredOn after PowerOnVM_Task (state: {:?})",
                    runtime.power_state
                )));
            }

            // Wait for the cloud-config's unattended install to finish and the
            // VM to power itself off (install.poweroff: true, no reboot — the
            // disk is never booted by the build). A cloud-config missing that
            // contract times out here rather than hanging the Job forever;
            // the VM is left powered on for console debugging.
            info!(
                vm_moref = %vm_moref,
                timeout_seconds = req.install_timeout_seconds,
                "waiting for the unattended Kairos install to finish and the VM to power itself off"
            );
            self.wait_for_install_poweroff(&vmm, &vm_moref, req.install_timeout_seconds)
                .await?;

            // Generalized and shut down: strip the ISO-backed CD-ROM so no
            // future clone of this template carries it.
            let cfg = vmm
                .config()
                .await
                .map_err(|e| Error::Vsphere(format!("VirtualMachine.config({vm_moref}): {e}")))?;
            let cdrom_key = cfg
                .as_ref()
                .and_then(|c| c.hardware.device.as_deref())
                .and_then(find_cdrom_key);
            match cdrom_key {
                Some(key) => {
                    let remove_cdrom = VirtualDeviceConfigSpec {
                        operation: Some(VirtualDeviceConfigSpecOperationEnum::Remove),
                        device: Box::new(VirtualCdrom {
                            virtual_device_: VirtualDevice {
                                key,
                                ..Default::default()
                            },
                        }),
                        ..Default::default()
                    };
                    let reconfig_spec = VirtualMachineConfigSpec {
                        device_change: Some(vec![Box::new(remove_cdrom)]),
                        ..Default::default()
                    };
                    let task = vmm.reconfig_vm_task(&reconfig_spec).await.map_err(|e| {
                        Error::Vsphere(format!("ReconfigVM_Task({vm_moref}) [remove CD-ROM]: {e}"))
                    })?;
                    self.wait_for_task(&task.value, "remove CD-ROM").await?;
                }
                None => {
                    tracing::warn!(
                        moref = %vm_moref,
                        "no CD-ROM device found to remove; template may still reference the install ISO"
                    );
                }
            }
        }

        vmm.mark_as_template()
            .await
            .map_err(|e| Error::Vsphere(format!("MarkAsTemplate({vm_moref}): {e}")))?;

        info!(template = %resolved, moref = %vm_moref, "created vSphere template");
        Ok(resolved)
    }

    async fn ensure_datastore_dir(
        &self,
        datacenter_moref: &str,
        datastore: &str,
        dir: &str,
    ) -> Result<()> {
        let sc = self.client.service_content();
        let fm_moref = sc
            .file_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.file_manager"))?;
        let fm = FileManager::new(self.client.clone(), &fm_moref.value);
        let dc = ManagedObjectReference {
            r#type: MoTypesEnum::Datacenter,
            value: datacenter_moref.to_string(),
        };
        let name = format!("[{datastore}] {dir}");
        match fm.make_directory(&name, Some(&dc), Some(true)).await {
            Ok(()) => Ok(()),
            // Idempotent: an existing directory is success, not an error. vCenter
            // reports this as a FileAlreadyExists fault; match on the message
            // rather than the fault type (vim_rs surfaces faults as strings).
            Err(e) if e.to_string().to_lowercase().contains("already exists") => Ok(()),
            Err(e) => Err(Error::Vsphere(format!("make_directory({name}): {e}"))),
        }
    }

    async fn destroy_if_present(
        &self,
        datacenter_moref: &str,
        folder: &str,
        name: &str,
    ) -> Result<()> {
        // Scoped to the zone's own folder (ADR-0020 Decision #5 / the same
        // fix as `import_iso_template`'s idempotency check) — every zone
        // shares this display name, so a datacenter-wide lookup here would
        // risk destroying a different zone's in-flight VM.
        let dc = VimDatacenter::new(self.client.clone(), datacenter_moref);
        let vm_folder = dc
            .vm_folder()
            .await
            .map_err(|e| Error::Vsphere(format!("Datacenter.vmFolder({datacenter_moref}): {e}")))?;
        let target_folder = self.ensure_folder(&vm_folder.value, folder).await?;

        let Some(existing) = self.find_vm_moref_by_name(&target_folder, name).await? else {
            return Ok(());
        };
        info!(name, moref = %existing, "destroying existing VM/template before recreate");
        self.power_off_and_destroy(&existing).await
    }

    async fn clone_vm(&self, req: &crate::client::CloneVmRequest) -> Result<String> {
        // Resource pool (cluster) + VM folder (datacenter), same resolution
        // as import_iso_template.
        let ccr = ClusterComputeResource::new(self.client.clone(), &req.cluster_moref);
        let pool = ccr
            .resource_pool()
            .await
            .map_err(|e| {
                Error::Vsphere(format!("Cluster.resourcePool({}): {e}", req.cluster_moref))
            })?
            .ok_or(Error::Missing("ClusterComputeResource.resourcePool"))?;
        let dc = VimDatacenter::new(self.client.clone(), &req.datacenter_moref);
        let vm_folder = dc.vm_folder().await.map_err(|e| {
            Error::Vsphere(format!(
                "Datacenter.vmFolder({}): {e}",
                req.datacenter_moref
            ))
        })?;
        let target_folder = match req.folder.as_deref() {
            Some(path) if !path.is_empty() => self.ensure_folder(&vm_folder.value, path).await?,
            _ => vm_folder.value.clone(),
        };

        // Read the template's own devices to find its (single) NIC's device
        // key — the clone reconfigures that same key onto the target zone's
        // port group rather than adding a new device.
        let template = VimVirtualMachine::new(self.client.clone(), &req.template_moref);
        let template_cfg = template
            .config()
            .await
            .map_err(|e| {
                Error::Vsphere(format!(
                    "VirtualMachine.config({}): {e}",
                    req.template_moref
                ))
            })?
            .ok_or_else(|| {
                Error::Vsphere(format!("{}: no config on template", req.template_moref))
            })?;
        let template_devices = template_cfg.hardware.device.unwrap_or_default();
        let (nic_key, nic_adapter_type, nic_pci_slot) = find_first_nic_key(&template_devices)
            .ok_or_else(|| {
                Error::Vsphere(format!(
                    "{}: template has no NIC to reconfigure",
                    req.template_moref
                ))
            })?;
        // Diagnostic (found live: a clone came up ens33 despite the template
        // itself reportedly being pinned to 192, even with the PCI-slot
        // re-pinning fix deployed) — logs exactly what was read off the
        // *real* template's NIC so a live retry shows whether slot_info
        // round-tripped through vim_rs's deserializer/downcast at all,
        // rather than guessing further from unit-test fixtures alone.
        info!(
            template = %req.template_moref,
            nic_key,
            adapter_type = ?nic_adapter_type,
            pci_slot = ?nic_pci_slot,
            "template NIC resolved for clone"
        );

        let nic_backing = self
            .build_nic_backing(&req.network, &req.network_moref, req.network_distributed)
            .await?;
        let nic_change = VirtualDeviceConfigSpec {
            operation: Some(VirtualDeviceConfigSpecOperationEnum::Edit),
            device: build_nic_edit_device(nic_key, nic_backing, nic_adapter_type, nic_pci_slot),
            ..Default::default()
        };

        let mut extra_config: Vec<Box<dyn vim_rs::types::traits::OptionValueTrait>> = req
            .extra_config
            .iter()
            .map(|(key, value)| {
                Box::new(OptionValue {
                    key: key.clone(),
                    value: Some(VimAny::Value(ValueElements::PrimitiveString(value.clone()))),
                }) as Box<dyn vim_rs::types::traits::OptionValueTrait>
            })
            .collect();

        // Carry the template's own `ethernet0.pciSlotNumber` ExtraConfig
        // entry forward explicitly, rather than relying on CloneVM_Task to
        // inherit it implicitly from the source VMX. This is the ACTUAL
        // field governing a stable ens192 in the guest (confirmed via
        // govc's own -e flag semantics; NOT the structured
        // VirtualDevice.slotInfo this same function's `nic_change` edits
        // above, which found live never controlled guest-visible PCI
        // placement in the first place) — read from the template's own
        // extraConfig, not re-derived from `nic_pci_slot` (that structured
        // read can legitimately differ, e.g. if it was silently
        // reassigned), matching "explicit over implicit" rather than
        // assuming CloneVM_Task's inheritance behavior is what's wanted.
        let template_pci_slot_number = template_cfg
            .extra_config
            .as_ref()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|ov| ov.get_option_value().key == "ethernet0.pciSlotNumber")
            })
            .and_then(|ov| match &ov.get_option_value().value {
                Some(VimAny::Value(ValueElements::PrimitiveString(s))) => Some(s.clone()),
                _ => None,
            });
        // Diagnostic (companion to the structured `pci_slot` log above):
        // the structured `slotInfo` read never controlled guest-visible PCI
        // placement, but silently logging "found" vs. "absent" here left no
        // way to tell, on a live ens33 report, whether the template's own
        // ExtraConfig actually carried `ethernet0.pciSlotNumber` at all
        // versus this carry-forward step finding nothing to propagate.
        info!(
            template = %req.template_moref,
            template_pci_slot_number = ?template_pci_slot_number,
            "template ethernet0.pciSlotNumber ExtraConfig resolved for clone carry-forward"
        );
        if let Some(pci_slot_number) = template_pci_slot_number {
            extra_config.push(Box::new(OptionValue {
                key: "ethernet0.pciSlotNumber".to_string(),
                value: Some(VimAny::Value(ValueElements::PrimitiveString(
                    pci_slot_number,
                ))),
            }));
        }

        let clone_spec = VirtualMachineCloneSpec {
            location: VirtualMachineRelocateSpec {
                pool: Some(pool),
                datastore: Some(ManagedObjectReference {
                    r#type: MoTypesEnum::Datastore,
                    value: req.datastore_moref.clone(),
                }),
                folder: Some(ManagedObjectReference {
                    r#type: MoTypesEnum::Folder,
                    value: target_folder.clone(),
                }),
                ..Default::default()
            },
            template: false,
            config: Some(VirtualMachineConfigSpec {
                num_cp_us: Some(req.num_cpus),
                memory_mb: Some(req.memory_mib),
                device_change: Some(vec![Box::new(nic_change)]),
                extra_config: Some(extra_config),
                ..Default::default()
            }),
            // Always cloned powered off — the caller drives the desired
            // power state afterward via `set_power_state` (ADR-0024).
            power_on: false,
            ..Default::default()
        };

        let task = template
            .clone_vm_task(
                &ManagedObjectReference {
                    r#type: MoTypesEnum::Folder,
                    value: vm_folder.value.clone(),
                },
                &req.vm_name,
                &clone_spec,
            )
            .await
            .map_err(|e| Error::Vsphere(format!("CloneVM_Task({}): {e}", req.template_moref)))?;
        self.wait_for_task(&task.value, "clone VM").await?;

        self.find_vm_moref_by_name(&target_folder, &req.vm_name)
            .await?
            .ok_or_else(|| Error::Vsphere("cloned VM not found after CloneVM_Task".to_string()))
    }

    async fn set_power_state(&self, vm_moref: &str, desired: PowerState) -> Result<()> {
        let vmm = VimVirtualMachine::new(self.client.clone(), vm_moref);
        let task = match desired {
            PowerState::PoweredOn => vmm
                .power_on_vm_task(None)
                .await
                .map_err(|e| Error::Vsphere(format!("PowerOnVM_Task({vm_moref}): {e}")))?,
            PowerState::PoweredOff => vmm
                .power_off_vm_task()
                .await
                .map_err(|e| Error::Vsphere(format!("PowerOffVM_Task({vm_moref}): {e}")))?,
            PowerState::Suspended => vmm
                .suspend_vm_task()
                .await
                .map_err(|e| Error::Vsphere(format!("SuspendVM_Task({vm_moref}): {e}")))?,
        };
        self.wait_for_task(&task.value, "set power state").await
    }

    async fn add_tpm_device(&self, vm_moref: &str) -> Result<()> {
        let vmm = VimVirtualMachine::new(self.client.clone(), vm_moref);
        let spec = build_add_tpm_reconfigure_spec();
        let task = vmm
            .reconfig_vm_task(&spec)
            .await
            .map_err(|e| Error::Vsphere(format!("ReconfigVM_Task({vm_moref}) [add vTPM]: {e}")))?;
        self.wait_for_task(&task.value, "add vTPM device").await
    }

    async fn power_state(&self, vm_moref: &str) -> Result<PowerState> {
        let vmm = VimVirtualMachine::new(self.client.clone(), vm_moref);
        let runtime = vmm
            .runtime()
            .await
            .map_err(|e| Error::Vsphere(format!("VirtualMachine.runtime({vm_moref}): {e}")))?;
        map_vim_power_state(&runtime.power_state).ok_or_else(|| {
            Error::Vsphere(format!(
                "{vm_moref}: unrecognized power state {:?}",
                runtime.power_state
            ))
        })
    }

    async fn destroy_vm(&self, vm_moref: &str) -> Result<()> {
        info!(moref = %vm_moref, "destroying VSphereMachine's backend VM");
        match self.power_off_and_destroy(vm_moref).await {
            Ok(()) => Ok(()),
            Err(e)
                if e.to_string()
                    .to_lowercase()
                    .contains("managedobjectnotfound") =>
            {
                info!(moref = %vm_moref, "backend VM already gone; treating as destroyed");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl VimClientImpl {
    /// Power off (if not already) and destroy the VM at `moref`. Shared by
    /// [`VSphereClient::destroy_if_present`] (name+folder based, template
    /// rebuilds) and [`VSphereClient::destroy_vm`] (moref-based,
    /// `VSphereMachine` deletion, ADR-0026) — both need the exact same
    /// "can't destroy a running VM, and vCenter rejects a redundant
    /// power-off" sequence once they've settled on a moref.
    async fn power_off_and_destroy(&self, moref: &str) -> Result<()> {
        let vmm = VimVirtualMachine::new(self.client.clone(), moref);

        // Destroy_Task rejects a powered-on VM (InvalidPowerState) — e.g. a
        // prior run left this target stuck mid-install, still running. Power
        // it off (hard — it's about to be destroyed) before destroying.
        let runtime = vmm
            .runtime()
            .await
            .map_err(|e| Error::Vsphere(format!("VirtualMachine.runtime({moref}): {e}")))?;
        if !matches!(
            runtime.power_state,
            VirtualMachinePowerStateEnum::PoweredOff
        ) {
            info!(
                moref,
                power_state = ?runtime.power_state,
                "powering off existing target before destroy"
            );
            let power_off_task = vmm
                .power_off_vm_task()
                .await
                .map_err(|e| Error::Vsphere(format!("PowerOffVM_Task({moref}): {e}")))?;
            self.wait_for_task(&power_off_task.value, "power off existing target")
                .await?;
        }

        let task = vmm
            .destroy_task()
            .await
            .map_err(|e| Error::Vsphere(format!("Destroy_Task({moref}): {e}")))?;
        self.wait_for_task(&task.value, "destroy existing target")
            .await
    }

    /// Poll a vCenter task to completion. `Ok(())` on `Success`; `Err` on
    /// `Error` (carrying the fault message) or if it does not settle within
    /// [`TASK_POLL_MAX_ATTEMPTS`]. `what` names the operation for diagnostics.
    async fn wait_for_task(&self, task_moref: &str, what: &str) -> Result<()> {
        let task = Task::new(self.client.clone(), task_moref);
        for _ in 0..TASK_POLL_MAX_ATTEMPTS {
            let info = task
                .info()
                .await
                .map_err(|e| Error::Vsphere(format!("Task.info({task_moref}) [{what}]: {e}")))?;
            match info.state {
                TaskInfoStateEnum::Success => return Ok(()),
                TaskInfoStateEnum::Error => {
                    // Debug-format the MethodFault — enough detail for the Job
                    // log without guessing the localizable-message shape.
                    let msg = info
                        .error
                        .map(|f| format!("{f:?}"))
                        .unwrap_or_else(|| "no fault detail".to_string());
                    return Err(Error::Vsphere(format!("{what} task failed: {msg}")));
                }
                // Queued / Running / any future state — keep polling.
                _ => {
                    tokio::time::sleep(TASK_POLL_INTERVAL).await;
                }
            }
        }
        Err(Error::Vsphere(format!(
            "{what} task {task_moref} did not complete within {}s",
            TASK_POLL_MAX_ATTEMPTS as u64 * TASK_POLL_INTERVAL.as_secs()
        )))
    }

    /// Poll `vmm.runtime().power_state` until it reports `poweredOff` (the
    /// cloud-config's `install.poweroff: true` firing once the unattended
    /// Kairos install completes), or fail once
    /// [`install_poll_max_attempts`] is exhausted. Never destroys or powers
    /// off the VM itself — a timeout leaves it running for console debugging
    /// (ADR-0021).
    async fn wait_for_install_poweroff(
        &self,
        vmm: &VimVirtualMachine,
        vm_moref: &str,
        install_timeout_seconds: i32,
    ) -> Result<()> {
        let max_attempts = install_poll_max_attempts(install_timeout_seconds);
        for _ in 0..max_attempts {
            let runtime = vmm
                .runtime()
                .await
                .map_err(|e| Error::Vsphere(format!("VirtualMachine.runtime({vm_moref}): {e}")))?;
            if matches!(
                runtime.power_state,
                VirtualMachinePowerStateEnum::PoweredOff
            ) {
                return Ok(());
            }
            tokio::time::sleep(INSTALL_POLL_INTERVAL).await;
        }
        Err(Error::Vsphere(format!(
            "{vm_moref} did not power itself off within {}s of the unattended Kairos install \
             starting; the cloud-config must set install.poweroff: true and an \
             after-install-chroot identity-wipe stage (ADR-0021) — the VM was left running \
             for inspection",
            u64::from(max_attempts) * INSTALL_POLL_INTERVAL.as_secs()
        )))
    }

    /// Find a VirtualMachine (template or not) by display name within a
    /// specific VM folder, returning its moref. Mirrors
    /// [`VSphereClient::find_template`] but without the `config.template`
    /// filter — used to locate the VM created by `CreateVM_Task` before
    /// `MarkAsTemplate`, and any existing one to destroy on `forceCreate`.
    ///
    /// Deliberately scoped to `folder_moref`, not the whole datacenter: every
    /// zone's template shares the same display name (the `VMImage` name),
    /// distinguished only by which zone-specific folder it lives in
    /// (ADR-0020 Decision #5). A datacenter-wide search would match — and
    /// `forceCreate` would then destroy — a *different* zone's in-flight
    /// build that happens to share the name (found live: concurrent per-zone
    /// import Jobs destroying each other's just-created VMs).
    async fn find_vm_moref_by_name(
        &self,
        folder_moref: &str,
        name: &str,
    ) -> Result<Option<String>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);
        let folder = ManagedObjectReference {
            r#type: MoTypesEnum::Folder,
            value: folder_moref.to_string(),
        };
        let view_ref = vm
            .create_container_view(&folder, Some(&[MO_TYPE_VIRTUAL_MACHINE.to_string()]), true)
            .await
            .map_err(|e| Error::Vsphere(format!("create_container_view(VirtualMachine): {e}")))?;
        let view = ContainerView::new(self.client.clone(), &view_ref.value);
        let morefs = view
            .view()
            .await
            .map_err(|e| Error::Vsphere(format!("ContainerView.view: {e}")))?
            .unwrap_or_default();
        let _ = view.destroy_view().await;

        for moref in morefs {
            let vmm = VimVirtualMachine::new(self.client.clone(), &moref.value);
            match vmm.config().await {
                Ok(Some(cfg)) if cfg.name == name => return Ok(Some(moref.value)),
                Ok(_) => {}
                Err(e) => {
                    return Err(Error::Vsphere(format!(
                        "VirtualMachine.config({}): {e}",
                        moref.value
                    )));
                }
            }
        }
        Ok(None)
    }

    /// Resolve a distributed (vDS) port group moref to its
    /// `(portgroupKey, switchUuid)` — the pair a distributed-port NIC backing
    /// needs.
    async fn resolve_dvs_port(&self, portgroup_moref: &str) -> Result<(String, String)> {
        let dvpg = DistributedVirtualPortgroup::new(self.client.clone(), portgroup_moref);
        let key = dvpg
            .key()
            .await
            .map_err(|e| Error::Vsphere(format!("DVPortgroup.key({portgroup_moref}): {e}")))?;
        let cfg = dvpg
            .config()
            .await
            .map_err(|e| Error::Vsphere(format!("DVPortgroup.config({portgroup_moref}): {e}")))?;
        let vds_moref = cfg.distributed_virtual_switch.ok_or(Error::Missing(
            "DVPortgroupConfigInfo.distributedVirtualSwitch",
        ))?;
        let uuid = DistributedVirtualSwitch::new(self.client.clone(), &vds_moref.value)
            .uuid()
            .await
            .map_err(|e| Error::Vsphere(format!("DVS.uuid({}): {e}", vds_moref.value)))?;
        Ok((key, uuid))
    }

    /// Build a NIC backing for `network`/`network_moref`: distributed vDS
    /// port (portgroupKey + switchUuid) for a dvPortGroup, else a standard
    /// device-name backing. Shared by `import_iso_template` and `clone_vm`.
    async fn build_nic_backing(
        &self,
        network: &str,
        network_moref: &str,
        network_distributed: bool,
    ) -> Result<Box<dyn VirtualDeviceBackingInfoTrait>> {
        if network_distributed {
            let (portgroup_key, switch_uuid) = self.resolve_dvs_port(network_moref).await?;
            Ok(Box::new(
                VirtualEthernetCardDistributedVirtualPortBackingInfo {
                    port: DistributedVirtualSwitchPortConnection {
                        switch_uuid,
                        portgroup_key: Some(portgroup_key),
                        ..Default::default()
                    },
                },
            ))
        } else {
            Ok(Box::new(VirtualEthernetCardNetworkBackingInfo {
                virtual_device_device_backing_info_: VirtualDeviceDeviceBackingInfo {
                    device_name: network.to_string(),
                    ..Default::default()
                },
                network: Some(ManagedObjectReference {
                    r#type: MoTypesEnum::Network,
                    value: network_moref.to_string(),
                }),
                ..Default::default()
            }))
        }
    }

    /// Find-or-create a folder `path` (slash-separated, e.g. `templates/kairos`)
    /// under `root_moref`, returning the leaf folder's moref. Each missing
    /// segment is created (`Folder.CreateFolder`), mirroring the
    /// `govc folder.create` in `create-kairos-template.sh`.
    async fn ensure_folder(&self, root_moref: &str, path: &str) -> Result<String> {
        let mut current = root_moref.to_string();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            let folder = Folder::new(self.client.clone(), &current);
            let children = folder
                .child_entity()
                .await
                .map_err(|e| Error::Vsphere(format!("Folder.childEntity({current}): {e}")))?
                .unwrap_or_default();
            let mut found = None;
            for child in &children {
                if child.r#type != MoTypesEnum::Folder {
                    continue;
                }
                let name = Folder::new(self.client.clone(), &child.value)
                    .name()
                    .await
                    .unwrap_or_default();
                if name == seg {
                    found = Some(child.value.clone());
                    break;
                }
            }
            current = match found {
                Some(m) => m,
                None => {
                    folder
                        .create_folder(seg)
                        .await
                        .map_err(|e| Error::Vsphere(format!("CreateFolder({seg}): {e}")))?
                        .value
                }
            };
        }
        Ok(current)
    }

    /// Read-only counterpart to [`Self::ensure_folder`]: walk `path` under
    /// `root_moref`, returning `Ok(None)` the moment any segment is
    /// missing rather than creating it. Used by a lookup (`find_template`)
    /// that must never have the side effect of creating folders.
    async fn find_folder(&self, root_moref: &str, path: &str) -> Result<Option<String>> {
        let mut current = root_moref.to_string();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            let folder = Folder::new(self.client.clone(), &current);
            let children = folder
                .child_entity()
                .await
                .map_err(|e| Error::Vsphere(format!("Folder.childEntity({current}): {e}")))?
                .unwrap_or_default();
            let mut found = None;
            for child in &children {
                if child.r#type != MoTypesEnum::Folder {
                    continue;
                }
                let name = Folder::new(self.client.clone(), &child.value)
                    .name()
                    .await
                    .unwrap_or_default();
                if name == seg {
                    found = Some(child.value.clone());
                    break;
                }
            }
            match found {
                Some(m) => current = m,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }
}

/// Build the `VirtualMachineConfigSpec` for the template: an EFI VM with a
/// paravirtual SCSI controller + blank thin disk (install target), an IDE
/// CD-ROM backed by the uploaded ISO, and a vmxnet3 NIC (`nic_backing`) at PCI
/// slot 192. `files.vmPathName` points at the resolved datastore so the VM (and
/// its disk) land there. Mirrors `create-kairos-template.sh`.
/// Number of [`INSTALL_POLL_INTERVAL`] polls to attempt before failing the
/// install wait, derived from a timeout in seconds. A non-positive
/// `timeout_seconds` (the field's unset zero value) falls back to
/// [`DEFAULT_INSTALL_TIMEOUT_SECS`]. Pure so the bound is unit-testable
/// without a live vCenter (ADR-0021).
fn install_poll_max_attempts(timeout_seconds: i32) -> u32 {
    let secs = u32::try_from(timeout_seconds).unwrap_or(0);
    let secs = if secs == 0 {
        DEFAULT_INSTALL_TIMEOUT_SECS
    } else {
        secs
    };
    let interval = u32::try_from(INSTALL_POLL_INTERVAL.as_secs())
        .unwrap_or(1)
        .max(1);
    secs.div_ceil(interval).max(1)
}

/// Map vim_rs's `VirtualMachinePowerStateEnum` onto banlieue's own
/// `PowerState` (ADR-0034). `None` for `Other_(_)` — vim_rs's catch-all for
/// values not known at the pinned SDK version; the caller turns that into
/// an error rather than guessing. Pure — no vCenter I/O — so it's
/// unit-testable independent of a live VM.
fn map_vim_power_state(state: &VirtualMachinePowerStateEnum) -> Option<PowerState> {
    match state {
        VirtualMachinePowerStateEnum::PoweredOn => Some(PowerState::PoweredOn),
        VirtualMachinePowerStateEnum::PoweredOff => Some(PowerState::PoweredOff),
        VirtualMachinePowerStateEnum::Suspended => Some(PowerState::Suspended),
        VirtualMachinePowerStateEnum::Other_(_) => None,
    }
}

/// Find the device key of the first CD-ROM among a VM's hardware devices.
/// Pure — operates on an already-fetched device list, no vCenter I/O — so
/// it's unit-testable independent of a live VM (ADR-0021: a generalized
/// template must not carry an ISO-backed CD-ROM device). Identifies the
/// CD-ROM via `VimObjectTrait::data_type()` (vCenter's own `StructType` tag)
/// rather than `Any` downcasting, since vim_rs 0.5's generated `AsAny`
/// blanket impl does not round-trip through the `VirtualDeviceTrait` object
/// for every device type.
fn find_cdrom_key(devices: &[Box<dyn vim_rs::types::traits::VirtualDeviceTrait>]) -> Option<i32> {
    devices
        .iter()
        .find(|d| {
            matches!(
                d.data_type(),
                vim_rs::types::struct_enum::StructType::VirtualCdrom
            )
        })
        .map(|d| d.get_virtual_device().key)
}

/// A CD-ROM's placement identity — just enough to submit a well-formed
/// `Edit` `device_change`. vCenter's Reconfigure rejects an edited device
/// that omits `controllerKey` with `MissingController` ("Device requires a
/// controller"), even when only an unrelated field (like `connectable`) is
/// being changed, mirroring `govc device.connect`'s always-re-send-the-
/// whole-device approach. `VirtualDevice` itself is not `Clone` (its
/// `backing` is a boxed trait object), so this carries only the `Copy`
/// fields needed to reconstruct the device; the backing (the ISO path) is
/// rebuilt from what the caller already knows, not copied off the live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CdromPlacement {
    key: i32,
    controller_key: Option<i32>,
    unit_number: Option<i32>,
}

/// Find the first CD-ROM among a VM's hardware devices and return its
/// [`CdromPlacement`]. Pure — see [`find_cdrom_key`].
fn find_cdrom_placement(
    devices: &[Box<dyn vim_rs::types::traits::VirtualDeviceTrait>],
) -> Option<CdromPlacement> {
    devices
        .iter()
        .find(|d| {
            matches!(
                d.data_type(),
                vim_rs::types::struct_enum::StructType::VirtualCdrom
            )
        })
        .map(|d| {
            let vd = d.get_virtual_device();
            CdromPlacement {
                key: vd.key,
                controller_key: vd.controller_key,
                unit_number: vd.unit_number,
            }
        })
}

/// Find the device key of the first virtual disk among a VM's hardware
/// devices. Pure — see [`find_cdrom_key`].
fn find_disk_key(devices: &[Box<dyn vim_rs::types::traits::VirtualDeviceTrait>]) -> Option<i32> {
    devices
        .iter()
        .find(|d| {
            matches!(
                d.data_type(),
                vim_rs::types::struct_enum::StructType::VirtualDisk
            )
        })
        .map(|d| d.get_virtual_device().key)
}

/// Find the device key, adapter type, and pinned PCI slot (if any) of the
/// first NIC — of any adapter type — among a VM's hardware devices. Pure —
/// see [`find_cdrom_key`]. Used by `clone_vm` (ADR-0024) to redirect a
/// clone's existing NIC onto the target zone's port group, whatever adapter
/// type the source template happens to use. See [`find_all_nic_keys`] for
/// the multi-NIC counterpart `import_iso_template` uses to validate its
/// CreateVM_Task produced the expected number of NIC devices (ADR-0031).
///
/// The PCI slot is read back out via [`VirtualDeviceBusSlotInfoTrait`]'s
/// `AsAny` bound (`vim_rs`'s own documented downcast pattern, see the crate
/// root docs) rather than `find_cdrom_key`'s `data_type()`-only approach —
/// unlike a device's own adapter type, `slot_info` has no significance to
/// filter *on*, only a value to read *out*, once found.
fn find_first_nic_key(
    devices: &[Box<dyn vim_rs::types::traits::VirtualDeviceTrait>],
) -> Option<(i32, vim_rs::types::struct_enum::StructType, Option<i32>)> {
    devices
        .iter()
        .find(|d| {
            matches!(
                d.data_type(),
                vim_rs::types::struct_enum::StructType::VirtualVmxnet3
                    | vim_rs::types::struct_enum::StructType::VirtualVmxnet2
                    | vim_rs::types::struct_enum::StructType::VirtualVmxnet
                    | vim_rs::types::struct_enum::StructType::VirtualE1000
                    | vim_rs::types::struct_enum::StructType::VirtualE1000E
            )
        })
        .map(|d| {
            let vd = d.get_virtual_device();
            let pci_slot = vd.slot_info.as_deref().and_then(|si| {
                si.as_any_ref()
                    .downcast_ref::<VirtualDevicePciBusSlotInfo>()
                    .map(|pci| pci.pci_slot_number)
            });
            (vd.key, d.data_type(), pci_slot)
        })
}

/// Find every NIC's device key and adapter type, in the order they appear
/// in `devices` (ADR-0031) — the multi-NIC counterpart of
/// [`find_first_nic_key`], used by `import_iso_template`'s post-create
/// PCI-slot pin. Correlated with `IsoImportRequest.nics` by that same
/// order: `CreateVM_Task`'s `deviceChange` list is processed in the order
/// given, and this project's existing single-NIC code already trusted
/// device-list order (`find_first_nic_key`/`find_disk_key`/`find_cdrom_key`
/// all take "the first match," not an identity-correlated one) — this
/// extends the same trust to "the Nth match is the Nth requested NIC"
/// rather than introducing per-device backing-network correlation.
fn find_all_nic_keys(
    devices: &[Box<dyn vim_rs::types::traits::VirtualDeviceTrait>],
) -> Vec<(i32, vim_rs::types::struct_enum::StructType)> {
    devices
        .iter()
        .filter(|d| {
            matches!(
                d.data_type(),
                vim_rs::types::struct_enum::StructType::VirtualVmxnet3
                    | vim_rs::types::struct_enum::StructType::VirtualVmxnet2
                    | vim_rs::types::struct_enum::StructType::VirtualVmxnet
                    | vim_rs::types::struct_enum::StructType::VirtualE1000
                    | vim_rs::types::struct_enum::StructType::VirtualE1000E
            )
        })
        .map(|d| (d.get_virtual_device().key, d.data_type()))
        .collect()
}

/// Build the device-edit spec for `clone_vm`'s NIC reconfigure, as the
/// *same concrete adapter type* the template's own NIC already is
/// (`adapter_type`, from [`find_first_nic_key`]).
///
/// `VirtualEthernetCard` — the abstract base every NIC adapter type
/// inherits from — cannot itself be sent as a device spec (found live:
/// vCenter rejected it with `InvalidDeviceSpec`, "Invalid configuration
/// for device '0'"); a `deviceChange` entry must name a concrete,
/// instantiable subtype.
///
/// `pci_slot` re-pins the template's own PCI slot (also from
/// [`find_first_nic_key`]) rather than leaving vCenter to assign a fresh
/// one. Found live: a clone of a `ens192`-pinned template came up as
/// `ens33` — the clone's `deviceChange` edit changes the NIC's backing
/// (network) in the same call that creates the destination VM, which
/// vCenter treats more like a `CreateVM` device placement than an in-place
/// `Reconfigure` of a long-lived VM; omitting `slotInfo` let it fall back to
/// auto-assignment instead of keeping the source's slot. `None` when the
/// template reported no `slot_info` of its own — nothing to reproduce.
fn build_nic_edit_device(
    key: i32,
    backing: Box<dyn VirtualDeviceBackingInfoTrait>,
    adapter_type: vim_rs::types::struct_enum::StructType,
    pci_slot: Option<i32>,
) -> Box<dyn vim_rs::types::traits::VirtualDeviceTrait> {
    use vim_rs::types::struct_enum::StructType;

    let device = VirtualDevice {
        key,
        backing: Some(backing),
        slot_info: pci_slot.map(|pci_slot_number| {
            Box::new(VirtualDevicePciBusSlotInfo { pci_slot_number })
                as Box<dyn vim_rs::types::traits::VirtualDeviceBusSlotInfoTrait>
        }),
        ..Default::default()
    };
    let ethernet_card = VirtualEthernetCard {
        virtual_device_: device,
        ..Default::default()
    };
    match adapter_type {
        StructType::VirtualVmxnet3 => Box::new(VirtualVmxnet3 {
            virtual_vmxnet_: VirtualVmxnet {
                virtual_ethernet_card_: ethernet_card,
            },
            ..Default::default()
        }),
        StructType::VirtualVmxnet2 => Box::new(VirtualVmxnet2 {
            virtual_vmxnet_: VirtualVmxnet {
                virtual_ethernet_card_: ethernet_card,
            },
        }),
        StructType::VirtualVmxnet => Box::new(VirtualVmxnet {
            virtual_ethernet_card_: ethernet_card,
        }),
        StructType::VirtualE1000E => Box::new(VirtualE1000E {
            virtual_ethernet_card_: ethernet_card,
        }),
        // find_first_nic_key only ever matches one of the five arms above
        // (VirtualE1000 is one of them); the wildcard is what actually
        // covers it, plus any other match, without a `_ => unreachable!()`.
        _ => Box::new(VirtualE1000 {
            virtual_ethernet_card_: ethernet_card,
        }),
    }
}

/// Build the post-create reconfigure spec that sets every NIC's
/// `ethernetN.pciSlotNumber` ExtraConfig entry — and nothing else: no
/// `device_change` at all. `pci_slots[i]` is NIC `i`'s requested slot,
/// 0-based in `ethernetN` naming order (matching `req.nics`'s own order,
/// ADR-0031). Pure — no vCenter I/O — so it's unit-testable independent of
/// a live VM.
///
/// Must run as a genuinely separate `ReconfigVM_Task`, *after*
/// `CreateVM_Task` has already committed the NIC device with vCenter's own
/// auto-assigned slot — not folded into that same `CreateVM_Task`'s
/// `extra_config`. Found live: setting this ExtraConfig entry inline,
/// alongside the `device_change` that creates the NIC itself, didn't stick
/// either — the freshly created template's own config still read back the
/// auto-assigned slot, not the requested one. `create-kairos-template.sh`'s
/// own reference sequence is the same two-step shape: a bare `govc
/// vm.create`, then a wholly separate `govc vm.change -e
/// "ethernet0.pciSlotNumber=192"` once the VM already exists — this is
/// that second step.
fn build_nic_pci_slot_extra_config_reconfigure_spec(pci_slots: &[i32]) -> VirtualMachineConfigSpec {
    let extra_config: Vec<Box<dyn vim_rs::types::traits::OptionValueTrait>> = pci_slots
        .iter()
        .enumerate()
        .map(|(i, pci_slot)| {
            Box::new(OptionValue {
                key: format!("ethernet{i}.pciSlotNumber"),
                value: Some(VimAny::Value(ValueElements::PrimitiveString(
                    pci_slot.to_string(),
                ))),
            }) as Box<dyn vim_rs::types::traits::OptionValueTrait>
        })
        .collect();
    VirtualMachineConfigSpec {
        extra_config: Some(extra_config),
        ..Default::default()
    }
}

/// Build the standalone `ReconfigVM_Task` spec that adds a vTPM device
/// (ADR-0039). Pure — no vCenter I/O — so it's unit-testable independent of
/// a live VM, mirroring [`build_nic_pci_slot_extra_config_reconfigure_spec`].
/// `govc` has no wrapping subcommand for this (checked 0.52.0 and 0.56.0);
/// this is the same `VirtualDeviceConfigSpec`/`VirtualTPM` add the vCenter
/// UI and PowerCLI's `New-VTpm` issue.
fn build_add_tpm_reconfigure_spec() -> VirtualMachineConfigSpec {
    let tpm = VirtualTpm {
        virtual_device_: VirtualDevice {
            key: KEY_TPM,
            ..Default::default()
        },
        endorsement_key_certificate_signing_request: None,
        endorsement_key_certificate: None,
    };
    let device_change: Vec<Box<dyn vim_rs::types::traits::VirtualDeviceConfigSpecTrait>> =
        vec![Box::new(VirtualDeviceConfigSpec {
            operation: Some(VirtualDeviceConfigSpecOperationEnum::Add),
            device: Box::new(tpm),
            ..Default::default()
        })];
    VirtualMachineConfigSpec {
        device_change: Some(device_change),
        ..Default::default()
    }
}

/// Build the post-create reconfigure spec that explicitly connects the
/// CD-ROM and sets the boot order to `[cdrom, disk, ethernet]` by real
/// device key — mirroring `create-vm.sh`'s `govc device.connect` +
/// `device.boot -order cdrom,disk,ethernet`. Pure — takes already-resolved
/// device keys, no vCenter I/O — so it's unit-testable independent of a live
/// VM. A boot order embedded in the *initial* `CreateVM_Task` spec
/// (referencing the provisional negative keys) was not reliably honored by
/// EFI firmware on a freshly created VM (found live, ADR-0021); this must
/// run as a *separate* reconfigure once the devices have real keys.
fn build_boot_order_reconfigure_spec(
    cdrom: CdromPlacement,
    iso_datastore_path: &str,
    disk_key: i32,
    nic_key: i32,
) -> VirtualMachineConfigSpec {
    let connect_cdrom = VirtualDeviceConfigSpec {
        operation: Some(VirtualDeviceConfigSpecOperationEnum::Edit),
        device: Box::new(VirtualCdrom {
            virtual_device_: VirtualDevice {
                key: cdrom.key,
                controller_key: cdrom.controller_key,
                unit_number: cdrom.unit_number,
                backing: Some(Box::new(VirtualCdromIsoBackingInfo {
                    virtual_device_file_backing_info_: VirtualDeviceFileBackingInfo {
                        file_name: iso_datastore_path.to_string(),
                        ..Default::default()
                    },
                })),
                connectable: Some(VirtualDeviceConnectInfo {
                    connected: true,
                    start_connected: true,
                    allow_guest_control: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    VirtualMachineConfigSpec {
        device_change: Some(vec![Box::new(connect_cdrom)]),
        boot_options: Some(VirtualMachineBootOptions {
            boot_order: Some(vec![
                Box::new(VirtualMachineBootOptionsBootableCdromDevice {}),
                Box::new(VirtualMachineBootOptionsBootableDiskDevice {
                    device_key: disk_key,
                }),
                Box::new(VirtualMachineBootOptionsBootableEthernetDevice {
                    device_key: nic_key,
                }),
            ]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn build_template_config_spec(
    req: &crate::client::IsoImportRequest,
    nic_backings: Vec<Box<dyn VirtualDeviceBackingInfoTrait>>,
) -> VirtualMachineConfigSpec {
    // SCSI controller of the requested type (all compose VirtualScsiController).
    let scsi_base = || VirtualScsiController {
        virtual_controller_: VirtualController {
            virtual_device_: VirtualDevice {
                key: KEY_SCSI,
                ..Default::default()
            },
            ..Default::default()
        },
        shared_bus: VirtualScsiSharingEnum::NoSharing,
        ..Default::default()
    };
    let scsi: Box<dyn vim_rs::types::traits::VirtualDeviceTrait> = match req.disk_controller {
        DiskController::Pvscsi => Box::new(ParaVirtualScsiController {
            virtual_scsi_controller_: scsi_base(),
        }),
        DiskController::LsiLogic => Box::new(VirtualLsiLogicController {
            virtual_scsi_controller_: scsi_base(),
        }),
        DiskController::LsiLogicSas => Box::new(VirtualLsiLogicSasController {
            virtual_scsi_controller_: scsi_base(),
        }),
        DiskController::BusLogic => Box::new(VirtualBusLogicController {
            virtual_scsi_controller_: scsi_base(),
        }),
    };
    // Disk provisioning: thin, thick (lazy-zeroed), or thick eager-zeroed.
    let (thin_provisioned, eagerly_scrub) = match req.disk_provisioning {
        DiskProvisioning::Thin => (Some(true), None),
        DiskProvisioning::Thick => (Some(false), Some(false)),
        DiskProvisioning::EagerZeroed => (Some(false), Some(true)),
    };
    // Blank disk on the SCSI controller (file_operation: create).
    let disk = VirtualDisk {
        virtual_device_: VirtualDevice {
            key: KEY_DISK,
            controller_key: Some(KEY_SCSI),
            unit_number: Some(0),
            backing: Some(Box::new(VirtualDiskFlatVer2BackingInfo {
                virtual_device_file_backing_info_: VirtualDeviceFileBackingInfo {
                    // Empty -> vCenter names it in the VM's own directory.
                    file_name: String::new(),
                    ..Default::default()
                },
                disk_mode: DISK_MODE_PERSISTENT.to_string(),
                thin_provisioned,
                eagerly_scrub,
                ..Default::default()
            })),
            ..Default::default()
        },
        capacity_in_kb: req.disk_gib.max(1) * KIB_PER_GIB,
        ..Default::default()
    };
    // IDE controller + CD-ROM backed by the uploaded ISO
    let ide = VirtualIdeController {
        virtual_controller_: VirtualController {
            virtual_device_: VirtualDevice {
                key: KEY_IDE,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let cdrom = VirtualCdrom {
        virtual_device_: VirtualDevice {
            key: KEY_CDROM,
            controller_key: Some(KEY_IDE),
            unit_number: Some(0),
            backing: Some(Box::new(VirtualCdromIsoBackingInfo {
                virtual_device_file_backing_info_: VirtualDeviceFileBackingInfo {
                    file_name: req.iso_datastore_path.clone(),
                    ..Default::default()
                },
            })),
            connectable: Some(VirtualDeviceConnectInfo {
                start_connected: true,
                allow_guest_control: true,
                connected: false,
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    // NICs on their resolved zone port groups (ADR-0031: one or more).
    // Deliberately NO slot_info on any of them here — the PCI slot (default
    // 192 + index, for stable ens192/ens193/... in the guest) is pinned via
    // ExtraConfig below, and the structured slotInfo device property is
    // additionally pinned in a SEPARATE post-create ReconfigVM_Task
    // (build_nic_pci_slot_reconfigure_spec), once every other device
    // already has a concrete, auto-assigned slot. Found live: requesting a
    // slot inline here, in the SAME CreateVM_Task that also creates the
    // SCSI controller/disk/CD-ROM with no explicit slots of their own, got
    // silently reassigned — vim_rs's own doc comment on `pci_slot_number`
    // says explicit slots should be given to ALL devices in a CreateVM
    // operation, or none; `create-kairos-template.sh` (the reference this
    // logic otherwise matches) also does it as a separate `govc vm.change`
    // after `govc vm.create`, not inline.
    let build_nic = |backing: Box<dyn VirtualDeviceBackingInfoTrait>,
                     key: i32,
                     adapter: NicAdapter|
     -> Box<dyn vim_rs::types::traits::VirtualDeviceTrait> {
        let ethernet = VirtualEthernetCard {
            virtual_device_: VirtualDevice {
                key,
                backing: Some(backing),
                connectable: Some(VirtualDeviceConnectInfo {
                    start_connected: true,
                    allow_guest_control: true,
                    connected: false,
                    ..Default::default()
                }),
                ..Default::default()
            },
            // Let vCenter generate the MAC.
            address_type: Some("generated".to_string()),
            ..Default::default()
        };
        match adapter {
            NicAdapter::Vmxnet3 => Box::new(VirtualVmxnet3 {
                virtual_vmxnet_: VirtualVmxnet {
                    virtual_ethernet_card_: ethernet,
                },
                ..Default::default()
            }),
            NicAdapter::Vmxnet2 => Box::new(VirtualVmxnet2 {
                virtual_vmxnet_: VirtualVmxnet {
                    virtual_ethernet_card_: ethernet,
                },
            }),
            NicAdapter::E1000 => Box::new(VirtualE1000 {
                virtual_ethernet_card_: ethernet,
            }),
            NicAdapter::E1000e => Box::new(VirtualE1000E {
                virtual_ethernet_card_: ethernet,
            }),
        }
    };
    let nics: Vec<Box<dyn vim_rs::types::traits::VirtualDeviceTrait>> = nic_backings
        .into_iter()
        .enumerate()
        .map(|(i, backing)| {
            let key = KEY_NIC - i32::try_from(i).unwrap_or(0);
            build_nic(backing, key, req.nics[i].adapter)
        })
        .collect();
    // Firmware: vCenter takes only "bios" / "efi"; secure boot is a boot option
    // layered on EFI. `efi-secure` therefore maps to efi + secureBootEnabled.
    let (firmware_str, secure_boot) = match req.firmware {
        Firmware::Bios => (FIRMWARE_BIOS, false),
        Firmware::Efi => (FIRMWARE_EFI, false),
        Firmware::EfiSecure => (FIRMWARE_EFI, true),
    };
    // No boot order set here, deliberately — matches
    // `create-kairos-template.sh`: boot order is set in a *separate*
    // reconfigure after the VM exists (`build_boot_order_reconfigure_spec`),
    // once the CD-ROM/disk/NIC have their real (positive) device keys,
    // mirroring `create-vm.sh`'s `govc device.connect` + `device.boot
    // -order cdrom,disk,ethernet`. A boot order embedded in the *initial*
    // CreateVM_Task spec — referencing the provisional negative keys — was
    // not reliably honored (found live): EFI firmware on the freshly created
    // VM still stopped at the interactive Boot Manager menu instead of
    // auto-booting the CD-ROM.
    let boot_options = secure_boot.then(|| VirtualMachineBootOptions {
        efi_secure_boot_enabled: Some(true),
        ..Default::default()
    });

    let add = |device: Box<dyn vim_rs::types::traits::VirtualDeviceTrait>,
               create_file: bool|
     -> Box<dyn vim_rs::types::traits::VirtualDeviceConfigSpecTrait> {
        Box::new(VirtualDeviceConfigSpec {
            operation: Some(VirtualDeviceConfigSpecOperationEnum::Add),
            file_operation: create_file.then_some(VirtualDeviceConfigSpecFileOperationEnum::Create),
            device,
            ..Default::default()
        })
    };

    VirtualMachineConfigSpec {
        name: Some(req.template_name.clone()),
        guest_id: Some(req.guest_id.clone()),
        num_cp_us: Some(req.cpus),
        memory_mb: Some(req.memory_mib),
        firmware: Some(firmware_str.to_string()),
        boot_options,
        files: Some(VirtualMachineFileInfo {
            vm_path_name: Some(format!("[{}]", req.datastore)),
            ..Default::default()
        }),
        device_change: Some(
            std::iter::empty()
                .chain([add(scsi, false), add(Box::new(disk), true)])
                .chain([add(Box::new(ide), false), add(Box::new(cdrom), false)])
                .chain(nics.into_iter().map(|n| add(n, false)))
                .collect(),
        ),
        // `ethernetN.pciSlotNumber` — the mechanism actually governing
        // guest-visible PCI placement — is deliberately NOT set here. Found
        // live: even as a plain ExtraConfig entry (not the structured
        // VirtualDevice.slotInfo this file already knows not to trust),
        // setting it inline in the *same* CreateVM_Task that also creates
        // the NIC device itself still didn't stick — the live template's
        // own config read back `ethernet0.pciSlotNumber` as vCenter's
        // auto-assigned slot, not the requested one, even with no other
        // reconfigure touching it afterward. `create-kairos-template.sh`'s
        // reference sequence never attempts this either: it's a bare `govc
        // vm.create` (no `-e` at all) followed by a wholly separate `govc
        // vm.change -e "ethernet0.pciSlotNumber=192"` once the VM already
        // exists. See `build_nic_pci_slot_extra_config_reconfigure_spec`,
        // applied by `import_iso_template` in that same separate-step shape.
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "vim_tests.rs"]
mod vim_tests;
