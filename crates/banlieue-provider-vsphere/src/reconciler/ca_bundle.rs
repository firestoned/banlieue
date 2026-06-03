// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Resolve `Provider.spec.connection.caBundle` to PEM text (ADR-0008).
//!
//! A [`CABundleSource`] is a value-or-source: inline PEM, or a `configMapRef` /
//! `secretRef` naming a key (default [`DEFAULT_CA_BUNDLE_KEY`]) in the Provider's
//! namespace. Resolution needs cluster access, so it lives here in the reconciler
//! layer rather than in the `vim_rs`-facing client factory — the factory takes
//! the already-resolved PEM (see [`crate::client::VSphereClientFactory::build`]).
//!
//! The "exactly one source" invariant is validated here (controller-side floor),
//! mirroring the `ValidatingAdmissionPolicy` that enforces it at admission.

use banlieue_api::common::{CABundleSource, DEFAULT_CA_BUNDLE_KEY, KeySelector};
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::Api;

use crate::context::Context;
use crate::error::{Error, Result};

/// Outcome of the pure (no-I/O) classification step of [`resolve_ca_bundle`].
///
/// Separated out so the validation + inline path is unit-testable without a kube
/// cluster; only [`Plan::ConfigMap`] / [`Plan::Secret`] require an API read.
#[derive(Debug, PartialEq, Eq)]
enum Plan<'a> {
    /// No bundle configured — use the system trust roots.
    None,
    /// PEM available directly from the spec.
    Inline(&'a str),
    /// Read the given key from this ConfigMap.
    ConfigMap(&'a KeySelector),
    /// Read the given key from this Secret.
    Secret(&'a KeySelector),
}

/// Pure classification of a [`CABundleSource`] into a [`Plan`], enforcing the
/// "exactly one source" invariant. No cluster access.
///
/// # Errors
/// [`Error::Vsphere`] when zero or more than one source is set.
fn plan(source: &Option<CABundleSource>) -> Result<Plan<'_>> {
    let Some(source) = source else {
        return Ok(Plan::None);
    };
    // Controller-side enforcement of "exactly one of inline/configMapRef/secretRef".
    source
        .validate()
        .map_err(|msg| Error::Vsphere(msg.to_string()))?;

    if let Some(pem) = &source.inline {
        return Ok(Plan::Inline(pem));
    }
    if let Some(sel) = &source.config_map_ref {
        return Ok(Plan::ConfigMap(sel));
    }
    if let Some(sel) = &source.secret_ref {
        return Ok(Plan::Secret(sel));
    }
    // Unreachable: validate() guarantees exactly one branch fired.
    Err(Error::Vsphere(
        "caBundle: no source resolved after validation".to_string(),
    ))
}

/// Resolve an optional [`CABundleSource`] to PEM text.
///
/// - `None` → `Ok(None)`: no bundle, the client uses the system trust roots.
/// - inline → the PEM verbatim.
/// - `configMapRef` → value of the key (default `ca.crt`) in the named ConfigMap.
/// - `secretRef` → value of the key (default `ca.crt`) in the named Secret.
///
/// All references are namespace-local (`namespace`), like the credentials Secret.
///
/// # Errors
/// - [`Error::Vsphere`] if more than one or zero sources are set (invariant).
/// - [`Error::Missing`] if the referenced ConfigMap/Secret or key is absent.
/// - [`Error::Kube`] for any other API error.
pub async fn resolve_ca_bundle(
    ctx: &Context,
    namespace: &str,
    source: &Option<CABundleSource>,
) -> Result<Option<String>> {
    match plan(source)? {
        Plan::None => Ok(None),
        Plan::Inline(pem) => Ok(Some(pem.to_string())),
        Plan::ConfigMap(sel) => Ok(Some(read_config_map_key(ctx, namespace, sel).await?)),
        Plan::Secret(sel) => Ok(Some(read_secret_key(ctx, namespace, sel).await?)),
    }
}

/// Read `selector.key` (default `ca.crt`) from a ConfigMap's `data`.
async fn read_config_map_key(
    ctx: &Context,
    namespace: &str,
    selector: &KeySelector,
) -> Result<String> {
    let key = selector.key_or(DEFAULT_CA_BUNDLE_KEY);
    let api: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), namespace);
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
        .cloned()
        .ok_or(Error::Missing("caBundle.configMapRef: key not found"))
}

/// Read `selector.key` (default `ca.crt`) from a Secret's `data` (base64-decoded
/// by kube into raw bytes; interpreted as UTF-8 PEM).
async fn read_secret_key(ctx: &Context, namespace: &str, selector: &KeySelector) -> Result<String> {
    let key = selector.key_or(DEFAULT_CA_BUNDLE_KEY);
    let api: Api<Secret> = Api::namespaced(ctx.client.clone(), namespace);
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
    String::from_utf8(raw.0).map_err(|_| Error::Missing("caBundle.secretRef: value not UTF-8 PEM"))
}

#[cfg(test)]
#[path = "ca_bundle_tests.rs"]
mod ca_bundle_tests;
