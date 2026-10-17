use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;

use rustix::fs::OFlags;
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

/// A fresh pseudo-terminal for tests that need a real terminal: the master and its slave, neither
/// becoming the controlling terminal and neither inherited across `exec`.
///
/// The slave opens by the raw bytes of its name. The caller owns both descriptors and decides when
/// each closes: a slave handed to a child is moved, and the master stays open as long as the slave
/// must read or write.
///
/// # Errors
///
/// Fails when the system cannot allocate, unlock or name a pseudo-terminal, or open its slave.
pub fn open_pty() -> io::Result<(OwnedFd, File)> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    fcntl_setfd(&master, FdFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = File::options()
        .read(true)
        .write(true)
        .custom_flags(OFlags::NOCTTY.bits().cast_signed())
        .open(OsStr::from_bytes(name.as_bytes()))?;
    Ok((master, slave))
}
