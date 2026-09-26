// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A machine's files on the host: directories, OS disk, seed, and removal.
//!
//! Ownership follows ADR-0063 Decision 5, with one correction found while
//! writing this: machine and run directories are `guest-uid:provider-group`
//! mode **0770**, not 0700/0750. The provider has to delete their contents on
//! teardown, and removing a file needs write permission on the directory,
//! which `CAP_FOWNER` does not grant. Group write gives the provider exactly
//! that and nothing more; other guests, in neither the uid nor the group,
//! still cannot enter.
//!
//! Every write goes through a temporary name and an atomic rename, so a
//! crash mid-write never leaves a half disk or half seed under the real name.
//! Blocking; the caller runs these on a blocking thread.

use crate::plan::MachinePlan;
use crate::sys::{self, CloneKind};
use std::fs::File;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt, fchown};
use std::path::Path;

/// Machine and run directories: owner (the guest) and group (the provider)
/// only, setgid so what the VMM creates inside — its API socket, the serial
/// log — lands in the provider's group rather than the guest's own.
const DIR_MODE: u32 = 0o2770;
/// The VMM's API socket once the provider has opened it to its group.
const SOCKET_MODE: u32 = 0o660;
/// Permission bits (no file type, no setuid/setgid/sticky).
const PERMISSION_BITS: u32 = 0o777;
/// Bits that would grant anything to users outside owner and group.
const OTHER_BITS: u32 = 0o007;
/// The OS disk: the guest reads and writes it, the provider can remove it.
const DISK_MODE: u32 = 0o660;
/// The seed: the guest reads it, nobody writes it after creation.
const SEED_MODE: u32 = 0o440;
/// The installer's per-machine copy: read-only, like the seed.
const INSTALL_MEDIA_MODE: u32 = 0o440;
/// A file in the TPM state directory, once handed to the guest's swtpm.
const TPM_STATE_FILE_MODE: u32 = 0o660;
/// The EK certificate directory: the provider's alone.
const EK_DIR_MODE: u32 = 0o700;
/// Extension of the EK certificates `swtpm_setup --write-ek-cert-files`
/// writes (DER).
const EK_CERT_EXTENSION: &str = "crt";

/// A template unit's environment file: the provider's alone.
const ENV_FILE_MODE: u32 = 0o600;
/// The directory holding them.
const ENV_DIR_MODE: u32 = 0o700;

/// Suffix for in-progress writes, renamed into place when complete.
const TMP_SUFFIX: &str = ".tmp";

/// What happened to the OS disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskOutcome {
    /// It already existed and was left alone.
    Existing,
    /// It was created from the image, by the given kind of copy.
    Created(CloneKind),
    /// It was created empty, for a `Deferred` install (ADR-0065).
    CreatedEmpty,
}

/// Create the machine and run directories, owned by the guest, shared with
/// `group`. Existing directories are re-owned and re-moded, which also
/// repairs a directory a previous crash left half set up.
///
/// # Errors
/// The I/O error, including a missing storage or run root.
pub fn prepare_dirs(plan: &MachinePlan, group: u32) -> io::Result<()> {
    for dir in [&plan.machine_dir, &plan.run_dir] {
        match fs::create_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(with_path(e, dir)),
        }
        // Re-own through a handle opened without following links: a symlink
        // at this path is refused, never followed and re-owned.
        let handle = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)
            .map_err(|e| with_path(e, dir))?;
        own(&handle, dir, plan.host_uid, group, DIR_MODE)?;
    }
    Ok(())
}

/// Make the VMM's API socket reachable by the provider's group.
///
/// Cloud Hypervisor v53 sets its own umask to `0077`, whatever the unit
/// says, so its socket is always `0700` and the provider — the group, not
/// the owner — cannot connect. The provider holds `CAP_FOWNER` and sets it
/// to `0660`, but only after checking it is what the VMM created: a socket,
/// owned by the guest `uid`, in the provider's `group` (the setgid run
/// directory gives it that), granting nothing to others.
///
/// The guest owns the run directory, so it could put a symlink or another
/// file where the socket belongs, and `CAP_FOWNER` would let a naive chmod
/// change any file on the host. The path is therefore opened `O_PATH |
/// O_NOFOLLOW`, the checks run on that open inode, and the mode is set on
/// that same inode through `/proc/self/fd` — nothing is followed and there
/// is no window between check and change.
///
/// Returns `Ok(false)` while the socket does not exist yet.
///
/// # Errors
/// `InvalidData` when something other than the guest's socket is there, or
/// the I/O error.
pub fn grant_api_socket(path: &Path, uid: u32, group: u32) -> io::Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        // O_PATH|O_NOFOLLOW on a symlink opens the link itself, so this is
        // only reached for other errors.
        Err(e) => return Err(with_path(e, path)),
    };
    let meta = file.metadata()?;
    let refuse = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {why}", path.display()),
        ))
    };
    if !meta.file_type().is_socket() {
        return refuse(format!("not a socket ({:?})", meta.file_type()));
    }
    if meta.uid() != uid || meta.gid() != group {
        return refuse(format!(
            "owned {}:{}, expected {uid}:{group}",
            meta.uid(),
            meta.gid()
        ));
    }
    let perms = meta.mode() & PERMISSION_BITS;
    if perms & OTHER_BITS != 0 {
        return refuse(format!("mode {perms:o} grants access to others"));
    }
    if perms != SOCKET_MODE {
        let by_fd = format!("/proc/self/fd/{}", file.as_raw_fd());
        fs::set_permissions(&by_fd, Permissions::from_mode(SOCKET_MODE))
            .map_err(|e| with_path(e, path))?;
    }
    Ok(true)
}

