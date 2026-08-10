// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue provider vsphere image-import` — the per-zone data path of
//! ADR-0010 / ADR-0020.
//!
//! The `VMImage` reconciler creates one Job per failure domain; each Job runs
//! this subcommand. It resolves the target zone (datacenter / cluster /
//! datastore / network) from the `Provider`, verifies the built ISO against its
//! published checksum, connects to vCenter, and asks the client to turn the ISO
//! into a template in that zone ([`VSphereClient::import_iso_template`]).
//! Nothing here reconciles or writes status — the reconciler reads the Job's
//! own success/failure and translates it into `status.perProvider[].zones[]`.
//!
//! Like the libvirt import path, the Job re-reads the `Provider` (and its
//! credentials / CA) rather than taking them on the command line: the TLS
//! material is a Secret and the CA bundle may be inline / a ConfigMap / a
//! Secret, and flattening secrets onto argv would expose them in `/proc`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use banlieue_api::banlieue::{DiskController, Provider, VMImage};
use banlieue_api::common::DiskProvisioning;
use banlieue_provider_sdk::client::build_client;
use clap::Args;
use kube::api::Api;
use tokio::fs::File;
use tracing::info;

use crate::client::vim::{build_http_client, build_upload_http_client, server_address};
use crate::client::{
    Credentials, Datastore, IsoImportRequest, VSphereClientFactory, VimClientFactory,
};

const SECRET_KEY_USERNAME: &str = "username";
const SECRET_KEY_PASSWORD: &str = "password";

/// Datastore folder the ISO is uploaded into, relative to the datastore root.
const DATASTORE_UPLOAD_DIR: &str = "banlieue-images";

/// Read size when hashing the artifact — large enough that a multi-gigabyte ISO
/// is not millions of syscalls, small enough to stay off the heap.
const HASH_CHUNK_BYTES: usize = 1024 * 1024;

/// Attribute keys the provider reconciler writes into each failure domain's
/// `attributes.raw` (see `reconciler::provider`).
const ATTR_DATACENTER: &str = "datacenter";
const ATTR_CLUSTER: &str = "cluster";

/// `spec.capabilities.storageClasses[].target` keys (ADR-0019).
const TARGET_DATASTORE: &str = "datastore";
const TARGET_DATASTORE_CLUSTER: &str = "datastoreCluster";
/// `spec.capabilities.networkClasses[].target` keys (ADR-0019).
const TARGET_PORT_GROUP: &str = "portGroup";
const TARGET_DISTRIBUTED_PORT_GROUP: &str = "distributedPortGroup";

/// Fallback vCenter `guestId` for a Linux image whose distribution we do not
/// map to a more specific value.
const DEFAULT_LINUX_GUEST_ID: &str = "otherLinux64Guest";
/// Fallback vCenter `guestId` for a non-Linux image.
const DEFAULT_GUEST_ID: &str = "otherGuest64";

