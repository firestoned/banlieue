// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Collision-safe Kubernetes object names. Lives in the provider SDK
//! ([`banlieue_provider_sdk::k8s_name`]), shared with the Azure provider's
//! publish Jobs; re-exported here so existing paths keep working.

pub use banlieue_provider_sdk::k8s_name::{MAX_NAME_LEN, collision_safe_name};
