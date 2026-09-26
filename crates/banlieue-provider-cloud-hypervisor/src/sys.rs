// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The few syscalls the provider needs that `std` does not wrap: tap devices,
//! bridge membership, link state and reflink copies.
//!
//! The crate root denies `unsafe`; this module is the one exception, with
//! exactly two blocks, `ioctl_ifreq` and `ioctl_tun_int`. Each takes its
//! request number from a closed enum, so no caller can pair a request with
//! the wrong kind of argument. Everything else goes through `rustix`'s safe
//! wrappers (sockets, interface index, reflink, ids).
//! Tap devices are created with the tun ioctls rather than `rtnetlink`
//! (ADR-0063 Decision 4 named rtnetlink): Linux has no netlink call that
//! creates a tap, and bridge membership and link state are one ioctl each.
//! No new dependency, and nothing here runs a subprocess.

// The one module allowed `unsafe` (the crate root denies it). Two blocks,
// both `ioctl` calls whose request numbers come from the closed enums below,
// so a request can never be paired with the wrong kind of argument.
#![allow(unsafe_code)]

use rustix::net::{AddressFamily, SocketFlags, SocketType};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Where the tun/tap clone device lives.
const TUN_DEVICE: &str = "/dev/net/tun";
/// `SIOCBRADDIF` from `<linux/sockios.h>`; `libc` does not export it.
const SIOCBRADDIF: libc::c_ulong = 0x89a2;
/// Where the kernel lists network interfaces.
const SYS_CLASS_NET: &str = "/sys/class/net";
/// Bytes of `struct ifreq` after the name: the size of its union.
const IFREQ_DATA_LEN: usize = std::mem::size_of::<libc::ifreq>() - libc::IFNAMSIZ;

/// `struct ifreq`, spelled so it can be built and read without `unsafe`: the
/// name, then the union as plain bytes. Only the members this module uses
/// are read or written, each at offset 0 of the union, as the kernel lays
/// them out.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct IfReq {
    pub(crate) name: [u8; libc::IFNAMSIZ],
    pub(crate) data: [u8; IFREQ_DATA_LEN],
    // `libc::ifreq`'s union holds pointers and a `sockaddr`, so it is
    // pointer-aligned; this zero-sized field gives ours the same alignment.
    _align: [usize; 0],
}

// The kernel copies exactly `sizeof(struct ifreq)` through the pointer.
const _: () = assert!(std::mem::size_of::<IfReq>() == std::mem::size_of::<libc::ifreq>());
const _: () = assert!(std::mem::align_of::<IfReq>() == std::mem::align_of::<libc::ifreq>());

impl IfReq {
    /// `ifr_flags` (a `short`).
    pub(crate) fn flags(&self) -> i16 {
        i16::from_ne_bytes([self.data[0], self.data[1]])
    }
    pub(crate) fn set_flags(&mut self, flags: i16) {
        self.data[..2].copy_from_slice(&flags.to_ne_bytes());
    }
    /// `ifr_ifindex` (an `int`).
    pub(crate) fn set_ifindex(&mut self, index: i32) {
        self.data[..4].copy_from_slice(&index.to_ne_bytes());
    }
}

/// The ioctls that take a `struct ifreq *`. Closed: nothing else can be
/// issued through [`ioctl_ifreq`].
#[derive(Clone, Copy, Debug)]
enum IfreqIoctl {
    /// Attach a tun fd to (or create) the named tap.
    TunSetIff,
    /// Read `ifr_flags`.
    GetFlags,
    /// Write `ifr_flags`.
    SetFlags,
    /// Enslave the interface `ifr_ifindex` to the named bridge.
    BridgeAddIf,
}

impl IfreqIoctl {
    fn request(self) -> libc::c_ulong {
        match self {
            Self::TunSetIff => libc::TUNSETIFF,
            Self::GetFlags => libc::SIOCGIFFLAGS,
            Self::SetFlags => libc::SIOCSIFFLAGS,
            Self::BridgeAddIf => SIOCBRADDIF,
        }
    }
}

/// The tun ioctls that take a plain integer. Closed, as above.
#[derive(Clone, Copy, Debug)]
enum TunIntIoctl {
    /// Give the tap to this uid, so an unprivileged VMM can open it.
    Owner(u32),
    /// Keep the tap after the last fd closes (or stop keeping it).
    Persist(bool),
}

