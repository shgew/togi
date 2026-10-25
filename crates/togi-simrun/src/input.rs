use std::io::Write;
use std::num::NonZeroU32;
use std::path::PathBuf;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_defect::Entry;
use togi_journal::{Build, Event, MemFs};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, Emergency, ResumeCapsule, SessionError, Stop};
use togi_sim::Simulator;

/// Where the state directory lives across the simulated boots. Either way `state.json` is published when each run
/// finishes, and a crashed run publishes it from its resume capsule.
#[derive(Clone, Debug)]
pub enum Persistence {
    /// An in-memory directory the caller keeps the handle of; nothing reaches the disk unless
    /// [`SimInput::dump_to`] asks.
    Memory(MemFs),
    /// A directory on disk locked for each boot, with the journal buffered and nothing synced.
    Files(PathBuf),
}

/// How a boot after a crash gets the state the crashed one folded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Replay {
    /// The crashed run's resume capsule moves to the next run, which adopts it instead of folding the journal.
    Warm,
    /// The next run folds the whole journal, as `togi run` does. The crashed run's capsule is still verified.
    Cold,
}

/// What an [`Observer`] wants done after seeing an event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Observe {
    Continue,
    /// Cancel the run like a signal; the observer is not called again.
    Stop,
}

/// Called with every event a run appends, once and in order, after it is durable.
pub type Observer = Box<dyn FnMut(&Event) -> Observe>;

pub struct SimInput {
    pub config: Config,
    pub source: ConfigSource,
    pub build: Build,
    /// The machine to boot and reboot. Its history, seed and replay facts are set by whoever built it.
    pub simulator: Simulator,
    pub persistence: Persistence,
    pub replay: Replay,
    /// Verify the resume capsule after every Nth boot that ends in a crash. The last boot of a capped run and a
    /// run that stops are always verified.
    pub verify_every: Option<NonZeroU32>,
    pub max_boots: NonZeroU32,
    /// The simulated time the operator ends the night: the first event a run appends at or after it cancels the run
    /// like a signal, in every boot. The trial in flight still finishes, as the simulated machine runs a trial in one
    /// step, and the session records the end before it stops. `None` leaves the night to end at the policy's own
    /// dead ends.
    pub stop_at: Option<jiff::Timestamp>,
    /// Write each trial's samples under the state directory's `trials`; only a directory on disk has one.
    pub write_samples: bool,
    pub defects: &'static [Entry],
    /// Receives each appended event's line.
    pub log: Option<Box<dyn Write>>,
    pub renderer: Renderer,
    pub zone: TimeZone,
    pub cancel: CancellationToken,
    pub observer: Option<Observer>,
    /// Write the in-memory state directory here when the simulation ends.
    pub dump_to: Option<PathBuf>,
}

/// How the run that did not crash ended; its journal is closed and left in the state directory.
pub struct Finished {
    pub stop: Option<Stop>,
    pub errors: Vec<SessionError>,
    pub emergency: Option<Emergency>,
    pub capsule: Option<ResumeCapsule>,
}

pub enum SimOutcome {
    Stopped(Box<Finished>),
    /// The last of `boots` boots crashed and the machine was rebooted.
    Capped {
        boots: NonZeroU32,
    },
}
