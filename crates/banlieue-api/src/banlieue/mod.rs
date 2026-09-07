// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` API group.

pub mod provider;
pub mod providerclass;
pub mod virtualmachine;
pub mod vmclass;
pub mod vmimage;

pub use provider::{
    FailureDomain, FailureDomainAttributes, FailureDomainNameOverride, NetworkClassMapping,
    Provider, ProviderCapabilities, ProviderConnection, ProviderSpec, ProviderStatus,
    ProviderWorkloadStatus, ScopedSubnet, ScopedTarget, StorageClassMapping, SubnetShape,
};
pub use providerclass::{
    DEFAULT_PROVIDER_REPLICAS, ImagePullPolicy, LoggingSpec, ProviderClass, ProviderClassSpec,
    ProviderClassStatus, ProviderImage,
};
pub use virtualmachine::{
    AffinityMode, AntiAffinityRule, DEFAULT_USER_DATA_KEY, DiskOverride, HardwareOverride,
    MigrationPolicy, NetworkInterfaceOverride, PlacementSpec, ResolvedResource, ScheduledPlacement,
    UserDataSpec, VirtualMachine, VirtualMachineSpec, VirtualMachineStatus,
};
pub use vmclass::{
    DiskSpec, HardwareSpec, NetworkInterfaceSpec, NetworkSpec, VMClass, VMClassSpec,
};
pub use vmimage::{
    Architecture, BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus, DiskController,
    GuestAgent, ImagePerProviderStatus, ImageSource, ImageSourceKind, InstallMode, IsoOverlayFile,
    IsoOverlaySource, NicAdapter, OsFamily, VMImage, VMImageSpec, VMImageStatus, VMImageTemplate,
    VMImageTemplateDisk, VMImageTemplateNic, ZoneImageStatus,
};
