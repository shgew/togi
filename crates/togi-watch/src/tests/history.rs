//! The tests of what the history says, fed to the facts alone: the journals here are ones the tuner would
//! refuse, which is the point: the history must say what the events say.

use jiff::Timestamp;
use togi_journal::{
    CorePhase, CycleEvent, Decision, DeepeningRound, HuntOutcome, Inference, Mce, Member, Outcome,
    Payload, ProfileApplied, RoundEvent, TrialId, TrialProgress, TunerDecision,
};
use togi_machine::{BankType, BootId, Condition, CpuId, Regime, Signal};

use super::kit::*;
use crate::history::{
    Entry, Failed, Groups, HISTORY_LIMIT, KnownFailure, Named, PassedIn, Passes, Verdict, What,
    fold_entry,
};
use crate::model::Count;
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

#[test]
fn reboot_history_during_trial() {
    struct Case {
        name: &'static str,
        signal: Signal,
        evidence: Option<Payload>,
        end: &'static str,
    }
    let cases = [
        Case {
            name: "ordinary crash",
            signal: Signal::Crash,
            evidence: None,
            end: "crash: R2 heavy vector on core 00 at -20 · rebooted · no core named",
        },
        Case {
            name: "retained computation failure",
            signal: Signal::ComputationError,
            evidence: Some(
                TrialProgress {
                    trial: trial_id("trial"),
                    detail: String::new(),
                    signal: Some(Signal::ComputationError),
                    core: Some(core(0)),
                }
                .into(),
            ),
            end: "fail: R2 heavy vector on core 00 at -20 · wrong result · no core named",
        },
        Case {
            name: "retained corrected MCE",
            signal: Signal::CorrectedMce,
            evidence: Some(mce(true)),
            end: "fail: R2 heavy vector on core 00 at -20 · corrected hardware error · no core named",
        },
        Case {
            name: "retained uncorrected MCE",
            signal: Signal::UncorrectedMce,
            evidence: Some(mce(false)),
            end: "fail: R2 heavy vector on core 00 at -20 · uncorrected hardware error · no core named",
        },
    ];
    for case in cases {
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
            }
            .into(),
        );
        let evidence_seq = case.evidence.clone().map(|payload| h.push(payload));
        h.boot = "next".to_owned();
        let crash_seq = h.push(crash("boot", Some(intent_seq)).into());
        assert_eq!(
            h.sentences(),
            [
                "start: session started on 3 cores",
                "crash: with the offsets applied, rebooted"
            ],
            "{}: a reboot is visible before the trial end is appended",
            case.name
        );
        let cause = [intent_seq, evidence_seq.unwrap_or(crash_seq)];
        let mut end = end_payload(&trial_id("trial"), Outcome::Failure, Some(case.signal));
        end.interrupted = case.evidence.is_some();
        h.push_with(end.into(), &cause);
        let mut failed = failure(case.signal, Some("trial"));
        failed.regime = Some(Regime::R2);
        h.push(failed.into());
        let mut want = vec!["start: session started on 3 cores"];
        if case.evidence.is_some() {
            want.push("crash: with the offsets applied, rebooted");
        }
        want.push(case.end);
        assert_eq!(
            h.sentences(),
            want,
            "{}: only a crash outcome represents the reboot",
            case.name
        );
        let (entries, _) = h.facts.history.recent();
        let reboot = &entries[usize::from(case.evidence.is_some())];
        assert_eq!(reboot.reboot(), Some(crash_seq), "{}", case.name);
        assert_eq!(
            reboot.at,
            Timestamp::from_second(1000 + i64::try_from(crash_seq.get()).unwrap()).unwrap(),
            "{}: the reboot keeps its original time",
            case.name
        );
    }
}

