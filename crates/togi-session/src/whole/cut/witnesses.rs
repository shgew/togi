//! Selected recovery boundaries that remain in the ordinary test loop.

use test_case::test_case;
use togi_sim::definition::ScriptDef;
use togi_sim::faults::{HookPhase, SmuOp};

use super::*;

fn small_definition() -> MachineDefinition {
    MachineDefinition {
        cores: 4,
        core: (0..4).map(|id| limits(id, -50)).collect(),
        ..MachineDefinition::default()
    }
}

#[test_case(Fate::Killed; "same boot killed process")]
#[test_case(Fate::Crashed; "new boot crash")]
fn an_interrupted_failing_trial_is_committed_once(fate: Fate) {
    // Discover the first isolated nonzero trial through the public run, then pin only its outcome.
    let definition = small_definition();
    let baseline = Bench::defined(&definition);
    let (_, events) = play(&baseline, Phase::Run { trials: 120 }, 0, None);
    let (trial, core) = events.iter().find_map(|event| {
        if let Payload::TrialIntent(intent) = &event.payload
            && matches!(intent.tested, Some(Tested::Isolated { offset, .. }) if offset.get() < 0)
            && intent.core.is_some()
        {
            Some((intent.trial.clone(), intent.core.unwrap()))
        } else {
            None
        }
    }).expect("the bounded run reaches isolated mapping");
    let scripted = Bench::defined(&MachineDefinition {
        script: vec![ScriptDef {
            trial: trial.to_string(),
            signal: Some(Signal::ComputationError),
            at_s: 2.0,
            core: i64::from(core.get()),
            ..ScriptDef::default()
        }],
        ..definition
    });
    let mut phases = Vec::new();
    step(&scripted, &mut phases, Phase::Run { trials: 120 });
    let phase = &phases[0];
    let after = phase
        .events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::TrialProgress(progress)
            if progress.trial == trial && progress.signal == Some(Signal::ComputationError))
        })
        .expect("the failing trial records its signal before its end")
        + 1;
    let resumed = phase.before.bench(&scripted.sim.model());
    let (_, events) = play(&resumed, phase.phase, 0, Some((after, fate)));
    let events = drain_failure(&resumed, phase.phase, events);
    assert_eq!(
        &events[..after],
        &phase.events[..after],
        "{fate:?}: retained prefix"
    );
    let intent = events
        .iter()
        .find_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload
                && intent.trial == trial
            {
                Some(intent)
            } else {
                None
            }
        })
        .unwrap();
    let ends: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload
                && end.trial == trial
            {
                Some(end)
            } else {
                None
            }
        })
        .collect();
    let failures: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::Failure(failure) = &event.payload
                && failure.trial.as_ref() == Some(&trial)
            {
                Some(failure)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ends.len(), 1, "{fate:?}: {trial}");
    assert_eq!(failures.len(), 1, "{fate:?}: {trial}");
    assert_eq!(ends[0].outcome, Outcome::Failure, "{fate:?}: {trial}");
    assert_eq!(ends[0].core, intent.core, "{fate:?}: {trial}");
    assert_eq!(
        ends[0].signal,
        Some(Signal::ComputationError),
        "{fate:?}: {trial}"
    );
    let failure = failures[0];
    assert_eq!(
        failure.condition,
        Some(intent.condition),
        "{fate:?}: {trial}"
    );
    assert_eq!(
        failure.profile.as_ref(),
        Some(&intent.profile),
        "{fate:?}: {trial}"
    );
    assert_eq!(failure.core, intent.core, "{fate:?}: {trial}");
    assert_eq!(
        failure.signal,
        Signal::ComputationError,
        "{fate:?}: {trial}"
    );
    assert_eq!(
        failure.attribution,
        Attribution::Attributed,
        "{fate:?}: {trial}"
    );
    assert!(ends[0].interrupted, "{fate:?}: {trial}");
    let Some(Tested::Isolated { offset, .. }) = intent.tested else {
        panic!("{fate:?}: {trial} is not an isolated mapping trial");
    };
    assert_eq!(failure.offset, Some(offset), "{fate:?}: {trial}");
    assert_eq!(
        togi_journal::State::replay(&events).unwrap().open.len(),
        0,
        "{fate:?}: {trial}"
    );
    assert_settled(&resumed, phase.phase, &events);
}

#[test]
fn a_between_trial_power_cut_preserves_commitments_with_matched_downtime() {
    let bench = Bench::defined(&small_definition());
    let mut phases = Vec::new();
    step(&bench, &mut phases, Phase::Run { trials: 120 });
    let phase = &phases[0];
    let trial = phase.events.iter().find_map(|event| {
        if let Payload::TrialIntent(intent) = &event.payload
            && matches!(intent.tested, Some(Tested::Isolated { offset, .. }) if offset.get() < 0)
        {
            Some(&intent.trial)
        } else {
            None
        }
    }).expect("the bounded run reaches isolated mapping");
    let after = phase
        .events
        .iter()
        .position(|event| matches!(&event.payload, Payload::TrialEnd(end) if &end.trial == trial))
        .expect("the mapping trial ends")
        + 1;
    assert!(
        phase.events[after..]
            .iter()
            .any(|event| matches!(event.payload, Payload::CcdCap(_))),
        "the cut must precede a policy cap commitment"
    );
    let events = resume(phase, &bench.sim.model(), after, Fate::PowerLoss);
    assert_unchanged_decisions(
        phase,
        &bench.sim.model(),
        &events,
        after,
        &phase.events[after - 1].boot,
        None,
    );
}

