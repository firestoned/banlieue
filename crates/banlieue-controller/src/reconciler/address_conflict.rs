// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Duplicate static-address detection (ADR-0083).
//!
//! A `VirtualMachine` that declares a static address another VM already
//! claims on the same `networkClass` is blocked before scheduling, so two
//! guests are never configured with one address. Only banlieue's own VMs are
//! considered: nothing here asks a hypervisor, an IPAM or the network.
//! Everything is pure; the `virtualmachine` reconcile gathers the inputs and
//! acts on the answer.
//!
//! A *claim* is a `(networkClass, address)` pair, of two kinds:
//!
//! - **declared**: a `spec.networkOverrides[]` entry, scoped by the
//!   `networkClass` of the `VMClass` interface it names;
//! - **held**: a `status.heldAddresses` entry, which the controller writes
//!   only when it applies the VM's infra CR. It records what the guest is
//!   configured with, survives reboots, and is the same on every backend.
//!
//! The decision is one global, greedy order over the VMs that share an
//! address: held claims are reserved first and never reassigned; then each VM,
//! oldest first, is admitted only if none of its declared claims is already
//! reserved by another VM, and reserves them if so. A blocked VM reserves
//! nothing new, and a single order cannot cycle.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use banlieue_api::banlieue::{HeldAddress, VMClass, VirtualMachine};
use k8s_openapi::jiff::Timestamp;
use kube::ResourceExt;

/// `Ready` condition reason for a VM blocked on a duplicate address.
pub const REASON_DUPLICATE_ADDRESS: &str = "DuplicateAddress";

/// Who holds the address this VM was blocked on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    /// A VM in the blocked VM's own namespace, as `namespace/name`.
    SameNamespace(String),
    /// A VM in another namespace. Deliberately unnamed, so the condition
    /// message cannot be used to enumerate another tenant's VMs.
    OtherNamespace,
}

/// Why a VM is blocked: one of its declared addresses is reserved by another
/// VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateAddress {
    /// The contested address, normalised.
    pub address: String,
    /// The `networkClass` the collision is on.
    pub network_class: String,
    /// The blocked VM's interface that declares the address.
    pub interface: String,
    /// The VM that keeps it.
    pub holder: Holder,
}

impl DuplicateAddress {
    /// The `Ready=False` condition message: address, network, interface and
    /// holder, so the fix is readable off `kubectl describe`.
    #[must_use]
    pub fn message(&self) -> String {
        let holder = match &self.holder {
            Holder::SameNamespace(qualified) => format!("VirtualMachine {qualified}"),
            Holder::OtherNamespace => "a VirtualMachine in another namespace".to_string(),
        };
        format!(
            "address {} on interface {} (networkClass {}) is already claimed by {}; \
             choose a free address or delete the holder (ADR-0083)",
            self.address, self.interface, self.network_class, holder
        )
    }
}

/// One claim. `network_class: None` means "unknown" and matches every
/// network: used when a claimant's `VMClass` cannot be resolved, so an
/// unresolvable claimant fails closed rather than being ignored.
#[derive(Clone, Debug)]
struct Claim {
    interface: String,
    network_class: Option<String>,
    address: String,
}

impl Claim {
    fn matches(&self, other: &Self) -> bool {
        self.address == other.address
            && match (&self.network_class, &other.network_class) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }
}

/// `(namespace, name)`: identifies a VM across the store.
type VmKey = (String, String);

fn key(vm: &VirtualMachine) -> VmKey {
    (vm.namespace().unwrap_or_default(), vm.name_any())
}

/// `Some` when `vm` must be blocked because one of its declared addresses is
/// reserved by another VM (ADR-0083).
///
/// # Arguments
/// * `vm` - the VM being reconciled; authoritative over any copy of it in
///   `contenders`
/// * `contenders` - the VMs that share an address with `vm`
///   ([`contending_vms`]); a (possibly stale) copy of `vm` is ignored
/// * `classes` - the `VMClass`es of `vm` and the contenders; a contender
///   whose class is absent claims its declared addresses on every network
#[must_use]
pub fn find_duplicate_address(
    vm: &VirtualMachine,
    contenders: &[VirtualMachine],
    classes: &[VMClass],
) -> Option<DuplicateAddress> {
    let me = key(vm);
    let mut everyone: Vec<&VirtualMachine> = contenders.iter().filter(|c| key(c) != me).collect();
    everyone.push(vm);
    everyone.sort_by(|a, b| {
        created(a)
            .cmp(&created(b))
            .then_with(|| key(a).cmp(&key(b)))
    });

    let class_of = |v: &VirtualMachine| {
        classes
            .iter()
            .find(|c| c.name_any() == v.spec.class_ref.name)
    };
    let mut reserved: Vec<(Claim, VmKey)> = Vec::new();
    let mut blocked: BTreeMap<VmKey, (Claim, VmKey)> = BTreeMap::new();

    // 1. Held claims, oldest first. A held claim is never reassigned; of two
    //    VMs holding the same one, the older keeps it.
    for v in &everyone {
        let k = key(v);
        for claim in held_claims(v) {
            let owner = reserved
                .iter()
                .find(|(r, o)| *o != k && r.matches(&claim))
                .map(|(_, o)| o.clone());
            match owner {
                Some(owner) => {
                    blocked.entry(k.clone()).or_insert((claim, owner));
                }
                None => reserved.push((claim, k.clone())),
            }
        }
    }

    // 2. Declared claims, oldest first. A VM is admitted only if nothing it
    //    declares is reserved by another VM; only then does it reserve.
    for v in &everyone {
        let k = key(v);
        if blocked.contains_key(&k) {
            continue;
        }
        let declared = declared_claims(v, class_of(v));
        let conflict = declared.iter().find_map(|claim| {
            reserved
                .iter()
                .find(|(r, o)| *o != k && r.matches(claim))
                .map(|(_, o)| (claim.clone(), o.clone()))
        });
        match conflict {
            Some(c) => {
                blocked.insert(k, c);
            }
            None => reserved.extend(declared.into_iter().map(|c| (c, k.clone()))),
        }
    }

    let (claim, holder) = blocked.remove(&me)?;
    // Report the claim in this VM's own terms: the interface it declared.
    let mine = declared_claims(vm, class_of(vm))
        .into_iter()
        .find(|c| c.matches(&claim))
        .unwrap_or(claim);
    Some(DuplicateAddress {
        network_class: mine.network_class.clone().unwrap_or_default(),
        interface: mine.interface,
        address: mine.address,
        holder: if holder.0 == me.0 {
            Holder::SameNamespace(format!("{}/{}", holder.0, holder.1))
        } else {
            Holder::OtherNamespace
        },
    })
}