fn mce(corrected: bool) -> Payload {
    Mce {
        cpu: CpuId(0),
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
            "crash: R2 heavy vector on core 00 at -20 · rebooted · no core named",
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
fn fallback_history() {
    for (name, result, fallback, want) in [
        (
            "unresolved fallback",
            HuntOutcome::Fallback,
            true,
            [
                "start: session started on 3 cores",
                "combo: C1 over 00 02 · hunt 3 unresolved",
                "hunt: #3 done · 00 02 unresolved",
            ],
        ),
        (
            "proven combination",
            HuntOutcome::Combination,
            false,
            [
                "start: session started on 3 cores",
                "combo: C1: 00 -20  02 -10",
                "hunt: #3 done · 00 02 kept together",
            ],
        ),
    ] {
        let mut h = Hist::three();
        h.push(combination(1, 3, &[(0, -20), (2, -10)], fallback).into());
        h.push(hunt_end(3, result, &[0, 2]).into());
        assert_eq!(h.sentences(), want, "{name}");
        let (entries, _) = h.facts.history.recent();
        assert_eq!(entries[1].tone, Tone::Combo, "{name}");
        assert_eq!(entries[0].tone, Tone::Warn, "{name}");
    }
}

#[test]
fn cycle_history() {
    let mut h = Hist::three();
    let start = |n| {
        let mut c = cycle(n, CycleEvent::Start);
        c.steps = vec![Regime::R1];
        Payload::from(c)
    };
    h.push(start(1));
    let mut c = cycle(1, CycleEvent::End);
    c.passed = true;
    c.missing = vec!["R2: 3 more steps".to_owned()];
    h.push(c.into());
    h.push(start(2));
    let mut c = cycle(2, CycleEvent::End);
    c.passed = true;
    c.full = true;
    h.push(c.into());
    h.push(start(3));
    let mut c = cycle(3, CycleEvent::End);
    c.reason = Some("failure".to_owned());
    h.push(c.into());
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "cycle: cycle 1 started · 1 step",
            "cycle: cycle 1 passed but missing R2: 3 more steps",
            "cycle: cycle 2 started · 1 step",
            "cycle: cycle 2 passed, a full cycle",
            "cycle: cycle 3 started · 1 step",
            "cycle: cycle 3 ended early: failure",
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

#[test]
fn deepening_history_separates_deeper_and_yielded_members() {
    let mut h = Hist::three();
    for (id, to, offset) in [
        (0, CorePhase::HasRoom, -20),
        (1, CorePhase::AtLimit, -30),
        (2, CorePhase::AtLimit, -10),
    ] {
        let mut start = Start::has_room(offset);
        (start.from, start.pass) = (Some(CorePhase::Search), Some(offset));
        start.to = to;
        h.push(transition(id, start).into());
    }
    h.push(
        DeepeningRound {
            round: nz(2),
            event: RoundEvent::Start,
            base: offsets(&[]),
            base_seq: None,
            target: offsets(&[-22, -29, -10]),
            profile: offsets(&[-21, -29, -10]),
            cores: set(&[0, 1]),
            ranking: Vec::new(),
            trials: None,
            trial_s: None,
            passed: false,
            reason: None,
        }
        .into(),
    );
    let decision = |core_id, kind, from, to| -> Payload {
        TunerDecision {
            core: core(core_id),
            activity: togi_journal::Activity::Deepening,
            decision: kind,
            from_offset: off(from),
            to_offset: off(to),
            pass: None,
            failure_point: None,
            workloads: Vec::new(),
            reason: String::new(),
        }
        .into()
    };
    h.push(decision(1, Decision::Yield, -30, -29));
    h.push(decision(0, Decision::Deepen, -20, -21));
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "limit: core 00 solo limit -20",
            "limit: core 01 solo limit -30",
            "limit: core 02 solo limit -10",
            "round: #2: core 00 goes deeper to -21, core 01 yields to -29",
            "room: core 01 yields -30 → -29, back to its round baseline",
            "room: core 00 -20 → -21",
        ]
    );
}

#[test]
fn inferred_group_answers_do_not_hide_outcome_changes() {
    let mut h = Hist::three();
    h.push(hunt_start(4, &[0, 1, 2]).into());
    for (group, cores, answer) in [
        (2, &[0][..], Inference::Pass),
        (3, &[1], Inference::Pass),
        (4, &[0, 1], Inference::Failure),
    ] {
        let mut g = hunt_group(4, group, cores);
        g.inferred = Some(answer);
        h.push(g.into());
    }
    h.push(hunt_start(5, &[0, 1]).into());
    let mut g = hunt_group(5, 5, &[1]);
    g.inferred = Some(Inference::Pass);
    h.push(g.into());
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "hunt: #4 started · part 1: 00 at failing offsets, 01 02 parked",
            "group: hunt 4 groups 2-3 · parts of 00 01 · all passed on existing evidence",
            "group: hunt 4 group 4 · 00 01 · failed on existing evidence",
            "hunt: #5 started · part 1: 01 at failing offsets, 00 parked",
            "group: hunt 5 group 5 · 01 · passed on existing evidence",
        ]
    );
}

