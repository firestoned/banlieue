// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Domain XML construction (ADR-0050 Decision 6).
//!
//! libvirt's API surface for defining a domain is a single XML document, so
//! this is where banlieue's `LibvirtMachine` spec becomes something libvirtd
//! will accept. Two rules hold throughout:
//!
//! 1. **Nothing reaches the document unescaped.** See [`escape::esc`].
//! 2. **The builders are pure.** They take a spec and return a `String`, with
//!    no connection and no I/O, so every shape is a table test.

pub mod domain;
pub mod escape;

pub use domain::{DomainXmlError, DomainXmlInput, build_domain_xml, ejected_install_cdrom_xml};
pub use escape::{XmlError, esc};
