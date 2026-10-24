//! The machine a command runs on, as one seam.
//!
//! Every command reaches the host through [`System`]: the platform, the host lock, the default
//! configuration file, the boot id, the clock, the hardware checks, GRUB, the reboot, the machine
//! `run` tunes and the process's own terminal, stderr and dashboard. [`Real`] is the host;
//! [`crate::run()`] uses it. [`crate::run_with`] takes any other, so a test on Linux can stand for
//! another platform (`check_platform` fails with [`togi_hardware::Error::Unsupported`]), acquire a
//! descriptor-free host lock, read configuration and journals from an in-memory filesystem, or
//! answer the hardware checks without touching hardware.
//!
//! A command that must stop on a platform other than Linux asks [`System::check_platform`] before
//! it reads the boot id, opens the journal, takes the host lock or makes a check.

use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use rustix::time::{ClockId, clock_gettime};
use togi_config::Config;
use togi_hardware::{Diagnosis, Grub, RebootError};
use togi_hostlock::{HostLock, Policy};
use togi_journal::fs::Fs;
use togi_journal::{Build, DirError, FileError, LockError, Recording, StateDir};
use togi_machine::{BiosContext, BootId, CancellationToken, Clock, Error, Machine};
use togi_render::OutputFacts;
use togi_tuningboot::{Bootloader, RETRY_MARKER};

use crate::dashboard::{Surface, Watch};

/// What a command needs from the host.
pub trait System {
    /// Whether this platform can run on real hardware; `Err` is the refusal a command reports.
    fn check_platform(&self) -> Result<(), togi_hardware::Error>;

    /// Whether the process runs as root.
    fn privileged(&self) -> bool;

    /// Installs the command's signal listener; fake hosts can retain the token without host effects.
    fn listen(
        &self,
        token: &CancellationToken,
    ) -> Result<Box<dyn std::any::Any>, crate::signals::Error> {
        listen_for_signals(token)
    }

    /// The host lock file and the ownership it must have.
    fn host_lock(&self) -> (&Path, Policy);

    /// Acquires the host-wide command lock. Test hosts may provide descriptor-free exclusion.
    fn acquire_host_lock(&self) -> Result<HostLock, togi_hostlock::Error> {
        let (path, policy) = self.host_lock();
        HostLock::acquire(path, policy)
    }

    /// The journal filesystem, when injected; `None` uses the durable production filesystem.
    fn journal_fs(&self) -> Option<Arc<dyn Fs>> {
        None
    }

