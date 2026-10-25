//! The tests of what the history says, fed to the facts alone: the journals here are ones the tuner would
//! refuse, which is the point: the history must say what the events say.

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::{
    Activity, Mce, Outcome, Payload, ProfileApplied, QualificationState, TaskId, Tested, TrialId,
    TrialProgress,
};
use togi_machine::{BankType, BootId, CcdId, Condition, CpuId, Regime, Signal};

use super::kit::*;
use super::words::{edge_event, plan_event, qualification_event};
use crate::history::{Entry, Failed, HISTORY_LIMIT, Named, Passes, Verdict, What, fold_entry};
use crate::words::{Tag, Tone};

fn trial_id(text: &str) -> TrialId {
    TrialId::from(text.to_owned())
}

fn applied() -> Payload {
    ProfileApplied {
        offsets: offsets(&[-20, -10, -10]),
        condition: Condition::Together,
    }
    .into()
}

#[test]
fn reboot_history_during_smu_application() {
    let mut h = Hist::three();
    h.push(
        ProfileApplied {
            offsets: offsets(&[-10, -10, -10]),
            condition: Condition::Together,
        }
        .into(),
    );
    h.push(smu_intent(0, -20));
    h.boot = "next".to_owned();
    let at = h.push(crash("boot", Some(seq(3))).into());
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "crash: with the offsets applied, rebooted"
        ]
    );
    let (entries, _) = h.facts.history.recent();
    assert_eq!(h.facts.crashes, 1);
    assert_eq!(entries[0].reboot(), Some(at));
    assert_eq!(entries[0].tone, Tone::Bad);
}

#[test_case(
    Signal::Crash,
    None,
    "crash: checking · R2 heavy vector on core 00 at -20 · rebooted";
    "ordinary crash"
)]
#[test_case(
    Signal::ComputationError,
    Some(
        TrialProgress {
            trial: trial_id("trial"),
            detail: String::new(),
            signal: Some(Signal::ComputationError),
            core: Some(core(0)),
            pid: None,
            fault: None,
        }
        .into(),
    ),
    "fail: checking · R2 heavy vector on core 00 at -20 · wrong result";
    "retained computation failure"
)]
#[test_case(
    Signal::CorrectedMce,
    Some(mce(true)),
    "fail: checking · R2 heavy vector on core 00 at -20 · corrected hardware error";
    "retained corrected MCE"
)]
#[test_case(
    Signal::UncorrectedMce,
    Some(mce(false)),
    "fail: checking · R2 heavy vector on core 00 at -20 · uncorrected hardware error";
    "retained uncorrected MCE"
)]
fn reboot_history_during_trial(signal: Signal, evidence: Option<Payload>, end_sentence: &str) {
    let retained = evidence.is_some();
    let mut h = Hist::three();
    let mut trial = intent(
        "trial",
        Regime::R2,
        Condition::Together,
        &[-20, -10, -10],
        &[],
    );
    trial.core = Some(core(0));
    let intent_seq = h.push(trial.into());
    h.push(applied());
    h.push(
        togi_journal::TrialStart {
            trial: trial_id("trial"),
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
    );
    let evidence_seq = evidence.map(|payload| h.push(payload));
    h.boot = "next".to_owned();
    let crash_seq = h.push(crash("boot", Some(intent_seq)).into());
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "crash: with the offsets applied, rebooted"
        ],
        "a reboot is visible before the trial end is appended"
    );
    let cause = [intent_seq, evidence_seq.unwrap_or(crash_seq)];
    let mut end = end_payload(&trial_id("trial"), Outcome::Failure, Some(signal));
    end.interrupted = retained;
    h.push_with(end.into(), &cause);
    let mut failed = failure(signal, Some("trial"));
    failed.regime = Some(Regime::R2);
    h.push(failed.into());
    let mut want = vec!["start: session started on 3 cores"];
    if retained {
        want.push("crash: with the offsets applied, rebooted");
    }
    want.push(end_sentence);
    assert_eq!(
        h.sentences(),
        want,
        "only a crash outcome represents the reboot"
    );
    let (entries, _) = h.facts.history.recent();
    let reboot = &entries[usize::from(retained)];
    assert_eq!(reboot.reboot(), Some(crash_seq));
    assert_eq!(
        reboot.at,
        Timestamp::from_second(1000 + i64::try_from(crash_seq.get()).unwrap()).unwrap(),
        "the reboot keeps its original time"
    );
}

