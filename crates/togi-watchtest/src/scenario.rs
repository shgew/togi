use std::fmt;

use togi_journal::{Activity, Event, EventKind, Payload};
use togi_machine::Regime;
use togi_tuner::PhaseNo;

use crate::cut::{
    before_crash_detected, cut_at, cut_passed_trial, cut_trial, cut_when,
    hunt_with_unconfirmed_profile,
};
use crate::{Base, Error};

/// A watch golden scenario that has a journal: the state of a simulated session at the moment its name claims.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Scenario {
    /// The first search trial that tries a nonzero offset has started.
    Search,
    /// The probe machine's first search trial has started, before any core is confirmed.
    Confirm,
    /// The first R7 checking trial has started.
    Checking,
    /// The first hunt trial has started.
    Hunt,
    /// A hunt's first member probe trial has started, on the probe machine.
    MemberProbe,
    /// The probe machine's first deepening trial, a phase 2 round, has started.
    Round,
    /// A phase 2 confirmation trial has started.
    Confirmation,
    /// Phase 2 has concluded and a checking trial runs on.
    Concluded,
    /// A trial has started while a core's BIOS offset is unconfirmed.
    Unconfirmed,
    /// A hunt trial has started while the BIOS profile is confirmed and a core's offset is unconfirmed.
    HuntBios,
    /// The first idle (R6) trial has started.
    Idle,
    /// A trial the machine outlived: the next boot has started and has not yet recorded the crash.
    Crashed,
    /// The next boot has recorded the crash and recovers.
    Recovering,
    /// The probe machine's session ended with the combination it found.
    Combination,
    /// The first passed checking cycle trial has ended and the next has not begun.
    Between,
    /// The session has recorded a dead end without evidence, after its R7 checking trial ended and the next trial found
    /// no backend.
    Deadend,
    /// The session stopped at its first confirmed BIOS profile.
    FirstResult,
    /// The default session ran to its stop.
    Stopped,
}

impl Scenario {
    pub const ALL: [Self; 18] = [
        Self::Search,
        Self::Checking,
        Self::Hunt,
        Self::MemberProbe,
        Self::Round,
        Self::Confirmation,
        Self::Concluded,
        Self::Unconfirmed,
        Self::HuntBios,
        Self::Idle,
        Self::Crashed,
        Self::Recovering,
        Self::Combination,
        Self::Between,
        Self::Confirm,
        Self::Deadend,
        Self::FirstResult,
        Self::Stopped,
    ];

    /// The name of the scenario's golden.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Checking => "checking",
            Self::Hunt => "hunt",
            Self::MemberProbe => "member-probe",
            Self::Round => "round",
            Self::Confirmation => "confirmation",
            Self::Concluded => "concluded",
            Self::Unconfirmed => "unconfirmed",
            Self::HuntBios => "hunt-bios",
            Self::Idle => "idle",
            Self::Crashed => "crashed",
            Self::Recovering => "recovering",
            Self::Combination => "combination",
            Self::Between => "between",
            Self::Confirm => "confirm",
            Self::Deadend => "deadend",
            Self::FirstResult => "first-result",
            Self::Stopped => "stopped",
        }
    }

    /// The simulated session the scenario is cut from.
    pub const fn base(self) -> Base {
        match self {
            Self::Confirm | Self::MemberProbe | Self::Round => Base::Probe,
            Self::Combination => Base::Combination,
            Self::Deadend => Base::DeadEnd,
            Self::Concluded => Base::Concluded,
            Self::Unconfirmed => Base::Unconfirmed,
            Self::HuntBios => Base::LaterHunt,
            Self::FirstResult => Base::FirstResult,
            Self::Search
            | Self::Checking
            | Self::Hunt
            | Self::Confirmation
            | Self::Idle
            | Self::Crashed
            | Self::Recovering
            | Self::Between
            | Self::Stopped => Base::Session,
        }
    }

    /// The scenario's journal. Its base simulation runs once per process; the cut is copied.
    pub fn events(self) -> Result<Vec<Event>, Error> {
        self.base()
            .events()
            .and_then(|base| self.cut(base))
            .map_err(|source| self.failed(source))
    }

    /// The scenario cut from `base`, a journal of [`Scenario::base`] the caller simulated, such as with
    /// [`Base::simulate`].
    pub fn events_from(self, base: &[Event]) -> Result<Vec<Event>, Error> {
        self.cut(base).map_err(|source| self.failed(source))
    }

    fn failed(self, source: Error) -> Error {
        Error::Scenario {
            scenario: self,
            source: Box::new(source),
        }
    }

    fn cut(self, events: &[Event]) -> Result<Vec<Event>, Error> {
        let cut = match self {
            Self::Search => cut_trial(events, |intent| {
                intent.activity == Some(Activity::Search)
                    && intent.offset.is_some_and(|offset| offset.get() < 0)
            })?,
            Self::Confirm => cut_trial(events, |intent| intent.activity == Some(Activity::Search))?,
            Self::Checking => checking_r7(events)?,
            Self::Hunt => cut_trial(events, |intent| intent.activity == Some(Activity::Hunt))?,
            Self::MemberProbe => {
                let mut probing = false;
                cut_at(events, |event| {
                    if matches!(&event.payload, Payload::HuntGroup(group) if group.probe.is_some())
                    {
                        probing = true;
                    }
                    probing && event.kind() == EventKind::TrialStart
                })?
            }
            Self::Round => cut_trial(events, |intent| {
                intent.activity == Some(Activity::Deepening)
            })?,
            Self::Confirmation => cut_when(events, |state, event| {
                event.kind() == EventKind::TrialStart
                    && state
                        .phase_plan()
                        .is_some_and(|plan| plan.phase == PhaseNo::Two && plan.confirming)
            })?,
            Self::Concluded => cut_when(events, |state, event| {
                event.kind() == EventKind::TrialStart
                    && state
                        .phase_plan()
                        .is_some_and(|plan| plan.phase == PhaseNo::Concluded)
            })?,
            Self::Unconfirmed => cut_when(events, |state, event| {
                event.kind() == EventKind::TrialStart
                    && state
                        .bios_profile()
                        .is_some_and(|bios| !bios.unconfirmed.is_empty())
            })?,
            Self::HuntBios => hunt_with_unconfirmed_profile(events)?,
            Self::Idle => cut_trial(events, |intent| intent.regime == Regime::R6)?,
            Self::Between => cut_passed_trial(events, |intent| {
                intent.activity == Some(Activity::Checking)
                    && intent.cycle.is_some()
                    && !intent.rerun
            })?,
            Self::Crashed => before_crash_detected(events)?,
            Self::Recovering => cut_at(events, |event| event.kind() == EventKind::CrashDetected)?,
            Self::Stopped | Self::Combination | Self::FirstResult => events,
            Self::Deadend => cut_at(events, |event| event.kind() == EventKind::DeadEnd)?,
        };
        Ok(cut.to_vec())
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn checking_r7(events: &[Event]) -> Result<&[Event], Error> {
    cut_trial(events, |intent| {
        intent.activity == Some(Activity::Checking) && intent.regime == Regime::R7
    })
}
