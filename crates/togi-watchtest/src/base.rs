use std::cell::RefCell;
use std::ops::ControlFlow;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jiff::SignedDuration;
use togi_config::Config;
use togi_journal::{Activity, DayBootStart, Event, EventKind, Payload, Queue, SessionStart, Stamp};
use togi_machine::Machine;
use togi_simrun::day_night::{
    CalendarConfig, DayClassification, DayObservation, EndAnswer, Night, Policy, Ruleset13,
    RunError, run_with_policy,
};
use togi_tuner::policy::Params;

use crate::Error;
use crate::simulate::{Halt, build, crash_trial, default_machine, simulate};

/// The night the simulated operator gives the session: eight hours, as the scenarios have always had.
const NIGHT: SignedDuration = SignedDuration::from_hours(8);

/// Long enough to reach a mapping crash or exhaust all isolated -5 probes. Generation stops as soon as the
/// requested state is reached.
const LONG_NIGHT: SignedDuration = SignedDuration::from_hours(60);

/// The calendar simulator's machine for [`Base::Calendar`]: a deep edge, so the first promotion comes within a month.
const CALENDAR_MACHINE: &str = "edge-deep";

/// The calendar's safety bound: generation stops at the first reproduction trial start, or fails
/// if that transition is not reached within a month.
const CALENDAR_DAYS: u32 = 30;

/// The simulated sessions the scenarios are cut from. Each is the default machine at seed 1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Base {
    /// An eight-hour night of attested nonzero mapping, ended by the operator's signal.
    Night,
    /// A long mapping night, stopped when the next boot records its first crash below zero.
    Mapped,
    /// Real days and nights until the first filler trial at a promoted nonzero day profile starts.
    Filler,
    /// Bounded-exploration days and nights up to the first reproduction trial start on `edge-deep`, seed 1: the real policy
    /// decides every night trial and day step, the trial runner runs them on a simulated machine, and the day
    /// observer's boots, checkpoints and clean stops are the calendar's. Its nights qualify an exact profile, which a
    /// later cold boot promotes. The one scripted fact is the operator's: the first day after the promotion ends in
    /// a crash the operator answers as the CPU's, which makes it a CPU-suspect daily event the next night reproduces.
    /// The session is not `togi run`, so the journal has no `config.loaded`, preflight or SMU transaction; its builds
    /// are rewritten to [`crate::build`].
    Calendar,
    /// A night whose isolated -5 probes all crash: implicit stock passes remain, but no nonzero work is admissible.
    DeadEnd,
}

impl Base {
    pub const ALL: [Self; 5] = [
        Self::Night,
        Self::Mapped,
        Self::Filler,
        Self::Calendar,
        Self::DeadEnd,
    ];

    /// Simulates the session afresh. [`Base::events`] is the same journal, simulated once per process.
    pub fn simulate(self) -> Result<Vec<Event>, Error> {
        match self {
            Self::Night => simulate(default_machine(1)?, Config::default(), None, NIGHT),
            Self::Mapped => simulate(
                default_machine(1)?,
                Config::default(),
                Some(Box::new(|event| event.kind() == EventKind::CrashDetected)),
                LONG_NIGHT,
            ),
            Self::Filler => calendar(CalendarGoal::Filler),
            Self::Calendar => calendar(CalendarGoal::Reproduction),
            Self::DeadEnd => dead_end(),
        }
    }

    /// The simulated journal, simulated once per process.
    pub fn events(self) -> Result<&'static [Event], Error> {
        static CELLS: [OnceLock<Result<Vec<Event>, Arc<Error>>>; Base::ALL.len()] =
            [const { OnceLock::new() }; Base::ALL.len()];
        let index = Self::ALL.iter().position(|base| *base == self).unwrap_or(0);
        CELLS[index]
            .get_or_init(|| self.simulate().map_err(Arc::new))
            .as_deref()
            .map_err(|error| Error::Shared(Arc::clone(error)))
    }
}

