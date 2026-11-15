// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Cluster API bootstrap data for banlieue InfraMachines (ADR-0129).
//!
//! The CAPI v1beta2 InfraMachine contract leaves delivering a `Machine`'s
//! bootstrap data (`spec.bootstrap.dataSecretName`) to the infrastructure
//! provider. `banlieue-controller` copies it into the InfraMachine's
//! `spec.userData`; host providers hold the guest until it is there. This
//! module is the part both sides share, as pure functions:
//!
//! - which machines Cluster API means ([`is_capi_intended`]) and which a
//!   `Machine` already owns ([`owning_machine`]);
//! - whether a provider must hold creation ([`holds_creation`]), and the
//!   condition it must never write itself ([`retain_provider_conditions`]);
//! - what the controller reads off a `Machine` ([`MachineBootstrap`]),
//!   whether that Machine really is the InfraMachine's
//!   ([`machine_confirms_adoption`]), and which Secrets it will copy
//!   ([`bootstrap_value`]);
//! - one accessor trait over the four InfraMachine kinds
//!   ([`BootstrapTarget`]).
//!
//! What it encodes is Cluster API's own behaviour, checked against its
//! `release-1.14` source: `external.GenerateTemplate` creates an InfraMachine
//! owned by its MachineSet or control plane and labelled
//! [`CLUSTER_NAME_LABEL`], and the Machine controller adopts it later; CABPK
//! and k0smotron write the bootstrap Secret with type
//! [`BOOTSTRAP_SECRET_TYPE`], that label, a controller ownerReference to the
//! bootstrap config, and keys [`BOOTSTRAP_VALUE_KEY`] and
//! [`BOOTSTRAP_FORMAT_KEY`].

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, ObjectMeta, OwnerReference};
use serde_json::Value;

use crate::infrastructure::{
    CloudHypervisorMachine, LibvirtMachine, ProxmoxMachine, VSphereMachine,
};

/// Cluster API's core API group.
pub const CAPI_GROUP: &str = "cluster.x-k8s.io";
/// The suffix of every Cluster API provider group
/// (`controlplane.cluster.x-k8s.io`, `bootstrap.cluster.x-k8s.io`, ...).
pub const CAPI_GROUP_SUFFIX: &str = ".cluster.x-k8s.io";
/// The version of `Machine` banlieue reads: v1beta2, CAPI's storage version.
pub const MACHINE_VERSION: &str = "v1beta2";
/// `Machine`'s kind.
pub const MACHINE_KIND: &str = "Machine";
/// `Machine`'s plural.
pub const MACHINE_PLURAL: &str = "machines";
/// Label Cluster API sets on every object of a cluster, InfraMachines from
/// the moment it creates them included.
pub const CLUSTER_NAME_LABEL: &str = "cluster.x-k8s.io/cluster-name";
/// The type of a bootstrap data Secret.
pub const BOOTSTRAP_SECRET_TYPE: &str = "cluster.x-k8s.io/secret";
/// The bootstrap data itself.
pub const BOOTSTRAP_VALUE_KEY: &str = "value";
/// How the bootstrap data is encoded; absent means [`FORMAT_CLOUD_CONFIG`].
pub const BOOTSTRAP_FORMAT_KEY: &str = "format";
/// The one format every banlieue backend can deliver.
pub const FORMAT_CLOUD_CONFIG: &str = "cloud-config";
/// Condition type the controller alone writes on a CAPI InfraMachine.
pub const BOOTSTRAP_DATA_READY: &str = "BootstrapDataReady";

/// Reasons of [`BOOTSTRAP_DATA_READY`] (ADR-0129 Decision 6).
pub mod bootstrap_reasons {
    /// Not yet adopted by a Machine, no `dataSecretName` yet, or the Secret
    /// does not exist yet.
    pub const WAITING_FOR_BOOTSTRAP_DATA: &str = "WaitingForBootstrapData";
    /// The Secret is not CAPI bootstrap data for this Machine.
    pub const BOOTSTRAP_SECRET_INVALID: &str = "BootstrapSecretInvalid";
    /// The Secret's `format` is one no banlieue backend delivers.
    pub const UNSUPPORTED_BOOTSTRAP_DATA_FORMAT: &str = "UnsupportedBootstrapDataFormat";
    /// The controller may not read Secrets in this namespace.
    pub const BOOTSTRAP_SECRET_FORBIDDEN: &str = "BootstrapSecretForbidden";
    /// `spec.userData` holds the Secret's value.
    pub const BOOTSTRAP_DATA_DELIVERED: &str = "BootstrapDataDelivered";
    /// The Machine this machine's owner reference names is not that object,
    /// or does not name this machine as its `spec.infrastructureRef`.
    pub const MACHINE_REFERENCE_MISMATCH: &str = "MachineReferenceMismatch";
}

const CONDITION_TRUE: &str = "True";

