// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Azure provider reconcilers.
//!
//! - [`azurevirtualmachine`]: a scheduled VM becomes an Azure VM (ADR-0164).
//! - [`provider`]: zones, usable sizes and quota headroom as failure domains
//!   (ADR-0158, ADR-0160, ADR-0161), and the orphan sweep's trigger.
//! - [`image`]: a `VMImage`'s gallery image version: adopted for a
//!   `Template` source, published and kept to the declared state for a `Url`
//!   source (ADR-0162 Decisions 4 and 8).
//! - [`gallery`]: the publish decisions, as pure functions.
//! - [`publish_job`]: the publish and delete Jobs (ADR-0162 Decision 1).
//! - [`resources`]: the tagged resources banlieue owns, and the orphan
//!   sweep (ADR-0158 Decisions 5 and 6).

pub mod azurevirtualmachine;
pub mod gallery;
pub mod image;
pub mod provider;
pub mod publish_job;
pub mod resources;
