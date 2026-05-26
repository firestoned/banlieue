// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Build provider-specific infrastructure CRs from a scheduler [`Decision`].
//!
//! Phase 1A iteration 2: only `vsphere` is implemented. Iteration 1B/1C/1D
//! add Proxmox and libvirt builders behind a shared trait.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{ImagePerProviderStatus, Provider, VMClass, VMImage, VirtualMachine};
use banlieue_api::common::{IpamSpec, MachineAddress};
use banlieue_api::infrastructure::{
    VSphereDiskSpec, VSphereMachine, VSphereMachineSpec, VSphereNicSpec,
};
use kube::ResourceExt;
use kube::api::ObjectMeta;
use kube::core::Resource;

use super::scheduler::Decision;

/// Failure-domain raw-attribute key for vSphere datacenter name.
pub const FD_RAW_VSPHERE_DATACENTER: &str = "datacenter";
/// Failure-domain raw-attribute key for vSphere cluster name.
pub const FD_RAW_VSPHERE_CLUSTER: &str = "cluster";

/// Why the infra builder couldn't produce a VSphereMachine.
#[derive(Debug, thiserror::Error)]
pub enum InfraBuildError {
    /// The chosen failure domain didn't carry `datacenter` / `cluster` in
    /// `attributes.raw`. The provider's controller must populate these.
    #[error("failure domain {0} missing raw attribute '{1}'")]
    MissingFdRaw(String, &'static str),

    /// `VMImage.status.perProvider[i].resolved_ref` was None for the chosen
    /// provider.
    #[error("VMImage {image} has no resolved_ref for provider {provider}")]
    MissingResolvedImageRef { image: String, provider: String },

    /// Decision lacks a backend_id for a class — should never happen because
    /// the scheduler resolves all classes before returning.
    #[error("decision did not resolve class '{0}' to a backend identifier")]
    UnresolvedClass(String),
}

/// Build a [`VSphereMachine`] from the scheduler [`Decision`], the original
/// VM, its class, image, and the chosen [`Provider`]. Owner-reference is set
/// to `vm` so the VSphereMachine is garbage-collected when the parent VM is
/// deleted.
///
/// The `provider` parameter is currently unused by the vSphere builder — the
/// `Decision` already carries the resolved storage / network backend IDs the
/// scheduler computed from `Provider.spec.capabilities`. We accept it on the
/// signature so the contract is right for Phase 1C (Proxmox needs
/// `Provider.spec.connection.endpoint` to target a specific cluster) and
/// Phase 1D (libvirt needs SSH transport settings).
pub fn build_vsphere_machine(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    decision: &Decision,
    _provider: &Provider,
) -> Result<VSphereMachine, InfraBuildError> {
    let datacenter = decision
        .failure_domain_raw
        .get(FD_RAW_VSPHERE_DATACENTER)
        .cloned()
        .ok_or_else(|| {
            InfraBuildError::MissingFdRaw(
                decision.failure_domain_name.clone(),
                FD_RAW_VSPHERE_DATACENTER,
            )
        })?;
    let cluster = decision
        .failure_domain_raw
        .get(FD_RAW_VSPHERE_CLUSTER)
        .cloned()
        .ok_or_else(|| {
            InfraBuildError::MissingFdRaw(
                decision.failure_domain_name.clone(),
                FD_RAW_VSPHERE_CLUSTER,
            )
        })?;

    // OS disk is the first disk; its backend_id is the resolved datastore.
    let os_storage = decision
        .resolved_storage
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no disks)".into()))?;
    let datastore = os_storage.backend_id.clone();

    // Template resolved from the VMImage's per-provider status.
    let template = resolve_template_ref(image, &decision.provider_name).ok_or_else(|| {
        InfraBuildError::MissingResolvedImageRef {
            image: image.name_any(),
            provider: decision.provider_name.clone(),
        }
    })?;

    // Disks: 1:1 with VMClass disks. Storage class resolution is via
    // resolved_storage (same order).
    let disks: Vec<VSphereDiskSpec> = class
        .spec
        .hardware
        .disks
        .iter()
        .map(|d| VSphereDiskSpec {
            name: d.name.clone(),
            size_gi_b: d.size_gi_b,
            provisioning: d.provisioning.clone(),
        })
        .collect();

    // NICs: 1:1 with VMClass NICs. port_group resolved from resolved_networks.
    let nics: Vec<VSphereNicSpec> = class
        .spec
        .network
        .interfaces
        .iter()
        .zip(decision.resolved_networks.iter())
        .map(|(nic, resolved)| VSphereNicSpec {
            name: nic.name.clone(),
            port_group: resolved.backend_id.clone(),
            mac_address: None,
            ipam: nic.ipam.clone(),
        })
        .collect();

    let spec = VSphereMachineSpec {
        provider_id: None,
        failure_domain: Some(decision.failure_domain_name.clone()),
        provider_ref: banlieue_api::common::LocalObjectReference {
            name: decision.provider_name.clone(),
        },
        template,
        datacenter,
        cluster,
        datastore,
        folder: None,
        resource_pool: None,
        num_cpus: class.spec.hardware.cpus,
        memory_mi_b: class.spec.hardware.memory_mi_b,
        firmware: class.spec.firmware.clone(),
        disks,
        network: nics,
    };

    Ok(VSphereMachine {
        metadata: ObjectMeta {
            // Same name + namespace as the parent VM. This is the convention
            // for 1:1 owned infra CRs — keeps the relationship discoverable
            // without indexing.
            name: Some(vm.name_any()),
            namespace: vm.namespace(),
            owner_references: Some(vec![owner_reference_for(vm)]),
            labels: Some(propagate_labels(vm)),
            ..Default::default()
        },
        spec,
        status: None,
    })
}

/// Construct a controller-owning [`OwnerReference`] back to the parent VM.
fn owner_reference_for(
    vm: &VirtualMachine,
) -> k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
    OwnerReference {
        api_version: format!(
            "{}/{}",
            VirtualMachine::group(&()),
            VirtualMachine::version(&())
        ),
        kind: "VirtualMachine".to_string(),
        name: vm.name_any(),
        uid: vm.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

/// Copy a small set of labels from the parent VM onto the infra CR for
/// discoverability via `kubectl get vspheremachines -l app=db-prod`.
fn propagate_labels(vm: &VirtualMachine) -> BTreeMap<String, String> {
    let mut labels: BTreeMap<String, String> = vm.labels().clone();
    labels.insert("banlieue.io/owned-by".into(), vm.name_any());
    labels
}

/// Look up `VMImage.status.per_provider[i].resolved_ref` for the chosen provider.
fn resolve_template_ref(image: &VMImage, provider_name: &str) -> Option<String> {
    image.status.as_ref().and_then(|s| {
        s.per_provider
            .iter()
            .find(|p: &&ImagePerProviderStatus| p.provider_name == provider_name)
            .and_then(|p| p.resolved_ref.clone())
    })
}

// Convenience accessors used by status_mirror tests. Suppresses unused-warning
// noise on the trait import once everything is wired.
#[allow(dead_code)]
pub(crate) fn unused_addresses_marker(addrs: &[MachineAddress]) -> usize {
    addrs.len()
}
#[allow(dead_code)]
pub(crate) fn unused_ipam_marker(_i: &IpamSpec) {}

#[cfg(test)]
#[path = "infra_tests.rs"]
mod infra_tests;