/// The API group of an `apiVersion`: `""` for the core group (`v1`).
#[must_use]
pub fn api_group(api_version: &str) -> &str {
    api_version.rsplit_once('/').map_or("", |(group, _)| group)
}

/// Whether `group` is Cluster API's or one of its providers'.
#[must_use]
pub fn is_capi_group(group: &str) -> bool {
    group == CAPI_GROUP || group.ends_with(CAPI_GROUP_SUFFIX)
}

/// Whether Cluster API means this machine for one of its Machines: it carries
/// [`CLUSTER_NAME_LABEL`], or a Cluster API object owns it. True from the
/// moment CAPI creates the machine, before any Machine adopts it.
#[must_use]
pub fn is_capi_intended(meta: &ObjectMeta) -> bool {
    let labelled = meta
        .labels
        .as_ref()
        .is_some_and(|l| l.contains_key(CLUSTER_NAME_LABEL));
    if labelled {
        return true;
    }
    meta.owner_references
        .iter()
        .flatten()
        .any(|o| is_capi_group(api_group(&o.api_version)))
}

/// The CAPI `Machine` that owns this machine, once it has been adopted.
#[must_use]
pub fn owning_machine(meta: &ObjectMeta) -> Option<&OwnerReference> {
    meta.owner_references
        .iter()
        .flatten()
        .find(|o| api_group(&o.api_version) == CAPI_GROUP && o.kind == MACHINE_KIND)
}

/// Whether [`BOOTSTRAP_DATA_READY`] is `True`.
#[must_use]
pub fn is_bootstrap_data_ready(conditions: &[Condition]) -> bool {
    conditions
        .iter()
        .any(|c| c.type_ == BOOTSTRAP_DATA_READY && c.status == CONDITION_TRUE)
}

/// Whether a host provider must not create this machine's guest yet: Cluster
/// API means it, and it is not yet both owned by a Machine and delivered.
/// Never true for a standalone or `VirtualMachine`-owned machine.
#[must_use]
pub fn holds_creation(meta: &ObjectMeta, conditions: &[Condition]) -> bool {
    if !is_capi_intended(meta) {
        return false;
    }
    owning_machine(meta).is_none() || !is_bootstrap_data_ready(conditions)
}

/// Drop the controller's [`BOOTSTRAP_DATA_READY`] from conditions a provider
/// is about to apply: providers apply their whole status from a snapshot, and
/// must neither own that entry nor overwrite it from a stale copy.
pub fn retain_provider_conditions(conditions: &mut Vec<Condition>) {
    conditions.retain(|c| c.type_ != BOOTSTRAP_DATA_READY);
}

/// A reference in a v1beta2 Machine (`configRef`, `infrastructureRef`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapiObjectRef {
    /// API group, without a version.
    pub api_group: String,
    /// Kind.
    pub kind: String,
    /// Name, in the Machine's namespace.
    pub name: String,
}

impl CapiObjectRef {
    fn from_value(v: &Value) -> Option<Self> {
        let field = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        Some(Self {
            api_group: field("apiGroup")?.to_string(),
            kind: field("kind")?.to_string(),
            name: field("name")?.to_string(),
        })
    }
}

/// What the controller reads off a v1beta2 `Machine`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineBootstrap {
    /// `spec.clusterName`.
    pub cluster_name: String,
    /// `spec.bootstrap.dataSecretName`, `None` while unset or empty.
    pub data_secret_name: Option<String>,
    /// `spec.bootstrap.configRef`.
    pub config_ref: Option<CapiObjectRef>,
    /// `spec.infrastructureRef`.
    pub infrastructure_ref: Option<CapiObjectRef>,
}

