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
use banlieue_api::banlieue::{DiskController, ProviderConnection};
use banlieue_api::common::DiskProvisioning;
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
use vim_rs::types::enums::{
    MoTypesEnum, TaskInfoStateEnum, VirtualDeviceConfigSpecFileOperationEnum,
    VirtualDeviceConfigSpecOperationEnum, VirtualScsiSharingEnum,
};
use vim_rs::types::structs::{
    DistributedVirtualSwitchPortConnection, ManagedObjectReference, ParaVirtualScsiController,
    VirtualBusLogicController, VirtualCdrom, VirtualCdromIsoBackingInfo, VirtualController,
    VirtualDevice, VirtualDeviceConfigSpec, VirtualDeviceConnectInfo,
    VirtualDeviceDeviceBackingInfo, VirtualDeviceFileBackingInfo, VirtualDevicePciBusSlotInfo,
    VirtualDisk, VirtualDiskFlatVer2BackingInfo, VirtualEthernetCard,
    VirtualEthernetCardDistributedVirtualPortBackingInfo, VirtualEthernetCardNetworkBackingInfo,
    VirtualIdeController, VirtualLsiLogicController, VirtualLsiLogicSasController,
    VirtualMachineConfigSpec, VirtualMachineFileInfo, VirtualScsiController, VirtualVmxnet,
    VirtualVmxnet3,
};
use vim_rs::types::traits::VirtualDeviceBackingInfoTrait;

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
/// CPU / memory / disk of the created template. Small — it is a clone source,
/// never itself powered on; clones override sizing.
const TEMPLATE_NUM_CPUS: i32 = 2;
const TEMPLATE_MEMORY_MB: i64 = 4096;
/// UEFI firmware — matches the maintainer's `create-kairos-template.sh`.
const TEMPLATE_FIRMWARE_EFI: &str = "efi";
const DISK_MODE_PERSISTENT: &str = "persistent";
// Negative device keys are the vSphere convention for devices being added in a
// single CreateVM/Reconfigure spec; controllers are referenced by these keys.
const KEY_SCSI: i32 = -1000;
const KEY_DISK: i32 = -1001;
const KEY_IDE: i32 = -1002;
const KEY_CDROM: i32 = -1003;
const KEY_NIC: i32 = -1004;
/// PCI slot for the NIC — matches `create-kairos-template.sh`
/// (`ethernet0.pciSlotNumber=192`) so the guest sees a stable NIC.
const NIC_PCI_SLOT: i32 = 192;
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

    async fn find_template(&self, dc: &Datacenter, name: &str) -> Result<Option<Template>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);

        let dc_moref = ManagedObjectReference {
            r#type: vim_rs::types::enums::MoTypesEnum::Datacenter,
            value: dc.moref.clone(),
        };
        let view_ref = vm
            .create_container_view(
                &dc_moref,
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

        // Idempotency + forceCreate: an existing VM/template of this name is a
        // no-op unless forceCreate, which destroys it first.
        if let Some(existing) = self
            .find_vm_moref_by_name(&req.datacenter_moref, &req.template_name)
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

        // NIC backing: distributed vDS port (portgroupKey + switchUuid) for a
        // dvPortGroup, else a standard device-name backing.
        let nic_backing: Box<dyn VirtualDeviceBackingInfoTrait> = if req.network_distributed {
            let (portgroup_key, switch_uuid) = self.resolve_dvs_port(&req.network_moref).await?;
            Box::new(VirtualEthernetCardDistributedVirtualPortBackingInfo {
                port: DistributedVirtualSwitchPortConnection {
                    switch_uuid,
                    portgroup_key: Some(portgroup_key),
                    ..Default::default()
                },
            })
        } else {
            Box::new(VirtualEthernetCardNetworkBackingInfo {
                virtual_device_device_backing_info_: VirtualDeviceDeviceBackingInfo {
                    device_name: req.network.clone(),
                    ..Default::default()
                },
                network: Some(ManagedObjectReference {
                    r#type: MoTypesEnum::Network,
                    value: req.network_moref.clone(),
                }),
                ..Default::default()
            })
        };

        // Place the template in the requested folder (created if missing), else
        // the datacenter VM-folder root.
        let target_folder = match req.folder.as_deref() {
            Some(path) if !path.is_empty() => self.ensure_folder(&vm_folder.value, path).await?,
            _ => vm_folder.value.clone(),
        };

        let config = build_template_config_spec(req, nic_backing);
        let folder = Folder::new(self.client.clone(), &target_folder);
        let task = folder
            .create_vm_task(&config, &pool, None)
            .await
            .map_err(|e| Error::Vsphere(format!("CreateVM_Task({}): {e}", req.template_name)))?;
        self.wait_for_task(&task.value, "create VM").await?;

        // Find the just-created VM and mark it as a template.
        let vm_moref = self
            .find_vm_moref_by_name(&req.datacenter_moref, &req.template_name)
            .await?
            .ok_or(Error::Vsphere(
                "created VM not found after CreateVM_Task".to_string(),
            ))?;
        VimVirtualMachine::new(self.client.clone(), &vm_moref)
            .mark_as_template()
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
}

