// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! vSphere provider reconcilers.
//!
//! [`provider`] — capability introspection; [`vmimage`] — per-zone template
//! build. [`vspheremachine`] currently ships only the create-path's pure
//! guestinfo-construction logic (ADR-0024); its watch loop and the actual
//! `CloneVM_Task` call are follow-up work, not yet wired into `app.rs`.

pub mod ca_bundle;
pub mod provider;
pub mod vmimage;
pub mod vspheremachine;
