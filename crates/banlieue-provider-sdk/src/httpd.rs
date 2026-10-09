// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The minimal HTTP/1.1 listener behind the health server (ADR-0093) and the
//! metrics server (ADR-0091).
//!
//! Deliberately not a web framework: both servers answer a handful of `GET`
//! paths from kubelet and Prometheus, so all this module does is read the
//! request line (capped at [`MAX_REQUEST_LINE_BYTES`]), hand it to a router,
//! write one response and close the connection. Headers and bodies are never
//! parsed.
//!
//! The decision logic is pure: [`parse_request_line`] and [`respond`] turn
//! request bytes into a [`Response`] with no I/O, and are unit-tested in
//! `httpd_tests.rs`. Only [`bind`] and [`spawn_server`] touch the network.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info};

/// Largest request line accepted, in bytes. A request whose first line does
/// not end within this many bytes is answered `400` and closed (ADR-0093
/// Consequences: the same 1 KiB cap the original health server read).
pub const MAX_REQUEST_LINE_BYTES: usize = 1024;

/// How long a client gets to send its request line before the connection is
/// dropped, so a connection that never sends one cannot hold a task forever.
pub const REQUEST_READ_TIMEOUT_SECS: u64 = 5;

/// Address every observability listener binds: all IPv4 interfaces, as the
/// original health server did, so kubelet and Prometheus reach the pod IP.
pub const BIND_ADDRESS: &str = "0.0.0.0";

/// Content type of every plain-text response.
pub const TEXT_PLAIN: &str = "text/plain; charset=utf-8";

/// Body of a `400` response.
const BAD_REQUEST_BODY: &str = "bad request";

/// Body of a `404` response.
const NOT_FOUND_BODY: &str = "not found";

/// Prefix every accepted HTTP version must carry. HTTP/2 prior knowledge and
/// HTTP/0.9 are both answered `400`.
const HTTP_1_VERSION_PREFIX: &str = "HTTP/1.";

/// Number of space-separated parts in a request line: method, target,
/// version.
const REQUEST_LINE_PARTS: usize = 3;

/// The only method either server routes.
pub const METHOD_GET: &str = "GET";

/// The HTTP status codes these servers answer with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `200 OK`.
    Ok,
    /// `400 Bad Request`: malformed or oversized request line.
    BadRequest,
    /// `404 Not Found`: any path or method the router does not serve.
    NotFound,
    /// `503 Service Unavailable`: `/readyz` while not ready.
    ServiceUnavailable,
}

impl Status {
    /// The numeric status code.
    pub fn code(self) -> u16 {
        const OK: u16 = 200;
        const BAD_REQUEST: u16 = 400;
        const NOT_FOUND: u16 = 404;
        const SERVICE_UNAVAILABLE: u16 = 503;
        match self {
            Self::Ok => OK,
            Self::BadRequest => BAD_REQUEST,
            Self::NotFound => NOT_FOUND,
            Self::ServiceUnavailable => SERVICE_UNAVAILABLE,
        }
    }

    /// The reason phrase written after the code.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::BadRequest => "Bad Request",
            Self::NotFound => "Not Found",
            Self::ServiceUnavailable => "Service Unavailable",
        }
    }
}

/// One complete response: status, content type and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status line code and reason.
    pub status: Status,
    /// `Content-Type` header value.
    pub content_type: &'static str,
    /// Response body.
    pub body: String,
}

impl Response {
    /// A plain-text response.
    pub fn text(status: Status, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: TEXT_PLAIN,
            body: body.into(),
        }
    }

    /// The `404` every router answers for a path it does not serve.
    pub fn not_found() -> Self {
        Self::text(Status::NotFound, NOT_FOUND_BODY)
    }

    /// The `400` answered for a malformed or oversized request line.
    pub fn bad_request() -> Self {
        Self::text(Status::BadRequest, BAD_REQUEST_BODY)
    }

    /// Serialise to the bytes written on the wire. Every response closes the
    /// connection: neither server supports keep-alive.
    pub fn to_bytes(&self) -> Vec<u8> {
        format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.status.code(),
            self.status.reason(),
            self.content_type,
            self.body.len(),
            self.body,
        )
        .into_bytes()
    }
}

/// The parts of a request line a router needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestLine<'a> {
    /// The method, e.g. `GET`.
    pub method: &'a str,
    /// The request target's path, without any query string.
    pub path: &'a str,
}

/// Why a request line was rejected with `400`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// No line terminator within [`MAX_REQUEST_LINE_BYTES`].
    #[error("request line exceeds {MAX_REQUEST_LINE_BYTES} bytes")]
    TooLarge,
    /// The line is incomplete, not UTF-8, or not `METHOD /path HTTP/1.x`.
    #[error("malformed request line")]
    Malformed,
}

