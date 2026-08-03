// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Session handling: framed message exchange over any byte stream, plus the
//! TLS connector used in production.
//!
//! [`Session`] is generic over `AsyncRead + AsyncWrite`, which is what makes
//! the protocol testable: the unit tests drive it over an in-memory
//! [`tokio::io::duplex`] pair with a scripted peer, so send/receive framing,
//! serial matching, and remote-error decoding are all exercised with no
//! socket and no live libvirtd.
//!
//! Transport is **TLS only** (ADR-0011). libvirt's plaintext TCP transport
//! pairs with SASL DIGEST-MD5, which RFC 6331 declared obsolete and which
//! leaves the session — including uploaded disk image bytes — unencrypted.
//! With `auth_tls = "none"` (libvirt's default) the client certificate *is*
//! the credential, so there is no password to handle anywhere in this crate.

use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

use crate::rpc::{
    MESSAGE_HEADER_LEN, MESSAGE_LEN_PREFIX_LEN, MessageHeader, MessageStatus, MessageType,
    REMOTE_PROGRAM, REMOTE_PROTOCOL_VERSION, RpcError, encode_message, parse_length_prefix,
};
use crate::xdr::Decoder;

/// Default libvirt TLS port (`qemu+tls://host/system`).
pub const DEFAULT_TLS_PORT: u16 = 16514;

/// The byte libvirtd sends after the TLS handshake when it has accepted the
/// client's certificate and source address.
///
/// This step is in no protocol document — only in libvirt's own client
/// (`virnetclient.c`), whose comment reads: *"At this point, the server is
/// verifying _our_ certificate, IP address, etc. If we make the grade, it will
/// send us a '\1' byte."*
const TLS_CONFIRM_OK: u8 = 0x01;

