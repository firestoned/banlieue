// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! vSphere provider reconcilers.
//!
//! Phase 1B iteration 1 ships only the [`provider`] reconciler — capability
//! introspection against a real vCenter (or vcsim). The `VSphereMachine`
//! reconciler (VM lifecycle) lands in iteration 2.

pub mod ca_bundle;
pub mod provider;
pub mod vmimage;
