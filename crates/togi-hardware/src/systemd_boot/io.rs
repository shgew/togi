use std::io;
use std::path::PathBuf;

use super::{BootIo, Identity, LoaderEntry};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::SystemIo;

#[cfg(not(target_os = "linux"))]
pub(super) struct SystemIo;

#[cfg(not(target_os = "linux"))]
impl SystemIo {
    pub(super) fn new(_esp: PathBuf, _boot: PathBuf, _state: PathBuf) -> Self {
        Self
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "systemd-boot requires Linux EFI variable access",
    ))
}

#[cfg(not(target_os = "linux"))]
impl BootIo for SystemIo {
    fn lock(&mut self) -> io::Result<()> {
        unsupported()
    }
    fn unlock(&mut self) {}
    fn read_variable(&mut self, _name: &str) -> io::Result<Option<Vec<u8>>> {
        unsupported()
    }
    fn write_variable(&mut self, _name: &str, _value: Option<&[u8]>) -> io::Result<()> {
        unsupported()
    }
    fn load_state(&mut self) -> io::Result<Option<Vec<u8>>> {
        unsupported()
    }
    fn store_state(&mut self, _value: &[u8]) -> io::Result<()> {
        unsupported()
    }
    fn entries(&mut self) -> io::Result<Vec<LoaderEntry>> {
        unsupported()
    }
    fn identity(&mut self, _specialisation: &str, _running: bool) -> io::Result<Identity> {
        unsupported()
    }
    fn configured_default(&mut self) -> io::Result<Option<String>> {
        unsupported()
    }
    fn boot_id(&mut self) -> io::Result<String> {
        unsupported()
    }
}
