// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Ownership tags on every Azure resource banlieue creates (ADR-0158
//! Decision 4).
//!
//! Azure refuses tag names containing `<`, `>`, `%`, `&`, `\`, `?` or `/`
//! (`InvalidTagNameCharacters`, seen live), so the Kubernetes convention
//! `banlieue.io/owner-uid` cannot be used; the names are dash-separated.
//! banlieue deletes only a resource whose [`TAG_OWNER_UID`] equals the object
//! it acts for, and adopts a tagged resource instead of creating another.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// The owning object's `metadata.uid`.
pub const TAG_OWNER_UID: &str = "banlieue-owner-uid";
/// The kind that created the resource.
pub const TAG_OWNER_KIND: &str = "banlieue-owner-kind";
/// `<namespace>.<name>` of the `Provider` the resource belongs to.
pub const TAG_PROVIDER: &str = "banlieue-provider";
/// The owning object's namespace (ADR-0158 Decision 4, amended 2026-11-25):
/// lets a provider that can list only its home namespace sweep exactly the
/// resources it can prove are orphans, and nothing a tenant namespace owns.
pub const TAG_OWNER_NAMESPACE: &str = "banlieue-owner-namespace";
/// On a published image version: the SHA-256 (lowercase hex) of the raw
/// disk it was published from (ADR-0162 Decision 6). A provider boots only
/// a version whose recorded digest matches the `VMImage` it was asked for.
pub const TAG_SOURCE_SHA256: &str = "banlieue-source-sha256";

/// Azure's limit on a tag name.
pub const MAX_TAG_NAME_LEN: usize = 512;
/// Azure's limit on a tag value.
pub const MAX_TAG_VALUE_LEN: usize = 256;
/// Azure's limit on tags per resource.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// Characters Azure refuses in a tag name.
const RESERVED_TAG_NAME_CHARS: [char; 7] = ['<', '>', '%', '&', '\\', '?', '/'];

/// The three ownership tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipTags {
    /// The owning object's UID.
    pub owner_uid: String,
    /// The owning object's kind.
    pub owner_kind: String,
    /// `<namespace>.<name>` of the `Provider`.
    pub provider: String,
    /// The owning object's namespace, when it has one.
    pub owner_namespace: Option<String>,
}

impl OwnershipTags {
    /// Tags for an object of `owner_kind` with `owner_uid`, served by the
    /// `Provider` `provider_namespace/provider_name`.
    #[must_use]
    pub fn new(
        owner_uid: &str,
        owner_kind: &str,
        provider_namespace: &str,
        provider_name: &str,
    ) -> Self {
        Self {
            owner_uid: owner_uid.to_string(),
            owner_kind: owner_kind.to_string(),
            provider: format!("{provider_namespace}.{provider_name}"),
            owner_namespace: None,
        }
    }

    /// The same tags, also naming the owning object's namespace
    /// ([`TAG_OWNER_NAMESPACE`]).
    #[must_use]
    pub fn with_owner_namespace(mut self, namespace: &str) -> Self {
        self.owner_namespace = Some(namespace.to_string());
        self
    }

    /// The tags as Azure's `tags` object.
    #[must_use]
    pub fn to_map(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::from([
            (TAG_OWNER_UID.to_string(), self.owner_uid.clone()),
            (TAG_OWNER_KIND.to_string(), self.owner_kind.clone()),
            (TAG_PROVIDER.to_string(), self.provider.clone()),
        ]);
        if let Some(namespace) = &self.owner_namespace {
            map.insert(TAG_OWNER_NAMESPACE.to_string(), namespace.clone());
        }
        map
    }

    /// Reads the ownership tags back from a resource's tags; `None` unless
    /// all three are present.
    #[must_use]
    pub fn from_tags(tags: &BTreeMap<String, String>) -> Option<Self> {
        Some(Self {
            owner_uid: tags.get(TAG_OWNER_UID)?.clone(),
            owner_kind: tags.get(TAG_OWNER_KIND)?.clone(),
            provider: tags.get(TAG_PROVIDER)?.clone(),
            owner_namespace: tags.get(TAG_OWNER_NAMESPACE).cloned(),
        })
    }

    /// Whether a resource with `tags` belongs to this owner: its UID tag
    /// equals ours exactly. Name, location and the other tags are never
    /// evidence of ownership.
    #[must_use]
    pub fn owns(&self, tags: &BTreeMap<String, String>) -> bool {
        tags.get(TAG_OWNER_UID) == Some(&self.owner_uid)
    }

    /// The ownership tags plus `extra` (for example tags an organisation's
    /// Azure Policy requires), validated and within Azure's count limit.
    ///
    /// # Errors
    /// [`Error::InvalidTag`] if `extra` names an ownership tag, if any name
    /// or value is invalid, or if the result exceeds
    /// [`MAX_TAGS_PER_RESOURCE`].
    pub fn merged_with(
        &self,
        extra: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>> {
        let mut merged = self.to_map();
        for (name, value) in extra {
            if merged.contains_key(name) {
                return Err(Error::InvalidTag(format!(
                    "{name} is an ownership tag and cannot be set from configuration"
                )));
            }
            merged.insert(name.clone(), value.clone());
        }
        validate_tags(&merged)?;
        Ok(merged)
    }
}

/// Validates a tag name against Azure's rules.
///
/// # Errors
/// [`Error::InvalidTag`] if the name is empty, too long, or contains a
/// reserved or control character.
pub fn validate_tag_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidTag(
            "a tag name must not be empty".to_string(),
        ));
    }
    if name.chars().count() > MAX_TAG_NAME_LEN {
        return Err(Error::InvalidTag(format!(
            "tag name longer than {MAX_TAG_NAME_LEN} characters"
        )));
    }
    if let Some(c) = name
        .chars()
        .find(|c| RESERVED_TAG_NAME_CHARS.contains(c) || c.is_control())
    {
        return Err(Error::InvalidTag(format!(
            "tag name {name:?} contains {c:?}, which Azure refuses"
        )));
    }
    Ok(())
}

/// Validates a tag value against Azure's length limit.
///
/// # Errors
/// [`Error::InvalidTag`] if the value is too long.
pub fn validate_tag_value(value: &str) -> Result<()> {
    if value.chars().count() > MAX_TAG_VALUE_LEN {
        return Err(Error::InvalidTag(format!(
            "tag value longer than {MAX_TAG_VALUE_LEN} characters"
        )));
    }
    Ok(())
}

/// Validates every tag and the count.
///
/// # Errors
/// [`Error::InvalidTag`] on the first invalid name or value, or too many tags.
pub fn validate_tags(tags: &BTreeMap<String, String>) -> Result<()> {
    if tags.len() > MAX_TAGS_PER_RESOURCE {
        return Err(Error::InvalidTag(format!(
            "{} tags; Azure allows {MAX_TAGS_PER_RESOURCE}",
            tags.len()
        )));
    }
    for (name, value) in tags {
        validate_tag_name(name)?;
        validate_tag_value(value)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tags_tests.rs"]
mod tags_tests;