    /// Locks a state directory using the same filesystem as its readers.
    #[expect(
        clippy::disallowed_methods,
        reason = "the host seam explicitly selects an injected journal filesystem"
    )]
    fn lock_state_dir(&self, path: &Path) -> Result<StateDir, LockError> {
        match self.journal_fs() {
            Some(fs) => StateDir::lock_with_fs(fs, path),
            None => StateDir::lock(path),
        }
    }

    /// Reads the first-line build stamp without taking the writer lock.
    fn recorded_build(&self, path: &Path) -> Result<Option<Build>, DirError> {
        match self.journal_fs() {
            Some(fs) => togi_journal::recorded_build_in(&*fs, path),
            None => togi_journal::recorded_build(path),
        }
    }

    /// Reads a journal with strict event decoding.
    fn recording(&self, path: &Path) -> Result<Recording, FileError> {
        match self.journal_fs() {
            Some(fs) => Recording::read_in(&*fs, path),
            None => Recording::read(path),
        }
    }

    /// Reads a journal while retaining diagnostics for unknown event kinds and fields.
    fn recording_lenient(&self, path: &Path) -> Result<Recording, FileError> {
        match self.journal_fs() {
            Some(fs) => Recording::read_in_lenient(&*fs, path),
            None => Recording::read_lenient(path),
        }
    }

    /// Reads configuration bytes through the host's filesystem.
    fn read_config(&self, path: &Path) -> io::Result<String> {
        match self.journal_fs() {
            Some(fs) => String::from_utf8(fs.read(path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            None => std::fs::read_to_string(path),
        }
    }

    /// The configuration file read when `--config` is absent.
    fn config_path(&self) -> &Path;

    /// The identifier of the current boot.
    fn boot_id(&self) -> Result<BootId, togi_detect::Error>;

    /// The wall clock.
    fn now(&self) -> Timestamp;

    /// Time since boot; refuses malformed kernel clock components.
    fn monotonic(&self) -> Result<Duration, togi_machine::Error>;

    /// The zone event lines are shown in.
    fn zone(&self) -> TimeZone;

    /// The online cores, read without accessing the SMU mailbox.
    fn topology(&self) -> Result<Vec<togi_machine::CoreInfo>, togi_hardware::Error>;

    /// The checks `run` makes at startup; privileged checks borrow the held host lock.
    fn diagnose(
        &self,
        config: &Config,
        recorded: Option<&BiosContext>,
        lock: Option<&HostLock>,
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

    /// What decides whether stdout is coloured or connected to the system journal.
    fn stdout_facts(&self) -> OutputFacts {
        OutputFacts::detect(
            &io::stdout(),
            std::env::var_os("NO_COLOR").as_deref(),
            std::env::var_os("JOURNAL_STREAM").as_deref(),
        )
    }

    /// The machine `run` tunes, borrowing the host lock until every hardware seam is dropped.
    fn open_machine<'lock>(
        &self,
        config: &Config,
        state_dir: &Path,
        lock: &'lock HostLock,
    ) -> Result<Machine<'lock>, togi_hardware::Error>;

    /// Opens read-only daily observation under the held host lock.
    fn day_observer<'lock>(
        &self,
        lock: &'lock HostLock,
    ) -> Result<Box<dyn togi_hardware::day::Observer + 'lock>, Error> {
        togi_hardware::day::observe(lock)
    }

    /// Opens the day question listener; in-memory command hosts have no external socket.
    fn day_socket(&self) -> Result<Option<crate::day::socket::Server>, crate::day::socket::Error> {
        Ok(None)
    }

    /// Sends a question request without requiring journal access or elevated privileges.
    fn day_request(
        &self,
        request: &crate::day::socket::Request,
    ) -> Result<crate::day::socket::Reply, crate::day::socket::Error> {
        crate::day::socket::request(request)
    }

    /// Plasma is the supported actions desktop; others get a plain notification.
    fn day_notification_actions(&self) -> bool {
        std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| {
            desktop
                .split(':')
                .any(|name| name.eq_ignore_ascii_case("KDE"))
        })
    }

    /// Runs the installed notification CLI with bounded output and runtime.
    fn day_notification(
        &self,
        invocation: &togi_command::Invocation,
    ) -> Result<togi_command::Output, togi_command::Error> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the graphical-session adapter invokes the installed libnotify CLI"
        )]
        togi_command::run_blocking(invocation, Duration::from_secs(130))
    }

    /// Whether stdin and stderr are both terminals.
    fn interactive(&self) -> bool;

    /// The next line typed on standard input with its newline, or `None` when the input ended.
    fn prompt_line(&self) -> io::Result<Option<String>> {
        let mut line = String::new();
        Ok((io::stdin().read_line(&mut line)? > 0).then_some(line))
    }

    /// A handle on standard error that outlives the command's borrowed streams: the event lines of
    /// a run and the report of its emergency.
    fn stderr(&self) -> Box<dyn Write>;

    /// What draws the dashboard when `run` shows one.
    fn surface(&self) -> Arc<dyn Surface>;

    /// Assembles the dashboard with this host's terminal effects.
    fn dashboard(&self, state: &Path, since: u64) -> crate::dashboard::Dashboard {
        crate::dashboard::Dashboard::new(self.surface(), state, since)
    }
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

    fn day_socket(&self) -> Result<Option<crate::day::socket::Server>, crate::day::socket::Error> {
        crate::day::socket::Server::bind().map(Some)
    }

    fn host_lock(&self) -> (&Path, Policy) {
        let policy = if cfg!(target_os = "linux") {
            Policy::Host
        } else {
            Policy::Private
        };
        (Path::new(togi_hostlock::DEFAULT_PATH), policy)
    }

    fn config_path(&self) -> &Path {
        Path::new(togi_config::DEFAULT_PATH)
    }

    fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
        togi_detect::boot_id()
    }

    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
        let now = clock_gettime(ClockId::Monotonic);
        togi_machine::monotonic_duration(now.tv_sec, now.tv_nsec)
    }

    fn zone(&self) -> TimeZone {
        TimeZone::system()
    }

    fn topology(&self) -> Result<Vec<togi_machine::CoreInfo>, togi_hardware::Error> {
        togi_hardware::topology()
    }

    fn diagnose(
        &self,
        config: &Config,
        recorded: Option<&BiosContext>,
        lock: Option<&HostLock>,
    ) -> Result<Diagnosis, togi_hardware::Error> {
        togi_hardware::diagnose(config, recorded, lock)
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

    fn open_machine<'lock>(
        &self,
        config: &Config,
        state_dir: &Path,
        lock: &'lock HostLock,
    ) -> Result<Machine<'lock>, togi_hardware::Error> {
        togi_hardware::open(config, state_dir, lock)
    }

    fn interactive(&self) -> bool {
        both_terminals(&io::stdin(), &io::stderr())
    }

    fn stderr(&self) -> Box<dyn Write> {
        Box::new(io::stderr())
    }

    fn surface(&self) -> Arc<dyn Surface> {
        Arc::new(Watch)
    }
}

/// Whether both streams are terminals, as a terminal ioctl says: a character device such as
/// `/dev/null` is not.
#[must_use]
pub fn both_terminals(stdin: &impl IsTerminal, stderr: &impl IsTerminal) -> bool {
    stdin.is_terminal() && stderr.is_terminal()
}

/// The clock of a [`System`], for the session functions that stamp the events they append.
pub struct SystemClock<'a>(pub &'a dyn System);

impl Clock for SystemClock<'_> {
    fn now(&self) -> Timestamp {
        self.0.now()
    }

    fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
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

#[cfg(test)]
mod tests;

/// Installs a real signal listener for explicit process fixtures as well as production hosts.
///
/// # Errors
///
/// Returns the error from registering the process signal listener.
pub fn listen_for_signals(
    token: &CancellationToken,
) -> Result<Box<dyn std::any::Any>, crate::signals::Error> {
    crate::signals::listen(token).map(|guard| Box::new(guard) as Box<dyn std::any::Any>)
}
