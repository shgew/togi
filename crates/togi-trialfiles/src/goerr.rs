//! Go's error text for the failures [`crate::compress`] reports. `trialfiles.Compress` errors are written verbatim into
//! the journal, so each one renders like the `*os.PathError` or `*os.LinkError` Go would have returned, with the text
//! of `syscall.Errno.Error()` from Go's errors table for the target (`zerrors_linux_amd64.go`, `zerrors_linux_arm64.go`
//! and `zerrors_darwin_arm64.go`; the two Linux tables differ only by errno 133 and the Apple table is arm64's, which
//! has errno 106 that Go's darwin/amd64 table lacks). An errno outside the table renders as `errno N`. Paths are Go
//! strings: the lexically cleaned result of `filepath.Join`, rendered lossily when they are not UTF-8 (the journal's
//! JSON encoder does the same).

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rustix::io::Errno;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Go's errno tables are transcribed for Linux and macOS only");

/// `syscall.Errno.Error()`: Go's table text, or `errno N` outside it.
pub(crate) fn errno_text(code: i32) -> Cow<'static, str> {
    match known_errno_text(code) {
        Some(text) => Cow::Borrowed(text),
        None => Cow::Owned(format!("errno {code}")),
    }
}

#[cfg(target_os = "linux")]
fn known_errno_text(code: i32) -> Option<&'static str> {
    Some(match code {
        1 => "operation not permitted",
        2 => "no such file or directory",
        3 => "no such process",
        4 => "interrupted system call",
        5 => "input/output error",
        6 => "no such device or address",
        7 => "argument list too long",
        8 => "exec format error",
        9 => "bad file descriptor",
        10 => "no child processes",
        11 => "resource temporarily unavailable",
        12 => "cannot allocate memory",
        13 => "permission denied",
        14 => "bad address",
        15 => "block device required",
        16 => "device or resource busy",
        17 => "file exists",
        18 => "invalid cross-device link",
        19 => "no such device",
        20 => "not a directory",
        21 => "is a directory",
        22 => "invalid argument",
        23 => "too many open files in system",
        24 => "too many open files",
        25 => "inappropriate ioctl for device",
        26 => "text file busy",
        27 => "file too large",
        28 => "no space left on device",
        29 => "illegal seek",
        30 => "read-only file system",
        31 => "too many links",
        32 => "broken pipe",
        33 => "numerical argument out of domain",
        34 => "numerical result out of range",
        35 => "resource deadlock avoided",
        36 => "file name too long",
        37 => "no locks available",
        38 => "function not implemented",
        39 => "directory not empty",
        40 => "too many levels of symbolic links",
        42 => "no message of desired type",
        43 => "identifier removed",
        44 => "channel number out of range",
        45 => "level 2 not synchronized",
        46 => "level 3 halted",
        47 => "level 3 reset",
        48 => "link number out of range",
        49 => "protocol driver not attached",
        50 => "no CSI structure available",
        51 => "level 2 halted",
        52 => "invalid exchange",
        53 => "invalid request descriptor",
        54 => "exchange full",
        55 => "no anode",
        56 => "invalid request code",
        57 => "invalid slot",
        59 => "bad font file format",
        60 => "device not a stream",
        61 => "no data available",
        62 => "timer expired",
        63 => "out of streams resources",
        64 => "machine is not on the network",
        65 => "package not installed",
        66 => "object is remote",
        67 => "link has been severed",
        68 => "advertise error",
        69 => "srmount error",
        70 => "communication error on send",
        71 => "protocol error",
        72 => "multihop attempted",
        73 => "RFS specific error",
        74 => "bad message",
        75 => "value too large for defined data type",
        76 => "name not unique on network",
        77 => "file descriptor in bad state",
        78 => "remote address changed",
        79 => "can not access a needed shared library",
        80 => "accessing a corrupted shared library",
        81 => ".lib section in a.out corrupted",
        82 => "attempting to link in too many shared libraries",
        83 => "cannot exec a shared library directly",
        84 => "invalid or incomplete multibyte or wide character",
        85 => "interrupted system call should be restarted",
        86 => "streams pipe error",
        87 => "too many users",
        88 => "socket operation on non-socket",
        89 => "destination address required",
        90 => "message too long",
        91 => "protocol wrong type for socket",
        92 => "protocol not available",
        93 => "protocol not supported",
        94 => "socket type not supported",
        95 => "operation not supported",
        96 => "protocol family not supported",
        97 => "address family not supported by protocol",
        98 => "address already in use",
        99 => "cannot assign requested address",
        100 => "network is down",
        101 => "network is unreachable",
        102 => "network dropped connection on reset",
        103 => "software caused connection abort",
        104 => "connection reset by peer",
        105 => "no buffer space available",
        106 => "transport endpoint is already connected",
        107 => "transport endpoint is not connected",
        108 => "cannot send after transport endpoint shutdown",
        109 => "too many references: cannot splice",
        110 => "connection timed out",
        111 => "connection refused",
        112 => "host is down",
        113 => "no route to host",
        114 => "operation already in progress",
        115 => "operation now in progress",
        116 => "stale file handle",
        117 => "structure needs cleaning",
        118 => "not a XENIX named type file",
        119 => "no XENIX semaphores available",
        120 => "is a named type file",
        121 => "remote I/O error",
        122 => "disk quota exceeded",
        123 => "no medium found",
        124 => "wrong medium type",
        125 => "operation canceled",
        126 => "required key not available",
        127 => "key has expired",
        128 => "key has been revoked",
        129 => "key was rejected by service",
        130 => "owner died",
        131 => "state not recoverable",
        132 => "operation not possible due to RF-kill",
        #[cfg(target_arch = "aarch64")]
        133 => "memory page has hardware error",
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
fn known_errno_text(code: i32) -> Option<&'static str> {
    Some(match code {
        1 => "operation not permitted",
        2 => "no such file or directory",
        3 => "no such process",
        4 => "interrupted system call",
        5 => "input/output error",
        6 => "device not configured",
        7 => "argument list too long",
        8 => "exec format error",
        9 => "bad file descriptor",
        10 => "no child processes",
        11 => "resource deadlock avoided",
        12 => "cannot allocate memory",
        13 => "permission denied",
        14 => "bad address",
        15 => "block device required",
        16 => "resource busy",
        17 => "file exists",
        18 => "cross-device link",
        19 => "operation not supported by device",
        20 => "not a directory",
        21 => "is a directory",
        22 => "invalid argument",
        23 => "too many open files in system",
        24 => "too many open files",
        25 => "inappropriate ioctl for device",
        26 => "text file busy",
        27 => "file too large",
        28 => "no space left on device",
        29 => "illegal seek",
        30 => "read-only file system",
        31 => "too many links",
        32 => "broken pipe",
        33 => "numerical argument out of domain",
        34 => "result too large",
        35 => "resource temporarily unavailable",
        36 => "operation now in progress",
        37 => "operation already in progress",
        38 => "socket operation on non-socket",
        39 => "destination address required",
        40 => "message too long",
        41 => "protocol wrong type for socket",
        42 => "protocol not available",
        43 => "protocol not supported",
        44 => "socket type not supported",
        45 => "operation not supported",
        46 => "protocol family not supported",
        47 => "address family not supported by protocol family",
        48 => "address already in use",
        49 => "can't assign requested address",
        50 => "network is down",
        51 => "network is unreachable",
        52 => "network dropped connection on reset",
        53 => "software caused connection abort",
        54 => "connection reset by peer",
        55 => "no buffer space available",
        56 => "socket is already connected",
        57 => "socket is not connected",
        58 => "can't send after socket shutdown",
        59 => "too many references: can't splice",
        60 => "operation timed out",
        61 => "connection refused",
        62 => "too many levels of symbolic links",
        63 => "file name too long",
        64 => "host is down",
        65 => "no route to host",
        66 => "directory not empty",
        67 => "too many processes",
        68 => "too many users",
        69 => "disc quota exceeded",
        70 => "stale NFS file handle",
        71 => "too many levels of remote in path",
        72 => "RPC struct is bad",
        73 => "RPC version wrong",
        74 => "RPC prog. not avail",
        75 => "program version wrong",
        76 => "bad procedure for program",
        77 => "no locks available",
        78 => "function not implemented",
        79 => "inappropriate file type or format",
        80 => "authentication error",
        81 => "need authenticator",
        82 => "device power is off",
        83 => "device error",
        84 => "value too large to be stored in data type",
        85 => "bad executable (or shared library)",
        86 => "bad CPU type in executable",
        87 => "shared library version mismatch",
        88 => "malformed Mach-o file",
        89 => "operation canceled",
        90 => "identifier removed",
        91 => "no message of desired type",
        92 => "illegal byte sequence",
        93 => "attribute not found",
        94 => "bad message",
        95 => "EMULTIHOP (Reserved)",
        96 => "no message available on STREAM",
        97 => "ENOLINK (Reserved)",
        98 => "no STREAM resources",
        99 => "not a STREAM",
        100 => "protocol error",
        101 => "STREAM ioctl timeout",
        102 => "operation not supported on socket",
        103 => "policy not found",
        104 => "state not recoverable",
        105 => "previous owner died",
        106 => "interface output queue is full",
        _ => return None,
    })
}

/// The errno of a failed std file operation; they always carry one, so `EIO` is only a defensive default.
pub(crate) fn errno_of(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(Errno::IO.raw_os_error())
}

fn lossy(path: &Path) -> Cow<'_, str> {
    path.to_string_lossy()
}

