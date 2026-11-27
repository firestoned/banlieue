// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The AWS-facing seam.
//!
//! [`Ec2ClientFactory`] builds an `Arc<dyn Ec2Api>` for a `Provider`, so
//! tests inject [`banlieue_aws::FakeEc2`] and run the whole reconcile path
//! with no account, no TLS and no network, the same pattern the sibling
//! providers use.
//!
//! There is no Secret (ADR-0159). The credential is the provider's projected
//! ServiceAccount token (audience `sts.amazonaws.com`), read from
//! [`DEFAULT_WEB_IDENTITY_TOKEN_FILE`] (or `AWS_WEB_IDENTITY_TOKEN_FILE`) and
//! exchanged at STS by `AssumeRoleWithWebIdentity` for the role the
//! `Provider` declares in `spec.connection.ec2`, as a fifteen-minute session
//! refreshed at ten minutes (ADR-0174 Decision 6). The token and the session
//! stay in memory in `banlieue-aws`; nothing here logs or stores them.
//!
//! Every host a token goes to is checked first: `connection.endpoint` must be
//! the region's EC2 endpoint (or a VPC endpoint of it) and `stsEndpoint` the
//! region's STS (or a VPC endpoint of it), the rules admission applies too
//! ([`banlieue_api::banlieue::Ec2Connection::validate`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use banlieue_api::banlieue::{Ec2Connection, Provider, ProviderConnection};
use banlieue_aws::sts::role_session_name;
use banlieue_aws::{Ec2Api, Ec2Client, WebIdentityConfig, WebIdentitySource};
use kube::ResourceExt;

use crate::error::{Error, Result};

/// Where the operator mounts the projected ServiceAccount token (audience
/// `sts.amazonaws.com`), when `AWS_WEB_IDENTITY_TOKEN_FILE` is unset.
pub const DEFAULT_WEB_IDENTITY_TOKEN_FILE: &str = "/var/run/secrets/aws/tokens/aws-identity-token";

/// What a `Provider`'s connection field is called in errors.
const CONNECTION_FIELD: &str = "Provider.spec.connection.ec2";

/// Builds EC2 clients.
#[async_trait]
pub trait Ec2ClientFactory: Send + Sync {
    /// A client for `provider`'s account and region, acting as its role.
    ///
    /// # Errors
    /// [`Error::Invalid`] when `spec.connection.ec2` is missing or breaks a
    /// rule of [`checked_ec2_connection`]; [`Error::Aws`] when the client
    /// cannot be built.
    async fn build(&self, provider: &Provider) -> Result<Arc<dyn Ec2Api>>;
}

/// The `ec2` block of `connection`.
///
/// # Errors
/// [`Error::Invalid`] when it is not set: an `ec2` `Provider` without it
/// names no region or role.
pub fn ec2_connection(connection: &ProviderConnection) -> Result<&Ec2Connection> {
    connection.ec2.as_ref().ok_or_else(|| Error::Invalid {
        what: CONNECTION_FIELD,
        detail: "is not set; an ec2 Provider declares its region and the role to assume there"
            .to_string(),
    })
}

/// The `ec2` block of `connection`, after the checks admission applies: the
/// endpoint and STS endpoint pinned to the region's own AWS names, the role
/// in the region's partition, KMS keys and extra tags of the right shape,
/// and no `credentialsRef` (ADR-0159: the ServiceAccount token is the only
/// credential). Checked before any client is built, so a bad connection is
/// the `Provider`'s `ConnectionInvalid` condition and no token is sent.
///
/// # Errors
/// [`Error::Invalid`] naming the first rule broken.
pub fn checked_ec2_connection(connection: &ProviderConnection) -> Result<&Ec2Connection> {
    let ec2 = ec2_connection(connection)?;
    if connection.credentials_ref.is_some() {
        return Err(Error::Invalid {
            what: "Provider.spec.connection.credentialsRef",
            detail: "is refused for an ec2 Provider: its only credential is the projected ServiceAccount token (ADR-0159)".to_string(),
        });
    }
    ec2.validate(&connection.endpoint)
        .map_err(|detail| Error::Invalid {
            what: CONNECTION_FIELD,
            detail,
        })?;
    Ok(ec2)
}

