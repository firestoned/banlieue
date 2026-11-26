// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue-azure-guest customdata`: customData from the local provisioning
//! disk into `/oem` (ADR-0163 Decision 3, ADR-0171 Decision 4).
//!
//! Azure attaches a small UDF CD-ROM to a new VM with `ovf-env.xml` on it.
//! The helper finds it by scanning `/dev/sr*`, mounts it read-only (no
//! exec, no suid, no devices), reads `ovf-env.xml` within a size cap,
//! unmounts it, and hands the bytes to [`crate::ovf`]. A cloud-config that
//! passes is written to `/oem/91_azure_customdata.yaml`, mode 0600, through
//! a temporary file and a rename, so a crash never leaves half a file that
//! Kairos would apply. Writing the same bytes again is a no-op.
//!
//! The file sorts after the image's own `/oem/90_custom.yaml`, so customData
//! layers over the baked configuration, and it persists on `COS_OEM`, which
//! a Kairos reset keeps: the first boot of a raw image is a recovery boot
//! that writes it and resets, and the first active boot applies it from its
//! first stage.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::azure;
use crate::kmsg::Logger;
use crate::ovf::{self, OVF_ENV_MAX_BYTES, OvfError};

/// Kairos's OEM partition, where every `*.yaml` is applied on every boot.
pub const OEM_DIR: &str = "/oem";

/// The file customData is written to. Sorts after the image's
/// `90_custom.yaml`; the same name the spike used.
pub const CUSTOM_DATA_FILE_NAME: &str = "91_azure_customdata.yaml";

/// customData may carry secrets (a join token): owner read/write only.
pub const CUSTOM_DATA_FILE_MODE: u32 = 0o600;

/// The temporary file the write goes through. A dot file without a `.yaml`
/// extension, so Kairos never reads it.
const TEMP_FILE_NAME: &str = ".91_azure_customdata.tmp";

/// The file Azure puts on the provisioning disk.
pub const OVF_ENV_FILE_NAME: &str = "ovf-env.xml";

/// Where the provisioning disk is mounted while it is read.
pub const MOUNT_DIR: &str = "/run/banlieue-azure-guest/provisioning";

/// The mount point is private to root.
const MOUNT_DIR_MODE: u32 = 0o700;

/// Device directory scanned for CD-ROMs.
pub const DEV_DIR: &str = "/dev";

/// Kernel name prefix of a SCSI CD-ROM, which Azure's provisioning disk is.
const CDROM_PREFIX: &str = "sr";

/// Filesystems the provisioning disk is tried as, in order: Azure writes
/// UDF; ISO 9660 covers a disk built by hand for a test.
const PROVISIONING_FS_TYPES: [&str; 2] = ["udf", "iso9660"];

/// Why `customdata` failed. Never carries customData itself.
#[derive(Debug, Error)]
pub enum CustomDataError {
    /// `ovf-env.xml` or its customData was refused.
    #[error(transparent)]
    Ovf(#[from] OvfError),
    /// Writing `/oem` failed.
    #[error("writing {path}: {source}")]
    Write {
        /// The file being written.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
}

/// What `customdata` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not on Azure: nothing was read or written.
    NotAzure,
    /// No CD-ROM carried `ovf-env.xml`.
    NoProvisioningDisk,
    /// The VM was created without customData; nothing written.
    NoCustomData,
    /// customData was written (this many bytes).
    Written(usize),
    /// The file already held exactly this customData.
    Unchanged,
}

/// The paths `customdata` works on: the system's, or a test's.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The DMI chassis asset tag file.
    pub asset_tag: PathBuf,
    /// Directory scanned for `sr*` devices.
    pub dev_dir: PathBuf,
    /// The OEM directory written to.
    pub oem_dir: PathBuf,
    /// Where the provisioning disk is mounted.
    pub mount_dir: PathBuf,
}

impl Paths {
    /// The paths on a real guest.
    #[must_use]
    pub fn system() -> Self {
        Self {
            asset_tag: PathBuf::from(azure::CHASSIS_ASSET_TAG_PATH),
            dev_dir: PathBuf::from(DEV_DIR),
            oem_dir: PathBuf::from(OEM_DIR),
            mount_dir: PathBuf::from(MOUNT_DIR),
        }
    }
}

/// `sr0`, `sr1`, ... under `dev_dir`, in numeric order. Anything else
/// (`sr`, `srx`, `sr0p1`) is ignored.
#[must_use]
pub fn candidate_devices(dev_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dev_dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let digits = name.strip_prefix(CDROM_PREFIX)?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((digits.parse().ok()?, e.path()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

/// Read `path`, failing when it is larger than `cap` (the excess is never
/// read into memory).
///
/// # Errors
/// The I/O error, or `InvalidData` when the file is over `cap`.
pub fn read_capped(path: &Path, cap: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit).read_to_end(&mut buf)?;
    if buf.len() > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {cap} bytes"),
        ));
    }
    Ok(buf)
}

