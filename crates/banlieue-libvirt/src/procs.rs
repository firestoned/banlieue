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
    PROC_CONNECT_OPEN, PROC_DOMAIN_CREATE_WITH_FLAGS, PROC_DOMAIN_DEFINE_XML_FLAGS,
    PROC_DOMAIN_DESTROY, PROC_DOMAIN_GET_STATE, PROC_DOMAIN_INTERFACE_ADDRESSES,
    PROC_DOMAIN_LOOKUP_BY_NAME, PROC_DOMAIN_SHUTDOWN, PROC_DOMAIN_UNDEFINE_FLAGS,
    PROC_NETWORK_GET_DHCP_LEASES, PROC_STORAGE_POOL_LIST_ALL_VOLUMES,
    PROC_STORAGE_POOL_LOOKUP_BY_NAME, PROC_STORAGE_POOL_REFRESH, PROC_STORAGE_VOL_CREATE_XML,
    PROC_STORAGE_VOL_DELETE, PROC_STORAGE_VOL_LOOKUP_BY_NAME, PROC_STORAGE_VOL_UPLOAD,
    STREAM_CHUNK_MAX,
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

/// `VIR_ERR_NO_DOMAIN` — domain not found.
pub const VIR_ERR_NO_DOMAIN: i32 = 42;
/// `VIR_ERR_NO_STORAGE_POOL` — storage pool not found.
pub const VIR_ERR_NO_STORAGE_POOL: i32 = 49;
/// `VIR_ERR_NO_STORAGE_VOL` — storage volume not found.
pub const VIR_ERR_NO_STORAGE_VOL: i32 = 50;

/// Whether `err` means "that object does not exist".
///
/// libvirt answers a lookup for a missing object with an error reply rather
/// than an empty one, so every "does this already exist?" question a
/// reconciler asks goes through here. The codes are transcribed from
/// `virterror.h`; anything else stays a failure, because reading a permission
/// error as "absent" would make a reconciler recreate something that is
/// already there.
#[must_use]
pub fn is_not_found(err: &TransportError) -> bool {
    matches!(
        err,
        TransportError::Remote { code, .. }
            if *code == VIR_ERR_NO_DOMAIN
                || *code == VIR_ERR_NO_STORAGE_POOL
                || *code == VIR_ERR_NO_STORAGE_VOL
    )
}

/// Longest volume name banlieue will construct or accept.
///
/// Comfortably above anything generated from a Kubernetes name
/// (`<namespace>-<name>-<disk>.qcow2`) and well under any filesystem's own
/// limit, so the failure is ours and legible rather than the kernel's.
pub const VOLUME_NAME_MAX: usize = 200;

/// Check that `name` is safe to place in volume XML and to use as a filename
/// inside a pool.
///
/// An allowlist rather than an escaper, deliberately. Volume names are
/// banlieue-generated from Kubernetes object names, which are already
/// DNS-1123-restricted, so anything outside this set means something
/// constructed the name wrongly — and a *mangled but accepted* name creates a
/// file nobody can look up again, which is worse than a loud failure. It also
/// closes path traversal, which escaping would not.
///
/// # Errors
/// [`TransportError::Protocol`] describing what was wrong with the name.
pub fn validate_volume_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(TransportError::Protocol {
            detail: "volume name is empty".to_string(),
        });
    }
    if name.len() > VOLUME_NAME_MAX {
        return Err(TransportError::Protocol {
            detail: format!(
                "volume name is {} characters, over the {VOLUME_NAME_MAX} limit",
                name.len()
            ),
        });
    }
    // `.` is allowed inside a name (extensions), but a name that *is* `.` or
    // `..` names a directory rather than a volume.
    if name == "." || name == ".." || name.starts_with("../") {
        return Err(TransportError::Protocol {
            detail: format!("volume name {name:?} is a path, not a name"),
        });
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(TransportError::Protocol {
            detail: format!(
                "volume name {name:?} contains {bad:?}; only ASCII letters, \
                 digits, '-', '_' and '.' are allowed"
            ),
        });
    }
    Ok(())
}

