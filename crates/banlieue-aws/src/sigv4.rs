// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! AWS Signature Version 4 (ADR-0174 Decision 2).
//!
//! Header signing only, on aws-lc-rs's HMAC-SHA-256 and SHA-256: the
//! process's one crypto library (ADR-0170). Every step is a public function
//! so the tests can hold each against AWS's published vectors (the
//! aws-c-auth SigV4 test suite and the S3 API Reference's worked examples,
//! under `tests/fixtures/sigv4/`):
//!
//! 1. the canonical request, [`canonical_request`] (method,
//!    [`canonical_uri`], [`canonical_query`], canonical headers, signed
//!    headers, [`payload_hash`]);
//! 2. the string to sign, [`string_to_sign`] (algorithm, [`amz_date`],
//!    [`credential_scope`], and the canonical request's SHA-256);
//! 3. the signing key, [`signing_key`] (the HMAC chain from the secret over
//!    date, region, service and `aws4_request`);
//! 4. the signature, [`signature`] (the HMAC of the string to sign under
//!    that key).
//!
//! [`sign`] runs all four and returns the headers to add to the request.
//!
//! The secret and the session token never appear in a `Debug` rendering:
//! [`Credentials`] and [`Signed`] redact them.

use std::collections::BTreeMap;
use std::fmt;

use aws_lc_rs::{digest, hmac};
use jiff::Timestamp;

use crate::error::{Error, Result};

/// The SigV4 algorithm name, first line of the string to sign.
pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";
/// The payload hash S3 accepts in place of a body's SHA-256 (part uploads).
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
/// The `Authorization` header [`sign`] returns.
pub const AUTHORIZATION: &str = "Authorization";
/// The request-time header [`sign`] returns, `YYYYMMDDTHHMMSSZ`.
pub const X_AMZ_DATE: &str = "X-Amz-Date";
/// The payload-hash header [`sign`] returns when
/// [`SigningOptions::content_sha256_header`] is set (S3 requires it).
pub const X_AMZ_CONTENT_SHA256: &str = "X-Amz-Content-Sha256";
/// The session-token header [`sign`] returns for temporary credentials.
pub const X_AMZ_SECURITY_TOKEN: &str = "X-Amz-Security-Token";
/// Length in bytes of a SHA-256 output, and so of a derived signing key.
pub const SIGNING_KEY_LEN: usize = 32;

/// Canonical (lowercase) names of the headers this module sets itself; a
/// caller passing one is refused.
const HOST: &str = "host";
const AMZ_DATE_LOWER: &str = "x-amz-date";
const AMZ_CONTENT_SHA256_LOWER: &str = "x-amz-content-sha256";
const AMZ_SECURITY_TOKEN_LOWER: &str = "x-amz-security-token";
const AUTHORIZATION_LOWER: &str = "authorization";
const RESERVED_HEADERS: [&str; 5] = [
    HOST,
    AMZ_DATE_LOWER,
    AMZ_CONTENT_SHA256_LOWER,
    AMZ_SECURITY_TOKEN_LOWER,
    AUTHORIZATION_LOWER,
];

/// `x-amz-date`: ISO 8601 basic format, UTC.
const AMZ_DATE_FORMAT: &str = "%Y%m%dT%H%M%SZ";
/// The date in the credential scope.
const SCOPE_DATE_FORMAT: &str = "%Y%m%d";
/// The credential scope's last element.
const SCOPE_TERMINATOR: &str = "aws4_request";
/// Prefixed to the secret to key the first HMAC.
const SECRET_PREFIX: &str = "AWS4";
/// Shown in place of a secret by `Debug`.
const REDACTED: &str = "<redacted>";
/// The path of an empty or fully collapsed URI.
const ROOT_PATH: &str = "/";

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";
const HEX_DIGITS_PER_BYTE: usize = 2;
const NIBBLE_BITS: u8 = 4;
const LOW_NIBBLE: u8 = 0x0f;
/// Characters a header name may hold besides ASCII letters and digits
/// (RFC 9110 `tchar`).
const HEADER_NAME_SYMBOLS: &str = "!#$%&'*+-.^_`|~";
/// Characters a host may hold besides ASCII letters, digits and `-`
/// (a port, an IPv6 literal).
const HOST_SYMBOLS: [char; 4] = ['.', ':', '[', ']'];

/// An AWS credential: an access key pair and, for a session from STS, its
/// token.
///
/// `Debug` shows the access key ID only; the secret and the token are
/// redacted.
#[derive(Clone)]
pub struct Credentials {
    /// The access key ID (`AKIA...`, or `ASIA...` for a session).
    pub access_key_id: String,
    /// The secret access key. Never logged.
    pub secret_access_key: String,
    /// The session token of temporary credentials. Never logged.
    pub session_token: Option<String>,
}