fn ioctl_ifreq(fd: BorrowedFd<'_>, op: IfreqIoctl, req: &mut IfReq) -> io::Result<()> {
    // SAFETY: `fd` is a live descriptor for the duration of the borrow.
    // Every request in `IfreqIoctl` takes a `struct ifreq *`, and `req` is an
    // exclusively borrowed `IfReq` whose size and alignment are asserted
    // equal to the kernel's, so the kernel reads and writes only memory we
    // own. Any byte pattern is a valid `IfReq`.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), op.request(), std::ptr::from_mut(req)) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn ioctl_tun_int(tun: &File, op: TunIntIoctl) -> io::Result<()> {
    let (request, value) = match op {
        TunIntIoctl::Owner(uid) => (libc::TUNSETOWNER, libc::c_ulong::from(uid)),
        TunIntIoctl::Persist(on) => (libc::TUNSETPERSIST, libc::c_ulong::from(on)),
    };
    // SAFETY: `tun` is an open `/dev/net/tun` file attached to a tap. Both
    // requests in `TunIntIoctl` take their argument by value, not as a
    // pointer, so no memory of ours is read or written.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let rc = unsafe { libc::ioctl(tun.as_raw_fd(), request, value) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether `name` is a name the kernel accepts for a network interface
/// (`dev_valid_name`): 1 to 15 bytes, not `.` or `..`, and no `/`, `:`,
/// whitespace or NUL. Everything here checks this before a syscall or a
/// sysfs path, so a name can never climb out of `/sys/class/net`.
#[must_use]
pub fn valid_ifname(name: &str) -> bool {
    !name.is_empty()
        && name.len() < libc::IFNAMSIZ
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'/' && b != b':')
}

/// An `ifreq` with `name` filled in, or `InvalidInput` if it is not a valid
/// interface name.
pub(crate) fn ifreq_named(name: &str) -> io::Result<IfReq> {
    if !valid_ifname(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a valid interface name"),
        ));
    }
    let mut req = IfReq {
        name: [0; libc::IFNAMSIZ],
        data: [0; IFREQ_DATA_LEN],
        _align: [],
    };
    // Shorter than IFNAMSIZ, so the trailing NUL stays.
    req.name[..name.len()].copy_from_slice(name.as_bytes());
    Ok(req)
}

/// Open `/dev/net/tun` and attach it to the tap called `name`, creating it
/// if absent. The returned file keeps the attachment until dropped.
fn attach_tap(name: &str) -> io::Result<File> {
    let tun = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(TUN_DEVICE)?;
    let mut req = ifreq_named(name)?;
    let flags = libc::IFF_TAP | libc::IFF_NO_PI;
    req.set_flags(i16::try_from(flags).map_err(|_| io::Error::other("tun flags overflow"))?);
    ioctl_ifreq(tun.as_fd(), IfreqIoctl::TunSetIff, &mut req)?;
    Ok(tun)
}

/// Create a persistent tap called `name`, owned by `owner_uid` so an
/// unprivileged VMM running as that uid can open it (ADR-0063 Decision 4).
///
/// # Errors
/// The OS error, typically `EPERM` without `CAP_NET_ADMIN`.
pub fn create_persistent_tap(name: &str, owner_uid: Option<u32>) -> io::Result<()> {
    let tun = attach_tap(name)?;
    if let Some(uid) = owner_uid {
        ioctl_tun_int(&tun, TunIntIoctl::Owner(uid))?;
    }
    ioctl_tun_int(&tun, TunIntIoctl::Persist(true))
}

/// Make sure the tap `name` exists, is on `bridge` and is up — the one call
/// a reconcile pass makes, every pass.
///
/// Creates it (owned by `owner_uid`) only when absent. An existing tap is
/// never attached to: the VMM holds its only queue, so attaching would fail
/// with `EBUSY` (and must not take the queue away if it could). The bridge
/// and link state are repaired without attaching.
///
/// # Errors
/// The OS error from creating, enslaving or bringing it up.
pub fn ensure_tap(name: &str, owner_uid: Option<u32>, bridge: &str) -> io::Result<()> {
    if !interface_exists(name) {
        create_persistent_tap(name, owner_uid)?;
    }
    add_to_bridge(bridge, name)?;
    set_link_up(name)
}

/// Delete the persistent tap called `name`. Absent is success.
///
/// # Errors
/// The OS error from clearing persistence.
pub fn delete_persistent_tap(name: &str) -> io::Result<()> {
    if !interface_exists(name) {
        return Ok(());
    }
    let tun = attach_tap(name)?;
    ioctl_tun_int(&tun, TunIntIoctl::Persist(false))
}

/// A datagram socket, the handle `SIOC*` interface ioctls are issued on.
fn control_socket() -> io::Result<OwnedFd> {
    Ok(rustix::net::socket_with(
        AddressFamily::INET,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC,
        None,
    )?)
}

