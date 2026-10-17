use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use togi_config::Config;
use togi_journal::{Activity, Event, Payload};
use togi_machine::{Backend, Regime};
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;
use togi_tuner::State;

use crate::Error;
use crate::simulate::{
    Stop, Until, crash_trial, default_machine, machine, probe_machine, simulate,
};

/// The seed whose default machine, run through three clean cycles, leaves a core's BIOS offset unconfirmed
/// after a failure, found by scanning seeds 1 to 60.
const UNCONFIRMED_SEED: u64 = 43;

/// How many consecutive trials crash without naming a core, starting at the first non-R7 checking trial of cycle 4
/// on after a BIOS profile is confirmed. [INFERENCE: found by scanning; a shorter burst stays within the hold.]
const BURST: u32 = 6;

/// The simulated sessions the scenarios and the other named journals are cut from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Base {
    /// The default machine, seed 1, through its first clean cycle: search, checking with R6, hunts, crash recoveries
    /// and the stop.
    Session,
    /// The default machine, seed 1, through two clean cycles, so checking runs on after phase 2.
    Concluded,
    /// The default machine on seed 43 through three clean cycles.
    Unconfirmed,
    /// The default machine without crash MCEs through three clean cycles, with six consecutive crashes naming no
    /// core starting at the first non-R7 checking trial of cycle 4 on after a BIOS profile is confirmed: after a held
    /// first failure, a second makes a hunt while the confirmed profile shows a held, unconfirmed core.
    LaterHunt,
    /// The default machine stopped at its first confirmed BIOS profile.
    FirstResult,
    /// The probe machine until its first deepening trial starts.
    Probe,
    /// The probe machine to its stop, with the combination it found.
    Combination,
    /// The default machine on seed 5 through its first clean cycle, whose checking crashes are followed by a backoff
    /// that moves the cycle on.
    CrashedCycle,
    /// The default machine, seed 1, whose mprime backend goes missing once the first R7 checking cycle trial has
    /// started: that trial ends, the next mprime trial cannot start, and the session dead-ends without evidence.
    DeadEnd,
}

impl Base {
    pub const ALL: [Self; 9] = [
        Self::Session,
        Self::Concluded,
        Self::Unconfirmed,
        Self::LaterHunt,
        Self::FirstResult,
        Self::Probe,
        Self::Combination,
        Self::CrashedCycle,
        Self::DeadEnd,
    ];

    /// Simulates the session afresh. [`Base::events`] is the same journal, simulated once per process.
    pub fn simulate(self) -> Result<Vec<Event>, Error> {
        match self {
            Self::Session => simulate(
                default_machine(1)?,
                Config::default(),
                None,
                Stop::cycles(1),
            ),
            Self::Concluded => simulate(
                default_machine(1)?,
                Config::default(),
                None,
                Stop::cycles(2),
            ),
            Self::Unconfirmed => simulate(
                default_machine(UNCONFIRMED_SEED)?,
                Config::default(),
                None,
                Stop::cycles(3),
            ),
            Self::LaterHunt => later_hunt(),
            Self::FirstResult => simulate(
                default_machine(1)?,
                Config::default(),
                None,
                Stop::FirstResult,
            ),
            Self::Probe => {
                let (machine, config) = probe_machine()?;
                let mut deepening = false;
                simulate(
                    machine,
                    config,
                    Some(Box::new(move |event| match &event.payload {
                        Payload::TrialIntent(intent) => {
                            deepening = intent.activity == Some(Activity::Deepening);
                            false
                        }
                        Payload::TrialStart(_) => deepening,
                        _ => false,
                    })),
                    Stop::cycles(1),
                )
            }
            Self::Combination => {
                let (machine, config) = probe_machine()?;
                simulate(machine, config, None, Stop::cycles(1))
            }
            Self::CrashedCycle => simulate(
                default_machine(5)?,
                Config::default(),
                None,
                Stop::cycles(1),
            ),
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

fn later_hunt() -> Result<Vec<Event>, Error> {
    // Scripted per-core crashes otherwise draw MCE evidence, turning the intended unnamed burst into named failures.
    let definition = MachineDefinition {
        model: ModelDef {
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    };
    let machine = machine(&definition, 1)?;
    let fault = machine.clone();
    let armed = Rc::new(RefCell::new(Ok(0)));
    let result = Rc::clone(&armed);
    let mut state = State::new();
    let arm: Until = Box::new(move |event| {
        let mut result = result.borrow_mut();
        let Ok(count) = result.as_mut() else {
            return true;
        };
        if *count == BURST {
            return false;
        }
        if *count == 0
            && let Err(error) = state.fold(event)
        {
            *result = Err(error.into());
            return true;
        }
        let Payload::TrialIntent(intent) = &event.payload else {
            return false;
        };
        if *count == 0
            && !(intent.activity == Some(Activity::Checking)
                && intent.cycle.is_some_and(|cycle| cycle.get() >= 4)
                && intent.regime != Regime::R7
                && state
                    .bios_profile()
                    .is_some_and(|bios| bios.confirmed.is_some()))
        {
            return false;
        }
        match crash_trial(&fault, intent.trial.as_str(), 7.0, 3) {
            Ok(()) => {
                *count += 1;
                false
            }
            Err(error) => {
                *result = Err(error);
                true
            }
        }
    });
    let events = simulate(machine, Config::default(), Some(arm), Stop::cycles(3))?;
    let count = armed.replace(Ok(0))?;
    if count != BURST {
        return Err(Error::Fixture(format!(
            "the later hunt armed {count} crash trials, not the full burst of {BURST}"
        )));
    }
    Ok(events)
}

fn dead_end() -> Result<Vec<Event>, Error> {
    let machine = default_machine(1)?;
    let fault = machine.clone();
    let mut checking_r7 = false;
    let mut armed = false;
    let arm: Until = Box::new(move |event| {
        match &event.payload {
            Payload::TrialIntent(intent) => {
                checking_r7 = intent.activity == Some(Activity::Checking)
                    && intent.regime == Regime::R7
                    && intent.cycle.is_some()
                    && !intent.rerun;
            }
            Payload::TrialStart(_) if checking_r7 && !armed => {
                fault.miss_backend(Backend::Mprime);
                armed = true;
            }
            _ => {}
        }
        false
    });
    simulate(machine, Config::default(), Some(arm), Stop::cycles(1))
}
