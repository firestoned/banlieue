// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue-imagebuilder` reconcilers.
//!
//! One reconciler: [`vmimage`] drives the shared raw-disk build for
//! `VMImage`'s `Url`-kind sources, and [`push`] builds the Job that pushes
//! the result to a registry for host-resident providers (ADR-0064).

pub mod push;
pub mod vmimage;