/// Check that `path` is safe to place in volume XML.
///
/// Paths legitimately contain `/`, so they cannot use
/// [`validate_volume_name`]'s allowlist. What they cannot contain is anything
/// that would terminate the XML element they sit in, or a character XML 1.0
/// cannot represent.
fn validate_volume_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(TransportError::Protocol {
            detail: "volume path is empty".to_string(),
        });
    }
    if let Some(bad) = path
        .chars()
        .find(|&c| matches!(c, '<' | '>' | '&' | '\'' | '"') || c.is_control())
    {
        return Err(TransportError::Protocol {
            detail: format!("volume path {path:?} contains {bad:?}, which cannot appear in XML"),
        });
    }
    Ok(())
}

/// Volume XML for an **empty** qcow2 volume of `capacity_bytes`.
///
/// The `Deferred` install shape (ADR-0040, ADR-0050): the guest installs
/// itself onto this disk from an attached ISO, so it starts with nothing on
/// it. qcow2 rather than raw because the disk is sparse until the install
/// writes to it, and a pool full of fully-allocated empty sandbox disks is
/// the difference between a warm pool fitting on a host and not.
///
/// # Errors
/// [`TransportError::Protocol`] if `name` is not a valid volume name.
pub fn qcow2_volume_xml(name: &str, capacity_bytes: u64) -> Result<String> {
    validate_volume_name(name)?;
    Ok(format!(
        "<volume type='file'>\
<name>{name}</name>\
<capacity unit='bytes'>{capacity_bytes}</capacity>\
<allocation unit='bytes'>0</allocation>\
<target><format type='qcow2'/></target>\
</volume>"
    ))
}

/// Volume XML for a qcow2 **overlay** over `backing_path`.
///
/// The `Immediate` install shape: the OS is already in the backing image and
/// this volume holds only what the VM changes.
///
/// `backing_format` is not optional and is never guessed. libvirt does not
/// probe a backing file's format, and an undeclared one is both a long-
/// standing security advisory (a raw image whose *contents* are a qcow2
/// header gets reinterpreted, reaching files the guest should not) and a hard
/// refusal on current libvirt. The caller knows the format — for banlieue's
/// own uploaded artifacts it is `raw` (ADR-0011) — so it passes it.
///
/// # Errors
/// [`TransportError::Protocol`] if `name` is not a valid volume name, or
/// `backing_path` contains characters that cannot appear in XML.
pub fn qcow2_overlay_volume_xml(
    name: &str,
    capacity_bytes: u64,
    backing_path: &str,
    backing_format: &str,
) -> Result<String> {
    validate_volume_name(name)?;
    validate_volume_path(backing_path)?;
    validate_volume_path(backing_format)?;
    Ok(format!(
        "<volume type='file'>\
<name>{name}</name>\
<capacity unit='bytes'>{capacity_bytes}</capacity>\
<allocation unit='bytes'>0</allocation>\
<target><format type='qcow2'/></target>\
<backingStore>\
<path>{backing_path}</path>\
<format type='{backing_format}'/>\
</backingStore>\
</volume>"
    ))
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

/// Encode `remote_storage_pool_lookup_by_name_args { string name; }`.
#[must_use]
pub fn encode_storage_pool_lookup_by_name_args(name: &str) -> Vec<u8> {
    let mut e = Encoder::new();
    e.write_string(name);
    e.into_bytes()
}

/// Encode `remote_storage_vol_lookup_by_name_args { pool; string name; }`.
#[must_use]
pub fn encode_storage_vol_lookup_by_name_args(pool: &StoragePool, name: &str) -> Vec<u8> {
    let mut e = Encoder::new();
    // `remote_nonnull_storage_pool` is name + uuid, with no trailing id —
    // unlike the domain handle. An extra field here shifts `name`.
    e.write_string(&pool.name);
    e.write_opaque_fixed(&pool.uuid);
    e.write_string(name);
    e.into_bytes()
}

/// Encode `remote_storage_vol_delete_args { vol; uint flags; }`.
#[must_use]
pub fn encode_storage_vol_delete_args(vol: &StorageVol) -> Vec<u8> {
    let mut e = Encoder::new();
    vol.encode(&mut e);
    e.write_u32(0); // flags: normal delete, not zeroed
    e.into_bytes()
}

/// Encode `remote_storage_pool_refresh_args { pool; uint flags; }`.
#[must_use]
pub fn encode_storage_pool_refresh_args(pool: &StoragePool) -> Vec<u8> {
    let mut e = Encoder::new();
    e.write_string(&pool.name);
    e.write_opaque_fixed(&pool.uuid);
    e.write_u32(0);
    e.into_bytes()
}

/// Decode a reply whose whole body is one `remote_nonnull_storage_pool`.
///
/// # Errors
/// [`TransportError::Protocol`] if the payload is short or malformed.
pub fn decode_storage_pool_ret(body: &[u8]) -> Result<StoragePool> {
    let mut d = Decoder::new(body);
    let name = d.read_string()?.to_string();
    let mut uuid = [0u8; UUID_LEN];
    uuid.copy_from_slice(d.read_opaque_fixed(UUID_LEN)?);
    Ok(StoragePool { name, uuid })
}

/// Decode a reply whose whole body is one `remote_nonnull_storage_vol`.
///
/// # Errors
/// [`TransportError::Protocol`] if the payload is short or malformed.
pub fn decode_storage_vol_ret(body: &[u8]) -> Result<StorageVol> {
    let mut d = Decoder::new(body);
    StorageVol::decode(&mut d)
}

/// Look up a storage pool by name.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error means no such pool.
pub async fn storage_pool_lookup_by_name<S>(
    session: &mut Session<S>,
    name: &str,
) -> Result<StoragePool>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_storage_pool_lookup_by_name_args(name);
    let body = session
        .call(PROC_STORAGE_POOL_LOOKUP_BY_NAME, &args)
        .await?;
    decode_storage_pool_ret(&body)
}

