// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `Provider.status` for this host: one failure domain, the classes it can
//! really serve, and whether it can run guests at all.
//!
//! A class the `Provider` declares is published as available only when its
//! mapping's `target.hostClass` names a class in the host config **and** the
//! directory or bridge behind it exists now. The scheduler places VMs from
//! this list, so advertising a class that cannot work means a VM that is
//! scheduled and then fails on the host (the same rule the libvirt provider
//! applies to pools and networks).
//!
//! Features are passed through from the `Provider`, except that `vtpm` is
//! withheld unless this host can really serve one (ADR-0065 Decision 2): a
//! `[tpm]` section whose `swtpm`, `swtpm_setup`, setup configuration and
//! EK CA certificate all exist. Advertising it otherwise would let the
//! scheduler place a `tpmEnabled` VM here only for planning to refuse it.
//! With a vTPM, the host's EK CA certificate is published on the status
//! (Decision 6).

use crate::error::{Error, Result};
use crate::host_config::HostConfig;
use crate::reconciler::Context;
use crate::sys;
use banlieue_api::banlieue::{FailureDomain, FailureDomainAttributes, Provider, ProviderStatus};
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::status::{condition_status, set_condition};
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

/// The key in a class mapping's `target` naming the host class.
pub const TARGET_KEY_HOST_CLASS: &str = "hostClass";
/// The feature string the scheduler matches for vTPM.
const FEATURE_VTPM: &str = "vtpm";
/// The KVM device.
const DEV_KVM: &str = "/dev/kvm";
/// Where the host's CPUs and memory are read from.
const PROC_CPUINFO: &str = "/proc/cpuinfo";
const PROC_MEMINFO: &str = "/proc/meminfo";
const KIB_PER_MIB: u64 = 1024;
const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;

/// Condition types on `Provider.status`.
pub mod condition_types {
    /// Everything this host needs to run guests is present.
    pub const READY: &str = "Ready";
    /// The provider is running and can talk to the API server.
    pub const PROVIDER_REACHABLE: &str = "ProviderReachable";
}

/// Stable reasons. Operators match on these.
pub mod reasons {
    /// Running, and every declared class is served.
    pub const RECONCILED: &str = "Reconciled";
    /// Some declared classes are not served on this host.
    pub const CAPABILITIES_INCOMPLETE: &str = "CapabilitiesIncomplete";
    /// The host lacks KVM, the VMM or its firmware.
    pub const HOST_NOT_READY: &str = "HostNotReady";
    /// The Provider names a credentials Secret, which this provider must
    /// never read (ADR-0060 Decision 4).
    pub const CREDENTIALS_NOT_ALLOWED: &str = "CredentialsNotAllowed";
}

/// What the host has right now. Gathered by [`gather_facts`]; separate so the
/// status logic is testable without a host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostFacts {
    /// `/dev/kvm` exists.
    pub kvm: bool,
    /// The VMM binary exists.
    pub vmm_binary: bool,
    /// The firmware exists.
    pub firmware: bool,
    /// Host storage classes whose directory exists.
    pub storage_present: BTreeSet<String>,
    /// Host network classes whose bridge exists.
    pub bridges_present: BTreeSet<String>,
    /// A vTPM can be manufactured and run here.
    pub vtpm: bool,
    /// The host's EK CA certificate (PEM), when it parses as one.
    pub ek_ca_pem: Option<String>,
    /// What the host has to give guests.
    pub capacity: HostCapacity,
}

/// The host's capacity, as published on its failure domain. Each figure is
/// `None` (left out of status) when the host would not give it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCapacity {
    /// Logical CPUs.
    pub cpus: Option<u32>,
    /// The CPU model, as `/proc/cpuinfo` names it.
    pub cpu_model: Option<String>,
    /// Physical memory, MiB.
    pub memory_mib: Option<u64>,
    /// Memory reserved as hugepages, MiB. Reserved, not allocated on demand,
    /// so it is its own figure: a scheduler that counts it as free memory
    /// overcommits a host that looks half empty.
    pub hugepages_mib: Option<u64>,
    /// Free space for guests' disks, GiB, by host storage class.
    pub storage_free_gib: BTreeMap<String, u64>,
}

/// Logical CPU count and the first CPU's model from `/proc/cpuinfo` text.
#[must_use]
pub fn parse_cpuinfo(text: &str) -> (Option<u32>, Option<String>) {
    let field = |l: &str, key: &str| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    };
    let count = text
        .lines()
        .filter(|l| field(l, "processor").is_some())
        .count();
    let model = text.lines().find_map(|l| field(l, "model name"));
    (u32::try_from(count).ok().filter(|n| *n > 0), model)
}

/// Total memory and reserved hugepages, both MiB, from `/proc/meminfo` text.
#[must_use]
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim() != key {
                return None;
            }
            v.split_whitespace().next()?.parse::<u64>().ok()
        })
    };
    let memory = value("MemTotal").map(|kib| kib / KIB_PER_MIB);
    let hugepages = value("HugePages_Total")
        .zip(value("Hugepagesize"))
        .map(|(count, page_kib)| count * page_kib / KIB_PER_MIB);
    (memory, hugepages)
}

