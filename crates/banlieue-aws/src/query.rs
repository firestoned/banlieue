// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The EC2 Query protocol's request body (ADR-0174 Decision 2).
//!
//! An EC2 call is a `POST /` whose body is the form
//! `Action=...&Version=2016-11-15&...`, sent as [`FORM_CONTENT_TYPE`] and
//! signed with SigV4 as the payload. The body is exactly SigV4's canonical
//! query of the parameters ([`crate::sigv4::canonical_query`]: RFC 3986
//! encoding, a space as `%20`, never `+`, sorted by encoded key), so the
//! same parameters always give the same bytes and so the same payload hash.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::sigv4::canonical_query;

/// EC2's Query API version, pinned (the API's only version); changed
/// deliberately, never discovered.
pub const EC2_API_VERSION: &str = "2016-11-15";
/// The `Content-Type` of a Query protocol body.
pub const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded; charset=utf-8";

const ACTION_KEY: &str = "Action";
const VERSION_KEY: &str = "Version";

/// The parameters of one EC2 Query call: `Action`, `Version`, and the
/// action's own parameters (`InstanceId.1`, `Filter.1.Name`, ...).
///
/// Each key appears once: a second value for a key is a bug in the caller
/// (two `TagSpecification.1...` entries), refused rather than sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryParams {
    params: BTreeMap<String, String>,
}

impl QueryParams {
    /// Parameters for `action`, with [`EC2_API_VERSION`].
    #[must_use]
    pub fn new(action: &str) -> Self {
        Self::with_version(action, EC2_API_VERSION)
    }

    /// Parameters for `action` of another Query protocol service, with that
    /// service's own pinned `version` (STS is `2011-06-15`, see
    /// [`crate::sts::STS_API_VERSION`]).
    #[must_use]
    pub fn with_version(action: &str, version: &str) -> Self {
        let mut params = BTreeMap::new();
        params.insert(ACTION_KEY.to_string(), action.to_string());
        params.insert(VERSION_KEY.to_string(), version.to_string());
        Self { params }
    }

    /// Adds one parameter.
    ///
    /// # Errors
    /// [`Error::Invalid`] if `key` is empty or already present (`Action`
    /// and `Version` included); nothing is added.
    pub fn push(&mut self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        let key = key.into();
        self.check_new_key(&key)?;
        self.params.insert(key, value.into());
        Ok(())
    }

    /// Adds every pair, or none: a pair whose key is empty, already
    /// present, or repeated within `pairs` refuses the whole call.
    ///
    /// # Errors
    /// [`Error::Invalid`] as [`QueryParams::push`]; nothing is added.
    pub fn extend(&mut self, pairs: impl IntoIterator<Item = (String, String)>) -> Result<()> {
        let mut staged: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in pairs {
            self.check_new_key(&key)?;
            if staged.contains_key(&key) {
                return Err(duplicate(&key));
            }
            staged.insert(key, value);
        }
        self.params.append(&mut staged);
        Ok(())
    }

    /// The value of `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }

    /// The parameters, unencoded, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.params.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The `application/x-www-form-urlencoded` body: `key=value` pairs
    /// joined with `&`, encoded as SigV4 encodes a query (so `/`, `=`, `+`
    /// and space are escaped), sorted by encoded key.
    #[must_use]
    pub fn encode(&self) -> String {
        let pairs: Vec<(String, String)> = self
            .params
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        canonical_query(&pairs)
    }

    fn check_new_key(&self, key: &str) -> Result<()> {
        if key.is_empty() {
            return Err(Error::Invalid(
                "EC2 query: a parameter has an empty name".to_string(),
            ));
        }
        if self.params.contains_key(key) {
            return Err(duplicate(key));
        }
        Ok(())
    }
}

fn duplicate(key: &str) -> Error {
    Error::Invalid(format!("EC2 query: parameter {key:?} is set twice"))
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod query_tests;