fn mce(corrected: bool) -> Payload {
    Mce {
        cpu: CpuId::from(0),
        core: Some(core(0)),
        bank: 0,
        bank_type: BankType::LoadStore,
        corrected,
        monotonic_ns: None,
        from_boot: Some(BootId::from("boot".to_owned())),
        trial: Some(trial_id("trial")),
        between_trials: false,
        lines: Vec::new(),
    }
    .into()
}

#[test]
fn multiple_reboots_stay_distinct() {
    let mut h = Hist::three();
    h.push(Payload::SmuIntent(
        serde_json::from_str(r#"{"op":"set_all","offset":-10}"#).unwrap(),
    ));
    h.boot = "next".to_owned();
    let first = h.push(crash("boot", Some(seq(2))).into());
    let mut trial = intent(
        "trial",
        Regime::R2,
        Condition::Together,
        &[-20, -10, -10],
        &[],
    );
    trial.core = Some(core(0));
    h.push(trial.into());
    h.push(applied());
    h.push(
        togi_journal::TrialStart {
            trial: trial_id("trial"),
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
    );
    h.boot = "last".to_owned();
    let second = h.push(crash("next", Some(seq(4))).into());
    h.push_with(
        end_payload(&trial_id("trial"), Outcome::Failure, Some(Signal::Crash)).into(),
        &[second],
    );
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "crash: with the offsets applied, rebooted",
            "crash: checking · R2 heavy vector on core 00 at -20 · rebooted",
        ]
    );
    let (entries, _) = h.facts.history.recent();
    assert_eq!(h.facts.crashes, 2);
    assert_eq!(entries[1].reboot(), Some(first));
    assert_eq!(entries[0].reboot(), Some(second));
}

#[test]
fn reboot_history_requires_matching_cause() {
    let mut h = Hist::three();
    h.boot = "next".to_owned();
    h.push(crash("boot", None).into());
    h.push_with(
        end_payload(
            &trial_id("unrelated"),
            Outcome::Failure,
            Some(Signal::Crash),
        )
        .into(),
        &[seq(1)],
    );
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "crash: idle with the offsets applied, rebooted",
            "crash: trial unrelated · rebooted",
        ]
    );
}

#[test]
fn orphan_trial_id_stays_raw() {
    let id =
        "orphan\u{1b}]52;c;payload\u{7}\n\r\t\u{7f}\u{9b}2J\u{9d}title\u{2028}\u{2029}\u{202e}";
    let mut h = Hist::three();
    h.push(
        end_payload(
            &trial_id(id),
            Outcome::Failure,
            Some(Signal::ComputationError),
        )
        .into(),
    );
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores".to_owned(),
            format!("fail: trial {id} · wrong result"),
        ]
    );
}

fn warnings(h: &mut Hist, count: usize) {
    for i in 0..count {
        h.push(warning(&i.to_string()));
    }
}

#[test]
fn history_bounds_and_counts_what_it_leaves_out() {
    let mut h = Hist::three();
    warnings(&mut h, 2 * HISTORY_LIMIT + 5);
    let (entries, dropped) = h.facts.history.recent();
    assert_eq!(entries.len(), HISTORY_LIMIT);
    assert_eq!(dropped + entries.len(), 2 * HISTORY_LIMIT + 6);
    assert_eq!(
        entries[0].text(),
        warning(&(2 * HISTORY_LIMIT + 4).to_string()).message()
    );
}

#[test]
fn a_reset_and_an_mce_have_lines() {
    let mut h = Hist::three();
    h.push(
        togi_journal::Mce {
            cpu: togi_machine::CpuId::from(3),
            core: Some(core(1)),
            bank: 1,
            bank_type: togi_machine::BankType::ExecutionUnit,
            corrected: false,
            monotonic_ns: None,
            from_boot: None,
            trial: None,
            between_trials: true,
            lines: Vec::new(),
        }
        .into(),
    );
    assert_eq!(
        h.sentences()[1],
        "mce: core 01 uncorrected error between trials (recorded only)"
    );
    h.push(
        togi_journal::CommandReset {
            target: togi_journal::ResetTarget::All,
        }
        .into(),
    );
    assert_eq!(
        h.sentences()[2],
        "reset: reset all: new session on next run"
    );
}

