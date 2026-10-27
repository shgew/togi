use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{IFlags, ioctl_getflags, ioctl_setflags};

use super::{BootIo, Identity, LoaderEntry, PathBuf, io};
use crate::systemd_boot::entries::{copied_filename, entry_id, fields};

const EFI_ROOT: &str = "/sys/firmware/efi/efivars";
const VENDOR: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
const LIMIT: u64 = 1024 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) struct SystemIo {
    esp: PathBuf,
    boot: PathBuf,
    state: PathBuf,
    lock: Option<File>,
}

impl SystemIo {
    pub(crate) fn new(esp: PathBuf, boot: PathBuf, state: PathBuf) -> Self {
        Self {
            esp,
            boot,
            state,
            lock: None,
        }
    }
}

impl BootIo for SystemIo {
    fn lock(&mut self) -> io::Result<()> {
        durable_directory(&self.state)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.state.join("backend.lock"))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
        self.lock = Some(file);
        Ok(())
    }

    fn unlock(&mut self) {
        self.lock = None;
    }

    fn read_variable(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        // efivarfs can retain an uncommitted zero-length inode after failed creation or resume.
        // The firmware has no variable in that case, just as for ENOENT.
        Ok(optional_read(&variable_path(name))?.filter(|bytes| !bytes.is_empty()))
    }

    fn write_variable(&mut self, name: &str, value: Option<&[u8]>) -> io::Result<()> {
        let path = variable_path(name);
        // Only the two loader variables the policy owns reach this operation. Never clear flags
        // on arbitrary efivarfs nodes: deleting unrelated vendor variables can brick firmware.
        let existing = match File::open(&path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let flags = existing.as_ref().map(ioctl_getflags).transpose()?;
        if let (Some(file), Some(flags)) = (&existing, flags) {
            if flags.contains(IFlags::IMMUTABLE) {
                ioctl_setflags(file, flags - IFlags::IMMUTABLE)?;
            }
        }
        let result = match value {
            Some(bytes) => write_efi(&path, bytes),
            None => match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        };
        // Restore the original immutable flag even after a failed write/delete. A successful
        // deletion has no inode left to protect. Attribute errors must not disappear on success.
        let restore = if let (Some(file), Some(flags)) = (&existing, flags) {
            if flags.contains(IFlags::IMMUTABLE) && (value.is_some() || result.is_err()) {
                ioctl_setflags(file, flags).map_err(io::Error::from)
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };
        let operation = match value {
            Some(_) => Operation::Write,
            None => Operation::Delete,
        };
        combine(operation, name, result, restore)
    }

    fn load_state(&mut self) -> io::Result<Option<Vec<u8>>> {
        optional_read(&self.state.join("backend.json"))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "production loader-state durability adapter; ordinary tests inject memory I/O"
    )]
    fn store_state(&mut self, value: &[u8]) -> io::Result<()> {
        durable_directory(&self.state)?;
        let (temporary, mut file) = loop {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let temporary = self
                .state
                .join(format!(".backend-{}-{id}.tmp", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => break (temporary, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        };
        let publish = (|| {
            file.write_all(value)?;
            file.sync_all()?;
            fs::rename(&temporary, self.state.join("backend.json"))?;
            File::open(&self.state)?.sync_all()
        })();
        if publish.is_err() {
            match fs::remove_file(temporary) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                // Keep the publication error; the unreferenced temporary file is never read.
                Err(_) => {}
            }
        }
        publish
    }

    fn entries(&mut self) -> io::Result<Vec<LoaderEntry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(self.boot.join("loader/entries"))? {
            let entry = entry?;
            let filename = entry.file_name();
            let Some(filename) = filename.to_str() else {
                continue;
            };
            let Some(id) = entry_id(filename) else {
                continue;
            };
            entries.push(LoaderEntry {
                id,
                contents: read_text(&entry.path())?,
            });
        }
        Ok(entries)
    }

    fn identity(&mut self, specialisation: &str, running: bool) -> io::Result<Identity> {
        if Path::new(specialisation).components().count() != 1
            || !matches!(
                Path::new(specialisation).components().next(),
                Some(Component::Normal(_))
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "specialisation must be one path component",
            ));
        }
        let current = Path::new("/run/current-system");
        let specialisation = current.join("specialisation").join(specialisation);
        let root = match fs::canonicalize(specialisation) {
            Ok(root) => root,
            Err(error) if running && error.kind() == io::ErrorKind::NotFound => {
                fs::canonicalize(current)?
            }
            Err(error) => return Err(error),
        };
        let kernel = copied_filename(&fs::canonicalize(root.join("kernel"))?)?;
        let initrd = copied_filename(&fs::canonicalize(root.join("initrd"))?)?;
        let mut options = vec![format!("init={}/init", root.display())];
        options.extend(
            read_text(&root.join("kernel-params"))?
                .split_whitespace()
                .map(str::to_owned),
        );
        Ok(Identity {
            kernel,
            initrd,
            options,
        })
    }

    fn configured_default(&mut self) -> io::Result<Option<String>> {
        let Some(bytes) = optional_read(&self.esp.join("loader/loader.conf"))? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(fields(&text)
            .filter(|(key, _)| *key == "default")
            .map(|(_, value)| value.to_owned())
            .last())
    }

    fn boot_id(&mut self) -> io::Result<String> {
        Ok(read_text(Path::new("/proc/sys/kernel/random/boot_id"))?
            .trim()
            .to_owned())
    }
}

fn variable_path(name: &str) -> PathBuf {
    Path::new(EFI_ROOT).join(format!("{name}-{VENDOR}"))
}

fn optional_read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot metadata exceeds 1 MiB",
        ));
    }
    Ok(Some(bytes))
}

fn read_text(path: &Path) -> io::Result<String> {
    let bytes = optional_read(path)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "boot metadata is absent"))?;
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_efi(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // efivarfs consumes one complete attribute+data write. A second short-write continuation
    // would be a different firmware transaction, not an ordinary file append. No O_TRUNC.
    let written = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?
        .write(bytes)?;
    if written != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short EFI variable write",
        ));
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "production loader-state directory durability; ordinary tests inject memory I/O"
)]
fn durable_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
            File::open(path)?.sync_all()
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(error)?;
            durable_directory(parent)?;
            durable_directory(path)
        }
        Err(error) => Err(error),
    }
}

/// What `write_variable` did to the EFI variable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Write,
    Delete,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Write => "write",
            Self::Delete => "delete",
        })
    }
}

/// The variable operation failed, and so did restoring the variable's immutable flag afterwards,
/// which may have left the variable unprotected.
#[derive(Debug, thiserror::Error)]
#[error("{operation} EFI variable {name} (restoring its immutable flag then failed: {restore})")]
struct RestoreFailed {
    operation: Operation,
    name: String,
    #[source]
    source: io::Error,
    restore: io::Error,
}

/// The result of a variable operation followed by its flag restoration. A single failure is
/// returned as it is; a double failure keeps both errors, the operation's as the source.
fn combine(
    operation: Operation,
    name: &str,
    result: io::Result<()>,
    restore: io::Result<()>,
) -> io::Result<()> {
    match (result, restore) {
        (Err(source), Err(restore)) => Err(io::Error::other(RestoreFailed {
            operation,
            name: name.to_owned(),
            source,
            restore,
        })),
        (result, restore) => result.and(restore),
    }
}

#[cfg(test)]
mod tests;
