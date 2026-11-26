// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Fixed VHD (Microsoft Virtual Hard Disk Image Format Specification, v1.0).
//!
//! A fixed VHD is the raw disk followed by one 512-byte footer. Azure takes
//! only fixed VHDs, and only when the virtual size is a whole number of MiB,
//! so [`FixedVhdLayout`] pads the raw disk with zeros up to the next MiB
//! before the footer (ADR-0162 finding 1: auroraboot's raw disks are already
//! aligned, and then the padding is empty).
//!
//! [`FixedVhdReader`] streams `raw + padding + footer` from any
//! [`Read`], so a multi-gigabyte disk is never held in memory.
//!
//! All multi-byte footer fields are big-endian.

use std::io::{self, Read};

/// A disk sector.
pub const SECTOR_SIZE: u64 = 512;
/// One mebibyte: Azure's alignment for a VHD's virtual size.
pub const MIB: u64 = 1024 * 1024;
/// One gibibyte: the unit of a managed disk's size.
pub const GIB: u64 = 1024 * MIB;
/// The footer's length.
pub const FOOTER_LEN: usize = 512;
/// The footer's length as a byte count on disk.
pub const FOOTER_LEN_U64: u64 = 512;

/// The footer's magic.
pub const COOKIE: &[u8; 8] = b"conectix";
/// Features: the "reserved" bit, which the spec says must always be set.
pub const FEATURES_RESERVED: u32 = 0x0000_0002;
/// File format version 1.0.
pub const FORMAT_VERSION: u32 = 0x0001_0000;
/// Data offset of a fixed disk: there is no dynamic header.
pub const FIXED_DATA_OFFSET: u64 = u64::MAX;
/// Creator application: banlieue.
pub const CREATOR_APP: &[u8; 4] = b"banl";
/// Creator version (major 1, minor 0).
pub const CREATOR_VERSION: u32 = 0x0001_0000;
/// Creator host OS: `Wi2k`, the value Hyper-V and Azure write and accept.
pub const CREATOR_HOST_OS_WI2K: &[u8; 4] = b"Wi2k";
/// Disk type: fixed.
pub const DISK_TYPE_FIXED: u32 = 2;

/// Seconds from the Unix epoch to 2000-01-01T00:00:00Z, the VHD epoch.
pub const VHD_EPOCH_UNIX_SECS: u64 = 946_684_800;

// Footer field offsets (spec, "Hard Disk Footer Format").
const OFF_FEATURES: usize = 8;
const OFF_FORMAT_VERSION: usize = 12;
const OFF_DATA_OFFSET: usize = 16;
const OFF_TIMESTAMP: usize = 24;
const OFF_CREATOR_APP: usize = 28;
const OFF_CREATOR_VERSION: usize = 32;
const OFF_CREATOR_HOST_OS: usize = 36;
const OFF_ORIGINAL_SIZE: usize = 40;
const OFF_CURRENT_SIZE: usize = 48;
const OFF_CYLINDERS: usize = 56;
const OFF_HEADS: usize = 58;
const OFF_SECTORS_PER_TRACK: usize = 59;
const OFF_DISK_TYPE: usize = 60;
const OFF_CHECKSUM: usize = 64;
const OFF_UNIQUE_ID: usize = 68;
const OFF_SAVED_STATE: usize = 84;
const CHECKSUM_LEN: usize = 4;
const UNIQUE_ID_LEN: usize = 16;

// CHS geometry limits (spec, "CHS Calculation").
const MAX_CHS_SECTORS: u64 = 65_535 * 16 * 255;
const LARGE_DISK_SECTORS: u64 = 65_535 * 16 * 63;
const SPT_LARGE: u64 = 255;
const SPT_SMALL: u64 = 17;
const SPT_MEDIUM: u64 = 31;
const SPT_63: u64 = 63;
const HEADS_MAX: u64 = 16;
const HEADS_MIN: u64 = 4;
const CYLINDERS_PER_HEAD_LIMIT: u64 = 1024;
const MAX_CYLINDERS: u64 = 65_535;

/// What can go wrong laying out a VHD.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VhdError {
    /// A zero-length raw disk: publishing it would register an unbootable
    /// image.
    #[error("the raw disk is empty")]
    Empty,
    /// Padding the disk to a MiB, plus the footer, overflows `u64`.
    #[error("the raw disk of {0} bytes is too large for a VHD")]
    TooLarge(u64),
    /// A footer that is not a VHD footer.
    #[error("not a VHD footer: {0}")]
    InvalidFooter(String),
}