#[test]
fn tally_never_counts_past_the_requirement() {
    for (first, runs, of, want) in [
        (1, 4, 4, "4 of 4"),
        (1, 3, 4, "trials 1-3 of 4"),
        (2, 1, 4, "trial 2 of 4"),
        (2, 4, 5, "trials 2-5 of 5"),
        (2, 5, 5, "5"),
        (1, 1, 1, "1"),
    ] {
        let passes = Passes {
            subject: String::new(),
            runs,
            first,
            of,
            peak: None,
            within: None,
        };
        assert_eq!(passes.tally(), want, "first {first}, runs {runs}, of {of}");
    }
}

#[test]
fn repeated_outcomes_fold_into_one_line() {
    let clock = std::cell::Cell::new(Timestamp::UNIX_EPOCH);
    let next = || {
        clock.set(
            clock
                .get()
                .checked_add(jiff::SignedDuration::from_secs(120))
                .unwrap(),
        );
        clock.get()
    };
    let pass = |first, peak| {
        let passes = Passes {
            subject: "R7 full CCD 0 on 00-07".to_owned(),
            runs: 1,
            first,
            of: 4,
            peak: Some(peak),
            within: None,
        };
        Entry::new(next(), Tag::Pass, What::Passes(passes), Tone::Good)
    };
    let limit = |id: u8, offset| {
        let limits = What::Limits {
            cores: std::iter::once(core(id)).collect(),
            offset: off(offset),
        };
        Entry::new(next(), Tag::Limit, limits, Tone::Good)
    };
    let lines = vec![
        pass(1, 70),
        pass(2, 76),
        pass(3, 62),
        Entry::line(
            next(),
            Tag::Crash,
            "R7 full CCD 0 on 00-07 · rebooted".to_owned(),
            Tone::Bad,
        ),
        pass(4, 60),
        limit(12, -50),
        limit(14, -50),
        limit(3, -34),
    ];
    let mut history = Vec::new();
    for line in lines {
        crate::history::fold_entry(&mut history, line);
    }
    let got: Vec<String> = history
        .iter()
        .map(|e| {
            let minutes = e.at.as_second() / 60;
            format!("{:02}:{:02} {}", minutes / 60, minutes % 60, sentence(e))
        })
        .collect();
    assert_eq!(
        got,
        [
            "00:06 pass: R7 full CCD 0 on 00-07 · trials 1-3 of 4 passed · Tctl max 76°C",
            "00:08 crash: R7 full CCD 0 on 00-07 · rebooted",
            "00:10 pass: R7 full CCD 0 on 00-07 · trial 4 of 4 passed · Tctl max 60°C",
            "00:14 limit: cores 12, 14 solo limit -50, the deepest offset",
            "00:16 limit: core 03 solo limit -34",
        ]
    );
}

fn warnings(h: &mut Hist, count: usize) {
    for i in 0..count {
        h.push(warning(&i.to_string()));
    }
}

fn probe_start(h: &mut Hist, trials: u32) {
    let mut start = hunt_start(1, &[0, 1]);
    start.regime = Regime::R2;
    start.trials = trials;
    start.trial_s = 90;
    h.push(start.into());
}

/// One probe group of `trials` passing trials, in the order a session records them: the group, then each trial.
fn probe_group(group: u32, trials: u32) -> Vec<(Payload, Option<Count>)> {
    let probe_core = u8::try_from((group - 1) % 2).unwrap();
    let mut g = hunt_group(1, group, &[0, 1]);
    g.stage = togi_journal::HuntStage::Probe;
    g.profile = offsets(&[-19, -19, 0]);
    g.probe = Some(Member {
        core: core(probe_core),
        offset: off(-19),
    });
    g.held = vec![Member {
        core: core(1 - probe_core),
        offset: off(-19),
    }];
    let mut events = vec![(Payload::from(g), None)];
    for trial in 1..=trials {
        let id = format!("probe-{group}-{trial}");
        let mut i = intent(&id, Regime::R2, Condition::Parked, &[-19, -19, 0], &[0, 1]);
        i.hunt = Some(nz(1));
        i.group = Some(nz(group));
        i.activity = Some(togi_journal::Activity::Hunt);
        events.push((i.into(), None));
        let mut end = end_payload(&trial_id(&id), Outcome::Pass, None);
        end.tctl_max_c = Some(i32::try_from(60 + group % 20).unwrap());
        events.push((
            end.into(),
            Some(Count {
                passed: trial - 1,
                index: trial,
                of: 0,
            }),
        ));
    }
    events
}