/// A trial of a Ruleset 13 night: what it is for, what it tests and the task it belongs to.
fn night_trial(id: &str, activity: Activity, tested: Tested) -> togi_journal::TrialIntent {
    let mut trial = intent(
        id,
        Regime::R2,
        Condition::Together,
        &[-10, -10, -10],
        &[0, 1, 2],
    );
    trial.activity = Some(activity);
    trial.tested = Some(tested);
    trial.night = Some(togi_journal::NightId(seq(4)));
    trial.task = Some(TaskId {
        queue: togi_journal::Queue::Mapping,
        ordinal: 3,
    });
    trial
}

fn isolated() -> Tested {
    Tested::Isolated {
        ccd: CcdId::new(1).unwrap(),
        offset: off(-15),
    }
}

#[test]
fn a_passed_night_trial_names_its_activity_what_it_tested_and_its_task() {
    let mut h = Hist::three();
    h.push(night_trial("m1", Activity::Mapping, isolated()).into());
    h.push(end_payload(&trial_id("m1"), Outcome::Pass, None).into());
    assert_eq!(
        h.sentences()[1],
        "pass: mapping · CCD 1 at -15 · mapping task 3 · R2 heavy vector on 00-02 · passed"
    );
}

#[test_case(Activity::Qualification, Tested::Exact {}, false, "qualification · the exact profile · mapping task 3 · R2 heavy vector on 00-02"; "exact qualification")]
#[test_case(Activity::Control, Tested::Exact {}, true, "control · the exact profile · mapping task 3 · retry · R2 heavy vector on 00-02"; "a retried control")]
fn a_night_trial_says_whether_it_is_a_retry(
    activity: Activity,
    tested: Tested,
    retry: bool,
    subject: &str,
) {
    let mut h = Hist::three();
    let mut trial = night_trial("t", activity, tested);
    trial.retry = retry;
    h.push(trial.into());
    h.push(end_payload(&trial_id("t"), Outcome::Failure, Some(Signal::Stall)).into());
    assert_eq!(
        h.sentences()[1],
        format!("fail: {subject} · the backend stalled")
    );
}

#[test]
fn passes_of_one_night_trial_merge_and_another_subject_starts_a_line() {
    let mut h = Hist::three();
    for id in ["a", "b"] {
        h.push(night_trial(id, Activity::Mapping, isolated()).into());
        let mut end = end_payload(&trial_id(id), Outcome::Pass, None);
        end.tctl_max_c = Some(if id == "a" { 70 } else { 76 });
        h.push(end.into());
    }
    h.push(
        night_trial(
            "c",
            Activity::Mapping,
            Tested::Isolated {
                ccd: CcdId::new(0).unwrap(),
                offset: off(-15),
            },
        )
        .into(),
    );
    h.push(end_payload(&trial_id("c"), Outcome::Pass, None).into());
    assert_eq!(
        h.sentences()[1..],
        [
            "pass: mapping · CCD 1 at -15 · mapping task 3 · R2 heavy vector on 00-02 · 2 passed · Tctl max 76°C",
            "pass: mapping · CCD 0 at -15 · mapping task 3 · R2 heavy vector on 00-02 · passed",
        ]
    );
}

#[test]
fn a_crash_late_in_a_night_trial_says_how_far_it_got() {
    let mut h = Hist::three();
    h.push(night_trial("m", Activity::Mapping, isolated()).into());
    let mut end = end_payload(&trial_id("m"), Outcome::Failure, Some(Signal::Crash));
    end.last_sample_s = Some(100);
    end.core = Some(core(1));
    h.push(end.into());
    assert_eq!(
        h.sentences()[1],
        "crash: mapping · CCD 1 at -15 · mapping task 3 · R2 heavy vector on 00-02 · rebooted · late in the trial · core 01 named"
    );
}

#[test]
fn an_unclear_night_trial_says_why() {
    let mut h = Hist::three();
    h.push(night_trial("m", Activity::Mapping, isolated()).into());
    let mut end = end_payload(&trial_id("m"), Outcome::Inconclusive, None);
    end.reason = Some("the backend was missing".to_owned());
    h.push(end.into());
    assert_eq!(
        h.sentences()[1],
        "unclear: mapping · CCD 1 at -15 · mapping task 3 · R2 heavy vector on 00-02 · the backend was missing"
    );
}

#[test]
fn a_trial_of_an_earlier_ruleset_is_named_by_its_load_and_offset() {
    let mut h = Hist::three();
    let mut trial = intent("old", Regime::R2, Condition::Alone, &[-20, 0, 0], &[0]);
    trial.activity = Some(Activity::Search);
    h.push(trial.into());
    h.push(end_payload(&trial_id("old"), Outcome::Pass, None).into());
    assert_eq!(
        h.sentences()[1],
        "pass: search · R2 heavy vector on core 00 at -20 · passed"
    );
}

