// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The HTTPS implementation of [`ProxmoxApi`] (ADR-0074).

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Method;

use crate::api::ProxmoxApi;
use crate::error::{Error, Result};
use crate::token::ApiToken;
use crate::types::{
    CloneParams, ClusterVm, GuestInterface, GuestInterfaces, NetworkIface, Node, Storage,
    TaskStatus, Version, VmConfig, VmId, VmStatus, Volume,
};
use crate::upid::Upid;
use crate::wire::{Params, api_message, decode_data, encode_segment};

/// Path prefix of the JSON API.
const API_PREFIX: &str = "/api2/json";
/// Bound on establishing a connection (ADR-0074 Decision 8).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on one whole request, so a wedged `pveproxy` cannot stall a reconcile.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest response body accepted. Proxmox's biggest reply here is a cluster
/// inventory, well under this; the cap exists for a hostile endpoint.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = RESPONSE_CAP_MIB * BYTES_PER_MIB;
/// Size of the default cap, in MiB.
const RESPONSE_CAP_MIB: usize = 16;
const BYTES_PER_MIB: usize = 1024 * 1024;
/// Multipart boundary; lengthened until it does not occur in the payload.
const MULTIPART_BOUNDARY: &str = "banlieue-proxmox-boundary";
/// Room reserved for the multipart framing around the payload.
const MULTIPART_OVERHEAD: usize = 512;

/// How to reach one Proxmox endpoint.
#[derive(Clone)]
pub struct ClientConfig {
    /// `https://host:8006`. The scheme is required.
    pub endpoint: String,
    /// The API token.
    pub token: ApiToken,
    /// PEM CA bundle to trust in addition to the system roots, typically the
    /// node's `/etc/pve/pve-root-ca.pem`.
    pub ca_bundle_pem: Option<String>,
    /// Skip certificate verification. Gated by admission policy upstream.
    pub insecure_skip_tls_verify: bool,
    /// See [`DEFAULT_CONNECT_TIMEOUT`].
    pub connect_timeout: Duration,
    /// See [`DEFAULT_REQUEST_TIMEOUT`].
    pub request_timeout: Duration,
    /// See [`DEFAULT_MAX_RESPONSE_BYTES`].
    pub max_response_bytes: usize,
}

impl ClientConfig {
    /// Verified TLS against the system roots, default timeouts.
    #[must_use]
    pub fn new(endpoint: &str, token: ApiToken) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            token,
            ca_bundle_pem: None,
            insecure_skip_tls_verify: false,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("endpoint", &self.endpoint)
            .field("token", &self.token)
            .field("ca_bundle", &self.ca_bundle_pem.is_some())
            .field("insecure_skip_tls_verify", &self.insecure_skip_tls_verify)
            .finish_non_exhaustive()
    }
}

/// Proxmox VE over HTTPS with an API token.
///
/// Needs a process-default rustls crypto provider (ADR-0009), which the
/// binary installs before any TLS use.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: ApiToken,
    request_timeout: Duration,
    max_response_bytes: usize,
}

fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

impl Client {
    /// Build a client. Nothing is sent.
    ///
    /// # Errors
    /// [`Error::Config`] for an endpoint without an http(s) scheme or an
    /// unparseable CA bundle — a bad CA must fail loudly, never fall back to
    /// trusting less or more than asked.
    pub fn new(config: ClientConfig) -> Result<Self> {
        let endpoint = config.endpoint.trim().trim_end_matches('/');
        if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
            return Err(Error::Config(format!(
                "endpoint {endpoint:?} needs an http:// or https:// scheme"
            )));
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            // Never follow a redirect: the Authorization header would go with it.
            .redirect(reqwest::redirect::Policy::none());
        if let Some(pem) = &config.ca_bundle_pem {
            let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
                .map_err(|e| Error::Config(format!("caBundle: {e}")))?;
            if certs.is_empty() {
                return Err(Error::Config("caBundle holds no certificates".to_string()));
            }
            for cert in certs {
                builder = builder.add_root_certificate(cert);
            }
        }
        if config.insecure_skip_tls_verify {
            builder = builder
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }
        let http = builder.build().map_err(|e| Error::Config(chain(&e)))?;
        Ok(Self {
            http,
            base: format!("{endpoint}{API_PREFIX}"),
            token: config.token,
            request_timeout: config.request_timeout,
            max_response_bytes: config.max_response_bytes,
        })
    }