/// A disk's cylinder/head/sector geometry, as the footer records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Cylinders.
    pub cylinders: u16,
    /// Heads.
    pub heads: u8,
    /// Sectors per track.
    pub sectors_per_track: u8,
}

impl Geometry {
    /// The number of sectors the geometry addresses.
    #[must_use]
    pub fn sectors(self) -> u64 {
        u64::from(self.cylinders) * u64::from(self.heads) * u64::from(self.sectors_per_track)
    }
}

/// The CHS geometry of a disk of `size_bytes`, by the spec's algorithm
/// (the same one Hyper-V, `qemu-img` and `disk2vhd` use). Disks larger than
/// CHS can address get the maximum geometry; the footer's size field, not
/// the geometry, is the disk's size.
#[must_use]
pub fn chs_geometry(size_bytes: u64) -> Geometry {
    let total = (size_bytes / SECTOR_SIZE).min(MAX_CHS_SECTORS);
    let (spt, heads, cyl_times_heads) = if total >= LARGE_DISK_SECTORS {
        (SPT_LARGE, HEADS_MAX, total / SPT_LARGE)
    } else {
        let mut spt = SPT_SMALL;
        let mut cth = total / spt;
        let mut heads = cth.div_ceil(CYLINDERS_PER_HEAD_LIMIT).max(HEADS_MIN);
        if cth >= heads * CYLINDERS_PER_HEAD_LIMIT || heads > HEADS_MAX {
            spt = SPT_MEDIUM;
            heads = HEADS_MAX;
            cth = total / spt;
        }
        if cth >= heads * CYLINDERS_PER_HEAD_LIMIT {
            spt = SPT_63;
            heads = HEADS_MAX;
            cth = total / spt;
        }
        (spt, heads, cth)
    };
    let cylinders = (cyl_times_heads / heads).min(MAX_CYLINDERS);
    // Each value is bounded above by the constants it was clamped to.
    Geometry {
        cylinders: u16::try_from(cylinders).unwrap_or(u16::MAX),
        heads: u8::try_from(heads).unwrap_or(u8::MAX),
        sectors_per_track: u8::try_from(spt).unwrap_or(u8::MAX),
    }
}

/// Seconds since the VHD epoch (2000-01-01T00:00:00Z) for a Unix time;
/// zero before the epoch, saturating at `u32::MAX`.
#[must_use]
pub fn vhd_timestamp(unix_secs: u64) -> u32 {
    u32::try_from(unix_secs.saturating_sub(VHD_EPOCH_UNIX_SECS)).unwrap_or(u32::MAX)
}

/// The spec's checksum: the one's complement of the sum of every footer
/// byte, the checksum field itself excluded.
#[must_use]
pub fn footer_checksum(footer: &[u8; FOOTER_LEN]) -> u32 {
    let sum = footer
        .iter()
        .enumerate()
        .filter(|(i, _)| !(OFF_CHECKSUM..OFF_CHECKSUM + CHECKSUM_LEN).contains(i))
        .fold(0u32, |acc, (_, b)| acc.wrapping_add(u32::from(*b)));
    !sum
}

/// The footer of a fixed VHD whose virtual size is `size_bytes`.
///
/// `timestamp` is [`vhd_timestamp`]'s; `unique_id` identifies the disk (a
/// caller that wants the same bytes for the same source derives it from the
/// source's digest).
#[must_use]
pub fn fixed_footer(size_bytes: u64, timestamp: u32, unique_id: [u8; 16]) -> [u8; FOOTER_LEN] {
    let mut f = [0u8; FOOTER_LEN];
    let geometry = chs_geometry(size_bytes);
    f[..COOKIE.len()].copy_from_slice(COOKIE);
    put_u32(&mut f, OFF_FEATURES, FEATURES_RESERVED);
    put_u32(&mut f, OFF_FORMAT_VERSION, FORMAT_VERSION);
    put_u64(&mut f, OFF_DATA_OFFSET, FIXED_DATA_OFFSET);
    put_u32(&mut f, OFF_TIMESTAMP, timestamp);
    f[OFF_CREATOR_APP..OFF_CREATOR_APP + CREATOR_APP.len()].copy_from_slice(CREATOR_APP);
    put_u32(&mut f, OFF_CREATOR_VERSION, CREATOR_VERSION);
    f[OFF_CREATOR_HOST_OS..OFF_CREATOR_HOST_OS + CREATOR_HOST_OS_WI2K.len()]
        .copy_from_slice(CREATOR_HOST_OS_WI2K);
    put_u64(&mut f, OFF_ORIGINAL_SIZE, size_bytes);
    put_u64(&mut f, OFF_CURRENT_SIZE, size_bytes);
    f[OFF_CYLINDERS..OFF_CYLINDERS + 2].copy_from_slice(&geometry.cylinders.to_be_bytes());
    f[OFF_HEADS] = geometry.heads;
    f[OFF_SECTORS_PER_TRACK] = geometry.sectors_per_track;
    put_u32(&mut f, OFF_DISK_TYPE, DISK_TYPE_FIXED);
    f[OFF_UNIQUE_ID..OFF_UNIQUE_ID + UNIQUE_ID_LEN].copy_from_slice(&unique_id);
    f[OFF_SAVED_STATE] = 0;
    let checksum = footer_checksum(&f);
    put_u32(&mut f, OFF_CHECKSUM, checksum);
    f
}

