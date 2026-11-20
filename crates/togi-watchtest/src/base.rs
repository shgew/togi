use std::num::NonZeroU32;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use togi_journal::{DeadEndCondition, Event, EventKind, MemFs, Outcome, Payload, ShutdownReason};
use togi_machine::{ControllerMode, CoreId, Purpose, Regime, ResetKind, TargetOffset, TrialSpec};
use togi_sim::{History, ScriptedMachine, ScriptedTopology, Simulator, TrialOutcome};
use togi_simrun::Persistence;

use crate::Error;
use crate::simulate::simulate;
use crate::staged_day;

const FAULT_AT: Duration = Duration::from_secs(7);

/// The simulated sessions the scenarios are cut from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Base {
    /// Attested nonzero work, ended by the operator's signal in the trial after the first failure.
    Night,
    /// Mapping and exact qualification work, stopped when the next boot records the mapping crash.
    Mapped,
    /// A staged day promotion followed by the first filler trial at the applied nonzero profile.
    Filler,
    /// Staged day observations and real night work, up to reproduction of the promoted profile.
    StagedReproduction,
    /// A night whose isolated -5 probes all crash, leaving no admissible nonzero work.
    DeadEnd,
}

impl Base {
    pub const ALL: [Self; 5] = [
        Self::Night,
        Self::Mapped,
        Self::Filler,
        Self::StagedReproduction,
        Self::DeadEnd,
    ];

    /// Simulates the session afresh. [`Base::events`] is the same journal, simulated once per process.
    pub fn simulate(self) -> Result<Vec<Event>, Error> {
        match self {
            Self::Night => night(),
            Self::Mapped => mapped(),
            Self::Filler => staged_day::build_journal(staged_day::Goal::Filler),
            Self::StagedReproduction => staged_day::build_journal(staged_day::Goal::Reproduction),
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

fn machine(outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome) -> Result<Simulator, Error> {
    Ok(Simulator::scripted(
        ScriptedMachine::new(ScriptedTopology::SixteenCores, TargetOffset::ZERO, outcome),
        1,
        History::default(),
    )?)
}

fn memory() -> Persistence {
    Persistence::Memory(MemFs::new())
}

fn isolated_r1(spec: &TrialSpec, offsets: &[TargetOffset]) -> Option<(CoreId, i64)> {
    let ControllerMode::Managed { purpose, .. } = spec.controller;
    if purpose != Purpose::Isolated || spec.regime != Regime::R1 {
        return None;
    }
    let core = *spec.cores.first()?;
    Some((core, offsets.get(core.index())?.get()))
}

fn night_outcome(spec: &TrialSpec, offsets: &[TargetOffset]) -> TrialOutcome {
    match isolated_r1(spec, offsets) {
        // Pass the first mapping step, including placement and exposure at R2.
        Some((core, -10)) if core.index() < 8 => {
            TrialOutcome::ComputationError { core, at: FAULT_AT }
        }
        _ => TrialOutcome::Pass,
    }
}

fn mapped_outcome(spec: &TrialSpec, offsets: &[TargetOffset]) -> TrialOutcome {
    match isolated_r1(spec, offsets) {
        // The first edge admits exact qualification. Leave the second CCD enough
        // passing work for the scheduler to reach (and pass) the first R6 hold.
        Some((core, -20)) if core.index() >= 8 => {
            TrialOutcome::UncorrectedMce { core, at: FAULT_AT }
        }
        _ => night_outcome(spec, offsets),
    }
}

fn dead_end_outcome(spec: &TrialSpec, offsets: &[TargetOffset]) -> TrialOutcome {
    match isolated_r1(spec, offsets) {
        Some((core, -5)) => TrialOutcome::Reset {
            stalled: Some(core),
            kind: ResetKind::SyncFlood,
            at: FAULT_AT,
        },
        _ => TrialOutcome::Pass,
    }
}

fn night() -> Result<Vec<Event>, Error> {
    let mut failed = false;
    let events = simulate(
        machine(night_outcome)?,
        &memory(),
        NonZeroU32::MIN,
        move |event| {
            if matches!(&event.payload, Payload::TrialEnd(end) if end.outcome == Outcome::Failure) {
                failed = true;
            }
            failed && event.kind() == EventKind::TrialStart
        },
    )?;
    require_shutdown(
        &events,
        ShutdownReason::Signal,
        "the signal after a nonzero failure",
    )?;
    Ok(events)
}

fn mapped() -> Result<Vec<Event>, Error> {
    let events = simulate(
        machine(mapped_outcome)?,
        &memory(),
        NonZeroU32::MIN.saturating_add(1),
        |event| event.kind() == EventKind::CrashDetected,
    )?;
    if !events
        .iter()
        .any(|event| event.kind() == EventKind::CrashDetected)
    {
        return Err(Error::NoCut(
            "the scripted mapping crash after qualification",
        ));
    }
    Ok(events)
}

fn dead_end() -> Result<Vec<Event>, Error> {
    let simulator = machine(dead_end_outcome)?;
    let events = simulate(
        simulator.clone(),
        &memory(),
        NonZeroU32::MIN.saturating_add(2),
        |_| false,
    )?;
    if !events.iter().any(
        |event| matches!(&event.payload, Payload::DeadEnd(dead) if dead.condition == DeadEndCondition::NoEvidence),
    ) {
        return Err(Error::NoCut("a dead end after exhausting isolated probes"));
    }
    require_shutdown(&events, ShutdownReason::DeadEnd, "the no-work shutdown")?;
    let mut smu = simulator.seams().smu;
    for index in 0..16 {
        let core = CoreId::new(index)?;
        let offset = smu
            .offset(core)
            .map_err(|source| Error::StockRead { core, source })?;
        if offset != TargetOffset::ZERO.observed() {
            return Err(Error::NotStock { core, offset });
        }
    }
    Ok(events)
}

fn require_shutdown(
    events: &[Event],
    reason: ShutdownReason,
    milestone: &'static str,
) -> Result<(), Error> {
    if events.last().is_some_and(
        |event| matches!(&event.payload, Payload::Shutdown(shutdown) if shutdown.reason == reason),
    ) {
        Ok(())
    } else {
        Err(Error::NoCut(milestone))
    }
}