/// Look up a volume by name within `pool`.
///
/// The returned [`StorageVol::key`] is the volume's absolute path on the host
/// for a directory pool — which is what domain XML needs, so nothing has to
/// parse the pool's own XML to find where its volumes live.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error means no such volume, which is
/// how a caller tests for existence.
pub async fn storage_vol_lookup_by_name<S>(
    session: &mut Session<S>,
    pool: &StoragePool,
    name: &str,
) -> Result<StorageVol>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_storage_vol_lookup_by_name_args(pool, name);
    let body = session.call(PROC_STORAGE_VOL_LOOKUP_BY_NAME, &args).await?;
    decode_storage_vol_ret(&body)
}

/// Delete a volume.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error if the volume is gone already.
pub async fn storage_vol_delete<S>(session: &mut Session<S>, vol: &StorageVol) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_storage_vol_delete_args(vol);
    session.call(PROC_STORAGE_VOL_DELETE, &args).await?;
    Ok(())
}

/// Re-scan a pool's backing directory.
///
/// Called defensively after creating a volume: a pool does not always notice
/// a new file on its own, and a subsequent lookup that should succeed then
/// returns "no such volume" for no visible reason.
///
/// # Errors
/// Any [`TransportError`].
pub async fn storage_pool_refresh<S>(session: &mut Session<S>, pool: &StoragePool) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_storage_pool_refresh_args(pool);
    session.call(PROC_STORAGE_POOL_REFRESH, &args).await?;
    Ok(())
}

/// `REMOTE_NETWORK_DHCP_LEASES_MAX`.
pub const NETWORK_DHCP_LEASES_MAX: usize = 65536;

/// One DHCP lease a libvirt-managed network has issued.
///
/// The interesting field is `hostname`: it is the name the **guest**
/// announced in its DHCP request, so a lease carrying one is evidence that
/// the guest's own configuration ran — which is what makes this readable
/// proof that a cloud-init seed was consumed (ADR-0054).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpLease {
    /// Host-side interface the lease was issued on, e.g. `virbr0`.
    pub iface: String,
    /// The leased address.
    pub ipaddr: String,
    /// Prefix length in bits.
    pub prefix: u32,
    /// MAC the lease belongs to, when the server recorded one.
    pub mac: Option<String>,
    /// Hostname the guest announced. `None` when it sent none.
    pub hostname: Option<String>,
}

/// Encode `remote_network_get_dhcp_leases_args`.
///
/// `mac` is a pointer type, so it is an optional: a boolean, then the
/// string only when `Some`. Getting that wrong shifts `need_results` and
/// `flags` after it.
#[must_use]
pub fn encode_network_get_dhcp_leases_args(net: &Network, mac: Option<&str>) -> Vec<u8> {
    let mut e = Encoder::new();
    e.write_string(&net.name);
    e.write_opaque_fixed(&net.uuid);
    match mac {
        Some(m) => {
            e.write_bool(true);
            e.write_string(m);
        }
        None => e.write_bool(false),
    }
    e.write_i32(1); // need_results
    e.write_u32(0); // flags
    e.into_bytes()
}

