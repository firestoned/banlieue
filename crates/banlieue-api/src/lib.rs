// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! API types and CRD generation for **banlieue**, a Kubernetes-native
//! abstract virtualization API for libvirt, Proxmox, and vSphere.
//!
//! Two API groups are exposed:
//!
//! - [`banlieue`] — the user-facing API group `banlieue.io/v1alpha1`:
//!   [`Provider`], [`ProviderClass`], [`VMClass`], [`VMImage`],
//!   [`VirtualMachine`].
//! - [`infrastructure`] — provider-specific infra CRDs under
//!   `infrastructure.banlieue.io/v1alpha1`. Currently:
//!   [`VSphereCluster`] (InfraCluster), [`VSphereMachine`] (InfraMachine,
//!   with [`VSphereMachineTemplate`]).
//!
//! Provider infra CRDs intentionally satisfy the **CAPI v1beta2 InfraMachine
//! and InfraCluster contracts** so they can be consumed either by banlieue's
//! own `VirtualMachine` controller or by CAPI's `Machine` / `Cluster`
//! controllers.
//!
//! [`Provider`]: banlieue::Provider
//! [`ProviderClass`]: banlieue::ProviderClass
//! [`VMClass`]: banlieue::VMClass
//! [`VMImage`]: banlieue::VMImage
//! [`VirtualMachine`]: banlieue::VirtualMachine
//! [`VSphereCluster`]: infrastructure::VSphereCluster
//! [`VSphereMachine`]: infrastructure::VSphereMachine
//! [`VSphereMachineTemplate`]: infrastructure::VSphereMachineTemplate

pub mod banlieue;
pub mod common;
pub mod infrastructure;

/// CRD post-processing shared by the `crdgen` binary and `banlieue bootstrap`.
///
/// Deliberately **not** gated behind the `crdgen` feature: bootstrap builds
/// CRDs from these same Rust types at runtime so the schemas it applies are by
/// construction the ones the running binary implements (ADR-0013). Only the
/// `serde_yaml` dependency stays feature-gated.
pub mod crdgen_support;

/// Markdown API-reference generation used by the `crddoc` binary. Only
/// compiled with the `crdgen` feature.
#[cfg(feature = "crdgen")]
pub mod crddoc;

/// Re-export of the most commonly used items.
pub mod prelude {
    pub use crate::banlieue::*;
    pub use crate::common::*;
    pub use crate::infrastructure::*;
}