/// Go's `*os.PathError`: `op path: errno text`.
#[derive(Debug)]
pub struct PathError {
    op: &'static str,
    path: String,
    source: io::Error,
}

impl PathError {
    pub(crate) fn new(op: &'static str, path: &Path, source: &io::Error) -> Self {
        Self::from_errno(op, path, errno_of(source))
    }

    pub(crate) fn from_errno(op: &'static str, path: &Path, errno: i32) -> Self {
        Self {
            op,
            path: lossy(path).into_owned(),
            source: io::Error::from_raw_os_error(errno),
        }
    }

    /// The failed operation, in Go's spelling.
    #[must_use]
    pub fn op(&self) -> &str {
        self.op
    }

    #[must_use]
    pub fn errno(&self) -> i32 {
        errno_of(&self.source)
    }

    /// Go's `errors.Is(err, os.ErrNotExist)`.
    pub(crate) fn is_not_exist(&self) -> bool {
        self.errno() == Errno::NOENT.raw_os_error()
    }
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.op, self.path, errno_text(self.errno()))
    }
}

impl std::error::Error for PathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Go's `*os.LinkError` for a rename: `rename old new: errno text`.
#[derive(Debug)]
pub struct LinkError {
    old: String,
    new: String,
    source: io::Error,
}

impl LinkError {
    pub(crate) fn rename(old: &Path, new: &Path, errno: i32) -> Self {
        Self {
            old: lossy(old).into_owned(),
            new: lossy(new).into_owned(),
            source: io::Error::from_raw_os_error(errno),
        }
    }

