// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler: the AMI a `VMImage` boots from on EC2.
//!
//! - **`Template`**: the source's `ref` is the ID of an AMI an admin
//!   published by hand (`ami-` and 8 or 17 hex digits). Admin-asserted, like
//!   Azure's `Template` and a Proxmox template VMID, and **regional**: one row
//!   per `Provider` with `zones` empty, which the scheduler reads as ready in
//!   every zone. The provider reads the AMI with `DescribeImages` and records
//!   what the controller and scheduler decide on in the row's `ec2`
//!   attributes: architecture, root device name, boot mode and TPM support
//!   (roadmap 24 phase E). An AMI EC2 does not know (or this identity may not
//!   see), one not `available`, one not EBS-backed, or one of an
//!   architecture banlieue does not run is refused with a named reason.
//!   Throttling and other transient failures keep the row as it was, so a
//!   ready image never flips to not ready on a slow EC2. banlieue never
//!   touches the AMI (ADR-0028), so the `VMImage` gets no finalizer.
//! - **`Url`** and every other kind: not ready, `UnsupportedSourceKind`.
//!   Publishing a `Url` source into an AMI (ADR-0162 Decision 9) is roadmap
//!   24 phase D.

use std::sync::Arc;

use banlieue_api::aws;
use banlieue_api::banlieue::vmimage::{MAX_EC2_IMAGE_ATTRIBUTE_LEN, MAX_EC2_ROOT_DEVICE_NAME_LEN};
use banlieue_api::banlieue::{
    Architecture, Ec2ImageAttributes, ImagePerProviderStatus, ImageSource, ImageSourceKind,
    Provider, VMImage,
};
use banlieue_aws::Ec2Api;
use banlieue_aws::types::ImageInfo;
use banlieue_provider_sdk::reconciler::{
    list_providers_of_class, providers_in_scope, requeue_default, requeue_long,
};
use banlieue_provider_sdk::ssa::{FIELD_MANAGER_PROVIDER_EC2, apply_status, status_field_manager};
use banlieue_provider_sdk::status::{image_row, provider_rows_status};
use kube::{ResourceExt, api::Api, runtime::controller::Action};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::Result;
use crate::reconciler::provider::PROVIDER_CLASS_NAME;

/// Stable `reason` strings for `ImagePerProviderStatus.reason`.
pub mod reasons {
    /// The reference is an AMI ID.
    pub const RECONCILED: &str = "Reconciled";
    /// `ref` is not an AMI ID.
    pub const INVALID_REF: &str = "InvalidRef";
    /// Only `Template` sources are served on EC2 for now.
    pub const UNSUPPORTED_SOURCE_KIND: &str = "UnsupportedSourceKind";
    /// EC2 does not know the AMI, or this identity may not see it.
    pub const AMI_NOT_FOUND: &str = "AmiNotFound";
    /// The AMI's state is not `available` (`pending`, `failed`,
    /// `deregistered`, ...).
    pub const AMI_NOT_AVAILABLE: &str = "AmiNotAvailable";
    /// The AMI is instance-store backed; banlieue launches EBS only.
    pub const AMI_NOT_EBS_BACKED: &str = "AmiNotEbsBacked";
    /// The AMI's architecture is neither `x86_64` nor `arm64`.
    pub const AMI_ARCHITECTURE_UNSUPPORTED: &str = "AmiArchitectureUnsupported";
    /// The AMI's root device name is not a `/dev/` device name.
    pub const AMI_ROOT_DEVICE_INVALID: &str = "AmiRootDeviceInvalid";
    /// The AMI could not be read yet (throttled, EC2 or the network
    /// failing) and no earlier reading exists.
    pub const AMI_UNVERIFIED: &str = "AmiUnverified";
    /// EC2 refused to describe the AMI (policy, credentials, a malformed
    /// ID).
    pub const AMI_DESCRIBE_FAILED: &str = "AmiDescribeFailed";
}

/// EC2's `architecture` of an `amd64` image.
const EC2_ARCH_X86_64: &str = "x86_64";
/// EC2's `architecture` of an `arm64` image.
const EC2_ARCH_ARM64: &str = "arm64";
/// The prefix of a device name.
const DEVICE_PREFIX: &str = "/dev/";

/// The first source declared for class `ec2`.
#[must_use]
pub fn find_ec2_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources
        .iter()
        .find(|s| s.provider_class == PROVIDER_CLASS_NAME)
}