/// Create the OS disk from the cached image and grow it, unless it exists;
/// for a `Deferred` install, create it empty instead.
///
/// An existing disk is **never** replaced: it may hold an installed guest.
///
/// # Errors
/// `NotFound` naming the image when it is not in the cache yet;
/// `InvalidInput` when the image is larger than the requested disk (a disk is
/// never shrunk); otherwise the I/O error.
pub fn ensure_os_disk(plan: &MachinePlan, group: u32) -> io::Result<DiskOutcome> {
    if plan.os_disk.exists() {
        return Ok(DiskOutcome::Existing);
    }
    if plan.empty_os_disk {
        return create_empty_os_disk(plan, group);
    }
    let image_len = fs::metadata(&plan.image)
        .map_err(|e| with_path(e, &plan.image))?
        .len();
    if image_len > plan.os_disk_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "image {} is {image_len} bytes, larger than the {}-byte OS disk requested",
                plan.image.display(),
                plan.os_disk_bytes
            ),
        ));
    }
    let tmp = tmp_path(&plan.os_disk);
    remove_if_present(&tmp)?;
    // The machine directory is the guest's, so from here on everything acts
    // on the file clone_file created O_EXCL, never on the path, which the
    // guest could swap for a symlink.
    let (disk, kind) = sys::clone_file(&plan.image, &tmp).map_err(|e| with_path(e, &tmp))?;
    // Grow before first boot (ADR-0062 Decision 3). set_len extends sparsely.
    disk.set_len(plan.os_disk_bytes)
        .map_err(|e| with_path(e, &tmp))?;
    own(&disk, &tmp, plan.host_uid, group, DISK_MODE)?;
    drop(disk);
    fs::rename(&tmp, &plan.os_disk)?;
    Ok(DiskOutcome::Created(kind))
}

/// Keep the machine's copy of the installer in step with the plan: staged
/// while the installer is attached, deleted once it has been ejected
/// (ADR-0065 Decisions 3 and 4, amended).
///
/// The guest's VMM cannot read the provider's image cache, so it gets its
/// own read-only copy in its machine directory, cloned (reflink where the
/// filesystem supports it) like an `Immediate` OS disk. The directory is
/// the guest's: anything but a regular file at the path is replaced, and
/// the copy is created `O_EXCL` under a temporary name and acted on by
/// handle.
///
/// # Errors
/// The I/O error, naming the path; a missing cache image names the image.
pub fn ensure_install_media(plan: &MachinePlan, group: u32) -> io::Result<()> {
    if !plan.has_install_media() {
        return remove_if_present(&plan.install_media);
    }
    match fs::symlink_metadata(&plan.install_media) {
        Ok(m) if m.is_file() => return Ok(()),
        Ok(_) => remove_if_present(&plan.install_media)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(with_path(e, &plan.install_media)),
    }
    let tmp = tmp_path(&plan.install_media);
    remove_if_present(&tmp)?;
    let (copy, _) = sys::clone_file(&plan.image, &tmp).map_err(|e| with_path(e, &plan.image))?;
    own(&copy, &tmp, plan.host_uid, group, INSTALL_MEDIA_MODE)?;
    drop(copy);
    fs::rename(&tmp, &plan.install_media).map_err(|e| with_path(e, &plan.install_media))
}

/// An empty, sparse OS disk of the requested size, through a temporary
/// name created `O_EXCL` and acted on by handle.
fn create_empty_os_disk(plan: &MachinePlan, group: u32) -> io::Result<DiskOutcome> {
    let tmp = tmp_path(&plan.os_disk);
    remove_if_present(&tmp)?;
    let disk = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&tmp)
        .map_err(|e| with_path(e, &tmp))?;
    disk.set_len(plan.os_disk_bytes)
        .map_err(|e| with_path(e, &tmp))?;
    own(&disk, &tmp, plan.host_uid, group, DISK_MODE)?;
    drop(disk);
    fs::rename(&tmp, &plan.os_disk)?;
    Ok(DiskOutcome::CreatedEmpty)
}