/// Decode `remote_network_get_dhcp_leases_ret`.
///
/// # Errors
/// [`TransportError::Protocol`] on a short payload or an over-large count.
pub fn decode_network_get_dhcp_leases_ret(body: &[u8]) -> Result<Vec<DhcpLease>> {
    let mut d = Decoder::new(body);
    let count = read_checked_count(&mut d, NETWORK_DHCP_LEASES_MAX, "dhcp lease")?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let iface = d.read_string()?.to_string();
        let _expiry = d.read_i64()?;
        let _type = d.read_i32()?;
        let mac = optional_string(&mut d)?;
        let _iaid = optional_string(&mut d)?;
        let ipaddr = d.read_string()?.to_string();
        let prefix = d.read_u32()?;
        let hostname = optional_string(&mut d)?;
        let _clientid = optional_string(&mut d)?;
        out.push(DhcpLease {
            iface,
            ipaddr,
            prefix,
            mac,
            hostname,
        });
    }
    Ok(out)
}

/// Read a `remote_string` — a pointer type, so a boolean then the value.
fn optional_string(d: &mut Decoder<'_>) -> Result<Option<String>> {
    if d.read_bool()? {
        Ok(Some(d.read_string()?.to_string()))
    } else {
        Ok(None)
    }
}

/// Every DHCP lease `net` has issued.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error if the network has no lease
/// database, which is the case for a bridge rather than a managed network.
pub async fn network_get_dhcp_leases<S>(
    session: &mut Session<S>,
    net: &Network,
    mac: Option<&str>,
) -> Result<Vec<DhcpLease>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_network_get_dhcp_leases_args(net, mac);
    let body = session.call(PROC_NETWORK_GET_DHCP_LEASES, &args).await?;
    decode_network_get_dhcp_leases_ret(&body)
}

// ─────────────────────────────────────────────────────────────────────────
// Domains (ADR-0050)
// ─────────────────────────────────────────────────────────────────────────

/// `REMOTE_DOMAIN_INTERFACE_MAX`.
pub const DOMAIN_INTERFACE_MAX: usize = 2048;

/// `REMOTE_DOMAIN_IP_ADDR_MAX`.
pub const DOMAIN_IP_ADDR_MAX: usize = 2048;

/// `VIR_DOMAIN_UNDEFINE_MANAGED_SAVE` — also discard a managed-save image.
pub const DOMAIN_UNDEFINE_MANAGED_SAVE: u32 = 1 << 0;

/// `VIR_DOMAIN_UNDEFINE_NVRAM` — also delete the UEFI varstore.
pub const DOMAIN_UNDEFINE_NVRAM: u32 = 1 << 2;

/// `VIR_DOMAIN_UNDEFINE_TPM` — also delete the emulated TPM's state.
pub const DOMAIN_UNDEFINE_TPM: u32 = 1 << 5;

/// The undefine flags banlieue always passes (ADR-0050).
///
/// Unconditional, not "when the domain is EFI" or "when it has a vTPM": the
/// flags are no-ops when there is nothing to remove, and the failure mode of
/// omitting one is silent. A single-use sandbox that leaves its varstore and
/// swtpm state behind has leaked the very per-VM secret material its vTPM
/// existed to isolate, and the leak is invisible until the datastore fills.
pub const DOMAIN_UNDEFINE_EPHEMERAL: u32 =
    DOMAIN_UNDEFINE_MANAGED_SAVE | DOMAIN_UNDEFINE_NVRAM | DOMAIN_UNDEFINE_TPM;

/// `VIR_DOMAIN_NOSTATE`.
pub const DOMAIN_STATE_NOSTATE: i32 = 0;
/// `VIR_DOMAIN_RUNNING`.
pub const DOMAIN_STATE_RUNNING: i32 = 1;
/// `VIR_DOMAIN_BLOCKED`.
pub const DOMAIN_STATE_BLOCKED: i32 = 2;
/// `VIR_DOMAIN_PAUSED`.
pub const DOMAIN_STATE_PAUSED: i32 = 3;
/// `VIR_DOMAIN_SHUTDOWN` — shutting *down*, not down.
pub const DOMAIN_STATE_SHUTDOWN: i32 = 4;
/// `VIR_DOMAIN_SHUTOFF`.
pub const DOMAIN_STATE_SHUTOFF: i32 = 5;
/// `VIR_DOMAIN_CRASHED`.
pub const DOMAIN_STATE_CRASHED: i32 = 6;
/// `VIR_DOMAIN_PMSUSPENDED`.
pub const DOMAIN_STATE_PMSUSPENDED: i32 = 7;

