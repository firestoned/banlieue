// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Controller reconcilers.
//!
//! Phase 1A iteration 3: scheduler, infra builder, status mirror, migration
//! sub-loop, and the main `virtualmachine` reconcile loop.

pub mod infra;
pub mod migration;
pub mod scheduler;
pub mod status_mirror;
pub mod virtualmachine;
pub mod vsphere_cluster;
