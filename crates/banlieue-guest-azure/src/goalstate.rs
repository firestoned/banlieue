// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! WireServer's goal state in, the `Health` report out (ADR-0163).
//!
//! The guest reads three values from `GET /machine/?comp=goalstate`
//! (`Incarnation`, `ContainerId`, the role's `InstanceId`) and echoes them in
//! the `Health` document it posts to `comp=health` with
//! `<State>Ready</State>`. Those values are interpolated into XML, so each
//! is checked against a narrow character set first: a goal state carrying
//! anything else is refused rather than escaped.

use thiserror::Error;

/// Longest value accepted for any of the three fields.
const MAX_FIELD_LEN: usize = 128;

/// Why a goal state was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GoalStateError {
    /// The document is not UTF-8.
    #[error("goal state is not UTF-8")]
    NotUtf8,
    /// A required element is missing or empty.
    #[error("goal state has no {0}")]
    Missing(&'static str),
    /// An element carries characters outside its allowed set, or is too long.
    #[error("goal state {0} is not acceptable")]
    Invalid(&'static str),
}

/// The three values a `Health` report echoes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalState {
    /// `Incarnation`: decimal digits.
    pub incarnation: String,
    /// `Container/ContainerId`: a GUID.
    pub container_id: String,
    /// `RoleInstance/InstanceId`: the role instance's identifier.
    pub instance_id: String,
}

/// The text of the first `<name>...</name>` element, without attributes.
fn element_text<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find(&close)?;
    Some(xml[start..start + len].trim())
}

fn field<'a>(
    xml: &'a str,
    name: &'static str,
    allowed: fn(u8) -> bool,
) -> Result<&'a str, GoalStateError> {
    let value = element_text(xml, name)
        .filter(|v| !v.is_empty())
        .ok_or(GoalStateError::Missing(name))?;
    if value.len() > MAX_FIELD_LEN || !value.bytes().all(allowed) {
        return Err(GoalStateError::Invalid(name));
    }
    Ok(value)
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

fn is_guid_char(b: u8) -> bool {
    b.is_ascii_hexdigit() || b == b'-'
}

fn is_instance_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

/// Parse a goal state document.
///
/// # Errors
/// [`GoalStateError`] for a document that is not UTF-8, lacks a field, or
/// carries a value outside its character set.
pub fn parse_goal_state(document: &[u8]) -> Result<GoalState, GoalStateError> {
    let xml = std::str::from_utf8(document).map_err(|_| GoalStateError::NotUtf8)?;
    Ok(GoalState {
        incarnation: field(xml, "Incarnation", is_digit)?.to_string(),
        container_id: field(xml, "ContainerId", is_guid_char)?.to_string(),
        instance_id: field(xml, "InstanceId", is_instance_char)?.to_string(),
    })
}

/// The `Health` document reporting the role instance `Ready`, in the shape
/// the spike posted and WireServer answered 200 to.
#[must_use]
pub fn health_ready_document(gs: &GoalState) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<Health xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\
<GoalStateIncarnation>{}</GoalStateIncarnation>\
<Container><ContainerId>{}</ContainerId>\
<RoleInstanceList><Role><InstanceId>{}</InstanceId>\
<Health><State>Ready</State></Health></Role></RoleInstanceList></Container></Health>",
        gs.incarnation, gs.container_id, gs.instance_id
    )
}

#[cfg(test)]
#[path = "goalstate_tests.rs"]
mod goalstate_tests;
