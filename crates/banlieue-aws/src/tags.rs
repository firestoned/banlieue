// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Ownership tags on every AWS resource banlieue creates (ADR-0158
//! Decision 4).
//!
//! The names are the same on both clouds (ADR-0158): `banlieue-owner-uid`,
//! `banlieue-owner-kind`, `banlieue-provider`, `banlieue-owner-namespace`,
//! and `banlieue-source-sha256` on a published image (ADR-0162 Decision 6).
//! banlieue deletes only a resource whose [`TAG_OWNER_UID`] equals the
//! object it acts for ([`owned_by`]), and the sweep (ADR-0158 Decision 5)
//! finds candidates with an EC2 `tag:<key>` filter ([`tag_filter_name`]).
//!
//! EC2's tag rules, from
//! <https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/Using_Tags.html>:
//! a key of 1 to 128 Unicode characters, a value of 0 to 256, at most 50
//! tags per resource, no key starting with `aws:` (reserved for AWS), and
//! only letters, digits, spaces and `+ - = . _ : / @`. Tags are set at
//! create time through `TagSpecification` ([`tag_specification_params`]),
//! so a resource is never visible untagged.

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
/// resources it can prove are orphans.
pub const TAG_OWNER_NAMESPACE: &str = "banlieue-owner-namespace";
/// On a published image: the SHA-256 (lowercase hex) of the raw disk it was
/// published from (ADR-0162 Decision 6).
pub const TAG_SOURCE_SHA256: &str = "banlieue-source-sha256";
/// EC2's display name tag, shown in the console's Name column.
pub const TAG_NAME: &str = "Name";

/// EC2's limit on a tag key, in Unicode characters.
pub const MAX_TAG_KEY_LEN: usize = 128;
/// EC2's limit on a tag value, in Unicode characters.
pub const MAX_TAG_VALUE_LEN: usize = 256;
/// EC2's limit on tags per resource.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;
/// The key prefix AWS reserves for itself, refused in any case.
pub const AWS_RESERVED_TAG_PREFIX: &str = "aws:";
/// The prefix of every tag banlieue sets itself. Configured extra tags may
/// not use it, in any case (ADR-0172 Decision 1, applied to EC2).
pub const RESERVED_TAG_PREFIX: &str = "banlieue-";
/// The most tags banlieue sets itself on one resource: the four ownership
/// tags, [`TAG_SOURCE_SHA256`] and [`TAG_NAME`].
pub const BANLIEUE_TAG_COUNT: usize = 6;
/// The most extra tags a `Provider` may configure, so every resource fits
/// [`MAX_TAGS_PER_RESOURCE`] with all of banlieue's own.
pub const MAX_EXTRA_TAGS: usize = MAX_TAGS_PER_RESOURCE - BANLIEUE_TAG_COUNT;

/// EC2's prefix for a tag filter name (`Filter.N.Name=tag:<key>`).
const TAG_FILTER_PREFIX: &str = "tag:";
/// The punctuation EC2 allows in a tag key or value, besides letters and
/// digits.
const ALLOWED_TAG_PUNCTUATION: [char; 9] = [' ', '+', '-', '=', '.', '_', ':', '/', '@'];
/// The Query parameter prefix of a tag specification.
const TAG_SPECIFICATION_PARAM: &str = "TagSpecification";
/// Each tag encodes as a key pair and a value pair.
const QUERY_PAIRS_PER_TAG: usize = 2;