/// Default ceiling on a single connect or call.
///
/// Not optional hardening: a libvirtd that accepts a connection and then never
/// replies leaves an un-timed-out client blocked forever, which in a
/// controller means one wedged backend stalls every other reconcile. That
/// exact hang was observed while bringing this crate up.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors from establishing or using a libvirt session.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Framing or decoding failed.
    #[error("rpc: {0}")]
    Rpc(#[from] RpcError),

    /// XDR decoding of a procedure payload failed. Distinct from [`Self::Rpc`]
    /// so `?` works directly on the codec inside the procedure layer.
    #[error("xdr: {0}")]
    Xdr(#[from] crate::xdr::XdrError),

    /// A reply was structurally decodable but violated the protocol — for
    /// example an array count beyond libvirt's own declared maximum.
    #[error("protocol violation: {detail}")]
    Protocol { detail: String },

    /// Socket I/O failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// TLS setup or handshake failed.
    #[error("tls: {0}")]
    Tls(String),

    /// A certificate or key could not be parsed.
    #[error("invalid PEM for {what}: {detail}")]
    Pem { what: &'static str, detail: String },

    /// The endpoint was not a usable host name or address.
    #[error("invalid server name: {0}")]
    InvalidServerName(String),

    /// libvirtd returned an error reply.
    #[error("libvirt error (code {code}): {message}")]
    Remote { code: i32, message: String },

    /// The reply did not correspond to the call that was sent. Treated as
    /// fatal rather than skipped: this connection carries one in-flight call
    /// at a time, so a mismatch means the stream has desynchronised.
    #[error("reply serial {got} does not match request serial {expected}")]
    SerialMismatch { expected: u32, got: u32 },

    /// A reply arrived with an unexpected message type.
    #[error("expected a Reply message, got {0:?}")]
    UnexpectedMessageType(MessageType),

    /// An operation exceeded its deadline. `op` names the stalled operation
    /// so a wedged endpoint is diagnosable from the error alone.
    #[error("{op} timed out after {after:?}")]
    Timeout { op: &'static str, after: Duration },

    /// A reply's program or version did not match what we sent. Almost always
    /// means the byte stream has desynchronised.
    #[error(
        "stream desynchronised: expected program {expected_program:#x} v{expected_version}, got {got_program:#x} v{got_version}"
    )]
    Desynchronised {
        expected_program: u32,
        expected_version: u32,
        got_program: u32,
        got_version: u32,
    },
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, TransportError>;

/// TLS material for a libvirt connection.
///
/// The client certificate is the credential — there is no password field
/// here, by design.
#[derive(Debug, Clone)]
pub struct TlsIdentity {
    /// PEM CA bundle that signed libvirtd's server certificate.
    pub ca_pem: Vec<u8>,
    /// PEM client certificate chain.
    pub client_cert_pem: Vec<u8>,
    /// PEM client private key.
    pub client_key_pem: Vec<u8>,
}

/// A libvirt RPC session over a byte stream.
///
/// Holds one in-flight call at a time and matches replies by serial.
#[derive(Debug)]
pub struct Session<S> {
    stream: S,
    serial: u32,
    timeout: Duration,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    /// Wrap an already-connected stream, using [`DEFAULT_TIMEOUT`].
    pub fn new(stream: S) -> Self {
        Self::with_timeout(stream, DEFAULT_TIMEOUT)
    }

    /// Wrap an already-connected stream with an explicit per-call timeout.
    pub fn with_timeout(stream: S, timeout: Duration) -> Self {
        Self {
            stream,
            serial: 0,
            timeout,
        }
    }

    /// The per-call timeout in force.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Consume the session and return the underlying stream.
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Serial for the next call.
    ///
    /// Starts at **0**. libvirt's RPC documentation says serials begin at 1,
    /// but a real client traced with `LIBVIRT_DEBUG=1` sends `serial=0` on its
    /// first call and `serial=1` on its second. The server echoes whatever it
    /// is given, so this is not load-bearing — but matching the reference
    /// implementation beats matching its prose.
    fn next_serial(&mut self) -> u32 {
        let current = self.serial;
        self.serial = self.serial.wrapping_add(1);
        current
    }

    /// Write one framed message.
    ///
    /// The write path gets the same deadline as the receive path: a peer that
    /// stops *reading* fills the socket buffer and would otherwise block
    /// `write_all` forever — the same one-wedged-backend stall, from the
    /// other direction (SEC-012).
    pub async fn send(&mut self, header: &MessageHeader, payload: &[u8]) -> Result<()> {
        let msg = encode_message(header, payload);
        let write = async {
            self.stream.write_all(&msg).await?;
            self.stream.flush().await
        };
        tokio::time::timeout(self.timeout, write)
            .await
            .map_err(|_| TransportError::Timeout {
                op: "send",
                after: self.timeout,
            })??;
        Ok(())
    }

    /// Read one framed message.
    ///
    /// The length prefix is validated by [`parse_length_prefix`] before it is
    /// used to size the read buffer, so a corrupt or hostile prefix cannot
    /// drive a huge allocation.
    pub async fn recv(&mut self) -> Result<(MessageHeader, Vec<u8>)> {
        let mut prefix = [0u8; MESSAGE_LEN_PREFIX_LEN];
        // read_exact, not read: a stream may deliver a message across several
        // reads, and a short read would otherwise be mistaken for a truncated
        // message.
        self.stream.read_exact(&mut prefix).await?;
        let total = parse_length_prefix(&prefix)?;

        let rest_len = total - MESSAGE_LEN_PREFIX_LEN;
        let mut rest = vec![0u8; rest_len];
        self.stream.read_exact(&mut rest).await?;

        let mut d = Decoder::new(&rest);
        let header = MessageHeader::decode(&mut d)?;

        // Cheap desynchronisation tripwire. Every message on this connection
        // belongs to the same program and version, so a mismatch means we are
        // reading from the wrong offset — which otherwise surfaces as a
        // nonsensical length and an indefinite block rather than an error.
        if header.program != REMOTE_PROGRAM || header.version != REMOTE_PROTOCOL_VERSION {
            return Err(TransportError::Desynchronised {
                expected_program: REMOTE_PROGRAM,
                expected_version: REMOTE_PROTOCOL_VERSION,
                got_program: header.program,
                got_version: header.version,
            });
        }

        let payload = rest[MESSAGE_HEADER_LEN..].to_vec();
        Ok((header, payload))
    }

    /// Issue a call and await its reply, returning the reply payload.
    ///
    /// # Errors
    /// [`TransportError::Remote`] if libvirtd replied with an error status,
    /// [`TransportError::SerialMismatch`] or
    /// [`TransportError::UnexpectedMessageType`] if the reply does not match
    /// the request, plus any I/O or decoding error.
    pub async fn call(&mut self, procedure: i32, payload: &[u8]) -> Result<Vec<u8>> {
        self.call_with_serial(procedure, payload)
            .await
            .map(|(_, b)| b)
    }

    /// As [`call`](Self::call), but also returns the serial that was used.
    ///
    /// Streaming procedures need it: every stream packet must carry the same
    /// serial *and* procedure as the call that opened the stream (confirmed
    /// against a real `virsh vol-upload` trace).
    ///
    /// # Errors
    /// As [`call`](Self::call).
    pub async fn call_with_serial(
        &mut self,
        procedure: i32,
        payload: &[u8],
    ) -> Result<(u32, Vec<u8>)> {
        let serial = self.next_serial();
        let header = MessageHeader {
            program: REMOTE_PROGRAM,
            version: REMOTE_PROTOCOL_VERSION,
            procedure,
            message_type: MessageType::Call,
            serial,
            status: MessageStatus::Ok,
        };
        self.send(&header, payload).await?;

        let (reply, body) = tokio::time::timeout(self.timeout, self.recv())
            .await
            .map_err(|_| TransportError::Timeout {
                op: "receive",
                after: self.timeout,
            })??;
        if reply.serial != serial {
            return Err(TransportError::SerialMismatch {
                expected: serial,
                got: reply.serial,
            });
        }
        if reply.message_type != MessageType::Reply {
            return Err(TransportError::UnexpectedMessageType(reply.message_type));
        }
        match reply.status {
            MessageStatus::Ok => Ok((serial, body)),
            MessageStatus::Error => Err(decode_remote_error(&body)),
            // A bare Continue is only meaningful inside a stream, which this
            // call path does not establish.
            MessageStatus::Continue => {
                Err(TransportError::UnexpectedMessageType(reply.message_type))
            }
        }
    }

    /// Send one stream data packet: `type = Stream`, `status = Continue`.
    ///
    /// `procedure` and `serial` must match the call that opened the stream.
    /// The payload is raw bytes — stream packets are **not** XDR-encoded.
    ///
    /// # Errors
    /// [`TransportError::Protocol`] if `data` exceeds
    /// [`STREAM_CHUNK_MAX`](crate::rpc::STREAM_CHUNK_MAX), plus any I/O error.
    pub async fn send_stream_data(
        &mut self,
        procedure: i32,
        serial: u32,
        data: &[u8],
    ) -> Result<()> {
        if data.len() > crate::rpc::STREAM_CHUNK_MAX {
            return Err(TransportError::Protocol {
                detail: format!(
                    "stream chunk of {} bytes exceeds the maximum of {}",
                    data.len(),
                    crate::rpc::STREAM_CHUNK_MAX
                ),
            });
        }
        let header = MessageHeader {
            program: REMOTE_PROGRAM,
            version: REMOTE_PROTOCOL_VERSION,
            procedure,
            message_type: MessageType::Stream,
            serial,
            status: MessageStatus::Continue,
        };
        self.send(&header, data).await
    }

    /// Close a stream: `type = Stream`, `status = Ok`, empty payload.
    ///
    /// The server answers with its own `Stream`/`Ok` message, which this
    /// awaits, so a successful return means the server accepted the whole
    /// stream rather than merely that the bytes were written.
    ///
    /// # Errors
    /// [`TransportError::Remote`] if the server reports failure, plus any I/O
    /// or decoding error.
    pub async fn finish_stream(&mut self, procedure: i32, serial: u32) -> Result<()> {
        let header = MessageHeader {
            program: REMOTE_PROGRAM,
            version: REMOTE_PROTOCOL_VERSION,
            procedure,
            message_type: MessageType::Stream,
            serial,
            status: MessageStatus::Ok,
        };
        self.send(&header, &[]).await?;

        let (reply, body) = tokio::time::timeout(self.timeout, self.recv())
            .await
            .map_err(|_| TransportError::Timeout {
                op: "receive",
                after: self.timeout,
            })??;
        if reply.serial != serial {
            return Err(TransportError::SerialMismatch {
                expected: serial,
                got: reply.serial,
            });
        }
        match reply.status {
            MessageStatus::Ok => Ok(()),
            MessageStatus::Error => Err(decode_remote_error(&body)),
            MessageStatus::Continue => Err(TransportError::Protocol {
                detail: "server sent Continue in response to stream completion".into(),
            }),
        }
    }
}

/// Decode a `virNetMessageError` payload into a [`TransportError::Remote`].
///
/// Only the leading fields are read — `code`, `domain`, and the optional
/// `message` — which is everything needed to report a useful error. XDR
/// encodes an optional (`pointer`) field as a boolean followed by the value
/// when present, so the message must be read through that indirection.
///
/// A payload that cannot be decoded still yields a `Remote` error rather than
/// a decoding error: libvirtd has told us the call failed, and losing that
/// because its error body was unparseable would be the worse outcome.
fn decode_remote_error(payload: &[u8]) -> TransportError {
    let mut d = Decoder::new(payload);
    let code = d.read_i32().unwrap_or(-1);
    let _domain = d.read_i32().unwrap_or(-1);
    let message = match d.read_bool() {
        Ok(true) => d.read_string().unwrap_or("<unparseable>").to_string(),
        Ok(false) => "<no message>".to_string(),
        Err(_) => "<unparseable>".to_string(),
    };
    TransportError::Remote { code, message }
}

/// Connect to libvirtd over mutual TLS.
///
/// `host` must match a name or address in libvirtd's server certificate SANs
/// — libvirt validates against whatever the client dialled, so connecting by
/// IP requires that IP to be a SAN.
///
/// # Errors
/// [`TransportError::Pem`] for unparseable certificates or keys,
/// [`TransportError::Tls`] for handshake or configuration failure, and
/// [`TransportError::Io`] for connection failure.
pub async fn connect_tls(
    host: &str,
    port: u16,
    identity: &TlsIdentity,
) -> Result<Session<TlsStream<TcpStream>>> {
    connect_tls_with_timeout(host, port, identity, DEFAULT_TIMEOUT).await
}

/// As [`connect_tls`], with an explicit deadline for the whole connect path
/// and for subsequent calls on the returned session.
///
/// # Errors
/// As [`connect_tls`], plus [`TransportError::Timeout`].
pub async fn connect_tls_with_timeout(
    host: &str,
    port: u16,
    identity: &TlsIdentity,
    timeout: Duration,
) -> Result<Session<TlsStream<TcpStream>>> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(&identity.ca_pem) {
        let cert = cert.map_err(|e| TransportError::Pem {
            what: "caBundle",
            detail: e.to_string(),
        })?;
        roots.add(cert).map_err(|e| TransportError::Pem {
            what: "caBundle",
            detail: e.to_string(),
        })?;
    }

    let chain = CertificateDer::pem_slice_iter(&identity.client_cert_pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| TransportError::Pem {
            what: "client certificate",
            detail: e.to_string(),
        })?;

    let key = PrivateKeyDer::from_pem_slice(&identity.client_key_pem).map_err(|e| {
        TransportError::Pem {
            what: "client key",
            detail: e.to_string(),
        }
    })?;

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|e| TransportError::Tls(e.to_string()))?;

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| TransportError::InvalidServerName(host.to_string()))?;

    // The whole connect path is bounded: a host that accepts TCP but never
    // completes the handshake, or completes it and never sends the
    // confirmation byte, must not block a caller indefinitely.
    let connect = async {
        let tcp = TcpStream::connect((host, port)).await?;
        TlsConnector::from(Arc::new(config))
            .connect(server_name, tcp)
            .await
            .map_err(|e| TransportError::Tls(e.to_string()))
    };
    let mut stream =
        tokio::time::timeout(timeout, connect)
            .await
            .map_err(|_| TransportError::Timeout {
                op: "connect",
                after: timeout,
            })??;

    // libvirt's TLS transport has one more step after the handshake, and it is
    // mandatory: the server validates our certificate and source address, then
    // sends a single [`TLS_CONFIRM_OK`] byte. libvirt's own client reads it
    // before issuing any RPC.
    //
    // Skipping it does NOT fail loudly. The byte simply sits unread in the
    // stream, so the first reply's length prefix is parsed one byte out of
    // alignment; the resulting garbage length then blocks `read_exact`
    // forever. Diagnosed exactly that way against a live libvirtd — the
    // session established, the first call never returned, and libvirtd logged
    // nothing because from its side it had answered correctly.
    let mut confirm = [0u8; 1];
    tokio::time::timeout(timeout, stream.read_exact(&mut confirm))
        .await
        .map_err(|_| TransportError::Timeout {
            op: "read server confirmation byte",
            after: timeout,
        })??;
    if confirm[0] != TLS_CONFIRM_OK {
        return Err(TransportError::Tls(format!(
            "server rejected our client certificate or source address \
             (confirmation byte {:#04x}, expected {TLS_CONFIRM_OK:#04x})",
            confirm[0]
        )));
    }

    Ok(Session::with_timeout(stream, timeout))
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