fn play(h: &mut Hist, events: &[(Payload, Option<Count>)], of: u32) {
    for (payload, count) in events {
        if let (Payload::TrialEnd(end), Some(count)) = (payload, count) {
            h.facts.counts.insert(
                end.trial.clone(),
                Count {
                    of,
                    index: count.index.min(of),
                    passed: count.passed.min(of),
                },
            );
        }
        h.push(payload.clone());
    }
}

/// Builds the same journal twice, once keeping 200 entries and once keeping all of them, and checks that
/// retention keeps exactly the newest folded lines of the reference and counts the rest.
fn retained(build: impl Fn(&mut Hist)) -> (Vec<Entry>, usize) {
    let mut kept = Hist::three();
    let mut all = Hist::unbounded_three();
    build(&mut kept);
    build(&mut all);
    let (reference, none) = all.facts.history.recent();
    assert_eq!(none, 0);
    let (entries, dropped) = kept.facts.history.recent();
    assert!(
        entries.len() <= HISTORY_LIMIT,
        "kept {} entries",
        entries.len()
    );
    assert_eq!(
        dropped + entries.len(),
        reference.len(),
        "kept and dropped count every folded line"
    );
    assert_eq!(
        entries,
        reference[..entries.len()],
        "retention keeps the reference's newest folded lines"
    );
    (entries, dropped)
}

#[test]
fn probe_history_before_retention() {
    let steps = |h: &mut Hist| {
        probe_start(h, 1);
        play(h, &probe_group(1, 1), 1);
        play(h, &probe_group(2, 1), 1);
        h.push(hunt_end(1, HuntOutcome::Combination, &[0, 1]).into());
    };
    let mut h = Hist::unbounded_three();
    steps(&mut h);
    assert_eq!(
        h.sentences(),
        [
            "start: session started on 3 cores",
            "hunt: #1 started · candidates 00 01",
            "pass: hunt 1 groups 1-2 · member probes · each 1 passed",
            "hunt: #1 done · 00 01 kept together, probed",
        ]
    );
    let (entries, dropped) = retained(|h| {
        steps(h);
        warnings(h, 396);
    });
    assert_eq!((dropped, entries.len()), (200, 200));
}

#[test]
fn probe_history_across_the_retention_boundary() {
    let (entries, _) = retained(|h| {
        warnings(h, 198);
        probe_start(h, 1);
        play(h, &probe_group(1, 1), 1);
        play(h, &probe_group(2, 1), 1);
        warnings(h, 199);
    });
    assert_eq!(
        entries.last().unwrap().sentence().0,
        "hunt 1 groups 1-2 · member probes · each 1 passed"
    );
}

#[test]
fn probe_history_long_run() {
    let build = |h: &mut Hist| {
        probe_start(h, 1);
        for group in 1..=1205 {
            play(h, &probe_group(group, 1), 1);
        }
        warnings(h, 150);
    };
    let (entries, dropped) = retained(build);
    assert_eq!((dropped, entries.len()), (0, 153));
    assert_eq!(
        entries[150].sentence().0,
        "hunt 1 groups 1-1205 · member probes · each 1 passed"
    );
    let mut bounded = Hist::three();
    probe_start(&mut bounded, 1);
    for group in 1..=1205 {
        play(&mut bounded, &probe_group(group, 1), 1);
        assert!(
            bounded.facts.history.entries.len() <= 2 * HISTORY_LIMIT,
            "periodic compaction keeps the history bounded at group {group}"
        );
    }
}

#[test]
fn probe_history_completes_after_retention() {
    let last = probe_group(3, 2);
    let head = |h: &mut Hist| {
        warnings(h, 396);
        probe_start(h, 2);
        play(h, &probe_group(1, 2), 2);
        play(h, &probe_group(2, 2), 2);
        play(h, &last[..3], 2);
    };
    let (pending, pending_dropped) = retained(head);
    assert_eq!((passes(&pending[0]).runs, passes(&pending[0]).of), (1, 2));
    assert_eq!(
        hunt_groups(&pending[1]).last,
        2,
        "an incomplete probe stays apart"
    );
    let (complete, complete_dropped) = retained(|h| {
        head(h);
        play(h, &last[3..], 2);
    });
    assert_eq!(
        complete[0].sentence().0,
        "hunt 1 groups 1-3 · member probes · each 2 of 2 passed"
    );
    assert_eq!(complete_dropped, pending_dropped);
    assert_eq!(complete.len(), pending.len() - 1);
}

