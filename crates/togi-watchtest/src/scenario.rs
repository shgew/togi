use std::fmt;

use togi_journal::{Activity, ClimbAction, Event, EventKind, Payload, Queue, Tested};
use togi_machine::{Regime, TargetOffset};

use crate::cut::{before_crash_detected, cut_at, cut_passed_trial, cut_trial};
use crate::{Base, Error};

/// A watch golden scenario captured from a live bounded-exploration session at the named state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Scenario {
    /// The first night's first CO-0 control trial has started.
    Controls,
    /// The first R6 control trial, the idle hold, has started.
    Idle,
    /// An R3 control trial has passed and the next has not begun.
    Between,
    /// The first isolated mapping trial below CO 0 has started: CCD0 at CO -5.
    Mapping,
    /// A mapping trial the machine did not outlive: the next boot has started and has not yet recorded the crash.
    Crashed,
    /// The next boot has recorded the mapping trial's crash and recovers.
    Recovering,
    /// Mapping has nothing left to do and the first filler trial has started.
    Filler,
    /// A calendar night is qualifying the next exact day profile: its first qualification trial has started.
    Qualification,
    /// A cold boot proposes the first hold-earned exact-qualified `climb.decision` promotion.
    DayStep,
    /// A calendar night reproduces the CPU-suspect daily event at the promoted profile: its first reproduction trial
    /// has started.
    Reproduction,
    /// A control trial crashed the machine at CO 0 and the session recorded the dead end.
    Deadend,
    /// The operator ended the night with work left, interrupting a control trial.
    Stopped,
}

impl Scenario {
    pub const ALL: [Self; 12] = [
        Self::Controls,
        Self::Idle,
        Self::Between,
        Self::Mapping,
        Self::Crashed,
        Self::Recovering,
        Self::Filler,
        Self::Qualification,
        Self::DayStep,
        Self::Reproduction,
        Self::Deadend,
        Self::Stopped,
    ];

    /// The name of the scenario's golden.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Controls => "controls",
            Self::Idle => "idle",
            Self::Between => "between",
            Self::Mapping => "mapping",
            Self::Crashed => "crashed",
            Self::Recovering => "recovering",
            Self::Filler => "filler",
            Self::Qualification => "qualification",
            Self::DayStep => "day-step",
            Self::Reproduction => "reproduction",
            Self::Deadend => "deadend",
            Self::Stopped => "stopped",
        }
    }

    /// The simulated session the scenario is cut from.
    pub const fn base(self) -> Base {
        match self {
            Self::Controls | Self::Idle | Self::Between | Self::Stopped => Base::Night,
            Self::Mapping | Self::Crashed | Self::Recovering => Base::Mapped,
            Self::Filler => Base::Filler,
            Self::Qualification | Self::DayStep | Self::Reproduction => Base::Calendar,
            Self::Deadend => Base::DeadEnd,
        }
    }

    /// The compile-time scenario fixture, decoded only through its recorded prefix without simulating or cutting.
    pub fn events(self) -> Result<Vec<Event>, Error> {
        self.base()
            .fixture_events()
            .take(self.fixture_len())
            .collect::<Result<_, _>>()
            .map_err(Error::ReadFixture)
            .map_err(|source| self.failed(source))
    }

    /// The scenario cut from `base`, a journal of [`Scenario::base`] the caller simulated, such as with
    /// [`Base::simulate`].
    pub fn events_from(self, base: &[Event]) -> Result<Vec<Event>, Error> {
        self.cut(base).map_err(|source| self.failed(source))
    }

    const fn fixture_len(self) -> usize {
        include!("fixtures/scenario-cuts.inc")
    }

    fn failed(self, source: Error) -> Error {
        Error::Scenario {
            scenario: self,
            source: Box::new(source),
        }
    }

    fn cut(self, events: &[Event]) -> Result<Vec<Event>, Error> {
        let cut = match self {
            Self::Controls => {
                cut_trial(events, |intent| intent.activity == Some(Activity::Control))?
            }
            Self::Idle => cut_trial(events, |intent| intent.regime == Regime::R6)?,
            Self::Between => cut_passed_trial(events, |intent| {
                intent.activity == Some(Activity::Control) && intent.regime == Regime::R3
            })?,
            Self::Mapping => cut_trial(events, |intent| {
                intent.task.is_some_and(|task| task.queue == Queue::Mapping)
                    && matches!(intent.tested, Some(Tested::Isolated { offset, .. }) if offset != TargetOffset::ZERO)
            })?,
            Self::Crashed => before_crash_detected(events)?,
            Self::Recovering => cut_at(events, |event| event.kind() == EventKind::CrashDetected)?,
            Self::Filler => cut_trial(events, |intent| {
                intent.task.is_some_and(|task| task.queue == Queue::Filler)
            })?,
            Self::Qualification => cut_trial(events, |intent| {
                intent.activity == Some(Activity::Qualification)
            })?,
            Self::DayStep => cut_at(
                events,
                |event| matches!(&event.payload, Payload::ClimbDecision(step) if matches!(step.decision, ClimbAction::Promote { .. })),
            )?,
            Self::Reproduction => cut_trial(events, |intent| {
                intent.activity == Some(Activity::Reproduction)
            })?,
            Self::Deadend => cut_at(events, |event| matches!(event.payload, Payload::DeadEnd(_)))?,
            Self::Stopped => events,
        };
        Ok(cut.to_vec())
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