/// Arguments for `banlieue provider vsphere image-import`.
///
/// Every one is set by
/// [`crate::reconciler::vmimage::build_import_job`]; exposed as flags so a
/// failed import can be reproduced by hand.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Name of the `VMImage` being imported (also the template name).
    #[arg(long)]
    pub vmimage: String,

    /// Name of the `Provider` whose vCenter to import into.
    #[arg(long)]
    pub provider: String,

    /// Namespace of that `Provider`. Not this Job's own namespace: the Job runs
    /// beside the artifacts PVC in the build namespace.
    #[arg(long)]
    pub provider_namespace: String,

    /// Failure domain (zone) to import into, matching
    /// `Provider.status.failureDomains[].name`.
    #[arg(long)]
    pub failure_domain: String,

    /// Path to the ISO to upload, inside the mounted artifacts volume.
    #[arg(long)]
    pub source: PathBuf,

    /// Datastore to upload the ISO to and create the template on. Overrides the
    /// zone's first `availableStorageClasses` target; required when the failure
    /// domain has not been enriched with reachable storage classes (ADR-0019).
    #[arg(long)]
    pub datastore: Option<String>,

    /// Expected checksum of the source ISO, `<alg>:<hex>` (sha256 or sha512).
    /// When set, the ISO is hashed before anything touches vCenter; a mismatch
    /// — or an unsupported algorithm — fails the Job closed (SEC-004).
    #[arg(long)]
    pub checksum: Option<String>,

    /// Re-upload the ISO even if it already exists on the datastore (deletes
    /// the existing file first — the datastore file API does not overwrite in
    /// place). Without it, an already-present ISO is reused. Set from
    /// `VMImage.spec.forceUpload`.
    #[arg(long, default_value_t = false)]
    pub force_upload: bool,

    /// Recreate the template even if one of this name already exists (destroys
    /// the existing template VM first). Without it, an already-present template
    /// is left in place. Set from `VMImage.spec.forceCreate`.
    #[arg(long, default_value_t = false)]
    pub force_create: bool,

    /// Template install-disk size in GiB. Set from
    /// `VMImage.spec.template.disk.size` (default 100).
    #[arg(long, default_value_t = 100)]
    pub disk_gb: i64,

    /// Disk provisioning: `thin` (default), `thick`, or `eagerZeroed`. Set from
    /// `spec.template.disk.type`.
    #[arg(long, default_value = "thin")]
    pub disk_type: DiskProvisioning,

    /// Disk controller: `pvscsi` (default), `lsiLogic`, `lsiLogicSas`,
    /// `busLogic`. Set from `spec.template.disk.controller`.
    #[arg(long, default_value = "pvscsi")]
    pub disk_controller: DiskController,

    /// Port group the template's NIC attaches to. Overrides the zone's first
    /// `availableNetworkClasses` target; set from `spec.template.network`.
    #[arg(long)]
    pub network: Option<String>,

    /// vCenter folder (path under the datacenter VM folder) to place the
    /// template in, created if missing. Set from `VMImage.spec.template.folder`.
    #[arg(long)]
    pub folder: Option<String>,
}

/// The concrete vCenter placement resolved for one failure domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZonePlan {
    /// Datacenter the target cluster lives in.
    pub datacenter: String,
    /// Compute cluster whose resource pool hosts the created VM.
    pub cluster: String,
    /// Datastore the ISO is uploaded to and the template created on.
    pub datastore: String,
    /// Port group the template's NIC attaches to.
    pub network: String,
}

/// Map a VMImage's OS to a vCenter `guestId`.
///
/// A coarse mapping — enough to set a sensible guest type on the template;
/// operators can refine per-image later. Unknown Linux distributions fall back
/// to a generic 64-bit Linux guest, non-Linux to a generic 64-bit guest.
#[must_use]
pub fn guest_id_for(os_family: &str, os_distribution: &str) -> String {
    let distro = os_distribution.to_ascii_lowercase();
    match os_family.to_ascii_lowercase().as_str() {
        "linux" => {
            if distro.contains("rhel") || distro.contains("red hat") || distro.contains("redhat") {
                "rhel9_64Guest".to_string()
            } else if distro.contains("ubuntu") {
                "ubuntu64Guest".to_string()
            } else if distro.contains("debian") {
                "debian11_64Guest".to_string()
            } else if distro.contains("fedora") || distro.contains("coreos") {
                "fedora64Guest".to_string()
            } else {
                DEFAULT_LINUX_GUEST_ID.to_string()
            }
        }
        "windows" => "windows2019srv_64Guest".to_string(),
        _ => DEFAULT_GUEST_ID.to_string(),
    }
}