    #[must_use]
    pub fn errno(&self) -> i32 {
        errno_of(&self.source)
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rename {} {}: {}",
            self.old,
            self.new,
            errno_text(self.errno())
        )
    }
}

impl std::error::Error for LinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Either kind of Go OS error.
#[derive(Debug, thiserror::Error)]
pub enum Cause {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Link(#[from] LinkError),
}

/// `filepath.Clean` on Unix: lexical, never touches the file system.
pub(crate) fn clean(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let rooted = bytes.first() == Some(&b'/');
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in bytes.split(|&b| b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => match parts.last() {
                Some(&last) if last != b".." => {
                    parts.pop();
                }
                _ if rooted => {}
                _ => parts.push(part),
            },
            _ => parts.push(part),
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    if rooted {
        out.push(b'/');
    }
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(part);
    }
    if out.is_empty() {
        out.push(b'.');
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// `filepath.Join(dir, name)`: empty elements are ignored and the result is cleaned.
pub(crate) fn join(dir: &Path, name: &OsStr) -> PathBuf {
    if dir.as_os_str().is_empty() {
        return if name.is_empty() {
            PathBuf::new()
        } else {
            clean(Path::new(name))
        };
    }
    let mut joined = dir.as_os_str().to_os_string();
    if !name.is_empty() {
        joined.push("/");
        joined.push(name);
    }
    clean(Path::new(&joined))
}

/// `filepath.Dir` of an already cleaned path.
pub(crate) fn dir(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    match bytes.iter().rposition(|&b| b == b'/') {
        Some(i) => clean(Path::new(OsStr::from_bytes(&bytes[..=i]))),
        None => PathBuf::from("."),
    }
}

/// `filepath.Base`, rendered for an error message.
pub(crate) fn base(path: &Path) -> String {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() {
        return ".".to_owned();
    }
    let Some(end) = bytes.iter().rposition(|&b| b != b'/').map(|i| i + 1) else {
        return "/".to_owned();
    };
    let start = bytes[..end]
        .iter()
        .rposition(|&b| b == b'/')
        .map_or(0, |i| i + 1);
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}
