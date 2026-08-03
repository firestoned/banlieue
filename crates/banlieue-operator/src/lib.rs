// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-operator
//!
//! The provider lifecycle controller — what makes banlieue an operator in the
//! strict sense. It watches [`Provider`] and [`ProviderClass`] CRs and turns
//! each Provider into a running backend controller: one Deployment,
//! ServiceAccount, Role, RoleBinding and ClusterRoleBinding **per Provider**
//! (ADR-0003), built from the install metadata its ProviderClass carries
//! (ADR-0012).
//!
//! Per-instance rather than per-class is the whole point: a hung or slow
//! reconcile against one backend cannot stall any other, each pod holds
//! exactly one backend's credentials, and per-backend network policy becomes
//! expressible.
//!
//! This crate holds no backend credentials and speaks to no backend SDK. It
//! creates workloads through the Kubernetes API and reads desired state from
//! CRs; the spawned provider talks to its backend on its own. Communication
//! between roles is **CRD-only**.
//!
//! This crate is a library: the unified `banlieue` binary calls [`run`] for the
//! `banlieue operator` subcommand (ADR-0004). It has no `main`.
//!
//! [`Provider`]: banlieue_api::banlieue::Provider
//! [`ProviderClass`]: banlieue_api::banlieue::ProviderClass

pub mod app;
pub mod bootstrap;
pub mod context;
pub mod error;
pub mod events;
pub mod naming;
pub mod reconciler;
pub mod workload;

pub use app::{Cli, run};
pub use context::Context;
pub use error::{Error, Result};
pub use workload::{
    WorkloadInputs, WorkloadSet, build_cluster_role_binding, build_deployment, build_role,
    build_role_binding, build_service_account, build_workload, owner_reference,
};
