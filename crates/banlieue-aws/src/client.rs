// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The HTTPS client for EC2 (ADR-0174 Decision 2).
//!
//! [`Ec2Client`] implements [`Ec2Api`] over `reqwest`. Every call is a
//! SigV4-signed `POST /` of a Query form ([`crate::query`]) to the
//! regional endpoint. Throttling, and only throttling, is retried inside
//! the client with capped exponential back-off and full jitter
//! ([`crate::retry`]); every throttled answer is counted so the provider
//! can report sustained throttling ([`Ec2Client::throttling_sustained`]).
//! Nothing blocks on an instance's state change.
//!
//! TLS is the process's rustls provider, which the binary installs
//! (`banlieue-tls`, aws-lc-rs, X25519MLKEM768 first, ADR-0170).

use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jiff::Timestamp;

use crate::api::Ec2Api;
use crate::auth::CredentialSource;
use crate::error::{Error, Result};
use crate::instance::{
    Ec2Request, EkKeyType, RunInstancesRequest, describe_availability_zones, describe_image,
    describe_instance, describe_instance_type_offerings, describe_instance_types,
    describe_instances_by_tag, disable_instance_metadata, get_console_output,
    get_instance_tpm_ek_pub, start_instances, stop_instances, terminate_instances,
};
use crate::query::{FORM_CONTENT_TYPE, QueryParams};
use crate::retry::{Draw, RetryPolicy, Throttling, system_draw};
use crate::sigv4::{Payload, SigningOptions, SigningRequest, sign};
use crate::types::{
    AvailabilityZone, ImageInfo, Instance, InstanceState, InstanceStateChange, InstanceTypeInfo,
    InstanceTypeOffering, TpmEkPub, parse_availability_zones, parse_console_output,
    parse_describe_images, parse_describe_instances, parse_error, parse_instance_state_changes,
    parse_instance_type_offerings, parse_instance_types, parse_next_token, parse_run_instances,
    parse_tpm_ek_pub,
};

/// EC2's SigV4 signing name.
pub const EC2_SERVICE: &str = "ec2";
/// The longest region name accepted; AWS's longest is well under it.
pub const MAX_REGION_LEN: usize = 32;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// A page of `DescribeInstances` or offerings is well under this.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Stops a `nextToken` loop that never ends.
const MAX_PAGES: usize = 50;
const USER_AGENT: &str = concat!("banlieue-aws/", env!("CARGO_PKG_VERSION"));
const CONTENT_TYPE_HEADER: &str = "content-type";
const ROOT_PATH: &str = "/";
const POST: &str = "POST";
const P_NEXT_TOKEN: &str = "NextToken";
const SCHEME_HTTPS: &str = "https";
const SCHEME_HTTP: &str = "http";
/// A region's first part: a two-letter country or area code.
const REGION_AREA_LEN: usize = 2;
/// Area, at least one name part, and the number.
const MIN_REGION_PARTS: usize = 3;
const REGION_SEPARATOR: char = '-';

/// Checks `region` has the shape of an AWS region name,
/// `^[a-z]{2}(-[a-z0-9]+)+-[0-9]+$` (`us-east-1`, `us-gov-west-1`), so it
/// can sit in a host name.
///
/// # Errors
/// [`Error::Invalid`] when it does not.
pub fn validate_region(region: &str) -> Result<()> {
    let parts: Vec<&str> = region.split(REGION_SEPARATOR).collect();
    let well_formed = region.len() <= MAX_REGION_LEN
        && parts.len() >= MIN_REGION_PARTS
        && parts.first().is_some_and(|area| {
            area.len() == REGION_AREA_LEN && area.bytes().all(|b| b.is_ascii_lowercase())
        })
        && parts
            .last()
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && parts[1..parts.len() - 1].iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if !well_formed {
        return Err(Error::Invalid(format!(
            "{region:?} is not an AWS region name"
        )));
    }
    Ok(())
}

/// The regional EC2 endpoint, `https://ec2.<region>.amazonaws.com/`.
///
/// # Errors
/// [`Error::Invalid`] when `region` is not an AWS region name.
pub fn ec2_endpoint(region: &str) -> Result<String> {
    validate_region(region)?;
    Ok(format!("https://ec2.{region}.amazonaws.com/"))
}

