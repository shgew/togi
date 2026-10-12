//! What `run` and `doctor` take from the host: the platform check, the host lock's place, the boot id,
//! the real machine, the bootloader, the reboot request and the process's own terminal and stderr.
//!
//! [`Real`] is the host; tests give [`crate::cli::run_with`] a [`System`] of their own, which is how a
//! Linux test reaches the unsupported-platform path and how `run` is exercised on a simulated machine
//! without touching hardware, a lock under `/run` or the boot entry.

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use togi_config::Config;
use togi_hardware::{Error, Grub, RebootError};
use togi_hostlock::{HostLock, Policy};
use togi_machine::{BootId, Machine};
use togi_render::{OutputFacts, Renderer};
use togi_tuningboot::Bootloader;

use crate::dashboard::{Stop, Surface};

/// The host, as far as the commands that reach hardware depend on it.
pub trait System {
    /// Whether this platform can run on real hardware; the first thing `run` and `doctor` ask.
    fn check_platform(&self) -> Result<(), Error>;

    /// The host lock file and the ownership rule it is held under.
    fn host_lock(&self) -> (&Path, Policy);

    /// This boot's id.
    fn boot_id(&self) -> Result<BootId, togi_detect::Error>;

    /// The wall clock.
    fn now(&self) -> Timestamp;

    /// GRUB's environment block at `grubenv`.
    fn bootloader(&self, grubenv: &Path) -> Box<dyn Bootloader>;

    /// Asks the system to reboot into the normal system.
    fn reboot(&self) -> Result<(), RebootError>;

    /// The machine, assembled under the held host lock.
    fn open_machine(
        &self,
        config: &Config,
        state_dir: &Path,
        lock: &HostLock,
    ) -> Result<Machine, Error>;

    /// Whether stdin and stderr are both terminals.
    fn interactive(&self) -> bool;

    /// A handle on standard error that outlives the command's borrowed streams: the event lines of a run and
    /// the report of its emergency.
    fn stderr(&self) -> Box<dyn Write>;

    /// How stderr is styled.
    fn renderer(&self) -> Renderer;

    /// What draws the dashboard when `run` shows one.
    fn surface(&self) -> Arc<dyn Surface>;
}

/// The machine togi runs on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Real;

impl System for Real {
    fn check_platform(&self) -> Result<(), Error> {
        togi_hardware::check_platform()
    }

    fn host_lock(&self) -> (&Path, Policy) {
        (Path::new(togi_hostlock::DEFAULT_PATH), Policy::Host)
    }

    fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
        togi_detect::boot_id()
    }

    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn bootloader(&self, grubenv: &Path) -> Box<dyn Bootloader> {
        Box::new(Grub::new(grubenv))
    }

    fn reboot(&self) -> Result<(), RebootError> {
        togi_hardware::reboot()
    }

    fn open_machine(
        &self,
        config: &Config,
        state_dir: &Path,
        lock: &HostLock,
    ) -> Result<Machine, Error> {
        togi_hardware::open(config, state_dir, lock)
    }

    fn interactive(&self) -> bool {
        both_terminals(&io::stdin(), &io::stderr())
    }

    fn stderr(&self) -> Box<dyn Write> {
        Box::new(io::stderr())
    }

    fn renderer(&self) -> Renderer {
        Renderer::new(OutputFacts::detect(
            &io::stderr(),
            std::env::var_os("NO_COLOR").as_deref(),
            std::env::var_os("JOURNAL_STREAM").as_deref(),
        ))
    }

    fn surface(&self) -> Arc<dyn Surface> {
        Arc::new(Unavailable)
    }
}

struct Unavailable;

impl Surface for Unavailable {
    fn draw(&self, _: &Path, _: u64, _: &Stop) -> Result<(), String> {
        Err("no dashboard is built in".to_owned())
    }
}

/// Whether both streams are terminals, as a terminal ioctl says: a character device such as `/dev/null` is not.
#[must_use]
pub fn both_terminals(stdin: &impl std::io::IsTerminal, stderr: &impl std::io::IsTerminal) -> bool {
    stdin.is_terminal() && stderr.is_terminal()
}

#[cfg(test)]
mod tests;
