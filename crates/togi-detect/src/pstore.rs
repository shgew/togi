use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, open, openat, statat};
use rustix::io::{Errno, pread};
use rustix::process::geteuid;

use crate::boot::JournalBoot;
use crate::journal::BootEntry;

pub(crate) const PSTORE_DIR: &str = "/var/lib/systemd/pstore";
const HEADER_BYTES: usize = 128;
const TAIL_BYTES: u64 = 3968;
const TAIL_LINES: usize = 32;
const DUMP_FILE: &str = "dmesg.txt";

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const REGULAR: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    List,
    Open,
    Inspect,
    ReadHeader,
    ReadTail,
}

impl std::fmt::Display for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::List => "list",
            Self::Open => "open",
            Self::Inspect => "inspect",
            Self::ReadHeader => "read the header of",
            Self::ReadTail => "read the tail of",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unsafe {
    NotRegular,
    Owner,
    Writable,
    Linked,
}

impl std::fmt::Display for Unsafe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotRegular => "is not a regular file",
            Self::Owner => "is owned by neither root nor this user",
            Self::Writable => "is writable by its group or others",
            Self::Linked => "has more than one hard link",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PstoreError {
    #[error("cannot {access} the pstore archive {}", path.display())]
    Io {
        access: Access,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the pstore archive {} {reason}", path.display())]
    Unsafe { path: PathBuf, reason: Unsafe },
    #[error("the pstore record {} has no valid dump identity", path.display())]
    Identity { path: PathBuf },
}

impl PstoreError {
    fn io(access: Access, path: &Path, errno: Errno) -> Self {
        Self::Io {
            access,
            path: path.to_owned(),
            source: errno.into(),
        }
    }
}

/// The wall-clock interval a boot ran, from the first entries of it and of its successor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BootSpan {
    start_micros: i64,
    end_micros: i64,
}

impl BootSpan {
    pub(crate) fn of(boots: &[BootEntry], boot: &JournalBoot) -> Option<Self> {
        boots.windows(2).find_map(|pair| {
            let [own, next] = pair else {
                return None;
            };
            let start = own.first_entry.filter(|&first| first > 0)?;
            let end = next.first_entry.filter(|&first| first > start)?;
            (own.boot_id == boot.as_str()).then_some(Self {
                start_micros: start,
                end_micros: end,
            })
        })
    }

    fn contains_second(self, seconds: i64) -> bool {
        seconds
            .checked_mul(1_000_000)
            .is_some_and(|micros| (self.start_micros..self.end_micros).contains(&micros))
    }
}

/// Which dump of a boot a chunk belongs to, and its place in it. The chunk with the lowest part
/// holds the newest messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DumpId {
    count: u64,
    part: u64,
}

impl DumpId {
    pub(crate) fn of_header(header: &[u8]) -> Option<Self> {
        let first_line = header.split(|&byte| byte == b'\n').next()?;
        if first_line.len() == header.len() {
            return None;
        }
        let line = String::from_utf8_lossy(first_line);
        let (_, identity) = line.split_once('#')?;
        let (count, part) = identity.split_once(" Part")?;
        let part = decimal(part)?;
        (part > 0).then_some(Self {
            count: decimal(count)?,
            part,
        })
    }

    fn supersedes(self, other: Self) -> bool {
        self.count > other.count || self.count == other.count && self.part < other.part
    }
}

fn decimal(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())?
}

/// The newest lines of a dump file. `truncated` says the window starts inside the file, so its
/// first line may be cut.
pub(crate) fn tail_lines(window: &[u8], truncated: bool) -> Vec<String> {
    let mut data = window;
    if truncated {
        data = if let Some(newline) = data.iter().position(|&byte| byte == b'\n') {
            &data[newline + 1..]
        } else {
            let start = data
                .iter()
                .position(|&byte| byte & 0xC0 != 0x80)
                .unwrap_or(data.len());
            &data[start..]
        };
    }
    let data = data.strip_suffix(b"\n").unwrap_or(data);
    let lines: Vec<&[u8]> = data.split(|&byte| byte == b'\n').collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..]
        .iter()
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect()
}

struct Stamp {
    seconds: i64,
    name: OsString,
}

/// The pstore archive directory, held by descriptor so nothing below it is reached by path.
pub(crate) struct Archive<D = OwnedFd> {
    root: PathBuf,
    directory: D,
    stamps: Vec<Stamp>,
}