/// Resolve the concrete vСenter placement for `failure_domain` from a
/// `Provider`.
///
/// Pure, so the zone-selection rules are unit-testable without kube or vCenter.
/// Datacenter and cluster always come from the failure domain's discovered
/// `attributes.raw`. The datastore and network come from the explicit
/// `datastore_override` / `network_override` when given, else from the **first**
/// available storage/network class in that zone resolved through
/// `spec.capabilities` — the override path lets operators import into a zone
/// whose `availableStorageClasses` / `availableNetworkClasses` have not been
/// enriched yet (ADR-0019).
///
/// # Errors
/// A human-readable message when the failure domain is absent, carries no
/// datacenter/cluster, or (absent an override) has no reachable storage/network
/// class to import into — the Job is unattended, so its error text is the whole
/// diagnostic.
pub fn resolve_zone(
    provider: &Provider,
    failure_domain: &str,
    datastore_override: Option<&str>,
    network_override: Option<&str>,
) -> Result<ZonePlan> {
    let status = provider
        .status
        .as_ref()
        .ok_or_else(|| anyhow!("Provider has no status yet"))?;
    let fd = status
        .failure_domains
        .iter()
        .find(|f| f.name == failure_domain)
        .ok_or_else(|| {
            anyhow!(
                "failure domain {failure_domain:?} not found in Provider.status.failureDomains[]"
            )
        })?;

    let datacenter = fd
        .attributes
        .raw
        .get(ATTR_DATACENTER)
        .cloned()
        .ok_or_else(|| anyhow!("failure domain {failure_domain:?} has no datacenter attribute"))?;
    let cluster = fd
        .attributes
        .raw
        .get(ATTR_CLUSTER)
        .cloned()
        .ok_or_else(|| anyhow!("failure domain {failure_domain:?} has no cluster attribute"))?;

    let datastore = match datastore_override {
        Some(ds) => ds.to_string(),
        None => {
            let storage_class = fd.attributes.available_storage_classes.first().ok_or_else(|| {
                anyhow!(
                    "failure domain {failure_domain:?} has no available storage class; pass --datastore to import into a specific datastore"
                )
            })?;
            resolve_storage_target(provider, storage_class).ok_or_else(|| {
                anyhow!("storage class {storage_class:?} has no datastore/datastoreCluster target")
            })?
        }
    };

    let network = match network_override {
        Some(net) => net.to_string(),
        None => {
            let network_class = fd.attributes.available_network_classes.first().ok_or_else(|| {
                anyhow!(
                    "failure domain {failure_domain:?} has no available network class; pass --network to import onto a specific port group"
                )
            })?;
            resolve_network_target(provider, network_class).ok_or_else(|| {
                anyhow!(
                    "network class {network_class:?} has no portGroup/distributedPortGroup target"
                )
            })?
        }
    };

    Ok(ZonePlan {
        datacenter,
        cluster,
        datastore,
        network,
    })
}

/// The datastore (or datastore-cluster) name a declared storage class maps to.
fn resolve_storage_target(provider: &Provider, class_name: &str) -> Option<String> {
    let target = &provider
        .spec
        .capabilities
        .storage_classes
        .iter()
        .find(|c| c.name == class_name)?
        .target;
    target
        .get(TARGET_DATASTORE)
        .or_else(|| target.get(TARGET_DATASTORE_CLUSTER))
        .cloned()
}

/// The port-group (standard or distributed) name a declared network class maps to.
fn resolve_network_target(provider: &Provider, class_name: &str) -> Option<String> {
    let target = &provider
        .spec
        .capabilities
        .network_classes
        .iter()
        .find(|c| c.name == class_name)?
        .target;
    target
        .get(TARGET_PORT_GROUP)
        .or_else(|| target.get(TARGET_DISTRIBUTED_PORT_GROUP))
        .cloned()
}

/// Size of the source ISO. A zero-byte file means the build produced nothing.
async fn source_length(path: &Path) -> Result<u64> {
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("cannot read {}", path.display()))?;
    let len = meta.len();
    if len == 0 {
        bail!("{} is empty; the build produced no ISO", path.display());
    }
    Ok(len)
}

