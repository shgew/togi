//! The flags more than one command shares, as reusable [`clap::Args`] structs.
//!
//! `--state-dir` is global on [`crate::cli::Cli`], so every command takes it before or after the
//! command name and reads it from [`crate::cli::Context`]. `--config` belongs to `doctor`, `run`
//! and `reset` only: each of them flattens [`ConfigArgs`] into its own `Args`, and the other
//! commands reject it with the usage error `--config applies only to doctor, run and reset`.
//! [`crate::cli::config_after_command`] lets it be written before the command name as well.

use std::path::{Path, PathBuf};

use clap::{Args, ValueHint};
use togi_config::{Config, LoadError};

pub use togi_config::DEFAULT_PATH as DEFAULT_CONFIG_PATH;

/// The state directory when `--state-dir` is absent.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/togi";

/// `--state-dir`, flattened once into [`crate::cli::Cli`] with `global = true`.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct StateDirArgs {
    /// State directory
    #[arg(
        long,
        global = true,
        value_name = "path",
        value_hint = ValueHint::DirPath,
        default_value = DEFAULT_STATE_DIR,
    )]
    pub state_dir: PathBuf,
}

/// `--config`, flattened into the `Args` of each command that reads the configuration.
#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigArgs {
    /// JSON configuration file [default: /etc/togi/config.json]
    #[arg(
        long = "config",
        value_name = "path",
        value_hint = ValueHint::FilePath,
    )]
    pub path: Option<PathBuf>,
}

impl ConfigArgs {
    /// The file to read: the one given, else `default`.
    #[must_use]
    pub fn path<'a>(&'a self, default: &'a Path) -> &'a Path {
        self.path.as_deref().unwrap_or(default)
    }

    /// Whether `--config` was given: a missing default file means the default configuration, a
    /// missing explicit one is an error.
    #[must_use]
    pub fn is_explicit(&self) -> bool {
        self.path.is_some()
    }

    /// Reads the configuration: a missing default file is the default configuration, a missing
    /// explicit one an error.
    ///
    /// # Errors
    ///
    /// The file cannot be read or is not a valid configuration.
    pub fn load(&self, default: &Path) -> Result<Config, LoadError> {
        read(self.path(default), self.is_explicit())
    }
}

fn read(path: &Path, explicit: bool) -> Result<Config, LoadError> {
    match Config::load(path) {
        Err(LoadError::Read { source, .. })
            if !explicit && source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(Config::default())
        }
        loaded => loaded,
    }
}

#[cfg(test)]
mod tests;
