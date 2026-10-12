use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{
    CorePhase, DeadEndCondition, Outcome, Payload, ProfileRestored, Shutdown, ShutdownReason,
    TrialId,
};
use togi_machine::{Condition, ObservedOffset, Regime};
use togi_tuner::{Action, PhaseNo};

use super::kit::*;
use crate::history::{LOG_LIMIT, LogEntry};
use crate::model::{CoreState, FoldError, Model, project};
use crate::words::Tag;

fn searching() -> Script {
    Script::new(
        &[(0, 0), (1, 0), (2, 1)],
        &[Start::search(-25), Start::search(-5), Start::search(-5)],
    )
}

fn at_limit() -> Script {
    Script::new(&[(0, 0), (1, 0), (2, 1)], &[Start::at_limit(-20, -21); 3])
}

fn shutdown(reason: ShutdownReason) -> Payload {
    Shutdown {
        reason,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    }
    .into()
}

fn restored() -> Payload {
    ProfileRestored {
        offsets: offsets(&[0, 0, 0]),
    }
    .into()
}

#[test]
fn empty_model_has_no_session() {
    let model = Model::new();
    let snap = model.snapshot();
    assert!(model.problem().is_none());
    assert!(snap.session.is_none());
    assert!(snap.cores.is_empty());
    assert!(snap.history.is_empty());
    assert!(snap.trial.is_none() && snap.cycle.is_none() && snap.hunt.is_none());
    assert_eq!((snap.log.len(), snap.log_total, snap.failures), (0, 0, 0));
    assert!(!snap.resting(), "nothing rests before a session starts");
}

#[test]
fn a_refused_event_turns_the_model_into_a_problem() {
    let mut script = at_limit();
    let mut model = script.model();
    assert!(model.snapshot().session.is_some());
    let event = script.event(
        togi_journal::TunerDecision {
            core: core(9),
            activity: togi_journal::Activity::Search,
            decision: togi_journal::Decision::StepDeeper,
            from_offset: off(0),
            to_offset: off(-5),
            pass: None,
            failure_point: None,
            workloads: Vec::new(),
            reason: String::new(),
        }
        .into(),
        &[],
    );
    let error = model.fold(&event).unwrap_err();
    assert!(matches!(
        error,
        FoldError::Journal(togi_journal::FoldError::UnknownCore { .. })
    ));
    assert_eq!(model.problem(), Some(&error));
    let snap = model.snapshot();
    assert!(snap.session.is_none() && snap.cores.is_empty() && snap.history.is_empty());
    assert_eq!(snap.log_total, 0, "no half-applied fact is shown");
    script.append(warning("later"), &[]);
    let later = script.events.last().unwrap();
    assert_eq!(
        model.fold(later),
        Err(error),
        "a problem state stays a problem"
    );
}

#[test]
fn an_event_that_does_not_follow_is_a_problem() {
    let script = searching();
    let mut model = Model::new();
    assert!(matches!(
        model.fold(&script.events[1]),
        Err(FoldError::Journal(togi_journal::FoldError::Gap { .. }))
    ));
    assert!(model.snapshot().session.is_none());
}

#[test]
fn project_folds_like_fold() {
    let script = searching();
    let projected = project(&script.events);
    assert!(projected.problem().is_none());
    assert_eq!(projected.snapshot().cores.len(), 3);
    let broken = project(script.events.iter().skip(1));
    assert!(broken.problem().is_some());
}

#[test]
fn trial_lifecycle() {
    let mut script = searching();
    let plan = script.plan();
    let (intent, trial) = script.intent(&plan);
    script.append(smu_intent(0, -25), &[]);
    script.append(readback(0, -25), &[]);
    let started = script.start(&trial);
    let model = script.model();
    let snap = model.snapshot();
    let running = snap.trial.expect("the open trial");
    assert_eq!(running.id, trial);
    assert_eq!(running.started, Some(at(started)));
    assert_eq!(
        running.duration,
        Duration::from_secs(u64::from(plan.duration.get())),
        "an SMU write after the intent does not hide the running trial"
    );
    let first = &snap.cores[0];
    assert!(first.loaded);
    assert_eq!(
        (first.applied, first.profile),
        (ObservedOffset(-25), off(-25))
    );
    let waiting = &snap.cores[1];
    assert!(!waiting.loaded && waiting.state != CoreState::Parked);
    assert!(!snap.cores[1].judged);
    script.end(intent, &trial, Outcome::Pass);
    let model = script.model();
    let snap = model.snapshot();
    assert!(snap.trial.is_none() && !snap.cores[0].loaded);
    assert!(
        snap.history.iter().all(|entry| entry.tag != Tag::Pass),
        "a search pass does not crowd the account of what happened"
    );
}

