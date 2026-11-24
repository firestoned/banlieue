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
//! - [`error`]: ARM error bodies and statuses as typed errors.
//! - [`retry`]: the `Retry-After` header, parsed and capped.
//! - [`operation`]: long-running operation handles and statuses.
//! - [`vm`]: the VM and NIC request bodies.
//! - [`token`]: the federated token exchange.
//!
//! TLS: the HTTP client added in the next increment uses the process's
//! rustls provider. Post-quantum key exchange (X25519MLKEM768) needs the
//! workspace to move from `ring` to `aws-lc-rs`, roadmap 22's open phase 0
//! question; ARM and Entra ID do not negotiate it yet in any case.

pub mod error;
pub mod operation;
pub mod retry;
pub mod tags;
pub mod token;
pub mod vm;

pub use error::{Error, Result};
pub use operation::{OperationHandle, OperationKind, OperationStatus};
pub use tags::OwnershipTags;
pub use token::AccessToken;
pub use vm::{NicRequest, VmRequest};