/// Parse the request line at the start of `buf`.
///
/// Only the first line is examined; anything after it (headers) is ignored.
///
/// # Errors
/// [`RequestError::TooLarge`] when `buf` holds [`MAX_REQUEST_LINE_BYTES`] or
/// more without a line terminator, [`RequestError::Malformed`] for anything
/// else that is not `METHOD /path HTTP/1.x`.
pub fn parse_request_line(buf: &[u8]) -> Result<RequestLine<'_>, RequestError> {
    let Some(end) = buf.iter().position(|b| *b == b'\n') else {
        if buf.len() >= MAX_REQUEST_LINE_BYTES {
            return Err(RequestError::TooLarge);
        }
        return Err(RequestError::Malformed);
    };
    if end >= MAX_REQUEST_LINE_BYTES {
        return Err(RequestError::TooLarge);
    }

    let line = std::str::from_utf8(&buf[..end]).map_err(|_| RequestError::Malformed)?;
    let line = line.strip_suffix('\r').unwrap_or(line);

    let parts: Vec<&str> = line.split(' ').collect();
    if parts.len() != REQUEST_LINE_PARTS {
        return Err(RequestError::Malformed);
    }
    let (method, target, version) = (parts[0], parts[1], parts[REQUEST_LINE_PARTS - 1]);

    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(RequestError::Malformed);
    }
    if !target.starts_with('/') {
        return Err(RequestError::Malformed);
    }
    if !version.starts_with(HTTP_1_VERSION_PREFIX) {
        return Err(RequestError::Malformed);
    }

    let path = target.split_once('?').map_or(target, |(path, _query)| path);
    Ok(RequestLine { method, path })
}

/// Turn raw request bytes into a response: `400` for a request line that does
/// not parse, otherwise whatever `route` answers.
pub fn respond(buf: &[u8], route: impl FnOnce(&RequestLine<'_>) -> Response) -> Response {
    match parse_request_line(buf) {
        Ok(line) => route(&line),
        Err(e) => {
            debug!(error = %e, "rejecting request");
            Response::bad_request()
        }
    }
}

/// A router: raw request bytes in, one response out.
pub type Handler = Arc<dyn Fn(&[u8]) -> Response + Send + Sync>;

/// Bind a listener on [`BIND_ADDRESS`]`:port`.
///
/// # Errors
/// Returns the bind error (port in use, permission denied). Callers treat it
/// as fatal (ADR-0093 Decision 4).
pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind((BIND_ADDRESS, port)).await
}

/// Serve `handler` on an already-bound `listener` until the process exits.
///
/// Each connection is handled on its own task: read the request line, write
/// one response, close.
///
/// # Arguments
/// * `listener` - from [`bind`].
/// * `server` - a short name for logs (`health`, `metrics`).
/// * `handler` - the router.
pub fn spawn_server(listener: TcpListener, server: &'static str, handler: Handler) {
    let port = listener.local_addr().map(|a| a.port()).unwrap_or_default();
    info!(server, port, "listening");
    tokio::spawn(async move {
        loop {
            let socket = match listener.accept().await {
                Ok((socket, _peer)) => socket,
                Err(e) => {
                    error!(server, error = %e, "accept failed");
                    continue;
                }
            };
            let handler = handler.clone();
            tokio::spawn(handle_connection(socket, handler));
        }
    });
}

/// Read one request line from `socket`, answer it and close the connection.
async fn handle_connection(mut socket: TcpStream, handler: Handler) {
    let timeout = Duration::from_secs(REQUEST_READ_TIMEOUT_SECS);
    let Ok(buf) = tokio::time::timeout(timeout, read_request_line(&mut socket)).await else {
        debug!("request line not received in time; closing");
        return;
    };
    let response = handler(&buf);
    let _ = socket.write_all(&response.to_bytes()).await;
    let _ = socket.shutdown().await;
}

/// Read until the first line terminator, [`MAX_REQUEST_LINE_BYTES`], or EOF,
/// whichever comes first. What was read is returned either way; the parser
/// decides whether it is a request.
async fn read_request_line(socket: &mut TcpStream) -> Vec<u8> {
    let mut buf = vec![0u8; MAX_REQUEST_LINE_BYTES];
    let mut filled = 0;
    while filled < MAX_REQUEST_LINE_BYTES {
        let read = match socket.read(&mut buf[filled..]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let terminated = buf[filled..filled + read].contains(&b'\n');
        filled += read;
        if terminated {
            break;
        }
    }
    buf.truncate(filled);
    buf
}

#[cfg(test)]
#[path = "httpd_tests.rs"]
mod httpd_tests;
