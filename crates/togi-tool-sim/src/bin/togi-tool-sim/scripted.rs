use std::io::Write as _;
use std::time::Duration;

use togi_journal::{
    ApplyCause, DeadEndCondition, Event, EventKind, Outcome, Payload, Placement, ShutdownReason,
};
use togi_machine::{
    BankType, ControllerMode, Purpose, Regime, ResetKind, Signal, TargetOffset, TrialSpec,
};
use togi_sim::{History, ScriptedMachine, ScriptedTopology, Simulator, TrialOutcome};
use togi_simrun::Persistence;
use togi_simrun::scripted::{Error, StagedDayGoal, night, staged_day};
use togi_tool_sim::scripted::{CONTRACT_VERSION, ScriptedCase};

use crate::{ScriptedArgs, build, log::Log};

const FAULT_AT: Duration = Duration::from_secs(7);
// The session records the intent before selecting managed placement, so the first trial's
// persistent rotation is 1. After a reboot the resumed trials carry later rotations.
const FIRST_TRIAL: u64 = 1;

pub(crate) fn run(args: ScriptedArgs, log: &mut Log) -> u8 {
    if args.contract != CONTRACT_VERSION {
        let _ = writeln!(
            log,
            "sim: cannot run the scripted comparison set: expected scripted input contract v{CONTRACT_VERSION}, got {:?}",
            args.contract
        );
        return 1;
    }
    if args.max_boots != args.case.max_boots() {
        let _ = writeln!(
            log,
            "sim: {} requires its fixed boot bound of {}",
            args.case.name(),
            args.case.max_boots()
        );
        return 1;
    }
    let case = args.case;
    match execute(case, &Persistence::Files(args.state_dir)) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(
                log,
                "sim: {}: {}",
                case.name(),
                togi_machine::error_chain(&error)
            );
            1
        }
    }
}

fn execute(case: ScriptedCase, persistence: &Persistence) -> Result<(), Error> {
    if !togi_simrun::events(persistence)?.is_empty() {
        return Err(Error::Milestone(
            "the scripted state directory must start with an empty journal",
        ));
    }
    let build = build();
    let outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome = match case {
        ScriptedCase::MappingStop => mapping_outcome,
        ScriptedCase::MceResume => mce_outcome,
        ScriptedCase::PowerButtonReset => power_button_outcome,
        ScriptedCase::NoNonzeroWork => no_work_outcome,
        ScriptedCase::ProfilePlacement => {
            let events = staged_day(persistence, &build, StagedDayGoal::Filler)?;
            require(
                profile_and_placement(&events),
                "a verified day profile and a managed exact-profile trial",
            )?;
            return shutdown(&events, ShutdownReason::Signal);
        }
    };
    let script = ScriptedMachine::new(ScriptedTopology::FourCores, TargetOffset::ZERO, outcome);
    let sim = Simulator::scripted(script, 1, History::default())?;
    let mut milestone = false;
    let events = night(
        sim.clone(),
        persistence,
        &build,
        case.max_boots(),
        move |event| match case {
            ScriptedCase::MappingStop => {
                milestone |= matches!(&event.payload, Payload::TrialEnd(end)
                    if end.signal == Some(Signal::ComputationError));
                milestone && event.kind() == EventKind::TrialStart
            }
            ScriptedCase::MceResume | ScriptedCase::PowerButtonReset => {
                milestone |= event.kind() == EventKind::CrashDetected;
                milestone
                    && matches!(&event.payload, Payload::TrialEnd(end) if end.outcome == Outcome::Pass)
            }
            ScriptedCase::ProfilePlacement | ScriptedCase::NoNonzeroWork => false,
        },
    )?;
    match case {
        ScriptedCase::MappingStop => {
            require(events.iter().any(|event| matches!(&event.payload, Payload::TrialEnd(end)
                if end.signal == Some(Signal::ComputationError))) && events.iter().any(|event|
                    matches!(&event.payload, Payload::ExposureObserved(witness)
                        if witness.class.regime == Regime::R2 && witness.outcome == Outcome::Pass && witness.eligible_s > 0)),
                "credited R2 work and a nonzero computation error")?;
        }
        ScriptedCase::MceResume => {
            require(
                events.iter().any(|event| {
                    matches!(&event.payload, Payload::Mce(mce)
                if !mce.corrected && mce.bank_type == BankType::LoadStore)
                }),
                "an uncorrected load/store machine check",
            )?;
            resumed_after(&events, ResetKind::SyncFlood)?;
        }
        ScriptedCase::PowerButtonReset => resumed_after(&events, ResetKind::PowerButton)?,
        ScriptedCase::NoNonzeroWork => {
            require(
                events.iter().any(|event| {
                    matches!(&event.payload, Payload::DeadEnd(dead)
                if dead.condition == DeadEndCondition::NoEvidence)
                }),
                "a no-evidence dead end",
            )?;
            shutdown(&events, ShutdownReason::DeadEnd)?;
            return at_stock(&sim);
        }
        ScriptedCase::ProfilePlacement => return Err(Error::Milestone("a night-only script")),
    }
    shutdown(&events, ShutdownReason::Signal)?;
    at_stock(&sim)
}

