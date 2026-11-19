// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! vSphere provider reconcilers.
//!
//! [`provider`] — capability introspection; [`vmimage`] — per-zone template
//! build. [`vspheremachine`] is wired into `app.rs` and drives the
//! `CloneVM_Task` + `PowerOnVM_Task` create path (ADR-0024); it is
//! create-path only — update/drift reconciliation and live migration beyond
//! a recreate-on-change fallback are not yet implemented (ADR-0036).

pub mod ca_bundle;
pub mod provider;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod vmimage;
pub mod vspheremachine;