pub(crate) struct Dump<F = OwnedFd> {
    path: PathBuf,
    file: F,
}

struct Candidate<F> {
    id: DumpId,
    dump: Dump<F>,
}

pub(crate) trait Directory: Sized {
    type File: ReadAt;
    fn entries(&self, path: &Path) -> Result<Vec<(OsString, FileType)>, PstoreError>;
    fn is_directory(
        &self,
        name: &OsString,
        kind: FileType,
        path: &Path,
    ) -> Result<bool, PstoreError>;
    fn directory(&self, name: &OsString, path: &Path) -> Result<Self, PstoreError>;
    fn dump(&self, path: &Path) -> Result<Option<Dump<Self::File>>, PstoreError>;
}

pub(crate) trait ReadAt {
    fn read(&self, buffer: &mut [u8], offset: u64) -> Result<usize, Errno>;
    fn size(&self) -> Result<u64, Errno>;
}

impl ReadAt for OwnedFd {
    fn read(&self, buffer: &mut [u8], offset: u64) -> Result<usize, Errno> {
        pread(self, buffer, offset)
    }

    fn size(&self) -> Result<u64, Errno> {
        Ok(u64::try_from(fstat(self)?.st_size).unwrap_or(0))
    }
}

impl Directory for OwnedFd {
    type File = OwnedFd;

    fn entries(&self, path: &Path) -> Result<Vec<(OsString, FileType)>, PstoreError> {
        list(self, path)
    }

    fn is_directory(
        &self,
        name: &OsString,
        kind: FileType,
        path: &Path,
    ) -> Result<bool, PstoreError> {
        is_directory(self, name, kind, path)
    }

    fn directory(&self, name: &OsString, path: &Path) -> Result<Self, PstoreError> {
        open_directory(self, name, path)
    }

    fn dump(&self, path: &Path) -> Result<Option<Dump>, PstoreError> {
        open_dump(self, path)
    }
}

impl Archive {
    /// `None` when the archive does not exist or holds nothing.
    pub(crate) fn open(root: &Path) -> Result<Option<Self>, PstoreError> {
        let directory = match open(root, DIRECTORY, Mode::empty()) {
            Ok(directory) => directory,
            Err(Errno::NOENT) => return Ok(None),
            Err(errno) => return Err(PstoreError::io(Access::Open, root, errno)),
        };
        check_directory(&directory, root)?;
        Self::from_directory(root, directory)
    }
}

impl<D: Directory> Archive<D> {
    pub(crate) fn from_directory(root: &Path, directory: D) -> Result<Option<Self>, PstoreError> {
        let entries = directory.entries(root)?;
        if entries.is_empty() {
            return Ok(None);
        }
        let mut stamps = Vec::new();
        for (name, kind) in entries {
            let Some(seconds) = name
                .to_str()
                .and_then(decimal)
                .and_then(|seconds| i64::try_from(seconds).ok())
                .filter(|&seconds| seconds > 0)
            else {
                continue;
            };
            if directory.is_directory(&name, kind, root)? {
                stamps.push(Stamp { seconds, name });
            }
        }
        stamps.sort_by_key(|stamp| stamp.seconds);
        Ok(Some(Self {
            root: root.to_owned(),
            directory,
            stamps,
        }))
    }

    /// The dump with the highest count within `span`, preferring its lowest part.
    pub(crate) fn newest_dump(&self, span: BootSpan) -> Result<Option<Dump<D::File>>, PstoreError> {
        let mut best: Option<Candidate<D::File>> = None;
        for stamp in self
            .stamps
            .iter()
            .filter(|stamp| span.contains_second(stamp.seconds))
        {
            let stamp_path = self.root.join(&stamp.name);
            let stamp_dir = self.directory.directory(&stamp.name, &stamp_path)?;
            for (name, kind) in stamp_dir.entries(&stamp_path)? {
                let is_suffix = name.len() == 3 && name.as_bytes().iter().all(u8::is_ascii_digit);
                if !is_suffix || !stamp_dir.is_directory(&name, kind, &stamp_path)? {
                    continue;
                }
                let suffix_path = stamp_path.join(&name);
                let suffix_dir = stamp_dir.directory(&name, &suffix_path)?;
                let Some(dump) = suffix_dir.dump(&suffix_path)? else {
                    continue;
                };
                let id = dump.identity()?;
                if best
                    .as_ref()
                    .is_none_or(|current| id.supersedes(current.id))
                {
                    best = Some(Candidate { id, dump });
                }
            }
        }
        Ok(best.map(|candidate| candidate.dump))
    }
}