/// Verify the artifact against an expected `<alg>:<hex>` checksum.
///
/// Streams the file so multi-gigabyte ISOs never sit in memory. Fails
/// **closed**: an unsupported algorithm is an error, not a skip (SEC-004).
///
/// # Errors
/// When the algorithm is unsupported, the file cannot be read, or the digest
/// does not match.
pub async fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    use sha2::Digest;
    use tokio::io::AsyncReadExt;

    let (alg, expected_hex) = expected
        .split_once(':')
        .ok_or_else(|| anyhow!("--checksum expected `<alg>:<hex>`, got {expected:?}"))?;

    let mut file = File::open(path)
        .await
        .with_context(|| format!("cannot read {}", path.display()))?;
    let mut buf = vec![0u8; HASH_CHUNK_BYTES];

    let actual_hex = match alg {
        "sha256" => {
            let mut h = sha2::Sha256::new();
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            format!("{:x}", h.finalize())
        }
        "sha512" => {
            let mut h = sha2::Sha512::new();
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            format!("{:x}", h.finalize())
        }
        other => bail!(
            "unsupported checksum algorithm {other:?} (supported: sha256, sha512); \
             refusing to import an unverified artifact"
        ),
    };

    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        bail!(
            "checksum mismatch for {}: expected {expected}, computed {alg}:{actual_hex}; \
             refusing to import a corrupted or substituted artifact",
            path.display()
        );
    }
    info!(path = %path.display(), algorithm = %alg, "ISO checksum verified");
    Ok(())
}

/// Read the Provider's credentials Secret (`username` / `password`).
async fn read_credentials(
    client: &kube::Client,
    namespace: &str,
    provider: &Provider,
) -> Result<Credentials> {
    let secret_name = &provider.spec.connection.credentials_ref.name;
    let api: Api<k8s_openapi::api::core::v1::Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api
        .get(secret_name)
        .await
        .with_context(|| format!("reading credentials Secret {secret_name:?}"))?;
    let data = secret.data.unwrap_or_default();
    let username = data
        .get(SECRET_KEY_USERNAME)
        .ok_or_else(|| anyhow!("secret.data.username missing"))?;
    let password = data
        .get(SECRET_KEY_PASSWORD)
        .ok_or_else(|| anyhow!("secret.data.password missing"))?;
    Ok(Credentials {
        username: String::from_utf8(username.0.clone())
            .map_err(|_| anyhow!("secret.data.username is not UTF-8"))?,
        password: String::from_utf8(password.0.clone())
            .map_err(|_| anyhow!("secret.data.password is not UTF-8"))?,
    })
}

