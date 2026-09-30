// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler: verify the template a `VMImage` names and publish
//! its VMID (ADR-0075).
//!
//! The controller takes `ProxmoxMachine.spec.templateVmid` from the image's
//! per-provider `resolvedRef`, parsed as a VMID. This reconciler is what
//! publishes it, after checking that the VMID really is a **template**: a live
//! guest must never be advertised as a clone source.
//!
//! Only `Template` sources are supported. Importing a `Url` image needs a
//! disk import the API does not offer (ADR-0074), so it is reported as
//! unsupported rather than skipped. banlieue creates no template here, so
//! there is nothing to clean up and no finalizer.
//!
//! Rows go under this provider's own field manager into the keyed
//! `status.perProvider` list, so they merge with other providers' rows.

use std::sync::Arc;

use banlieue_api::banlieue::{
    ImagePerProviderStatus, ImageSource, ImageSourceKind, Provider, VMImage, VMImageStatus,
};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_PROXMOX;
use banlieue_proxmox::ProxmoxApi;
use kube::{
    Resource, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::reconciler::provider::PROVIDER_CLASS_NAME;

/// `cluster_vms` `type` of a QEMU guest.
const KIND_QEMU: &str = "qemu";

/// Stable `reason` strings for `ImagePerProviderStatus.reason`.
pub mod reasons {
    /// The VMID is a template on this cluster.
    pub const RECONCILED: &str = "Reconciled";
    /// No guest has that VMID.
    pub const TEMPLATE_NOT_FOUND: &str = "TemplateNotFound";
    /// A guest has that VMID but is not a template.
    pub const NOT_A_TEMPLATE: &str = "NotATemplate";
    /// `ref` is not a VMID.
    pub const INVALID_REF: &str = "InvalidRef";
    /// Only `Template` sources are supported on Proxmox.
    pub const UNSUPPORTED_SOURCE_KIND: &str = "UnsupportedSourceKind";
    /// The API could not be reached.
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    /// The token was refused (401/403).
    pub const UNAUTHORIZED: &str = "Unauthorized";
    /// The credentials Secret is missing or malformed.
    pub const SECRET_UNAVAILABLE: &str = "SecretUnavailable";
}

/// The first source declared for class `proxmox`.
#[must_use]
pub fn find_proxmox_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources
        .iter()
        .find(|s| s.provider_class == PROVIDER_CLASS_NAME)
}

/// The stable reason for an API failure.
#[must_use]
pub fn api_failure_reason(e: &banlieue_proxmox::Error) -> &'static str {
    if e.is_unauthorized() {
        return reasons::UNAUTHORIZED;
    }
    reasons::CONNECT_FAILED
}

/// A not-ready row for `provider`.
#[must_use]
pub fn failure_row(provider: &Provider, reason: &str, message: String) -> ImagePerProviderStatus {
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready: false,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message: Some(message),
        zones: vec![],
    }
}

/// Resolve `source` against one cluster.
pub async fn row_for(
    api: &dyn ProxmoxApi,
    provider: &Provider,
    source: &ImageSource,
) -> ImagePerProviderStatus {
    if source.kind != ImageSourceKind::Template {
        return failure_row(
            provider,
            reasons::UNSUPPORTED_SOURCE_KIND,
            "Proxmox supports Template sources only (a template VMID)".to_string(),
        );
    }
    let Ok(vmid) = source.reference.trim().parse::<u32>() else {
        return failure_row(
            provider,
            reasons::INVALID_REF,
            format!("ref {:?} is not a template VMID", source.reference),
        );
    };
    let vms = match api.cluster_vms().await {
        Ok(v) => v,
        Err(e) => return failure_row(provider, api_failure_reason(&e), e.to_string()),
    };
    let Some(vm) = vms
        .iter()
        .find(|v| v.vmid == vmid && (v.kind.is_empty() || v.kind == KIND_QEMU))
    else {
        return failure_row(
            provider,
            reasons::TEMPLATE_NOT_FOUND,
            format!("no VM with VMID {vmid}"),
        );
    };
    if !vm.template {
        return failure_row(
            provider,
            reasons::NOT_A_TEMPLATE,
            format!("VMID {vmid} is not a template; convert it before use"),
        );
    }
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready: true,
        resolved_ref: Some(vmid.to_string()),
        reason: Some(reasons::RECONCILED.to_string()),
        message: None,
        zones: vec![],
    }
}