/// Where `domain_interface_addresses` should look (`VIR_DOMAIN_INTERFACE_
/// ADDRESSES_SRC_*`).
///
/// Ordered by authority, not by convenience: the agent knows what the guest
/// actually configured, a lease only records what DHCP offered, and ARP is an
/// inference from traffic. ADR-0050 Decision 8 tries them in that order and
/// records which one answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum InterfaceAddressSource {
    /// Parse the DHCP lease file. Libvirt-managed networks only.
    Lease = 0,
    /// Query `qemu-guest-agent` inside the guest.
    Agent = 1,
    /// Read the host ARP table. Best effort, and blind until the guest talks.
    Arp = 2,
}

/// A domain handle — `remote_nonnull_domain { string name; uuid; int id; }`.
///
/// Every domain procedure takes one of these, and `DOMAIN_LOOKUP_BY_NAME` /
/// `DOMAIN_DEFINE_XML_FLAGS` are how you get one. Note the trailing `id`,
/// which the storage-pool handle does not have: `-1` for an inactive domain,
/// otherwise the live domain ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    /// Domain name, unique per host.
    pub name: String,
    /// Raw 16-byte UUID. Stable across restarts; the domain's real identity.
    pub uuid: [u8; UUID_LEN],
    /// Live domain ID, or `-1` when the domain is not running.
    pub id: i32,
}

impl Domain {
    /// Encode as `remote_nonnull_domain`.
    fn encode(&self, e: &mut Encoder) {
        e.write_string(&self.name);
        e.write_opaque_fixed(&self.uuid);
        e.write_i32(self.id);
    }

    /// Decode a `remote_nonnull_domain`.
    pub(crate) fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        let name = d.read_string()?.to_string();
        let mut uuid = [0u8; UUID_LEN];
        uuid.copy_from_slice(d.read_opaque_fixed(UUID_LEN)?);
        Ok(Self {
            name,
            uuid,
            id: d.read_i32()?,
        })
    }
}

/// A domain's run state (`virDomainState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainState {
    /// No state reported.
    NoState,
    /// Executing.
    Running,
    /// Executing but blocked on a resource.
    Blocked,
    /// Paused by the user.
    Paused,
    /// Being shut down — still executing.
    ShuttingDown,
    /// Not executing.
    ShutOff,
    /// Crashed.
    Crashed,
    /// Suspended by the guest's own power management.
    PmSuspended,
    /// A state this client does not know. Carried through rather than
    /// guessed at, so a newer libvirt shows up in logs instead of being
    /// silently read as something it is not.
    Unknown(i32),
}

impl DomainState {
    fn from_wire(v: i32) -> Self {
        match v {
            DOMAIN_STATE_NOSTATE => Self::NoState,
            DOMAIN_STATE_RUNNING => Self::Running,
            DOMAIN_STATE_BLOCKED => Self::Blocked,
            DOMAIN_STATE_PAUSED => Self::Paused,
            DOMAIN_STATE_SHUTDOWN => Self::ShuttingDown,
            DOMAIN_STATE_SHUTOFF => Self::ShutOff,
            DOMAIN_STATE_CRASHED => Self::Crashed,
            DOMAIN_STATE_PMSUSPENDED => Self::PmSuspended,
            other => Self::Unknown(other),
        }
    }

    /// Whether the domain still occupies host resources.
    ///
    /// Deliberately conservative: only [`ShutOff`](Self::ShutOff) and
    /// [`Crashed`](Self::Crashed) count as stopped. `ShuttingDown` is an
    /// operation in flight, and an unknown state is treated as stopped
    /// because a reconciler that undefines on "not running" must never act
    /// on a state it could not interpret.
    #[must_use]
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Running | Self::Blocked | Self::Paused | Self::ShuttingDown | Self::PmSuspended
        )
    }
}

/// One address on a guest interface (`remote_domain_ip_addr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainIpAddr {
    /// `VIR_IP_ADDR_TYPE_IPV4` (0) or `IPV6` (1).
    pub kind: i32,
    /// The address in its textual form.
    pub addr: String,
    /// Prefix length in bits.
    pub prefix: u32,
}

