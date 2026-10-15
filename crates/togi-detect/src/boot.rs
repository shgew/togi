use togi_machine::BootId;

use crate::error::{BootIdError, Error};

/// A boot id as `journalctl -b` and the journal's `_BOOT_ID` write it: 32 lowercase hex digits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JournalBoot(String);

impl JournalBoot {
    pub(crate) fn parse(id: &str) -> Result<Self, Error> {
        let hex: String = id.chars().filter(|&c| c != '-').collect();
        if hex.len() == 32 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(hex.to_ascii_lowercase()))
        } else {
            Err(Error::InvalidBootId { id: id.to_owned() })
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn checked(text: &str) -> Result<BootId, BootIdError> {
    let id = text.trim();
    JournalBoot::parse(id).map_err(|_| BootIdError::Invalid(id.to_owned()))?;
    Ok(BootId::from(id.to_owned()))
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn read_boot_id(
    read: impl FnOnce() -> std::io::Result<String>,
) -> Result<BootId, Error> {
    let text = read().map_err(|source| Error::BootId(source.into()))?;
    checked(&text).map_err(Error::BootId)
}

#[cfg(target_os = "linux")]
pub fn boot_id() -> Result<BootId, Error> {
    read_boot_id(|| std::fs::read_to_string("/proc/sys/kernel/random/boot_id"))
}

#[cfg(target_os = "macos")]
pub fn boot_id() -> Result<BootId, Error> {
    use std::time::Duration;
    use togi_command::Invocation;

    const SYSCTL_DEADLINE: Duration = Duration::from_secs(10);
    let invocation = Invocation::new("/usr/sbin/sysctl")
        .arg("-n")
        .arg("kern.bootsessionuuid");
    #[expect(
        clippy::disallowed_methods,
        reason = "sysctl is the macOS boot session probe, detect's approved command besides journalctl"
    )]
    let output = togi_command::run_blocking(&invocation, SYSCTL_DEADLINE)
        .map_err(|source| Error::BootId(source.into()))?;
    if !output.status.success() {
        return Err(Error::BootId(BootIdError::Exit {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| Error::BootId(BootIdError::NotText))?;
    checked(&text).map_err(Error::BootId)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn boot_id() -> Result<BootId, Error> {
    Err(Error::BootId(BootIdError::Unsupported))
}
