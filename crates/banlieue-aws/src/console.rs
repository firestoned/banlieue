// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The guest's ready line on the serial console (ADR-0175 Decision 5).
//!
//! The image's `banlieue-ec2-guest ready` writes one line to `/dev/kmsg`
//! once user data has applied:
//!
//! ```text
//! banlieue-ec2-guest: GuestReady instance=<instance-id>
//! ```
//!
//! It reaches the console the provider reads with `GetConsoleOutput`
//! (ADR-0174 Decision 3), usually behind a kernel timestamp
//! (`[   12.345678] `) and with `\r\n` endings. The provider accepts it
//! only with its own instance's ID: a line naming another instance is
//! reported, never taken as ready.

/// What precedes the instance ID on the ready line.
pub const READY_MARKER: &str = "banlieue-ec2-guest: GuestReady instance=";

/// What the console says about readiness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadyState {
    /// A complete line names exactly the expected instance.
    Ready,
    /// No complete ready line yet.
    NotYet,
    /// Ready lines, but only for this other instance ID (the first seen);
    /// never ready.
    OtherInstance(String),
}

/// Reads `console_text` for the ready line of `expected_instance_id`.
///
/// Only complete lines are read: text after the last `\n` may be cut off
/// by the console snapshot (`i-0abc` of `i-0abcdef...`), so it waits for
/// the next read. The ID must equal `expected_instance_id` exactly; a
/// prefix of it, or a longer ID, is another instance.
#[must_use]
pub fn ready_state(console_text: &str, expected_instance_id: &str) -> ReadyState {
    let mut other: Option<String> = None;
    for id in reported_ids(console_text) {
        if !expected_instance_id.is_empty() && id == expected_instance_id {
            return ReadyState::Ready;
        }
        if other.is_none() {
            other = Some(id.to_string());
        }
    }
    match other {
        Some(id) => ReadyState::OtherInstance(id),
        None => ReadyState::NotYet,
    }
}

/// The instance IDs named on the complete ready lines, in order.
fn reported_ids(console_text: &str) -> impl Iterator<Item = &str> {
    let complete = match console_text.rfind('\n') {
        Some(end) => &console_text[..end],
        None => "",
    };
    complete.split('\n').filter_map(|line| {
        let (_, rest) = line.split_once(READY_MARKER)?;
        let id = rest.split_whitespace().next()?;
        Some(id)
    })
}

#[cfg(test)]
#[path = "console_tests.rs"]
mod console_tests;