/// A guest network interface as reported by `DOMAIN_INTERFACE_ADDRESSES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainInterface {
    /// Interface name as the *source* names it: the guest's name (`enp1s0`)
    /// from the agent, but the host-side tap name from a lease or ARP.
    pub name: String,
    /// MAC address. Optional on the wire (`remote_string`), and genuinely
    /// absent for some sources.
    pub hwaddr: Option<String>,
    /// Addresses currently configured on the interface.
    pub addrs: Vec<DomainIpAddr>,
}

/// Encode `remote_domain_lookup_by_name_args { remote_nonnull_string name; }`.
#[must_use]
pub fn encode_domain_lookup_by_name_args(name: &str) -> Vec<u8> {
    let mut e = Encoder::new();
    e.write_string(name);
    e.into_bytes()
}

/// Encode `remote_domain_define_xml_flags_args { string xml; uint flags; }`.
#[must_use]
pub fn encode_domain_define_xml_flags_args(xml: &str, flags: u32) -> Vec<u8> {
    let mut e = Encoder::new();
    e.write_string(xml);
    e.write_u32(flags);
    e.into_bytes()
}

/// Encode the `{ remote_nonnull_domain dom; }` argument struct shared by
/// `DOMAIN_DESTROY` and `DOMAIN_SHUTDOWN`.
#[must_use]
pub fn encode_domain_args(dom: &Domain) -> Vec<u8> {
    let mut e = Encoder::new();
    dom.encode(&mut e);
    e.into_bytes()
}

/// Encode the `{ remote_nonnull_domain dom; uint flags; }` argument struct
/// shared by `DOMAIN_CREATE_WITH_FLAGS`, `DOMAIN_UNDEFINE_FLAGS` and
/// `DOMAIN_GET_STATE`.
#[must_use]
pub fn encode_domain_flags_args(dom: &Domain, flags: u32) -> Vec<u8> {
    let mut e = Encoder::new();
    dom.encode(&mut e);
    e.write_u32(flags);
    e.into_bytes()
}

/// Encode `remote_domain_interface_addresses_args { dom; uint source; uint
/// flags; }`.
#[must_use]
pub fn encode_domain_interface_addresses_args(
    dom: &Domain,
    source: InterfaceAddressSource,
    flags: u32,
) -> Vec<u8> {
    let mut e = Encoder::new();
    dom.encode(&mut e);
    e.write_u32(source as u32);
    e.write_u32(flags);
    e.into_bytes()
}

/// Decode a reply whose whole body is one `remote_nonnull_domain`.
///
/// # Errors
/// [`TransportError::Protocol`] if the payload is short or malformed.
pub fn decode_domain_ret(body: &[u8]) -> Result<Domain> {
    let mut d = Decoder::new(body);
    Domain::decode(&mut d)
}

/// Decode `remote_domain_get_state_ret { int state; int reason; }`.
///
/// The `reason` is read (it must be, to consume the payload) but not
/// returned: libvirt's reason codes are per-state enumerations with no
/// stable cross-state meaning, and nothing in banlieue branches on one.
///
/// # Errors
/// [`TransportError::Protocol`] if the payload is short.
pub fn decode_domain_get_state_ret(body: &[u8]) -> Result<DomainState> {
    let mut d = Decoder::new(body);
    let state = d.read_i32()?;
    let _reason = d.read_i32()?;
    Ok(DomainState::from_wire(state))
}

/// Decode `remote_domain_interface_addresses_ret`.
///
/// Both array counts arrive from the network and are checked against
/// libvirt's own declared maxima before anything is allocated.
///
/// # Errors
/// [`TransportError::Protocol`] on a short payload or an over-large count.
pub fn decode_domain_interface_addresses_ret(body: &[u8]) -> Result<Vec<DomainInterface>> {
    let mut d = Decoder::new(body);
    let count = read_checked_count(&mut d, DOMAIN_INTERFACE_MAX, "domain interface")?;
    let mut ifaces = Vec::with_capacity(count);
    for _ in 0..count {
        let name = d.read_string()?.to_string();
        // `remote_string hwaddr` is a pointer type: a boolean, then the
        // string only when that boolean is true.
        let hwaddr = if d.read_bool()? {
            Some(d.read_string()?.to_string())
        } else {
            None
        };
        let n_addrs = read_checked_count(&mut d, DOMAIN_IP_ADDR_MAX, "domain interface address")?;
        let mut addrs = Vec::with_capacity(n_addrs);
        for _ in 0..n_addrs {
            addrs.push(DomainIpAddr {
                kind: d.read_i32()?,
                addr: d.read_string()?.to_string(),
                prefix: d.read_u32()?,
            });
        }
        ifaces.push(DomainInterface {
            name,
            hwaddr,
            addrs,
        });
    }
    Ok(ifaces)
}

