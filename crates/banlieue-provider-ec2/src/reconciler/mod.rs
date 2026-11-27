// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! EC2 provider reconcilers.
//!
//! - [`ec2machine`]: a scheduled VM becomes a hardened EC2 instance
//!   (ADR-0174 Decision 3).
//! - [`provider`]: zones and offered instance types as failure domains
//!   (ADR-0158, ADR-0160), and the orphan sweep's trigger.
//! - [`image`]: a `VMImage`'s `Template` source (a hand-published AMI ID)
//!   as one ready row per `Provider`; publishing is roadmap 24 phase D.
//! - [`instances`]: the tagged instances banlieue owns, and the orphan sweep
//!   (ADR-0158 Decisions 4 to 6).

pub mod ec2machine;
pub mod image;
pub mod instances;
pub mod provider;