impl<F: ReadAt> Dump<F> {
    pub(crate) fn from_reader(path: PathBuf, file: F) -> Self {
        Self { path, file }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn identity(&self) -> Result<DumpId, PstoreError> {
        let mut header = [0; HEADER_BYTES];
        let length = read_at(&self.file, &mut header, 0, true)
            .map_err(|errno| PstoreError::io(Access::ReadHeader, &self.path, errno))?;
        DumpId::of_header(&header[..length]).ok_or_else(|| PstoreError::Identity {
            path: self.path.clone(),
        })
    }

    /// The last lines of the file, read from the descriptor whose header was checked.
    pub(crate) fn tail(&self) -> Result<Vec<String>, PstoreError> {
        let fail = |errno| PstoreError::io(Access::ReadTail, &self.path, errno);
        let size = self.file.size().map_err(fail)?;
        let length = size.min(TAIL_BYTES);
        let mut window = vec![0; usize::try_from(length).unwrap_or(0)];
        let read = read_at(&self.file, &mut window, size - length, false).map_err(fail)?;
        window.truncate(read);
        Ok(tail_lines(&window, size > length))
    }
}

pub(crate) fn read_at(
    file: &impl ReadAt,
    buffer: &mut [u8],
    offset: u64,
    until_eof: bool,
) -> Result<usize, Errno> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read(&mut buffer[filled..], offset + filled as u64)?;
        if read == 0 {
            return if until_eof {
                Ok(filled)
            } else {
                Err(Errno::IO)
            };
        }
        filled += read;
    }
    Ok(filled)
}

fn list(directory: &OwnedFd, path: &Path) -> Result<Vec<(OsString, FileType)>, PstoreError> {
    let fail = |errno| PstoreError::io(Access::List, path, errno);
    let mut entries = Vec::new();
    for entry in Dir::read_from(directory).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            entries.push((OsString::from_vec(name.to_vec()), entry.file_type()));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

pub(crate) fn is_directory(
    parent: &impl AsFd,
    name: &OsString,
    kind: FileType,
    path: &Path,
) -> Result<bool, PstoreError> {
    match kind {
        FileType::Unknown => {
            let stat = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|errno| PstoreError::io(Access::Inspect, &path.join(name), errno))?;
            Ok(FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
        }
        kind => Ok(kind == FileType::Directory),
    }
}

fn owned_by_us(stat: &Stat) -> bool {
    stat.st_uid == 0 || stat.st_uid == geteuid().as_raw()
}

fn check_directory(directory: &OwnedFd, path: &Path) -> Result<(), PstoreError> {
    let stat = fstat(directory).map_err(|errno| PstoreError::io(Access::Inspect, path, errno))?;
    let reason = if !owned_by_us(&stat) {
        Unsafe::Owner
    } else if Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH) {
        Unsafe::Writable
    } else {
        return Ok(());
    };
    Err(PstoreError::Unsafe {
        path: path.to_owned(),
        reason,
    })
}

fn open_directory(parent: &OwnedFd, name: &OsString, path: &Path) -> Result<OwnedFd, PstoreError> {
    let directory = openat(parent, name, DIRECTORY, Mode::empty())
        .map_err(|errno| PstoreError::io(Access::Open, path, errno))?;
    check_directory(&directory, path)?;
    Ok(directory)
}

fn open_dump(suffix_dir: &OwnedFd, suffix_path: &Path) -> Result<Option<Dump>, PstoreError> {
    let path = suffix_path.join(DUMP_FILE);
    let file = match openat(suffix_dir, DUMP_FILE, REGULAR, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(errno) => return Err(PstoreError::io(Access::Open, &path, errno)),
    };
    let stat = fstat(&file).map_err(|errno| PstoreError::io(Access::Inspect, &path, errno))?;
    let reason = if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        Unsafe::NotRegular
    } else if !owned_by_us(&stat) {
        Unsafe::Owner
    } else if Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH) {
        Unsafe::Writable
    } else if stat.st_nlink != 1 {
        Unsafe::Linked
    } else {
        return Ok(Some(Dump::from_reader(path, file)));
    };
    Err(PstoreError::Unsafe { path, reason })
}
