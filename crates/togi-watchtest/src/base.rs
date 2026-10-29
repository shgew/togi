use std::cell::RefCell;
use std::ops::ControlFlow;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jiff::SignedDuration;
use togi_config::Config;
use togi_journal::{
    Activity, DayBootStart, Event, EventKind, Payload, Queue, SessionStart, Stamp, TrialId,
};
use togi_machine::{CoreId, Machine};
use togi_simrun::day_night::{
    CalendarConfig, DayClassification, DayObservation, EndAnswer, Night, Policy, Ruleset13,
    RunError, run_with_policy,
};
use togi_tuner::policy::Params;

use crate::Error;
use crate::simulate::{Halt, build, crash_trial, default_machine, simulate};

/// The night the simulated operator gives the session: eight hours, as the scenarios have always had.
const NIGHT: SignedDuration = SignedDuration::from_hours(8);

/// The night the operator leaves running until mapping is done: longer than the default machine needs to reach its
/// first filler trial, about fifty hours in. The bases that need it stop as soon as their state is reached.
const LONG_NIGHT: SignedDuration = SignedDuration::from_hours(60);

/// The calendar simulator's machine for [`Base::Calendar`]: a deep edge, so the first promotion comes within a month.
const CALENDAR_MACHINE: &str = "edge-deep";

/// The calendar's safety bound: generation stops at the first reproduction trial start, or fails
/// if that transition is not reached within a month.
const CALENDAR_DAYS: u32 = 30;

/// How many control trials run before [`Base::DeadEnd`] crashes one: the fourth.
const CRASH_AFTER: usize = 3;

/// The simulated sessions the scenarios are cut from. Each is the default machine at seed 1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Base {
    /// An eight-hour night the operator ends: the first night's CO-0 controls, all passing, and the operator's
    /// signal interrupting the last of them.
    Night,
    /// A night the operator leaves running past its controls into mapping, stopped once the next boot has recorded
    /// the crash a mapping trial at CCD1, CO -10 causes (about fifteen hours in).
    Mapped,
    /// The same long night, stopped once its first filler trial has ended, after mapping has nothing left to do
    /// (about fifty hours in).
    Filler,
    /// Ruleset 13 days and nights up to the first reproduction trial start on `edge-deep`, seed 1: the real policy
    /// decides every night trial and day step, the trial runner runs them on a simulated machine, and the day
    /// observer's boots, checkpoints and clean stops are the calendar's. Its nights qualify an exact profile, which a
    /// later cold boot promotes. The one scripted fact is the operator's: the first day after the promotion ends in
    /// a crash the operator answers as the CPU's, which makes it a CPU-suspect daily event the next night reproduces.
    /// The session is not `togi run`, so the journal has no `config.loaded`, preflight or SMU transaction; its builds
    /// are rewritten to [`crate::build`].
    Calendar,
    /// The night whose fourth control trial crashes the machine at CO 0, a conclusive failure at stock: the next
    /// boot records the crash and the session dead-ends.
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
            Self::Filler => simulate(
                default_machine(1)?,
                Config::default(),
                Some(after_first_filler()),
                LONG_NIGHT,
            ),
            Self::Calendar => calendar(),
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

/// Halts after the `trial.end` of the first filler trial.
fn after_first_filler() -> Halt {
    let mut filler: Option<TrialId> = None;
    Box::new(move |event| {
        if let Payload::TrialIntent(intent) = &event.payload {
            if filler.is_none() && intent.task.is_some_and(|task| task.queue == Queue::Filler) {
                filler = Some(intent.trial.clone());
            }
        } else if let Payload::TrialEnd(end) = &event.payload {
            return filler.as_ref() == Some(&end.trial);
        }
        false
    })
}

fn calendar() -> Result<Vec<Event>, Error> {
    let mut policy = CpuAnswerAfterPromotion {
        inner: Ruleset13::with_params(Params::SPEC).recording(),
        progress: CalendarProgress::AwaitingPromotion,
    };
    run_with_policy(
        CALENDAR_MACHINE,
        1,
        CalendarConfig {
            days: CALENDAR_DAYS,
            ..CalendarConfig::default()
        },
        &mut policy,
    )?;
    if !policy.is_finished() {
        return Err(Error::NoCut("the Calendar's first reproduction trial start"));
    }
    Ok(policy
        .inner
        .events()
        .ok_or(Error::NoCut("the recorded Calendar journal"))?
        .iter()
        .map(with_fixed_build)
        .collect())
}

/// Ruleset 13, except that the first day observed at a promoted profile ends in a crash the operator blames on the
/// CPU.
struct CpuAnswerAfterPromotion {
    inner: Ruleset13,
    progress: CalendarProgress,
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
        if promoted && matches!(self.progress, CalendarProgress::AwaitingPromotion) {
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
        let progress = self.inner.night_until_trial_start(night, machine, |intent| {
            intent.activity == Some(Activity::Reproduction)
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
    let mut intents = 0;
    let arm: Halt = Box::new(move |event| {
        let Payload::TrialIntent(intent) = &event.payload else {
            return false;
        };
        if intent.activity != Some(Activity::Control) {
            return false;
        }
        intents += 1;
        if intents != CRASH_AFTER + 1 {
            return false;
        }
        let crash = CoreId::new(3)
            .map_err(Error::from)
            .and_then(|core| crash_trial(&fault, intent.trial.as_str(), 7.0, core));
        match crash {
            Ok(()) => false,
            Err(error) => {
                *report.borrow_mut() = Some(error);
                true
            }
        }
    });
    let events = simulate(machine, Config::default(), Some(arm), NIGHT)?;
    if let Some(error) = failed.take() {
        return Err(error);
    }
    Ok(events)
}
