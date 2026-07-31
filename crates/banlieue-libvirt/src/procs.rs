// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The libvirt procedures banlieue actually calls.
//!
//! Argument and return layouts are transcribed from
//! `src/remote/remote_protocol.x`. Two XDR conventions dominate and are worth
//! stating once:
//!
//! - A **pointer** field (`remote_string`, i.e. `remote_nonnull_string *`) is
//!   an *optional*: a boolean, then the value only when that boolean is true.
//!   Writing the value unconditionally silently shifts every following field.
//! - A **variable-length array** (`pools<MAX>`) is a `u32` count followed by
//!   that many elements. The count arrives from the network, so it is checked
//!   against libvirt's own declared maximum before anything is allocated.
//!
//! The encode/decode halves are plain functions over bytes rather than
//! methods on a session, so their exact wire output is unit-testable without
//! a connection.

use crate::rpc::{
    PROC_AUTH_LIST, PROC_CONNECT_LIST_ALL_NETWORKS, PROC_CONNECT_LIST_ALL_STORAGE_POOLS,
    PROC_CONNECT_OPEN, PROC_STORAGE_POOL_LIST_ALL_VOLUMES, PROC_STORAGE_VOL_CREATE_XML,
    PROC_STORAGE_VOL_UPLOAD, STREAM_CHUNK_MAX,
};
use crate::transport::{Result, Session, TransportError};
use crate::xdr::{Decoder, Encoder};
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncRead, AsyncWrite};

/// `VIR_UUID_BUFLEN` — libvirt UUIDs are a fixed 16 raw bytes on the wire,
/// not the 36-character string form.
pub const UUID_LEN: usize = 16;

/// `REMOTE_STORAGE_POOL_LIST_MAX`.
pub const STORAGE_POOL_LIST_MAX: usize = 16384;

/// `REMOTE_NETWORK_LIST_MAX`.
pub const NETWORK_LIST_MAX: usize = 16384;

/// `VIR_CONNECT_RO` — open the connection read-only.
pub const CONNECT_RO: u32 = 1 << 0;

/// `REMOTE_AUTH_TYPE_LIST_MAX`.
pub const AUTH_TYPE_LIST_MAX: usize = 20;

/// `REMOTE_STORAGE_VOL_LIST_MAX`.
pub const STORAGE_VOL_LIST_MAX: usize = 16384;

/// `remote_auth_type` — the authentication mechanisms libvirtd offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    /// No authentication beyond the transport itself. What `auth_tls="none"`
    /// yields: the x509 client certificate already established identity.
    None,
    /// SASL negotiation required.
    Sasl,
    /// PolicyKit required (local UNIX socket only).
    Polkit,
    /// A mechanism this client does not know.
    Unknown(i32),
}

impl AuthType {
    fn from_wire(v: i32) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Sasl,
            2 => Self::Polkit,
            other => Self::Unknown(other),
        }
    }
}

/// A storage pool as returned by `CONNECT_LIST_ALL_STORAGE_POOLS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePool {
    /// Pool name, e.g. `default`.
    pub name: String,
    /// Raw 16-byte UUID.
    pub uuid: [u8; UUID_LEN],
}

/// A network as returned by `CONNECT_LIST_ALL_NETWORKS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    /// Network name, e.g. `default`.
    pub name: String,
    /// Raw 16-byte UUID.
    pub uuid: [u8; UUID_LEN],
}

/// Encode `remote_connect_open_args { remote_string name; unsigned int flags; }`.
///
/// `name` is a pointer type, so it is encoded as an optional: a boolean
/// followed by the string only when `Some`.
pub fn encode_connect_open_args(uri: Option<&str>, flags: u32) -> Vec<u8> {
    let mut e = Encoder::new();
    match uri {
        Some(u) => {
            e.write_bool(true);
            e.write_string(u);
        }
        None => e.write_bool(false),
    }
    e.write_u32(flags);
    e.into_bytes()
}