/// The `status.heldAddresses` to record when `vm`'s infra CR is applied:
/// its declared claims, resolved through `class` and normalised. Overrides
/// naming no interface of the class are left out, as the infra builders
/// leave them out.
#[must_use]
pub fn held_addresses(vm: &VirtualMachine, class: &VMClass) -> Vec<HeldAddress> {
    declared_claims(vm, Some(class))
        .into_iter()
        .map(|c| HeldAddress {
            interface: c.interface,
            network_class: c.network_class.unwrap_or_default(),
            address: c.address,
        })
        .collect()
}

/// Every VM other than `vm` that shares an address with it, transitively
/// (declared or held, on any network). Only these can affect `vm`'s outcome,
/// so only their classes need resolving. Matching on the address string
/// alone over-approximates, which is safe: the network is checked later.
#[must_use]
pub fn contending_vms<'a>(
    vm: &VirtualMachine,
    all: impl IntoIterator<Item = &'a VirtualMachine>,
) -> Vec<VirtualMachine> {
    let me = key(vm);
    let mut pending: Vec<&VirtualMachine> = all.into_iter().filter(|v| key(v) != me).collect();
    let mut addresses: BTreeSet<String> = addresses_of(vm);
    let mut found = Vec::new();
    loop {
        let (sharing, rest): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|v| !addresses_of(v).is_disjoint(&addresses));
        if sharing.is_empty() {
            return found;
        }
        for v in sharing {
            addresses.extend(addresses_of(v));
            found.push(v.clone());
        }
        pending = rest;
    }
}

/// Every address `vm` declares or holds, normalised.
fn addresses_of(vm: &VirtualMachine) -> BTreeSet<String> {
    vm.spec
        .network_overrides
        .iter()
        .map(|o| normalize(&o.static_.address))
        .chain(held_claims(vm).into_iter().map(|c| c.address))
        .collect()
}

/// `vm`'s declared claims. An override naming no interface of the class is
/// dropped; with no class, every override claims on every network.
fn declared_claims(vm: &VirtualMachine, class: Option<&VMClass>) -> Vec<Claim> {
    vm.spec
        .network_overrides
        .iter()
        .filter_map(|o| {
            let network_class = match class {
                None => None,
                Some(c) => Some(
                    c.spec
                        .network
                        .interfaces
                        .iter()
                        .find(|nic| nic.name == o.name)?
                        .network_class
                        .clone(),
                ),
            };
            Some(Claim {
                interface: o.name.clone(),
                network_class,
                address: normalize(&o.static_.address),
            })
        })
        .collect()
}

/// `vm`'s held claims, from `status.heldAddresses`.
fn held_claims(vm: &VirtualMachine) -> Vec<Claim> {
    vm.status
        .iter()
        .flat_map(|s| s.held_addresses.iter())
        .map(|h| Claim {
            interface: h.interface.clone(),
            network_class: Some(h.network_class.clone()),
            address: normalize(&h.address),
        })
        .collect()
}

/// Creation time; a VM with none (not yet persisted) ranks as the newest.
fn created(vm: &VirtualMachine) -> Timestamp {
    vm.metadata
        .creation_timestamp
        .as_ref()
        .map_or(Timestamp::MAX, |t| t.0)
}

/// Canonical form of an address: parsed and re-printed when it is an IP
/// (so `2001:0db8::0001` equals `2001:db8::1`), trimmed text otherwise.
fn normalize(address: &str) -> String {
    let trimmed = address.trim();
    trimmed
        .parse::<IpAddr>()
        .map_or_else(|_| trimmed.to_string(), |ip| ip.to_string())
}

#[cfg(test)]
#[path = "address_conflict_tests.rs"]
mod address_conflict_tests;