/// Parses an endpoint override and accepts only `https://`, or `http://`
/// to a loopback address (tests), with no user info, query or fragment.
///
/// # Errors
/// [`Error::Invalid`] for anything else.
pub fn check_endpoint(url: &str) -> Result<reqwest::Url> {
    let refuse = |why: &str| Error::Invalid(format!("endpoint {url:?} {why}"));
    let parsed = reqwest::Url::parse(url).map_err(|_| refuse("is not a URL"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(refuse("carries user info"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(refuse("carries a query or fragment"));
    }
    // An IPv6 host comes bracketed (`[::1]`); a name is never trusted as
    // loopback, since resolution could send it elsewhere.
    let loopback = parsed
        .host_str()
        .map(|h| h.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|h| h.parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    match parsed.scheme() {
        SCHEME_HTTPS if parsed.host().is_some() => Ok(parsed),
        SCHEME_HTTP if loopback => Ok(parsed),
        _ => Err(refuse("must be https://, or http:// on a loopback address")),
    }
}

/// The `host` header SigV4 signs for `url`: the host, with `:port` when
/// the URL names one other than the scheme's default (as reqwest sends it).
fn host_header(url: &reqwest::Url) -> Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::Invalid(format!("endpoint {url} has no host")))?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
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
        .map_err(|e| Error::Transport(e.without_url().to_string()))?
    {
        if body.len() + chunk.len() > cap {
            return Err(Error::Decode(format!("response larger than {cap} bytes")));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn build_http(timeouts: bool) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        // Never follow a redirect: a signed request belongs to one host.
        .redirect(reqwest::redirect::Policy::none());
    if timeouts {
        builder = builder
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT);
    }
    builder
        .build()
        .map_err(|e| Error::Transport(e.without_url().to_string()))
}

/// An EC2 client for one region.
pub struct Ec2Client {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    host: String,
    region: String,
    credentials: Arc<dyn CredentialSource>,
    retry: RetryPolicy,
    draw: Draw,
    throttling: Mutex<Throttling>,
}

impl std::fmt::Debug for Ec2Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ec2Client")
            .field("endpoint", &self.endpoint.as_str())
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl Ec2Client {
    /// A client for `region`'s public endpoint. Makes no request.
    ///
    /// # Errors
    /// [`Error::Invalid`] if `region` is not a region name;
    /// [`Error::Transport`] if the HTTP client cannot be built.
    pub fn new(region: &str, credentials: Arc<dyn CredentialSource>) -> Result<Self> {
        let endpoint = check_endpoint(&ec2_endpoint(region)?)?;
        Ok(Self {
            http: build_http(true)?,
            host: host_header(&endpoint)?,
            endpoint,
            region: region.to_string(),
            credentials,
            retry: RetryPolicy::default(),
            draw: system_draw,
            throttling: Mutex::new(Throttling::default()),
        })
    }

    /// The same client sending to `url` instead. Meant for tests (a local
    /// server on `http://127.0.0.1:<port>`); `http://` is refused unless
    /// the host is a loopback address, so a signed request never crosses a
    /// network in the clear.
    ///
    /// # Errors
    /// [`Error::Invalid`] per [`check_endpoint`].
    pub fn with_endpoint(mut self, url: &str) -> Result<Self> {
        let endpoint = check_endpoint(url)?;
        self.host = host_header(&endpoint)?;
        self.endpoint = endpoint;
        Ok(self)
    }

    /// The same client with another retry budget.
    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Tests on tokio's paused clock: no reqwest timers, which the clock's
    /// auto-advance would otherwise fire while the local server answers.
    #[cfg(test)]
    pub(crate) fn without_timeouts(mut self) -> Result<Self> {
        self.http = build_http(false)?;
        Ok(self)
    }

    /// The endpoint in use.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }

    /// The region requests are signed for.
    #[must_use]
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Whether EC2 has throttled this client persistently: at least
    /// [`crate::retry::SUSTAINED_THROTTLE_COUNT`] throttled answers within
    /// [`crate::retry::SUSTAINED_THROTTLE_WINDOW`], for the provider's
    /// condition on its `Provider`.
    #[must_use]
    pub fn throttling_sustained(&self) -> bool {
        self.throttling
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_sustained(Instant::now())
    }

    /// Sends `request` and returns the response body. A throttled answer is
    /// retried per the [`RetryPolicy`]; any other error is returned at
    /// once.
    ///
    /// # Errors
    /// The typed AWS error ([`parse_error`]) for a non-2xx answer, the
    /// credential source's error, or [`Error::Transport`] (which carries
    /// neither the credentials nor the body).
    pub async fn send(&self, request: &Ec2Request) -> Result<String> {
        let mut attempt: u32 = 0;
        loop {
            let error = match self.send_once(request).await {
                Ok(body) => return Ok(body),
                Err(e) => e,
            };
            self.throttling
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .observe(&error, Instant::now());
            if !self.retry.should_retry(&error, attempt) {
                return Err(error);
            }
            let Some(delay) = self.retry.delay_with(attempt, self.draw) else {
                return Err(error);
            };
            tokio::time::sleep(delay).await;
            attempt = attempt.saturating_add(1);
        }
    }

    async fn send_once(&self, request: &Ec2Request) -> Result<String> {
        let mut params = QueryParams::new(request.action);
        params.extend(request.params.iter().cloned())?;
        let body = params.encode();
        let credentials = self.credentials.credentials().await?;
        let headers = [(
            CONTENT_TYPE_HEADER.to_string(),
            FORM_CONTENT_TYPE.to_string(),
        )];
        let signed = sign(
            &SigningRequest {
                method: POST,
                host: &self.host,
                path: ROOT_PATH,
                query: &[],
                headers: &headers,
                payload: Payload::Bytes(body.as_bytes()),
                region: &self.region,
                service: EC2_SERVICE,
                timestamp: Timestamp::now(),
            },
            &credentials,
            &SigningOptions::standard(),
        )?;
        let mut builder = self
            .http
            .post(self.endpoint.clone())
            .header(reqwest::header::CONTENT_TYPE, FORM_CONTENT_TYPE);
        for (name, value) in &signed.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let status = response.status();
        let bytes = read_capped(response, MAX_RESPONSE_BYTES).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !status.is_success() {
            return Err(parse_error(status.as_u16(), &text));
        }
        Ok(text)
    }

    /// Sends `request`, following `nextToken` (as `NextToken`) until a page
    /// has none, and returns every page's items.
    async fn send_all_pages<T>(
        &self,
        request: &Ec2Request,
        parse: impl Fn(&str) -> Result<Vec<T>>,
    ) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut next: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut page = request.clone();
            if let Some(token) = next.take() {
                page.params.push((P_NEXT_TOKEN.to_string(), token));
            }
            let body = self.send(&page).await?;
            items.extend(parse(&body)?);
            next = parse_next_token(&body)?;
            if next.is_none() {
                return Ok(items);
            }
        }
        Err(Error::Decode(format!(
            "{} returned more than {MAX_PAGES} pages",
            request.action
        )))
    }

    async fn state_change(&self, request: &Ec2Request, instance_id: &str) -> Result<InstanceState> {
        let body = self.send(request).await?;
        first_state(parse_instance_state_changes(&body)?, instance_id)
    }
}

/// The new state of `instance_id` among `changes`.
fn first_state(changes: Vec<InstanceStateChange>, instance_id: &str) -> Result<InstanceState> {
    changes
        .into_iter()
        .find(|c| c.instance_id == instance_id)
        .map(|c| c.current)
        .ok_or_else(|| Error::Decode(format!("no state change for {instance_id}")))
}

#[async_trait]
impl Ec2Api for Ec2Client {
    async fn run_instance(&self, request: &RunInstancesRequest) -> Result<String> {
        let body = self.send(&request.to_request()?).await?;
        parse_run_instances(&body)
    }

    async fn describe_instance(&self, instance_id: &str) -> Result<Option<Instance>> {
        let body = match self.send(&describe_instance(instance_id)?).await {
            Ok(body) => body,
            // Gone, or not visible yet just after the create.
            Err(e) if e.is_not_found() => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(parse_describe_instances(&body)?
            .into_iter()
            .find(|i| i.instance_id == instance_id))
    }

    async fn find_instances_by_tag(&self, key: &str, value: &str) -> Result<Vec<Instance>> {
        self.send_all_pages(
            &describe_instances_by_tag(key, value)?,
            parse_describe_instances,
        )
        .await
    }

    async fn terminate_instance(&self, instance_id: &str) -> Result<Option<InstanceState>> {
        match self
            .state_change(&terminate_instances(instance_id)?, instance_id)
            .await
        {
            Ok(state) => Ok(Some(state)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn stop_instance(&self, instance_id: &str) -> Result<InstanceState> {
        self.state_change(&stop_instances(instance_id)?, instance_id)
            .await
    }

    async fn start_instance(&self, instance_id: &str) -> Result<InstanceState> {
        self.state_change(&start_instances(instance_id)?, instance_id)
            .await
    }

    async fn console_output(&self, instance_id: &str) -> Result<String> {
        let body = self.send(&get_console_output(instance_id)?).await?;
        parse_console_output(&body)
    }

    async fn tpm_ek_pub(&self, instance_id: &str, key_type: EkKeyType) -> Result<TpmEkPub> {
        let body = self
            .send(&get_instance_tpm_ek_pub(instance_id, key_type)?)
            .await?;
        parse_tpm_ek_pub(&body)
    }

    async fn disable_instance_metadata(&self, instance_id: &str) -> Result<()> {
        self.send(&disable_instance_metadata(instance_id)?).await?;
        Ok(())
    }

    async fn availability_zones(&self) -> Result<Vec<AvailabilityZone>> {
        let body = self.send(&describe_availability_zones()).await?;
        parse_availability_zones(&body)
    }

    async fn instance_type_offerings(
        &self,
        instance_types: &[String],
    ) -> Result<Vec<InstanceTypeOffering>> {
        self.send_all_pages(
            &describe_instance_type_offerings(instance_types)?,
            parse_instance_type_offerings,
        )
        .await
    }

    async fn instance_types(&self, instance_types: &[String]) -> Result<Vec<InstanceTypeInfo>> {
        let body = self.send(&describe_instance_types(instance_types)?).await?;
        parse_instance_types(&body)
    }

    async fn describe_image(&self, image_id: &str) -> Result<Option<ImageInfo>> {
        let body = match self.send(&describe_image(image_id)?).await {
            Ok(body) => body,
            Err(e) if e.is_not_found() => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(parse_describe_images(&body)?
            .into_iter()
            .find(|i| i.image_id == image_id))
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