#[test]
fn a_day_smu_power_cut_resumes_with_the_nonzero_applied_profile() {
    let bench = bench();
    let phases = reference(&bench, &SCENARIO[..4]);
    let phase = phases
        .iter()
        .rev()
        .find(|phase| {
            matches!(phase.phase, Phase::Day { .. })
                && phase
                    .accesses
                    .iter()
                    .any(|access| access.op == SmuOp::Set && access.offset < 0)
        })
        .expect("the qualified day applies a nonzero profile");
    // The last nonzero read follows durable day.applied, before the observer records changed offsets.
    let at = phase
        .accesses
        .iter()
        .rposition(|access| {
            access.op == SmuOp::Read && access.phase == HookPhase::After && access.offset < 0
        })
        .unwrap()
        + 1;
    let events = resume_at_smu(phase, &bench.sim.model(), at);
    let applied = folded(&events).project().applied.unwrap().to_ccd().unwrap();
    let cores = bench.sim.seams().host.topology().unwrap();
    assert!(
        applied
            .expand(&cores)
            .unwrap()
            .iter()
            .any(|(_, offset)| offset.get() < 0),
        "the resumed day must retain a nonzero applied profile"
    );
}

#[test_case(Fate::PowerLoss, togi_journal::EventKind::DeadEnd; "crash after dead end")]
#[test_case(Fate::PowerLoss, togi_journal::EventKind::SmuIntent; "crash after zero intent")]
#[test_case(Fate::PowerLoss, togi_journal::EventKind::SmuWrite; "crash after zero write")]
#[test_case(Fate::PowerLoss, togi_journal::EventKind::ProfileRestored; "crash after restoration")]
#[test_case(Fate::PowerLoss, togi_journal::EventKind::Shutdown; "crash after completed stop")]
#[test_case(Fate::Killed, togi_journal::EventKind::SmuIntent; "kill after zero intent")]
#[test_case(Fate::Killed, togi_journal::EventKind::Shutdown; "kill after completed stop")]
fn an_interrupted_no_nonzero_work_stop_is_recovered(fate: Fate, boundary: togi_journal::EventKind) {
    let bench = Bench::defined(&MachineDefinition {
        core: (0..4).map(|id| limits(id, 0)).collect(),
        ..small_definition()
    });
    let mut phases = Vec::new();
    step(&bench, &mut phases, Phase::Run { trials: 120 });
    let phase = &phases[0];
    let dead_end = phase
        .events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::DeadEnd(end) if end.detail.starts_with("no_nonzero_work:"))
        })
        .expect("mapping reaches a no-nonzero-work stop");
    let after = phase.events[dead_end..]
        .iter()
        .position(|event| event.kind() == boundary)
        .expect("the stop records its cleanup boundary")
        + dead_end
        + 1;
    let events = resume(phase, &bench.sim.model(), after, fate);
    assert_eq!(&events[..after], &phase.events[..after]);
    if matches!(fate, Fate::Killed) {
        assert_eq!(projection(&events), projection(&phase.events));
    } else {
        assert_unchanged_decisions(
            phase,
            &bench.sim.model(),
            &events,
            after,
            &events[after - 1].boot,
            None,
        );
    }
}

/// A short tuning history with a known reset reported only by a day boot, followed by a fresh tuning boot.
fn tuning_after_observed_days() -> (Model, Played) {
    let bench = Bench::defined(&small_definition());
    let mut phases = Vec::new();
    step(&bench, &mut phases, Phase::Run { trials: 1 });
    step(&bench, &mut phases, Phase::Boot);
    step(&bench, &mut phases, Phase::Crash { hours: 1 });
    step(&bench, &mut phases, Phase::Day { hours: 1 });
    step(&bench, &mut phases, Phase::Boot);
    step(&bench, &mut phases, Phase::Run { trials: 1 });
    let phase = phases.pop().unwrap();
    assert!(matches!(phase.own()[0].payload, Payload::StateRebuilt(_)));
    assert!(matches!(phase.own()[1].payload, Payload::ConfigLoaded(_)));
    (bench.sim.model(), phase)
}

#[test]
fn a_power_cut_after_state_rebuilt_before_config_loaded_is_detected() {
    let (model, phase) = tuning_after_observed_days();
    resume(&phase, &model, phase.start + 1, Fate::PowerLoss);
}

#[test]
fn a_day_only_reset_report_does_not_make_a_crashed_trial_inconclusive() {
    let (model, phase) = tuning_after_observed_days();
    let (after, trial) = phase
        .events
        .iter()
        .enumerate()
        .skip(phase.start)
        .find_map(|(at, event)| {
            if let Payload::TrialStart(start) = &event.payload {
                Some((at + 1, start.trial.clone()))
            } else {
                None
            }
        })
        .unwrap();
    let bench = phase.before.bench(&model);
    let (_, events) = play(
        &bench,
        phase.phase,
        phase.start,
        Some((after, Fate::PowerLoss)),
    );
    assert_eq!(&events[..after], &phase.events[..after]);
    let detected = events[after..]
        .iter()
        .find(|event| {
            matches!(&event.payload, Payload::CrashDetected(crash)
            if crash.previous_boot == phase.events[after - 1].boot)
        })
        .unwrap();
    let Payload::CrashDetected(crash) = &detected.payload else {
        unreachable!();
    };
    assert_eq!(crash.reset_reason, None);
    assert!(!crash.inconclusive);
    let ends: Vec<_> = events[after..]
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload
                && end.trial == trial
            {
                Some((event, end))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ends.len(), 1);
    let (event, end) = ends[0];
    assert_eq!(end.outcome, Outcome::Failure);
    assert_eq!(end.signal, Some(Signal::Crash));
    assert!(!end.interrupted);
    assert_eq!(end.duration_s, 0);
    assert!(event.cause.contains(&detected.seq));
    assert_settled(&bench, phase.phase, &events);
}
