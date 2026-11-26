// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Customer-managed keys: the disk encryption set a `Provider` names
//! (ADR-0172 Decision 2).
//!
//! banlieue never creates, reads the key of, or deletes a disk encryption
//! set. It only names one by resource ID on what it creates: a VM's OS disk,
//! and the publish Job's upload disk, snapshot and image version. The ID is
//! checked here before any call, so a typo surfaces on the `Provider` and not
//! as a refused create: it must be a `Microsoft.Compute/diskEncryptionSets`
//! resource, and in the subscription of the resources it encrypts (Azure's
//! restriction for disks, snapshots and gallery images).

use serde_json::{Value, json};

use crate::error::{Error, Result};

/// The error code for a malformed or misplaced disk encryption set ID.
pub const CODE_INVALID_DISK_ENCRYPTION_SET: &str = "InvalidDiskEncryptionSetId";
/// `encryption.type` of a disk or snapshot encrypted with a customer key.
pub const ENCRYPTION_AT_REST_WITH_CUSTOMER_KEY: &str = "EncryptionAtRestWithCustomerKey";

/// The fixed segments of a disk encryption set ID, in order, with a `None`
/// where a variable segment goes:
/// `/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Compute/diskEncryptionSets/{name}`.
const SHAPE: [Option<&str>; SEGMENTS] = [
    Some("subscriptions"),
    None,
    Some("resourceGroups"),
    None,
    Some("providers"),
    Some("Microsoft.Compute"),
    Some("diskEncryptionSets"),
    None,
];
/// Segments of a disk encryption set ID after the leading slash.
const SEGMENTS: usize = 8;
const SUBSCRIPTION_AT: usize = 1;
const RESOURCE_GROUP_AT: usize = 3;
const NAME_AT: usize = 7;
/// Azure's limit on a resource group name.
const MAX_RESOURCE_GROUP_LEN: usize = 90;
/// Azure's limit on a disk encryption set name.
const MAX_DES_NAME_LEN: usize = 80;
/// Length of a subscription GUID.
const GUID_LEN: usize = 36;
/// Where the dashes of a GUID go.
const GUID_DASHES: [usize; 4] = [8, 13, 18, 23];

/// A disk encryption set's resource ID, in parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskEncryptionSetId {
    /// The subscription it is in.
    pub subscription_id: String,
    /// Its resource group.
    pub resource_group: String,
    /// Its name.
    pub name: String,
}

fn invalid(message: String) -> Error {
    Error::InvalidRequest {
        code: CODE_INVALID_DISK_ENCRYPTION_SET.to_string(),
        message,
    }
}

fn is_guid(s: &str) -> bool {
    s.len() == GUID_LEN
        && s.char_indices().all(|(i, c)| {
            if GUID_DASHES.contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Azure resource group names: letters, digits, `-`, `_`, `.`, `(`, `)`,
/// not ending in `.`.
fn is_resource_group(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_RESOURCE_GROUP_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '(' | ')'))
        && !s.ends_with('.')
}

/// Disk encryption set names: letters, digits, `-` and `_`.
fn is_des_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_DES_NAME_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Parses a disk encryption set resource ID. The fixed segments match
/// case-insensitively, as ARM matches them.
///
/// # Errors
/// [`Error::InvalidRequest`] with [`CODE_INVALID_DISK_ENCRYPTION_SET`] for
/// anything else.
pub fn parse_disk_encryption_set_id(id: &str) -> Result<DiskEncryptionSetId> {
    let shape_error = || {
        invalid(format!(
            "{id:?} is not a disk encryption set ID: /subscriptions/<id>/resourceGroups/<rg>/providers/Microsoft.Compute/diskEncryptionSets/<name>"
        ))
    };
    let Some(rest) = id.strip_prefix('/') else {
        return Err(shape_error());
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() != SHAPE.len() {
        return Err(shape_error());
    }
    for (part, fixed) in parts.iter().zip(SHAPE) {
        if let Some(fixed) = fixed
            && !part.eq_ignore_ascii_case(fixed)
        {
            return Err(shape_error());
        }
    }
    let (subscription, group, name) = (
        parts[SUBSCRIPTION_AT],
        parts[RESOURCE_GROUP_AT],
        parts[NAME_AT],
    );
    if !is_guid(subscription) || !is_resource_group(group) || !is_des_name(name) {
        return Err(shape_error());
    }
    Ok(DiskEncryptionSetId {
        subscription_id: subscription.to_string(),
        resource_group: group.to_string(),
        name: name.to_string(),
    })
}

/// Checks `id` is a disk encryption set in `subscription_id`.
///
/// # Errors
/// As [`parse_disk_encryption_set_id`], and when the set is in another
/// subscription.
pub fn check_disk_encryption_set_id(id: &str, subscription_id: &str) -> Result<()> {
    let parsed = parse_disk_encryption_set_id(id)?;
    if !parsed.subscription_id.eq_ignore_ascii_case(subscription_id) {
        return Err(invalid(format!(
            "disk encryption set {id} is in subscription {}, not {subscription_id}; Azure requires the set in the subscription of the disks it encrypts",
            parsed.subscription_id
        )));
    }
    Ok(())
}

/// The `properties.encryption` of a disk or snapshot encrypted with the
/// set `id`.
#[must_use]
pub fn disk_encryption_body(id: &str) -> Value {
    json!({
        "type": ENCRYPTION_AT_REST_WITH_CUSTOMER_KEY,
        "diskEncryptionSetId": id,
    })
}

#[cfg(test)]
#[path = "encryption_tests.rs"]
mod encryption_tests;
