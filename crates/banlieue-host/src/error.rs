// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Why a `banlieue host cloud-hypervisor` verb failed.

use std::io;

/// Why a `banlieue host cloud-hypervisor` verb failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation or command failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// A setting's value is not usable.
    #[error("invalid {name}: {why}")]
    Setting {
        /// The flag.
        name: &'static str,
        /// Why.
        why: String,
    },
    /// The host is not fit for guests; every problem found.
    #[error("preflight failed:\n  {}", .0.join("\n  "))]
    Preflight(Vec<String>),
    /// The pieces do not work together; every problem found.
    #[error("self-test failed:\n  {}", .0.join("\n  "))]
    Selftest(Vec<String>),
    /// `--only <stage>` without what that stage builds on.
    #[error("stage {stage} needs {}: run those stages first", .missing.join(", "))]
    Prerequisite {
        /// The stage asked for.
        stage: &'static str,
        /// What is missing.
        missing: Vec<String>,
    },
    /// A downloaded or supplied artifact does not match its digest.
    #[error("{name}: sha256 {got}, expected {want}; nothing was installed")]
    Pin {
        /// The artifact.
        name: &'static str,
        /// What it hashed to.
        got: String,
        /// What it must hash to.
        want: String,
    },
    /// An artifact could not be fetched.
    #[error("{name}: {why}")]
    Fetch {
        /// The artifact.
        name: &'static str,
        /// Why.
        why: String,
    },
    /// A template kept a placeholder no value filled.
    #[error("template {template}: no value for {placeholder}")]
    Template {
        /// The template.
        template: &'static str,
        /// The placeholder.
        placeholder: String,
    },
    /// The rendered host config does not parse as the provider would.
    #[error("the host config this would write does not load: {0}")]
    Config(String),
    /// The verb needs root.
    #[error("{0} must run as root")]
    NotRoot(&'static str),
    /// Not supported on this host.
    #[error("{0}")]
    Unsupported(String),
}