/// Free space on the filesystem holding `dir`, GiB, for an unprivileged
/// writer (`f_bavail`).
fn free_gib(dir: &Path) -> Option<u64> {
    let st = rustix::fs::statvfs(dir).ok()?;
    Some(st.f_bavail.saturating_mul(st.f_frsize) / BYTES_PER_GIB)
}

fn gather_capacity(config: &HostConfig) -> HostCapacity {
    let (cpus, cpu_model) = std::fs::read_to_string(PROC_CPUINFO)
        .map(|t| parse_cpuinfo(&t))
        .unwrap_or_default();
    let (memory_mib, hugepages_mib) = std::fs::read_to_string(PROC_MEMINFO)
        .map(|t| parse_meminfo(&t))
        .unwrap_or_default();
    HostCapacity {
        cpus,
        cpu_model,
        memory_mib,
        hugepages_mib,
        storage_free_gib: config
            .storage_classes
            .iter()
            .filter_map(|(n, p)| Some((n.clone(), free_gib(p)?)))
            .collect(),
    }
}

/// Look at the host. Existence checks only; nothing is created.
#[must_use]
pub fn gather_facts(config: &HostConfig) -> HostFacts {
    HostFacts {
        kvm: Path::new(DEV_KVM).exists(),
        vmm_binary: config.vmm.binary.exists(),
        firmware: config.vmm.firmware.exists(),
        storage_present: config
            .storage_classes
            .iter()
            .filter(|(_, p)| p.is_dir())
            .map(|(n, _)| n.clone())
            .collect(),
        bridges_present: config
            .network_classes
            .iter()
            .filter(|(_, b)| sys::interface_exists(b))
            .map(|(n, _)| n.clone())
            .collect(),
        vtpm: config.tpm.as_ref().is_some_and(|t| {
            [
                &t.swtpm,
                &t.swtpm_setup,
                &t.setup_config,
                &t.ek_ca_certificate,
            ]
            .iter()
            .all(|p| p.is_file())
        }),
        ek_ca_pem: config
            .tpm
            .as_ref()
            .and_then(|t| std::fs::read_to_string(&t.ek_ca_certificate).ok())
            .and_then(|text| banlieue_provider_sdk::ek::parse_ek_pem_str(&text)),
        capacity: gather_capacity(config),
    }
}

fn host_class(target: Option<&BTreeMap<String, String>>) -> Option<&str> {
    target
        .and_then(|t| t.get(TARGET_KEY_HOST_CLASS))
        .map(String::as_str)
}

/// Publish the host's capacity; a figure it would not give is left out.
fn insert_capacity(raw: &mut BTreeMap<String, String>, c: &HostCapacity) {
    let figures = [
        ("cpus", c.cpus.map(|v| v.to_string())),
        ("cpuModel", c.cpu_model.clone()),
        ("memoryMiB", c.memory_mib.map(|v| v.to_string())),
        ("hugepagesMiB", c.hugepages_mib.map(|v| v.to_string())),
    ];
    for (key, value) in figures {
        if let Some(v) = value {
            raw.insert(key.to_string(), v);
        }
    }
    if !c.storage_free_gib.is_empty() {
        let free: Vec<String> = c
            .storage_free_gib
            .iter()
            .map(|(class, gib)| format!("{class}={gib}"))
            .collect();
        raw.insert("storageFreeGiB".to_string(), free.join(","));
    }
    // This class never offers nested virtualization, whatever the CPU can
    // do: the VMM is always started with `nested=off` (ADR-0061).
    raw.insert("nestedVirtualization".to_string(), "false".to_string());
}

