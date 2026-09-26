// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A minimal ISO9660 writer with a Joliet supplementary descriptor
//! (ADR-0054).
//!
//! Scoped to exactly one job: a cloud-init NoCloud seed. Root directory
//! only, a handful of small files, no boot record, no Rock Ridge. That is
//! the entire shape a seed ever has, and refusing to generalise is what
//! keeps this small enough to reason about — a general ISO9660 writer is a
//! filesystem implementation, this is a few hundred lines of byte layout.
//!
//! # Why Joliet is not optional
//!
//! cloud-init looks for `user-data` and `meta-data`. ISO9660 Level 1 names
//! are 8.3, uppercase, from a d-character set that **excludes the hyphen**,
//! so `meta-data` — nine characters, hyphenated — cannot be written in the
//! primary tree at all. It appears there as `META_DATA.`, which cloud-init
//! does not match. The Joliet supplementary tree carries the real name in
//! UCS-2BE, and Linux prefers it when mounting. That is why the documented
//! `genisoimage` invocation passes `-joliet`.
//!
//! # Layout
//!
//! ```text
//!   0..16  system area (zeroed)
//!      16  primary volume descriptor
//!      17  supplementary volume descriptor (Joliet)
//!      18  volume descriptor set terminator
//!      19  path table, primary, little endian
//!      20  path table, primary, big endian
//!      21  path table, Joliet, little endian
//!      22  path table, Joliet, big endian
//!      23  root directory, primary
//!      24  root directory, Joliet
//!     25..  file extents, one sector-aligned run each
//! ```

/// ISO9660 sector size. Fixed by the standard.
const SECTOR: usize = 2048;
/// Sectors reserved before the first volume descriptor.
const SYSTEM_AREA_SECTORS: usize = 16;
/// Longest volume identifier the descriptor field holds.
const LABEL_MAX: usize = 32;
/// Longest name accepted in the primary tree, after mapping. Level 1 is
/// stricter still, but a seed's names are short and the extra room costs
/// nothing.
const PRIMARY_NAME_MAX: usize = 30;

const LBA_PVD: u32 = 16;
const LBA_SVD: u32 = 17;
const LBA_TERMINATOR: u32 = 18;
const LBA_PATH_L: u32 = 19;
const LBA_PATH_M: u32 = 20;
const LBA_PATH_L_JOLIET: u32 = 21;
const LBA_PATH_M_JOLIET: u32 = 22;
const LBA_ROOT: u32 = 23;
const LBA_ROOT_JOLIET: u32 = 24;
const LBA_FIRST_FILE: u32 = 25;

/// Volume descriptor date fields: offset of each 17-byte field (16 ASCII
/// digits, then a GMT offset byte).
const PVD_CREATION_DATE: usize = 813;
const PVD_MODIFICATION_DATE: usize = 830;
const PVD_EXPIRATION_DATE: usize = 847;
const PVD_EFFECTIVE_DATE: usize = 864;
/// Digits in a volume descriptor date, before the GMT offset byte.
const DATE_DIGITS: usize = 16;
/// 1970-01-01 00:00:00.00 — fixed so identical inputs give identical bytes.
const FIXED_VOLUME_DATE: &[u8; DATE_DIGITS] = b"1970010100000000";

/// Why a seed image could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IsoError {
    /// A file name contained a path separator. This writer is flat by
    /// design; flattening silently would put the file somewhere the caller
    /// did not ask for.
    #[error("file name {name:?} contains a path separator; this writer is flat (ADR-0054)")]
    NestedPath {
        /// The offending name.
        name: String,
    },

    /// A file name was empty.
    #[error("file name is empty")]
    EmptyName,

    /// A name did not fit the primary tree's directory record.
    #[error("file name {name:?} is {len} characters, over the {PRIMARY_NAME_MAX} limit")]
    NameTooLong {
        /// The offending name.
        name: String,
        /// Its length.
        len: usize,
    },

    /// The volume identifier field is 32 bytes; a longer label would
    /// truncate into something cloud-init does not match.
    #[error("volume label {label:?} is {len} bytes, over the {LABEL_MAX} limit")]
    LabelTooLong {
        /// The offending label.
        label: String,
        /// Its length.
        len: usize,
    },
}

/// One file to place in the image's root directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoFile {
    /// Name as the guest should see it, e.g. `user-data`.
    pub name: String,
    /// Contents, written verbatim.
    pub data: Vec<u8>,
}

