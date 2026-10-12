use std::fmt;
use std::sync::OnceLock;

use togi_config::Config;
use togi_journal::{Activity, Event, Payload};
use togi_machine::Regime;

use crate::Error;
use crate::simulate::{Stop, crash_trial, default_machine, never, probe_machine, simulate};

/// The seed whose default machine, run through three clean cycles, leaves a core's BIOS offset unconfirmed after a
/// failure. [INFERENCE: other seeds steer clear of it; found by scanning seeds 1 to 60. The Go simulator's seed 30
/// does not leave one here.]
const UNCONFIRMED_SEED: u64 = 43;

/// How many trials from the first checking trial of cycle 4 on crash without naming a core. [INFERENCE: found by
/// scanning; a shorter burst stays within the hold.]
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
    /// The default machine through six clean cycles, rerun with a burst of crashes naming no core on the trials from
    /// the first checking trial of cycle 4 on: after a held first failure, a second makes a hunt while the confirmed
    /// profile shows a held, unconfirmed core.
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
}

impl Base {
    pub const ALL: [Self; 8] = [
        Self::Session,
        Self::Concluded,
        Self::Unconfirmed,
        Self::LaterHunt,
        Self::FirstResult,
        Self::Probe,
        Self::Combination,
        Self::CrashedCycle,
    ];

    /// Simulates the session afresh. [`Base::events`] is the same journal, simulated once per process.
    pub fn simulate(self) -> Result<Vec<Event>, Error> {
        match self {
            Self::Session => simulate(
                default_machine(1)?,
                Config::default(),
                never,
                Stop::cycles(1),
            ),
            Self::Concluded => simulate(
                default_machine(1)?,
                Config::default(),
                never,
                Stop::cycles(2),
            ),
            Self::Unconfirmed => simulate(
                default_machine(UNCONFIRMED_SEED)?,
                Config::default(),
                never,
                Stop::cycles(3),
            ),
            Self::LaterHunt => later_hunt(),
            Self::FirstResult => simulate(
                default_machine(1)?,
                Config::default(),
                never,
                Stop::FirstResult,
            ),
            Self::Probe => {
                let (machine, config) = probe_machine()?;
                let mut deepening = false;
                simulate(
                    machine,
                    config,
                    move |event| match &event.payload {
                        Payload::TrialIntent(intent) => {
                            deepening = intent.activity == Some(Activity::Deepening);
                            false
                        }
                        Payload::TrialStart(_) => deepening,
                        _ => false,
                    },
                    Stop::cycles(1),
                )
            }
            Self::Combination => {
                let (machine, config) = probe_machine()?;
                simulate(machine, config, never, Stop::cycles(1))
            }
            Self::CrashedCycle => simulate(
                default_machine(5)?,
                Config::default(),
                never,
                Stop::cycles(1),
            ),
        }
    }

    /// The simulated journal, simulated once per process.
    pub fn events(self) -> Result<&'static [Event], Error> {
        static CELLS: [OnceLock<Result<Vec<Event>, String>>; 8] = [const { OnceLock::new() }; 8];
        let index = Self::ALL.iter().position(|base| *base == self).unwrap_or(0);
        CELLS[index]
            .get_or_init(|| self.simulate().map_err(|error| error.to_string()))
            .as_deref()
            .map_err(|message| Error::Shared(message.clone()))
    }
}

impl fmt::Display for Base {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

fn later_hunt() -> Result<Vec<Event>, Error> {
    let six = Stop::cycles(6);
    let base = simulate(default_machine(1)?, Config::default(), never, six)?;
    let first = base
        .iter()
        .find_map(|event| match &event.payload {
            Payload::TrialIntent(intent)
                if intent.activity == Some(Activity::Checking)
                    && intent.cycle.is_some_and(|cycle| cycle.get() >= 4)
                    && intent.regime != Regime::R7 =>
            {
                intent.trial.as_str().parse::<u32>().ok()
            }
            _ => None,
        })
        .ok_or_else(|| Error::Fixture("no checking trial in cycle 4".into()))?;
    let machine = default_machine(1)?;
    for id in first..first + BURST {
        crash_trial(&machine, &format!("{id:04}"), 7.0, 3)?;
    }
    simulate(machine, Config::default(), never, six)
}