impl Credentials {
    /// Credentials from their parts.
    #[must_use]
    pub fn new(
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
        session_token: Option<String>,
    ) -> Self {
        Self {
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token,
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &REDACTED)
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| REDACTED),
            )
            .finish()
    }
}

/// What the payload hash covers.
#[derive(Clone, Copy)]
pub enum Payload<'a> {
    /// The body's bytes, hashed with SHA-256 (empty for no body).
    Bytes(&'a [u8]),
    /// [`UNSIGNED_PAYLOAD`], verbatim: S3 only, for part uploads.
    Unsigned,
}

impl fmt::Debug for Payload<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::Unsigned => f.write_str("Unsigned"),
        }
    }
}

/// How a request is canonicalized: S3 differs from every other service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigningOptions {
    /// Remove `.`, `..` and empty segments from the path before encoding
    /// it. Every service but S3.
    pub normalize_path: bool,
    /// URI-encode the already-encoded path a second time. Every service but
    /// S3.
    pub double_encode_path: bool,
    /// Sign and send `x-amz-content-sha256` with the payload hash. S3
    /// requires it.
    pub content_sha256_header: bool,
}

impl SigningOptions {
    /// EC2, STS and every service but S3: normalized, double-encoded path,
    /// no `x-amz-content-sha256`.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            normalize_path: true,
            double_encode_path: true,
            content_sha256_header: false,
        }
    }

    /// S3: the object key encoded once and never normalized, and
    /// `x-amz-content-sha256` signed.
    #[must_use]
    pub const fn s3() -> Self {
        Self {
            normalize_path: false,
            double_encode_path: false,
            content_sha256_header: true,
        }
    }
}

impl Default for SigningOptions {
    fn default() -> Self {
        Self::standard()
    }
}

/// The parts of an HTTP request SigV4 signs.
///
/// `path` and `query` are unencoded: this module encodes them. Every header
/// in `headers` is signed; leave out hop-by-hop headers (`connection`,
/// `user-agent`, `transfer-encoding`), which proxies change. `host`,
/// `x-amz-date`, `x-amz-content-sha256`, `x-amz-security-token` and
/// `authorization` are this module's to set and are refused here.
#[derive(Clone, Copy, Debug)]
pub struct SigningRequest<'a> {
    /// HTTP method, uppercase (`GET`, `POST`, `PUT`).
    pub method: &'a str,
    /// The `host` header: the endpoint's host name (with `:port` if not
    /// the scheme's default).
    pub host: &'a str,
    /// The URI path, unencoded (`/` for EC2 and STS).
    pub path: &'a str,
    /// Query parameters, unencoded, in any order; repeated keys allowed.
    pub query: &'a [(String, String)],
    /// Further headers to sign and send.
    pub headers: &'a [(String, String)],
    /// The body, or S3's unsigned-payload marker.
    pub payload: Payload<'a>,
    /// AWS region (`us-east-1`).
    pub region: &'a str,
    /// Signing name of the service (`ec2`, `sts`, `s3`).
    pub service: &'a str,
    /// The request time: `x-amz-date` and the scope's date.
    pub timestamp: Timestamp,
}

/// The result of [`sign`]: every intermediate, and the headers to add.
///
/// `Debug` shows the signature and the header names only: the canonical
/// request and the headers carry the session token.
#[derive(Clone)]
pub struct Signed {
    /// The canonical request (step 1). Holds the session token, if any.
    pub canonical_request: String,
    /// The string to sign (step 2).
    pub string_to_sign: String,
    /// The signature, lowercase hex (step 4).
    pub signature: String,
    /// Headers to add to the request as sent, besides `host` and the
    /// caller's: [`X_AMZ_DATE`], [`X_AMZ_CONTENT_SHA256`] when asked for,
    /// [`X_AMZ_SECURITY_TOKEN`] for a session, and [`AUTHORIZATION`].
    pub headers: Vec<(String, String)>,
}

impl fmt::Debug for Signed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("Signed")
            .field("signature", &self.signature)
            .field("headers", &header_names)
            .finish_non_exhaustive()
    }
}

/// A canonical request with the signed-header list it ends with.
struct Canonical {
    text: String,
    signed_headers: String,
    payload_hash: String,
}