/// Mount `device` read-only at `mount_dir`, read `ovf-env.xml` from it if
/// present, and unmount. `Ok(None)` when it mounted but has no
/// `ovf-env.xml`.
///
/// Needs root; not unit tested (the scan and everything after the read
/// are).
///
/// # Errors
/// The mount failing as every type in [`PROVISIONING_FS_TYPES`], or the
/// read failing.
#[cfg(target_os = "linux")]
pub fn mount_and_read(device: &Path, mount_dir: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fs::create_dir_all(mount_dir)?;
    fs::set_permissions(mount_dir, fs::Permissions::from_mode(MOUNT_DIR_MODE))?;
    let src = CString::new(device.as_os_str().as_bytes())?;
    let dst = CString::new(mount_dir.as_os_str().as_bytes())?;
    let flags = libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;

    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for fs_type in PROVISIONING_FS_TYPES {
        let fstype = CString::new(fs_type)?;
        // SAFETY: every pointer is a valid NUL-terminated C string that
        // outlives the call; `data` is null, which mount(2) accepts.
        let rc = unsafe {
            libc::mount(
                src.as_ptr(),
                dst.as_ptr(),
                fstype.as_ptr(),
                flags,
                std::ptr::null(),
            )
        };
        if rc != 0 {
            last = io::Error::last_os_error();
            continue;
        }
        let path = mount_dir.join(OVF_ENV_FILE_NAME);
        let read = if path.exists() {
            read_capped(&path, OVF_ENV_MAX_BYTES).map(Some)
        } else {
            Ok(None)
        };
        // SAFETY: `dst` is a valid NUL-terminated C string; flags 0 is a
        // plain unmount.
        unsafe {
            libc::umount2(dst.as_ptr(), 0);
        }
        return read;
    }
    Err(last)
}

/// Not Linux: there is no provisioning disk to mount.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_os = "linux"))]
pub fn mount_and_read(_device: &Path, _mount_dir: &Path) -> io::Result<Option<Vec<u8>>> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// The first device whose reader returns an `ovf-env.xml`. A device that
/// fails to mount or has no `ovf-env.xml` is logged and skipped.
pub fn find_ovf_env<F>(devices: &[PathBuf], mut read: F, log: &mut Logger) -> Option<Vec<u8>>
where
    F: FnMut(&Path) -> io::Result<Option<Vec<u8>>>,
{
    for device in devices {
        match read(device) {
            Ok(Some(bytes)) => {
                log.info(&format!(
                    "customdata: provisioning disk {}",
                    device.display()
                ));
                return Some(bytes);
            }
            Ok(None) => log.info(&format!(
                "customdata: {} has no {OVF_ENV_FILE_NAME}",
                device.display()
            )),
            Err(e) => log.info(&format!(
                "customdata: {} not readable: {e}",
                device.display()
            )),
        }
    }
    None
}

/// Write `bytes` to `oem_dir/91_azure_customdata.yaml`, mode 0600,
/// atomically: a temporary file, `fsync`, `rename`, then `fsync` of the
/// directory. Writing the bytes the file already holds changes nothing.
///
/// # Errors
/// Any I/O error; the target is never left partially written.
pub fn write_custom_data(oem_dir: &Path, bytes: &[u8]) -> io::Result<Outcome> {
    let target = oem_dir.join(CUSTOM_DATA_FILE_NAME);
    if let Ok(existing) = fs::read(&target)
        && existing == bytes
    {
        let mode = fs::metadata(&target)?.permissions().mode() & 0o777;
        if mode != CUSTOM_DATA_FILE_MODE {
            fs::set_permissions(&target, fs::Permissions::from_mode(CUSTOM_DATA_FILE_MODE))?;
        }
        return Ok(Outcome::Unchanged);
    }
    let temp = oem_dir.join(TEMP_FILE_NAME);
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(CUSTOM_DATA_FILE_MODE)
        .open(&temp)?;
    // The umask cannot widen the mode, but set it explicitly anyway.
    file.set_permissions(fs::Permissions::from_mode(CUSTOM_DATA_FILE_MODE))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, &target)?;
    File::open(oem_dir)?.sync_all()?;
    Ok(Outcome::Written(bytes.len()))
}

/// The whole `customdata` command, with the provisioning-disk reader
/// injected so it runs in a test.
///
/// # Errors
/// [`CustomDataError::Ovf`] when `ovf-env.xml` or its customData is
/// refused (nothing is written), [`CustomDataError::Write`] when `/oem`
/// cannot be written.
pub fn run_with<F>(paths: &Paths, read: F, log: &mut Logger) -> Result<Outcome, CustomDataError>
where
    F: FnMut(&Path) -> io::Result<Option<Vec<u8>>>,
{
    if !azure::is_azure(&paths.asset_tag) {
        return Ok(Outcome::NotAzure);
    }
    let devices = candidate_devices(&paths.dev_dir);
    let Some(document) = find_ovf_env(&devices, read, log) else {
        log.info("customdata: no provisioning disk with ovf-env.xml");
        return Ok(Outcome::NoProvisioningDisk);
    };
    let Some(cloud_config) = ovf::cloud_config_from_ovf_env(&document)? else {
        log.info("customdata: the VM has no customData");
        return Ok(Outcome::NoCustomData);
    };
    let outcome = write_custom_data(&paths.oem_dir, &cloud_config).map_err(|source| {
        CustomDataError::Write {
            path: paths.oem_dir.join(CUSTOM_DATA_FILE_NAME),
            source,
        }
    })?;
    match outcome {
        Outcome::Written(n) => log.info(&format!(
            "customdata: wrote {n} bytes to {}",
            paths.oem_dir.join(CUSTOM_DATA_FILE_NAME).display()
        )),
        _ => log.info("customdata: unchanged"),
    }
    Ok(outcome)
}

/// The `customdata` command on a real guest.
///
/// # Errors
/// As [`run_with`].
pub fn run(log: &mut Logger) -> Result<Outcome, CustomDataError> {
    let paths = Paths::system();
    let mount_dir = paths.mount_dir.clone();
    run_with(&paths, |dev| mount_and_read(dev, &mount_dir), log)
}

#[cfg(test)]
#[path = "customdata_tests.rs"]
mod customdata_tests;
