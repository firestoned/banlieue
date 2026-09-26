// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pure encode and decode halves of each call, testable with no socket
//! (ADR-0061 Decision 3).

use crate::error::{Error, Result};
use crate::types::{GuestPlan, VmConfigRequest, VmInfo, VmmPing};
use http::Method;

/// Path prefix every endpoint lives under.
pub const API_PREFIX: &str = "/api/v1/";

/// The release this crate is written against. Older VMMs are refused.
/// Kept in step with `spec/PIN`; `spec_tests.rs` checks that.
pub const PINNED_VERSION: VmmVersion = VmmVersion {
    major: 53,
    minor: 0,
    patch: 0,
};

/// The endpoints banlieue calls. Nothing else gets a variant until something
/// uses it (ADR-0061 Decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// `GET vmm.ping`.
    VmmPing,
    /// `PUT vm.create`.
    VmCreate,
    /// `PUT vm.boot`.
    VmBoot,
    /// `GET vm.info`.
    VmInfo,
    /// `PUT vm.power-button`: ask the guest to shut down (ACPI).
    VmPowerButton,
    /// `PUT vm.shutdown`: stop the VM now.
    VmShutdown,
    /// `PUT vm.delete`.
    VmDelete,
    /// `PUT vmm.shutdown`: end the VMM process.
    VmmShutdown,
    /// `PUT vm.remove-device`: hot-unplug a device by id (ADR-0065).
    VmRemoveDevice,
}

impl Endpoint {
    /// Every endpoint, for tests that walk them all.
    pub const ALL: [Self; 9] = [
        Self::VmmPing,
        Self::VmCreate,
        Self::VmBoot,
        Self::VmInfo,
        Self::VmPowerButton,
        Self::VmShutdown,
        Self::VmDelete,
        Self::VmmShutdown,
        Self::VmRemoveDevice,
    ];

    /// HTTP method.
    #[must_use]
    pub fn method(self) -> Method {
        match self {
            Self::VmmPing | Self::VmInfo => Method::GET,
            _ => Method::PUT,
        }
    }

    /// Request path.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::VmmPing => "/api/v1/vmm.ping",
            Self::VmCreate => "/api/v1/vm.create",
            Self::VmBoot => "/api/v1/vm.boot",
            Self::VmInfo => "/api/v1/vm.info",
            Self::VmPowerButton => "/api/v1/vm.power-button",
            Self::VmShutdown => "/api/v1/vm.shutdown",
            Self::VmDelete => "/api/v1/vm.delete",
            Self::VmmShutdown => "/api/v1/vmm.shutdown",
            Self::VmRemoveDevice => "/api/v1/vm.remove-device",
        }
    }
}

/// `vm.create` body for `plan`.
#[must_use]
pub fn encode_vm_create(plan: &GuestPlan) -> Vec<u8> {
    // Serializing a struct of strings, paths, numbers and bools cannot fail.
    serde_json::to_vec(&VmConfigRequest::for_guest(plan)).unwrap_or_default()
}

/// `vm.remove-device` body.
#[must_use]
pub fn encode_remove_device(id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "id": id })).unwrap_or_default()
}

/// Decode a `vmm.ping` body.
///
/// # Errors
/// [`Error::Decode`] when the body is not a ping response.
pub fn decode_ping(body: &[u8]) -> Result<VmmPing> {
    Ok(serde_json::from_slice(body)?)
}

/// Decode a `vm.info` body.
///
/// # Errors
/// [`Error::Decode`] when the body is not a VM info response.
pub fn decode_info(body: &[u8]) -> Result<VmInfo> {
    Ok(serde_json::from_slice(body)?)
}

/// The error for a non-success reply.
///
/// The VMM reports errors as a JSON array of messages, outermost first
/// (captured from v53.0). A body of any other shape is kept verbatim as the
/// single message rather than dropped.
#[must_use]
pub fn api_error(status: u16, body: &[u8]) -> Error {
    let messages = serde_json::from_slice::<Vec<String>>(body)
        .unwrap_or_else(|_| vec![String::from_utf8_lossy(body).trim().to_string()]);
    Error::Api { status, messages }
}

/// A VMM version, `major.minor.patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VmmVersion {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Patch.
    pub patch: u32,
}

/// Parse `53.0.0`, `v53.0.0`, `53.0` or `53.0.0-dirty`.
#[must_use]
pub fn parse_version(s: &str) -> Option<VmmVersion> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some(VmmVersion {
        major,
        minor,
        patch,
    })
}

/// Accept a VMM at or above [`PINNED_VERSION`]'s `major.minor`.
///
/// # Errors
/// [`Error::VersionUnsupported`] for an older or unparseable version.
pub fn check_version(ping: &VmmPing) -> Result<VmmVersion> {
    let minimum = format!("{}.{}", PINNED_VERSION.major, PINNED_VERSION.minor);
    let unsupported = || Error::VersionUnsupported {
        found: ping.version.clone(),
        minimum: minimum.clone(),
    };
    let found = parse_version(&ping.version).ok_or_else(unsupported)?;
    if (found.major, found.minor) < (PINNED_VERSION.major, PINNED_VERSION.minor) {
        return Err(unsupported());
    }
    Ok(found)
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod wire_tests;
