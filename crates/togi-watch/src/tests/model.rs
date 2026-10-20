use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{
    Activity, CorePhase, DeadEndCondition, Outcome, Payload, ProfileRestored, Shutdown,
    ShutdownReason, TrialId,
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
    let FoldError::Journal(expected @ togi_journal::FoldError::UnknownCore { .. }) = &error else {
        panic!("not the refused journal event: {error:?}");
    };
    assert!(matches!(model.problem(), Some(FoldError::Journal(kept)) if kept == expected));
    let snap = model.snapshot();
    assert!(snap.session.is_none() && snap.cores.is_empty() && snap.history.is_empty());
    assert_eq!(snap.log_total, 0, "no half-applied fact is shown");
    script.append(warning("later"), &[]);
    let later = script.events.last().unwrap();
    assert!(
        matches!(model.fold(later), Err(FoldError::Journal(kept)) if &kept == expected),
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
    let broken = project(&script.events[1..]);
    assert!(broken.problem().is_some());
}

#[test]
fn a_batch_without_trial_ends_matches_incremental_folding() {
    let script = searching();
    let mut batch = Model::new();
    batch.fold_batch(&script.events).unwrap();
    let each = script.model();
    assert_eq!(
        format!("{:?}", batch.snapshot()),
        format!("{:?}", each.snapshot())
    );
    assert_eq!(ended_of(&batch), None);
}

#[test]
fn a_refused_batch_reports_the_first_event_and_keeps_the_problem() {
    let script = searching();
    let mut model = Model::new();
    let error = model.fold_batch(&script.events[1..]).unwrap_err();
    assert_eq!(error.index, 0);
    assert!(matches!(
        (model.problem(), &error.source),
        (Some(FoldError::Journal(cached)), FoldError::Journal(returned)) if cached == returned
    ));
    assert!(matches!(
        error.source,
        FoldError::Journal(togi_journal::FoldError::Gap { .. })
    ));
    assert!(model.snapshot().session.is_none());
    let later = model.fold_batch(&script.events).unwrap_err();
    assert_eq!(later.index, 0);
    assert!(matches!(
        (model.problem(), &later.source),
        (Some(FoldError::Journal(cached)), FoldError::Journal(returned)) if cached == returned
    ));
    model.fold_batch(&[]).unwrap();
    assert!(matches!(
        (model.problem(), &later.source),
        (Some(FoldError::Journal(cached)), FoldError::Journal(returned)) if cached == returned
    ));
}

#[test]
fn a_batch_reports_the_failure_position_after_a_valid_prefix() {
    let mut script = searching();
    script.events[1] = script.events[0].clone();
    let mut model = Model::new();
    let error = model.fold_batch(&script.events).unwrap_err();
    assert_eq!(error.index, 1);
    assert!(matches!(
        (model.problem(), &error.source),
        (Some(FoldError::Journal(cached)), FoldError::Journal(returned)) if cached == returned
    ));
    assert!(model.snapshot().session.is_none());
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
        (Some(ObservedOffset(-25)), off(-25))
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
    assert_eq!(first.applied, Some(ObservedOffset(-25)));
}

#[derive(Clone, Copy)]
enum TrialStartSelection {
    Before,
    Latest,
    Unrelated,
    Epoch,
}

#[test_case::test_case(TrialStartSelection::Before; "before intent")]
#[test_case::test_case(TrialStartSelection::Latest; "latest matching start")]
#[test_case::test_case(TrialStartSelection::Unrelated; "only unrelated start")]
#[test_case::test_case(TrialStartSelection::Epoch; "epoch timestamp")]
fn trial_start_selection(kind: TrialStartSelection) {
    let mut script = searching();
    let id = TrialId::from("0001".to_owned());
    let other = TrialId::from("other".to_owned());
    let mut want = if matches!(kind, TrialStartSelection::Before) {
        Some(at(script.start(&id)))
    } else {
        None
    };
    let plan = script.plan();
    script.intent(&plan);
    match kind {
        TrialStartSelection::Before => {}
        TrialStartSelection::Latest => {
            script.start(&id);
            want = Some(at(script.start(&id)));
            script.start(&other);
        }
        TrialStartSelection::Unrelated => {
            script.start(&other);
        }
        TrialStartSelection::Epoch => {
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
                    roles: Vec::new(),
                    controller: None,
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
    assert_eq!(trial.started, want);
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
    assert_eq!(snap.cores[0].applied, Some(ObservedOffset(0)));
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
        (Some(ObservedOffset(0)), off(-25))
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
    assert_eq!(snap.cores[1].applied, Some(ObservedOffset(-9)));
    assert_eq!(snap.order, [core(0), core(8)]);
}

#[test]
fn sparse_readbacks_do_not_fill_other_topology_cores() {
    let mut script = Script::new(&[(0, 0), (8, 1)], &[Start::search(-5), Start::search(-9)]);
    script.append(
        ProfileRestored {
            offsets: [(core(8), off(-9))].into_iter().collect(),
        }
        .into(),
        &[],
    );
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(
        snap.cores
            .iter()
            .map(|core| (core.id, core.applied))
            .collect::<Vec<_>>(),
        [(core(0), None), (core(8), Some(ObservedOffset(-9)))]
    );
}

#[test_case::test_case(false, false; "offset related crash")]
#[test_case::test_case(false, true; "power reset")]
#[test_case::test_case(true, false; "before offsets")]
fn crash_classification(stray: bool, inconclusive: bool) {
    let mut script = searching();
    let mut crashed = crash("boot", None);
    crashed.stray = stray;
    crashed.inconclusive = inconclusive;
    script.append(crashed.into(), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(snap.crashes, 1);
    assert!(snap.recover.is_some());
}

#[test_case::test_case("boot"; "same boot")]
#[test_case::test_case("older"; "older boot")]
fn a_crash_ends_only_its_boot_trial(previous: &str) {
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

#[test_case::test_case(0; "zero")]
#[test_case::test_case(LOG_LIMIT - 1; "below limit")]
#[test_case::test_case(LOG_LIMIT; "at limit")]
#[test_case::test_case(LOG_LIMIT + 1; "above limit")]
#[test_case::test_case(3 * LOG_LIMIT + 5; "several limits")]
fn the_raw_log_keeps_the_newest_eligible_events_in_order(count: usize) {
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

/// What a frame between trials shows of the trial that just ended: its id and what each outcome led to.
fn ended_of(model: &Model) -> Option<(String, String)> {
    model
        .snapshot()
        .ended
        .map(|ended| (ended.trial.id.to_string(), format!("{:?}", ended.forecast)))
}

#[test]
fn only_the_last_end_of_a_batch_keeps_its_outcomes_and_a_batch_without_one_keeps_the_last() {
    let mut script = Script::at_limit();
    let mut ends = Vec::new();
    for outcome in [Outcome::Pass, Outcome::Failure] {
        let plan = script.next_trial();
        let (intent, trial) = script.intent(&plan);
        script.start(&trial);
        script.append(ended(&trial, &plan, outcome), &[intent]);
        ends.push(script.events.len());
    }
    let plan = script.next_trial();
    let next_decided = script.events.len();
    script.intent(&plan);
    let events = &script.events;
    let next_intent = events.len();
    assert!(
        next_decided > ends[1],
        "the tuner decides something between the last end and the next intent"
    );

    // Folding one event at a time keeps every end; the batch must show the same as the incremental fold at each
    // endpoint: just ended, the next intent, and with two ends in the history.
    for &shown in &[ends[0], ends[1], next_decided, next_intent] {
        let mut each = Model::new();
        for event in &events[..shown] {
            each.fold(event).unwrap();
        }
        let mut batch = Model::new();
        batch.fold_batch(&events[..shown]).unwrap();
        assert_eq!(ended_of(&batch), ended_of(&each), "{shown} events");
        assert_eq!(
            batch.snapshot().last.map(|last| &last.id),
            each.snapshot().last.map(|last| &last.id)
        );
        assert_eq!(
            batch.snapshot().history.len(),
            each.snapshot().history.len()
        );
    }
    assert!(
        ended_of(&project(&events[..ends[1]])).is_some(),
        "the last end keeps its outcomes"
    );

    // A batch holding no end leaves what the last end kept: the events decided after it keep the same outcomes.
    let kept = ended_of(&project(&events[..ends[1]]));
    let mut rest = project(&events[..ends[1]]);
    rest.fold_batch(&events[ends[1]..next_decided]).unwrap();
    assert!(
        ended_of(&rest).is_some(),
        "the end survives a batch without one"
    );
    assert_eq!(ended_of(&rest), kept);

    // The next intent ends the time between trials, so nothing is kept for it.
    rest.fold_batch(&events[next_decided..=next_decided])
        .unwrap();
    assert_eq!(ended_of(&rest), None);
}

#[test]
fn an_empty_batch_keeps_the_last_outcome_and_snapshot() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (intent, trial) = script.intent(&plan);
    script.end(intent, &trial, Outcome::Pass);
    let mut model = script.model();
    let kept = ended_of(&model);
    let before = format!("{:?}", model.snapshot());
    model.fold_batch(&[]).unwrap();
    assert!(kept.is_some());
    assert_eq!(ended_of(&model), kept);
    assert_eq!(format!("{:?}", model.snapshot()), before);
}

#[test]
fn a_batch_ending_in_shutdown_hides_the_retained_outcome() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (intent, trial) = script.intent(&plan);
    script.end(intent, &trial, Outcome::Pass);
    let after_end = script.events.len();
    script.append(shutdown(ShutdownReason::Signal), &[]);
    let mut batch = project(&script.events[..after_end]);
    assert!(ended_of(&batch).is_some());
    batch.fold_batch(&script.events[after_end..]).unwrap();
    let each = script.model();
    assert_eq!(ended_of(&batch), None);
    assert_eq!(
        batch.snapshot().stopped.map(|stop| stop.reason),
        Some(ShutdownReason::Signal)
    );
    assert_eq!(
        format!("{:?}", batch.snapshot()),
        format!("{:?}", each.snapshot())
    );
    assert_eq!(ended_of(&project(&script.events)), None);
}

#[test]
fn a_reset_batch_retains_the_outcome_without_reopening_a_stop() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (intent, trial) = script.intent(&plan);
    script.end(intent, &trial, Outcome::Pass);
    let after_end = script.events.len();
    let mut batch = project(&script.events);
    let kept = ended_of(&batch);
    let reset = script.append(
        togi_journal::CommandReset {
            target: togi_journal::ResetTarget::Core(core(1)),
        }
        .into(),
        &[],
    );
    batch.fold_batch(&script.events[after_end..]).unwrap();
    assert!(kept.is_some());
    assert_eq!(ended_of(&batch), kept);
    assert_eq!(batch.snapshot().cores[1].queued, Some(reset));
    let before_stop = script.events.len();
    script.append(shutdown(ShutdownReason::Signal), &[]);
    let after_stop = script.events.len();
    batch.fold_batch(&script.events[before_stop..]).unwrap();
    script.append(
        togi_journal::CommandReset {
            target: togi_journal::ResetTarget::Core(core(0)),
        }
        .into(),
        &[],
    );
    batch.fold_batch(&script.events[after_stop..]).unwrap();
    assert_eq!(ended_of(&batch), None);
    assert!(batch.snapshot().stopped.is_some());
    let each = script.model();
    assert_eq!(
        format!("{:?}", batch.snapshot()),
        format!("{:?}", each.snapshot())
    );
}

#[test]
fn a_recovered_trial_keeps_its_place_in_the_cycle() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (intent, trial) = script.intent(&plan);
    let started = script.start(&trial);
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
    assert_eq!(
        recovered.started,
        Some(at(started)),
        "the crashed trial had started"
    );
    assert!(!recovered.partial, "it loaded every core");
}

#[test]
fn a_prior_recovery_does_not_mask_a_later_boot_crash() {
    let mut script = searching();
    let first = script.plan();
    let (first_intent, first_trial) = script.intent(&first);
    script.start(&first_trial);
    script.boot = "next".to_owned();
    let crashed = script.append(crash("boot", Some(first_intent)).into(), &[]);
    script.append(
        end_payload(
            &first_trial,
            Outcome::Failure,
            Some(togi_machine::Signal::Crash),
        )
        .into(),
        &[crashed],
    );
    let second = script.next_trial();
    let (_, second_trial) = script.intent(&second);
    let model = script.model();
    let snap = model.snapshot();
    let trial = snap.trial.expect("the next trial is intended");
    assert_eq!(trial.id, second_trial);
    assert!(trial.started.is_none() && !trial.crashed);
    let recover = snap.recover.expect("the crash is being recovered");
    assert_eq!(recover.trial.as_ref().map(|t| &t.id), Some(&first_trial));
    assert_eq!(recover.end.as_ref().map(|end| &end.id), Some(&first_trial));

    script.boot = "later".to_owned();
    script.append(warning("a later boot wrote this"), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert!(
        snap.crashed_trial(),
        "the intended trial's boot was outlived"
    );
    assert!(snap.trial.is_some_and(|trial| trial.started.is_none()));
    assert!(
        snap.recover.is_none(),
        "the earlier recovery does not stand for this crash"
    );
    assert!(snap.resting());
}

#[test]
fn a_crash_ending_a_cycle_trial_says_which_trial_of_its_part_it_was() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (intent, trial) = script.intent(&plan);
    script.start(&trial);
    script.boot = "next".to_owned();
    let crashed = script.append(crash("boot", Some(intent)).into(), &[]);
    let mut over = end_payload(&trial, Outcome::Failure, Some(togi_machine::Signal::Crash));
    over.last_sample_s = Some(100);
    script.append(over.into(), &[crashed]);
    let model = script.model();
    let history = model.snapshot().history;
    let ended = history
        .iter()
        .find(|entry| entry.tag == crate::words::Tag::Crash)
        .expect("one crash line");
    assert!(
        ended.text().contains(" · rebooted · late in trial 1 of "),
        "{}",
        ended.text()
    );
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.tag == crate::words::Tag::Crash)
            .count(),
        1,
        "the trial's end takes the place of the reboot line"
    );
}