/// The `Provider` status for this host.
#[must_use]
pub fn compute_status(
    provider: &Provider,
    config: &HostConfig,
    facts: &HostFacts,
    generation: i64,
) -> ProviderStatus {
    let caps = &provider.spec.capabilities;
    let mut missing: Vec<String> = Vec::new();

    let mut storage = Vec::new();
    for c in &caps.storage_classes {
        match host_class(c.target.as_ref()) {
            Some(h) if facts.storage_present.contains(h) => storage.push(c.name.clone()),
            _ => missing.push(format!("storageClass {}", c.name)),
        }
    }
    let mut network = Vec::new();
    for c in &caps.network_classes {
        match host_class(c.target.as_ref()) {
            Some(h) if facts.bridges_present.contains(h) => network.push(c.name.clone()),
            _ => missing.push(format!("networkClass {}", c.name)),
        }
    }

    let name = provider.name_any();
    let mut labels = provider.metadata.labels.clone().unwrap_or_default();
    labels.insert("name".to_string(), name.clone());

    let mut raw = BTreeMap::new();
    raw.insert(
        "vmm".to_string(),
        format!("cloud-hypervisor {}", config.vmm.version),
    );
    raw.insert(
        "hostStorageClasses".to_string(),
        config.storage_class_names().join(","),
    );
    raw.insert(
        "hostNetworkClasses".to_string(),
        config.network_class_names().join(","),
    );
    insert_capacity(&mut raw, &facts.capacity);

    let fd = FailureDomain {
        name,
        labels,
        attributes: FailureDomainAttributes {
            available_storage_classes: storage,
            available_network_classes: network,
            features: caps
                .features
                .iter()
                .filter(|f| f.as_str() != FEATURE_VTPM || facts.vtpm)
                .cloned()
                .collect(),
            raw,
        },
    };

    // Start from what is published, so an unchanged condition keeps its
    // lastTransitionTime instead of moving it every pass.
    let mut conditions = provider
        .status
        .as_ref()
        .map(|s| s.conditions.clone())
        .unwrap_or_default();
    set_condition(
        &mut conditions,
        condition_types::PROVIDER_REACHABLE,
        condition_status::TRUE,
        reasons::RECONCILED,
        "host-resident provider running".to_string(),
        generation,
    );

    let mut host_gaps = Vec::new();
    if !facts.kvm {
        host_gaps.push("/dev/kvm");
    }
    if !facts.vmm_binary {
        host_gaps.push("cloud-hypervisor binary");
    }
    if !facts.firmware {
        host_gaps.push("CLOUDHV.fd firmware");
    }
    // Checked first: a misconfigured Provider is refused whatever the host
    // looks like, and the Secret it names is never read.
    let credentials_named = provider.spec.connection.credentials_ref.is_some();
    let (status, reason, message) = if credentials_named {
        (
            condition_status::FALSE,
            reasons::CREDENTIALS_NOT_ALLOWED,
            "spec.connection.credentialsRef must be unset: this provider runs on the host and \
             reads no Secret (ADR-0060 Decision 4)"
                .to_string(),
        )
    } else if !host_gaps.is_empty() {
        (
            condition_status::FALSE,
            reasons::HOST_NOT_READY,
            format!("host is missing: {}", host_gaps.join(", ")),
        )
    } else if !missing.is_empty() {
        (
            condition_status::FALSE,
            reasons::CAPABILITIES_INCOMPLETE,
            format!(
                "declared but not served on this host: {}",
                missing.join(", ")
            ),
        )
    } else {
        (
            condition_status::TRUE,
            reasons::RECONCILED,
            "every declared class is served".to_string(),
        )
    };
    set_condition(
        &mut conditions,
        condition_types::READY,
        status,
        reason,
        message,
        generation,
    );

    ProviderStatus {
        // A host that cannot run guests advertises no failure domain, so the
        // scheduler places nothing here.
        failure_domains: if host_gaps.is_empty() && !credentials_named {
            vec![fd]
        } else {
            vec![]
        },
        conditions,
        // Owned by banlieue-operator's field manager (ADR-0012).
        workload: None,
        observed_generation: Some(generation),
        // Decision 6: the anchor for this host's guests' EK certificates.
        ek_ca_certificates: if facts.vtpm {
            facts.ek_ca_pem.clone().into_iter().collect()
        } else {
            Vec::new()
        },
    }
}

/// Whether `provider` is the one this host serves.
#[must_use]
pub fn is_this_host(provider: &Provider, config: &HostConfig) -> bool {
    provider.name_any() == config.provider.name
        && provider.namespace().as_deref() == Some(config.provider.namespace.as_str())
}

/// Reconcile this host's `Provider`: look at the host, publish what it can
/// serve. Requeued on a long period because host facts (a bridge coming
/// up, the firmware being installed) change without any Kubernetes event.
///
/// # Errors
/// Kubernetes API errors from the status patch.
pub async fn reconcile(provider: Arc<Provider>, ctx: Arc<Context>) -> Result<Action> {
    if !is_this_host(&provider, &ctx.config) {
        return Ok(Action::await_change());
    }
    let name = provider.name_any();
    let generation = provider.metadata.generation.unwrap_or(0);
    let facts = gather_facts(&ctx.config);
    let status = compute_status(&provider, &ctx.config, &facts, generation);
    if status.failure_domains.is_empty() {
        warn!(provider = %name, ?facts, "host cannot run guests");
    }
    patch_status(&ctx, &name, &status).await?;
    info!(provider = %name, "Provider status published");
    Ok(requeue_long())
}

async fn patch_status(ctx: &Context, name: &str, status: &ProviderStatus) -> Result<()> {
    let patch = json!({
        "apiVersion": Provider::api_version(&()).to_string(),
        "kind": Provider::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": status,
    });
    let api: Api<Provider> = Api::namespaced(ctx.client.clone(), &ctx.config.provider.namespace);
    api.patch_status(
        name,
        // Scoped per ADR-0087. Force is still right here: this host is the
        // sole writer of its OWN Provider's status, so there is no other
        // manager to take fields from.
        &PatchParams::apply(&ctx.config.field_manager()).force(),
        &Patch::Apply(&patch),
    )
    .await
    .map_err(Error::from)?;
    Ok(())
}

/// Requeue after a reconcile error.
pub fn error_policy(_p: Arc<Provider>, e: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %e, "Provider reconcile error");
    requeue_on_error()
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
