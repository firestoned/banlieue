// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue-imagebuilder` reconcilers.
//!
//! One reconciler today: [`vmimage`] drives the shared raw-disk build for
//! `VMImage`'s `Url`-kind sources.

pub mod vmimage;
