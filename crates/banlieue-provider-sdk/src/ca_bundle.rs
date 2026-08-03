// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Resolve `Provider.spec.connection.caBundle` to PEM bytes (ADR-0008).
//!
//! A [`CABundleSource`] is a value-or-source: inline PEM, or a `configMapRef` /
//! `secretRef` naming a key (default [`DEFAULT_CA_BUNDLE_KEY`]) in the
//! Provider's namespace. Every backend that speaks TLS to its host needs this,
//! and each was growing its own copy — vSphere's and libvirt's had already
//! drifted in error type and return shape while resolving the identical spec.
//!
//! **What stays with the provider.** Whether a bundle is *required* is a
//! backend decision, not a shared one: vSphere falls back to the system trust
//! roots when it is absent, while libvirt rejects that, because libvirtd's
//! certificate is issued by a private CA in every realistic deployment. So
//! [`resolve`] returns `Option<Vec<u8>>` and says nothing about whether `None`
//! is acceptable — the caller decides.
//!
//! The "exactly one source" invariant *is* shared, and is enforced here as the
//! controller-side floor under the `ValidatingAdmissionPolicy` that enforces it
//! at admission.

use banlieue_api::common::{CABundleSource, DEFAULT_CA_BUNDLE_KEY, KeySelector};
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::{Api, Client};

use crate::error::{Error, Result};

/// Outcome of the pure (no-I/O) classification step of [`resolve`].
///
/// Public so a caller can classify without reading anything — the inline and
/// absent cases are then answerable with no cluster access at all.
#[derive(Debug, PartialEq, Eq)]
pub enum CABundlePlan<'a> {
    /// No bundle configured. What that means is the caller's decision.
    None,
    /// PEM available directly from the spec.
    Inline(&'a str),
    /// Read the given key from this ConfigMap.
    ConfigMap(&'a KeySelector),
    /// Read the given key from this Secret.
    Secret(&'a KeySelector),
}

/// Classify a [`CABundleSource`], enforcing "exactly one source". No I/O.
///
/// # Errors
/// [`Error::Invalid`] when zero or more than one source is set.
pub fn plan(source: &Option<CABundleSource>) -> Result<CABundlePlan<'_>> {
    let Some(source) = source else {
        return Ok(CABundlePlan::None);
    };
    source.validate().map_err(Error::Invalid)?;

    if let Some(pem) = &source.inline {
        return Ok(CABundlePlan::Inline(pem));
    }
    if let Some(sel) = &source.config_map_ref {
        return Ok(CABundlePlan::ConfigMap(sel));
    }
    if let Some(sel) = &source.secret_ref {
        return Ok(CABundlePlan::Secret(sel));
    }
    // Unreachable: validate() guarantees exactly one branch fired.
    Err(Error::Invalid(
        "caBundle: no source resolved after validation",
    ))
}

/// Resolve an optional [`CABundleSource`] to PEM bytes.
///
/// - `None` → `Ok(None)`. Whether that is acceptable is the caller's call.
/// - inline → the PEM verbatim.
/// - `configMapRef` / `secretRef` → the key's value (default `ca.crt`) from the
///   named object in `namespace`.
///
/// All references are namespace-local, like the credentials Secret.
///
/// # Errors
/// - [`Error::Invalid`] if zero or more than one source is set.
/// - [`Error::Missing`] if the referenced object or key is absent, or a Secret
///   value is not UTF-8.
/// - [`Error::Kube`] for any other API error.
pub async fn resolve(
    client: &Client,
    namespace: &str,
    source: &Option<CABundleSource>,
) -> Result<Option<Vec<u8>>> {
    match plan(source)? {
        CABundlePlan::None => Ok(None),
        CABundlePlan::Inline(pem) => Ok(Some(pem.as_bytes().to_vec())),
        CABundlePlan::ConfigMap(sel) => {
            Ok(Some(read_config_map_key(client, namespace, sel).await?))
        }
        CABundlePlan::Secret(sel) => Ok(Some(read_secret_key(client, namespace, sel).await?)),
    }
}

/// Read `selector.key` (default `ca.crt`) from a ConfigMap's `data`.
async fn read_config_map_key(
    client: &Client,
    namespace: &str,
    selector: &KeySelector,
) -> Result<Vec<u8>> {
    let key = selector.key_or(DEFAULT_CA_BUNDLE_KEY);
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let cm = api.get(&selector.name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == 404
        {
            return Error::Missing("Provider.spec.connection.caBundle.configMapRef");
        }
        Error::Kube(e)
    })?;
    cm.data
        .unwrap_or_default()
        .get(key)
        .map(|v| v.clone().into_bytes())
        .ok_or(Error::Missing("caBundle.configMapRef: key not found"))
}

/// Read `selector.key` (default `ca.crt`) from a Secret's `data`.
///
/// kube base64-decodes Secret values into raw bytes; a CA bundle is PEM, so it
/// must still be valid UTF-8 — a binary DER certificate here is a
/// misconfiguration worth naming rather than passing on to the TLS stack.
async fn read_secret_key(
    client: &Client,
    namespace: &str,
    selector: &KeySelector,
) -> Result<Vec<u8>> {
    let key = selector.key_or(DEFAULT_CA_BUNDLE_KEY);
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api.get(&selector.name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == 404
        {
            return Error::Missing("Provider.spec.connection.caBundle.secretRef");
        }
        Error::Kube(e)
    })?;
    let raw = secret
        .data
        .unwrap_or_default()
        .get(key)
        .cloned()
        .ok_or(Error::Missing("caBundle.secretRef: key not found"))?;
    pem_from_secret_value(raw.0)
}

/// Validate a Secret value as PEM text.
///
/// Split out from the read so it is testable without a cluster: reaching it
/// through [`resolve`] needs a kube API, and mutation testing showed the check
/// was consequently pinned by nothing at all.
///
/// # Errors
/// [`Error::Missing`] if the bytes are not UTF-8. A binary DER certificate here
/// is a misconfiguration worth naming rather than passing to the TLS stack,
/// which would reject it with a far less obvious message.
pub fn pem_from_secret_value(raw: Vec<u8>) -> Result<Vec<u8>> {
    if std::str::from_utf8(&raw).is_err() {
        return Err(Error::Missing("caBundle.secretRef: value not UTF-8 PEM"));
    }
    Ok(raw)
}

#[cfg(test)]
#[path = "ca_bundle_tests.rs"]
mod ca_bundle_tests;
