// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! libvirt RPC message framing.
//!
//! Every message on the wire is:
//!
//! ```text
//! +----------------+------------------------+-------------------+
//! | length (u32)   | header (24 bytes, XDR) | payload (XDR)     |
//! +----------------+------------------------+-------------------+
//! ```
//!
//! The length prefix is the **total** message size *including the prefix
//! itself* — a detail that silently truncates every message if got wrong.
//!
//! All constants and field orders here are transcribed from libvirt's
//! `src/rpc/virnetprotocol.x` and `src/remote/remote_protocol.x` rather than
//! from documentation, because these values are the contract: an enum value or
//! a field ordering that is subtly wrong still round-trips against itself and
//! only fails on contact with a real libvirtd.

use crate::xdr::{Decoder, Encoder, XdrError};

/// `REMOTE_PROGRAM` — identifies the libvirt remote driver program.
pub const REMOTE_PROGRAM: u32 = 0x2000_8086;

/// `REMOTE_PROTOCOL_VERSION`.
pub const REMOTE_PROTOCOL_VERSION: u32 = 1;

/// `VIR_NET_MESSAGE_HEADER_MAX` — the fixed header is six 32-bit fields.
pub const MESSAGE_HEADER_LEN: usize = 24;

/// `VIR_NET_MESSAGE_LEN_MAX` — width of the length prefix.
pub const MESSAGE_LEN_PREFIX_LEN: usize = 4;

/// `VIR_NET_MESSAGE_MAX` — the largest message libvirtd will accept (32 MiB).
pub const MESSAGE_MAX: usize = 33_554_432;

/// `VIR_NET_MESSAGE_PAYLOAD_MAX` — [`MESSAGE_MAX`] less the header.
pub const PAYLOAD_MAX: usize = MESSAGE_MAX - MESSAGE_HEADER_LEN;

/// `VIR_NET_MESSAGE_LEGACY_PAYLOAD_MAX` — the payload size libvirt's own
/// client uses for stream data packets.
///
/// Confirmed against a real `virsh vol-upload` trace: every full stream packet
/// was `len=262148` on the wire, and `262148 - 4 - 24 == 262120`. Matching this
/// exactly keeps our chunking indistinguishable from the reference client's.
pub const STREAM_CHUNK_MAX: usize = 262_120;

/// Smallest structurally valid message: a prefix and a header, no payload.
const MESSAGE_MIN: usize = MESSAGE_LEN_PREFIX_LEN + MESSAGE_HEADER_LEN;

// Procedure numbers (`remote_protocol.x`). Only those banlieue uses.
/// `REMOTE_PROC_CONNECT_OPEN`.
pub const PROC_CONNECT_OPEN: i32 = 1;
/// `REMOTE_PROC_CONNECT_CLOSE`.
pub const PROC_CONNECT_CLOSE: i32 = 2;
/// `REMOTE_PROC_AUTH_LIST` — the authentication negotiation every libvirt
/// client performs *before* `CONNECT_OPEN`. Verified against a real client:
/// libvirt's own first message on any connection is this procedure.
pub const PROC_AUTH_LIST: i32 = 66;
/// `REMOTE_PROC_STORAGE_POOL_REFRESH`.
pub const PROC_STORAGE_POOL_REFRESH: i32 = 83;
/// `REMOTE_PROC_STORAGE_POOL_LOOKUP_BY_NAME`.
pub const PROC_STORAGE_POOL_LOOKUP_BY_NAME: i32 = 84;
/// `REMOTE_PROC_STORAGE_POOL_GET_XML_DESC`.
pub const PROC_STORAGE_POOL_GET_XML_DESC: i32 = 88;
/// `REMOTE_PROC_STORAGE_VOL_CREATE_XML`.
pub const PROC_STORAGE_VOL_CREATE_XML: i32 = 93;
/// `REMOTE_PROC_STORAGE_VOL_UPLOAD`.
pub const PROC_STORAGE_VOL_UPLOAD: i32 = 208;
/// `REMOTE_PROC_CONNECT_LIST_ALL_STORAGE_POOLS`.
pub const PROC_CONNECT_LIST_ALL_STORAGE_POOLS: i32 = 281;
/// `REMOTE_PROC_STORAGE_POOL_LIST_ALL_VOLUMES`.
pub const PROC_STORAGE_POOL_LIST_ALL_VOLUMES: i32 = 282;
/// `REMOTE_PROC_CONNECT_LIST_ALL_NETWORKS`.
pub const PROC_CONNECT_LIST_ALL_NETWORKS: i32 = 283;

/// Errors from framing and decoding RPC messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    /// The XDR layer failed.
    #[error("xdr: {0}")]
    Xdr(#[from] XdrError),

    /// The length prefix is structurally impossible — below the minimum
    /// message size or above `VIR_NET_MESSAGE_MAX`.
    #[error("invalid message length: {0}")]
    InvalidLength(u32),

    /// The buffer is shorter than the length prefix claims.
    #[error("truncated message: prefix claims {expected} bytes, got {actual}")]
    Truncated { expected: usize, actual: usize },

    /// `type` was not a known `virNetMessageType`.
    #[error("invalid message type: {0}")]
    InvalidMessageType(i32),

    /// `status` was not a known `virNetMessageStatus`.
    #[error("invalid message status: {0}")]
    InvalidMessageStatus(i32),
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, RpcError>;

/// `virNetMessageType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum MessageType {
    /// A method call.
    Call = 0,
    /// A reply to a call.
    Reply = 1,
    /// An asynchronous event.
    Message = 2,
    /// Stream data.
    Stream = 3,
    /// A call carrying file descriptors.
    CallWithFds = 4,
    /// A reply carrying file descriptors.
    ReplyWithFds = 5,
    /// A hole in a sparse stream.
    StreamHole = 6,
}