/// The ownership tags (ADR-0158 Decision 4).
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

    /// The ownership tags as a key to value map.
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
    /// the UID, kind and provider tags are all present.
    #[must_use]
    pub fn from_tags(tags: &BTreeMap<String, String>) -> Option<Self> {
        Some(Self {
            owner_uid: tags.get(TAG_OWNER_UID)?.clone(),
            owner_kind: tags.get(TAG_OWNER_KIND)?.clone(),
            provider: tags.get(TAG_PROVIDER)?.clone(),
            owner_namespace: tags.get(TAG_OWNER_NAMESPACE).cloned(),
        })
    }

    /// Whether a resource with `tags` belongs to this owner ([`owned_by`]).
    #[must_use]
    pub fn owns(&self, tags: &BTreeMap<String, String>) -> bool {
        owned_by(tags, &self.owner_uid)
    }

    /// Every tag for a resource banlieue creates: the ownership tags, the
    /// [`TAG_NAME`] tag when `name` is given, [`TAG_SOURCE_SHA256`] when
    /// `source_sha256` is given (a published image), and the `Provider`'s
    /// configured `extra` tags. banlieue's own tags always win: `extra` may
    /// not use [`RESERVED_TAG_PREFIX`] nor set [`TAG_NAME`].
    ///
    /// # Errors
    /// [`Error::Invalid`] when [`validate_extra_tags`] refuses `extra`, or
    /// when the result breaks an EC2 rule ([`validate_tags`]).
    pub fn resource_tags(
        &self,
        name: Option<&str>,
        source_sha256: Option<&str>,
        extra: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>> {
        validate_extra_tags(extra)?;
        let mut tags = extra.clone();
        tags.extend(self.to_map());
        if let Some(name) = name {
            tags.insert(TAG_NAME.to_string(), name.to_string());
        }
        if let Some(digest) = source_sha256 {
            tags.insert(TAG_SOURCE_SHA256.to_string(), digest.to_string());
        }
        validate_tags(&tags)?;
        Ok(tags)
    }
}

/// Whether a resource with `tags` belongs to the object with `uid`: its
/// [`TAG_OWNER_UID`] equals `uid` exactly. Name, other tags and location are
/// never evidence of ownership, and an empty `uid` owns nothing.
#[must_use]
pub fn owned_by(tags: &BTreeMap<String, String>, uid: &str) -> bool {
    if uid.is_empty() {
        return false;
    }
    tags.get(TAG_OWNER_UID).map(String::as_str) == Some(uid)
}

/// EC2's filter name for a tag key: `tag:<key>`, for `Filter.N.Name`.
#[must_use]
pub fn tag_filter_name(key: &str) -> String {
    format!("{TAG_FILTER_PREFIX}{key}")
}

/// Whether EC2 allows `c` in a tag key or value: a Unicode letter or digit,
/// or one of `space + - = . _ : / @`.
fn is_allowed_tag_char(c: char) -> bool {
    c.is_alphanumeric() || ALLOWED_TAG_PUNCTUATION.contains(&c)
}

/// Refuses the first character EC2 does not allow in `text`.
fn check_tag_chars(what: &str, text: &str) -> Result<()> {
    let Some(c) = text.chars().find(|c| !is_allowed_tag_char(*c)) else {
        return Ok(());
    };
    Err(Error::Invalid(format!(
        "tag {what} {text:?} contains {c:?}; EC2 allows letters, digits, spaces and + - = . _ : / @"
    )))
}

/// Whether `text` starts with the ASCII `prefix`, ignoring ASCII case.
fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Validates a tag key against EC2's rules.
///
/// # Errors
/// [`Error::Invalid`] if the key is empty, longer than [`MAX_TAG_KEY_LEN`]
/// characters, starts with [`AWS_RESERVED_TAG_PREFIX`] in any case, or holds
/// a character EC2 does not allow.
pub fn validate_tag_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(Error::Invalid("a tag key must not be empty".to_string()));
    }
    if key.chars().count() > MAX_TAG_KEY_LEN {
        return Err(Error::Invalid(format!(
            "tag key longer than {MAX_TAG_KEY_LEN} characters"
        )));
    }
    if starts_with_ignore_ascii_case(key, AWS_RESERVED_TAG_PREFIX) {
        return Err(Error::Invalid(format!(
            "tag key {key:?} starts with {AWS_RESERVED_TAG_PREFIX}, which AWS reserves"
        )));
    }
    check_tag_chars("key", key)
}

/// Validates a tag value against EC2's rules.
///
/// # Errors
/// [`Error::Invalid`] if the value is longer than [`MAX_TAG_VALUE_LEN`]
/// characters or holds a character EC2 does not allow.
pub fn validate_tag_value(value: &str) -> Result<()> {
    if value.chars().count() > MAX_TAG_VALUE_LEN {
        return Err(Error::Invalid(format!(
            "tag value longer than {MAX_TAG_VALUE_LEN} characters"
        )));
    }
    check_tag_chars("value", value)
}