impl IsoFile {
    /// Construct.
    #[must_use]
    pub fn new(name: &str, data: Vec<u8>) -> Self {
        Self {
            name: name.to_string(),
            data,
        }
    }
}

/// Build an ISO9660 image with a Joliet tree containing `files` at the root.
///
/// # Errors
/// [`IsoError`] when the label or a file name cannot be represented.
pub fn build_iso9660(label: &str, files: &[IsoFile]) -> Result<Vec<u8>, IsoError> {
    if label.len() > LABEL_MAX {
        return Err(IsoError::LabelTooLong {
            label: label.to_string(),
            len: label.len(),
        });
    }
    for f in files {
        if f.name.is_empty() {
            return Err(IsoError::EmptyName);
        }
        if f.name.contains('/') || f.name.contains('\\') {
            return Err(IsoError::NestedPath {
                name: f.name.clone(),
            });
        }
        if primary_name(&f.name).len() > PRIMARY_NAME_MAX {
            return Err(IsoError::NameTooLong {
                name: f.name.clone(),
                len: f.name.len(),
            });
        }
    }

    // Lay the files out first: both directory trees need their extents.
    let mut lba = LBA_FIRST_FILE;
    let placed: Vec<(&IsoFile, u32)> = files
        .iter()
        .map(|f| {
            let at = lba;
            lba += sectors_for(f.data.len());
            (f, at)
        })
        .collect();
    let total_sectors = lba;

    let mut img = vec![0u8; SYSTEM_AREA_SECTORS * SECTOR];

    // Every structure below is written sequentially, and every reference to
    // one is by hard-coded LBA. `at` ties the two together: if a structure
    // ever changes size, the layout constants and the writes disagree here
    // rather than producing an image that mounts and is subtly wrong.
    let at = |img: &Vec<u8>| u32::try_from(img.len() / SECTOR).unwrap_or(u32::MAX);

    debug_assert_eq!(at(&img), LBA_PVD);
    img.extend_from_slice(&volume_descriptor(
        DescriptorKind::Primary,
        label,
        total_sectors,
    ));
    debug_assert_eq!(at(&img), LBA_SVD);
    img.extend_from_slice(&volume_descriptor(
        DescriptorKind::Joliet,
        label,
        total_sectors,
    ));
    debug_assert_eq!(at(&img), LBA_TERMINATOR);
    img.extend_from_slice(&terminator());

    debug_assert_eq!(at(&img), LBA_PATH_L);
    img.extend_from_slice(&path_table(Endian::Little, LBA_ROOT));
    debug_assert_eq!(at(&img), LBA_PATH_M);
    img.extend_from_slice(&path_table(Endian::Big, LBA_ROOT));
    debug_assert_eq!(at(&img), LBA_PATH_L_JOLIET);
    img.extend_from_slice(&path_table(Endian::Little, LBA_ROOT_JOLIET));
    debug_assert_eq!(at(&img), LBA_PATH_M_JOLIET);
    img.extend_from_slice(&path_table(Endian::Big, LBA_ROOT_JOLIET));

    debug_assert_eq!(at(&img), LBA_ROOT);
    img.extend_from_slice(&root_directory(DescriptorKind::Primary, &placed));
    debug_assert_eq!(at(&img), LBA_ROOT_JOLIET);
    img.extend_from_slice(&root_directory(DescriptorKind::Joliet, &placed));
    debug_assert_eq!(at(&img), LBA_FIRST_FILE);

    for (f, at) in &placed {
        debug_assert_eq!(img.len(), *at as usize * SECTOR, "extent misaligned");
        img.extend_from_slice(&f.data);
        // Pad to the extent this file was *reserved*, not to the next
        // sector boundary: an empty file occupies a whole sector, and
        // rounding its zero bytes up would round to zero and leave the
        // following file's extent address pointing at the wrong place.
        let end = (at + sectors_for(f.data.len())) as usize * SECTOR;
        img.resize(end, 0);
    }

    debug_assert_eq!(img.len(), total_sectors as usize * SECTOR);
    Ok(img)
}

/// Which of the two trees a structure belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DescriptorKind {
    Primary,
    Joliet,
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

/// Sectors needed to hold `len` bytes. An empty file still occupies one, so
/// its extent address is meaningful.
fn sectors_for(len: usize) -> u32 {
    u32::try_from(len.max(1).div_ceil(SECTOR)).unwrap_or(1)
}