/// Signs `request`: canonical request, string to sign, signing key,
/// signature, and the headers to send.
///
/// # Errors
/// [`Error::Invalid`] if a header is one this module sets, a header name or
/// value could split the request, or the method, host, region or service is
/// empty or malformed.
pub fn sign(
    request: &SigningRequest<'_>,
    credentials: &Credentials,
    options: &SigningOptions,
) -> Result<Signed> {
    let canonical = build_canonical(request, credentials, options)?;
    let scope = credential_scope(request.timestamp, request.region, request.service);
    let string_to_sign = string_to_sign(request.timestamp, &scope, &canonical.text);
    let key = signing_key(
        &credentials.secret_access_key,
        request.timestamp,
        request.region,
        request.service,
    );
    let signature = signature(&key, &string_to_sign);
    let authorization = format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
        credentials.access_key_id, canonical.signed_headers
    );

    let mut headers = vec![(X_AMZ_DATE.to_string(), amz_date(request.timestamp))];
    if options.content_sha256_header {
        headers.push((X_AMZ_CONTENT_SHA256.to_string(), canonical.payload_hash));
    }
    if let Some(token) = &credentials.session_token {
        headers.push((X_AMZ_SECURITY_TOKEN.to_string(), token.clone()));
    }
    headers.push((AUTHORIZATION.to_string(), authorization));

    Ok(Signed {
        canonical_request: canonical.text,
        string_to_sign,
        signature,
        headers,
    })
}

/// The canonical request (SigV4 step 1).
///
/// # Errors
/// As [`sign`].
pub fn canonical_request(
    request: &SigningRequest<'_>,
    credentials: &Credentials,
    options: &SigningOptions,
) -> Result<String> {
    build_canonical(request, credentials, options).map(|c| c.text)
}

/// The string to sign (SigV4 step 2) for a canonical request under `scope`.
#[must_use]
pub fn string_to_sign(timestamp: Timestamp, scope: &str, canonical_request: &str) -> String {
    format!(
        "{ALGORITHM}\n{}\n{scope}\n{}",
        amz_date(timestamp),
        sha256_hex(canonical_request.as_bytes())
    )
}

/// The derived signing key (SigV4 step 3):
/// `HMAC(HMAC(HMAC(HMAC("AWS4" + secret, date), region), service), "aws4_request")`.
#[must_use]
pub fn signing_key(
    secret_access_key: &str,
    timestamp: Timestamp,
    region: &str,
    service: &str,
) -> [u8; SIGNING_KEY_LEN] {
    let secret = format!("{SECRET_PREFIX}{secret_access_key}");
    let date = timestamp.strftime(SCOPE_DATE_FORMAT).to_string();
    let date_key = hmac_sha256(secret.as_bytes(), date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    hmac_sha256(&service_key, SCOPE_TERMINATOR.as_bytes())
}

/// The signature (SigV4 step 4): `hex(HMAC(signing_key, string_to_sign))`.
#[must_use]
pub fn signature(signing_key: &[u8], string_to_sign: &str) -> String {
    hex_lower(&hmac_sha256(signing_key, string_to_sign.as_bytes()))
}

/// `x-amz-date`'s value: `YYYYMMDDTHHMMSSZ`, UTC.
#[must_use]
pub fn amz_date(timestamp: Timestamp) -> String {
    timestamp.strftime(AMZ_DATE_FORMAT).to_string()
}

/// The credential scope: `YYYYMMDD/region/service/aws4_request`.
#[must_use]
pub fn credential_scope(timestamp: Timestamp, region: &str, service: &str) -> String {
    format!(
        "{}/{region}/{service}/{SCOPE_TERMINATOR}",
        timestamp.strftime(SCOPE_DATE_FORMAT)
    )
}

/// The payload hash: lowercase hex SHA-256 of the body, or
/// [`UNSIGNED_PAYLOAD`] verbatim.
#[must_use]
pub fn payload_hash(payload: Payload<'_>) -> String {
    match payload {
        Payload::Bytes(bytes) => sha256_hex(bytes),
        Payload::Unsigned => UNSIGNED_PAYLOAD.to_string(),
    }
}

/// Lowercase hex SHA-256 of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    hex_lower(digest::digest(&digest::SHA256, data).as_ref())
}

/// AWS's `UriEncode`: every byte of the UTF-8 encoding except the
/// unreserved `A-Z a-z 0-9 - _ . ~` as `%XX`, uppercase hex; a space is
/// `%20`, never `+`. `/` is encoded only when `encode_slash` is set (it is
/// kept in a path, encoded in a query or form value).
#[must_use]
pub fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        if is_unreserved(byte) || (byte == b'/' && !encode_slash) {
            out.push(char::from(byte));
            continue;
        }
        out.push('%');
        out.push(char::from(HEX_UPPER[usize::from(byte >> NIBBLE_BITS)]));
        out.push(char::from(HEX_UPPER[usize::from(byte & LOW_NIBBLE)]));
    }
    out
}