#[test]
fn trial_preparation_does_not_claim_load() {
    let mut script = searching();
    let plan = script.plan();
    script.intent(&plan);
    script.append(readback(0, -25), &[]);
    let model = script.model();
    let snap = model.snapshot();
    let trial = snap.trial.unwrap();
    assert!(trial.started.is_none());
    let first = &snap.cores[0];
    assert!(!first.loaded && !first.judged);
    assert_eq!(first.applied, ObservedOffset(-25));
}

#[test]
fn trial_start_selection() {
    enum Start {
        Before,
        Latest,
        Unrelated,
        Epoch,
    }
    for (name, kind) in [
        ("before intent", Start::Before),
        ("latest matching start", Start::Latest),
        ("only unrelated start", Start::Unrelated),
        ("epoch timestamp", Start::Epoch),
    ] {
        let mut script = searching();
        let id = TrialId::from("0001".to_owned());
        let other = TrialId::from("other".to_owned());
        let mut want = None;
        if matches!(kind, Start::Before) {
            want = Some(at(script.start(&id)));
        }
        let plan = script.plan();
        script.intent(&plan);
        match kind {
            Start::Before => {}
            Start::Latest => {
                script.start(&id);
                want = Some(at(script.start(&id)));
                script.start(&other);
            }
            Start::Unrelated => {
                script.start(&other);
            }
            Start::Epoch => {
                let mut event = script.event(
                    togi_journal::TrialStart {
                        trial: id.clone(),
                        window_start_ns: None,
                        scope: String::new(),
                        pid: 1,
                        cpus: Vec::new(),
                        argv: Vec::new(),
                        files: Vec::new(),
                        instances: Vec::new(),
                    }
                    .into(),
                    &[],
                );
                event.time = Timestamp::UNIX_EPOCH;
                script.shadow.fold(&event).unwrap();
                script.events.push(event);
                want = Some(Timestamp::UNIX_EPOCH);
            }
        }
        let model = script.model();
        let trial = model.snapshot().trial.expect("the open trial is projected");
        assert_eq!(trial.started, want, "{name}");
    }
}

#[test]
fn stopped_shows_the_saved_profile_not_the_applied_offsets() {
    let mut script = at_limit();
    script.append(readback(0, -20), &[]);
    script.append(readback(0, 0), &[]);
    script.append(restored(), &[]);
    script.append(shutdown(ShutdownReason::Signal), &[]);
    let model = script.model();
    let snap = model.snapshot();
    let stopped = snap.stopped.expect("stopped");
    assert!(stopped.saved);
    assert_eq!(snap.cores[0].applied, ObservedOffset(0));
    assert_eq!(snap.cores[0].profile, off(-20));
    assert!(snap.resting());
}

