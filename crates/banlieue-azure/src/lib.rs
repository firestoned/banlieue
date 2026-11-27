// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A first-party client for Azure Resource Manager (ADR-0164).
//!
//! Protocol only: no `kube`, no banlieue API types. The only credential is a
//! Kubernetes ServiceAccount token exchanged at Entra ID (ADR-0159).
//!
//! This first increment holds the pure pieces, each tested against what ARM
//! did in the roadmap 23 phase 0 spike:
//!
//! - [`tags`]: ownership tags under the names Azure accepts (ADR-0158).
//! - [`encryption`]: the disk encryption set a `Provider` names (ADR-0172).
//! - [`error`]: ARM error bodies and statuses as typed errors.
//! - [`retry`]: the `Retry-After` header, parsed and capped.
//! - [`operation`]: long-running operation handles and statuses.
//! - [`vm`]: the VM and NIC request bodies.
//! - [`token`]: the federated token exchange.
//! - [`types`]: what ARM returns, read into the facts the provider decides
//!   on: usable zones per size, quota headroom, provisioning and power
//!   state, the NIC's address, and tagged orphans.
//!
//! - [`gallery`]: publishing into a Compute Gallery (ADR-0162): the
//!   deterministic names, the requests, and what ARM reports back.
//! - [`upload`]: Put Page to an upload disk through a write SAS, all-zero
//!   ranges skipped, a few in parallel, never with the ARM token.
//!
//! - [`auth`]: the federated token source, cached and refreshed.
//! - [`client`]: [`ArmClient`], the HTTP implementation of [`api::ArmApi`].
//!
//! TLS: [`ArmClient`] uses the process's rustls provider, which the binary
//! installs: `banlieue-tls`'s aws-lc-rs build, offering X25519MLKEM768 first
//! (ADR-0170). ARM and Entra ID do not accept it yet (measured 2026-11-26),
//! so those handshakes fall back to a classical group until they do.

pub mod api;
pub mod assertion;
pub mod auth;
pub mod client;
pub mod encryption;
pub mod error;
pub mod fake;
pub mod fake_blob;
pub mod gallery;
pub mod operation;
pub mod retry;
pub mod tags;
pub mod token;
pub mod types;
pub mod upload;
pub mod vm;

#[cfg(test)]
mod test_server;

pub use api::{ArmApi, DiskAccess, PowerAction, PublishApi};
pub use assertion::{AssertionSource, FileAssertion, OpenBaoAssertion, OpenBaoConfig};
pub use auth::{FederatedConfig, FederatedToken, TokenProvider};
pub use client::{ArmClient, ArmConfig};
pub use error::{Error, Result};
pub use gallery::{
    GalleryImageDefinition, GalleryImageVersion, ManagedDisk, Snapshot, gallery_version_name,
    image_definition_name,
};
pub use operation::{OperationHandle, OperationKind, OperationStatus};
pub use tags::{OwnershipTags, TAG_SOURCE_SHA256};
pub use token::AccessToken;
pub use upload::{PageWriter, PageWriterFactory, SasUrl};
pub use vm::{NicRequest, VmRequest};
