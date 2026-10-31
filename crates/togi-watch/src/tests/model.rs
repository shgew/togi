use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::{
    Activity, ApplicationId, ApplyCause, DayApplied, DeadEndCondition, Outcome, Payload,
    ProfileRestored, SessionBaseline, Shutdown, ShutdownReason, TrialId,
};
use togi_machine::{BootId, CcdId, CcdProfile, Condition, ObservedOffset, PerCore, Regime};

use super::kit::*;
use crate::history::{LOG_LIMIT, LogEntry};
use crate::model::{FoldError, Model, project};
use crate::words::Tag;

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

/// The firmware baseline the day profile history starts from: every core at 0.
fn baseline(cores: &[u8]) -> Payload {
    SessionBaseline {
        offsets: cores
            .iter()
            .map(|&id| (core(id), ObservedOffset(0)))
            .collect(),
    }
    .into()
}

/// A verified day application of `offsets`, one per CCD.
fn day_applied(application: u64, offsets: &[(i64, i64)]) -> Payload {
    let ccd: BTreeMap<_, _> = offsets
        .iter()
        .map(|&(ccd, offset)| (CcdId::new(ccd).unwrap(), off(offset)))
        .collect();
    DayApplied {
        application: ApplicationId(application),
        cause: ApplyCause::Reapplication {},
        target: expanded(CcdProfile::try_from(ccd).unwrap()),
        readbacks: PerCore::default(),
        transactions: Vec::new(),
        boot: BootId::from("boot".to_owned()),
        mono_ns: 0,
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
    assert!(snap.trial.is_none());
    assert_eq!((snap.log.len(), snap.log_total, snap.failures), (0, 0, 0));
    assert!(!snap.resting(), "nothing rests before a session starts");
}

#[test]
fn a_refused_event_turns_the_model_into_a_problem() {
    let mut script = Script::three();
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
    let script = Script::three();
    let mut model = Model::new();
    assert!(matches!(
        model.fold(&script.events[1]),
        Err(FoldError::Journal(togi_journal::FoldError::Gap { .. }))
    ));
    assert!(model.snapshot().session.is_none());
}

#[test]
fn project_folds_like_fold() {
    let script = Script::three();
    let projected = project(&script.events);
    assert!(projected.problem().is_none());
    assert_eq!(projected.snapshot().cores.len(), 3);
    let broken = project(&script.events[1..]);
    assert!(broken.problem().is_some());
}

#[test]
fn a_batch_without_trial_ends_matches_incremental_folding() {
    let script = Script::three();
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
    let script = Script::three();
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
    let mut script = Script::three();
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
    let mut script = Script::three();
    let (intent, trial) = script.intent(Regime::R1, &[0]);
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
        Duration::from_secs(120),
        "an SMU write after the intent does not hide the running trial"
    );
    let first = &snap.cores[0];
    assert!(first.loaded);
    assert_eq!(
        (first.applied, first.profile),
        (Some(ObservedOffset(-25)), off(0))
    );
    let waiting = &snap.cores[1];
    assert!(!waiting.loaded);
    script.end(intent, &trial, Outcome::Pass);
    let model = script.model();
    let snap = model.snapshot();
    assert!(snap.trial.is_none() && !snap.cores[0].loaded);
    assert_eq!(
        snap.history.first().map(|entry| entry.tag),
        Some(Tag::Pass),
        "the pass is told"
    );
}

#[test]
fn trial_preparation_does_not_claim_load() {
    let mut script = Script::three();
    script.intent(Regime::R1, &[0]);
    script.append(readback(0, -25), &[]);
    let model = script.model();
    let snap = model.snapshot();
    let trial = snap.trial.unwrap();
    assert!(trial.started.is_none());
    let first = &snap.cores[0];
    assert!(!first.loaded);
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
    let mut script = Script::three();
    let id = TrialId::from("0001".to_owned());
    let other = TrialId::from("other".to_owned());
    let mut want = if matches!(kind, TrialStartSelection::Before) {
        Some(at(script.start(&id)))
    } else {
        None
    };
    script.intent(Regime::R1, &[0]);
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
            script.events.push(event);
            want = Some(Timestamp::UNIX_EPOCH);
        }
    }
    let model = script.model();
    let trial = model.snapshot().trial.expect("the open trial is projected");
    assert_eq!(trial.started, want);
}

#[test]
fn stopped_shows_the_day_profile_not_the_applied_offsets() {
    let mut script = Script::three();
    script.append(baseline(&[0, 1, 2]), &[]);
    script.append(day_applied(1, &[(0, -20), (1, -10)]), &[]);
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
    let mut script = Script::three();
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
    let mut script = Script::three();
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
        (Some(ObservedOffset(0)), off(0))
    );
}

