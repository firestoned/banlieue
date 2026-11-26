// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `customData` out of Azure's `ovf-env.xml`.
//!
//! Azure delivers a Linux VM's `osProfile.customData` base64-encoded in the
//! `<CustomData>` element of `ovf-env.xml`, on a UDF provisioning disk
//! (ADR-0163 Decision 3). The same document can carry the admin password, so
//! nothing here logs it and only `CustomData` is ever extracted.
//!
//! The parser is deliberately small and strict, not a general XML parser: it
//! walks the tags, requires the root element to be `Environment`, refuses a
//! DTD or CDATA section (no entity expansion is ever attempted), and accepts
//! exactly zero or one `CustomData` element whose content is plain text.
//! Anything else is an error and nothing is written.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use thiserror::Error;

/// Largest `ovf-env.xml` read. 64 KiB of customData is about 87 KiB of
/// base64; the rest of the document is a few KiB.
pub const OVF_ENV_MAX_BYTES: usize = 256 * 1024;

/// Largest decoded customData accepted: Azure's own limit (64 KiB).
pub const CUSTOM_DATA_MAX_BYTES: usize = 64 * 1024;

/// The header Kairos's collector requires; a file without it is silently
/// skipped, so anything else is refused here instead.
pub const CLOUD_CONFIG_HEADER: &str = "#cloud-config";

/// Local name of the document's root element.
const ROOT_ELEMENT: &str = "Environment";

/// Local name of the element carrying customData.
const CUSTOM_DATA_ELEMENT: &str = "CustomData";

/// Base64 groups: every 3 bytes become 4 characters.
const BASE64_GROUP_IN: usize = 3;
const BASE64_GROUP_OUT: usize = 4;