/// The path as it goes on the wire: normalized when `options` says so, and
/// encoded once. An empty path is `/`.
#[must_use]
pub fn wire_path(path: &str, options: &SigningOptions) -> String {
    if path.is_empty() {
        return ROOT_PATH.to_string();
    }
    if options.normalize_path {
        return uri_encode(&normalize_path(path), false);
    }
    uri_encode(path, false)
}

/// The canonical URI: the [`wire_path`], encoded a second time unless
/// `options` are S3's.
#[must_use]
pub fn canonical_uri(path: &str, options: &SigningOptions) -> String {
    let once = wire_path(path, options);
    if !options.double_encode_path {
        return once;
    }
    uri_encode(&once, false)
}

/// The canonical query string: each key and value [`uri_encode`]d (`/`
/// included), sorted by encoded key then encoded value, joined with `&`.
/// Empty for no parameters.
#[must_use]
pub fn canonical_query(query: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * HEX_DIGITS_PER_BYTE);
    for &byte in bytes {
        out.push(char::from(HEX_LOWER[usize::from(byte >> NIBBLE_BITS)]));
        out.push(char::from(HEX_LOWER[usize::from(byte & LOW_NIBBLE)]));
    }
    out
}

/// HMAC-SHA-256 of `data` under `key`.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; SIGNING_KEY_LEN] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data);
    let mut out = [0u8; SIGNING_KEY_LEN];
    out.copy_from_slice(tag.as_ref());
    out
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

/// Removes empty, `.` and `..` segments; keeps a trailing `/`.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return ROOT_PATH.to_string();
    }
    let trailing_slash = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    let mut out = format!("{ROOT_PATH}{}", segments.join("/"));
    if trailing_slash {
        out.push('/');
    }
    out
}

/// A header value as SigV4 signs it: trimmed, inner whitespace runs
/// collapsed to one space.
fn canonical_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || HEADER_NAME_SYMBOLS.contains(c))
}

/// True for a non-empty value of ASCII letters, digits, `-` and `extra`:
/// it cannot change the request line or the credential scope's structure.
fn is_plain_token(value: &str, extra: &[char]) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || extra.contains(&c))
}

fn validate(request: &SigningRequest<'_>) -> Result<()> {
    if request.method.is_empty() || !request.method.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(Error::Invalid(format!(
            "SigV4: method {:?} is not an uppercase HTTP method",
            request.method
        )));
    }
    if !is_plain_token(request.host, &HOST_SYMBOLS) {
        return Err(Error::Invalid(format!(
            "SigV4: host {:?} is not a host name",
            request.host
        )));
    }
    if !is_plain_token(request.region, &[]) || !is_plain_token(request.service, &[]) {
        return Err(Error::Invalid(format!(
            "SigV4: region {:?} or service {:?} is malformed",
            request.region, request.service
        )));
    }
    for (name, value) in request.headers {
        if !is_header_name(name) {
            return Err(Error::Invalid(format!(
                "SigV4: {name:?} is not a header name"
            )));
        }
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(Error::Invalid(format!(
                "SigV4: header {name:?} is set by the signer"
            )));
        }
        if value.contains(['\r', '\n', '\0']) {
            return Err(Error::Invalid(format!(
                "SigV4: header {name:?} has a line break or NUL in its value"
            )));
        }
    }
    Ok(())
}

fn build_canonical(
    request: &SigningRequest<'_>,
    credentials: &Credentials,
    options: &SigningOptions,
) -> Result<Canonical> {
    validate(request)?;
    let payload_hash = payload_hash(request.payload);

    // Lowercased name -> values in the order given; the map sorts names.
    let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    headers.insert(HOST.to_string(), vec![request.host.to_string()]);
    headers.insert(
        AMZ_DATE_LOWER.to_string(),
        vec![amz_date(request.timestamp)],
    );
    if options.content_sha256_header {
        headers.insert(
            AMZ_CONTENT_SHA256_LOWER.to_string(),
            vec![payload_hash.clone()],
        );
    }
    if let Some(token) = &credentials.session_token {
        headers.insert(AMZ_SECURITY_TOKEN_LOWER.to_string(), vec![token.clone()]);
    }
    for (name, value) in request.headers {
        headers
            .entry(name.to_ascii_lowercase())
            .or_default()
            .push(canonical_header_value(value));
    }

    let mut canonical_headers = String::new();
    for (name, values) in &headers {
        canonical_headers.push_str(name);
        canonical_headers.push(':');
        canonical_headers.push_str(&values.join(","));
        canonical_headers.push('\n');
    }
    let signed_headers = headers
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(";");

    let text = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        request.method,
        canonical_uri(request.path, options),
        canonical_query(request.query),
    );
    Ok(Canonical {
        text,
        signed_headers,
        payload_hash,
    })
}

#[cfg(test)]
#[path = "sigv4_tests.rs"]
mod sigv4_tests;