impl MessageType {
    /// Convert a wire value.
    ///
    /// # Errors
    /// [`RpcError::InvalidMessageType`] for anything unrecognised — an unknown
    /// value means the stream has desynchronised, so guessing is worse than
    /// failing.
    pub fn from_wire(v: i32) -> Result<Self> {
        match v {
            0 => Ok(Self::Call),
            1 => Ok(Self::Reply),
            2 => Ok(Self::Message),
            3 => Ok(Self::Stream),
            4 => Ok(Self::CallWithFds),
            5 => Ok(Self::ReplyWithFds),
            6 => Ok(Self::StreamHole),
            other => Err(RpcError::InvalidMessageType(other)),
        }
    }
}

/// `virNetMessageStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum MessageStatus {
    /// Success; the payload is the declared return type.
    Ok = 0,
    /// Failure; the payload is a `virNetMessageError`.
    Error = 1,
    /// More stream data follows.
    Continue = 2,
}

impl MessageStatus {
    /// Convert a wire value.
    ///
    /// # Errors
    /// [`RpcError::InvalidMessageStatus`] for anything unrecognised.
    pub fn from_wire(v: i32) -> Result<Self> {
        match v {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Error),
            2 => Ok(Self::Continue),
            other => Err(RpcError::InvalidMessageStatus(other)),
        }
    }
}

/// `virNetMessageHeader` — six 32-bit fields, in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageHeader {
    /// Program identifier; always [`REMOTE_PROGRAM`] for the remote driver.
    pub program: u32,
    /// Program version; always [`REMOTE_PROTOCOL_VERSION`].
    pub version: u32,
    /// Procedure number. Declared `int`, so genuinely signed.
    pub procedure: i32,
    /// Message type.
    pub message_type: MessageType,
    /// Per-call serial, echoed in the reply so replies can be matched.
    pub serial: u32,
    /// Status.
    pub status: MessageStatus,
}

impl MessageHeader {
    /// Encode to exactly [`MESSAGE_HEADER_LEN`] bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.write_u32(self.program);
        e.write_u32(self.version);
        e.write_i32(self.procedure);
        e.write_i32(self.message_type as i32);
        e.write_u32(self.serial);
        e.write_i32(self.status as i32);
        debug_assert_eq!(e.len(), MESSAGE_HEADER_LEN);
        e.into_bytes()
    }

    /// Decode from `d`, consuming [`MESSAGE_HEADER_LEN`] bytes.
    ///
    /// # Errors
    /// [`RpcError::Xdr`] if truncated, or the `Invalid*` variants if `type` or
    /// `status` is unrecognised.
    pub fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            program: d.read_u32()?,
            version: d.read_u32()?,
            procedure: d.read_i32()?,
            message_type: MessageType::from_wire(d.read_i32()?)?,
            serial: d.read_u32()?,
            status: MessageStatus::from_wire(d.read_i32()?)?,
        })
    }
}

/// Validate a 4-byte length prefix, returning the total message length.
///
/// Bounds are libvirtd's own: at least a prefix plus a header, at most
/// `VIR_NET_MESSAGE_MAX`. Checking here means a corrupt or hostile prefix is
/// rejected before it can be used to size an allocation.
///
/// # Errors
/// [`RpcError::InvalidLength`] when outside those bounds.
pub fn parse_length_prefix(bytes: &[u8; MESSAGE_LEN_PREFIX_LEN]) -> Result<usize> {
    let len = u32::from_be_bytes(*bytes);
    let as_usize = len as usize;
    if !(MESSAGE_MIN..=MESSAGE_MAX).contains(&as_usize) {
        return Err(RpcError::InvalidLength(len));
    }
    Ok(as_usize)
}

/// Frame a header and payload into a complete message.
pub fn encode_message(header: &MessageHeader, payload: &[u8]) -> Vec<u8> {
    let total = MESSAGE_LEN_PREFIX_LEN + MESSAGE_HEADER_LEN + payload.len();
    let mut out = Vec::with_capacity(total);
    // The prefix counts itself.
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&header.encode());
    out.extend_from_slice(payload);
    out
}

/// Decode a complete message, returning its header and payload.
///
/// # Errors
/// [`RpcError::InvalidLength`] for an impossible prefix,
/// [`RpcError::Truncated`] if fewer bytes are present than the prefix claims,
/// or a header decoding error.
pub fn decode_message(buf: &[u8]) -> Result<(MessageHeader, &[u8])> {
    if buf.len() < MESSAGE_LEN_PREFIX_LEN {
        return Err(RpcError::Truncated {
            expected: MESSAGE_LEN_PREFIX_LEN,
            actual: buf.len(),
        });
    }
    let prefix: [u8; MESSAGE_LEN_PREFIX_LEN] = buf[..MESSAGE_LEN_PREFIX_LEN]
        .try_into()
        .expect("slice is exactly MESSAGE_LEN_PREFIX_LEN bytes");
    let total = parse_length_prefix(&prefix)?;
    if buf.len() < total {
        return Err(RpcError::Truncated {
            expected: total,
            actual: buf.len(),
        });
    }

    let mut d = Decoder::new(&buf[MESSAGE_LEN_PREFIX_LEN..total]);
    let header = MessageHeader::decode(&mut d)?;
    Ok((
        header,
        &buf[MESSAGE_LEN_PREFIX_LEN + MESSAGE_HEADER_LEN..total],
    ))
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod rpc_tests;