    /// `https://host:8006/api2/json`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        params: &Params,
        body: Option<(String, Vec<u8>)>,
    ) -> Result<String> {
        let mut url = format!("{}{path}", self.base);
        let carries_body = method == Method::POST || method == Method::PUT;
        if !carries_body && !params.is_empty() {
            url.push('?');
            url.push_str(&params.encode());
        }
        tracing::debug!(%method, path, token = self.token.id(), "proxmox request");
        let mut req = self
            .http
            .request(method, &url)
            .header(reqwest::header::AUTHORIZATION, self.token.authorization());
        if let Some((content_type, bytes)) = body {
            req = req
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(bytes);
        } else if carries_body {
            req = req
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(params.encode());
        }
        let resp = req.send().await.map_err(|e| self.transport(&e))?;
        let status = resp.status();
        let reason = resp
            .extensions()
            .get::<hyper::ext::ReasonPhrase>()
            .and_then(|r| std::str::from_utf8(r.as_bytes()).ok())
            .map(str::to_string)
            .or_else(|| status.canonical_reason().map(str::to_string))
            .unwrap_or_default();
        let text = self.read_body(resp).await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                message: api_message(&reason, &text),
            });
        }
        Ok(text)
    }

    /// Read a body, refusing more than `max_response_bytes`: checked against
    /// the declared length first, then while streaming, since a server may
    /// lie or omit it.
    async fn read_body(&self, mut resp: reqwest::Response) -> Result<String> {
        let limit = self.max_response_bytes;
        let too_large = Error::ResponseTooLarge { limit };
        if resp.content_length().is_some_and(|n| n > limit as u64) {
            return Err(too_large);
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.transport(&e))? {
            if body.len() + chunk.len() > limit {
                return Err(too_large);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    fn transport(&self, e: &reqwest::Error) -> Error {
        if e.is_timeout() {
            return Error::Timeout(self.request_timeout);
        }
        Error::Transport(chain(e))
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str, params: &Params) -> Result<T> {
        decode_data(&self.call(Method::GET, path, params, None).await?)
    }

    async fn task(&self, method: Method, path: &str, params: &Params) -> Result<Upid> {
        let body = self.call(method, path, params, None).await?;
        Upid::parse(&decode_data::<String>(&body)?)
    }
}

fn qemu(node: &str, vmid: u32) -> String {
    format!("/nodes/{}/qemu/{vmid}", encode_segment(node))
}

/// Refuse a filename that could break out of a multipart header or the
/// storage's directory.
fn check_filename(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name.contains(['"', '\r', '\n', '/', '\\', '\0'])
        || name.starts_with('.');
    if bad {
        return Err(Error::Config(format!("unsafe upload filename {name:?}")));
    }
    Ok(())
}

fn multipart(filename: &str, data: &[u8]) -> (String, Vec<u8>) {
    let mut boundary = MULTIPART_BOUNDARY.to_string();
    while data
        .windows(boundary.len())
        .any(|w| w == boundary.as_bytes())
    {
        boundary.push('x');
    }
    let mut body = Vec::with_capacity(data.len() + MULTIPART_OVERHEAD);
    let push = |body: &mut Vec<u8>, s: &str| body.extend_from_slice(s.as_bytes());
    push(
        &mut body,
        &format!("--{boundary}\r\nContent-Disposition: form-data; name=\"content\"\r\n\r\niso\r\n"),
    );
    push(
        &mut body,
        &format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"filename\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        ),
    );
    body.extend_from_slice(data);
    push(&mut body, &format!("\r\n--{boundary}--\r\n"));
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[async_trait]
impl ProxmoxApi for Client {
    async fn version(&self) -> Result<Version> {
        self.get("/version", &Params::new()).await
    }

    async fn list_nodes(&self) -> Result<Vec<Node>> {
        self.get("/nodes", &Params::new()).await
    }

    async fn cluster_vms(&self) -> Result<Vec<ClusterVm>> {
        self.get("/cluster/resources", &Params::new().set("type", "vm"))
            .await
    }

    async fn next_id(&self) -> Result<VmId> {
        self.get("/cluster/nextid", &Params::new()).await
    }

    async fn node_storage(&self, node: &str) -> Result<Vec<Storage>> {
        self.get(
            &format!("/nodes/{}/storage", encode_segment(node)),
            &Params::new(),
        )
        .await
    }

    async fn node_networks(&self, node: &str) -> Result<Vec<NetworkIface>> {
        self.get(
            &format!("/nodes/{}/network", encode_segment(node)),
            &Params::new(),
        )
        .await
    }

    async fn storage_content(&self, node: &str, storage: &str) -> Result<Vec<Volume>> {
        let path = format!(
            "/nodes/{}/storage/{}/content",
            encode_segment(node),
            encode_segment(storage)
        );
        self.get(&path, &Params::new()).await
    }

    async fn clone_vm(&self, node: &str, template: u32, params: &CloneParams) -> Result<Upid> {
        self.task(
            Method::POST,
            &format!("{}/clone", qemu(node, template)),
            &params.params(),
        )
        .await
    }

    async fn vm_config(&self, node: &str, vmid: u32) -> Result<VmConfig> {
        self.get(&format!("{}/config", qemu(node, vmid)), &Params::new())
            .await
    }

    async fn set_vm_config(&self, node: &str, vmid: u32, params: &Params) -> Result<()> {
        let body = self
            .call(
                Method::PUT,
                &format!("{}/config", qemu(node, vmid)),
                params,
                None,
            )
            .await?;
        decode_data(&body)
    }

    async fn resize_disk(
        &self,
        node: &str,
        vmid: u32,
        disk: &str,
        size: &str,
    ) -> Result<Option<Upid>> {
        let params = Params::new().set("disk", disk).set("size", size);
        let body = self
            .call(
                Method::PUT,
                &format!("{}/resize", qemu(node, vmid)),
                &params,
                None,
            )
            .await?;
        decode_data::<Option<String>>(&body)?
            .map(|u| Upid::parse(&u))
            .transpose()
    }

    async fn start_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        self.task(
            Method::POST,
            &format!("{}/status/start", qemu(node, vmid)),
            &Params::new(),
        )
        .await
    }

    async fn stop_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        self.task(
            Method::POST,
            &format!("{}/status/stop", qemu(node, vmid)),
            &Params::new(),
        )
        .await
    }

    async fn shutdown_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        self.task(
            Method::POST,
            &format!("{}/status/shutdown", qemu(node, vmid)),
            &Params::new(),
        )
        .await
    }

    async fn vm_status(&self, node: &str, vmid: u32) -> Result<VmStatus> {
        self.get(
            &format!("{}/status/current", qemu(node, vmid)),
            &Params::new(),
        )
        .await
    }

    async fn delete_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        let params = Params::new()
            .flag("purge", true)
            .flag("destroy-unreferenced-disks", true);
        self.task(Method::DELETE, &qemu(node, vmid), &params).await
    }

    async fn upload_iso(
        &self,
        node: &str,
        storage: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<Upid> {
        check_filename(filename)?;
        let path = format!(
            "/nodes/{}/storage/{}/upload",
            encode_segment(node),
            encode_segment(storage)
        );
        let body = self
            .call(
                Method::POST,
                &path,
                &Params::new(),
                Some(multipart(filename, &data)),
            )
            .await?;
        Upid::parse(&decode_data::<String>(&body)?)
    }

    async fn delete_volume(&self, node: &str, storage: &str, volid: &str) -> Result<Upid> {
        let path = format!(
            "/nodes/{}/storage/{}/content/{}",
            encode_segment(node),
            encode_segment(storage),
            encode_segment(volid)
        );
        self.task(Method::DELETE, &path, &Params::new()).await
    }

    async fn task_status(&self, upid: &Upid) -> Result<TaskStatus> {
        let path = format!(
            "/nodes/{}/tasks/{}/status",
            encode_segment(upid.node()),
            encode_segment(upid.as_str())
        );
        self.get(&path, &Params::new()).await
    }

    async fn agent_interfaces(&self, node: &str, vmid: u32) -> Result<Vec<GuestInterface>> {
        let r: GuestInterfaces = self
            .get(
                &format!("{}/agent/network-get-interfaces", qemu(node, vmid)),
                &Params::new(),
            )
            .await?;
        Ok(r.result)
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