/// Encode the `{ int need_results; unsigned int flags; }` argument struct
/// shared by `CONNECT_LIST_ALL_STORAGE_POOLS` and
/// `CONNECT_LIST_ALL_NETWORKS`.
pub fn encode_list_all_args(need_results: bool, flags: u32) -> Vec<u8> {
    let mut e = Encoder::new();
    e.write_i32(i32::from(need_results));
    e.write_u32(flags);
    e.into_bytes()
}

/// Read a `u32` array count and check it against `max` before it is used to
/// reserve capacity.
///
/// libvirt declares these bounds itself (`REMOTE_*_LIST_MAX`); honouring them
/// means a corrupt or hostile count cannot turn into a huge allocation, the
/// same guard the XDR and framing layers apply to their own lengths.
fn read_checked_count(d: &mut Decoder<'_>, max: usize, what: &'static str) -> Result<usize> {
    let count = d.read_u32()? as usize;
    if count > max {
        return Err(TransportError::Protocol {
            detail: format!("{what} count {count} exceeds the protocol maximum of {max}"),
        });
    }
    Ok(count)
}

/// Read a fixed 16-byte UUID.
fn read_uuid(d: &mut Decoder<'_>) -> Result<[u8; UUID_LEN]> {
    let bytes = d.read_opaque_fixed(UUID_LEN)?;
    let mut uuid = [0u8; UUID_LEN];
    uuid.copy_from_slice(bytes);
    Ok(uuid)
}

/// Decode `remote_connect_list_all_storage_pools_ret`.
///
/// Layout: `pools<>` (count then `{ name: string, uuid: opaque[16] }`
/// elements), followed by a `u32` total which we ignore in favour of the
/// array we actually received.
pub fn decode_storage_pools(payload: &[u8]) -> Result<Vec<StoragePool>> {
    let mut d = Decoder::new(payload);
    let count = read_checked_count(&mut d, STORAGE_POOL_LIST_MAX, "storage pool")?;
    let mut pools = Vec::with_capacity(count);
    for _ in 0..count {
        pools.push(StoragePool {
            name: d.read_string()?.to_string(),
            uuid: read_uuid(&mut d)?,
        });
    }
    Ok(pools)
}

/// Decode `remote_connect_list_all_networks_ret`. Same shape as
/// [`decode_storage_pools`].
pub fn decode_networks(payload: &[u8]) -> Result<Vec<Network>> {
    let mut d = Decoder::new(payload);
    let count = read_checked_count(&mut d, NETWORK_LIST_MAX, "network")?;
    let mut nets = Vec::with_capacity(count);
    for _ in 0..count {
        nets.push(Network {
            name: d.read_string()?.to_string(),
            uuid: read_uuid(&mut d)?,
        });
    }
    Ok(nets)
}

/// Decode `remote_auth_list_ret { remote_auth_type types<>; }`.
pub fn decode_auth_list(payload: &[u8]) -> Result<Vec<AuthType>> {
    let mut d = Decoder::new(payload);
    let count = read_checked_count(&mut d, AUTH_TYPE_LIST_MAX, "auth type")?;
    let mut types = Vec::with_capacity(count);
    for _ in 0..count {
        types.push(AuthType::from_wire(d.read_i32()?));
    }
    Ok(types)
}

/// Ask libvirtd which authentication mechanisms it requires.
///
/// Takes no arguments (`remote_auth_list_args` does not exist).
///
/// # Errors
/// Any [`TransportError`].
pub async fn auth_list<S>(session: &mut Session<S>) -> Result<Vec<AuthType>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let body = session.call(PROC_AUTH_LIST, &[]).await?;
    decode_auth_list(&body)
}