fn put_u32(buf: &mut [u8; FOOTER_LEN], at: usize, value: u32) {
    buf[at..at + CHECKSUM_LEN].copy_from_slice(&value.to_be_bytes());
}

fn put_u64(buf: &mut [u8; FOOTER_LEN], at: usize, value: u64) {
    buf[at..at + size_of::<u64>()].copy_from_slice(&value.to_be_bytes());
}

fn get_u32(buf: &[u8; FOOTER_LEN], at: usize) -> u32 {
    let mut b = [0u8; CHECKSUM_LEN];
    b.copy_from_slice(&buf[at..at + CHECKSUM_LEN]);
    u32::from_be_bytes(b)
}

fn get_u64(buf: &[u8; FOOTER_LEN], at: usize) -> u64 {
    let mut b = [0u8; size_of::<u64>()];
    b.copy_from_slice(&buf[at..at + size_of::<u64>()]);
    u64::from_be_bytes(b)
}

/// The fields of a footer that a reader checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedFooter {
    /// Feature flags.
    pub features: u32,
    /// Format version.
    pub format_version: u32,
    /// Data offset; [`FIXED_DATA_OFFSET`] for a fixed disk.
    pub data_offset: u64,
    /// Seconds since 2000-01-01.
    pub timestamp: u32,
    /// Creator application.
    pub creator_app: [u8; 4],
    /// Creator host OS.
    pub creator_host_os: [u8; 4],
    /// Original size.
    pub original_size: u64,
    /// Current (virtual) size.
    pub current_size: u64,
    /// CHS geometry.
    pub geometry: Geometry,
    /// Disk type; [`DISK_TYPE_FIXED`] for a fixed disk.
    pub disk_type: u32,
    /// The stored checksum.
    pub checksum: u32,
    /// The disk's unique ID.
    pub unique_id: [u8; 16],
    /// Saved state flag.
    pub saved_state: u8,
}

/// Reads a footer back.
///
/// # Errors
/// [`VhdError::InvalidFooter`] when the cookie or checksum is wrong.
pub fn parse_footer(footer: &[u8; FOOTER_LEN]) -> Result<ParsedFooter, VhdError> {
    if &footer[..COOKIE.len()] != COOKIE {
        return Err(VhdError::InvalidFooter(
            "cookie is not conectix".to_string(),
        ));
    }
    let checksum = get_u32(footer, OFF_CHECKSUM);
    let computed = footer_checksum(footer);
    if checksum != computed {
        return Err(VhdError::InvalidFooter(format!(
            "checksum {checksum:#010x}, computed {computed:#010x}"
        )));
    }
    let mut creator_app = [0u8; 4];
    creator_app.copy_from_slice(&footer[OFF_CREATOR_APP..OFF_CREATOR_APP + CREATOR_APP.len()]);
    let mut creator_host_os = [0u8; 4];
    creator_host_os.copy_from_slice(
        &footer[OFF_CREATOR_HOST_OS..OFF_CREATOR_HOST_OS + CREATOR_HOST_OS_WI2K.len()],
    );
    let mut unique_id = [0u8; UNIQUE_ID_LEN];
    unique_id.copy_from_slice(&footer[OFF_UNIQUE_ID..OFF_UNIQUE_ID + UNIQUE_ID_LEN]);
    Ok(ParsedFooter {
        features: get_u32(footer, OFF_FEATURES),
        format_version: get_u32(footer, OFF_FORMAT_VERSION),
        data_offset: get_u64(footer, OFF_DATA_OFFSET),
        timestamp: get_u32(footer, OFF_TIMESTAMP),
        creator_app,
        creator_host_os,
        original_size: get_u64(footer, OFF_ORIGINAL_SIZE),
        current_size: get_u64(footer, OFF_CURRENT_SIZE),
        geometry: Geometry {
            cylinders: u16::from_be_bytes([footer[OFF_CYLINDERS], footer[OFF_CYLINDERS + 1]]),
            heads: footer[OFF_HEADS],
            sectors_per_track: footer[OFF_SECTORS_PER_TRACK],
        },
        disk_type: get_u32(footer, OFF_DISK_TYPE),
        checksum,
        unique_id,
        saved_state: footer[OFF_SAVED_STATE],
    })
}