/// Bring the interface `name` up.
///
/// # Errors
/// The OS error from reading or setting the flags.
pub fn set_link_up(name: &str) -> io::Result<()> {
    let sock = control_socket()?;
    let mut req = ifreq_named(name)?;
    ioctl_ifreq(sock.as_fd(), IfreqIoctl::GetFlags, &mut req)?;
    let up = i16::try_from(libc::IFF_UP).map_err(|_| io::Error::other("IFF_UP overflow"))?;
    req.set_flags(req.flags() | up);
    ioctl_ifreq(sock.as_fd(), IfreqIoctl::SetFlags, &mut req)
}

/// Enslave `port` to the Linux bridge `bridge`. Already a member is success.
///
/// # Errors
/// The OS error, `InvalidInput` for an invalid name, or `NotFound` if
/// `port` does not exist.
pub fn add_to_bridge(bridge: &str, port: &str) -> io::Result<()> {
    if master_of(port).as_deref() == Some(bridge) {
        return Ok(());
    }
    if !valid_ifname(port) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{port:?} is not a valid interface name"),
        ));
    }
    let sock = control_socket()?;
    let index = rustix::net::netdevice::name_to_index(&sock, port).map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("interface {port:?} not found: {e}"),
        )
    })?;
    let mut req = ifreq_named(bridge)?;
    req.set_ifindex(
        i32::try_from(index)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "interface index overflow"))?,
    );
    ioctl_ifreq(sock.as_fd(), IfreqIoctl::BridgeAddIf, &mut req)
}

/// Whether an interface called `name` exists.
#[must_use]
pub fn interface_exists(name: &str) -> bool {
    valid_ifname(name) && Path::new(SYS_CLASS_NET).join(name).exists()
}

/// The bridge `name` is enslaved to, if any.
#[must_use]
pub fn master_of(name: &str) -> Option<String> {
    if !valid_ifname(name) {
        return None;
    }
    let link = std::fs::read_link(Path::new(SYS_CLASS_NET).join(name).join("master")).ok()?;
    link.file_name()?.to_str().map(str::to_string)
}

/// Copy `src` to a new file `dst` as a reflink (shared extents) if the
/// filesystem supports it, else as a regular copy, and return the open
/// destination.
///
/// `dst` is created `O_CREAT|O_EXCL` (which never follows a symlink) and the
/// fallback copy goes into that same open file, never back through the
/// path: the caller may be working in a directory the guest can write, and
/// the returned handle is what it should keep acting on. The fallback copy
/// keeps the source's holes.
///
/// # Errors
/// `AlreadyExists` if anything, including a symlink, is at `dst`; or the OS
/// error from copying.
pub fn clone_file(src: &Path, dst: &Path) -> io::Result<(File, CloneKind)> {
    let mut from = File::open(src)?;
    let mut to = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(dst)?;
    if rustix::fs::ioctl_ficlone(&to, &from).is_ok() {
        return Ok((to, CloneKind::Reflink));
    }
    // No reflink support here (EOPNOTSUPP, EXDEV, EINVAL): a copy into the
    // file already open, of the data only, so the source's holes stay holes.
    sparse_copy(&mut from, &mut to)?;
    Ok((to, CloneKind::Copy))
}

/// Copy `from`'s data extents into `to` at the same offsets, and set the
/// length: holes are skipped, not written as zeros. A disk image is mostly
/// holes, and a byte-for-byte copy would allocate all of it.
///
/// `SEEK_DATA`/`SEEK_HOLE` are supported by every filesystem banlieue
/// targets; on one without them the whole file is a single data extent,
/// which is a plain copy.
fn sparse_copy(from: &mut File, to: &mut File) -> io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let len = from.metadata()?.len();
    let mut pos = 0;
    while pos < len {
        let data = match rustix::fs::seek(&*from, rustix::fs::SeekFrom::Data(pos)) {
            Ok(offset) => offset,
            // ENXIO: no data at or after `pos`, only a trailing hole.
            Err(rustix::io::Errno::NXIO) => break,
            Err(e) => return Err(e.into()),
        };
        let hole = rustix::fs::seek(&*from, rustix::fs::SeekFrom::Hole(data))?.min(len);
        from.seek(SeekFrom::Start(data))?;
        to.seek(SeekFrom::Start(data))?;
        io::copy(&mut Read::by_ref(from).take(hole - data), to)?;
        pos = hole;
    }
    to.set_len(len)
}

/// How [`clone_file`] produced the copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneKind {
    /// Shared extents; instant, no extra space until written.
    Reflink,
    /// A full copy.
    Copy,
}

/// The provider's effective gid: the group guest sockets and directories are
/// shared with (ADR-0063 Decision 5).
#[must_use]
pub fn effective_gid() -> u32 {
    rustix::process::getegid().as_raw()
}

/// The provider's effective uid.
#[must_use]
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(test)]
#[path = "sys_tests.rs"]
mod sys_tests;