/// The row for `provider` and `source`, decided from the source alone.
///
/// A `Template` whose `ref` is an AMI ID is ready with that ID (trimmed) as
/// `resolvedRef`; a `Template` with any other `ref` is `InvalidRef`; every
/// other kind is `UnsupportedSourceKind`.
#[must_use]
pub fn row_for(provider: &Provider, source: &ImageSource) -> ImagePerProviderStatus {
    if source.kind != ImageSourceKind::Template {
        return image_row(
            provider,
            false,
            reasons::UNSUPPORTED_SOURCE_KIND,
            Some(format!(
                "EC2 takes a Template source (the ID of a hand-published AMI); publishing a \
                 {:?} source into an AMI is roadmap 24 phase D",
                source.kind
            )),
        );
    }
    let ami = source.reference.trim();
    if !aws::is_ami_id(ami) {
        return image_row(
            provider,
            false,
            reasons::INVALID_REF,
            Some(format!(
                "ref {:?} is not an AMI ID (ami- and 8 or 17 hex digits)",
                source.reference
            )),
        );
    }
    ImagePerProviderStatus {
        resolved_ref: Some(ami.to_string()),
        ..image_row(provider, true, reasons::RECONCILED, None)
    }
}

/// The `banlieue` architecture of EC2's `architecture`, or `None` for one
/// banlieue does not run (`i386`, `x86_64_mac`, `arm64_mac`).
fn architecture_of(ec2: &str) -> Option<Architecture> {
    match ec2 {
        EC2_ARCH_X86_64 => Some(Architecture::Amd64),
        EC2_ARCH_ARM64 => Some(Architecture::Arm64),
        _ => None,
    }
}

