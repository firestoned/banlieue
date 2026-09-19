// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! XML escaping for domain XML (ADR-0050 Decision 5).
//!
//! Every value that reaches libvirt's domain XML is user- or admin-influenced
//! — domain names, volume paths, bridge and network names, MAC addresses,
//! machine types. Formatting one of those into markup unescaped is an
//! injection: a bridge name of `br0'/><disk .../><x a='` adds a device of the
//! caller's choosing to the domain. So `format!` of a raw string into a
//! template is prohibited, and [`esc`] is the only way a value gets in.
//!
//! # Why one function rather than two
//!
//! The usual split is an attribute escaper (which must handle quotes) and a
//! text escaper (which must not bother). That split creates a decision at
//! every call site, and a wrong decision is silent. [`esc`] escapes all five
//! predefined entities, so its output is safe in either position and there is
//! nothing to get wrong. The cost is a few `&quot;` in element text, which no
//! XML parser minds.
//!
//! # Why some input is rejected rather than escaped
//!
//! Most C0 control characters have no representation in XML 1.0 at all — not
//! literally, and not as a numeric character reference either. Escaping
//! cannot rescue them. Dropping them silently would rename a domain behind
//! the operator's back, so they are an error instead.

/// Why a value could not be put into XML.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XmlError {
    /// The value contains a character XML 1.0 cannot represent in any form.
    #[error(
        "value contains a character XML cannot represent \
         (U+{codepoint:04X} at character {position})"
    )]
    IllegalChar {
        /// Character index (not byte offset) of the offending character.
        position: usize,
        /// The offending Unicode scalar value.
        codepoint: u32,
    },
}

/// Tab. Legal in XML 1.0, and common in cloud-config payloads.
const TAB: char = '\t';
/// Line feed. Legal in XML 1.0.
const LF: char = '\n';
/// Carriage return. Legal in XML 1.0.
const CR: char = '\r';
/// First character of the C0 control block that XML 1.0 permits generally.
const FIRST_LEGAL_CONTROL_CHAR: char = '\u{20}';

/// Escape `value` so it is safe in both an attribute value and element text.
///
/// # Errors
/// [`XmlError::IllegalChar`] if `value` contains a character XML 1.0 cannot
/// represent — every C0 control character except tab, LF and CR.
pub fn esc(value: &str) -> Result<String, XmlError> {
    // Most values need no escaping at all; only pay for a new allocation's
    // growth when something actually expands.
    let mut out = String::with_capacity(value.len());
    for (position, ch) in value.chars().enumerate() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            TAB | LF | CR => out.push(ch),
            c if c < FIRST_LEGAL_CONTROL_CHAR => {
                return Err(XmlError::IllegalChar {
                    position,
                    codepoint: c as u32,
                });
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "escape_tests.rs"]
mod escape_tests;
