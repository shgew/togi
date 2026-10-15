use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use rustix::fs::{Mode, OFlags};

const LIMIT: usize = 4096;

/// Reads a small sysfs attribute: a regular file, opened without blocking on a FIFO, at most 4 KiB.
pub(crate) fn read_attribute(path: &Path) -> io::Result<String> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let file = File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    read_attribute_from(Ok(file))
}

pub(crate) fn read_attribute_from(source: io::Result<impl Read>) -> io::Result<String> {
    let file = source?;
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(io::Error::other(format!("larger than {LIMIT} bytes")));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
