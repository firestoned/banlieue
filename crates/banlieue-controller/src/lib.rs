// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-controller
//!
//! The main banlieue controller. Watches [`banlieue_api::banlieue::VirtualMachine`]
//! resources and reconciles them onto provider infrastructure CRDs (initially
//! [`banlieue_api::infrastructure::VSphereMachine`]).
//!
//! The controller is intentionally provider-agnostic — every backend-specific
//! operation lives in a separate `banlieue-provider-*` crate that watches the
//! corresponding `infrastructure.banlieue.io` kind. Communication between this
//! controller and providers is **CRD-only** (no RPC).

pub mod context;
pub mod error;
pub mod reconciler;

pub use context::Context;
pub use error::{Error, Result};
