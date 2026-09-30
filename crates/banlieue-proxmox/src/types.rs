// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Request and response types for the endpoints banlieue uses (ADR-0074).
//!
//! Proxmox is loose about JSON types (`"100"` and `100`, `1` and `true`), so
//! these decode leniently and expose plain Rust types.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::wire::Params;

fn flex_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        Value::String(s) => matches!(s.as_str(), "1" | "true"),
        _ => false,
    })
}

fn flex_bool_true<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    flex_bool(d)
}

const fn default_true() -> bool {
    true
}

/// `GET /version`.
#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    /// e.g. `9.0.3`.
    pub version: String,
    /// e.g. `9.0`.
    #[serde(default)]
    pub release: Option<String>,
    /// Build id.
    #[serde(default)]
    pub repoid: Option<String>,
}

impl Version {
    /// The major version, if `version` starts with a number.
    #[must_use]
    pub fn major(&self) -> Option<u32> {
        self.version.split('.').next()?.parse().ok()
    }
}

/// `GET /nodes` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    /// Node name.
    pub node: String,
    /// `online`, `offline` or `unknown`.
    pub status: String,
    /// Logical CPUs.
    #[serde(default)]
    pub maxcpu: Option<u32>,
    /// Memory in bytes.
    #[serde(default)]
    pub maxmem: Option<u64>,
}

impl Node {
    /// Whether the node is reachable.
    #[must_use]
    pub fn is_online(&self) -> bool {
        self.status == "online"
    }
}

/// A VMID, from `GET /cluster/nextid` (a JSON string) or anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmId(pub u32);

impl<'de> Deserialize<'de> for VmId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let id = match Value::deserialize(d)? {
            Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        id.map(VmId)
            .ok_or_else(|| serde::de::Error::custom("not a VMID"))
    }
}

/// `GET /cluster/resources?type=vm` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ClusterVm {
    /// VMID.
    pub vmid: u32,
    /// Node the guest is on.
    pub node: String,
    /// Guest name.
    #[serde(default)]
    pub name: Option<String>,
    /// `running`, `stopped`, …
    #[serde(default)]
    pub status: String,
    /// `qemu` or `lxc`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Whether it is a template.
    #[serde(default, deserialize_with = "flex_bool")]
    pub template: bool,
}

/// `GET /nodes/{n}/storage` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Storage {
    /// Storage id.
    pub storage: String,
    /// Backend type (`dir`, `lvmthin`, `nfs`, …).
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Comma-separated content types.
    #[serde(default)]
    pub content: String,
    /// Not administratively disabled.
    #[serde(default = "default_true", deserialize_with = "flex_bool_true")]
    pub enabled: bool,
    /// Currently mounted and reachable on this node.
    #[serde(default = "default_true", deserialize_with = "flex_bool_true")]
    pub active: bool,
    /// Reachable from every node.
    #[serde(default, deserialize_with = "flex_bool")]
    pub shared: bool,
    /// Bytes total.
    #[serde(default)]
    pub total: Option<u64>,
    /// Bytes available.
    #[serde(default)]
    pub avail: Option<u64>,
}

impl Storage {
    /// Whether `content` lists exactly `kind` (`images`, `iso`, `snippets`).
    #[must_use]
    pub fn has_content(&self, kind: &str) -> bool {
        self.content.split(',').any(|c| c.trim() == kind)
    }

    /// Enabled, active and allowed to hold `kind`. A storage can exist on a
    /// node and still refuse a disk (roadmap 06, Gotchas).
    #[must_use]
    pub fn usable_for(&self, kind: &str) -> bool {
        self.enabled && self.active && self.has_content(kind)
    }
}

/// `GET /nodes/{n}/network` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkIface {
    /// Interface name, e.g. `vmbr0`.
    pub iface: String,
    /// `bridge`, `eth`, `bond`, `vlan`, …
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Up.
    #[serde(default, deserialize_with = "flex_bool")]
    pub active: bool,
    /// A VLAN-aware bridge.
    #[serde(rename = "bridge_vlan_aware", default, deserialize_with = "flex_bool")]
    pub vlan_aware: bool,
}

impl NetworkIface {
    /// Whether a VM NIC can attach to it.
    #[must_use]
    pub fn is_bridge(&self) -> bool {
        self.kind == "bridge"
    }
}

/// `GET /nodes/{n}/storage/{s}/content` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Volume {
    /// `storage:content/name`.
    pub volid: String,
    /// Content type.
    #[serde(default)]
    pub content: String,
    /// Bytes.
    #[serde(default)]
    pub size: Option<u64>,
    /// Format.
    #[serde(default)]
    pub format: Option<String>,
}