impl VimClientImpl {
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

    /// Find a VirtualMachine (template or not) by display name within a
    /// datacenter, returning its moref. Mirrors [`VSphereClient::find_template`]
    /// but without the `config.template` filter — used to locate the VM created
    /// by `CreateVM_Task` before `MarkAsTemplate`, and any existing one to
    /// destroy on `forceCreate`.
    async fn find_vm_moref_by_name(
        &self,
        datacenter_moref: &str,
        name: &str,
    ) -> Result<Option<String>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);
        let dc_moref = ManagedObjectReference {
            r#type: MoTypesEnum::Datacenter,
            value: datacenter_moref.to_string(),
        };
        let view_ref = vm
            .create_container_view(
                &dc_moref,
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
}

/// Build the `VirtualMachineConfigSpec` for the template: an EFI VM with a
/// paravirtual SCSI controller + blank thin disk (install target), an IDE
/// CD-ROM backed by the uploaded ISO, and a vmxnet3 NIC (`nic_backing`) at PCI
/// slot 192. `files.vmPathName` points at the resolved datastore so the VM (and
/// its disk) land there. Mirrors `create-kairos-template.sh`.
fn build_template_config_spec(
    req: &crate::client::IsoImportRequest,
    nic_backing: Box<dyn VirtualDeviceBackingInfoTrait>,
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
    // vmxnet3 NIC on the zone port group, at a fixed PCI slot (192) so the
    // guest's NIC naming is stable — matches `create-kairos-template.sh`.
    let nic = VirtualVmxnet3 {
        virtual_vmxnet_: VirtualVmxnet {
            virtual_ethernet_card_: VirtualEthernetCard {
                virtual_device_: VirtualDevice {
                    key: KEY_NIC,
                    backing: Some(nic_backing),
                    slot_info: Some(Box::new(VirtualDevicePciBusSlotInfo {
                        pci_slot_number: NIC_PCI_SLOT,
                    })),
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
            },
        },
        ..Default::default()
    };

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
        num_cp_us: Some(TEMPLATE_NUM_CPUS),
        memory_mb: Some(TEMPLATE_MEMORY_MB),
        firmware: Some(TEMPLATE_FIRMWARE_EFI.to_string()),
        files: Some(VirtualMachineFileInfo {
            vm_path_name: Some(format!("[{}]", req.datastore)),
            ..Default::default()
        }),
        device_change: Some(vec![
            add(scsi, false),
            add(Box::new(disk), true),
            add(Box::new(ide), false),
            add(Box::new(cdrom), false),
            add(Box::new(nic), false),
        ]),
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "vim_tests.rs"]
mod vim_tests;
