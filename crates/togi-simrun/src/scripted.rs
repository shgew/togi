//! Runs short mechanic scripts through the real session, with the same writer and replay checks
//! used by longer runs. The bounds are fixture guards, not limits on a real tuning night.

use std::cell::Cell;
use std::num::{NonZeroU32, TryFromIntError};
use std::rc::Rc;

use jiff::tz::TimeZone;
use togi_config::{Config, DEFAULT_PATH};
use togi_journal::{AppendError, Build, CloseError, Event, LockError, OpenError};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, SessionError};
use togi_sim::Simulator;
use togi_tuner::policy::FoldError;

use crate::{HistoryError, Observe, Observer, Persistence, Replay, SimError, SimInput, SimOutcome};

pub use crate::staged_day::{Goal as StagedDayGoal, staged_day};

const MAX_EVENTS: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the scripted session ended with errors: {}", crate::error::listed(.0))]
    Session(Vec<SessionError>),
    #[error("simulate the scripted session")]
    Simulate(#[from] SimError),
    #[error("read the scripted journal")]
    History(#[from] HistoryError),
    #[error("the scripted session did not stop within {0} boots")]
    Capped(NonZeroU32),
    #[error("the scripted session did not stop within {MAX_EVENTS} observed events")]
    EventCap,
    #[error("construct or reboot the scripted machine")]
    Construct(#[from] togi_sim::ArithmeticError),
    #[error("fold the staged day journal")]
    Fold(#[from] FoldError),
    #[error("lock the staged day journal")]
    DayLock(#[from] LockError),
    #[error("open the staged day journal")]
    DayOpen(#[from] OpenError),
    #[error("append to the staged day journal")]
    DayAppend(#[from] AppendError),
    #[error("close the staged day journal")]
    DayClose(#[from] CloseError),
    #[error("access the staged day machine")]
    DayMachine(#[from] togi_machine::Error),
    #[error("query the staged day policy")]
    DayPolicy(#[from] togi_tuner::policy::NotReady),
    #[error("build the staged CCD profile")]
    DayProfile(#[from] togi_machine::CcdProfileError),
    #[error("apply the staged day profile")]
    DayWrite(#[from] togi_session::DayWriteError),
    #[error("index the staged day journal")]
    Index(#[from] TryFromIntError),
    #[error("expected scripted milestone: {0}")]
    Milestone(&'static str),
}

/// Run a real night on the scripted machine until its own dead end or until `halt` signals the
/// session to stop. Return the whole live journal including the teardown.
///
/// # Errors
///
/// Journal or session errors, or a script exceeding its boot or event bound.
pub fn night(
    machine: Simulator,
    persistence: &Persistence,
    build: &Build,
    max_boots: NonZeroU32,
    mut halt: impl FnMut(&Event) -> bool + 'static,
) -> Result<Vec<Event>, Error> {
    let exceeded = Rc::new(Cell::new(false));
    let guard = Rc::clone(&exceeded);
    let mut observed = 0;
    let observer: Observer = Box::new(move |event| {
        observed += 1;
        if observed > MAX_EVENTS {
            guard.set(true);
            Observe::Stop
        } else if halt(event) {
            Observe::Stop
        } else {
            Observe::Continue
        }
    });
    let outcome = crate::simulate(SimInput {
        config: Config::default(),
        source: ConfigSource {
            path: DEFAULT_PATH.to_owned(),
            file: false,
        },
        build: build.clone(),
        simulator: machine,
        persistence: persistence.clone(),
        replay: Replay::Warm,
        verify_every: None,
        max_boots,
        stop_at: None,
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: Some(observer),
        dump_to: None,
        adapt_trials: None,
    })?;
    if exceeded.get() {
        return Err(Error::EventCap);
    }
    match outcome {
        SimOutcome::Stopped(finished) if finished.errors.is_empty() => {}
        SimOutcome::Stopped(finished) => return Err(Error::Session(finished.errors)),
        SimOutcome::Capped { boots } => return Err(Error::Capped(boots)),
    }
    Ok(crate::events(persistence)?)
}
