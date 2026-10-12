//! The machine a command runs on, as one seam.
//!
//! Every command reaches the host through [`System`]: the platform, the host lock, the boot id,
//! the clock, the hardware checks, GRUB and the reboot. [`Real`] is the host; [`crate::run`] uses
//! it. [`crate::run_with`] takes any other, so a test on Linux can stand for another platform
//! (`check_platform` fails with [`togi_hardware::Error::Unsupported`]), hold the host lock on a
//! temporary file, or answer the hardware checks without touching hardware.
//!
//! A command that must stop on a platform other than Linux asks [`System::check_platform`] before
//! it reads the boot id, opens the journal, takes the host lock or makes a check.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use rustix::time::{ClockId, clock_gettime};
use togi_config::Config;
use togi_hardware::{Diagnosis, Grub, RebootError};
use togi_hostlock::Policy;
use togi_machine::{BiosContext, BootId, CancellationToken, Clock, Error};
use togi_render::OutputFacts;
use togi_tuningboot::{Bootloader, RETRY_MARKER};

/// What a command needs from the host.
pub trait System {
    /// Whether this platform can run on real hardware; `Err` is the refusal a command reports.
    fn check_platform(&self) -> Result<(), togi_hardware::Error>;

    /// Whether the process runs as root.
    fn privileged(&self) -> bool;

    /// The host lock file and the ownership it must have.
    fn host_lock(&self) -> (&Path, Policy);

    /// The identifier of the current boot.
    fn boot_id(&self) -> Result<BootId, togi_detect::Error>;

    /// The wall clock.
    fn now(&self) -> Timestamp;

    /// Time since boot.
    fn monotonic(&self) -> Duration;

    /// The checks `run` makes at startup, without writing anything.
    fn diagnose(
        &self,
        config: &Config,
        recorded: Option<&BiosContext>,
        privileged: bool,
    ) -> Result<Diagnosis, togi_hardware::Error>;

    /// The GRUB environment file `grubenv`.
    fn bootloader(&self, grubenv: &Path) -> Box<dyn Bootloader>;

    /// Creates the retry marker; fails when it exists.
    fn create_retry_marker(&self) -> io::Result<()>;

    /// Removes the retry marker; an absent marker is not an error.
    fn remove_retry_marker(&self) -> io::Result<()>;

    /// Asks systemd for a plain reboot.
    fn reboot(&self) -> Result<(), RebootError>;

    /// What decides whether stderr is coloured.
    fn stderr_facts(&self) -> OutputFacts;
}

/// The host the process runs on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Real;

impl System for Real {
    fn check_platform(&self) -> Result<(), togi_hardware::Error> {
        togi_hardware::check_platform()
    }

    fn privileged(&self) -> bool {
        rustix::process::geteuid().is_root()
    }

    fn host_lock(&self) -> (&Path, Policy) {
        let policy = if cfg!(target_os = "linux") {
            Policy::Host
        } else {
            Policy::Private
        };
        (Path::new(togi_hostlock::DEFAULT_PATH), policy)
    }

    fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
        togi_detect::boot_id()
    }

    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn monotonic(&self) -> Duration {
        let now = clock_gettime(ClockId::Monotonic);
        Duration::new(
            u64::try_from(now.tv_sec).unwrap_or_default(),
            u32::try_from(now.tv_nsec).unwrap_or_default(),
        )
    }

    fn diagnose(
        &self,
        config: &Config,
        recorded: Option<&BiosContext>,
        privileged: bool,
    ) -> Result<Diagnosis, togi_hardware::Error> {
        togi_hardware::diagnose(config, recorded, privileged)
    }

    fn bootloader(&self, grubenv: &Path) -> Box<dyn Bootloader> {
        Box::new(Grub::new(grubenv))
    }

    fn create_retry_marker(&self) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(RETRY_MARKER)
            .map(drop)
    }

    fn remove_retry_marker(&self) -> io::Result<()> {
        match std::fs::remove_file(RETRY_MARKER) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    fn reboot(&self) -> Result<(), RebootError> {
        togi_hardware::reboot()
    }

    fn stderr_facts(&self) -> OutputFacts {
        OutputFacts::detect(
            &io::stderr(),
            std::env::var_os("NO_COLOR").as_deref(),
            std::env::var_os("JOURNAL_STREAM").as_deref(),
        )
    }
}

/// The clock of a [`System`], for the session functions that stamp the events they append.
pub struct SystemClock<'a>(pub &'a dyn System);

impl Clock for SystemClock<'_> {
    fn now(&self) -> Timestamp {
        self.0.now()
    }

    fn monotonic(&self) -> Duration {
        self.0.monotonic()
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        if cancel.wait_timeout(duration) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
