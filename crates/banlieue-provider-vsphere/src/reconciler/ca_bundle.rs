// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Resolve `Provider.spec.connection.caBundle` to PEM text (ADR-0008).
//!
//! The resolution itself is shared — see [`banlieue_provider_sdk::ca_bundle`],
//! which every TLS-speaking backend uses. What remains here is the one thing
//! that is genuinely vSphere's: an absent bundle is **not** an error, because
//! vCenter is often fronted by a certificate the system trust roots already
//! accept. libvirt makes the opposite call for the opposite reason.
//!
//! Classification and the "exactly one source" invariant are tested in the SDK
//! (`ca_bundle_tests.rs` there); duplicating those cases here would only test a
//! re-export.
//!
//! The `vim_rs`-facing client factory takes the already-resolved PEM (see
//! [`crate::client::VSphereClientFactory::build`]), so cluster access stays in
//! the reconciler layer.

use banlieue_api::common::CABundleSource;
use banlieue_provider_sdk::ca_bundle;

use crate::context::Context;
use crate::error::Result;

/// Resolve an optional [`CABundleSource`] to PEM text.
///
/// `None` means no bundle was configured, and the client falls back to the
/// system trust roots.
///
/// # Errors
/// - [`crate::error::Error::Sdk`] if zero or more than one source is set, or
///   the referenced ConfigMap/Secret or key is absent.
pub async fn resolve_ca_bundle(
    ctx: &Context,
    namespace: &str,
    source: &Option<CABundleSource>,
) -> Result<Option<String>> {
    let Some(pem) = ca_bundle::resolve(&ctx.client, namespace, source).await? else {
        return Ok(None);
    };
    // The SDK already rejected non-UTF-8, so this cannot fail.
    Ok(Some(String::from_utf8_lossy(&pem).into_owned()))
}
