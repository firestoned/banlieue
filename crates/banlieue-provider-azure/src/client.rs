// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The Azure-facing seam.
//!
//! [`ArmClientFactory`] builds an `Arc<dyn ArmApi>` from a `Provider`'s
//! connection, so tests inject [`banlieue_azure::fake::FakeArm`] and run the
//! whole reconcile path with no subscription, no TLS and no network, the
//! same pattern the sibling providers use.
//!
//! There is no Secret (ADR-0159). The credential is the provider's projected
//! ServiceAccount token, read from [`DEFAULT_FEDERATED_TOKEN_FILE`] (or
//! `AZURE_FEDERATED_TOKEN_FILE`) and exchanged at Entra ID for the tenant and
//! client the `Provider` declares in `spec.connection.azure`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use banlieue_api::banlieue::{AzureConnection, ProviderConnection};
use banlieue_azure::encryption::check_disk_encryption_set_id;
use banlieue_azure::tags::validate_extra_tags;
use banlieue_azure::{ArmApi, ArmClient, ArmConfig, FederatedConfig, FederatedToken};

use crate::cloud::resolve_cloud;
use crate::error::{Error, Result};

/// Where the operator mounts the projected ServiceAccount token (audience
/// `api://AzureADTokenExchange`), when `AZURE_FEDERATED_TOKEN_FILE` is unset.
pub const DEFAULT_FEDERATED_TOKEN_FILE: &str = "/var/run/secrets/azure/tokens/azure-identity-token";

/// Builds ARM clients.
#[async_trait]
pub trait ArmClientFactory: Send + Sync {
    /// A client for `connection`.
    ///
    /// # Errors
    /// [`Error::Invalid`] when `connection.azure` is missing, or when the
    /// endpoint and authority are not a known Azure cloud's pair
    /// ([`crate::cloud`]); [`Error::Azure`] for a subscription the client
    /// refuses.
    async fn build(&self, connection: &ProviderConnection) -> Result<Arc<dyn ArmApi>>;
}

/// The `azure` block of `connection`.
///
/// # Errors
/// [`Error::Invalid`] when it is not set: an `azure` `Provider` without it
/// names no subscription, region or identity.
pub fn azure_connection(connection: &ProviderConnection) -> Result<&AzureConnection> {
    connection.azure.as_ref().ok_or_else(|| Error::Invalid {
        what: "Provider.spec.connection.azure",
        detail: "is not set; an azure Provider declares its tenant, client, subscription, location and resource group there".to_string(),
    })
}

/// The `azure` block of `connection`, after checking that its endpoint and
/// authority are a known Azure cloud's pair ([`crate::cloud`]): the hosts
/// that receive the ARM and ServiceAccount tokens are never taken on trust.
/// Its governance settings are checked too ([`check_governance`]).
///
/// # Errors
/// As [`azure_connection`], [`resolve_cloud`] and [`check_governance`].
pub fn checked_azure_connection(connection: &ProviderConnection) -> Result<&AzureConnection> {
    let azure = azure_connection(connection)?;
    resolve_cloud(&connection.endpoint, azure.authority_host.as_deref())?;
    check_governance(azure)?;
    Ok(azure)
}

/// Checks the settings a policy-governed subscription needs (ADR-0172):
/// the extra tags follow Azure's rules and never touch banlieue's own
/// `banlieue-` tags in any case, and the disk encryption set is one, in
/// this connection's subscription. Checked before any client is built, so
/// an invalid setting is the `Provider`'s `ConnectionInvalid` condition and
/// nothing is created with it.
///
/// # Errors
/// [`Error::Invalid`] naming the field.
pub fn check_governance(azure: &AzureConnection) -> Result<()> {
    validate_extra_tags(&azure.tags).map_err(|e| Error::Invalid {
        what: "Provider.spec.connection.azure.tags",
        detail: e.to_string(),
    })?;
    let Some(id) = &azure.disk_encryption_set_id else {
        return Ok(());
    };
    check_disk_encryption_set_id(id, &azure.subscription_id).map_err(|e| Error::Invalid {
        what: "Provider.spec.connection.azure.diskEncryptionSetId",
        detail: e.to_string(),
    })
}

/// The token exchange and ARM settings `connection` declares, with the token
/// read from `token_file`. Pure, so the mapping is tested without a network.
/// The endpoint, authority and token scope all come from the one known
/// cloud the connection names ([`resolve_cloud`]).
///
/// # Errors
/// As [`azure_connection`] and [`resolve_cloud`].
pub fn client_configs(
    connection: &ProviderConnection,
    token_file: &Path,
) -> Result<(FederatedConfig, ArmConfig)> {
    let azure = azure_connection(connection)?;
    let cloud = resolve_cloud(&connection.endpoint, azure.authority_host.as_deref())?;
    let federated = FederatedConfig {
        authority: cloud.authority_host.to_string(),
        tenant_id: azure.tenant_id.clone(),
        client_id: azure.client_id.clone(),
        token_file: token_file.to_path_buf(),
        scope: cloud.arm_scope(),
    };
    let mut arm = ArmConfig::new(&azure.subscription_id);
    arm.endpoint = cloud.arm_endpoint.to_string();
    Ok((federated, arm))
}

/// Install the process-default rustls crypto provider, which reqwest's
/// `rustls-no-provider` needs before its first TLS use: banlieue's one
/// provider, aws-lc-rs with X25519MLKEM768 offered first (ADR-0170).
/// Idempotent: an already-installed provider is not an error.
pub use banlieue_tls::install_default_crypto_provider;

/// What identifies one client: two `Provider`s with the same values share
/// one client and so one cached Entra ID token.
type ClientKey = (String, String, String, String, String);

/// The real factory: HTTPS to ARM with a federated token.
///
/// Clients are cached by connection, so the Entra ID token each caches
/// (refreshed before expiry) survives across reconciles instead of being
/// exchanged on every one. There is no Secret to rotate (ADR-0159): a changed
/// connection is a different key, and the kubelet-rotated token file is read
/// again on every exchange.
pub struct HttpClientFactory {
    token_file: PathBuf,
    clients: Mutex<BTreeMap<ClientKey, Arc<dyn ArmApi>>>,
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
impl ArmClientFactory for HttpClientFactory {
    async fn build(&self, connection: &ProviderConnection) -> Result<Arc<dyn ArmApi>> {
        let (federated, arm) = client_configs(connection, &self.token_file)?;
        check_governance(azure_connection(connection)?)?;
        let key = (
            arm.endpoint.clone(),
            arm.subscription_id.clone(),
            federated.authority.clone(),
            federated.tenant_id.clone(),
            federated.client_id.clone(),
        );
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(&key) {
            return Ok(client.clone());
        }
        let token = Arc::new(FederatedToken::new(federated)?);
        let client: Arc<dyn ArmApi> = Arc::new(ArmClient::new(arm, token)?);
        clients.insert(key, client.clone());
        Ok(client)
    }
}

/// A factory that always returns the API it was given: for tests and for
/// running the reconcilers against `FakeArm`.
#[derive(Clone)]
pub struct StaticClientFactory {
    api: Arc<dyn ArmApi>,
}

impl StaticClientFactory {
    /// Wrap `api`.
    pub fn new(api: Arc<dyn ArmApi>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl ArmClientFactory for StaticClientFactory {
    async fn build(&self, connection: &ProviderConnection) -> Result<Arc<dyn ArmApi>> {
        checked_azure_connection(connection)?;
        Ok(self.api.clone())
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