/// Where each part of a fixed VHD sits, for a raw disk of a given length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedVhdLayout {
    /// The raw disk's length.
    pub raw_len: u64,
    /// The VHD's virtual size: `raw_len` rounded up to a whole MiB.
    pub virtual_size: u64,
    /// Zeros between the raw disk and the footer.
    pub padding: u64,
    /// The whole file: virtual size plus the footer. A managed disk's
    /// `uploadSizeBytes`.
    pub total_len: u64,
}

impl FixedVhdLayout {
    /// The layout for a raw disk of `raw_len` bytes.
    ///
    /// # Errors
    /// [`VhdError::Empty`] for zero; [`VhdError::TooLarge`] on overflow.
    pub fn for_raw_len(raw_len: u64) -> Result<Self, VhdError> {
        if raw_len == 0 {
            return Err(VhdError::Empty);
        }
        let virtual_size = raw_len
            .checked_next_multiple_of(MIB)
            .ok_or(VhdError::TooLarge(raw_len))?;
        let total_len = virtual_size
            .checked_add(FOOTER_LEN_U64)
            .ok_or(VhdError::TooLarge(raw_len))?;
        Ok(Self {
            raw_len,
            virtual_size,
            padding: virtual_size - raw_len,
            total_len,
        })
    }

    /// The smallest whole number of GiB that holds the virtual size: the
    /// published image's minimum OS disk size (ADR-0162 finding 7).
    #[must_use]
    pub fn min_disk_gib(&self) -> u64 {
        self.virtual_size.div_ceil(GIB)
    }
}

/// Whether `buf` holds only zero bytes: such a range of a page blob need
/// not be written, since an upload disk starts zeroed.
#[must_use]
pub fn is_all_zero(buf: &[u8]) -> bool {
    buf.iter().all(|b| *b == 0)
}

/// Streams a fixed VHD: the raw disk read from `R`, then zeros up to the
/// MiB, then the footer. Fails if the source is shorter or longer than the
/// layout says, so a disk that changed under the reader is never uploaded
/// as if it had not.
pub struct FixedVhdReader<R> {
    raw: R,
    layout: FixedVhdLayout,
    footer: [u8; FOOTER_LEN],
    position: u64,
}

impl<R: Read> FixedVhdReader<R> {
    /// A reader over `raw`, which must be exactly `layout.raw_len` long.
    pub fn new(raw: R, layout: FixedVhdLayout, footer: [u8; FOOTER_LEN]) -> Self {
        Self {
            raw,
            layout,
            footer,
            position: 0,
        }
    }

    /// The layout being streamed.
    #[must_use]
    pub fn layout(&self) -> FixedVhdLayout {
        self.layout
    }
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl<R: Read> Read for FixedVhdReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let FixedVhdLayout {
            raw_len,
            virtual_size,
            total_len,
            ..
        } = self.layout;
        if self.position < raw_len {
            let want = buf.len().min(to_usize(raw_len - self.position));
            let n = self.raw.read(&mut buf[..want])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "raw disk ended at {} bytes, expected {raw_len}",
                        self.position
                    ),
                ));
            }
            self.position += n as u64;
            if self.position == raw_len {
                let mut probe = [0u8; 1];
                if self.raw.read(&mut probe)? != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("raw disk is longer than {raw_len} bytes"),
                    ));
                }
            }
            return Ok(n);
        }
        if self.position < virtual_size {
            let n = buf.len().min(to_usize(virtual_size - self.position));
            buf[..n].fill(0);
            self.position += n as u64;
            return Ok(n);
        }
        if self.position < total_len {
            let start = to_usize(self.position - virtual_size);
            let n = buf.len().min(FOOTER_LEN - start);
            buf[..n].copy_from_slice(&self.footer[start..start + n]);
            self.position += n as u64;
            return Ok(n);
        }
        Ok(0)
    }
}

#[cfg(test)]
#[path = "vhd_tests.rs"]
mod vhd_tests;