/// Open the connection: negotiate authentication, then `CONNECT_OPEN`.
///
/// **`AUTH_LIST` must come first.** This is not optional politeness — a real
/// libvirt client traced with `LIBVIRT_DEBUG=1` sends `AUTH_LIST` as its very
/// first message on every connection, and libvirtd holds a client in a
/// pre-auth state until it does. Sending `CONNECT_OPEN` straight away does not
/// produce an error; the server simply never replies, so the call hangs
/// forever. That failure mode is invisible to offline tests, which is exactly
/// why ADR-0011 requires this integration path.
///
/// `uri` is the driver URI as libvirtd sees it locally (`qemu:///system`) —
/// **not** the `qemu+tls://` URI used to reach the host, which describes the
/// transport rather than the driver.
///
/// # Errors
/// [`TransportError::Protocol`] if libvirtd requires a mechanism this client
/// does not implement (only `REMOTE_AUTH_NONE` is supported — with
/// `auth_tls="none"` the client certificate is already the credential), plus
/// any other [`TransportError`].
pub async fn connect_open<S>(
    session: &mut Session<S>,
    uri: Option<&str>,
    read_only: bool,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let offered = auth_list(session).await?;
    if !offered.contains(&AuthType::None) {
        return Err(TransportError::Protocol {
            detail: format!(
                "libvirtd requires authentication this client does not implement: {offered:?}. \
                 banlieue supports x509 client-certificate auth only (auth_tls=\"none\")."
            ),
        });
    }

    let flags = if read_only { CONNECT_RO } else { 0 };
    let args = encode_connect_open_args(uri, flags);
    session.call(PROC_CONNECT_OPEN, &args).await?;
    Ok(())
}

/// List every storage pool, defined or running.
///
/// # Errors
/// Any [`TransportError`].
pub async fn list_all_storage_pools<S>(session: &mut Session<S>) -> Result<Vec<StoragePool>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // need_results=true, flags=0 (no filtering: banlieue verifies the
    // admin-declared pools against the full list itself).
    let args = encode_list_all_args(true, 0);
    let body = session
        .call(PROC_CONNECT_LIST_ALL_STORAGE_POOLS, &args)
        .await?;
    decode_storage_pools(&body)
}

/// List every network, defined or running.
///
/// # Errors
/// Any [`TransportError`].
pub async fn list_all_networks<S>(session: &mut Session<S>) -> Result<Vec<Network>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_list_all_args(true, 0);
    let body = session.call(PROC_CONNECT_LIST_ALL_NETWORKS, &args).await?;
    decode_networks(&body)
}

/// A storage volume, as `remote_nonnull_storage_vol` appears on the wire:
/// three strings, and notably **no UUID** (unlike pools and networks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageVol {
    /// Name of the pool the volume belongs to.
    pub pool: String,
    /// Volume name.
    pub name: String,
    /// Backend-assigned key (for a directory pool, the file path).
    pub key: String,
}

impl StorageVol {
    /// Encode as `remote_nonnull_storage_vol`.
    fn encode(&self, e: &mut Encoder) {
        e.write_string(&self.pool);
        e.write_string(&self.name);
        e.write_string(&self.key);
    }

    /// Decode a `remote_nonnull_storage_vol`.
    pub(crate) fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            pool: d.read_string()?.to_string(),
            name: d.read_string()?.to_string(),
            key: d.read_string()?.to_string(),
        })
    }
}

/// Volume XML for a raw volume of `capacity_bytes`.
///
/// Raw rather than qcow2 is deliberate (ADR-0011): the artifact
/// `banlieue-imagebuilder` produces is already a raw disk, so uploading it
/// verbatim removes any need for `qemu-img` — and therefore for a
/// third-party tools image — anywhere in the pipeline.
pub fn raw_volume_xml(name: &str, capacity_bytes: u64) -> String {
    format!(
        "<volume type='file'>\
<name>{name}</name>\
<capacity unit='bytes'>{capacity_bytes}</capacity>\
<allocation unit='bytes'>0</allocation>\
<target><format type='raw'/></target>\
</volume>"
    )
}

