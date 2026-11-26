// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The Azure clouds a `Provider` may point at, and nothing else.
//!
//! `spec.connection.endpoint` receives the ARM bearer token on every call,
//! and `spec.connection.azure.authorityHost` receives the projected
//! ServiceAccount token, which Entra ID exchanges for the managed identity's
//! token. Anyone allowed to update a `Provider` could otherwise redirect
//! either token to a host of their choosing; an `azure` `Provider` has no
//! `credentialsRef`, so ADR-0157's Secret-read authorization policy never
//! applies to it. The two hosts are therefore pinned here, in the binary,
//! to the known clouds and paired with each other, and again at admission
//! (`deploy/admission/provider-connection.yaml`).
//!
//! The comparison is exact after trimming whitespace and at most one
//! trailing slash: no case folding, no port, no path, no userinfo, so
//! `https://management.azure.com.evil.example`,
//! `https://management.azure.com@evil.example` and
//! `https://management.azure.com:8443` are all refused.

use crate::error::{Error, Result};

/// Public Azure Resource Manager.
pub const PUBLIC_ARM_ENDPOINT: &str = "https://management.azure.com";
/// Public Azure Entra ID login host.
pub const PUBLIC_AUTHORITY_HOST: &str = "https://login.microsoftonline.com";
/// Azure US Government Resource Manager.
pub const US_GOV_ARM_ENDPOINT: &str = "https://management.usgovcloudapi.net";
/// Azure US Government Entra ID login host.
pub const US_GOV_AUTHORITY_HOST: &str = "https://login.microsoftonline.us";
/// Azure operated by 21Vianet (China) Resource Manager.
pub const CHINA_ARM_ENDPOINT: &str = "https://management.chinacloudapi.cn";
/// Azure operated by 21Vianet (China) Entra ID login host.
pub const CHINA_AUTHORITY_HOST: &str = "https://login.chinacloudapi.cn";

/// Name of the public cloud.
pub const PUBLIC_CLOUD_NAME: &str = "AzurePublicCloud";
/// Name of the US Government cloud.
pub const US_GOV_CLOUD_NAME: &str = "AzureUSGovernment";
/// Name of the China cloud.
pub const CHINA_CLOUD_NAME: &str = "AzureChinaCloud";

/// The field an unknown endpoint is reported against.
pub const ENDPOINT_FIELD: &str = "Provider.spec.connection.endpoint";
/// The field a mismatched authority is reported against.
pub const AUTHORITY_FIELD: &str = "Provider.spec.connection.azure.authorityHost";

/// The suffix of an Entra ID scope that asks for every permission granted.
const DEFAULT_SCOPE_SUFFIX: &str = "/.default";

/// One Azure cloud: its Resource Manager and the login host that mints
/// tokens for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AzureCloud {
    /// The cloud's well-known name.
    pub name: &'static str,
    /// Resource Manager base URL, no trailing slash.
    pub arm_endpoint: &'static str,
    /// Entra ID login host, no trailing slash.
    pub authority_host: &'static str,
}

impl AzureCloud {
    /// The Entra ID scope for this cloud's Resource Manager.
    #[must_use]
    pub fn arm_scope(&self) -> String {
        format!("{}{DEFAULT_SCOPE_SUFFIX}", self.arm_endpoint)
    }
}

/// Every cloud a `Provider` may name, each endpoint paired with its own
/// login host.
pub const AZURE_CLOUDS: [AzureCloud; 3] = [
    AzureCloud {
        name: PUBLIC_CLOUD_NAME,
        arm_endpoint: PUBLIC_ARM_ENDPOINT,
        authority_host: PUBLIC_AUTHORITY_HOST,
    },
    AzureCloud {
        name: US_GOV_CLOUD_NAME,
        arm_endpoint: US_GOV_ARM_ENDPOINT,
        authority_host: US_GOV_AUTHORITY_HOST,
    },
    AzureCloud {
        name: CHINA_CLOUD_NAME,
        arm_endpoint: CHINA_ARM_ENDPOINT,
        authority_host: CHINA_AUTHORITY_HOST,
    },
];

/// Public Azure's managed-disk upload domain: a direct-upload write SAS
/// names a host such as `md-<id>.z<n>.blob.storage.azure.net`.
pub const PUBLIC_UPLOAD_HOST_SUFFIX: &str = "blob.storage.azure.net";
/// Azure US Government's managed-disk upload domain.
pub const US_GOV_UPLOAD_HOST_SUFFIX: &str = "blob.storage.usgovcloudapi.net";
/// Azure operated by 21Vianet's managed-disk upload domain.
pub const CHINA_UPLOAD_HOST_SUFFIX: &str = "blob.storage.chinacloudapi.cn";

/// The only domain the publish Job sends a disk's write SAS (and the
/// image's bytes) to, for `cloud` (ADR-0162). Pinned here beside the ARM
/// allowlist for the same reason: the SAS host comes back from ARM, and a
/// fixed suffix is what makes "ARM said so" checkable.
#[must_use]
pub fn upload_host_suffix(cloud: &AzureCloud) -> &'static str {
    match cloud.name {
        US_GOV_CLOUD_NAME => US_GOV_UPLOAD_HOST_SUFFIX,
        CHINA_CLOUD_NAME => CHINA_UPLOAD_HOST_SUFFIX,
        _ => PUBLIC_UPLOAD_HOST_SUFFIX,
    }
}

/// `value` trimmed, with at most one trailing slash removed.
fn normalize(value: &str) -> &str {
    let trimmed = value.trim();
    trimmed.strip_suffix('/').unwrap_or(trimmed)
}

fn allowed_endpoints() -> String {
    AZURE_CLOUDS
        .iter()
        .map(|c| c.arm_endpoint)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The cloud `endpoint` and `authority_host` name together.
///
/// An empty `endpoint` is public Azure; an unset or blank `authority_host`
/// is the cloud's own login host.
///
/// # Errors
/// [`Error::Invalid`] (surfaced as `Ready=False`, reason
/// `ConnectionInvalid`) for an endpoint that is not exactly a known
/// Resource Manager ([`ENDPOINT_FIELD`]), or an authority that is not that
/// cloud's login host ([`AUTHORITY_FIELD`]).
pub fn resolve_cloud(endpoint: &str, authority_host: Option<&str>) -> Result<&'static AzureCloud> {
    let endpoint = normalize(endpoint);
    let endpoint = if endpoint.is_empty() {
        PUBLIC_ARM_ENDPOINT
    } else {
        endpoint
    };
    let cloud = AZURE_CLOUDS
        .iter()
        .find(|c| c.arm_endpoint == endpoint)
        .ok_or_else(|| Error::Invalid {
            what: ENDPOINT_FIELD,
            detail: format!(
                "{endpoint:?} is not a known Azure Resource Manager endpoint; it receives the ARM token, so it must be one of: {}",
                allowed_endpoints()
            ),
        })?;
    let Some(authority) = authority_host.map(normalize).filter(|a| !a.is_empty()) else {
        return Ok(cloud);
    };
    if authority != cloud.authority_host {
        return Err(Error::Invalid {
            what: AUTHORITY_FIELD,
            detail: format!(
                "{authority:?} is not the login host of {} ({}); it receives the ServiceAccount token, so it must be {} or unset",
                cloud.name, cloud.arm_endpoint, cloud.authority_host
            ),
        });
    }
    Ok(cloud)
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod cloud_tests;