/// The web identity exchange `provider` declares, with the token read from
/// `token_file`: the region, the role, a session named after the `Provider`
/// (`<namespace>.<name>`, so CloudTrail names who acted), the 900 s session
/// `banlieue-aws` defaults to, and the region's STS endpoint unless
/// `stsEndpoint` names a VPC endpoint of it. Pure, so the mapping is tested
/// without a network.
///
/// # Errors
/// As [`checked_ec2_connection`]; [`Error::Invalid`] when the session name
/// breaks STS's rules.
pub fn web_identity_config(provider: &Provider, token_file: PathBuf) -> Result<WebIdentityConfig> {
    let ec2 = checked_ec2_connection(&provider.spec.connection)?;
    let session = role_session_name(
        &provider.namespace().unwrap_or_default(),
        &provider.name_any(),
    )
    .map_err(|e| Error::Invalid {
        what: "Provider name",
        detail: e.to_string(),
    })?;
    let mut config = WebIdentityConfig::new(&ec2.region, &ec2.role_arn, &session, token_file);
    config.sts_endpoint = Some(ec2.sts_endpoint_or_default());
    Ok(config)
}

/// Install the process-default rustls crypto provider, which reqwest's
/// `rustls-no-provider` needs before its first TLS use: banlieue's one
/// provider, aws-lc-rs with X25519MLKEM768 offered first (ADR-0170).
/// Idempotent: an already-installed provider is not an error.
pub use banlieue_tls::install_default_crypto_provider;

/// What identifies one client: two `Provider`s with the same values share
/// one client and so one cached session.
type ClientKey = (String, String, String, String, String);

/// The real factory: signed HTTPS to EC2 with a web identity session.
///
/// Clients are cached by connection, so the STS session each caches
/// (refreshed at ten minutes of fifteen) survives across reconciles instead
/// of being exchanged on every one. There is no Secret to rotate (ADR-0159):
/// a changed connection is a different key, and the kubelet-rotated token
/// file is read again on every exchange.
pub struct HttpClientFactory {
    token_file: PathBuf,
    clients: Mutex<BTreeMap<ClientKey, Arc<dyn Ec2Api>>>,
}

impl HttpClientFactory {
    /// A factory reading the ServiceAccount token from `token_file`.
    #[must_use]
    pub fn new(token_file: PathBuf) -> Self {
        Self {
            token_file,
            clients: Mutex::new(BTreeMap::new()),
        }
    }
}

#[async_trait]
impl Ec2ClientFactory for HttpClientFactory {
    async fn build(&self, provider: &Provider) -> Result<Arc<dyn Ec2Api>> {
        let config = web_identity_config(provider, self.token_file.clone())?;
        let endpoint = provider.spec.connection.endpoint.clone();
        let key = (
            endpoint.clone(),
            config.region.clone(),
            config.role_arn.clone(),
            config.sts_endpoint.clone().unwrap_or_default(),
            config.role_session_name.clone(),
        );
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(&key) {
            return Ok(client.clone());
        }
        let region = config.region.clone();
        let source = Arc::new(WebIdentitySource::new(config)?);
        // The endpoint was pinned to the region's EC2 (or a VPC endpoint of
        // it) by `checked_ec2_connection`, which also covers the `aws-cn`
        // suffix the client's default would miss.
        let client: Arc<dyn Ec2Api> =
            Arc::new(Ec2Client::new(&region, source)?.with_endpoint(&endpoint)?);
        clients.insert(key, client.clone());
        Ok(client)
    }
}

/// A factory that always returns the API it was given: for tests and for
/// running the reconcilers against `FakeEc2`. It still checks the
/// connection, as the real factory does.
#[derive(Clone)]
pub struct StaticClientFactory {
    api: Arc<dyn Ec2Api>,
}

impl StaticClientFactory {
    /// Wrap `api`.
    pub fn new(api: Arc<dyn Ec2Api>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Ec2ClientFactory for StaticClientFactory {
    async fn build(&self, provider: &Provider) -> Result<Arc<dyn Ec2Api>> {
        checked_ec2_connection(&provider.spec.connection)?;
        Ok(self.api.clone())
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