#[test]
fn sparse_core_ids() {
    let mut script = Script::new(&[(0, 0), (8, 1)]);
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
}

#[test]
fn sparse_readbacks_do_not_fill_other_topology_cores() {
    let mut script = Script::new(&[(0, 0), (8, 1)]);
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
    let mut script = Script::three();
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
    let mut script = Script::three();
    let (intent, trial) = script.intent(Regime::R1, &[0]);
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
    let mut script = Script::three();
    script.boot = "next".to_owned();
    let mut crashed = crash("boot", None);
    crashed.stray = true;
    script.append(crashed.into(), &[]);
    let (_, trial) = script.intent(Regime::R1, &[0]);
    assert!(
        script.model().snapshot().recover.is_some(),
        "preparing offsets does not hide recovery"
    );
    script.start(&trial);
    assert!(script.model().snapshot().recover.is_none());
}

#[test]
fn failure_counters_separate_observed_and_skipped() {
    let mut h = Hist::three();
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
    let mut script = Script::three();
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
    let mut script = Script::three();
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
    let mut script = Script::three();
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
fn between_trials_the_last_outcome_stands() {
    let mut script = Script::three();
    assert!(
        script.model().snapshot().last.is_none(),
        "no trial has ended yet"
    );
    let (intent, trial) = script.intent(Regime::R1, &[0]);
    script.end(intent, &trial, Outcome::Pass);
    let model = script.model();
    let snap = model.snapshot();
    let last = snap.last.expect("the last trial");
    assert_eq!(last.id, trial);
    assert_eq!(last.outcome, Outcome::Pass);
    assert_eq!(last.regime, Some(Regime::R1));
    assert_eq!(last.cores, set(&[0]));
    assert_eq!(last.planned, Some(Duration::from_secs(120)));
    assert!(snap.trial.is_none());
}

/// What a frame between trials shows of the trial that just ended: its id and the policy as it stood.
fn ended_of(model: &Model) -> Option<(String, String)> {
    model.snapshot().ended.map(|ended| {
        (
            ended.trial.id.to_string(),
            format!("{:?}", ended.policy.night),
        )
    })
}

#[test]
fn only_the_last_end_of_a_batch_keeps_its_outcomes_and_a_batch_without_one_keeps_the_last() {
    let mut script = Script::three();
    let mut ends = Vec::new();
    for outcome in [Outcome::Pass, Outcome::Failure] {
        script.run(Regime::R1, &[0], outcome);
        ends.push(script.events.len());
    }
    script.append(warning("between trials"), &[]);
    let next_decided = script.events.len();
    script.intent(Regime::R1, &[0]);
    let events = &script.events;
    let next_intent = events.len();
    assert!(
        next_decided > ends[1],
        "something is recorded between the last end and the next intent"
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
    let mut script = Script::three();
    let (intent, trial) = script.intent(Regime::R1, &[0]);
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
    let mut script = Script::three();
    let (intent, trial) = script.intent(Regime::R1, &[0]);
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
    let mut script = Script::three();
    let (intent, trial) = script.intent(Regime::R1, &[0]);
    script.end(intent, &trial, Outcome::Pass);
    let after_end = script.events.len();
    let mut batch = project(&script.events);
    let kept = ended_of(&batch);
    script.append(
        togi_journal::CommandReset {
            target: togi_journal::ResetTarget::Core(core(1)),
        }
        .into(),
        &[],
    );
    batch.fold_batch(&script.events[after_end..]).unwrap();
    assert!(kept.is_some());
    assert_eq!(ended_of(&batch), kept);
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
fn a_recovered_trial_keeps_its_start() {
    let mut script = Script::three();
    let (intent, trial) = script.intent(Regime::R1, &[0]);
    let started = script.start(&trial);
    script.boot = "next".to_owned();
    script.append(crash("boot", Some(intent)).into(), &[]);
    let model = script.model();
    let recovered = model
        .snapshot()
        .recover
        .and_then(|r| r.trial.as_ref())
        .expect("the crashed trial");
    assert_eq!(recovered.id, trial);
    assert_eq!(
        recovered.started,
        Some(at(started)),
        "the crashed trial had started"
    );
    assert_eq!(recovered.idle, set(&[1, 2]), "it loaded core 00 alone");
}

#[test]
fn a_prior_recovery_does_not_mask_a_later_boot_crash() {
    let mut script = Script::three();
    let (first_intent, first_trial) = script.intent(Regime::R1, &[0]);
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
    let (_, second_trial) = script.intent(Regime::R1, &[0]);
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
fn a_crash_ending_a_trial_says_how_far_into_it_the_machine_went_down() {
    let mut script = Script::three();
    let (intent, trial) = script.intent(Regime::R1, &[0]);
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
        .find(|entry| entry.tag == Tag::Crash)
        .expect("one crash line");
    assert!(
        ended.text().contains("late in the trial"),
        "{}",
        ended.text()
    );
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.tag == Tag::Crash)
            .count(),
        1,
        "the trial's end takes the place of the reboot line"
    );
}

#[test_case(Regime::R6, true; "an idle trial holds")]
#[test_case(Regime::R7, false; "a loaded trial does not")]
fn only_a_started_idle_trial_holds_the_frame(regime: Regime, holds: bool) {
    let mut script = Script::three();
    let (_, trial) = script.intent(regime, &[0, 1, 2]);
    assert!(
        script.model().snapshot().idle_hold().is_none(),
        "nothing holds before the start"
    );
    let started = script.start(&trial);
    let model = script.model();
    let hold = model.snapshot().idle_hold();
    assert_eq!(hold.is_some(), holds);
    if let Some(hold) = hold {
        assert_eq!(hold.started, at(started));
        assert_eq!(
            hold.planned_end,
            at(started)
                .checked_add(jiff::SignedDuration::from_secs(120))
                .unwrap()
        );
    }
}

#[test]
fn a_reset_command_is_told_in_the_history() {
    let mut script = Script::three();
    script.append(
        Payload::CommandReset(togi_journal::CommandReset {
            target: togi_journal::ResetTarget::Core(core(1)),
        }),
        &[],
    );
    let model = script.model();
    assert_eq!(
        model
            .snapshot()
            .history
            .first()
            .map(|entry| entry.tag.word()),
        Some("reset")
    );
}

#[test]
fn configuration_reopens_a_stop_but_reset_and_warning_do_not() {
    let mut script = Script::three();
    script.append(restored(), &[]);
    script.append(shutdown(ShutdownReason::Signal), &[]);
    script.append(
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
    script.append(config_loaded(), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert!(snap.stopped.is_none(), "config.loaded reopens the session");
}

#[test_case("boot", true; "same boot remains stopped")]
#[test_case("day", false; "later boot resumes day observation")]
fn day_observation_reopens_a_stop_only_on_a_later_boot(boot: &str, stopped: bool) {
    let mut script = Script::three();
    script.append(restored(), &[]);
    script.append(shutdown(ShutdownReason::Signal), &[]);
    let mut model = script.model();
    let after_stop = script.events.len();
    script.boot = "intervening".to_owned();
    script.append(warning("stopped run"), &[]);
    script.boot = boot.to_owned();
    script.append(
        togi_journal::DayBootStart {
            build: build(),
            bios: togi_machine::BiosContext::default(),
            offsets: [0, 1, 2]
                .into_iter()
                .map(|id| (core(id), ObservedOffset(0)))
                .collect(),
            watchdog: None,
        }
        .into(),
        &[],
    );
    model.fold_batch(&script.events[after_stop..]).unwrap();
    assert_eq!(model.snapshot().stopped.is_some(), stopped);
    assert_eq!(model.snapshot(), script.model().snapshot());
}

#[test]
fn a_fold_after_a_snapshot_shows_the_new_event() {
    let mut script = Script::three();
    let mut model = script.model();
    let before = {
        let snap = model.snapshot();
        assert!(snap.trial.is_none());
        snap.log_total
    };
    let (_, trial) = script.intent(Regime::R1, &[0]);
    model.fold(script.events.last().unwrap()).unwrap();
    let snap = model.snapshot();
    assert_eq!(snap.trial.map(|t| &t.id), Some(&trial), "the open trial");
    assert_eq!(snap.log_total, before + 1);
}

#[test]
fn the_session_does_what_its_last_intent_did() {
    let mut h = Hist::three();
    assert_eq!(h.facts.session.as_ref().unwrap().activity, None);
    let mut first = intent(
        "c1",
        Regime::R7,
        Condition::Together,
        &[-20, -30, -10],
        &[0, 1, 2],
    );
    first.activity = Some(Activity::Mapping);
    h.push(first.into());
    assert_eq!(
        h.facts.session.as_ref().unwrap().activity,
        Some(Activity::Mapping)
    );
    let mut second = intent("c2", Regime::R1, Condition::Alone, &[-20], &[0]);
    second.activity = Some(Activity::Qualification);
    h.push(second.into());
    assert_eq!(
        h.facts.session.as_ref().unwrap().activity,
        Some(Activity::Qualification)
    );
}

#[test]
fn an_event_the_policy_refuses_is_a_problem() {
    let mut script = Script::three();
    script.append(baseline(&[0, 1, 2]), &[]);
    let mut model = script.model();
    let foreign = script.event(day_applied(1, &[(0, -10), (1, -10), (7, -10)]), &[]);
    let error = model.fold(&foreign).unwrap_err();
    assert!(
        matches!(
            error,
            FoldError::Tuner(togi_tuner::policy::FoldError::Climb(
                togi_tuner::climb::ClimbError::Profile(togi_tuner::UnitProfileError::Extra(
                    togi_tuner::UnitId::Ccd(ccd),
                )),
            )) if ccd.get() == 7
        ),
        "{error:?}"
    );
    let snap = model.snapshot();
    assert!(snap.session.is_none() && snap.history.is_empty());
    assert_eq!(snap.log_total, 0);
    let later = script.event(warning("later"), &[]);
    assert!(matches!(
        model.fold(&later),
        Err(FoldError::Tuner(togi_tuner::policy::FoldError::Climb(
            togi_tuner::climb::ClimbError::Profile(togi_tuner::UnitProfileError::Extra(
                togi_tuner::UnitId::Ccd(ccd),
            )),
        ))) if ccd.get() == 7
    ));
}

#[test]
fn a_reset_in_a_later_boot_leaves_the_crashed_trial_unrecovered() {
    let mut script = Script::three();
    let (_, trial) = script.intent(Regime::R1, &[0]);
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
    let mut script = Script::three();
    let (_, trial) = script.intent(Regime::R1, &[0]);
    let started = script.start(&trial);
    let planned_end = at(started)
        .checked_add(jiff::SignedDuration::from_secs(120))
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
    let model = Script::three().model();
    let snap = model.snapshot();
    assert_eq!(snap.cores[0].applied, None);
    assert_eq!(snap.cores[0].profile, off(0));
}

#[test_case(&[], (0, 0); "CO 0 before any day application")]
#[test_case(&[(0, -20), (1, -10)], (-20, -10); "each core shows its CCD's day offset")]
fn a_core_shows_the_day_offset_of_its_ccd(applied: &[(i64, i64)], want: (i64, i64)) {
    let mut script = Script::three();
    script.append(baseline(&[0, 1, 2]), &[]);
    if !applied.is_empty() {
        script.append(day_applied(1, applied), &[]);
    }
    let model = script.model();
    let snap = model.snapshot();
    let profiles: Vec<_> = snap.cores.iter().map(|core| core.profile).collect();
    assert_eq!(profiles, [off(want.0), off(want.0), off(want.1)]);
    let days: Vec<_> = snap
        .policy
        .units
        .iter()
        .map(|unit| (unit.unit.ccd().unwrap(), unit.day))
        .collect();
    assert_eq!(
        days,
        [
            (CcdId::new(0).unwrap(), off(want.0)),
            (CcdId::new(1).unwrap(), off(want.1)),
        ],
        "the snapshot carries the policy's projection"
    );
}

#[test]
fn the_ended_trial_keeps_the_policy_as_it_stood_before_its_end() {
    let mut script = Script::three();
    script.append(baseline(&[0, 1, 2]), &[]);
    let (intent, trial) = script.intent(Regime::R1, &[0]);
    script.start(&trial);
    let before = script.model().snapshot().policy.clone();
    script.end(intent, &trial, Outcome::Pass);
    script.append(day_applied(1, &[(0, -5), (1, -5)]), &[]);
    let model = script.model();
    let snap = model.snapshot();
    let ended = snap.ended.expect("the trial that just ended");
    assert_eq!(ended.trial.id, trial);
    assert_eq!(ended.policy, &before);
    assert_ne!(
        snap.policy, &before,
        "the live projection moved on with the application"
    );
}

#[test]
fn only_an_application_caused_by_a_step_marks_that_step_applied() {
    let mut script = Script::three();
    script.append(baseline(&[0, 1, 2]), &[]);
    script.append(day_applied(1, &[(0, -5), (1, -5)]), &[]);
    assert!(script.model().snapshot().applied_steps.is_empty());
    let mut stepped = day_applied(2, &[(0, -6), (1, -5)]);
    if let Payload::DayApplied(applied) = &mut stepped {
        applied.cause = ApplyCause::Step {
            decision: togi_journal::DecisionId(7),
        };
    }
    script.append(stepped, &[]);
    let model = script.model();
    assert_eq!(
        model.snapshot().applied_steps,
        &BTreeSet::from([togi_journal::DecisionId(7)])
    );
}
