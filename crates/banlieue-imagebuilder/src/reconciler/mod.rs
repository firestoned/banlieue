// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue-imagebuilder` reconcilers.
//!
//! One reconciler: [`vmimage`] drives the shared raw-disk build for
//! `VMImage`'s `Url`-kind sources, [`digest`] builds the Job that measures
//! the result (ADR-0010, amendment 2026-11-26), [`prelay`] the one that
//! pre-lays it (ADR-0113), and [`push`] the one that pushes it to a
//! registry for host-resident providers (ADR-0064).

pub mod digest;
mod job;
pub mod prelay;
pub mod push;
pub mod vmimage;