#[test]
fn probe_history_complete_tail_remains_mutable() {
    let last = probe_group(2, 2);
    let head = |h: &mut Hist| {
        warnings(h, 397);
        probe_start(h, 1);
        play(h, &probe_group(1, 1), 1);
        play(h, &last[..3], 1);
    };
    let (complete, dropped) = retained(head);
    assert_eq!((dropped, complete.len()), (200, 200));
    assert_eq!(
        complete[0].sentence().0,
        "hunt 1 groups 1-2 · member probes · each 1 passed"
    );
    let (extra, _) = retained(|h| {
        head(h);
        play(h, &last[3..], 1);
    });
    assert_eq!(
        (hunt_groups(&extra[0]), passes(&extra[0]).runs),
        (Groups::one(1, 2), 2),
        "a later trial of the same group stays its own line"
    );
    assert_eq!(hunt_groups(&extra[1]).last, 1);
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
    h.push(end_payload(&TrialId::from("p1".to_owned()), Outcome::Pass, None).into());
    assert_eq!(h.sentences()[1], "pass: R7 all-core on 00-02 · passed");
}

#[test]
fn a_reset_and_an_mce_have_lines() {
    let mut h = Hist::three();
    h.push(
        togi_journal::Mce {
            cpu: togi_machine::CpuId(3),
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
    assert_eq!(h.sentences()[2], "reset: requested from the command line");
}

fn plain(text: &str) -> Entry {
    Entry::line(at(seq(1)), Tag::Note, text.to_owned(), Tone::Plain)
}

#[test]
fn a_failure_draws_its_own_signal_as_the_alarm() {
    let failure = |signal| {
        let failed = Failed {
            subject: "wrong result rerun of R7 all-core".to_owned(),
            verdict: Verdict {
                signal: Some(signal),
                when: None,
                named: Some(Named::Nobody),
            },
            hunt: None,
            reboot: None,
        };
        Entry::new(at(seq(1)), Tag::Fail, What::Failed(failed), Tone::Bad)
    };
    assert_eq!(
        failure(Signal::ComputationError).sentence(),
        (
            "wrong result rerun of R7 all-core · ".to_owned(),
            "wrong result".to_owned(),
            " · no core named".to_owned()
        )
    );
    assert_eq!(
        failure(Signal::Crash).sentence(),
        (
            "wrong result rerun of R7 all-core · rebooted · no core named".to_owned(),
            String::new(),
            String::new()
        )
    );
}

#[test]
fn lines_merge_only_with_a_line_that_continues_them() {
    let mut history = Vec::new();
    fold_entry(&mut history, plain("first"));
    assert_eq!(history.len(), 1, "the first line has nothing to merge into");

    let group = |first: u32| {
        let answered = What::Answered {
            groups: Groups::one(1, first),
            cores: set(&[0]),
            inferred: Inference::Pass,
        };
        Entry::new(at(seq(1)), Tag::Group, answered, Tone::Hunt)
    };
    fold_entry(&mut history, group(1));
    fold_entry(&mut history, group(2));
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[1].what,
        What::Answered {
            groups: Groups {
                hunt: 1,
                first: 1,
                last: 2
            },
            cores: set(&[0]),
            inferred: Inference::Pass,
        }
    );
    fold_entry(&mut history, group(4));
    assert_eq!(history.len(), 3, "group 4 does not continue group 2");

    let known = Entry::new(
        at(seq(1)),
        Tag::Skip,
        What::KnownFailure(KnownFailure {
            regime: None,
            crashed: false,
            carried: false,
            hunt: None,
        }),
        Tone::Plain,
    );
    fold_entry(&mut history, known.clone());
    fold_entry(&mut history, known);
    assert_eq!(history.len(), 5, "a plain line never merges");
}

fn passes(entry: &Entry) -> &Passes {
    let What::Passes(passes) = &entry.what else {
        panic!("not passes: {entry:?}");
    };
    passes
}

fn hunt_groups(entry: &Entry) -> Groups {
    let Some(PassedIn::Hunt { groups, .. }) = passes(entry).within else {
        panic!("not passes in a hunt: {entry:?}");
    };
    groups
}
