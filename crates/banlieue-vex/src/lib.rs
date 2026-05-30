// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-vex
//!
//! Auto-VEX derivation tools for the banlieue release/supply-chain pipeline
//! (see `docs/adr/0006-release-and-supply-chain-pipeline.md`). Ported from the
//! 5-spot reference implementation.
//!
//! Two pure modules drive two thin CLI binaries:
//!
//! - [`auto_vex_presence`] — emit `not_affected + component_not_present` for
//!   Grype findings whose affected purl is absent from every image SBOM.
//! - [`auto_vex_reachability`] — emit `not_affected +
//!   vulnerable_code_not_in_execute_path` for Grype CVEs whose curated affected
//!   library symbols are all absent from the release binary's dynamic
//!   symbol-import table.
//!
//! All I/O lives in `src/bin/`; the modules are pure and exhaustively tested.

pub mod auto_vex_presence;
pub mod auto_vex_reachability;
