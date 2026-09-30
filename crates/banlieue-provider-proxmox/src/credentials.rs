// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Resolving a `Provider`'s API token and CA bundle.
//!
//! Token auth only (ADR-0074 Decision 3): the credentials Secret carries
//! `username` (the full `user@realm!tokenid`) and `tokenValue`. There is no
//! password path.

use std::collections::BTreeMap;

use banlieue_api::banlieue::Provider;
use banlieue_provider_sdk::ca_bundle;
use banlieue_proxmox::ApiToken;
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};

use crate::client::Credentials;
use crate::error::{Error, Result};

/// Secret key holding the full `user@realm!tokenid`.
pub const SECRET_KEY_USERNAME: &str = "username";
/// Secret key holding the token's secret.
pub const SECRET_KEY_TOKEN_VALUE: &str = "tokenValue";

/// HTTP status of an absent object.
const HTTP_NOT_FOUND: u16 = 404;

/// Build [`Credentials`] from a Secret's data and an already-resolved CA.
///
/// # Errors
/// [`Error::Missing`] for an absent key; [`Error::Proxmox`] if the token is
/// malformed; [`Error::Invalid`] if the CA bundle is not UTF-8 PEM.
pub fn credentials_from_secret(
    data: &BTreeMap<String, ByteString>,
    ca_pem: Option<Vec<u8>>,
) -> Result<Credentials> {
    let text = |key: &'static str, what: &'static str| -> Result<String> {
        let raw = data.get(key).ok_or(Error::Missing(what))?;
        String::from_utf8(raw.0.clone()).map_err(|_| Error::Invalid {
            what,
            detail: "not valid UTF-8".to_string(),
        })
    };
    let username = text(SECRET_KEY_USERNAME, "secret.data.username")?;
    let token_value = text(SECRET_KEY_TOKEN_VALUE, "secret.data.tokenValue")?;
    let token = ApiToken::new(&username, &token_value)?;
    let ca_pem = ca_pem
        .map(|b| {
            String::from_utf8(b).map_err(|_| Error::Invalid {
                what: "connection.caBundle",
                detail: "not valid UTF-8 PEM".to_string(),
            })
        })
        .transpose()?;
    Ok(Credentials { token, ca_pem })
}

/// Read the credentials Secret and CA bundle for `provider`.
///
/// Unlike libvirt, the CA is optional: a node behind a publicly-trusted
/// certificate verifies against the system roots (ADR-0074 Decision 5).
///
/// # Errors
/// As [`credentials_from_secret`], plus [`Error::Missing`] when the Provider
/// has no `credentialsRef` or the Secret is absent.
pub async fn resolve(client: &Client, namespace: &str, provider: &Provider) -> Result<Credentials> {
    let secret_name = provider
        .spec
        .connection
        .credentials_secret()
        .ok_or(Error::Missing("Provider.spec.connection.credentialsRef"))?;
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api.get(secret_name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == HTTP_NOT_FOUND
        {
            return Error::Missing("Provider.spec.connection.credentialsRef");
        }
        Error::Kube(e)
    })?;
    let ca = ca_bundle::resolve(client, namespace, &provider.spec.connection.ca_bundle).await?;
    credentials_from_secret(&secret.data.unwrap_or_default(), ca)
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
mod credentials_tests;
