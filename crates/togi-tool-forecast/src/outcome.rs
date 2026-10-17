use std::collections::BTreeSet;
use std::num::NonZeroU32;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use togi_journal::{Build, CycleEvent, Event, Payload, SessionId, ShutdownReason, State};
use togi_machine::{BootId, ObservedOffset, PerCore, object_only};
use togi_sim::REBOOT_TIME;

use crate::error::Error;
use crate::session::{Anchor, Session};

/// How a run stands after the anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The run reached the clean cycle `togi run --cycles 1` stops at.
    Concluded,
    /// The run stopped at a dead end before concluding.
    DeadEnd,
    /// The run had not concluded when its journal ends.
    Censored,
}

object_only! {
    /// What a run did after the anchor, through its first terminal point: a conclusion or a dead end. Events after
    /// that point are ignored.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Outcome {
        pub status: Status,
        /// Hours the run spent running after the anchor, through the terminal point or the last event of a run
        /// that has none: each invocation from its first event to its last, and the downtime before an
        /// invocation that records `crash.detected`. The time after a `shutdown`, before a restart that detects
        /// no crash and before a new session's `session.start` is not run time.
        pub hours: f64,
        /// `crash.detected` events.
        pub crashes: u32,
        /// Hunts that ran a trial; a hunt answered by carried facts alone runs none.
        pub hunts: u32,
        /// The profile of the session holding the terminal point, or of the newest session: its checking profile, or
        /// its baseline before it has one. Offsets are as the journal recorded them, whatever the hardware range.
        pub profile: PerCore<ObservedOffset>,
        /// The sum of the profile's offsets.
        pub depth: i64,
    }
}

/// How simulated hours differ from real ones. Forecasts report simulated hours as they are.
pub fn recovery_bias() -> String {
    format!(
        "hours are simulated; a simulated crash reboot takes {:.0} s, while real recovery took a median of 141 s on 2026-10-03 (about 37 minutes over that session), so real runs take longer; not corrected",
        REBOOT_TIME.as_secs_f64()
    )
}

/// What the sessions record after the anchor, through the first terminal point: a `checking.cycle` start after a
/// clean cycle, where `togi run --cycles 1` shuts down instead (that start is not counted), a `shutdown` with
/// reason `cycles`, or a dead end. A real run that kept checking past its conclusion therefore scores as the
/// `--cycles 1` run with the same history would. The session holding the terminal point, or the newest session
/// when there is none, must be [replayable](Session::replayable), so the tuner can project its profile.
///
/// `sessions` is [`read_dir`](crate::read_dir)'s result: oldest first, the anchor's session among them with its
/// journal through the anchor unchanged.
pub fn after(sessions: &[Session], anchor: &Anchor) -> Result<Outcome, Error> {
    Ok(follow::<togi_tuner::State>(sessions, anchor)?.outcome)
}

/// The tuner as the walk needs it, so the counting rules can be pinned without a journal the tuner would accept.
pub(crate) trait Replay: Default {
    fn fold(&mut self, event: &Event) -> Result<(), togi_tuner::FoldError>;
    fn clean_cycles(&self) -> u32;
    fn profile(&self) -> Option<PerCore<ObservedOffset>>;
}

impl Replay for togi_tuner::State {
    fn fold(&mut self, event: &Event) -> Result<(), togi_tuner::FoldError> {
        togi_tuner::State::fold(self, event)
    }

    fn clean_cycles(&self) -> u32 {
        togi_tuner::State::clean_cycles(self)
    }

    fn profile(&self) -> Option<PerCore<ObservedOffset>> {
        togi_tuner::State::profile(self).map(|profile| {
            profile
                .iter()
                .map(|(core, offset)| (core, offset.observed()))
                .collect()
        })
    }
}

/// An outcome and the distinct builds that ran in it, in order: the build in effect at the anchor when its
/// invocation wrote events after the anchor, then each build stamped after the anchor through the terminal point.
pub(crate) struct Followed {
    pub outcome: Outcome,
    pub builds: Vec<Build>,
}

pub(crate) fn follow<R: Replay>(sessions: &[Session], anchor: &Anchor) -> Result<Followed, Error> {
    let (first, at) = anchor.find(sessions)?;
    let mut scan = Scan::new(&sessions[first].events()[at]);
    let mut held = (&sessions[first], R::default(), State::default());
    'walk: for (i, session) in sessions[first..].iter().enumerate() {
        let start = if i == 0 { at + 1 } else { 0 };
        held = (session, R::default(), State::default());
        let (_, tuner, state) = &mut held;
        let mut in_effect: Option<&Build> = None;
        for (j, event) in session.events().iter().enumerate() {
            let (mut end, mut fold) = (false, true);
            if j < start {
                if let Some(build) = stamp_of(event) {
                    in_effect = Some(build);
                }
            } else {
                scan.stamp(event, in_effect.take());
                (end, fold) = scan.event(session, event, tuner);
            }
            if fold && session.replayable() {
                state.apply(event).map_err(|source| Error::JournalFold {
                    session: session.id(),
                    source,
                })?;
                tuner.fold(event).map_err(|source| Error::TunerFold {
                    session: session.id(),
                    source,
                })?;
            }
            if end {
                break 'walk;
            }
        }
    }
    let (session, tuner, state) = &held;
    if let Some(refusal) = session.refusal() {
        return Err(Error::NotReplayable {
            session: session.id(),
            refusal,
        });
    }
    let profile = tuner.profile().unwrap_or_else(|| baseline(state));
    let depth = profile.iter().map(|(_, offset)| offset.0).sum();
    Ok(Followed {
        outcome: Outcome {
            status: scan.status,
            hours: scan.time.total.as_secs_f64() / 3600.0,
            crashes: scan.crashes,
            hunts: u32::try_from(scan.hunts.len()).unwrap_or(u32::MAX),
            profile,
            depth,
        },
        builds: scan.builds,
    })
}

