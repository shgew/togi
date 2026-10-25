//! Persistent systemd-boot selection, with EFI and durable state I/O behind one boundary.
//!
//! A night snapshots the exact EFI values before changing the persistent default. Ending restores
//! `LastBooted` before Default, so interruption cannot expose `@saved` with the night's saved entry.
//! A newer default belongs to the operator: neither it nor `LastBooted` is changed on that path.

mod entries;
mod io;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io as stdio;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use togi_tuningboot::night::{Entry, NightError};
use togi_tuningboot::{Bootloader, Environment, SavedEntry};

use entries::{Identity, LoaderEntry};
use io::SystemIo;

const DEFAULT: &str = "LoaderEntryDefault";
const LAST_BOOTED: &str = "LoaderEntryLastBooted";

/// The real boundaries, not the selection policy. EFI bytes include the four attribute bytes.
trait BootIo {
    fn lock(&mut self) -> stdio::Result<()>;
    fn unlock(&mut self);
    fn read_variable(&mut self, name: &str) -> stdio::Result<Option<Vec<u8>>>;
    fn write_variable(&mut self, name: &str, value: Option<&[u8]>) -> stdio::Result<()>;
    fn load_state(&mut self) -> stdio::Result<Option<Vec<u8>>>;
    /// Atomically publish and durably sync both the file and its containing directory.
    fn store_state(&mut self, value: &[u8]) -> stdio::Result<()>;
    fn entries(&mut self) -> stdio::Result<Vec<LoaderEntry>>;
    fn identity(&mut self, specialisation: &str, running: bool) -> stdio::Result<Identity>;
    fn configured_default(&mut self) -> stdio::Result<Option<String>>;
    fn boot_id(&mut self) -> stdio::Result<String>;
}