#[test]
fn write_after_restoration_is_not_saved() {
    let mut script = at_limit();
    script.append(restored(), &[]);
    let mut cycles = shutdown(ShutdownReason::Cycles);
    if let Payload::Shutdown(shutdown) = &mut cycles {
        shutdown.cycles = Some(nz(1));
    }
    script.append(cycles, &[]);
    assert!(script.model().snapshot().stopped.unwrap().saved);
    script.append(smu_intent(0, -25), &[]);
    script.append(readback(0, 0), &[]);
    script.append(
        togi_journal::DeadEnd {
            condition: DeadEndCondition::Smu,
            core: None,
            detail: "core 00 read back 0 instead of -25".to_owned(),
            action: None,
        }
        .into(),
        &[],
    );
    script.append(shutdown(ShutdownReason::DeadEnd), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert!(!snap.stopped.unwrap().saved);
    assert_eq!(
        snap.dead_end.unwrap().detail,
        "core 00 read back 0 instead of -25"
    );
}

#[test]
fn smu_dead_end_keeps_the_readback() {
    let mut script = searching();
    script.append(smu_intent(0, -25), &[]);
    script.append(readback(0, 0), &[]);
    script.append(
        togi_journal::DeadEnd {
            condition: DeadEndCondition::Smu,
            core: None,
            detail: "core 00 read back 0 instead of -25".to_owned(),
            action: None,
        }
        .into(),
        &[],
    );
    script.append(shutdown(ShutdownReason::DeadEnd), &[]);
    let model = script.model();
    let first = &model.snapshot().cores[0];
    assert_eq!(
        (first.applied, first.profile),
        (ObservedOffset(0), off(-25))
    );
}

#[test]
fn sparse_core_ids() {
    let mut script = Script::new(&[(0, 0), (8, 1)], &[Start::search(-5), Start::search(-9)]);
    script.append(
        ProfileRestored {
            offsets: [(core(0), off(-5)), (core(8), off(-9))]
                .into_iter()
                .collect(),
        }
        .into(),
        &[],
    );
    let model = script.model();
    let snap = model.snapshot();
    let ids: Vec<u8> = snap.cores.iter().map(|c| c.id.get()).collect();
    assert_eq!(ids, [0, 8]);
    assert_eq!(snap.cores[1].applied, ObservedOffset(-9));
    assert_eq!(snap.order, [core(0), core(8)]);
}

#[test]
fn crash_classification() {
    for (name, stray, inconclusive) in [
        ("offset-related crash", false, false),
        ("power reset", false, true),
        ("before offsets", true, false),
    ] {
        let mut script = searching();
        let mut crashed = crash("boot", None);
        crashed.stray = stray;
        crashed.inconclusive = inconclusive;
        script.append(crashed.into(), &[]);
        let model = script.model();
        let snap = model.snapshot();
        assert_eq!(snap.crashes, 1, "{name}");
        assert!(snap.recover.is_some(), "{name}");
    }
}

#[test]
fn a_crash_ends_only_its_boot_trial() {
    for previous in ["boot", "older"] {
        let mut script = searching();
        let plan = script.plan();
        let (intent, trial) = script.intent(&plan);
        script.start(&trial);
        script.boot = "next".to_owned();
        script.append(crash(previous, Some(intent)).into(), &[]);
        let model = script.model();
        let snap = model.snapshot();
        let running = previous != "boot";
        assert_eq!(snap.trial.is_some(), running, "boot {previous}");
        assert_eq!(snap.cores[0].loaded, running, "boot {previous}");
        if running {
            assert!(snap.crashed_trial(), "a later boot outlived the trial");
            assert!(snap.resting());
        } else {
            assert!(snap.recover.unwrap().trial.is_some());
        }
    }
}

#[test]
fn recovery_ends_only_when_a_trial_starts() {
    let mut script = searching();
    script.boot = "next".to_owned();
    let mut crashed = crash("boot", None);
    crashed.stray = true;
    script.append(crashed.into(), &[]);
    let plan = script.plan();
    let (_, trial) = script.intent(&plan);
    assert!(
        script.model().snapshot().recover.is_some(),
        "preparing offsets does not hide recovery"
    );
    script.start(&trial);
    assert!(script.model().snapshot().recover.is_none());
}

#[test]
fn a_reset_core_drops_its_solo_limit() {
    let mut script = Script::new(&[(0, 0)], &[Start::search(0)]);
    let mut exit = Start::at_limit(-20, -21);
    exit.from = Some(CorePhase::Search);
    exit.pass = Some(-20);
    script.append(transition(0, exit).into(), &[]);
    assert_eq!(script.model().snapshot().cores[0].solo, Some(off(-20)));
    let mut again = Start::search(0);
    again.from = Some(CorePhase::AtLimit);
    script.append(transition(0, again).into(), &[]);
    assert_eq!(script.model().snapshot().cores[0].solo, None);
}

#[test]
fn solo_limit_is_the_offset_the_core_passed_alone() {
    let mut exit = Start::solo(-19, -20);
    exit.fail = Some(-21);
    let mut bare = Start::has_room(-9);
    bare.from = Some(CorePhase::Search);
    let script = Script::new(&[(0, 0), (1, 0)], &[exit, bare]);
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(
        snap.cores[0].solo,
        Some(off(-20)),
        "the margin -19 is not the limit"
    );
    assert_eq!(
        snap.cores[1].solo, None,
        "the tuner counts no solo limit without a pass"
    );
}

#[test]
fn phase_and_bios_before_phase_one_ends() {
    let script = Script::new(&[(0, 0), (1, 0), (2, 1)], &[Start::solo(-9, -10); 3]);
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(snap.phases.unwrap().phase, PhaseNo::One);
    assert!(snap.bios.is_none());
}

#[test]
fn failure_counters_separate_observed_and_skipped() {
    let mut h = Hist::three();
    h.push(transition(0, Start::has_room(-20)).into());
    let mut observed = failure(togi_machine::Signal::ComputationError, None);
    observed.attribution = togi_journal::Attribution::Attributed;
    observed.core = Some(core(0));
    observed.offset = Some(off(-20));
    let first = h.push(observed.clone().into());
    observed.known_failure = Some(seq(3));
    h.push(observed.into());
    assert_eq!(h.facts.failures, 2);
    assert_eq!(
        h.facts.last_failure,
        Some(Timestamp::from_second(1000 + i64::try_from(first.get()).unwrap()).unwrap())
    );
}

#[test]
fn the_log_counts_every_listed_event() {
    let mut script = searching();
    for i in 0..LOG_LIMIT + 30 {
        script.append(warning(&i.to_string()), &[]);
    }
    let listed = script.events.len();
    for _ in 0..90 {
        script.append(smu_intent(0, -20), &[]);
        script.append(preflight(), &[]);
    }
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!((snap.log_total, snap.log.len()), (listed, LOG_LIMIT));
    let log = snap.log();
    assert_eq!(
        (log.total, log.arrived, log.entries.len()),
        (listed, 0, LOG_LIMIT)
    );
}

#[test]
fn the_raw_log_keeps_the_newest_eligible_events_in_order() {
    for count in [
        0,
        LOG_LIMIT - 1,
        LOG_LIMIT,
        LOG_LIMIT + 1,
        3 * LOG_LIMIT + 5,
    ] {
        let mut script = searching();
        for i in 0..count {
            script.append(smu_intent(0, -20), &[]);
            script.append(readback(0, -20), &[]);
            script.append(preflight(), &[]);
            script.append(warning(&format!("{i}\u{1b}[31m")), &[]);
        }
        let mut want: Vec<LogEntry> = script
            .events
            .iter()
            .filter(|e| {
                !matches!(
                    e.payload,
                    Payload::SmuIntent(_)
                        | Payload::SmuWrite(_)
                        | Payload::SmuReadback(_)
                        | Payload::PreflightCheck(_)
                )
            })
            .map(|e| LogEntry {
                seq: e.seq,
                at: e.time,
                tag: crate::words::tag_word(&e.payload),
                msg: e.msg.clone(),
            })
            .collect();
        let total = want.len();
        want.drain(..want.len().saturating_sub(LOG_LIMIT));
        let model = script.model();
        let snap = model.snapshot();
        assert_eq!(
            snap.log, want,
            "{count}: the newest eligible events, raw, in journal order"
        );
        assert_eq!(snap.log_total, total);
    }
}

#[test]
fn history_counts_every_entry_it_leaves_out() {
    let mut script = searching();
    let entries = 2 * crate::history::HISTORY_LIMIT + 6;
    for i in 0..entries - 2 {
        script.append(warning(&i.to_string()), &[]);
    }
    let model = script.model();
    let snap = model.snapshot();
    let shown = snap.history.len();
    assert_eq!(shown, crate::history::HISTORY_LIMIT);
    assert_eq!(snap.history_dropped + shown, entries - 1);
}

#[test]
fn a_cycle_opens_with_its_steps() {
    let mut script = Script::at_limit();
    script.step(|_| Outcome::Pass);
    script.step(|_| Outcome::Pass);
    let model = script.model();
    let snap = model.snapshot();
    let cycle = snap.cycle.expect("the cycle opened");
    assert_eq!(cycle.plan.number.map(std::num::NonZeroU32::get), Some(1));
    assert_eq!(cycle.step_hunts.len(), cycle.plan.steps.len());
    assert!(script.shadow.next().is_ok());
    let _ = (Action::ReadRanking, Condition::Alone, Regime::R1);
}

#[test]
fn between_trials_the_last_outcome_and_the_forecast_stand() {
    let mut script = Script::at_limit();
    assert!(
        script.model().snapshot().last.is_none(),
        "no trial has ended yet"
    );
    let plan = script.next_trial();
    let (intent, trial) = script.intent(&plan);
    script.end(intent, &trial, Outcome::Pass);
    let model = script.model();
    let snap = model.snapshot();
    let last = snap.last.expect("the last trial");
    assert_eq!(last.id, trial);
    assert_eq!(last.outcome, Outcome::Pass);
    assert_eq!(last.regime, Some(plan.regime));
    assert_eq!(last.cores, plan.load.cores());
    assert_eq!(
        last.planned,
        Some(Duration::from_secs(u64::from(plan.duration.get())))
    );
    assert!(snap.trial.is_none());
    assert!(snap.forecast.branches.is_empty());
    assert!(
        snap.forecast.between.is_some(),
        "what runs next is forecast between trials"
    );
}

#[test]
fn a_recovered_trial_keeps_its_place_in_the_cycle() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (intent, trial) = script.intent(&plan);
    script.start(&trial);
    script.boot = "next".to_owned();
    script.append(crash("boot", Some(intent)).into(), &[]);
    let model = script.model();
    let recovered = model
        .snapshot()
        .recover
        .and_then(|r| r.trial.as_ref())
        .expect("the crashed trial");
    let place = recovered.cycle.expect("a cycle trial");
    assert_eq!((place.cycle, place.step, place.part), (1, Some(1), Some(1)));
}

#[test]
fn a_prior_recovery_does_not_mask_a_later_boot_crash() {
    let mut script = searching();
    script.boot = "next".to_owned();
    let mut stray = crash("boot", None);
    stray.stray = true;
    script.append(stray.into(), &[]);
    let plan = script.plan();
    let (_, trial) = script.intent(&plan);
    script.start(&trial);
    assert!(script.model().snapshot().recover.is_none());
    script.boot = "later".to_owned();
    script.append(warning("a later boot wrote this"), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert!(snap.crashed_trial(), "the trial's boot was outlived");
    assert!(snap.recover.is_none());
    assert!(snap.resting());
}