impl MachineBootstrap {
    /// Read a Machine's body (`apiVersion`, `kind`, `spec`, ...). `None` when
    /// `spec.clusterName`, which v1beta2 requires, is missing.
    #[must_use]
    pub fn from_machine(body: &Value) -> Option<Self> {
        let spec = body.get("spec")?;
        let cluster_name = spec
            .get("clusterName")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?
            .to_string();
        let bootstrap = spec.get("bootstrap");
        let data_secret_name = bootstrap
            .and_then(|b| b.get("dataSecretName"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Some(Self {
            cluster_name,
            data_secret_name,
            config_ref: bootstrap
                .and_then(|b| b.get("configRef"))
                .and_then(CapiObjectRef::from_value),
            infrastructure_ref: spec
                .get("infrastructureRef")
                .and_then(CapiObjectRef::from_value),
        })
    }
}

/// Whether the Machine an InfraMachine's owner reference names really is its
/// Machine: the same object (`machine_uid` equals the reference's
/// `owner_uid`), and its `spec.infrastructureRef` names `infra` back.
///
/// An owner reference is ordinary metadata that anyone allowed to create or
/// update the InfraMachine can write, so on its own it proves nothing. The
/// Machine's `spec.infrastructureRef` is written by Cluster API when it
/// creates the Machine, before it adopts the InfraMachine.
#[must_use]
pub fn machine_confirms_adoption(
    machine_uid: Option<&str>,
    machine: &MachineBootstrap,
    owner_uid: &str,
    infra: &CapiObjectRef,
) -> bool {
    machine_uid == Some(owner_uid) && machine.infrastructure_ref.as_ref() == Some(infra)
}

/// Why a Secret was not copied: a [`bootstrap_reasons`] value and a message
/// that never carries the Secret's contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretRejection {
    /// The condition reason.
    pub reason: &'static str,
    /// What was wrong.
    pub message: String,
}

fn invalid(message: String) -> SecretRejection {
    SecretRejection {
        reason: bootstrap_reasons::BOOTSTRAP_SECRET_INVALID,
        message,
    }
}

/// The bootstrap data to copy from `secret` for `machine`, after ADR-0129
/// Decision 4's checks: CAPI type, cluster label, a controller ownerReference
/// matching the Machine's `configRef` when it has one, a non-empty UTF-8
/// `value`, and `format` absent or [`FORMAT_CLOUD_CONFIG`].
///
/// These classify the Secret; they do not authorize whoever named it, which
/// the admission policy does (Decision 5).
///
/// # Errors
/// A [`SecretRejection`] naming the first check that failed.
pub fn bootstrap_value(
    secret: &Secret,
    machine: &MachineBootstrap,
) -> Result<String, SecretRejection> {
    let name = secret.metadata.name.as_deref().unwrap_or_default();
    if secret.type_.as_deref() != Some(BOOTSTRAP_SECRET_TYPE) {
        return Err(invalid(format!(
            "Secret {name} is of type {:?}, not {BOOTSTRAP_SECRET_TYPE}",
            secret.type_.as_deref().unwrap_or_default()
        )));
    }
    let cluster = secret
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get(CLUSTER_NAME_LABEL));
    if cluster.map(String::as_str) != Some(machine.cluster_name.as_str()) {
        return Err(invalid(format!(
            "Secret {name} is not labelled {CLUSTER_NAME_LABEL}={}",
            machine.cluster_name
        )));
    }
    if let Some(config) = &machine.config_ref {
        let written_by_config = secret.metadata.owner_references.iter().flatten().any(|o| {
            o.controller == Some(true)
                && api_group(&o.api_version) == config.api_group
                && o.kind == config.kind
                && o.name == config.name
        });
        if !written_by_config {
            return Err(invalid(format!(
                "Secret {name} is not controlled by the Machine's bootstrap config {} {}",
                config.kind, config.name
            )));
        }
    }
    let data = secret.data.as_ref();
    let format = data
        .and_then(|d| d.get(BOOTSTRAP_FORMAT_KEY))
        .map(|f| String::from_utf8_lossy(&f.0).into_owned());
    if let Some(format) = format.filter(|f| f != FORMAT_CLOUD_CONFIG) {
        return Err(SecretRejection {
            reason: bootstrap_reasons::UNSUPPORTED_BOOTSTRAP_DATA_FORMAT,
            message: format!(
                "Secret {name} has format {format:?}; banlieue delivers only {FORMAT_CLOUD_CONFIG}"
            ),
        });
    }
    let value = data
        .and_then(|d| d.get(BOOTSTRAP_VALUE_KEY))
        .filter(|v| !v.0.is_empty())
        .ok_or_else(|| invalid(format!("Secret {name} has no {BOOTSTRAP_VALUE_KEY}")))?;
    String::from_utf8(value.0.clone()).map_err(|_| {
        invalid(format!(
            "Secret {name}'s {BOOTSTRAP_VALUE_KEY} is not valid UTF-8"
        ))
    })
}

/// The fields ADR-0129 needs, read the same way on all four InfraMachine
/// kinds.
pub trait BootstrapTarget {
    /// `spec.userData`.
    fn user_data(&self) -> Option<&str>;
    /// `spec.providerID`.
    fn provider_id(&self) -> Option<&str>;
    /// `status.conditions`, empty when there is no status.
    fn conditions(&self) -> &[Condition];
}

macro_rules! bootstrap_target {
    ($($kind:ty),+) => {$(
        impl BootstrapTarget for $kind {
            fn user_data(&self) -> Option<&str> {
                self.spec.user_data.as_deref()
            }
            fn provider_id(&self) -> Option<&str> {
                self.spec.provider_id.as_deref()
            }
            fn conditions(&self) -> &[Condition] {
                self.status.as_ref().map_or(&[], |s| s.conditions.as_slice())
            }
        }
    )+};
}

bootstrap_target!(
    CloudHypervisorMachine,
    LibvirtMachine,
    VSphereMachine,
    ProxmoxMachine
);

#[cfg(test)]
#[path = "capi_tests.rs"]
mod capi_tests;
