// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The HTTP client for Azure Resource Manager (ADR-0164 Decision 2).
//!
//! [`ArmClient`] implements [`ArmApi`] over `reqwest`. It never blocks on a
//! long-running operation: it returns the operation's handle for the
//! provider to store and poll on a later reconcile. Every URL that carries
//! the bearer token is either built here from checked path segments or
//! checked to sit under the configured endpoint (operation URLs, `nextLink`,
//! and handles read back from a CR's status).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Method;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde_json::Value;

use crate::api::{ArmApi, DiskAccess, PowerAction, PublishApi};
use crate::auth::TokenProvider;
use crate::error::{Error, Result, classify, error_code_and_message, operation_failure};
use crate::gallery::{
    GalleryImageDefinition, GalleryImageVersion, GalleryImageVersionRequest,
    ImageDefinitionRequest, ManagedDisk, Snapshot, SnapshotRequest, UploadDiskRequest,
    check_version_name, parse_access_sas, parse_gallery_image_definition,
    parse_gallery_image_version, parse_gallery_image_versions, parse_managed_disk, parse_snapshot,
};
use crate::operation::{
    OperationHandle, OperationKind, OperationStatus, RETRY_AFTER_HEADER, ensure_under_endpoint,
    is_trusted_base, parse_operation_status,
};
use crate::retry::parse_retry_after;
use crate::types::{
    InstanceView, ResourceSku, TaggedResource, Usage, parse_instance_view, parse_resource_skus,
    parse_resources, parse_usages, parse_vm_tags, primary_private_ip,
};
use crate::upload::SasUrl;
use crate::vm::{NicRequest, VmRequest};

/// Public Azure's Resource Manager.
pub const DEFAULT_ARM_ENDPOINT: &str = "https://management.azure.com";
/// `Microsoft.Compute/virtualMachines`.
pub const COMPUTE_API_VERSION: &str = "2024-07-01";
/// `Microsoft.Network/networkInterfaces`.
pub const NETWORK_API_VERSION: &str = "2024-05-01";
/// `Microsoft.Compute/disks`.
pub const DISK_API_VERSION: &str = "2024-03-02";
/// `Microsoft.Compute/skus`.
pub const SKU_API_VERSION: &str = "2021-07-01";
/// `Microsoft.Compute/locations/{l}/usages`.
pub const USAGE_API_VERSION: &str = "2024-07-01";
/// `resourceGroups/{rg}/resources`.
pub const RESOURCES_API_VERSION: &str = "2021-04-01";
/// `Microsoft.Compute/galleries` (images and versions): the first version
/// with `safetyProfile.blockDeletionBeforeEndOfLife` (ADR-0162).
pub const GALLERY_API_VERSION: &str = "2024-03-03";

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// The resource SKU list for one region is a few MiB; this leaves room.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Stops a `nextLink` loop that never ends.
const MAX_PAGES: usize = 50;
const CONTENT_TYPE_JSON: &str = "application/json";
const BEARER_PREFIX: &str = "Bearer ";
const USER_AGENT: &str = concat!("banlieue-azure/", env!("CARGO_PKG_VERSION"));
const CODE_INVALID_NAME: &str = "InvalidName";
const CODE_RESOURCE_GROUP_NOT_FOUND: &str = "ResourceGroupNotFound";
const ACCESS_WRITE: &str = "Write";
const HTTP_OK: u16 = 200;
const HTTP_SUCCESS_MIN: u16 = 200;
const HTTP_SUCCESS_END: u16 = 300;
const HTTP_ACCEPTED: u16 = 202;
const HTTP_NO_CONTENT: u16 = 204;
const HTTP_UNAUTHORIZED: u16 = 401;
const HTTP_NOT_FOUND: u16 = 404;
const HTTP_TOO_MANY_REQUESTS: u16 = 429;
const HTTP_SERVER_ERROR_MIN: u16 = 500;
const COMPUTE: &str = "Microsoft.Compute";
const NETWORK: &str = "Microsoft.Network";