#[test]
fn a_night_and_an_edge_are_in_the_history_and_a_resume_follows_the_second_config() {
    let mut h = Hist::three();
    h.push(config_loaded());
    h.push(config_loaded());
    h.push(plan_event());
    h.push(edge_event(
        togi_journal::Edge::Unknown {},
        togi_journal::Edge::Observed {
            offset: off(-23),
            witness: seq(3),
        },
    ));
    let sentences = h.sentences();
    assert_eq!(sentences[0], "start: session started on 3 cores");
    assert_eq!(sentences[1], "resume: session resumed");
    assert_eq!(sentences[2], "night: night opened · 2 tasks");
    assert_eq!(sentences[3], "edge: CCD1 edge unknown → fails at -23");
}

#[test]
fn a_qualification_in_progress_adds_no_line() {
    let mut h = Hist::three();
    h.push(qualification_event(QualificationState::Progress {
        witnesses: Vec::new(),
        outstanding: Vec::new(),
    }));
    assert_eq!(h.sentences(), ["start: session started on 3 cores"]);
}

#[test]
fn a_kind_of_an_earlier_ruleset_is_the_message_it_recorded() {
    let mut h = Hist::three();
    let hunt = hunt_start(1, &[0, 2]);
    let message = Payload::from(hunt.clone()).message();
    h.push(hunt.into());
    let started = cycle(1, togi_journal::CycleEvent::Start);
    let cycle_message = Payload::from(started.clone()).message();
    h.push(started.into());
    assert_eq!(
        h.sentences()[1..],
        [format!("note: {message}"), format!("note: {cycle_message}")]
    );
}

#[test]
fn a_failure_draws_its_own_signal_as_the_alarm() {
    let failure = |signal| {
        let failed = Failed {
            subject: "mapping · CCD 1 at -15".to_owned(),
            verdict: Verdict {
                signal: Some(signal),
                when: None,
                named: Some(Named::Core(core(1))),
            },
            reboot: None,
        };
        Entry::new(at(seq(1)), Tag::Fail, What::Failed(failed), Tone::Bad)
    };
    assert_eq!(
        failure(Signal::ComputationError).sentence(),
        (
            "mapping · CCD 1 at -15 · ".to_owned(),
            "wrong result".to_owned(),
            " · core 01 named".to_owned()
        )
    );
    assert_eq!(
        failure(Signal::Crash).sentence(),
        (
            "mapping · CCD 1 at -15 · rebooted · core 01 named".to_owned(),
            String::new(),
            String::new()
        )
    );
}

#[test]
fn repeated_passes_fold_into_one_line_and_other_lines_never_merge() {
    let pass = |subject: &str, peak| {
        let passes = Passes {
            subject: subject.to_owned(),
            runs: 1,
            peak: Some(peak),
        };
        Entry::new(at(seq(1)), Tag::Pass, What::Passes(passes), Tone::Good)
    };
    let plain = Entry::line(at(seq(1)), Tag::Note, "first".to_owned(), Tone::Plain);
    let mut history = Vec::new();
    fold_entry(&mut history, plain.clone());
    assert_eq!(history.len(), 1, "the first line has nothing to merge into");
    fold_entry(&mut history, plain);
    assert_eq!(history.len(), 2, "a plain line never merges");
    for (subject, peak) in [("a", 70), ("a", 76), ("a", 62), ("b", 60)] {
        fold_entry(&mut history, pass(subject, peak));
    }
    assert_eq!(history.len(), 4);
    assert_eq!(
        history[2].what,
        What::Passes(Passes {
            subject: "a".to_owned(),
            runs: 3,
            peak: Some(76)
        })
    );
    assert_eq!(sentence(&history[2]), "pass: a · 3 passed · Tctl max 76°C");
    assert_eq!(sentence(&history[3]), "pass: b · passed · Tctl max 60°C");
}

#[test]
fn a_single_pass_is_one_passed_line() {
    let mut h = Hist::three();
    h.push(
        intent(
            "p1",
            Regime::R7,
            Condition::Together,
            &[-20, -30, -10],
            &[0, 1, 2],
        )
        .into(),
    );
    h.push(end_payload(&trial_id("p1"), Outcome::Pass, None).into());
    assert_eq!(
        h.sentences()[1],
        "pass: checking · R7 all-core on 00-02 · passed"
    );
}
