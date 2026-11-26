// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A minimal HTTP/1.1 client over `std::net::TcpStream`, for WireServer only.
//!
//! WireServer speaks plain HTTP on a host-local address (Azure's design: the
//! link never leaves the physical host), so there is no TLS here and no
//! reason for a general client. One request per connection
//! (`Connection: close`), bounded connect, read and write timeouts, a capped
//! response, and the two body framings a server may use: `Content-Length`
//! and `chunked`. Redirects are never followed.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use thiserror::Error;

/// Largest response accepted, headers included. A goal state is a few KiB.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// HTTP status codes in this range are success.
const SUCCESS_MIN: u16 = 200;
const SUCCESS_MAX: u16 = 299;

/// Radix of a chunk-size line.
const HEX_RADIX: u32 = 16;

/// Why a request failed.
#[derive(Debug, Error)]
pub enum HttpError {
    /// Connecting, writing or reading failed (no route, refused, timeout).
    #[error("connection: {0}")]
    Io(#[from] io::Error),
    /// The response was larger than [`MAX_RESPONSE_BYTES`].
    #[error("response larger than {MAX_RESPONSE_BYTES} bytes")]
    TooLarge,
    /// The response could not be parsed.
    #[error("malformed response: {0}")]
    Malformed(&'static str),
}

/// A parsed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The body, de-chunked.
    pub body: Vec<u8>,
}

impl Response {
    /// A 2xx status.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (SUCCESS_MIN..=SUCCESS_MAX).contains(&self.status)
    }
}

/// One request: method, path and query, extra headers, optional body.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// `GET` or `POST`.
    pub method: &'a str,
    /// Path and query, starting with `/`.
    pub path: &'a str,
    /// Extra headers, as name and value.
    pub headers: &'a [(&'a str, &'a str)],
    /// Body, sent with a `Content-Length`.
    pub body: Option<&'a [u8]>,
}

/// The bytes sent for `req` to `host`.
#[must_use]
pub fn encode_request(req: &Request<'_>, host: &str) -> Vec<u8> {
    let mut out = format!(
        "{} {} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n",
        req.method, req.path
    );
    for (name, value) in req.headers {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    if let Some(body) = req.body {
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    if let Some(body) = req.body {
        bytes.extend_from_slice(body);
    }
    bytes
}

/// Decode a `chunked` body; trailers are ignored.
fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    loop {
        let line_end = find(data, b"\r\n").ok_or(HttpError::Malformed("chunk size line"))?;
        let line = std::str::from_utf8(&data[..line_end])
            .map_err(|_| HttpError::Malformed("chunk size is not text"))?;
        let size_text = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, HEX_RADIX)
            .map_err(|_| HttpError::Malformed("chunk size is not hex"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 || &data[size..size + 2] != b"\r\n" {
            return Err(HttpError::Malformed("chunk shorter than its size"));
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse a complete response (the server closed the connection).
///
/// # Errors
/// [`HttpError::Malformed`] for a bad status line, header block or body
/// framing; a `Content-Length` longer than what arrived is malformed too.
pub fn parse_response(raw: &[u8]) -> Result<Response, HttpError> {
    let head_end = find(raw, b"\r\n\r\n").ok_or(HttpError::Malformed("no end of headers"))?;
    let head = std::str::from_utf8(&raw[..head_end])
        .map_err(|_| HttpError::Malformed("headers are not text"))?;
    let body = &raw[head_end + "\r\n\r\n".len()..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(HttpError::Malformed("not an HTTP/1.x status line"));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(HttpError::Malformed("no status code"))?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or(HttpError::Malformed("header without a colon"))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .parse()
                    .map_err(|_| HttpError::Malformed("bad Content-Length"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else if let Some(len) = content_length {
        if body.len() < len {
            return Err(HttpError::Malformed("body shorter than Content-Length"));
        }
        body[..len].to_vec()
    } else {
        body.to_vec()
    };
    Ok(Response { status, body })
}

/// Send `req` to `addr` and read the whole response.
///
/// # Errors
/// [`HttpError::Io`] when the connection fails or times out,
/// [`HttpError::TooLarge`] or [`HttpError::Malformed`] for the response.
pub fn send(addr: SocketAddr, req: &Request<'_>, timeout: Duration) -> Result<Response, HttpError> {
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(&encode_request(req, &addr.ip().to_string()))?;
    stream.flush()?;
    let mut raw = Vec::new();
    let limit = u64::try_from(MAX_RESPONSE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    (&mut stream).take(limit).read_to_end(&mut raw)?;
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(HttpError::TooLarge);
    }
    parse_response(&raw)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod http_tests;