fn calendar(goal: CalendarGoal) -> Result<Vec<Event>, Error> {
    let mut policy = CpuAnswerAfterPromotion {
        inner: Ruleset13::with_params(Params::SPEC).recording(),
        progress: CalendarProgress::AwaitingPromotion,
        goal,
    };
    run_with_policy(
        CALENDAR_MACHINE,
        1,
        CalendarConfig {
            days: if matches!(goal, CalendarGoal::Filler) {
                90
            } else {
                CALENDAR_DAYS
            },
            ..CalendarConfig::default()
        },
        &mut policy,
    )?;
    if !policy.is_finished() {
        return Err(Error::NoCut(match goal {
            CalendarGoal::Filler => "the Calendar's first nonzero filler trial start",
            CalendarGoal::Reproduction => "the Calendar's first reproduction trial start",
        }));
    }
    Ok(policy
        .inner
        .events()
        .ok_or(Error::NoCut("the recorded Calendar journal"))?
        .iter()
        .map(with_fixed_build)
        .collect())
}

/// The real ruleset policy, with an optional operator CPU answer after the first promotion.
struct CpuAnswerAfterPromotion {
    inner: Ruleset13,
    progress: CalendarProgress,
    goal: CalendarGoal,
}

#[derive(Clone, Copy)]
enum CalendarGoal {
    Filler,
    Reproduction,
}

enum CalendarProgress {
    AwaitingPromotion,
    AwaitingReproduction,
    Complete,
}

impl Policy for CpuAnswerAfterPromotion {
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.inner.firmware_changed(machine)
    }

    fn start_day(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.inner.start_day(day, machine)
    }

    fn observe_day(
        &mut self,
        day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        let promoted = self
            .inner
            .state()
            .project()
            .applied
            .is_some_and(|profile| !profile.is_zero());
        if promoted
            && matches!(self.goal, CalendarGoal::Reproduction)
            && matches!(self.progress, CalendarProgress::AwaitingPromotion)
        {
            self.progress = CalendarProgress::AwaitingReproduction;
            let crashed = DayObservation::Ended {
                active: Duration::from_hours(1),
                answer: Some(EndAnswer::Cpu),
            };
            return self.inner.observe_day(day, &crashed, machine);
        }
        self.inner.observe_day(day, evidence, machine)
    }

    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let goal = self.goal;
        let progress = self
            .inner
            .night_until_trial_start(night, machine, |intent| match goal {
                CalendarGoal::Reproduction => intent.activity == Some(Activity::Reproduction),
                CalendarGoal::Filler => {
                    intent.task.is_some_and(|task| task.queue == Queue::Filler)
                        && intent.profile.iter().any(|(_, offset)| offset.get() != 0)
                }
            })?;
        if let ControlFlow::Break(()) = progress {
            self.progress = CalendarProgress::Complete;
        }
        Ok(())
    }

    fn is_finished(&self) -> bool {
        matches!(self.progress, CalendarProgress::Complete)
    }
}

/// `event` naming [`crate::build`] instead of the calendar's build, whose version is the crate's.
fn with_fixed_build(event: &Event) -> Event {
    let payload = if let Payload::SessionStart(start) = &event.payload {
        Payload::SessionStart(SessionStart {
            build: build(),
            ..start.clone()
        })
    } else if let Payload::DayBootStart(boot) = &event.payload {
        Payload::DayBootStart(DayBootStart {
            build: build(),
            ..boot.clone()
        })
    } else {
        return event.clone();
    };
    Event::new(
        event.seq,
        Stamp {
            time: event.time,
            mono: event.mono,
        },
        event.boot.clone(),
        event.cause.clone(),
        payload,
    )
}

fn dead_end() -> Result<Vec<Event>, Error> {
    let machine = default_machine(1)?;
    let fault = machine.clone();
    let failed: Rc<RefCell<Option<Error>>> = Rc::new(RefCell::new(None));
    let report = Rc::clone(&failed);
    let arm: Halt = Box::new(move |event| {
        let Payload::TrialIntent(intent) = &event.payload else {
            return false;
        };
        if intent.activity != Some(Activity::Mapping) {
            return false;
        }
        let Some(core) = intent.cores.first() else {
            return false;
        };
        let crash = crash_trial(&fault, intent.trial.as_str(), 7.0, core);
        match crash {
            Ok(()) => false,
            Err(error) => {
                *report.borrow_mut() = Some(error);
                true
            }
        }
    });
    let events = simulate(machine, Config::default(), Some(arm), LONG_NIGHT)?;
    if let Some(error) = failed.take() {
        return Err(error);
    }
    Ok(events)
}