fn mapping_outcome(spec: &TrialSpec, offsets: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { purpose, .. } = spec.controller;
    if purpose == Purpose::Isolated
        && spec.regime == Regime::R1
        && let Some(&core) = spec.cores.first()
        && offsets[core.index()].get() == -10
    {
        return TrialOutcome::ComputationError { core, at: FAULT_AT };
    }
    TrialOutcome::Pass
}

fn mce_outcome(spec: &TrialSpec, _offsets: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { rotation, .. } = spec.controller;
    if rotation == FIRST_TRIAL
        && let Some(&core) = spec.cores.first()
    {
        return TrialOutcome::UncorrectedMce { core, at: FAULT_AT };
    }
    TrialOutcome::Pass
}

fn power_button_outcome(spec: &TrialSpec, _offsets: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { rotation, .. } = spec.controller;
    if rotation == FIRST_TRIAL {
        return TrialOutcome::Reset {
            stalled: None,
            kind: ResetKind::PowerButton,
            at: FAULT_AT,
        };
    }
    TrialOutcome::Pass
}

fn no_work_outcome(spec: &TrialSpec, offsets: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { purpose, .. } = spec.controller;
    if purpose == Purpose::Isolated
        && spec.regime == Regime::R1
        && let Some(&core) = spec.cores.first()
        && offsets[core.index()].get() == -5
    {
        return TrialOutcome::Reset {
            stalled: Some(core),
            kind: ResetKind::SyncFlood,
            at: FAULT_AT,
        };
    }
    TrialOutcome::Pass
}

fn profile_and_placement(events: &[Event]) -> bool {
    let Some(day) = events.iter().find_map(|event| {
        if let Payload::DayApplied(day) = &event.payload
            && matches!(day.cause, ApplyCause::Step { .. })
        {
            Some(day)
        } else {
            None
        }
    }) else {
        return false;
    };
    if day.transactions.is_empty() || !day.readbacks.iter().any(|(_, offset)| offset.0 < 0) {
        return false;
    }
    let intent = events.iter().find_map(|event| {
        if let Payload::TrialIntent(intent) = &event.payload
            && StagedDayGoal::Filler.needs(intent)
        {
            Some(intent)
        } else {
            None
        }
    });
    intent.is_some_and(|intent| {
        intent.placement == Some(Placement::Exact)
            && intent.profile.len() == day.readbacks.len()
            && intent.profile.iter().all(|(core, offset)| day.readbacks.get(core) == Some(offset.observed()))
            && events.iter().any(|event| matches!(&event.payload, Payload::TrialStart(start)
                if start.trial == intent.trial && start.controller.as_ref().is_some_and(|controller|
                    !start.cpus.is_empty() && start.cpus.iter().all(|cpu| controller.tested_cpus.contains(cpu)))))
    })
}

fn resumed_after(events: &[Event], reset: ResetKind) -> Result<(), Error> {
    let after = events.iter().skip_while(|event| {
        !matches!(&event.payload, Payload::CrashDetected(crash)
        if crash.in_flight.is_some() && crash.reset_reason == Some(reset))
    });
    require(after.skip(1).any(|event| matches!(&event.payload, Payload::TrialEnd(end) if end.outcome == Outcome::Pass)),
        "a real trial that passed after resuming the scripted in-flight reset")
}

fn shutdown(events: &[Event], reason: ShutdownReason) -> Result<(), Error> {
    require(
        events.last().is_some_and(
            |event| matches!(&event.payload, Payload::Shutdown(end) if end.reason == reason),
        ),
        "the expected clean shutdown",
    )
}

fn at_stock(sim: &Simulator) -> Result<(), Error> {
    let mut machine = sim.seams();
    for info in machine.host.topology()? {
        require(
            machine.smu.offset(info.core)? == TargetOffset::ZERO.observed(),
            "all core registers restored to stock",
        )?;
    }
    Ok(())
}

fn require(ok: bool, milestone: &'static str) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(Error::Milestone(milestone))
    }
}