#[test]
fn only_an_idle_trial_holds_and_only_r7_names_requesters() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (_, trial) = script.intent(&plan);
    script.start(&trial);
    let model = script.model();
    let snap = model.snapshot();
    assert!(
        snap.idle_hold().is_none(),
        "an R7 trial is not an idle hold"
    );
    assert_eq!(snap.r7_workload(), Some(plan_workload(&snap)));
    let search = searching();
    let model = search.model();
    let snap = model.snapshot();
    assert_eq!(snap.r7_workload(), None);
    assert!(snap.r7_top().is_empty());
}

fn plan_workload(snap: &crate::model::Snapshot<'_>) -> togi_machine::WorkloadId {
    snap.trial.expect("the running trial").workload
}

#[test]
fn a_reset_queued_for_a_core_shows_on_that_core() {
    let mut script = searching();
    let seq = script.append(
        Payload::CommandReset(togi_journal::CommandReset {
            target: togi_journal::ResetTarget::Core(core(1)),
        }),
        &[],
    );
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(snap.cores[1].queued, Some(seq));
    assert_eq!(snap.cores[0].queued, None);
    assert_eq!(
        snap.history.first().map(|entry| entry.tag.word()),
        Some("reset")
    );
}

#[test]
fn only_a_new_config_loaded_reopens_a_stopped_session() {
    let mut script = searching();
    script.append(restored(), &[]);
    script.append(shutdown(ShutdownReason::Signal), &[]);
    let reset = script.append(
        Payload::CommandReset(togi_journal::CommandReset {
            target: togi_journal::ResetTarget::Core(core(1)),
        }),
        &[],
    );
    script.append(warning("stop-time error"), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(
        snap.stopped.map(|stop| stop.reason),
        Some(ShutdownReason::Signal)
    );
    assert!(snap.resting() && !snap.between_trials());
    assert_eq!(snap.cores[1].queued, Some(reset), "the reset stays queued");
    script.append(config_loaded(), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert!(snap.stopped.is_none(), "config.loaded reopens the session");
    assert_eq!(
        snap.cores[1].queued,
        Some(reset),
        "still queued for this run"
    );
}

fn ended(trial: &TrialId, plan: &togi_tuner::TrialPlan, outcome: Outcome) -> Payload {
    let signal = (outcome == Outcome::Failure).then_some(togi_machine::Signal::ComputationError);
    let mut end = end_payload(trial, outcome, signal);
    end.duration_s = plan.duration.get();
    if outcome == Outcome::Failure {
        end.core = plan.load.cores().iter().next();
    }
    end.into()
}

/// The step of the next cycle trial once every trial before it passes.
fn next_cycle_step(script: &Script) -> Option<u32> {
    let mut ahead = script.clone();
    for _ in 0..200 {
        match ahead.shadow.next().ok()? {
            togi_tuner::Action::RunTrial(plan) => {
                if let togi_tuner::TrialContext::Cycle { step, .. } = plan.requirement.context {
                    return Some(step.get().get());
                }
                let (intent, trial) = ahead.intent(&plan);
                ahead.append(ended(&trial, &plan, Outcome::Pass), &[intent]);
            }
            togi_tuner::Action::Decide(_) | togi_tuner::Action::ReadRanking => {
                ahead.step(|_| Outcome::Pass);
            }
        }
    }
    None
}

#[test]
fn a_running_rerun_names_the_step_checking_resumes_at() {
    let cores: Vec<(u8, u8)> = (0..8).map(|id| (id, id / 4)).collect();
    let mut script = Script::new(&cores, &[Start::has_room(-20); 8]);
    script.ranking();
    let (mut cycle_trials, mut checked, mut skipped) = (0, 0, 0);
    for _ in 0..400 {
        let Ok(action) = script.shadow.next() else {
            break;
        };
        let togi_tuner::Action::RunTrial(plan) = action else {
            script.step(|_| Outcome::Pass);
            continue;
        };
        let (intent, trial) = script.intent(&plan);
        let mut outcome = Outcome::Pass;
        match plan.requirement.context {
            togi_tuner::TrialContext::Cycle { .. } => {
                cycle_trials += 1;
                if cycle_trials % 3 == 0 {
                    outcome = Outcome::Failure;
                }
            }
            togi_tuner::TrialContext::Rerun { .. } => {
                script.start(&trial);
                let model = script.model();
                let snap = model.snapshot();
                let cycle = snap.cycle.expect("a rerun holds the cycle");
                let mut passed = script.clone();
                passed.append(ended(&trial, &plan, Outcome::Pass), &[intent]);
                if let Some(step) = next_cycle_step(&passed) {
                    checked += 1;
                    if step as usize != cycle.plan.current + 1 {
                        skipped += 1;
                    }
                    assert_eq!(
                        cycle.resume_step() + 1,
                        step as usize,
                        "rerun {trial}: the step named differs from the next cycle trial's"
                    );
                }
            }
            togi_tuner::TrialContext::Search { .. }
            | togi_tuner::TrialContext::SoloLimit { .. }
            | togi_tuner::TrialContext::ZeroRerun { .. }
            | togi_tuner::TrialContext::Round { .. }
            | togi_tuner::TrialContext::Hunt { .. } => {}
        }
        script.append(ended(&trial, &plan, outcome), &[intent]);
        if skipped > 0 {
            break;
        }
    }
    assert!(
        checked > 0 && skipped > 0,
        "the session must hold a rerun whose next cycle trial is past the plan's current step: {checked} reruns, {skipped} past it"
    );
}

#[test]
fn a_fold_after_a_snapshot_shows_the_new_event() {
    let mut script = searching();
    let mut model = script.model();
    let before = {
        let snap = model.snapshot();
        assert!(snap.trial.is_none());
        snap.log_total
    };
    let plan = script.plan();
    let (_, trial) = script.intent(&plan);
    model.fold(script.events.last().unwrap()).unwrap();
    let snap = model.snapshot();
    assert_eq!(snap.trial.map(|t| &t.id), Some(&trial), "the open trial");
    assert_eq!(snap.log_total, before + 1);
}

#[test]
fn the_session_does_what_its_last_intent_or_decision_did() {
    let mut h = Hist::three();
    assert_eq!(h.facts.session.as_ref().unwrap().activity, None);
    h.push(
        intent(
            "c1",
            Regime::R7,
            Condition::Together,
            &[-20, -30, -10],
            &[0, 1, 2],
        )
        .into(),
    );
    assert_eq!(
        h.facts.session.as_ref().unwrap().activity,
        Some(Activity::Checking)
    );
    h.push(
        togi_journal::TunerDecision {
            core: core(0),
            activity: Activity::Search,
            decision: togi_journal::Decision::StepDeeper,
            from_offset: off(-20),
            to_offset: off(-25),
            pass: None,
            failure_point: None,
            workloads: Vec::new(),
            reason: String::new(),
        }
        .into(),
    );
    assert_eq!(
        h.facts.session.as_ref().unwrap().activity,
        Some(Activity::Search)
    );
}

#[test]
fn an_event_the_tuner_refuses_is_a_problem() {
    let mut script = searching();
    let plan = script.plan();
    let (intent, trial) = script.intent(&plan);
    script.start(&trial);
    let mut model = script.model();
    let unsigned = script.event(
        end_payload(&trial, Outcome::Failure, None).into(),
        &[intent],
    );
    let error = model.fold(&unsigned).unwrap_err();
    assert!(
        matches!(
            error,
            FoldError::Tuner(togi_tuner::FoldError::MissingSignal)
        ),
        "{error:?}"
    );
    let snap = model.snapshot();
    assert!(snap.session.is_none() && snap.history.is_empty());
    assert_eq!(snap.log_total, 0);
    let later = script.event(warning("later"), &[]);
    assert!(matches!(
        model.fold(&later),
        Err(FoldError::Tuner(togi_tuner::FoldError::MissingSignal))
    ));
}

#[test]
fn a_reset_in_a_later_boot_leaves_the_crashed_trial_unrecovered() {
    let mut script = searching();
    let plan = script.plan();
    let (_, trial) = script.intent(&plan);
    script.start(&trial);
    script.boot = "reset-boot".to_owned();
    let reset = script.append(
        Payload::CommandReset(togi_journal::CommandReset {
            target: togi_journal::ResetTarget::Core(core(0)),
        }),
        &[],
    );
    script.append(shutdown(ShutdownReason::Command), &[reset]);
    let model = script.model();
    let snap = model.snapshot();
    let crashed = snap.trial.expect("the outlived trial");
    assert_eq!(crashed.id, trial);
    assert!(crashed.crashed && crashed.started.is_some());
    assert!(snap.recover.is_none(), "nothing recovered it");
    assert_eq!(
        snap.stopped.map(|stop| stop.reason),
        Some(ShutdownReason::Command)
    );
}

#[test]
fn a_trial_reporting_progress_past_its_planned_end_is_still_running() {
    let mut script = searching();
    let plan = script.plan();
    assert_ne!(plan.regime, Regime::R6);
    let (_, trial) = script.intent(&plan);
    let started = script.start(&trial);
    let planned_end = at(started)
        .checked_add(jiff::SignedDuration::from_secs(i64::from(
            plan.duration.get(),
        )))
        .unwrap();
    let mut progress = script.event(
        togi_journal::TrialProgress {
            trial: trial.clone(),
            detail: "still running".to_owned(),
            signal: None,
            core: None,
            fault: None,
            pid: None,
        }
        .into(),
        &[],
    );
    progress.time = planned_end
        .checked_add(jiff::SignedDuration::from_secs(10))
        .unwrap();
    script.shadow.fold(&progress).unwrap();
    script.events.push(progress);
    let model = script.model();
    let snap = model.snapshot();
    let running = snap.trial.expect("the overdue trial");
    assert_eq!(running.id, trial);
    assert!(!running.crashed && running.started.is_some());
    assert!(snap.recover.is_none() && snap.stopped.is_none());
    let now = planned_end
        .checked_add(jiff::SignedDuration::from_secs(42))
        .unwrap();
    assert_eq!(running.past_end(now), Duration::from_secs(42));
}

#[test]
fn a_core_nobody_read_back_has_no_observed_offset() {
    let model = searching().model();
    let snap = model.snapshot();
    assert_eq!(snap.cores[0].applied, None);
    assert_eq!(snap.cores[0].profile, off(-25));
}

#[test]
fn a_named_failure_at_zero_dead_end_is_the_cores_zero_constraint_and_its_evidence() {
    let mut script = at_limit();
    let before = script.model().snapshot().cores[0].fail;
    script.append(
        togi_journal::DeadEnd {
            condition: DeadEndCondition::FailureAtZero,
            core: Some(core(1)),
            detail: "core 01 failed at 0".to_owned(),
            action: None,
        }
        .into(),
        &[],
    );
    let model = script.model();
    let snap = model.snapshot();
    let named = &snap.cores[1];
    assert_eq!(named.fail, Some(off(0)));
    assert_eq!(
        snap.cores[0].fail, before,
        "the dead end names only core 01"
    );
}
