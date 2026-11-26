// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The VM and NIC request bodies (ADR-0164 Decision 3).
//!
//! The VM body is the one that booted a banlieue Kairos image as a Trusted
//! Launch VM in the roadmap 23 phase 0 spike. Each field earned its place:
//!
//! - `storageProfile.diskControllerType: NVMe`: the usable 2-vCPU sizes were
//!   NVMe-only (ADR-0160).
//! - `osDisk.diskSizeGB`: the disk is otherwise the image's size and a
//!   Kairos first boot has no room to lay out its partitions (ADR-0160).
//! - `deleteOption: Delete` on the OS disk and NIC: deleting the VM removes
//!   both (ADR-0158 Decision 6).
//! - `osDisk.name` from the owner's UID ([`os_disk_name`]): ARM accepts no
//!   tags on the OS disk in the VM body and copies none of the VM's onto it,
//!   so the provider tags the disk by name once the VM exists (ADR-0173).
//! - `diagnosticsProfile.bootDiagnostics.enabled`: always, since a VM that
//!   times out cannot be changed afterwards (ADR-0163 Decision 4).
//! - `linuxConfiguration.provisionVMAgent: false`: the image reports ready
//!   itself (ADR-0163).
//! - an SSH public key: Azure requires one when password login is off. The
//!   provider passes a key whose private half it discards.
//!
//! In a policy-governed subscription (ADR-0172) the OS disk may name a disk
//! encryption set (`managedDisk.diskEncryptionSet.id`) and the
//! `securityProfile` may set `encryptionAtHost`.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::encryption::parse_disk_encryption_set_id;
use crate::error::{Error, Result};
use crate::tags::validate_tags;

/// Azure's limit on decoded `customData`.
pub const MAX_CUSTOM_DATA_BYTES: usize = 65_535;
/// The admin user name in `osProfile`; the image never lets it log in.
pub const DEFAULT_ADMIN_USERNAME: &str = "banlieue";
/// Azure's limit on a VM name.
pub const MAX_VM_NAME_LEN: usize = 64;
/// Azure's limit on a NIC name.
pub const MAX_NIC_NAME_LEN: usize = 80;
/// Azure's limit on a managed disk name.
pub const MAX_DISK_NAME_LEN: usize = 80;

const NAME_PREFIX: &str = "banlieue-";
const NIC_SUFFIX: &str = "-nic";
const OS_DISK_SUFFIX: &str = "-osdisk";
const IP_CONFIG_NAME: &str = "ipconfig1";
const DELETE_OPTION: &str = "Delete";

/// The VM name for an object with `uid`: stable, so a retried create is the
/// same VM.
#[must_use]
pub fn vm_name(uid: &str) -> String {
    format!("{NAME_PREFIX}{uid}")
}

/// The NIC name for an object with `uid`.
#[must_use]
pub fn nic_name(uid: &str) -> String {
    format!("{NAME_PREFIX}{uid}{NIC_SUFFIX}")
}

fn invalid(message: String) -> Error {
    Error::InvalidRequest {
        code: "InvalidVmRequest".to_string(),
        message,
    }
}

fn require(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(invalid(format!("{field} is required")));
    }
    Ok(())
}

/// The OS disk name for an object with `uid`. Starts with [`vm_name`], so a
/// disk left behind still reads as the machine's.
#[must_use]
pub fn os_disk_name(uid: &str) -> String {
    format!("{NAME_PREFIX}{uid}{OS_DISK_SUFFIX}")
}

/// What a VM create needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmRequest {
    /// Region, such as `eastus2`.
    pub location: String,
    /// Availability zone, such as `1`.
    pub zone: String,
    /// VM size, such as `Standard_D2s_v7`.
    pub size: String,
    /// Resource ID of the gallery image version.
    pub image_version_id: String,
    /// OS disk size; at least the published image's floor (ADR-0162).
    pub os_disk_size_gb: u32,
    /// Disk SKU, such as `StandardSSD_LRS`.
    pub os_disk_sku: String,
    /// The OS disk's resource name ([`os_disk_name`]), so it can be found
    /// and tagged without a listing.
    pub os_disk_name: String,
    /// Resource ID of the NIC, created first.
    pub nic_id: String,
    /// Guest host name.
    pub computer_name: String,
    /// Admin user name Azure requires.
    pub admin_username: String,
    /// An SSH public key Azure requires; nothing keeps its private half.
    pub ssh_public_key: String,
    /// User data, delivered as `customData`; empty for none.
    pub custom_data: Vec<u8>,
    /// Trusted Launch with Secure Boot and a vTPM.
    pub trusted_launch: bool,
    /// Tags, ownership tags included.
    pub tags: BTreeMap<String, String>,
    /// Resource ID of the disk encryption set (customer-managed key) for
    /// the OS disk; `None` for platform-managed keys (ADR-0172 Decision 2).
    pub disk_encryption_set_id: Option<String>,
    /// `securityProfile.encryptionAtHost` (ADR-0172 Decision 3).
    pub encryption_at_host: bool,
}