togi_machine::object_only! {
    #[derive(Debug, Default, Serialize)]
    #[serde(deny_unknown_fields)]
    struct State {
        variables: BTreeMap<String, String>,
        night: Night,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Snapshot {
        selected: String,
        default: Option<Vec<u8>>,
        last_booted: Option<Vec<u8>>,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Ended {
        boot_id: String,
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
enum Night {
    #[default]
    Unrecorded,
    Selected(Snapshot),
    // A repeat in the same boot must not reinterpret a restored @saved value as a menu night.
    Ended(Ended),
}

/// systemd-boot's persistent default and togi's independent durable restart/selection state.
/// Constructing it performs no I/O; unsupported platforms fail when a boundary is used.
pub struct SystemdBoot {
    specialisation: String,
    io: Box<dyn BootIo>,
}

impl SystemdBoot {
    pub fn new(esp: PathBuf, boot: PathBuf, state: PathBuf, specialisation: &str) -> Self {
        Self::with_io(
            specialisation.to_owned(),
            Box::new(SystemIo::new(esp, boot, state)),
        )
    }

    fn with_io(specialisation: String, io: Box<dyn BootIo>) -> Self {
        Self { specialisation, io }
    }

    fn with_lock<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> stdio::Result<T>,
    ) -> stdio::Result<T> {
        self.io.lock()?;
        let result = operation(self);
        self.io.unlock();
        result
    }

    fn state(&mut self) -> stdio::Result<State> {
        match self.io.load_state()? {
            None => Ok(State::default()),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| stdio::Error::new(stdio::ErrorKind::InvalidData, error)),
        }
    }

    fn persist(&mut self, state: &State) -> stdio::Result<()> {
        let bytes = serde_json::to_vec(state).map_err(stdio::Error::other)?;
        self.io.store_state(&bytes)
    }

    fn read_text(&mut self, name: &str) -> stdio::Result<Option<String>> {
        self.io
            .read_variable(name)?
            .as_deref()
            .map(decode)
            .transpose()
    }

    fn write_verified(&mut self, name: &str, value: Option<&[u8]>) -> stdio::Result<()> {
        if self.io.read_variable(name)?.as_deref() != value {
            self.io.write_variable(name, value)?;
        }
        if self.io.read_variable(name)?.as_deref() != value {
            return Err(stdio::Error::other(format!(
                "{name} did not read back after writing"
            )));
        }
        Ok(())
    }

    fn select(&mut self, tuning: &Entry) -> stdio::Result<()> {
        if tuning.as_str() != self.specialisation {
            return Err(stdio::Error::new(
                stdio::ErrorKind::InvalidInput,
                "tuning entry must name the configured specialisation",
            ));
        }
        let mut state = self.state()?;
        let default = self.io.read_variable(DEFAULT)?;
        let current = default.as_deref().map(decode).transpose()?;
        if let Night::Selected(snapshot) = &state.night {
            if current.as_deref() == Some(&snapshot.selected) {
                // Beginning an existing night is inert, including after a configuration switch.
                // Its already pinned generation and first day's snapshot both stay unchanged.
                return Ok(());
            }
        }
        let identity = self.io.identity(&self.specialisation, false)?;
        let selected = entries::resolve(&self.io.entries()?, &identity)?;
        let last_booted = self.io.read_variable(LAST_BOOTED)?;
        state.night = Night::Selected(Snapshot {
            selected: selected.clone(),
            default,
            last_booted,
        });
        // The snapshot reaches stable storage before any EFI mutation, including a failed write.
        self.persist(&state)?;
        self.write_verified(DEFAULT, Some(&encode(&selected)))
    }

    fn clear(&mut self) -> stdio::Result<SavedEntry> {
        let mut state = self.state()?;
        let before = self.read_text(DEFAULT)?;
        let boot_id = self.io.boot_id()?;
        match &state.night {
            Night::Selected(snapshot) => {
                if before.as_deref() == Some(&snapshot.selected) {
                    self.write_verified(LAST_BOOTED, snapshot.last_booted.as_deref())?;
                    self.write_verified(DEFAULT, snapshot.default.as_deref())?;
                }
                // A different default may be a user change or our completed restore before a crash.
                // Either way, preserving both current values is the only non-destructive choice.
            }
            Night::Ended(Ended { boot_id: ended }) if ended == &boot_id => {
                return Ok(SavedEntry {
                    before: before.clone().unwrap_or_default(),
                    after: before.unwrap_or_default(),
                });
            }
            Night::Unrecorded | Night::Ended(_) => {
                self.clear_menu_saved(before.as_deref())?;
            }
        }
        let after = self.read_text(DEFAULT)?;
        state.night = Night::Ended(Ended { boot_id });
        self.persist(&state)?;
        Ok(SavedEntry {
            before: before.unwrap_or_default(),
            after: after.unwrap_or_default(),
        })
    }

    fn clear_menu_saved(&mut self, default: Option<&str>) -> stdio::Result<()> {
        let effective = match default {
            Some(value) => Some(value.to_owned()),
            None => self.io.configured_default()?,
        };
        if effective.as_deref() != Some("@saved") {
            return Ok(());
        }
        let Some(saved) = self.read_text(LAST_BOOTED)? else {
            return Ok(());
        };
        let entries = self.io.entries()?;
        let Some(entry) = entries.iter().find(|entry| entry.id == saved) else {
            return Ok(());
        };
        // clear_saved_entry is invoked from the tuning boot as well as the normal boot. The
        // running closure is authoritative when a specialisation has no nested specialisations.
        let identity = self.io.identity(&self.specialisation, true)?;
        if entry.matches(&identity) {
            self.write_verified(LAST_BOOTED, None)?;
        }
        Ok(())
    }
}

impl Environment for SystemdBoot {
    fn get(&mut self, name: &str) -> stdio::Result<Option<String>> {
        self.with_lock(|backend| Ok(backend.state()?.variables.remove(name)))
    }

    fn set(&mut self, values: &[(&str, &str)]) -> stdio::Result<()> {
        self.with_lock(|backend| {
            let mut state = backend.state()?;
            for (name, value) in values {
                state
                    .variables
                    .insert((*name).to_owned(), (*value).to_owned());
            }
            backend.persist(&state)
        })
    }

    fn unset(&mut self, names: &[&str]) -> stdio::Result<()> {
        self.with_lock(|backend| {
            let mut state = backend.state()?;
            for name in names {
                state.variables.remove(*name);
            }
            backend.persist(&state)
        })
    }
}

impl Bootloader for SystemdBoot {
    fn select_tuning_entry(&mut self, tuning: &Entry) -> Result<(), NightError> {
        self.with_lock(|backend| backend.select(tuning))
            .map_err(NightError::Select)
    }

    fn clear_saved_entry(&mut self) -> stdio::Result<SavedEntry> {
        self.with_lock(Self::clear)
    }
}

fn encode(text: &str) -> Vec<u8> {
    // EFI_VARIABLE_NON_VOLATILE | BOOTSERVICE_ACCESS | RUNTIME_ACCESS.
    let mut bytes = Vec::with_capacity(4 + (text.len() + 1) * 2);
    bytes.extend_from_slice(&7_u32.to_le_bytes());
    for unit in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn decode(bytes: &[u8]) -> stdio::Result<String> {
    if bytes.len() < 6 || !bytes.len().is_multiple_of(2) {
        return Err(stdio::Error::new(
            stdio::ErrorKind::InvalidData,
            "malformed EFI string",
        ));
    }
    let mut units: Vec<u16> = bytes[4..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    if units.pop() != Some(0) || units.contains(&0) {
        return Err(stdio::Error::new(
            stdio::ErrorKind::InvalidData,
            "EFI string is not NUL terminated",
        ));
    }
    String::from_utf16(&units)
        .map_err(|error| stdio::Error::new(stdio::ErrorKind::InvalidData, error))
}