/// Where and how to reach ARM.
#[derive(Clone, Debug)]
pub struct ArmConfig {
    /// Resource Manager base URL; [`DEFAULT_ARM_ENDPOINT`] for public Azure.
    pub endpoint: String,
    /// The subscription every call is made in.
    pub subscription_id: String,
    /// TCP and TLS connect timeout.
    pub connect_timeout: Duration,
    /// Whole-request timeout.
    pub request_timeout: Duration,
    /// Larger responses are refused rather than buffered.
    pub max_response_bytes: usize,
}

impl ArmConfig {
    /// Defaults for `subscription_id` on public Azure.
    #[must_use]
    pub fn new(subscription_id: &str) -> Self {
        Self {
            endpoint: DEFAULT_ARM_ENDPOINT.to_string(),
            subscription_id: subscription_id.to_string(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

/// An ARM client for one subscription.
pub struct ArmClient {
    http: reqwest::Client,
    endpoint: String,
    subscription: String,
    token: Arc<dyn TokenProvider>,
    max_response_bytes: usize,
}

impl std::fmt::Debug for ArmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArmClient")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

/// A response's status, headers and (capped) body.
struct Reply {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string)
    }

    fn is_success(&self) -> bool {
        (HTTP_SUCCESS_MIN..HTTP_SUCCESS_END).contains(&self.status)
    }

    fn error(&self) -> Error {
        let retry_after = self
            .header(RETRY_AFTER_HEADER)
            .as_deref()
            .and_then(parse_retry_after);
        classify(self.status, retry_after, &self.body)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    #[serde(default)]
    value: Vec<Value>,
    next_link: Option<String>,
}

/// Reads at most `cap` bytes of `response`'s body.
///
/// # Errors
/// [`Error::Decode`] past the cap; [`Error::Transport`] on a read failure.
pub(crate) async fn read_capped(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?
    {
        if body.len() + chunk.len() > cap {
            return Err(Error::Decode(format!("response larger than {cap} bytes")));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Refuses a path segment that could change the URL's shape: empty, `.`
/// or `..`, or holding anything but letters, digits, `-`, `_`, `.`, `(`,
/// `)` (the characters ARM allows in the names banlieue uses).
fn segment<'a>(what: &str, value: &'a str) -> Result<&'a str> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '(' | ')');
    if value.is_empty() || value == "." || value == ".." || !value.chars().all(allowed) {
        return Err(Error::InvalidRequest {
            code: CODE_INVALID_NAME.to_string(),
            message: format!("{what} {value:?} is not a valid Azure name"),
        });
    }
    Ok(value)
}

impl ArmClient {
    /// Builds the client. Makes no request.
    ///
    /// # Errors
    /// [`Error::InvalidRequest`] if the endpoint is not HTTPS (or loopback
    /// HTTP) or the subscription ID could change the path;
    /// [`Error::Transport`] if the HTTP client cannot be built.
    pub fn new(config: ArmConfig, token: Arc<dyn TokenProvider>) -> Result<Self> {
        let endpoint = config.endpoint.trim_end_matches('/').to_string();
        if !is_trusted_base(&endpoint) {
            return Err(Error::InvalidRequest {
                code: CODE_INVALID_NAME.to_string(),
                message: format!("endpoint {endpoint:?} must be https://"),
            });
        }
        let subscription = segment("subscription", &config.subscription_id)?.to_string();
        let http = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .user_agent(USER_AGENT)
            // Never follow a redirect: the bearer token would go with it.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            http,
            endpoint,
            subscription,
            token,
            max_response_bytes: config.max_response_bytes,
        })
    }

    /// The Resource Manager base URL in use.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn subscription_url(&self, rest: &str) -> String {
        format!(
            "{}/subscriptions/{}{rest}",
            self.endpoint, self.subscription
        )
    }

    /// `.../resourceGroups/{rg}/providers/{namespace}/{kind}/{name}`.
    fn resource_url(&self, rg: &str, namespace: &str, kind: &str, name: &str) -> Result<String> {
        let rg = segment("resource group", rg)?;
        let name = segment("name", name)?;
        Ok(self.subscription_url(&format!(
            "/resourceGroups/{rg}/providers/{namespace}/{kind}/{name}"
        )))
    }

    fn with_query(url: &str, pairs: &[(&str, &str)]) -> String {
        let query = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
        format!("{url}?{query}")
    }

    async fn send(&self, method: Method, url: &str, body: Option<&Value>) -> Result<Reply> {
        ensure_under_endpoint(url, &self.endpoint)?;
        let token = self.token.token().await?;
        let mut request = self.http.request(method.clone(), url).header(
            reqwest::header::AUTHORIZATION,
            format!("{BEARER_PREFIX}{}", token.secret()),
        );
        if let Some(body) = body {
            let bytes = serde_json::to_vec(body).map_err(|e| Error::Decode(e.to_string()))?;
            request = request
                .header(reqwest::header::CONTENT_TYPE, CONTENT_TYPE_JSON)
                .body(bytes);
        } else if method == Method::POST {
            // ARM answers 411 to a POST without a length, and reqwest sends
            // none for an empty body.
            request = request
                .header(reqwest::header::CONTENT_LENGTH, "0")
                .body(Vec::new());
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = read_capped(response, self.max_response_bytes).await?;
        let reply = Reply {
            status,
            headers,
            body,
        };
        if reply.status == HTTP_UNAUTHORIZED {
            self.token.invalidate().await;
        }
        Ok(reply)
    }

    /// A PUT, POST or DELETE: the operation to observe, if ARM started one.
    fn operation(&self, reply: &Reply) -> Result<Option<OperationHandle>> {
        if !reply.is_success() {
            return Err(reply.error());
        }
        OperationHandle::from_headers(|name| reply.header(name), &self.endpoint)
    }

    async fn get_json(&self, url: &str) -> Result<Vec<u8>> {
        let reply = self.send(Method::GET, url, None).await?;
        if !reply.is_success() {
            return Err(reply.error());
        }
        Ok(reply.body)
    }

    async fn delete(&self, url: &str) -> Result<Option<OperationHandle>> {
        let reply = self.send(Method::DELETE, url, None).await?;
        if reply.status == HTTP_NO_CONTENT {
            return Ok(None);
        }
        // A 404 for the resource itself is gone already; for its group it
        // is a configuration error the provider must surface.
        if reply.status == HTTP_NOT_FOUND {
            let (code, _) = error_code_and_message(&reply.body);
            if code != CODE_RESOURCE_GROUP_NOT_FOUND {
                return Ok(None);
            }
        }
        self.operation(&reply)
    }

    /// Follows `nextLink` and returns every page's `value` as one list,
    /// re-wrapped as `{"value":[...]}` for the typed parsers.
    async fn get_all_pages(&self, first: String) -> Result<Vec<u8>> {
        let mut all = Vec::new();
        let mut next = Some(first);
        for _ in 0..MAX_PAGES {
            let Some(url) = next.take() else {
                break;
            };
            let body = self.get_json(&url).await?;
            let page: Page = serde_json::from_slice(&body)
                .map_err(|e| Error::Decode(format!("list page: {e}")))?;
            all.extend(page.value);
            next = page.next_link;
        }
        if next.is_some() {
            return Err(Error::Decode(format!("more than {MAX_PAGES} pages")));
        }
        serde_json::to_vec(&serde_json::json!({ "value": all }))
            .map_err(|e| Error::Decode(e.to_string()))
    }

    /// A GET whose 404 means "absent" (`None`), unless it names the
    /// resource group, which is a configuration error to surface.
    async fn get_optional(&self, url: &str) -> Result<Option<Vec<u8>>> {
        match self.get_json(url).await {
            Ok(body) => Ok(Some(body)),
            Err(e) if e.is_not_found() && e.code() != Some(CODE_RESOURCE_GROUP_NOT_FOUND) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn image_url(&self, rg: &str, gallery: &str, image: &str) -> Result<String> {
        let image = segment("image definition", image)?;
        Ok(format!(
            "{}/images/{image}",
            self.resource_url(rg, COMPUTE, "galleries", gallery)?
        ))
    }

    fn version_url(&self, rg: &str, gallery: &str, image: &str, version: &str) -> Result<String> {
        check_version_name(version)?;
        Ok(format!(
            "{}/versions/{version}",
            self.image_url(rg, gallery, image)?
        ))
    }

    fn disk_url(&self, rg: &str, name: &str) -> Result<String> {
        self.resource_url(rg, COMPUTE, "disks", name)
    }

    fn snapshot_url(&self, rg: &str, name: &str) -> Result<String> {
        self.resource_url(rg, COMPUTE, "snapshots", name)
    }

    async fn put(&self, url: &str, body: &Value) -> Result<Option<OperationHandle>> {
        let reply = self.send(Method::PUT, url, Some(body)).await?;
        self.operation(&reply)
    }

    fn vm_url(&self, rg: &str, name: &str) -> Result<String> {
        self.resource_url(rg, COMPUTE, "virtualMachines", name)
    }

    fn nic_url(&self, rg: &str, name: &str) -> Result<String> {
        self.resource_url(rg, NETWORK, "networkInterfaces", name)
    }

    fn api(url: &str, version: &str) -> String {
        Self::with_query(url, &[("api-version", version)])
    }
}

fn power_verb(action: PowerAction) -> &'static str {
    match action {
        PowerAction::Start => "start",
        PowerAction::Deallocate => "deallocate",
        PowerAction::PowerOff => "powerOff",
        PowerAction::Restart => "restart",
    }
}

#[async_trait]
impl ArmApi for ArmClient {
    async fn put_nic(
        &self,
        resource_group: &str,
        name: &str,
        request: &NicRequest,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(&self.nic_url(resource_group, name)?, NETWORK_API_VERSION);
        let body = request.to_body()?;
        let reply = self.send(Method::PUT, &url, Some(&body)).await?;
        self.operation(&reply)
    }

    async fn nic_private_ip(&self, resource_group: &str, name: &str) -> Result<Option<String>> {
        let url = Self::api(&self.nic_url(resource_group, name)?, NETWORK_API_VERSION);
        primary_private_ip(&self.get_json(&url).await?)
    }

    async fn delete_nic(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(&self.nic_url(resource_group, name)?, NETWORK_API_VERSION);
        self.delete(&url).await
    }

    async fn put_vm(
        &self,
        resource_group: &str,
        name: &str,
        request: &VmRequest,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(&self.vm_url(resource_group, name)?, COMPUTE_API_VERSION);
        let body = request.to_body()?;
        let reply = self.send(Method::PUT, &url, Some(&body)).await?;
        self.operation(&reply)
    }

    async fn vm_instance_view(&self, resource_group: &str, name: &str) -> Result<InstanceView> {
        let url = Self::api(
            &format!("{}/instanceView", self.vm_url(resource_group, name)?),
            COMPUTE_API_VERSION,
        );
        parse_instance_view(&self.get_json(&url).await?)
    }

    async fn vm_tags(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>> {
        let url = Self::api(&self.vm_url(resource_group, name)?, COMPUTE_API_VERSION);
        match self.get_json(&url).await {
            Ok(body) => parse_vm_tags(&body).map(Some),
            // A 404 for the VM is "no VM"; for its group it is a
            // configuration error the provider must surface.
            Err(e) if e.is_not_found() && e.code() != Some(CODE_RESOURCE_GROUP_NOT_FOUND) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn delete_vm(&self, resource_group: &str, name: &str) -> Result<Option<OperationHandle>> {
        let url = Self::api(&self.vm_url(resource_group, name)?, COMPUTE_API_VERSION);
        self.delete(&url).await
    }

    async fn power(
        &self,
        resource_group: &str,
        name: &str,
        action: PowerAction,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(
            &format!(
                "{}/{}",
                self.vm_url(resource_group, name)?,
                power_verb(action)
            ),
            COMPUTE_API_VERSION,
        );
        let reply = self.send(Method::POST, &url, None).await?;
        self.operation(&reply)
    }

    async fn delete_disk(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(
            &self.resource_url(resource_group, COMPUTE, "disks", name)?,
            DISK_API_VERSION,
        );
        self.delete(&url).await
    }

    async fn operation_status(&self, handle: &OperationHandle) -> Result<OperationStatus> {
        // `send` checks the URL again: a handle read back from a CR's
        // status is user-editable.
        let reply = self.send(Method::GET, &handle.url, None).await?;
        match handle.kind {
            OperationKind::AsyncOperation => {
                if !reply.is_success() {
                    return Err(reply.error());
                }
                parse_operation_status(&reply.body)
            }
            OperationKind::Location => {
                if reply.status == HTTP_ACCEPTED {
                    return Ok(OperationStatus::InProgress);
                }
                if reply.is_success() {
                    return Ok(OperationStatus::Succeeded);
                }
                // ARM being unwell is not the operation failing.
                if reply.status == HTTP_TOO_MANY_REQUESTS || reply.status >= HTTP_SERVER_ERROR_MIN {
                    return Err(reply.error());
                }
                let (code, message) = error_code_and_message(&reply.body);
                Ok(OperationStatus::Failed { code, message })
            }
        }
    }

    async fn resource_skus(&self, location: &str) -> Result<Vec<ResourceSku>> {
        let location = segment("location", location)?;
        let filter = format!("location eq '{location}'");
        let url = Self::with_query(
            &self.subscription_url(&format!("/providers/{COMPUTE}/skus")),
            &[("api-version", SKU_API_VERSION), ("$filter", &filter)],
        );
        parse_resource_skus(&self.get_all_pages(url).await?)
    }

    async fn usages(&self, location: &str) -> Result<Vec<Usage>> {
        let location = segment("location", location)?;
        let url = Self::api(
            &self.subscription_url(&format!("/providers/{COMPUTE}/locations/{location}/usages")),
            USAGE_API_VERSION,
        );
        parse_usages(&self.get_all_pages(url).await?)
    }

    async fn list_resources(&self, resource_group: &str) -> Result<Vec<TaggedResource>> {
        let rg = segment("resource group", resource_group)?;
        let url = Self::with_query(
            &self.subscription_url(&format!("/resourceGroups/{rg}/resources")),
            &[
                ("api-version", RESOURCES_API_VERSION),
                ("$expand", "createdTime"),
            ],
        );
        parse_resources(&self.get_all_pages(url).await?)
    }

    async fn get_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
    ) -> Result<Option<GalleryImageVersion>> {
        let url = Self::api(
            &self.version_url(resource_group, gallery, image, version)?,
            GALLERY_API_VERSION,
        );
        self.get_optional(&url)
            .await?
            .map(|body| parse_gallery_image_version(&body))
            .transpose()
    }

    async fn delete_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(
            &self.version_url(resource_group, gallery, image, version)?,
            GALLERY_API_VERSION,
        );
        self.delete(&url).await
    }
}

#[async_trait]
impl PublishApi for ArmClient {
    async fn get_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Option<GalleryImageDefinition>> {
        let url = Self::api(
            &self.image_url(resource_group, gallery, image)?,
            GALLERY_API_VERSION,
        );
        self.get_optional(&url)
            .await?
            .map(|body| parse_gallery_image_definition(&body))
            .transpose()
    }

    async fn put_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        request: &ImageDefinitionRequest,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(
            &self.image_url(resource_group, gallery, image)?,
            GALLERY_API_VERSION,
        );
        self.put(&url, &request.to_body()?).await
    }

    async fn delete_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(
            &self.image_url(resource_group, gallery, image)?,
            GALLERY_API_VERSION,
        );
        self.delete(&url).await
    }

    async fn list_gallery_image_versions(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Vec<GalleryImageVersion>> {
        let url = Self::api(
            &format!(
                "{}/versions",
                self.image_url(resource_group, gallery, image)?
            ),
            GALLERY_API_VERSION,
        );
        parse_gallery_image_versions(&self.get_all_pages(url).await?)
    }

    async fn put_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
        request: &GalleryImageVersionRequest,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(
            &self.version_url(resource_group, gallery, image, version)?,
            GALLERY_API_VERSION,
        );
        self.put(&url, &request.to_body()?).await
    }

    async fn get_disk(&self, resource_group: &str, name: &str) -> Result<Option<ManagedDisk>> {
        let url = Self::api(&self.disk_url(resource_group, name)?, DISK_API_VERSION);
        self.get_optional(&url)
            .await?
            .map(|body| parse_managed_disk(&body))
            .transpose()
    }

    async fn put_upload_disk(
        &self,
        resource_group: &str,
        name: &str,
        request: &UploadDiskRequest,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(&self.disk_url(resource_group, name)?, DISK_API_VERSION);
        self.put(&url, &request.to_body()?).await
    }

    async fn grant_disk_write_access(
        &self,
        resource_group: &str,
        name: &str,
        duration_secs: u32,
    ) -> Result<DiskAccess> {
        let url = Self::api(
            &format!("{}/beginGetAccess", self.disk_url(resource_group, name)?),
            DISK_API_VERSION,
        );
        let body =
            serde_json::json!({ "access": ACCESS_WRITE, "durationInSeconds": duration_secs });
        let reply = self.send(Method::POST, &url, Some(&body)).await?;
        if reply.status == HTTP_OK && !reply.body.is_empty() {
            return parse_access_sas(&reply.body).map(|sas| DiskAccess::Granted(SasUrl::new(sas)));
        }
        match self.operation(&reply)? {
            Some(handle) => Ok(DiskAccess::Pending(handle)),
            None => Err(Error::Decode(
                "beginGetAccess returned neither a SAS nor an operation".to_string(),
            )),
        }
    }

    async fn disk_access_result(&self, handle: &OperationHandle) -> Result<Option<SasUrl>> {
        // `send` checks the URL is under the endpoint: the ARM token goes
        // with it. The SAS comes back in the body, never in a URL we follow.
        let reply = self.send(Method::GET, &handle.url, None).await?;
        if !reply.is_success() {
            return Err(reply.error());
        }
        if handle.kind == OperationKind::Location {
            if reply.status == HTTP_ACCEPTED {
                return Ok(None);
            }
            return parse_access_sas(&reply.body).map(|sas| Some(SasUrl::new(sas)));
        }
        match parse_operation_status(&reply.body)? {
            OperationStatus::InProgress => Ok(None),
            OperationStatus::Succeeded => {
                parse_access_sas(&reply.body).map(|sas| Some(SasUrl::new(sas)))
            }
            OperationStatus::Failed { code, message } => Err(operation_failure(&code, &message)),
            OperationStatus::Canceled => Err(operation_failure(
                "Canceled",
                "the disk access operation was canceled",
            )),
        }
    }

    async fn revoke_disk_access(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(
            &format!("{}/endGetAccess", self.disk_url(resource_group, name)?),
            DISK_API_VERSION,
        );
        let reply = self.send(Method::POST, &url, None).await?;
        self.operation(&reply)
    }

    async fn get_snapshot(&self, resource_group: &str, name: &str) -> Result<Option<Snapshot>> {
        let url = Self::api(&self.snapshot_url(resource_group, name)?, DISK_API_VERSION);
        self.get_optional(&url)
            .await?
            .map(|body| parse_snapshot(&body))
            .transpose()
    }

    async fn put_snapshot(
        &self,
        resource_group: &str,
        name: &str,
        request: &SnapshotRequest,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(&self.snapshot_url(resource_group, name)?, DISK_API_VERSION);
        self.put(&url, &request.to_body()?).await
    }

    async fn delete_snapshot(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let url = Self::api(&self.snapshot_url(resource_group, name)?, DISK_API_VERSION);
        self.delete(&url).await
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