/// Reconcile one `VMImage`.
///
/// # Errors
/// [`Error`] on Kubernetes API failure. Backend and credential failures are
/// published as not-ready rows instead.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let name = image.name_any();
    let generation = image.metadata.generation.unwrap_or(0);
    if image.metadata.deletion_timestamp.is_some() {
        return Ok(requeue_long());
    }
    let Some(source) = find_proxmox_source(&image.spec.sources) else {
        return Ok(requeue_long());
    };

    let providers = list_providers(&ctx).await?;
    if providers.is_empty() {
        info!("no proxmox Providers in scope; leaving status untouched");
        return Ok(requeue_long());
    }
    let mut rows = Vec::with_capacity(providers.len());
    for provider in &providers {
        rows.push(row_for_provider(&ctx, provider, source).await);
    }
    let any_pending = rows.iter().any(|r| !r.ready);
    patch_vmimage_status(&ctx, &name, generation, rows).await?;
    Ok(if any_pending {
        requeue_default()
    } else {
        requeue_long()
    })
}

async fn row_for_provider(
    ctx: &Context,
    provider: &Provider,
    source: &ImageSource,
) -> ImagePerProviderStatus {
    let namespace = provider.namespace().unwrap_or_default();
    let creds = match crate::credentials::resolve(&ctx.client, &namespace, provider).await {
        Ok(c) => c,
        Err(e) => return failure_row(provider, reasons::SECRET_UNAVAILABLE, e.to_string()),
    };
    let api = match ctx.proxmox.build(&provider.spec.connection, &creds).await {
        Ok(a) => a,
        Err(e) => return failure_row(provider, reasons::CONNECT_FAILED, e.to_string()),
    };
    row_for(api.as_ref(), provider, source).await
}

/// Providers of class `proxmox` in scope for this process.
async fn list_providers(ctx: &Context) -> Result<Vec<Provider>> {
    let api: Api<Provider> = match ctx.namespace.as_deref() {
        Some(ns) => Api::namespaced(ctx.client.clone(), ns),
        None => Api::all(ctx.client.clone()),
    };
    Ok(api
        .list(&ListParams::default())
        .await?
        .into_iter()
        .filter(|p| p.spec.provider_class_ref.name == PROVIDER_CLASS_NAME)
        .filter(|p| {
            ctx.provider_name
                .as_deref()
                .is_none_or(|n| p.name_any() == n)
        })
        .collect())
}

/// Requeue after a reconcile error.
pub fn error_policy(_image: Arc<VMImage>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "vmimage reconcile error policy fired");
    requeue_on_error()
}

async fn patch_vmimage_status(
    ctx: &Context,
    name: &str,
    generation: i64,
    per_provider: Vec<ImagePerProviderStatus>,
) -> Result<()> {
    // Only this provider's rows: `buildArtifact` belongs to the imagebuilder
    // and the aggregate `Ready` to the controller (ADR-0010, ADR-0015).
    let status = VMImageStatus {
        per_provider,
        build_artifact: None,
        conditions: Vec::new(),
        observed_generation: Some(generation),
    };
    let patch = json!({
        "apiVersion": VMImage::api_version(&()).to_string(),
        "kind": VMImage::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": status,
    });
    let api: Api<VMImage> = Api::all(ctx.client.clone());
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER_PROVIDER_PROXMOX).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod image_tests;