/// Validates a resource's whole tag set: every key and value, and at most
/// [`MAX_TAGS_PER_RESOURCE`].
///
/// # Errors
/// [`Error::Invalid`] on too many tags or the first invalid key or value.
pub fn validate_tags(tags: &BTreeMap<String, String>) -> Result<()> {
    if tags.len() > MAX_TAGS_PER_RESOURCE {
        return Err(Error::Invalid(format!(
            "{} tags; EC2 allows {MAX_TAGS_PER_RESOURCE} per resource",
            tags.len()
        )));
    }
    for (key, value) in tags {
        validate_tag_key(key)?;
        validate_tag_value(value)?;
    }
    Ok(())
}

/// Validates tags configured on a `Provider` (ADR-0172 Decision 1, applied
/// to EC2): each key and value as EC2 requires, no key in
/// [`RESERVED_TAG_PREFIX`] (in any case, so a configured tag can never pose
/// as an ownership tag), not [`TAG_NAME`] (banlieue sets it), and at most
/// [`MAX_EXTRA_TAGS`] so every resource fits beside banlieue's own.
///
/// # Errors
/// [`Error::Invalid`] naming the first rule broken.
pub fn validate_extra_tags(extra: &BTreeMap<String, String>) -> Result<()> {
    if extra.len() > MAX_EXTRA_TAGS {
        return Err(Error::Invalid(format!(
            "{} extra tags; at most {MAX_EXTRA_TAGS} fit beside banlieue's own",
            extra.len()
        )));
    }
    for (key, value) in extra {
        validate_tag_key(key)?;
        validate_tag_value(value)?;
        if starts_with_ignore_ascii_case(key, RESERVED_TAG_PREFIX) {
            return Err(Error::Invalid(format!(
                "{key} is in banlieue's own {RESERVED_TAG_PREFIX} tag namespace and cannot be set from configuration"
            )));
        }
        if key == TAG_NAME {
            return Err(Error::Invalid(format!(
                "{TAG_NAME} is set by banlieue and cannot be set from configuration"
            )));
        }
    }
    Ok(())
}

/// The resource types banlieue tags at create time, as EC2's
/// `TagSpecification.N.ResourceType` names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagResourceType {
    /// `instance`
    Instance,
    /// `volume`
    Volume,
    /// `network-interface`
    NetworkInterface,
    /// `snapshot`
    Snapshot,
    /// `image`
    Image,
    /// `import-snapshot-task`
    ImportSnapshotTask,
}

impl TagResourceType {
    /// EC2's string for the type.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Volume => "volume",
            Self::NetworkInterface => "network-interface",
            Self::Snapshot => "snapshot",
            Self::Image => "image",
            Self::ImportSnapshotTask => "import-snapshot-task",
        }
    }
}

impl std::fmt::Display for TagResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The EC2 Query pairs of one tag specification:
/// `TagSpecification.<index>.ResourceType`, then
/// `TagSpecification.<index>.Tag.<n>.Key` and `.Value` for each tag, `n`
/// from 1 in key order, so the same tags always encode the same way.
/// `index` is the caller's 1-based position among the request's
/// specifications. The tags are not validated here; build them with
/// [`OwnershipTags::resource_tags`].
#[must_use]
pub fn tag_specification_params(
    index: usize,
    resource_type: TagResourceType,
    tags: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let prefix = format!("{TAG_SPECIFICATION_PARAM}.{index}");
    let mut params = Vec::with_capacity(1 + tags.len() * QUERY_PAIRS_PER_TAG);
    params.push((
        format!("{prefix}.ResourceType"),
        resource_type.as_str().to_string(),
    ));
    for (n, (key, value)) in (1..).zip(tags) {
        params.push((format!("{prefix}.Tag.{n}.Key"), key.clone()));
        params.push((format!("{prefix}.Tag.{n}.Value"), value.clone()));
    }
    params
}

#[cfg(test)]
#[path = "tags_tests.rs"]
mod tags_tests;
