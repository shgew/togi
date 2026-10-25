//! Switching between night and day: the boot loader's selection behind the one seam [`Selection`].
//!
//! A night is the tuning boot entered on purpose, and it lasts until the operator ends it or the
//! tuner stops on its own; no clock ends it. Beginning one selects the tuning entry and remembers
//! the selection it replaced; returning to the day restores that selection exactly and never
//! selects a particular generation. Crash reboots in between stay in the night, because the
//! selection stays. Menu-picked tuning boots follow their loader's default policy; GRUB adopts
//! the choice, while systemd-boot saves it only with effective `@saved`.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;

use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Serialize};

use crate::{Bootloader, SavedEntry};

/// GRUB's variable naming the entry `default=saved` boots.
pub const SAVED_ENTRY_VARIABLE: &str = "saved_entry";
/// The `saved_entry` the day had before the last night [`Selection::begin`] began; unset when the
/// day boots the boot loader's default. It outlasts the night, so ending twice restores the same
/// entry twice.
pub const DAY_ENTRY_VARIABLE: &str = "togi_day_entry";
/// Set, to [`NIGHT_BEGUN`], from [`Selection::begin`] until the night ends. Without it the day entry
/// belongs to a night already over, and a tuning boot picked from the menu does not return to it.
pub const NIGHT_VARIABLE: &str = "togi_night";
/// The value of [`NIGHT_VARIABLE`] while a night lasts.
pub const NIGHT_BEGUN: &str = "1";
/// Where the NixOS module writes the [`Setup`].
pub const SETUP_PATH: &str = "/etc/togi/night.json";

/// The longest entry name kept, so the night's variables leave room in GRUB's 1024-byte block.
const MAX_ENTRY_BYTES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum NightError {
    #[error("read the selected boot entry")]
    ReadSelection(#[source] io::Error),
    #[error("select the tuning entry")]
    Select(#[source] io::Error),
    #[error("the tuning entry {expected:?} is not selected after selecting it: found {found:?}")]
    NotSelected {
        expected: String,
        found: Option<String>,
    },
    #[error("restore the day's boot entry")]
    Restore(#[source] io::Error),
}

/// Why the setup was refused.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("setup is malformed")]
    Syntax(#[source] serde_json::Error),
    #[error("entry must have 1 to 128 printable ASCII bytes")]
    Entry,
}

/// A boot entry as the boot loader names it: 1 to 128 printable ASCII bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Entry(String);

impl Entry {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Entry {
    type Error = SetupError;

    fn try_from(value: String) -> Result<Self, SetupError> {
        let printable = value.bytes().all(|byte| (b' '..=b'~').contains(&byte));
        if (1..=MAX_ENTRY_BYTES).contains(&value.len()) && printable {
            Ok(Self(value))
        } else {
            Err(SetupError::Entry)
        }
    }
}

impl From<Entry> for String {
    fn from(entry: Entry) -> Self {
        entry.0
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the NixOS module tells the night switch about the installed tuning boot ([`SETUP_PATH`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "loader", deny_unknown_fields)]
pub enum Setup {
    #[serde(rename = "grub")]
    Grub { grubenv: PathBuf, entry: Entry },
    #[serde(rename = "systemd-boot")]
    SystemdBoot {
        esp: PathBuf,
        boot: PathBuf,
        state: PathBuf,
        specialisation: Entry,
    },
}

impl Setup {
    /// The boot selection passed to the host: GRUB's environment or the module's loader setup.
    pub fn boot_path(&self) -> &std::path::Path {
        match self {
            Self::Grub { grubenv, .. } => grubenv,
            Self::SystemdBoot { .. } => std::path::Path::new(SETUP_PATH),
        }
    }

    pub fn entry(&self) -> &Entry {
        match self {
            Self::Grub { entry, .. } => entry,
            Self::SystemdBoot { specialisation, .. } => specialisation,
        }
    }
}

impl FromStr for Setup {
    type Err = SetupError;

    fn from_str(text: &str) -> Result<Self, SetupError> {
        object_only(text).map_err(SetupError::Syntax)
    }
}

/// Decodes `text` only when it is a JSON object: serde's derived `Deserialize` for a struct also
/// accepts an array of the fields in declaration order, which would set them by position.
fn object_only<T: DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    if text.trim_start().starts_with('{') {
        serde_json::from_str(text)
    } else {
        Err(de::Error::custom("expected a JSON object"))
    }
}

/// The boot loader's selection of the next boot, as the night switch uses it. [`BootSelection`]
/// delegates to the selected boot loader, keeping loader details out of commands.
pub trait Selection {
    /// Selects `tuning` for every following boot, remembering the selection it replaces. Beginning
    /// while `tuning` is already selected changes nothing, so a second beginning keeps the day the
    /// first one remembered.
    ///
    /// # Errors
    ///
    /// The selection cannot be read or written, or does not read back as `tuning`.
    fn begin(&mut self, tuning: &Entry) -> Result<(), NightError>;

    /// Restores the day's selection exactly: [`Bootloader::clear_saved_entry`], which a boot loader
    /// implements by restoring what [`Selection::begin`] remembered. A tuning boot picked from the
    /// boot menu has no remembered selection, and its day is the boot loader's default.
    ///
    /// # Errors
    ///
    /// The selection cannot be restored or does not read back as the day's.
    fn end(&mut self) -> Result<SavedEntry, NightError>;
}

/// The night switch over the installed boot loader.
pub struct BootSelection(Box<dyn Bootloader>);

impl BootSelection {
    #[must_use]
    pub fn new(bootloader: Box<dyn Bootloader>) -> Self {
        Self(bootloader)
    }
}

impl Selection for BootSelection {
    fn begin(&mut self, tuning: &Entry) -> Result<(), NightError> {
        self.0.select_tuning_entry(tuning)
    }

    fn end(&mut self) -> Result<SavedEntry, NightError> {
        self.0.clear_saved_entry().map_err(NightError::Restore)
    }
}

#[cfg(test)]
mod tests;
