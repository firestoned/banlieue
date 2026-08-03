// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Resolving a `Provider`'s TLS material into a [`TlsIdentity`].
//!
//! With `auth_tls = "none"` the x509 client certificate *is* the credential
//! (ADR-0011), so there is no password anywhere here:
//!
//! - `connection.credentialsRef` → a Secret carrying `tls.crt` / `tls.key`.
//! - `connection.caBundle` → the CA that signed libvirtd's server certificate,
//!   resolved by the shared SDK resolver ([`banlieue_provider_sdk::ca_bundle`]).
//!
//! Takes a bare [`Client`] rather than a [`crate::context::Context`] because
//! the import Job ([`crate::import`]) resolves the same material without ever
//! constructing a reconcile context.

use banlieue_api::banlieue::Provider;
use banlieue_libvirt::TlsIdentity;
use banlieue_provider_sdk::ca_bundle;
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};

use crate::error::{Error, Result};
use crate::reconciler::provider::{SECRET_KEY_TLS_CRT, SECRET_KEY_TLS_KEY};

/// Read the client certificate, key, and CA bundle for `provider`.
///
/// # Errors
/// [`Error::Missing`] when a referenced object or key is absent, or
/// [`Error::Invalid`] when `caBundle` does not name exactly one source.
pub async fn resolve(client: &Client, namespace: &str, provider: &Provider) -> Result<TlsIdentity> {
    let secret_name = &provider.spec.connection.credentials_ref.name;
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api.get(secret_name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == 404
        {
            return Error::Missing("Provider.spec.connection.credentialsRef");
        }
        Error::Kube(e)
    })?;
    let data = secret.data.unwrap_or_default();
    let take = |k: &'static str| -> Result<Vec<u8>> {
        data.get(k)
            .map(|b| b.0.clone())
            .ok_or(Error::Missing(match k {
                SECRET_KEY_TLS_CRT => "secret.data.tls.crt",
                _ => "secret.data.tls.key",
            }))
    };

    let ca_pem = resolve_ca_bundle(client, namespace, &provider.spec.connection.ca_bundle).await?;
    Ok(TlsIdentity {
        ca_pem,
        client_cert_pem: take(SECRET_KEY_TLS_CRT)?,
        client_key_pem: take(SECRET_KEY_TLS_KEY)?,
    })
}

/// Resolve `caBundle` to PEM bytes, requiring one.
///
/// Unlike vSphere, where the bundle is optional and system roots are the
/// fallback, a CA is **required** here: libvirtd's certificate is issued by a
/// private CA in every realistic deployment, so falling back to public trust
/// roots would only fail later and less clearly.
///
/// The shared resolver deliberately returns `Option` and leaves that call to
/// the backend; this is where libvirt makes it.
async fn resolve_ca_bundle(
    client: &Client,
    namespace: &str,
    source: &Option<banlieue_api::common::CABundleSource>,
) -> Result<Vec<u8>> {
    ca_bundle::resolve(client, namespace, source)
        .await?
        .ok_or(Error::Missing("Provider.spec.connection.caBundle"))
}
