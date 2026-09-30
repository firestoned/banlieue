// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pure encode/decode halves of the client: form bodies, path segments, the
//! `{"data": …}` envelope and error messages.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};

/// An ordered set of request parameters, sent as a form body (POST/PUT) or a
/// query string (GET/DELETE). Setting a key again replaces it, so a config
/// update cannot carry two values for one key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params(Vec<(String, String)>);

impl Params {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `key` to any displayable value, replacing an earlier one.
    #[must_use]
    pub fn set(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        let value = value.to_string();
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key.to_string(), value)),
        }
        self
    }

    /// Set `key` only when `value` is `Some`.
    #[must_use]
    pub fn set_opt(self, key: &str, value: Option<impl std::fmt::Display>) -> Self {
        match value {
            Some(v) => self.set(key, v),
            None => self,
        }
    }

    /// Set a boolean the way Proxmox spells it: `1` or `0`.
    #[must_use]
    pub fn flag(self, key: &str, on: bool) -> Self {
        self.set(key, u8::from(on))
    }

    /// The value for `key`, if set.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Whether nothing is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate the pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// `application/x-www-form-urlencoded` form.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut s = form_urlencoded::Serializer::new(String::new());
        for (k, v) in &self.0 {
            s.append_pair(k, v);
        }
        s.finish()
    }
}

/// Percent-encode one URL path segment. `:` stays (volids are
/// `storage:content/name`); `/`, spaces and everything outside the unreserved
/// set are escaped, so a value can never add a path component or a `..`.
#[must_use]
pub fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    // A bare `..` would survive the loop; escape its dots.
    if out == ".." || out == "." {
        return out.replace('.', "%2E");
    }
    out
}

/// Unwrap the `{"data": …}` envelope.
///
/// # Errors
/// [`Error::Decode`] if the body is not JSON, has no `data` key, or `data`
/// is not a `T`.
pub fn decode_data<T: DeserializeOwned>(body: &str) -> Result<T> {
    let mut v: Value = serde_json::from_str(body).map_err(|e| Error::Decode(e.to_string()))?;
    let Some(data) = v.get_mut("data") else {
        return Err(Error::Decode("response has no `data` key".to_string()));
    };
    serde_json::from_value(data.take()).map_err(|e| Error::Decode(e.to_string()))
}

/// Compose an error message from Proxmox's reason phrase and, for parameter
/// validation failures, the body's `errors` map (ADR-0074 Decision 6).
#[must_use]
pub fn api_message(reason: &str, body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let detail = parsed
        .as_ref()
        .and_then(|v| v.get("errors"))
        .and_then(Value::as_object);
    let mut message = reason.trim().to_string();

    if message.is_empty()
        && let Some(m) = parsed
            .as_ref()
            .and_then(|v| v.get("message"))
            .and_then(Value::as_str)
    {
        message = m.to_string();
    }
    if let Some(errors) = detail.filter(|e| !e.is_empty()) {
        let joined: Vec<String> = errors
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}: {}",
                    v.as_str().map_or_else(|| v.to_string(), str::to_string)
                )
            })
            .collect();
        let joined = joined.join("; ");
        message = if message.is_empty() {
            joined
        } else {
            format!("{message}: {joined}")
        };
    }
    if message.is_empty() {
        return "no message from Proxmox".to_string();
    }
    message
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod wire_tests;