/// Run one per-zone ISO import to completion.
///
/// # Errors
/// Any failure to read the `Provider` / `VMImage`, resolve credentials or CA,
/// verify the ISO, connect to vCenter, or create the template. Every path exits
/// non-zero so the Job records a failure the reconciler can surface.
pub async fn run(args: ImportArgs) -> Result<()> {
    info!(
        vmimage = %args.vmimage,
        provider = %args.provider,
        failure_domain = %args.failure_domain,
        source = %args.source.display(),
        "starting vSphere ISO image import"
    );

    // Fail before contacting vCenter on a bad artifact.
    let _len = source_length(&args.source).await?;
    if let Some(expected) = args.checksum.as_deref() {
        verify_checksum(&args.source, expected).await?;
    }

    let client = build_client().await.context("constructing kube client")?;

    let provider_api: Api<Provider> = Api::namespaced(client.clone(), &args.provider_namespace);
    let provider = provider_api
        .get(&args.provider)
        .await
        .with_context(|| format!("reading Provider {}", args.provider))?;

    // VMImage is cluster-scoped; read it for the OS -> guestId mapping.
    let image_api: Api<VMImage> = Api::all(client.clone());
    let image = image_api
        .get(&args.vmimage)
        .await
        .with_context(|| format!("reading VMImage {}", args.vmimage))?;
    let guest_id = guest_id_for(
        &format!("{:?}", image.spec.os_family).to_ascii_lowercase(),
        &image.spec.os_distribution,
    );

    let plan = resolve_zone(
        &provider,
        &args.failure_domain,
        args.datastore.as_deref(),
        args.network.as_deref(),
    )?;

    let creds = read_credentials(&client, &args.provider_namespace, &provider).await?;
    let ca_bundle_pem = banlieue_provider_sdk::ca_bundle::resolve(
        &client,
        &args.provider_namespace,
        &provider.spec.connection.ca_bundle,
    )
    .await
    .context("resolving vCenter CA bundle")?
    .map(|pem| String::from_utf8_lossy(&pem).into_owned());

    let vsphere = VimClientFactory::new();
    let vim = vsphere
        .build(&provider.spec.connection, &creds, ca_bundle_pem.as_deref())
        .await
        .context("connecting to vCenter")?;

    // Resolve the zone's datacenter+cluster to morefs and turn the requested
    // datastore-or-datastore-cluster into a concrete member datastore (an SDRS
    // datastore cluster / StoragePod cannot be a datastore-upload target).
    let datacenter = vim
        .list_datacenters()
        .await
        .context("listing datacenters")?
        .into_iter()
        .find(|d| d.name == plan.datacenter)
        .ok_or_else(|| anyhow!("datacenter {:?} not found in vCenter", plan.datacenter))?;
    let cluster = vim
        .list_clusters(&datacenter)
        .await
        .context("listing clusters")?
        .into_iter()
        .find(|c| c.name == plan.cluster)
        .ok_or_else(|| {
            anyhow!(
                "cluster {:?} not found in datacenter {:?}",
                plan.cluster,
                plan.datacenter
            )
        })?;
    let datastores = vim
        .list_datastores(&cluster)
        .await
        .context("listing datastores")?;

    // Resolve the concrete member datastore. Idempotency first: if the ISO is
    // already on ANY member of the (datastore-)cluster, reuse that member so a
    // re-run neither re-uploads nor scatters copies across members. Only a
    // fresh placement (or --force-upload) falls back to the emptiest member.
    let candidates = candidate_datastores(&datastores, &plan.datastore)?;
    let host = server_address(&provider.spec.connection.endpoint);
    let remote_path = format!("{DATASTORE_UPLOAD_DIR}/{}.iso", args.vmimage);
    let probe = build_http_client(
        ca_bundle_pem.as_deref(),
        provider.spec.connection.insecure_skip_tls_verify,
    )
    .map_err(|e| anyhow!("{e}"))?;
    let mut chosen: Option<String> = None;
    if !args.force_upload {
        for d in &candidates {
            if datastore_file_exists(
                &probe,
                host,
                &plan.datacenter,
                &d.name,
                &remote_path,
                &creds,
            )
            .await?
            {
                info!(datastore = %d.name, "ISO already present on this cluster member; reusing it");
                chosen = Some(d.name.clone());
                break;
            }
        }
    }
    let datastore = chosen.unwrap_or_else(|| pick_emptiest(&candidates).name.clone());
    info!(requested = %plan.datastore, resolved = %datastore, "resolved concrete datastore");

    // Auto-create the upload directory so the datastore PUT has a target.
    vim.ensure_datastore_dir(&datacenter.moref, &datastore, DATASTORE_UPLOAD_DIR)
        .await
        .context("creating datastore upload directory")?;

    // Push/upload the ISO to the zone's datastore (the vCenter datastore HTTP
    // API — what `govc datastore.upload` does). Idempotent: an already-present
    // ISO is reused unless `--force-upload`, which deletes it first (the file
    // API does not overwrite in place).
    let iso_datastore_path = upload_iso_to_datastore(
        &provider.spec.connection.endpoint,
        &creds,
        ca_bundle_pem.as_deref(),
        provider.spec.connection.insecure_skip_tls_verify,
        &plan.datacenter,
        &datastore,
        &args.source,
        &format!("{}.iso", args.vmimage),
        args.force_upload,
    )
    .await?;

    // Resolve the port group moref (+ whether it's a distributed vDS one) for
    // the template NIC backing.
    let network = vim
        .list_networks(&cluster)
        .await
        .context("listing networks")?
        .into_iter()
        .find(|n| n.name == plan.network)
        .ok_or_else(|| {
            anyhow!(
                "network {:?} not reachable from cluster {:?}",
                plan.network,
                plan.cluster
            )
        })?;

    let req = IsoImportRequest {
        datacenter: plan.datacenter,
        datacenter_moref: datacenter.moref.clone(),
        cluster: plan.cluster,
        cluster_moref: cluster.moref.clone(),
        datastore,
        network: plan.network,
        network_moref: network.moref,
        network_distributed: network.distributed,
        disk_gib: args.disk_gb,
        disk_provisioning: args.disk_type.clone(),
        disk_controller: args.disk_controller,
        folder: args.folder.clone(),
        iso_datastore_path,
        template_name: args.vmimage.clone(),
        guest_id,
        force_create: args.force_create,
    };
    let resolved = vim
        .import_iso_template(&req)
        .await
        .with_context(|| format!("importing ISO as template in zone {}", args.failure_domain))?;

    info!(template = %resolved, "vSphere ISO import complete");
    Ok(())
}