impl VmRequest {
    /// The body of `PUT .../virtualMachines/{name}`.
    ///
    /// # Errors
    /// [`Error::InvalidRequest`] for a missing field, a zero disk size,
    /// oversized `customData` or a malformed disk encryption set ID;
    /// [`Error::InvalidTag`] for a tag Azure refuses.
    pub fn to_body(&self) -> Result<Value> {
        for (field, value) in [
            ("location", &self.location),
            ("zone", &self.zone),
            ("size", &self.size),
            ("image_version_id", &self.image_version_id),
            ("os_disk_sku", &self.os_disk_sku),
            ("os_disk_name", &self.os_disk_name),
            ("nic_id", &self.nic_id),
            ("computer_name", &self.computer_name),
            ("admin_username", &self.admin_username),
            ("ssh_public_key", &self.ssh_public_key),
        ] {
            require(field, value)?;
        }
        if self.os_disk_size_gb == 0 {
            return Err(invalid(
                "os_disk_size_gb must be greater than zero".to_string(),
            ));
        }
        if self.os_disk_name.len() > MAX_DISK_NAME_LEN {
            return Err(invalid(format!(
                "os_disk_name is {} characters; Azure allows {MAX_DISK_NAME_LEN}",
                self.os_disk_name.len()
            )));
        }
        if self.custom_data.len() > MAX_CUSTOM_DATA_BYTES {
            return Err(invalid(format!(
                "customData is {} bytes; Azure allows {MAX_CUSTOM_DATA_BYTES}",
                self.custom_data.len()
            )));
        }
        validate_tags(&self.tags)?;
        if let Some(id) = &self.disk_encryption_set_id {
            parse_disk_encryption_set_id(id)?;
        }

        let mut os_profile = json!({
            "computerName": self.computer_name,
            "adminUsername": self.admin_username,
            "linuxConfiguration": {
                "disablePasswordAuthentication": true,
                "provisionVMAgent": false,
                "ssh": {"publicKeys": [{
                    "path": format!("/home/{}/.ssh/authorized_keys", self.admin_username),
                    "keyData": self.ssh_public_key
                }]}
            }
        });
        if !self.custom_data.is_empty() {
            os_profile["customData"] =
                Value::String(base64::engine::general_purpose::STANDARD.encode(&self.custom_data));
        }

        let mut properties = json!({
            "hardwareProfile": {"vmSize": self.size},
            "storageProfile": {
                "imageReference": {"id": self.image_version_id},
                "diskControllerType": "NVMe",
                "osDisk": {
                    "name": self.os_disk_name,
                    "createOption": "FromImage",
                    "deleteOption": DELETE_OPTION,
                    "diskSizeGB": self.os_disk_size_gb,
                    "managedDisk": {"storageAccountType": self.os_disk_sku}
                }
            },
            "osProfile": os_profile,
            "networkProfile": {"networkInterfaces": [
                {"id": self.nic_id, "properties": {"primary": true, "deleteOption": DELETE_OPTION}}
            ]},
            "diagnosticsProfile": {"bootDiagnostics": {"enabled": true}}
        });
        if let Some(id) = &self.disk_encryption_set_id {
            properties["storageProfile"]["osDisk"]["managedDisk"]["diskEncryptionSet"] =
                json!({"id": id});
        }
        if self.trusted_launch {
            properties["securityProfile"] = json!({
                "securityType": "TrustedLaunch",
                "uefiSettings": {"secureBootEnabled": true, "vTpmEnabled": true}
            });
        }
        if self.encryption_at_host {
            properties["securityProfile"]["encryptionAtHost"] = Value::Bool(true);
        }

        Ok(json!({
            "location": self.location,
            "zones": [self.zone],
            "tags": self.tags,
            "properties": properties
        }))
    }
}

/// What a NIC create needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NicRequest {
    /// Region.
    pub location: String,
    /// Resource ID of the subnet (often in the network team's resource group).
    pub subnet_id: String,
    /// Accelerated networking (the image must leave the VF unmanaged, ADR-0163).
    pub accelerated_networking: bool,
    /// Tags, ownership tags included.
    pub tags: BTreeMap<String, String>,
}

impl NicRequest {
    /// The body of `PUT .../networkInterfaces/{name}`: one dynamic private IP
    /// on the subnet; no public IP.
    ///
    /// # Errors
    /// [`Error::InvalidRequest`] for a missing field; [`Error::InvalidTag`].
    pub fn to_body(&self) -> Result<Value> {
        require("location", &self.location)?;
        require("subnet_id", &self.subnet_id)?;
        validate_tags(&self.tags)?;
        Ok(json!({
            "location": self.location,
            "tags": self.tags,
            "properties": {
                "enableAcceleratedNetworking": self.accelerated_networking,
                "ipConfigurations": [{
                    "name": IP_CONFIG_NAME,
                    "properties": {
                        "primary": true,
                        "privateIPAllocationMethod": "Dynamic",
                        "subnet": {"id": self.subnet_id}
                    }
                }]
            }
        }))
    }
}

#[cfg(test)]
#[path = "vm_tests.rs"]
mod vm_tests;
