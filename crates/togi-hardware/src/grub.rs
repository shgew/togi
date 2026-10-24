use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::string::FromUtf8Error;
use std::time::Duration;

use togi_command::Invocation;
use togi_tuningboot::night::{DAY_ENTRY_VARIABLE, NIGHT_VARIABLE};
use togi_tuningboot::{Bootloader, Environment, SavedEntry};

const PROGRAM: &str = "grub-editenv";
const DEADLINE: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 64 * 1024;
const SAVED_ENTRY: &str = "saved_entry";
const MAX_NAME_BYTES: usize = 64;
const MAX_VALUE_BYTES: usize = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verb {
    List,
    Set,
    Unset,
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::List => "list",
            Self::Set => "set",
            Self::Unset => "unset",
        })
    }
}

/// One `grub-editenv` invocation, its variables already validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Edit {
    List,
    Set(Vec<(String, String)>),
    Unset(Vec<String>),
}

impl Edit {
    pub(crate) fn verb(&self) -> Verb {
        match self {
            Self::List => Verb::List,
            Self::Set(_) => Verb::Set,
            Self::Unset(_) => Verb::Unset,
        }
    }

    fn args(&self) -> Vec<OsString> {
        let mut args = vec![OsString::from(self.verb().to_string())];
        match self {
            Self::List => {}
            Self::Set(pairs) => args.extend(
                pairs
                    .iter()
                    .map(|(name, value)| OsString::from(format!("{name}={value}"))),
            ),
            Self::Unset(names) => args.extend(names.iter().map(OsString::from)),
        }
        args
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Command(#[from] togi_command::Error),
    #[error("exited with {status}{}", stderr_suffix(stderr))]
    Exit { status: ExitStatus, stderr: String },
    #[error("wrote output that is not UTF-8")]
    NotUtf8(#[from] FromUtf8Error),
}

fn stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

/// Runs one edit against a GRUB environment block and returns its standard output.
pub(crate) trait Editenv {
    fn run(&mut self, env: &Path, edit: &Edit) -> Result<String, CommandError>;
}

struct Process {
    program: OsString,
    deadline: Duration,
    output_limit: usize,
}

impl Process {
    fn system() -> Self {
        Self {
            program: OsString::from(PROGRAM),
            deadline: DEADLINE,
            output_limit: OUTPUT_LIMIT,
        }
    }
}

impl Editenv for Process {
    #[expect(
        clippy::disallowed_methods,
        reason = "grub-editenv is the one tool that reads and writes GRUB's environment block"
    )]
    fn run(&mut self, env: &Path, edit: &Edit) -> Result<String, CommandError> {
        let invocation = Invocation::new(&self.program)
            .arg(env)
            .args(edit.args())
            .output_limit(self.output_limit);
        let output = togi_command::run_blocking(&invocation, self.deadline)?;
        if !output.status.success() {
            return Err(CommandError::Exit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8(output.stdout)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GrubError {
    #[error(
        "GRUB variable name {name:?} must have 1 to {MAX_NAME_BYTES} ASCII letters, digits or underscores"
    )]
    Name { name: String },
    #[error("GRUB variable {name} exceeds {MAX_VALUE_BYTES} bytes")]
    ValueTooLong { name: String },
    #[error("GRUB variable {name} must be printable ASCII on one line")]
    ValueNotPrintable { name: String },
    #[error("{PROGRAM} {} {verb}", env.display())]
    Editenv {
        env: PathBuf,
        verb: Verb,
        #[source]
        source: CommandError,
    },
}

impl From<GrubError> for io::Error {
    fn from(error: GrubError) -> Self {
        let kind = match &error {
            GrubError::Name { .. }
            | GrubError::ValueTooLong { .. }
            | GrubError::ValueNotPrintable { .. } => io::ErrorKind::InvalidInput,
            GrubError::Editenv {
                source: CommandError::Command(togi_command::Error::TimedOut { .. }),
                ..
            } => io::ErrorKind::TimedOut,
            GrubError::Editenv { .. } => io::ErrorKind::Other,
        };
        Self::new(kind, error)
    }
}

/// How clearing `saved_entry` failed, and what had been observed by then. `before` is what the
/// variable held when read; an unset or a second read that failed may still have changed it.
#[derive(Debug, thiserror::Error)]
pub enum ClearError {
    #[error("read {SAVED_ENTRY} before the clear")]
    ReadBefore(#[source] GrubError),
    #[error("unset {SAVED_ENTRY}, which was {before:?}")]
    Unset {
        before: String,
        #[source]
        source: GrubError,
    },
    #[error("read {SAVED_ENTRY} after the unset, which found it {before:?}")]
    ReadAfter {
        before: String,
        #[source]
        source: GrubError,
    },
    #[error("{SAVED_ENTRY} is still {after:?} after the unset, which found it {before:?}")]
    Remains { before: String, after: String },
    #[error("restore {SAVED_ENTRY} to the day's {day:?}, which was {before:?}")]
    Restore {
        before: String,
        day: String,
        #[source]
        source: Box<GrubError>,
    },
    #[error(
        "{SAVED_ENTRY} is {after:?} after restoring the day's {day:?}, which found it {before:?}"
    )]
    NotRestored {
        before: String,
        day: String,
        after: String,
    },
}

impl ClearError {
    /// What the variable held before the unset, once it had been read.
    pub fn before(&self) -> Option<&str> {
        match self {
            Self::ReadBefore(_) => None,
            Self::Unset { before, .. }
            | Self::ReadAfter { before, .. }
            | Self::Remains { before, .. }
            | Self::Restore { before, .. }
            | Self::NotRestored { before, .. } => Some(before),
        }
    }

    /// What the variable held after the unset, when a read after it succeeded.
    pub fn after(&self) -> Option<&str> {
        match self {
            Self::Remains { after, .. } | Self::NotRestored { after, .. } => Some(after),
            Self::ReadBefore(_)
            | Self::Unset { .. }
            | Self::ReadAfter { .. }
            | Self::Restore { .. } => None,
        }
    }
}

impl From<ClearError> for io::Error {
    fn from(error: ClearError) -> Self {
        Self::other(error)
    }
}

fn validate_name(name: &str) -> Result<(), GrubError> {
    let valid = (1..=MAX_NAME_BYTES).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(GrubError::Name {
            name: name.to_owned(),
        })
    }
}

fn validate_value(name: &str, value: &str) -> Result<(), GrubError> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(GrubError::ValueTooLong {
            name: name.to_owned(),
        });
    }
    if !value.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
        return Err(GrubError::ValueNotPrintable {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// The value of `name` in a `grub-editenv list` listing: the text after the first exact `name=`
/// line prefix, minus the line's newline and nothing else.
fn lookup(listing: &str, name: &str) -> Option<String> {
    listing.split_inclusive('\n').find_map(|line| {
        let line = line.strip_suffix('\n').unwrap_or(line);
        line.strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
            .map(str::to_owned)
    })
}

/// GRUB's environment block, edited with `grub-editenv`; each call runs one bounded command.
pub struct Grub {
    env: PathBuf,
    editenv: Box<dyn Editenv>,
}

impl Grub {
    pub fn new(env: impl Into<PathBuf>) -> Self {
        Self::with_editenv(env.into(), Box::new(Process::system()))
    }

    pub(crate) fn with_editenv(env: PathBuf, editenv: Box<dyn Editenv>) -> Self {
        Self { env, editenv }
    }

    fn edit(&mut self, edit: &Edit) -> Result<String, GrubError> {
        self.editenv
            .run(&self.env, edit)
            .map_err(|source| GrubError::Editenv {
                env: self.env.clone(),
                verb: edit.verb(),
                source,
            })
    }

    fn read(&mut self, name: &str) -> Result<Option<String>, GrubError> {
        validate_name(name)?;
        Ok(lookup(&self.edit(&Edit::List)?, name))
    }

    /// Gives up the tuning selection and reads it back, then forgets the night record. When a
    /// night remembered the day's entry, `saved_entry` becomes that entry exactly, never a
    /// generation of its own choosing; otherwise it is unset, so the menu's first entry boots. The
    /// remembered entry stays, so a repeat selects the same day entry again.
    pub fn clear_saved(&mut self) -> Result<SavedEntry, ClearError> {
        let listing = self.edit(&Edit::List).map_err(ClearError::ReadBefore)?;
        let before = lookup(&listing, SAVED_ENTRY).unwrap_or_default();
        let day = lookup(&listing, DAY_ENTRY_VARIABLE).filter(|day| !day.is_empty());
        match day {
            Some(day) => self.restore(before, day),
            None => self.unset_saved(before),
        }
    }

    fn restore(&mut self, before: String, day: String) -> Result<SavedEntry, ClearError> {
        let set = Edit::Set(vec![(SAVED_ENTRY.to_owned(), day.clone())]);
        if let Err(source) = self.edit(&set) {
            return Err(ClearError::Restore {
                before,
                day,
                source: Box::new(source),
            });
        }
        let after = match self.read(SAVED_ENTRY) {
            Ok(after) => after.unwrap_or_default(),
            Err(source) => return Err(ClearError::ReadAfter { before, source }),
        };
        if after != day {
            return Err(ClearError::NotRestored { before, day, after });
        }
        let forget = Edit::Unset(vec![NIGHT_VARIABLE.to_owned()]);
        if let Err(source) = self.edit(&forget) {
            return Err(ClearError::Unset { before, source });
        }
        Ok(SavedEntry { before, after })
    }

    fn unset_saved(&mut self, before: String) -> Result<SavedEntry, ClearError> {
        let unset = Edit::Unset(vec![SAVED_ENTRY.to_owned(), NIGHT_VARIABLE.to_owned()]);
        if let Err(source) = self.edit(&unset) {
            return Err(ClearError::Unset { before, source });
        }
        let after = match self.read(SAVED_ENTRY) {
            Ok(after) => after.unwrap_or_default(),
            Err(source) => return Err(ClearError::ReadAfter { before, source }),
        };
        if after.is_empty() {
            Ok(SavedEntry { before, after })
        } else {
            Err(ClearError::Remains { before, after })
        }
    }
}

impl Environment for Grub {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        Ok(self.read(name)?)
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        for (name, value) in values {
            validate_name(name)?;
            validate_value(name, value)?;
        }
        if values.is_empty() {
            return Ok(());
        }
        let mut pairs: Vec<(String, String)> = values
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        pairs.sort_by(|left, right| left.0.cmp(&right.0));
        self.edit(&Edit::Set(pairs))?;
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        for name in names {
            validate_name(name)?;
        }
        if names.is_empty() {
            return Ok(());
        }
        let names = names.iter().map(|&name| name.to_owned()).collect();
        self.edit(&Edit::Unset(names))?;
        Ok(())
    }
}

impl Bootloader for Grub {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        Ok(self.clear_saved()?)
    }
}

#[cfg(test)]
mod tests;