/// Resolve a requested datastore name — which may be an individual datastore or
/// an SDRS datastore-cluster (StoragePod) name — to a concrete member datastore
/// reachable from the zone's cluster.
///
/// An exact datastore-name match wins. Otherwise the name is treated as a
/// datastore-cluster and the lexicographically-first member (deterministic) is
/// chosen. This lets both `--datastore <DSC>` and the declarative
/// `storageClasses: { datastoreCluster: … }` path resolve to a real datastore.
///
/// # Errors
/// When `target` matches neither a datastore nor a datastore-cluster reachable
/// from the cluster.
pub fn resolve_concrete_datastore(datastores: &[Datastore], target: &str) -> Result<String> {
    let candidates = candidate_datastores(datastores, target)?;
    Ok(pick_emptiest(&candidates).name.clone())
}

/// Candidate datastores a `target` (a datastore name, or an SDRS
/// datastore-cluster name) resolves to: the single named datastore, or all
/// members of the datastore-cluster. Pure.
///
/// # Errors
/// When `target` matches neither a datastore nor a datastore-cluster reachable
/// from the zone's cluster.
pub fn candidate_datastores(datastores: &[Datastore], target: &str) -> Result<Vec<Datastore>> {
    if let Some(d) = datastores.iter().find(|d| d.name == target) {
        return Ok(vec![d.clone()]);
    }
    let members: Vec<Datastore> = datastores
        .iter()
        .filter(|d| d.datastore_cluster.as_deref() == Some(target))
        .cloned()
        .collect();
    if members.is_empty() {
        bail!(
            "datastore {target:?} is neither a datastore nor a datastore-cluster reachable from \
             the zone's cluster; reachable datastores: [{}]",
            datastores
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(members)
}

/// Pick the emptiest datastore (most free space) for a fresh placement. Ties
/// (or unknown free space) break to the lexicographically-smallest name so the
/// choice is deterministic. Panics only on an empty slice — callers pass a
/// non-empty [`candidate_datastores`] result.
#[must_use]
pub fn pick_emptiest(candidates: &[Datastore]) -> &Datastore {
    candidates
        .iter()
        .max_by(|a, b| {
            a.free_space_bytes
                .unwrap_or(i64::MIN)
                .cmp(&b.free_space_bytes.unwrap_or(i64::MIN))
                // reversed name so equal free space prefers the smaller name
                .then_with(|| b.name.cmp(&a.name))
        })
        .expect("candidate_datastores returns a non-empty list")
}

/// Datastore-file HTTP URL: `https://<host>/folder/<path>?dcPath=&dsName=`.
fn datastore_file_url(
    host: &str,
    remote_path: &str,
    datacenter: &str,
    datastore: &str,
) -> Result<reqwest::Url> {
    let base = format!("https://{host}/folder/{remote_path}");
    reqwest::Url::parse_with_params(&base, &[("dcPath", datacenter), ("dsName", datastore)])
        .with_context(|| format!("building datastore URL from {base}"))
}

/// True when a file already exists at the datastore path (HEAD → 2xx). An
/// inconclusive request (network / auth hiccup) is treated as "absent" so the
/// caller falls through to a normal upload rather than failing the import.
async fn datastore_file_exists(
    client: &reqwest::Client,
    host: &str,
    datacenter: &str,
    datastore: &str,
    remote_path: &str,
    creds: &Credentials,
) -> Result<bool> {
    let url = datastore_file_url(host, remote_path, datacenter, datastore)?;
    match client
        .head(url)
        .basic_auth(&creds.username, Some(&creds.password))
        .send()
        .await
    {
        Ok(r) => Ok(r.status().is_success()),
        Err(_) => Ok(false),
    }
}

/// Upload a local ISO to a vCenter datastore over the datastore HTTP file API
/// and return its `[datastore] folder/file.iso` path.
///
/// This is the vim_rs-independent half of the push: vim_rs 0.5 has no
/// datastore file-transfer API, so banlieue does the same authenticated
/// `PUT https://<host>/folder/<path>?dcPath=&dsName=` that `govc
/// datastore.upload` performs, reusing the Provider's credentials + CA bundle.
///
/// # Errors
/// When the file cannot be read, the client cannot be built, the request fails,
/// or vCenter returns a non-success status.
#[allow(clippy::too_many_arguments)]
async fn upload_iso_to_datastore(
    endpoint: &str,
    creds: &Credentials,
    ca_bundle_pem: Option<&str>,
    insecure: bool,
    datacenter: &str,
    datastore: &str,
    local_path: &Path,
    remote_name: &str,
    force_upload: bool,
) -> Result<String> {
    let host = server_address(endpoint);
    let remote_path = format!("{DATASTORE_UPLOAD_DIR}/{remote_name}");
    let resolved = format!("[{datastore}] {remote_path}");
    let base = format!("https://{host}/folder/{remote_path}");
    // Build the query with proper percent-encoding (datacenter/datastore names
    // may contain spaces). `parse_with_params` is the reqwest-re-exported `url`
    // crate's encoder.
    let url =
        reqwest::Url::parse_with_params(&base, &[("dcPath", datacenter), ("dsName", datastore)])
            .with_context(|| format!("building datastore upload URL from {base}"))?;
    let url_str = url.to_string();

    // A short-timeout client for the existence check / delete (not the upload).
    let ctl = build_http_client(ca_bundle_pem, insecure).map_err(|e| anyhow!("{e}"))?;
    if force_upload {
        // The datastore file API does not overwrite in place — delete any
        // existing file first. A 404 (nothing there) is fine.
        match ctl
            .delete(url.clone())
            .basic_auth(&creds.username, Some(&creds.password))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() || r.status().as_u16() == 404 => {
                info!(url = %url_str, "forceUpload: removed any existing ISO before upload");
            }
            Ok(r) => bail!("deleting existing ISO at {url_str} returned {}", r.status()),
            Err(e) => return Err(anyhow!(e)).context("deleting existing ISO before re-upload"),
        }
    } else {
        // Skip the (multi-GB) upload if the ISO is already present.
        if let Ok(r) = ctl
            .head(url.clone())
            .basic_auth(&creds.username, Some(&creds.password))
            .send()
            .await
            && r.status().is_success()
        {
            info!(url = %url_str, "ISO already present on datastore; skipping upload (set forceUpload to replace)");
            return Ok(resolved);
        }
    }

    // Stream the ISO from disk rather than buffering multi-gigabyte files in
    // memory. `Content-Length` is set explicitly from the file size so the PUT
    // carries a known length (the vCenter datastore file API wants that, not
    // chunked transfer-encoding), while the body is fed lazily from the file.
    let file = File::open(local_path)
        .await
        .with_context(|| format!("opening {}", local_path.display()))?;
    let len = file
        .metadata()
        .await
        .with_context(|| format!("stat {}", local_path.display()))?
        .len();
    info!(
        url = %url_str, dc = %datacenter, ds = %datastore, bytes = len,
        "uploading ISO to datastore (streaming)"
    );
    let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));

    let http = build_upload_http_client(ca_bundle_pem, insecure).map_err(|e| anyhow!("{e}"))?;
    let resp = http
        .put(url)
        .header(reqwest::header::CONTENT_LENGTH, len)
        .basic_auth(&creds.username, Some(&creds.password))
        .body(body)
        .send()
        .await
        .context("datastore upload request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("datastore upload to {url_str} returned {status}: {body}");
    }
    Ok(resolved)
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;