/// Whether `name` is `/dev/` and lowercase letters or digits, within the
/// recorded length.
fn is_device_name(name: &str) -> bool {
    name.len() <= MAX_EC2_ROOT_DEVICE_NAME_LEN as usize
        && name.strip_prefix(DEVICE_PREFIX).is_some_and(|dev| {
            !dev.is_empty()
                && dev
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// `value` when it fits the recorded length, else `None` (never truncated:
/// a cut value could read as another).
fn bounded(value: Option<&String>) -> Option<String> {
    value
        .filter(|v| v.len() <= MAX_EC2_IMAGE_ATTRIBUTE_LEN as usize)
        .cloned()
}

/// The row for `provider` and the AMI `ami` as `DescribeImages` reported it
/// (`None`: EC2 does not know it, or this identity may not see it). Pure.
#[must_use]
pub fn row_from_image(
    provider: &Provider,
    ami: &str,
    image: Option<&ImageInfo>,
) -> ImagePerProviderStatus {
    let refused = |reason: &str, message: String| image_row(provider, false, reason, Some(message));
    let Some(image) = image else {
        return refused(
            reasons::AMI_NOT_FOUND,
            format!("EC2 does not know {ami}, or this Provider's role may not see it"),
        );
    };
    if !image.is_available() {
        return refused(
            reasons::AMI_NOT_AVAILABLE,
            format!("{ami} is {}, not available", image.state),
        );
    }
    if !image.is_ebs_backed() {
        return refused(
            reasons::AMI_NOT_EBS_BACKED,
            format!(
                "{ami} has root device type {}; banlieue launches EBS-backed AMIs only",
                image.root_device_type
            ),
        );
    }
    let Some(architecture) = architecture_of(&image.architecture) else {
        return refused(
            reasons::AMI_ARCHITECTURE_UNSUPPORTED,
            format!(
                "{ami} is {}; banlieue runs x86_64 (amd64) and arm64",
                image.architecture
            ),
        );
    };
    if !is_device_name(&image.root_device_name) {
        return refused(
            reasons::AMI_ROOT_DEVICE_INVALID,
            format!(
                "{ami} has root device {:?}, not a /dev/ device name",
                image.root_device_name
            ),
        );
    }
    ImagePerProviderStatus {
        resolved_ref: Some(ami.to_string()),
        min_os_disk_size_gib: image.root_volume_size_gib,
        ec2: Some(Ec2ImageAttributes {
            architecture,
            root_device_name: image.root_device_name.clone(),
            boot_mode: bounded(image.boot_mode.as_ref()),
            tpm_support: bounded(image.tpm_support.as_ref()),
            ena_support: image.ena_support,
        }),
        ..image_row(provider, true, reasons::RECONCILED, None)
    }
}

/// Whether `error` may clear by waiting: throttling, an EC2 server error or
/// no answer. Everything else (policy, credentials, a malformed ID) is a
/// refusal the row reports.
#[must_use]
pub fn is_transient(error: &banlieue_aws::Error) -> bool {
    matches!(
        error,
        banlieue_aws::Error::Throttled(_)
            | banlieue_aws::Error::Server(_)
            | banlieue_aws::Error::Transport(_)
    )
}

/// What [`template_row`] decided.
#[derive(Clone, Debug, PartialEq)]
pub struct RowDecision {
    /// The row to write.
    pub row: ImagePerProviderStatus,
    /// True when EC2 could not answer for now: read again soon.
    pub transient: bool,
}

/// The row for `provider` and an `ec2` `source`: [`row_for`]'s checks, then
/// `DescribeImages` for a `Template` AMI ([`row_from_image`]).
///
/// On a transient failure ([`is_transient`]) the `existing` row is kept when
/// it is for the same AMI, so a ready image stays ready; with no such row the
/// result is a not-ready [`reasons::AMI_UNVERIFIED`] row.
pub async fn template_row(
    api: &dyn Ec2Api,
    provider: &Provider,
    source: &ImageSource,
    existing: Option<&ImagePerProviderStatus>,
) -> RowDecision {
    let checked = row_for(provider, source);
    let Some(ami) = checked.resolved_ref.clone() else {
        return RowDecision {
            row: checked,
            transient: false,
        };
    };
    match api.describe_image(&ami).await {
        Ok(image) => RowDecision {
            row: row_from_image(provider, &ami, image.as_ref()),
            transient: false,
        },
        Err(e) if is_transient(&e) => {
            warn!(error = %e, "DescribeImages failed for now; keeping the row as it was");
            let kept = existing
                .filter(|r| r.resolved_ref.as_deref() == Some(ami.as_str()))
                .cloned();
            RowDecision {
                row: kept.unwrap_or_else(|| {
                    image_row(
                        provider,
                        false,
                        reasons::AMI_UNVERIFIED,
                        Some(format!("{ami} could not be read yet: {e}")),
                    )
                }),
                transient: true,
            }
        }
        Err(e) => RowDecision {
            row: image_row(
                provider,
                false,
                reasons::AMI_DESCRIBE_FAILED,
                Some(format!("DescribeImages for {ami} was refused: {e}")),
            ),
            transient: false,
        },
    }
}

/// This `VMImage`'s existing row for `provider`, if any.
fn existing_row<'a>(image: &'a VMImage, provider: &Provider) -> Option<&'a ImagePerProviderStatus> {
    let namespace = provider.namespace().unwrap_or_default();
    let name = provider.name_any();
    image
        .status
        .as_ref()?
        .per_provider
        .iter()
        .find(|r| r.provider_name == name && r.provider_namespace == namespace)
}

/// Reconcile one `VMImage`: one row per `ec2` `Provider` this workload
/// serves.
///
/// # Errors
/// [`crate::Error`] on Kubernetes API failure.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    if image.metadata.deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }
    let Some(source) = find_ec2_source(&image.spec.sources) else {
        return Ok(requeue_long());
    };
    let of_class =
        list_providers_of_class(&ctx.client, ctx.namespace.as_deref(), PROVIDER_CLASS_NAME).await?;
    let providers = providers_in_scope(of_class, ctx.provider_name.as_deref());
    if providers.is_empty() {
        info!("no ec2 Providers in scope; leaving status untouched");
        return Ok(requeue_long());
    }
    let mut rows: Vec<ImagePerProviderStatus> = Vec::with_capacity(providers.len());
    let mut any_transient = false;
    for provider in &providers {
        let existing = existing_row(&image, provider);
        let decision = match ctx.ec2.build(provider).await {
            Ok(api) => template_row(api.as_ref(), provider, source, existing).await,
            // No client (an unusable connection, STS unreachable): the
            // Provider's own status reports why; keep what was recorded.
            Err(e) => {
                warn!(error = %e, "no EC2 client for this Provider; keeping the row as it was");
                let checked = row_for(provider, source);
                RowDecision {
                    row: existing.cloned().unwrap_or_else(|| {
                        if checked.resolved_ref.is_none() {
                            return checked;
                        }
                        image_row(
                            provider,
                            false,
                            reasons::AMI_UNVERIFIED,
                            Some(format!("the AMI could not be read yet: {e}")),
                        )
                    }),
                    transient: true,
                }
            }
        };
        any_transient |= decision.transient;
        rows.push(decision.row);
    }

    // ADR-0087 (Amendment 2026-11-01): started for one Provider, this
    // workload writes its row under that Provider's own manager.
    let namespace = providers
        .first()
        .and_then(ResourceExt::namespace)
        .unwrap_or_default();
    let field_manager = status_field_manager(
        FIELD_MANAGER_PROVIDER_EC2,
        ctx.provider_name.as_deref(),
        &namespace,
    );
    let api: Api<VMImage> = Api::all(ctx.client.clone());
    let status = provider_rows_status(rows, image.metadata.generation.unwrap_or(0));
    apply_status(&api, &image.name_any(), &field_manager, &status, true).await?;
    // A deregistered AMI is noticed on the long cadence; a throttled read is
    // tried again soon.
    Ok(if any_transient {
        requeue_default()
    } else {
        requeue_long()
    })
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod image_tests;
