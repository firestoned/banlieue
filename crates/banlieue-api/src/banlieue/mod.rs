// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` API group.

pub mod provider;
pub mod virtualmachine;
pub mod vmclass;
pub mod vmimage;

pub use provider::{
    FailureDomain, FailureDomainAttributes, NetworkClassMapping, Provider, ProviderCapabilities,
    ProviderConnection, ProviderSpec, ProviderStatus, StorageClassMapping,
};
pub use virtualmachine::{
    AffinityMode, AntiAffinityRule, MigrationPolicy, PlacementSpec, ResolvedResource,
    ScheduledPlacement, UserDataSpec, VirtualMachine, VirtualMachineSpec, VirtualMachineStatus,
};
pub use vmclass::{
    DiskSpec, HardwareSpec, NetworkInterfaceSpec, NetworkSpec, VMClass, VMClassSpec,
};
pub use vmimage::{
    Architecture, GuestAgent, ImagePerProviderStatus, ImageSource, ImageSourceKind, OsFamily,
    VMImage, VMImageSpec, VMImageStatus,
};