fn baseline(state: &State) -> PerCore<ObservedOffset> {
    let recorded = state.session.as_ref().map(|session| session.baseline);
    state
        .cores
        .iter()
        .map(|facts| {
            let offset = recorded
                .and_then(|baseline| baseline.get(facts.core))
                .unwrap_or_default();
            (facts.core, offset)
        })
        .collect()
}

/// The build a `session.start` or `config.loaded` event stamps.
pub(crate) fn stamp_of(event: &Event) -> Option<&Build> {
    match &event.payload {
        Payload::SessionStart(start) => Some(&start.build),
        Payload::ConfigLoaded(loaded) => Some(&loaded.build),
        _ => None,
    }
}

/// A run's outcome and builds accumulated event by event after the anchor.
struct Scan {
    status: Status,
    time: RunTime,
    crashes: u32,
    hunts: BTreeSet<(SessionId, NonZeroU32)>,
    builds: Vec<Build>,
}

impl Scan {
    fn new(anchor: &Event) -> Self {
        Self {
            status: Status::Censored,
            time: RunTime::new(anchor),
            crashes: 0,
            hunts: BTreeSet::new(),
            builds: Vec::new(),
        }
    }

    /// Records the build `event` stamps or, when it stamps none, the build in effect at the anchor.
    fn stamp(&mut self, event: &Event, in_effect: Option<&Build>) {
        let Some(build) = stamp_of(event).or(in_effect) else {
            return;
        };
        if !self.builds.contains(build) {
            self.builds.push(build.clone());
        }
    }

    /// Counts `event` of `session`, whose tuner `tuner` has folded the events before it, and reports whether it
    /// is the terminal point and whether it is folded.
    fn event(&mut self, session: &Session, event: &Event, tuner: &impl Replay) -> (bool, bool) {
        self.time.event(event);
        let mut end = false;
        match &event.payload {
            Payload::CheckingCycle(cycle)
                if session.replayable()
                    && cycle.event == CycleEvent::Start
                    && tuner.clean_cycles() >= 1 =>
            {
                self.status = Status::Concluded;
                return (true, false);
            }
            Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Cycles => {
                self.status = Status::Concluded;
                end = true;
            }
            Payload::CrashDetected(_) => self.crashes += 1,
            Payload::TrialIntent(intent) => {
                if let Some(hunt) = intent.hunt {
                    self.hunts.insert((session.id(), hunt));
                }
            }
            Payload::DeadEnd(_) => {
                self.status = Status::DeadEnd;
                end = true;
            }
            _ => {}
        }
        (end, true)
    }
}

/// The time a run spent running, folded event by event from the anchor.
///
/// An invocation starts at its `session.start`, or at its `config.loaded` when no `session.start` precedes it,
/// or at its first event in another boot. The gap before an invocation counts only once the invocation records
/// `crash.detected`, and never after a `shutdown` or before a `session.start`. `mce` and `boot.leave_reason`
/// may be what a starting invocation records ahead of its `config.loaded`, and `command.reset` and
/// `session.archived` are recorded outside any run, so none of them moves where a gap starts or ends.
struct RunTime {
    total: SignedDuration,
    /// The latest event that settles where a gap starts.
    last: Timestamp,
    boot: BootId,
    stopped: bool,
    started: bool,
    /// The gap before the current invocation, run time once it records `crash.detected`.
    downtime: Option<SignedDuration>,
}

impl RunTime {
    fn new(anchor: &Event) -> Self {
        Self {
            total: SignedDuration::ZERO,
            last: anchor.time,
            boot: anchor.boot.clone(),
            stopped: matches!(anchor.payload, Payload::Shutdown(_)),
            started: matches!(anchor.payload, Payload::SessionStart(_)),
            downtime: None,
        }
    }

    fn event(&mut self, event: &Event) {
        if matches!(
            event.payload,
            Payload::Mce(_)
                | Payload::BootLeaveReason(_)
                | Payload::CommandReset(_)
                | Payload::SessionArchived(_)
        ) {
            return;
        }
        let gap = event
            .time
            .duration_since(self.last)
            .max(SignedDuration::ZERO);
        let starts = match event.payload {
            Payload::SessionStart(_) => true,
            Payload::ConfigLoaded(_) => !self.started,
            _ => event.boot != self.boot,
        };
        if matches!(event.payload, Payload::SessionStart(_)) || self.stopped {
            self.downtime = None;
        } else if starts {
            self.downtime = Some(gap);
        } else {
            self.total += gap;
        }
        if matches!(event.payload, Payload::CrashDetected(_))
            && let Some(downtime) = self.downtime.take()
        {
            self.total += downtime;
        }
        self.last = event.time;
        self.boot.clone_from(&event.boot);
        self.stopped = matches!(event.payload, Payload::Shutdown(_));
        self.started = matches!(event.payload, Payload::SessionStart(_));
    }
}