/// Create a volume in `pool` from the given XML.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error typically means the volume already
/// exists or the pool is inactive.
pub async fn storage_vol_create_xml<S>(
    session: &mut Session<S>,
    pool: &StoragePool,
    xml: &str,
) -> Result<StorageVol>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut e = Encoder::new();
    // remote_nonnull_storage_pool: name then the raw 16-byte uuid.
    e.write_string(&pool.name);
    e.write_opaque_fixed(&pool.uuid);
    e.write_string(xml);
    e.write_u32(0); // flags
    let body = session
        .call(PROC_STORAGE_VOL_CREATE_XML, &e.into_bytes())
        .await?;
    let mut d = Decoder::new(&body);
    StorageVol::decode(&mut d)
}

/// Upload `length` bytes from `reader` into `vol`.
///
/// Implements libvirt's stream protocol as observed from a real
/// `virsh vol-upload`:
///
/// 1. `CALL` STORAGE_VOL_UPLOAD (vol, offset, length, flags).
/// 2. Wait for the server's `REPLY` — data must not be sent before it.
/// 3. Send `Stream`/`Continue` packets of at most [`STREAM_CHUNK_MAX`] raw
///    bytes each, reusing the call's procedure and serial.
/// 4. Send `Stream`/`Ok` with an empty payload and await the server's
///    matching confirmation.
///
/// Bytes are streamed from `reader` a chunk at a time, so memory use stays
/// bounded regardless of image size — the whole reason image transfer runs in
/// a Job rather than a reconcile loop (ADR-0011).
///
/// # Errors
/// Any [`TransportError`]; [`TransportError::Protocol`] if `reader` yields
/// fewer than `length` bytes.
pub async fn storage_vol_upload<S, R>(
    session: &mut Session<S>,
    vol: &StorageVol,
    reader: &mut R,
    length: u64,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let mut e = Encoder::new();
    vol.encode(&mut e);
    e.write_u64(0); // offset
    e.write_u64(length);
    e.write_u32(0); // flags

    // The serial is reused by every stream packet below.
    let (serial, _) = session
        .call_with_serial(PROC_STORAGE_VOL_UPLOAD, &e.into_bytes())
        .await?;

    let mut buf = vec![0u8; STREAM_CHUNK_MAX];
    let mut sent: u64 = 0;
    while sent < length {
        let want = std::cmp::min(STREAM_CHUNK_MAX as u64, length - sent) as usize;
        // read_exact rather than read: a short read mid-file is not EOF, and
        // silently sending a truncated image would corrupt the volume.
        reader
            .read_exact(&mut buf[..want])
            .await
            .map_err(|e| TransportError::Protocol {
                detail: format!("source ended after {sent} of {length} bytes: {e}"),
            })?;
        session
            .send_stream_data(PROC_STORAGE_VOL_UPLOAD, serial, &buf[..want])
            .await?;
        sent += want as u64;
    }

    session.finish_stream(PROC_STORAGE_VOL_UPLOAD, serial).await
}

/// List every volume in `pool`.
///
/// Used to confirm a `BackingFile` image source actually exists before
/// reporting it ready, and to make volume creation idempotent — a re-reconcile
/// must not fail because the volume it created last time is still there.
///
/// # Errors
/// Any [`TransportError`].
pub async fn storage_pool_list_all_volumes<S>(
    session: &mut Session<S>,
    pool: &StoragePool,
) -> Result<Vec<StorageVol>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut e = Encoder::new();
    e.write_string(&pool.name);
    e.write_opaque_fixed(&pool.uuid);
    e.write_i32(1); // need_results
    e.write_u32(0); // flags
    let body = session
        .call(PROC_STORAGE_POOL_LIST_ALL_VOLUMES, &e.into_bytes())
        .await?;
    let mut d = Decoder::new(&body);
    let count = read_checked_count(&mut d, STORAGE_VOL_LIST_MAX, "storage volume")?;
    let mut vols = Vec::with_capacity(count);
    for _ in 0..count {
        vols.push(StorageVol::decode(&mut d)?);
    }
    Ok(vols)
}

#[cfg(test)]
#[path = "procs_tests.rs"]
mod procs_tests;