/// `both-endian` 32-bit: little then big, as the standard requires for
/// nearly every numeric field.
fn both32(v: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&v.to_le_bytes());
    out[4..].copy_from_slice(&v.to_be_bytes());
    out
}

/// `both-endian` 16-bit.
fn both16(v: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&v.to_le_bytes());
    out[2..].copy_from_slice(&v.to_be_bytes());
    out
}

/// A name as the primary tree must spell it: uppercase, d-characters only,
/// with a trailing `.` since the standard wants a separator even with no
/// extension. The hyphen is not a d-character, hence `META_DATA.`.
fn primary_name(name: &str) -> String {
    let mapped: String = name
        .to_ascii_uppercase()
        .chars()
        .map(|c| {
            if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.contains('.') {
        mapped
    } else {
        format!("{mapped}.")
    }
}

/// UCS-2 big endian, which is how Joliet spells every name.
fn ucs2be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// Write `text` into `field`, padded with `pad`, truncating if needed.
fn padded(field: &mut [u8], text: &[u8], pad: u8) {
    field.fill(pad);
    let n = text.len().min(field.len());
    field[..n].copy_from_slice(&text[..n]);
}

/// A primary or supplementary volume descriptor, one sector.
fn volume_descriptor(kind: DescriptorKind, label: &str, total_sectors: u32) -> Vec<u8> {
    let joliet = kind == DescriptorKind::Joliet;
    let mut s = vec![0u8; SECTOR];

    s[0] = if joliet { 2 } else { 1 };
    s[1..6].copy_from_slice(b"CD001");
    s[6] = 1;

    // System identifier (8..40) and volume identifier (40..72). Joliet
    // spells both in UCS-2, padded with UCS-2 spaces.
    if joliet {
        padded(&mut s[8..40], &[], 0);
        let mut id = ucs2be(label);
        while id.len() < 32 {
            id.extend_from_slice(&[0x00, 0x20]);
        }
        s[40..72].copy_from_slice(&id[..32]);
        // Escape sequence: %/E is UCS-2 level 3. This is what makes a
        // supplementary descriptor a *Joliet* one.
        s[88..91].copy_from_slice(b"%/E");
    } else {
        padded(&mut s[8..40], b"", b' ');
        padded(&mut s[40..72], label.as_bytes(), b' ');
    }

    s[80..88].copy_from_slice(&both32(total_sectors));
    s[120..124].copy_from_slice(&both16(1)); // volume set size
    s[124..128].copy_from_slice(&both16(1)); // volume sequence number
    s[128..132].copy_from_slice(&both16(SECTOR as u16));

    let root_lba = if joliet { LBA_ROOT_JOLIET } else { LBA_ROOT };
    let (path_l, path_m) = if joliet {
        (LBA_PATH_L_JOLIET, LBA_PATH_M_JOLIET)
    } else {
        (LBA_PATH_L, LBA_PATH_M)
    };
    s[132..140].copy_from_slice(&both32(SECTOR as u32)); // path table size
    s[140..144].copy_from_slice(&path_l.to_le_bytes());
    s[148..152].copy_from_slice(&path_m.to_be_bytes());

    // Root directory record, inline in the descriptor.
    let root = directory_record(kind, DirEntry::Root, root_lba, SECTOR as u32);
    s[156..156 + root.len()].copy_from_slice(&root);

    // The remaining identifier fields are unused by a seed; the standard
    // wants them space-filled rather than zeroed.
    for range in [318..446, 446..574, 574..702, 702..739] {
        padded(&mut s[range], b"", b' ');
    }
    // Dates: 16 ASCII digits then a GMT offset byte. Creation, modification
    // and effective get a fixed real date — the same epoch the directory
    // records use, so the bytes stay deterministic. All-'0' ("not
    // specified") is legal for these too, but go-diskfs before v1.9 parses
    // the creation date literally and rejects the volume, and Kairos uses
    // it to find a `CIDATA` seed by label when there is no CD-ROM (Cloud
    // Hypervisor). Only expiration stays unspecified, as `genisoimage`
    // leaves it.
    for start in [PVD_CREATION_DATE, PVD_MODIFICATION_DATE, PVD_EFFECTIVE_DATE] {
        s[start..start + DATE_DIGITS].copy_from_slice(FIXED_VOLUME_DATE);
        s[start + DATE_DIGITS] = 0;
    }
    padded(
        &mut s[PVD_EXPIRATION_DATE..PVD_EXPIRATION_DATE + DATE_DIGITS],
        b"",
        b'0',
    );
    s[PVD_EXPIRATION_DATE + DATE_DIGITS] = 0;
    s[881] = 1; // file structure version
    s
}

/// The volume descriptor set terminator, one sector.
fn terminator() -> Vec<u8> {
    let mut s = vec![0u8; SECTOR];
    s[0] = 255;
    s[1..6].copy_from_slice(b"CD001");
    s[6] = 1;
    s
}

/// A path table holding only the root, one sector.
fn path_table(endian: Endian, root_lba: u32) -> Vec<u8> {
    let mut s = vec![0u8; SECTOR];
    s[0] = 1; // directory identifier length: 1, the root's single NUL
    s[1] = 0; // extended attribute length
    match endian {
        Endian::Little => {
            s[2..6].copy_from_slice(&root_lba.to_le_bytes());
            s[6..8].copy_from_slice(&1u16.to_le_bytes());
        }
        Endian::Big => {
            s[2..6].copy_from_slice(&root_lba.to_be_bytes());
            s[6..8].copy_from_slice(&1u16.to_be_bytes());
        }
    }
    s[8] = 0; // the root's name: a single zero byte
    s
}

/// Which directory entry a record describes.
enum DirEntry<'a> {
    /// `.` — self.
    Root,
    /// `..` — parent. Identical to `.` for a flat image.
    Parent,
    /// A real file.
    File(&'a str),
}

/// One directory record. Records are even-length by rule.
fn directory_record(kind: DescriptorKind, entry: DirEntry<'_>, lba: u32, size: u32) -> Vec<u8> {
    let name: Vec<u8> = match (&entry, kind) {
        (DirEntry::Root, _) => vec![0x00],
        (DirEntry::Parent, _) => vec![0x01],
        (DirEntry::File(n), DescriptorKind::Primary) => primary_name(n).into_bytes(),
        (DirEntry::File(n), DescriptorKind::Joliet) => ucs2be(n),
    };

    let mut r = Vec::with_capacity(34 + name.len());
    r.push(0); // length, filled in below
    r.push(0); // extended attribute length
    r.extend_from_slice(&both32(lba));
    r.extend_from_slice(&both32(size));
    // Recording date: seven bytes, years since 1900. Fixed rather than
    // "now", so the same inputs produce the same bytes — a reconciler
    // rebuilds this image every pass and must not rewrite the volume.
    r.extend_from_slice(&[70, 1, 1, 0, 0, 0, 0]);
    r.push(if matches!(entry, DirEntry::File(_)) {
        0
    } else {
        0x02 // directory
    });
    r.push(0); // file unit size
    r.push(0); // interleave gap
    r.extend_from_slice(&both16(1)); // volume sequence number
    r.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
    r.extend_from_slice(&name);
    if r.len() % 2 != 0 {
        r.push(0);
    }
    r[0] = u8::try_from(r.len()).unwrap_or(u8::MAX);
    r
}

/// The root directory for one tree, one sector.
///
/// A seed's root holds `.`, `..` and a handful of files — comfortably
/// inside a single sector, which is why the descriptors can hard-code the
/// directory size.
fn root_directory(kind: DescriptorKind, placed: &[(&IsoFile, u32)]) -> Vec<u8> {
    let root_lba = match kind {
        DescriptorKind::Primary => LBA_ROOT,
        DescriptorKind::Joliet => LBA_ROOT_JOLIET,
    };
    let mut s = Vec::with_capacity(SECTOR);
    s.extend_from_slice(&directory_record(
        kind,
        DirEntry::Root,
        root_lba,
        SECTOR as u32,
    ));
    s.extend_from_slice(&directory_record(
        kind,
        DirEntry::Parent,
        root_lba,
        SECTOR as u32,
    ));
    for (f, at) in placed {
        s.extend_from_slice(&directory_record(
            kind,
            DirEntry::File(&f.name),
            *at,
            u32::try_from(f.data.len()).unwrap_or(u32::MAX),
        ));
    }
    s.resize(SECTOR, 0);
    s
}

#[cfg(test)]
#[path = "iso9660_tests.rs"]
mod iso9660_tests;