/// Look up a domain by name.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error means no such domain.
pub async fn domain_lookup_by_name<S>(session: &mut Session<S>, name: &str) -> Result<Domain>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_lookup_by_name_args(name);
    let body = session.call(PROC_DOMAIN_LOOKUP_BY_NAME, &args).await?;
    decode_domain_ret(&body)
}

/// Define (persist) a domain from XML, without starting it.
///
/// Idempotent in libvirt's own terms: defining over an existing domain of the
/// same name replaces its configuration rather than failing, which is what
/// lets a reconciler converge instead of branching on existence.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error typically means invalid XML.
pub async fn domain_define_xml<S>(session: &mut Session<S>, xml: &str) -> Result<Domain>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_define_xml_flags_args(xml, 0);
    let body = session.call(PROC_DOMAIN_DEFINE_XML_FLAGS, &args).await?;
    decode_domain_ret(&body)
}

/// Start a defined domain.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error if the domain is already running.
pub async fn domain_create<S>(session: &mut Session<S>, dom: &Domain) -> Result<Domain>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_flags_args(dom, 0);
    let body = session.call(PROC_DOMAIN_CREATE_WITH_FLAGS, &args).await?;
    decode_domain_ret(&body)
}

/// Request a graceful ACPI shutdown. The guest may ignore it.
///
/// # Errors
/// Any [`TransportError`].
pub async fn domain_shutdown<S>(session: &mut Session<S>, dom: &Domain) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_args(dom);
    session.call(PROC_DOMAIN_SHUTDOWN, &args).await?;
    Ok(())
}

/// Force a domain off. The virtual equivalent of pulling the cord.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error if the domain is not running.
pub async fn domain_destroy<S>(session: &mut Session<S>, dom: &Domain) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_args(dom);
    session.call(PROC_DOMAIN_DESTROY, &args).await?;
    Ok(())
}

/// Undefine a domain, always with [`DOMAIN_UNDEFINE_EPHEMERAL`].
///
/// There is deliberately no flags parameter. Every domain banlieue creates is
/// disposable, so there is no caller that should be able to keep NVRAM or TPM
/// state alive past the domain — and making that a parameter is how it
/// eventually gets passed as `0`.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error if the domain is still running
/// (call [`domain_destroy`] first).
pub async fn domain_undefine<S>(session: &mut Session<S>, dom: &Domain) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_flags_args(dom, DOMAIN_UNDEFINE_EPHEMERAL);
    session.call(PROC_DOMAIN_UNDEFINE_FLAGS, &args).await?;
    Ok(())
}

/// Read a domain's current run state.
///
/// # Errors
/// Any [`TransportError`].
pub async fn domain_get_state<S>(session: &mut Session<S>, dom: &Domain) -> Result<DomainState>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_flags_args(dom, 0);
    let body = session.call(PROC_DOMAIN_GET_STATE, &args).await?;
    decode_domain_get_state_ret(&body)
}

/// List a domain's interfaces and their addresses, from one source.
///
/// Callers pick the source; see [`InterfaceAddressSource`] for why the order
/// matters. A source that cannot answer returns a `Remote` error rather than
/// an empty list, so a caller falling back must treat the error as "try the
/// next source", not as fatal.
///
/// # Errors
/// Any [`TransportError`]; a `Remote` error when the source is unavailable
/// (no guest agent, no lease file, domain not running).
pub async fn domain_interface_addresses<S>(
    session: &mut Session<S>,
    dom: &Domain,
    source: InterfaceAddressSource,
) -> Result<Vec<DomainInterface>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let args = encode_domain_interface_addresses_args(dom, source, 0);
    let body = session.call(PROC_DOMAIN_INTERFACE_ADDRESSES, &args).await?;
    decode_domain_interface_addresses_ret(&body)
}

#[cfg(test)]
#[path = "procs_tests.rs"]
mod procs_tests;