/// Write the seed, unless an identical one is already there.
///
/// # Errors
/// The I/O error.
pub fn write_seed(plan: &MachinePlan, iso: &[u8], group: u32) -> io::Result<()> {
    // Read without following links: a symlink to identical bytes is not
    // "unchanged", it is replaced by a real file below.
    if read_nofollow(&plan.seed).is_ok_and(|existing| existing == iso) {
        return Ok(());
    }
    let tmp = tmp_path(&plan.seed);
    remove_if_present(&tmp)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&tmp)?;
    f.write_all(iso)?;
    own(&f, &tmp, plan.host_uid, group, SEED_MODE)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &plan.seed)?;
    Ok(())
}

/// Remove the machine and run directories and everything in them, then
/// check they are gone. Absent is success.
///
/// # Errors
/// The I/O error, or `Other` if a directory survived removal.
pub fn remove_machine(plan: &MachinePlan) -> io::Result<()> {
    let ek = plan.tpm.as_ref().map(|t| &t.ek_dir);
    let state = plan.tpm.as_ref().map(|t| &t.state_dir);
    if let Some(t) = &plan.tpm {
        remove_if_present(&t.setup_env)?;
    }
    for dir in [Some(&plan.run_dir), Some(&plan.machine_dir), state, ek]
        .into_iter()
        .flatten()
    {
        match fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(with_path(e, dir)),
        }
        // Verify rather than assume (the ADR-0050 lesson).
        if dir.exists() {
            return Err(io::Error::other(format!(
                "{} still exists after removal",
                dir.display()
            )));
        }
    }
    Ok(())
}

/// Whether anything of this machine is still on disk.
#[must_use]
pub fn machine_files_exist(plan: &MachinePlan) -> bool {
    plan.machine_dir.exists()
        || plan.run_dir.exists()
        || plan
            .tpm
            .as_ref()
            .is_some_and(|t| t.ek_dir.exists() || t.state_dir.exists() || t.setup_env.exists())
}

/// Get a vTPM's EK directory ready (ADR-0065 Decision 1): in the
/// provider's state root, `0700`, provider only. The state directory is
/// [`reset_tpm_state`]'s. A no-op without a vTPM.
///
/// # Errors
/// The I/O error, including a symlink where a directory belongs.
pub fn prepare_tpm(plan: &MachinePlan, provider_uid: u32, group: u32) -> io::Result<()> {
    let Some(t) = &plan.tpm else {
        return Ok(());
    };
    if let Some(root) = t.ek_dir.parent() {
        fs::create_dir_all(root).map_err(|e| with_path(e, root))?;
    }
    let ek = create_dir_nofollow(&t.ek_dir)?;
    own(&ek, &t.ek_dir, provider_uid, group, EK_DIR_MODE)?;
    Ok(())
}

/// A fresh, empty state directory for manufacture, owned by the provider
/// (which runs `swtpm_setup`). Called only just before the manufacture unit
/// starts, never while it runs.
///
/// The state is keyed by the guest's host uid, which is reused once its
/// machine is gone: anything there belongs to no manufactured TPM of this
/// machine, so it is cleared. Its parent is the provider's (`0711`), so the
/// entry itself cannot be a guest's symlink, and `remove_dir_all` does not
/// follow links inside it.
///
/// # Errors
/// The I/O error.
pub fn reset_tpm_state(plan: &MachinePlan, provider_uid: u32, group: u32) -> io::Result<()> {
    let Some(t) = &plan.tpm else {
        return Ok(());
    };
    if let Some(root) = t.state_dir.parent() {
        fs::create_dir_all(root).map_err(|e| with_path(e, root))?;
    }
    match fs::remove_dir_all(&t.state_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(with_path(e, &t.state_dir)),
    }
    let state = create_dir_nofollow(&t.state_dir)?;
    own(&state, &t.state_dir, provider_uid, group, DIR_MODE)
}

/// Whether this machine's TPM was manufactured: the host wrote its EK
/// certificates. Kept in the provider's own directory, so a guest cannot
/// fake it or, by deleting its state, get a TPM manufactured twice.
#[must_use]
pub fn tpm_manufactured(plan: &MachinePlan) -> bool {
    plan.tpm
        .as_ref()
        .is_some_and(|t| ek_files(&t.ek_dir).is_ok_and(|files| !files.is_empty()))
}