/// Why `ovf-env.xml` or its customData was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OvfError {
    /// The document is larger than [`OVF_ENV_MAX_BYTES`].
    #[error("ovf-env.xml is larger than {OVF_ENV_MAX_BYTES} bytes")]
    DocumentTooLarge,
    /// The document is not UTF-8.
    #[error("ovf-env.xml is not UTF-8")]
    NotUtf8,
    /// The document is not well formed enough for this parser.
    #[error("ovf-env.xml is malformed: {0}")]
    Malformed(&'static str),
    /// A DTD, CDATA section or entity reference where none is expected.
    #[error("ovf-env.xml uses {0}, which is refused")]
    Refused(&'static str),
    /// More than one `CustomData` element.
    #[error("ovf-env.xml has more than one CustomData element")]
    DuplicateCustomData,
    /// customData is not valid base64.
    #[error("CustomData is not valid base64")]
    NotBase64,
    /// Decoded customData is larger than [`CUSTOM_DATA_MAX_BYTES`].
    #[error("customData is larger than {CUSTOM_DATA_MAX_BYTES} bytes")]
    CustomDataTooLarge,
    /// customData is not a Kairos cloud-config.
    #[error("customData is not a #cloud-config document ({0})")]
    NotCloudConfig(&'static str),
}

/// One tag of the document, as the walker sees it.
#[derive(Debug, PartialEq, Eq)]
enum Tag<'a> {
    Start { local: &'a str, self_closing: bool },
    End { local: &'a str },
}

/// The part of a qualified name after any namespace prefix.
fn local_name(qname: &str) -> &str {
    qname.rsplit_once(':').map_or(qname, |(_, l)| l)
}

/// Parse the tag starting at `s` (just after `<`); returns the tag and the
/// byte length consumed up to and including `>`. Comments and processing
/// instructions return `None` with their length.
fn parse_tag(s: &str) -> Result<(Option<Tag<'_>>, usize), OvfError> {
    if let Some(rest) = s.strip_prefix("!--") {
        let end = rest
            .find("-->")
            .ok_or(OvfError::Malformed("unterminated comment"))?;
        return Ok((None, "!--".len() + end + "-->".len()));
    }
    if s.starts_with("![CDATA[") {
        return Err(OvfError::Refused("a CDATA section"));
    }
    if s.starts_with('!') {
        return Err(OvfError::Refused("a DTD"));
    }
    if let Some(rest) = s.strip_prefix('?') {
        let end = rest
            .find("?>")
            .ok_or(OvfError::Malformed("unterminated processing instruction"))?;
        return Ok((None, "?".len() + end + "?>".len()));
    }
    let close = s.find('>').ok_or(OvfError::Malformed("unterminated tag"))?;
    let body = &s[..close];
    if let Some(name) = body.strip_prefix('/') {
        let name = name.trim_end();
        if name.is_empty() {
            return Err(OvfError::Malformed("empty end tag"));
        }
        return Ok((
            Some(Tag::End {
                local: local_name(name),
            }),
            close + 1,
        ));
    }
    let self_closing = body.ends_with('/');
    let body = body.strip_suffix('/').unwrap_or(body);
    let name_end = body
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(body.len());
    let name = &body[..name_end];
    if name.is_empty() {
        return Err(OvfError::Malformed("empty start tag"));
    }
    Ok((
        Some(Tag::Start {
            local: local_name(name),
            self_closing,
        }),
        close + 1,
    ))
}

/// The raw (still base64) text of the single `CustomData` element, or `None`
/// when the document has none or it is empty.
///
/// # Errors
/// Any [`OvfError`] about the document itself: too large, not UTF-8,
/// malformed, a DTD or CDATA, a root that is not `Environment`, or a second
/// `CustomData`.
pub fn custom_data_text(document: &[u8]) -> Result<Option<String>, OvfError> {
    if document.len() > OVF_ENV_MAX_BYTES {
        return Err(OvfError::DocumentTooLarge);
    }
    let text = std::str::from_utf8(document).map_err(|_| OvfError::NotUtf8)?;

    let mut pos = 0;
    let mut depth: usize = 0;
    let mut saw_root = false;
    let mut found: Option<String> = None;
    // Byte offset just after a `<CustomData>` start tag still open.
    let mut open_custom_data: Option<usize> = None;

    while let Some(lt) = text[pos..].find('<') {
        let tag_start = pos + lt;
        let (tag, len) = parse_tag(&text[tag_start + 1..])?;
        let tag_end = tag_start + 1 + len;
        match tag {
            None => {}
            Some(Tag::Start {
                local,
                self_closing,
            }) => {
                if open_custom_data.is_some() {
                    return Err(OvfError::Malformed("an element inside CustomData"));
                }
                if depth == 0 {
                    if saw_root || local != ROOT_ELEMENT {
                        return Err(OvfError::Malformed("the root element is not Environment"));
                    }
                    saw_root = true;
                }
                if local == CUSTOM_DATA_ELEMENT {
                    if found.is_some() {
                        return Err(OvfError::DuplicateCustomData);
                    }
                    if self_closing {
                        found = Some(String::new());
                    } else {
                        open_custom_data = Some(tag_end);
                    }
                }
                if !self_closing {
                    depth += 1;
                }
            }
            Some(Tag::End { local }) => {
                if depth == 0 {
                    return Err(OvfError::Malformed("an end tag with no start"));
                }
                depth -= 1;
                if let Some(start) = open_custom_data.take() {
                    if local != CUSTOM_DATA_ELEMENT {
                        return Err(OvfError::Malformed("CustomData is not closed"));
                    }
                    let content = &text[start..tag_start];
                    if content.contains('&') {
                        return Err(OvfError::Refused("an entity reference in CustomData"));
                    }
                    found = Some(content.to_string());
                }
            }
        }
        pos = tag_end;
    }
    if open_custom_data.is_some() || depth != 0 {
        return Err(OvfError::Malformed("the document ends inside an element"));
    }
    if !saw_root {
        return Err(OvfError::Malformed("no root element"));
    }
    Ok(found.filter(|s| !s.trim().is_empty()))
}

/// Decode customData's base64 text, ignoring whitespace, within
/// [`CUSTOM_DATA_MAX_BYTES`].
///
/// # Errors
/// [`OvfError::CustomDataTooLarge`] (checked before decoding) or
/// [`OvfError::NotBase64`].
pub fn decode_custom_data(text: &str) -> Result<Vec<u8>, OvfError> {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let max_encoded = CUSTOM_DATA_MAX_BYTES.div_ceil(BASE64_GROUP_IN) * BASE64_GROUP_OUT;
    if compact.len() > max_encoded {
        return Err(OvfError::CustomDataTooLarge);
    }
    let bytes = STANDARD
        .decode(compact.as_bytes())
        .map_err(|_| OvfError::NotBase64)?;
    if bytes.len() > CUSTOM_DATA_MAX_BYTES {
        return Err(OvfError::CustomDataTooLarge);
    }
    Ok(bytes)
}

/// Accept only a Kairos cloud-config: UTF-8, no NUL, first line exactly
/// [`CLOUD_CONFIG_HEADER`] (trailing whitespace allowed). A shell script or
/// any other customData would be skipped by Kairos anyway; refusing it makes
/// that visible in the boot log instead of silent.
///
/// # Errors
/// [`OvfError::NotCloudConfig`] naming what is wrong.
pub fn validate_cloud_config(bytes: &[u8]) -> Result<(), OvfError> {
    if bytes.contains(&0) {
        return Err(OvfError::NotCloudConfig("it contains a NUL byte"));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| OvfError::NotCloudConfig("it is not UTF-8"))?;
    let first = text.lines().next().unwrap_or_default().trim_end();
    if first != CLOUD_CONFIG_HEADER {
        return Err(OvfError::NotCloudConfig(
            "its first line is not #cloud-config",
        ));
    }
    Ok(())
}

/// The cloud-config to write from an `ovf-env.xml`, or `None` when the VM
/// was created without customData.
///
/// # Errors
/// Any [`OvfError`]: the document, the encoding or the content refused.
pub fn cloud_config_from_ovf_env(document: &[u8]) -> Result<Option<Vec<u8>>, OvfError> {
    let Some(text) = custom_data_text(document)? else {
        return Ok(None);
    };
    let bytes = decode_custom_data(&text)?;
    validate_cloud_config(&bytes)?;
    Ok(Some(bytes))
}

#[cfg(test)]
#[path = "ovf_tests.rs"]
mod ovf_tests;
