use crate::files::{Files, read_limited};
use crate::mailbox::{Command, MailboxError, Poison, Status, Transport};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub(crate) const DRIVER_DIR: &str = "sys/kernel/ryzen_smu_drv";

pub(crate) struct Sysfs {
    dir: PathBuf,
    files: Files,
}
const MAX_FILE: u64 = 4096;

impl Sysfs {
    pub(crate) fn locate(root: &Path) -> Option<Self> {
        let dir = root.join(DRIVER_DIR);
        dir.is_dir().then_some(Self {
            dir,
            files: Files::System,
        })
    }

    pub(crate) fn from_readers(root: &Path, files: Files) -> Self {
        Self {
            dir: root.join(DRIVER_DIR),
            files,
        }
    }

    fn write(&self, name: &str, data: &[u8], poison: &Poison) -> Result<(), MailboxError> {
        poison.check()?;
        let path = self.dir.join(name);
        let written = match &self.files {
            Files::System => {
                let mut file = OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(|source| MailboxError::Open {
                        path: path.clone(),
                        source,
                    })?;
                poison.check()?;
                file.write(data)
            }
            Files::Readers(readers) => match (readers.write)(&path, data) {
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    return Err(MailboxError::Open { path, source });
                }
                outcome => outcome,
            },
        };
        match written {
            Ok(written) if written == data.len() => Ok(()),
            Ok(written) => Err(MailboxError::ShortWrite {
                path,
                written,
                expected: data.len(),
            }),
            Err(source) => Err(MailboxError::Write { path, source }),
        }
    }

    fn read<const N: usize>(&self, name: &str, poison: &Poison) -> Result<[u8; N], MailboxError> {
        poison.check()?;
        let path = self.dir.join(name);
        let bytes = match &self.files {
            Files::System => File::open(&path)
                .and_then(|file| read_limited(GatedRead { file, poison }, MAX_FILE, N)),
            Files::Readers(_) => self.files.read(&path, MAX_FILE, N),
        }
        .map_err(|source| MailboxError::Read {
            path: path.clone(),
            source,
        })?;
        let got = bytes.len();
        <[u8; N]>::try_from(bytes).map_err(|_| MailboxError::Length { path, got, want: N })
    }

    pub(crate) fn transact(
        &self,
        code: u32,
        args: [u32; 6],
        poison: &Poison,
    ) -> Result<[u32; 6], MailboxError> {
        let mut packed = [0_u8; 24];
        let (slots, _) = packed.as_chunks_mut::<4>();
        for (slot, arg) in slots.iter_mut().zip(args) {
            *slot = arg.to_le_bytes();
        }
        self.write("smu_args", &packed, poison)?;
        self.write("rsmu_cmd", &code.to_le_bytes(), poison)?;
        let status = u32::from_le_bytes(self.read::<4>("rsmu_cmd", poison)?);
        Status::check(status).map_err(|status| MailboxError::Status {
            command: code,
            status,
        })?;
        let response = self.read::<24>("smu_args", poison)?;
        let mut words = [0_u32; 6];
        let (chunks, _) = response.as_chunks::<4>();
        for (word, chunk) in words.iter_mut().zip(chunks) {
            *word = u32::from_le_bytes(*chunk);
        }
        Ok(words)
    }
}

impl Transport for Sysfs {
    fn command(
        &mut self,
        command: Command,
        args: [u32; 6],
        poison: &Poison,
    ) -> Result<[u32; 6], MailboxError> {
        self.transact(command.code(), args, poison)
    }

    fn read_smn(&mut self, address: u32, poison: &Poison) -> Result<u32, MailboxError> {
        self.write("smn", &address.to_le_bytes(), poison)?;
        Ok(u32::from_le_bytes(self.read::<4>("smn", poison)?))
    }
}

/// `read_to_end` can make several kernel calls; each needs its own poison check.
struct GatedRead<'a> {
    file: File,
    poison: &'a Poison,
}

impl Read for GatedRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.poison.check().map_err(io::Error::other)?;
        self.file.read(buffer)
    }
}