/// `GET /nodes/{n}/tasks/{upid}/status`.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskStatus {
    /// `running` or `stopped`.
    pub status: String,
    /// Set once stopped: `OK` or the failure text.
    #[serde(default)]
    pub exitstatus: Option<String>,
}

impl TaskStatus {
    /// Whether the task has not finished.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.status != "stopped"
    }

    /// Stopped with `exitstatus == "OK"`. A stopped task with no exit status
    /// did not succeed.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.is_running() && self.exitstatus.as_deref() == Some("OK")
    }
}

/// `GET /nodes/{n}/qemu/{id}/status/current`.
#[derive(Debug, Clone, Deserialize)]
pub struct VmStatus {
    /// `running` or `stopped`.
    pub status: String,
    /// Guest name.
    #[serde(default)]
    pub name: Option<String>,
    /// QMP state (`running`, `paused`, `prelaunch`, …).
    #[serde(default)]
    pub qmpstatus: Option<String>,
    /// Whether the QEMU guest agent is enabled in the config.
    #[serde(default, deserialize_with = "flex_bool")]
    pub agent: bool,
}

impl VmStatus {
    /// Whether the guest is powered on.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// `agent/network-get-interfaces` payload (`data.result`).
#[derive(Debug, Clone, Deserialize)]
pub struct GuestInterfaces {
    /// One entry per guest interface.
    pub result: Vec<GuestInterface>,
}

/// One guest interface as the QEMU guest agent reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct GuestInterface {
    /// Interface name inside the guest.
    pub name: String,
    /// MAC address.
    #[serde(rename = "hardware-address", default)]
    pub hardware_address: Option<String>,
    /// Addresses.
    #[serde(rename = "ip-addresses", default)]
    pub ip_addresses: Vec<GuestIp>,
}

/// One address on a guest interface.
#[derive(Debug, Clone, Deserialize)]
pub struct GuestIp {
    /// The address.
    #[serde(rename = "ip-address")]
    pub ip_address: String,
    /// `ipv4` or `ipv6`.
    #[serde(rename = "ip-address-type", default)]
    pub ip_address_type: String,
    /// Prefix length.
    #[serde(default)]
    pub prefix: Option<u8>,
}

impl GuestIp {
    /// Whether this is an IPv4 address.
    #[must_use]
    pub fn is_ipv4(&self) -> bool {
        self.ip_address_type == "ipv4"
    }
}

/// A VM's config as `key -> value`, every value stringified (Proxmox mixes
/// numbers and strings).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmConfig(pub BTreeMap<String, String>);

impl VmConfig {
    /// The value for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

impl<'de> Deserialize<'de> for VmConfig {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, Value>::deserialize(d)?;
        Ok(Self(
            raw.into_iter()
                .map(|(k, v)| {
                    let s = match v {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (k, s)
                })
                .collect(),
        ))
    }
}

/// Parameters for `POST /nodes/{n}/qemu/{template}/clone`.
#[derive(Debug, Clone)]
pub struct CloneParams {
    newid: u32,
    name: Option<String>,
    target: Option<String>,
    storage: Option<String>,
    pool: Option<String>,
    description: Option<String>,
    full: bool,
}

impl CloneParams {
    /// Clone to `newid`. Full clones by default: a linked clone would tie
    /// the guest's disk to its template.
    #[must_use]
    pub fn new(newid: u32) -> Self {
        Self {
            newid,
            name: None,
            target: None,
            storage: None,
            pool: None,
            description: None,
            full: true,
        }
    }

    /// Guest name.
    #[must_use]
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Target node, when it differs from the template's.
    #[must_use]
    pub fn target(mut self, node: &str) -> Self {
        self.target = Some(node.to_string());
        self
    }

    /// Storage for the cloned disks.
    #[must_use]
    pub fn storage(mut self, storage: &str) -> Self {
        self.storage = Some(storage.to_string());
        self
    }

    /// Resource pool.
    #[must_use]
    pub fn pool(mut self, pool: &str) -> Self {
        self.pool = Some(pool.to_string());
        self
    }

    /// Description written to the new VM atomically with the clone. banlieue
    /// puts its ownership marker here, so a VM is never visible without it.
    #[must_use]
    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    /// Full (true) or linked (false) clone.
    #[must_use]
    pub fn full(mut self, full: bool) -> Self {
        self.full = full;
        self
    }

    /// The request parameters.
    #[must_use]
    pub fn params(&self) -> Params {
        Params::new()
            .set("newid", self.newid)
            .set_opt("name", self.name.as_deref())
            .flag("full", self.full)
            .set_opt("target", self.target.as_deref())
            .set_opt("storage", self.storage.as_deref())
            .set_opt("pool", self.pool.as_deref())
            .set_opt("description", self.description.as_deref())
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