/// Hand the manufactured TPM state to the guest's uid, so its swtpm can
/// read and write it. Each file is opened `O_NOFOLLOW` relative to the
/// directory handle and changed through that handle: the machine directory
/// is the guest's, and a symlink planted here must not turn the provider's
/// `CAP_CHOWN` onto another file. Anything but a regular file is refused.
///
/// # Errors
/// `InvalidData` for a symlink or other non-file entry, or the I/O error.
pub fn adopt_tpm_state(plan: &MachinePlan, group: u32) -> io::Result<()> {
    let Some(t) = &plan.tpm else {
        return Ok(());
    };
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&t.state_dir)
        .map_err(|e| with_path(e, &t.state_dir))?;
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(&dir).map_err(io::Error::from)? {
        let entry = entry.map_err(io::Error::from)?;
        let name = entry.file_name().to_owned();
        if name.as_bytes() != b"." && name.as_bytes() != b".." {
            names.push(name);
        }
    }
    for name in names {
        let shown = t.state_dir.join(name.to_string_lossy().as_ref());
        let fd = rustix::fs::openat(
            &dir,
            name.as_c_str(),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| with_path(e.into(), &shown))?;
        let file = File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: not a regular file", shown.display()),
            ));
        }
        own(&file, &shown, plan.host_uid, group, TPM_STATE_FILE_MODE)?;
    }
    own(&dir, &t.state_dir, plan.host_uid, group, DIR_MODE)
}

/// The EK certificates the host wrote for this machine, as PEM, in file
/// name order. Empty without a vTPM.
///
/// # Errors
/// The I/O error.
pub fn ek_certificates(plan: &MachinePlan) -> io::Result<Vec<String>> {
    let Some(t) = &plan.tpm else {
        return Ok(Vec::new());
    };
    ek_files(&t.ek_dir)?
        .iter()
        .map(|f| read_nofollow(f).map(|der| banlieue_provider_sdk::pem::der_to_pem(&der)))
        .collect()
}

/// Whether swtpm's control socket exists, as a socket.
#[must_use]
pub fn tpm_socket_ready(plan: &MachinePlan) -> bool {
    plan.tpm
        .as_ref()
        .is_some_and(|t| fs::symlink_metadata(&t.socket).is_ok_and(|m| m.file_type().is_socket()))
}

/// `*.crt` files directly in `dir`, sorted. Absent directory: none.
fn ek_files(dir: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(with_path(e, dir)),
    };
    let mut files: Vec<std::path::PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == EK_CERT_EXTENSION))
        .collect();
    files.sort();
    Ok(files)
}

/// Create `dir` if absent and open it without following a link.
fn create_dir_nofollow(dir: &Path) -> io::Result<File> {
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(with_path(e, dir)),
    }
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)
        .map_err(|e| with_path(e, dir))
}

/// Write a template unit's environment file: `0600` in a `0700` directory
/// (created if absent), through a temporary name and a rename, so systemd
/// never reads a half-written one. The directory is the provider's own
/// (`<state_root>/units`), never a guest's.
///
/// # Errors
/// The I/O error.
pub fn write_env_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        match fs::create_dir(dir) {
            Ok(()) => fs::set_permissions(dir, Permissions::from_mode(ENV_DIR_MODE))?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(with_path(e, dir)),
        }
    }
    let tmp = tmp_path(path);
    remove_if_present(&tmp)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(ENV_FILE_MODE)
        .custom_flags(libc::O_CLOEXEC)
        .open(&tmp)
        .map_err(|e| with_path(e, &tmp))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path).map_err(|e| with_path(e, path))
}

/// Give an open file or directory to `uid`:`gid` with `mode`, through the
/// handle (`fchown`, `fchmod`), never the path. The provider holds
/// `CAP_CHOWN` and `CAP_FOWNER`, and these paths are in directories the
/// guest can write: a path-based call could be redirected by a symlink to
/// any file the provider can reach. `path` is for error messages only.
fn own(handle: &File, path: &Path, uid: u32, gid: u32, mode: u32) -> io::Result<()> {
    fchown(handle, Some(uid), Some(gid)).map_err(|e| with_path(e, path))?;
    handle
        .set_permissions(Permissions::from_mode(mode))
        .map_err(|e| with_path(e, path))
}

/// Read a file's bytes without following a symlink at `path`.
fn read_nofollow(path: &Path) -> io::Result<Vec<u8>> {
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn tmp_path(p: &Path) -> std::path::PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(TMP_SUFFIX);
    s.into()
}

fn remove_if_present(p: &Path) -> io::Result<()> {
    match fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(with_path(e, p)),
    }
}

fn with_path(e: io::Error, p: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", p.display()))
}

#[cfg(test)]
#[path = "hostfs_tests.rs"]
mod hostfs_tests;
