// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A first-party client for the Proxmox VE REST API (ADR-0074).
//!
//! Protocol only: no `kube`, no banlieue API types. API tokens are the only
//! credential; every mutation returns a [`Upid`] that [`ProxmoxApi::wait_task`]
//! awaits, bounded.
//!
//! - [`api`]: the [`ProxmoxApi`] trait reconcilers depend on.
//! - [`fake`]: an in-memory implementation that refuses what Proxmox refuses.
//! - [`types`], [`wire`], [`token`], [`upid`], [`error`]: the pure pieces.

pub mod api;
pub mod client;
pub mod error;
pub mod fake;
pub mod token;
pub mod types;
pub mod upid;
pub mod wire;

pub use api::{ProxmoxApi, TASK_POLL_INTERVAL};
pub use client::{Client, ClientConfig};
pub use error::{Error, Result};
pub use fake::FakeProxmox;
pub use token::ApiToken;
pub use types::{
    CloneParams, ClusterVm, GuestInterface, GuestIp, NetworkIface, Node, Storage, TaskStatus,
    Version, VmConfig, VmId, VmStatus, Volume,
};
pub use upid::Upid;
pub use wire::Params;
