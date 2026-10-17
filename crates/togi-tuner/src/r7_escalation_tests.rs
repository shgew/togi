use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use togi_journal::{
    Activity, Attribution, CorePhase, CoreTransition, Decision, Outcome, Payload, Seq,
    TunerDecision,
};
use togi_machine::{
    BootId, CcdId, Condition, CoreInfo, CoreSet, PerCore, Regime, Signal, WorkloadId, workloads,
};

use crate::r7::escalation::{BackoffKind, ESCALATE_AFTER, Escalation, Strikes};
use crate::r7::reason::{Note, R7Reason};
use crate::r7::respond::{R7Response, R7Route};
use crate::testkit::{Journal, R7Report, Start, build, fresh, infos, pairs, set};
use crate::tests::{core, off};
use crate::{Action, DecisionPayload, Load, LoadKey};

#[derive(Clone, Copy)]
struct EscalationLoad {
    workload: usize,
    cores: &'static [u8],
    top: u8,
}

fn escalation_load(op: char) -> EscalationLoad {
    match op.to_ascii_lowercase() {
        'a' => EscalationLoad {
            workload: 0,
            cores: &[0, 1],
            top: 0,
        },
        'b' => EscalationLoad {
            workload: 1,
            cores: &[0, 1],
            top: 0,
        },
        'c' => EscalationLoad {
            workload: 0,
            cores: &[2, 3],
            top: 2,
        },
        other => panic!("no load {other}"),
    }
}

fn r7_workload(index: usize) -> WorkloadId {
    workloads(Regime::R7)[index].id()
}

fn load_key(op: char) -> LoadKey {
    let load = escalation_load(op);
    LoadKey::new(r7_workload(load.workload), set(load.cores))
}

#[derive(Default)]
struct Played {
    failures: Vec<Seq>,
    backoffs: Vec<Seq>,
    frozen: Vec<u32>,
}

impl Played {
    fn last_failure(&self) -> Seq {
        *self.failures.last().expect("a failure")
    }
}

impl Journal {
    fn values(&self) -> Vec<i64> {
        self.offsets()
            .iter()
            .map(|(_, offset)| offset.get())
            .collect()
    }

    fn load_trial(&mut self, load: EscalationLoad, outcome: Outcome) -> Seq {
        self.load_trial_topped(load, outcome, &[load.top])
    }

    fn load_trial_topped(&mut self, load: EscalationLoad, outcome: Outcome, tops: &[u8]) -> Seq {
        let values = self.values();
        let plan = self.manual(
            Regime::R7,
            load.workload,
            Load::Many(set(load.cores)),
            Condition::Together,
            &values,
            120,
        );
        let (intent, trial) = self.intent(&plan);
        let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
        let mut end = Self::end_payload(&trial, outcome, signal);
        if outcome == Outcome::Failure {
            end.duration_s = 41;
            end.top_requesters = set(tops);
        } else {
            end.duration_s = 120;
        }
        self.append(end.into(), &[intent])
    }

    fn skip_phases(&mut self) -> Option<Action> {
        loop {
            let action = self.state.next().ok()?;
            match &action {
                Action::Decide(decision)
                    if matches!(decision.payload, DecisionPayload::CorePhase(_)) =>
                {
                    self.decide(decision.clone());
                }
                Action::Decide(_) | Action::RunTrial(_) | Action::ReadRanking => {
                    return Some(action);
                }
            }
        }
    }

    fn apply_backoff(&mut self) -> Seq {
        let Some(Action::Decide(decision)) = self.skip_phases() else {
            panic!("expected a backoff decision");
        };
        assert!(
            matches!(&decision.payload, DecisionPayload::TunerDecision(td) if td.decision == Decision::Backoff),
            "expected a backoff, got {decision:?}"
        );
        let seq = self.decide(decision);
        let values = self.values();
        self.profile_change(&values);
        seq
    }

    /// A lower-case op passes its load; an upper-case op fails it unattributed and, unless it is
    /// the last op, applies the backoff the tuner requires.
    fn play(&mut self, ops: &str) -> Played {
        let mut played = Played::default();
        let last = ops.len() - 1;
        for (at, op) in ops.chars().enumerate() {
            let load = escalation_load(op);
            if op.is_ascii_lowercase() {
                self.load_trial(load, Outcome::Pass);
                continue;
            }
            self.load_trial(load, Outcome::Failure);
            let Some(Action::Decide(attribution)) = self.skip_phases() else {
                panic!("op {at} ({op}): expected the attribution");
            };
            assert!(
                matches!(&attribution.payload, DecisionPayload::Failure(f) if f.attribution == Attribution::Unattributed),
                "op {at} ({op}): attribution {attribution:?}"
            );
            let failure = self.decide(attribution);
            played.failures.push(failure);
            played.frozen.push(self.frozen_backoffs(failure));
            if at == last {
                break;
            }
            played.backoffs.push(self.apply_backoff());
        }
        played
    }

    fn place(&mut self, id: u8, offset: i64) -> Seq {
        self.core_phase(CoreTransition {
            core: core(id),
            from: None,
            to: CorePhase::HasRoom,
            offset: off(offset),
            pass: None,
            failure_point: None,
            check_solo_limit: false,
            workloads: Vec::new(),
            cleared_combination: Vec::new(),
            reason: "test".to_owned(),
        })
    }

    fn strikes(&self, op: char) -> u32 {
        self.run().r7.strikes.failures(load_key(op))
    }

    fn frozen_backoffs(&self, failure: Seq) -> u32 {
        self.run()
            .r7
            .open
            .get(&failure)
            .expect("a frozen failed trial")
            .r7
            .backoffs
            .failures()
    }

    fn r7_backoff(&self) -> R7Reason {
        match self.run().r7_decision() {
            Some(R7Response::Backoff { why, decision }) => {
                assert_eq!(self.next_decision(), decision);
                why
            }
            other => panic!("expected a voltage-targeted backoff, got {other:?}"),
        }
    }

    fn located(&self) -> (Seq, Escalation, Vec<Seq>) {
        assert_eq!(self.run().r7_decision(), None);
        match self.run().locate_due() {
            Some(R7Route::Locate { failure, reason }) => {
                assert_eq!(reason.rerun_passed, None);
                let cause = reason.cause();
                (failure, reason.escalation.expect("an escalation"), cause)
            }
            other => panic!("expected a located failure, got {other:?}"),
        }
    }
}

fn ordinal(why: &R7Reason) -> u32 {
    why.notes
        .iter()
        .find_map(|note| {
            if let Note::Ordinal { n, after } = note {
                assert_eq!(*after, ESCALATE_AFTER);
                Some(*n)
            } else {
                None
            }
        })
        .expect("an ordinal note")
}

#[test]
fn escalation_boundaries() {
    type Case = (&'static str, &'static str, Option<&'static [usize]>, u32);
    let cases: [Case; 11] = [
        ("first failure of a load backs off", "A", None, 1),
        ("one backoff is not enough", "AA", None, 2),
        (
            "two backoffs escalate the next failure",
            "AAA",
            Some(&[0, 1]),
            0,
        ),
        ("a pass resets the count", "AAaA", None, 1),
        (
            "a pass resets it again after two more backoffs",
            "AAaAA",
            None,
            2,
        ),
        (
            "two backoffs after a pass escalate",
            "AAaAAA",
            Some(&[2, 3]),
            0,
        ),
        ("another workload counts separately", "AAB", None, 1),
        ("other loaded cores count separately", "AAC", None, 1),
        (
            "alternating loads keep separate counts",
            "ABABA",
            Some(&[0, 2]),
            0,
        ),
        ("alternating loads stay below the count", "ABAB", None, 2),
        (
            "a pass of another load does not reset",
            "AAbA",
            Some(&[0, 1]),
            0,
        ),
    ];
    for (name, ops, cited, count) in cases {
        let mut j = Journal::r7_session();
        let played = j.play(ops);
        if let Some(cited) = cited {
            let (failure, escalation, cause) = j.located();
            assert_eq!(failure, played.last_failure(), "{name}");
            let decisions: Vec<Seq> = cited.iter().map(|&at| played.backoffs[at]).collect();
            assert_eq!(
                escalation,
                Escalation::Backoffs {
                    failures: ESCALATE_AFTER,
                    decisions: decisions.clone(),
                },
                "{name}"
            );
            assert_eq!(cause, decisions, "{name}");
        } else {
            let why = j.r7_backoff();
            assert_eq!(why.failure, played.last_failure(), "{name}");
            let last = ops.chars().last().unwrap();
            assert_eq!(why.core, core(escalation_load(last).top), "{name}");
            assert_eq!(ordinal(&why), count, "{name}");
        }
        let resumed = j.resumed();
        assert_eq!(resumed.run().r7_decision(), j.run().r7_decision(), "{name}");
        assert_eq!(resumed.run().locate_due(), j.run().locate_due(), "{name}");
        j.assert_resumes();
    }
}

#[test]
fn escalation_count_survives_resume() {
    let mut j = Journal::r7_session().resuming_every(1);
    j.play("AAaA");
    assert_eq!(ordinal(&j.r7_backoff()), 1);
    j.apply_backoff();
    let mut j = j.resumed().resuming_every(1);
    assert_eq!(j.strikes('a'), 1);
    j.play("A");
    assert_eq!(ordinal(&j.r7_backoff()), 2);
    j.apply_backoff();
    let mut j = j.resumed().resuming_every(1);
    assert_eq!(j.strikes('a'), 2);
    let played = j.play("A");
    let (failure, escalation, _) = j.located();
    assert_eq!(failure, played.last_failure());
    assert!(matches!(
        escalation,
        Escalation::Backoffs { failures: 2, .. }
    ));
    assert_eq!(
        fresh(&j.events)
            .run()
            .unwrap()
            .r7
            .strikes
            .failures(load_key('a')),
        2
    );
    assert_eq!(
        fresh(&j.events).run().unwrap().r7.strikes,
        j.run().r7.strikes
    );
}

#[test]
fn an_escalated_failure_reads_the_ranking_before_it_is_located() {
    for chosen_shallower in [false, true] {
        let ids = [0, 1, 2, 3];
        let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-30); 4]);
        j.profile_change(&[-30, -30, -30, -30]);
        j.play("AA");
        j.apply_backoff();
        let failed = j.values();
        j.load_trial_topped(escalation_load('a'), Outcome::Failure, &[0, 1]);
        let Some(Action::Decide(attribution)) = j.skip_phases() else {
            panic!("expected the attribution");
        };
        let failure = j.decide(attribution);
        assert_eq!(j.skip_phases(), Some(Action::ReadRanking));
        assert_eq!(j.run().r7_decision(), Some(R7Response::ReadRanking));
        assert_eq!(j.run().locate_due(), None);
        j.ranking(&[0, 1, 2, 3]);
        if chosen_shallower {
            j.place(1, failed[1] + 1);
            assert_eq!(j.run().r7_decision(), None);
            assert_eq!(j.run().locate_due(), None);
            assert!(j.run().r7.open.contains_key(&failure));
            j.place(1, failed[1]);
        }
        let (located, escalation, _) = j.located();
        assert_eq!(located, failure, "{chosen_shallower}");
        assert!(matches!(
            escalation,
            Escalation::Backoffs { failures: 2, .. }
        ));
        j.assert_resumes();
    }
}

#[test]
fn an_escalated_failure_whose_chosen_core_is_shallower_stays_pending_without_a_hunt() {
    let mut j = Journal::r7_session();
    let played = j.play("AAA");
    let failed = j.values();
    let (failure, _, _) = j.located();
    assert_eq!(failure, played.last_failure());

    let mut shallower = failed.clone();
    shallower[0] += 1;
    j.place(0, shallower[0]);
    assert_eq!(j.values()[1], failed[1]);
    assert_eq!(j.run().r7_decision(), None);
    assert_eq!(j.run().locate_due(), None);
    assert!(j.run().r7.open.contains_key(&failure));
    j.assert_resumes();

    j.place(0, failed[0]);
    let (again, _, _) = j.located();
    assert_eq!(again, failure);
    j.assert_resumes();
}

#[test]
fn escalation_when_loaded_cores_cannot_back_off() {
    let mut j = Journal::r7_session();
    j.place(0, 0);
    j.place(1, 0);
    j.profile_change(&[0, 0, -30, -30]);
    let played = j.play("A");
    let (failure, escalation, cause) = j.located();
    assert_eq!(failure, played.last_failure());
    assert_eq!(escalation, Escalation::LoadedAtZero);
    assert!(cause.is_empty());
    j.assert_resumes();

    let mut j = Journal::r7_session();
    j.place(0, 0);
    j.profile_change(&[0, -30, -30, -30]);
    j.play("A");
    assert_eq!(j.run().locate_due(), None);
    let why = j.r7_backoff();
    assert_eq!(why.core, core(1));
    assert!(
        why.notes
            .iter()
            .any(|note| matches!(note, Note::SteppedDown { group, .. } if *group == set(&[1])))
    );
}

#[test]
fn escalation_reset_drops_discarded_backoffs() {
    for (name, reset, hunt) in [
        ("reset of a loaded core restarts the count", 0u8, false),
        ("reset of a core outside the load keeps the count", 3, true),
    ] {
        let mut j = Journal::r7_session();
        j.play("AA");
        j.apply_backoff();
        let offsets = j.values();
        j.reset(reset);
        for _ in 0..4 {
            match j.state.next().ok() {
                Some(Action::Decide(decision))
                    if matches!(decision.payload, DecisionPayload::CorePhase(_)) =>
                {
                    j.decide(decision);
                }
                _ => break,
            }
        }
        j.place(reset, offsets[usize::from(reset)]);
        j.profile_change(&offsets);
        j.play("A");
        if hunt {
            let (_, escalation, _) = j.located();
            assert!(
                matches!(escalation, Escalation::Backoffs { failures: 2, .. }),
                "{name}"
            );
        } else {
            assert_eq!(j.strikes('a'), 0, "{name}");
            assert_eq!(ordinal(&j.r7_backoff()), 1, "{name}");
        }
        let resumed = j.resumed();
        assert_eq!(resumed.run().r7_decision(), j.run().r7_decision(), "{name}");
        assert_eq!(resumed.run().locate_due(), j.run().locate_due(), "{name}");
        j.assert_resumes();
    }
}

#[test]
fn escalation_counts_per_load_and_resets_on_that_loads_pass_only() {
    let mut j = Journal::r7_session();
    let played = j.play("AACCaC");
    assert_eq!(j.strikes('a'), 0);
    assert_eq!(j.strikes('c'), 2);
    let (failure, escalation, _) = j.located();
    assert_eq!(failure, played.last_failure());
    assert_eq!(
        escalation,
        Escalation::Backoffs {
            failures: 2,
            decisions: played.backoffs[2..4].to_vec(),
        }
    );

    let mut j = Journal::r7_session();
    j.play("AACCcA");
    assert_eq!(j.strikes('a'), 2);
    assert_eq!(j.strikes('c'), 0);
    let (_, escalation, _) = j.located();
    assert!(matches!(
        escalation,
        Escalation::Backoffs { failures: 2, .. }
    ));

    let mut j = Journal::r7_session();
    j.play("AACCaA");
    assert_eq!(ordinal(&j.r7_backoff()), 1);
    j.assert_resumes();
}

#[test]
fn escalation_snapshot_is_frozen_when_the_failure_folds() {
    let mut j = Journal::r7_session();
    let first = j.play("AA");
    assert_eq!(first.frozen, [0, 1]);
    j.apply_backoff();
    assert_eq!(j.strikes('a'), 2);
    let third = j.play("A");
    let f3 = third.last_failure();
    assert_eq!(j.frozen_backoffs(f3), 2);
    let before = j.located();
    j.load_trial(escalation_load('a'), Outcome::Pass);
    assert_eq!(j.strikes('a'), 0);
    assert_eq!(j.frozen_backoffs(f3), 2);
    if let Some(R7Route::Locate { reason, .. }) = j.run().locate_due() {
        assert_eq!(reason.escalation, Some(before.1));
    }
    j.assert_resumes();
}

#[test]
fn a_load_that_passed_with_its_loaded_cores_as_deep_locates_its_failure_at_once() {
    type Case = (&'static str, char, [i64; 4], usize, bool);
    let cases: [Case; 6] = [
        (
            "an unloaded core shallower",
            'a',
            [-30, -30, -20, -30],
            0,
            true,
        ),
        (
            "loaded cores deeper too",
            'a',
            [-32, -31, -30, -10],
            0,
            true,
        ),
        (
            "a loaded core shallower",
            'a',
            [-30, -29, -20, -30],
            0,
            false,
        ),
        ("the profile it failed at", 'a', [-30; 4], 0, false),
        ("one pass short", 'a', [-30, -30, -20, -30], 1, false),
        (
            "another workload's passes",
            'b',
            [-30, -30, -20, -30],
            0,
            false,
        ),
    ];
    for (name, op, passed, short, located) in cases {
        let mut j = Journal::r7_session();
        for (id, offset) in (0..).zip(passed) {
            j.place(id, offset);
        }
        let n = usize::from(j.run().config.needed.get());
        let passes: Vec<Seq> = (0..n - short)
            .map(|_| j.load_trial(escalation_load(op), Outcome::Pass))
            .collect();
        for id in 0..4 {
            j.place(id, -30);
        }
        let played = j.play("A");
        if located {
            let (failure, escalation, cause) = j.located();
            assert_eq!(failure, played.last_failure(), "{name}");
            assert_eq!(
                escalation,
                Escalation::Covered {
                    passes: passes.clone()
                },
                "{name}"
            );
            assert_eq!(cause, passes, "{name}");
        } else {
            assert_eq!(j.r7_backoff().failure, played.last_failure(), "{name}");
        }
        j.assert_resumes();
    }
}

#[test]
fn coverage_is_frozen_when_the_failure_folds() {
    let mut j = Journal::r7_session();
    let n = usize::from(j.run().config.needed.get());
    let played = j.play("A");
    j.place(2, -20);
    for _ in 0..n {
        j.load_trial(escalation_load('a'), Outcome::Pass);
    }
    j.place(2, -30);
    assert_eq!(j.r7_backoff().failure, played.last_failure());
    j.assert_resumes();
}

/// A journal whose load passed `count` times at offsets with an unloaded core shallower, then
/// failed unattributed at `[-30; 4]`, so the failure folds already covered by the default count.
fn covered_failure(count: usize) -> (Journal, Vec<Seq>, Seq) {
    let mut j = Journal::r7_session();
    for (id, offset) in (0..).zip([-30, -30, -20, -30]) {
        j.place(id, offset);
    }
    let passes: Vec<Seq> = (0..count)
        .map(|_| j.load_trial(escalation_load('a'), Outcome::Pass))
        .collect();
    for id in 0..4 {
        j.place(id, -30);
    }
    let failure = j.play("A").last_failure();
    (j, passes, failure)
}

/// Passes of the failed load that cover its failure's profile, folded after the failure.
fn pass_after_the_fold(j: &mut Journal, count: usize) {
    j.place(2, -20);
    for _ in 0..count {
        j.load_trial(escalation_load('a'), Outcome::Pass);
    }
    j.place(2, -30);
}

#[test]
fn a_covered_failure_cites_the_earliest_passes_of_the_current_needed_count() {
    let bound = usize::from(togi_config::TrialCount::MAX);
    let at_bound = format!(
        r#"{{"evidence":{{"miss":{:e},"rate":0.5}}}}"#,
        0.5_f64.powf(f64::from(togi_config::TrialCount::MAX) - 0.5)
    );
    let cases: [(&str, String, usize, std::cmp::Ordering); 3] = [
        (
            "a larger count",
            r#"{"evidence":{"miss":0.01,"rate":0.5}}"#.to_owned(),
            20,
            std::cmp::Ordering::Greater,
        ),
        (
            "a smaller count",
            r#"{"evidence":{"miss":0.2,"rate":0.5}}"#.to_owned(),
            20,
            std::cmp::Ordering::Less,
        ),
        (
            "the supported bound",
            at_bound,
            bound + 1,
            std::cmp::Ordering::Greater,
        ),
    ];
    for (name, config, count, want) in cases {
        let (mut j, passes, failure) = covered_failure(count);
        let before = usize::from(j.run().config.needed.get());
        pass_after_the_fold(&mut j, 3);
        j.load(&crate::testkit::config(&config));
        let after = usize::from(j.run().config.needed.get());
        assert_eq!(after.cmp(&before), want, "{name}");
        assert!(after <= passes.len() && after <= bound, "{name}");
        if name == "the supported bound" {
            assert_eq!(after, bound, "{name}");
        }
        let (located, escalation, cause) = j.located();
        assert_eq!(located, failure, "{name}");
        assert_eq!(
            escalation,
            Escalation::Covered {
                passes: passes[..after].to_vec()
            },
            "{name}"
        );
        assert_eq!(cause, passes[..after], "{name}");
        j.assert_resumes();
    }
}

#[test]
fn passes_after_the_fold_never_lift_a_covered_failure_over_a_larger_count() {
    let (mut j, passes, failure) = covered_failure(6);
    let before = usize::from(j.run().config.needed.get());
    assert!(before <= passes.len());
    pass_after_the_fold(&mut j, 5);
    j.load(&crate::testkit::config(
        r#"{"evidence":{"miss":0.01,"rate":0.5}}"#,
    ));
    let after = usize::from(j.run().config.needed.get());
    assert!(after > passes.len());
    match j.run().r7_decision() {
        Some(R7Response::Backoff { why, .. }) => assert_eq!(why.failure, failure),
        other => panic!("expected a voltage-targeted backoff, got {other:?}"),
    }
    j.assert_resumes();
}

#[test]
fn a_carried_failure_its_loaded_cores_passed_as_deep_needs_no_move() {
    for covered in [true, false] {
        let mut j = Journal::r7_session();
        let n = usize::from(j.run().config.needed.get());
        let top = R7Report::default().top(&[0]);
        if covered {
            for _ in 0..n {
                j.r7_fact(true, &[0, 1], &[-30, -30, -20, -30], &top);
            }
        }
        let failure = j.r7_fact(false, &[0, 1], &[-30; 4], &top);
        if covered {
            assert_eq!(j.run().r7_decision(), None);
            assert_eq!(j.run().locate_due(), None);
            assert!(j.run().r7.open.contains_key(&failure));
        } else {
            assert_eq!(j.r7_backoff().failure, failure);
        }
        j.assert_resumes();
    }
}

#[test]
fn deepening_hold_and_named_backoffs_are_never_counted() {
    let load = load_key('a');
    let (decision, failure) = (Seq::new(2).unwrap(), Seq::new(1).unwrap());
    for kind in [
        BackoffKind::Unattributed,
        BackoffKind::Named,
        BackoffKind::LocatedNamed,
        BackoffKind::Deepening,
        BackoffKind::Hold,
    ] {
        let mut strikes = Strikes::default();
        strikes.backoff(load, kind, decision, failure);
        let counted = kind == BackoffKind::Unattributed;
        assert_eq!(kind.counts(), counted, "{kind:?}");
        assert_eq!(strikes.failures(load), u32::from(counted), "{kind:?}");
    }

    let mut j = Journal::r7_session();
    for _ in 0..2 {
        let values = j.values();
        j.r7_failed(&[0, 1], &values, &R7Report::default().top(&[0]).named(0));
        j.apply_backoff();
    }
    assert_eq!(j.strikes('a'), 0);
    j.play("A");
    assert_eq!(ordinal(&j.r7_backoff()), 1);

    let mut j = Journal::r7_session();
    for _ in 0..2 {
        let played = j.play("A");
        let from = j.offsets().get(core(0)).unwrap();
        let to = off(from.get() + 1);
        j.append(
            TunerDecision {
                core: core(0),
                activity: Activity::Deepening,
                decision: Decision::Backoff,
                from_offset: from,
                to_offset: to,
                pass: None,
                failure_point: Some(from),
                workloads: Vec::new(),
                reason: "test".to_owned(),
            }
            .into(),
            &[played.last_failure()],
        );
        let values = j.values();
        j.profile_change(&values);
    }
    assert_eq!(j.strikes('a'), 0);
    j.play("A");
    assert_eq!(ordinal(&j.r7_backoff()), 1);
    j.assert_resumes();
}

#[test]
fn r7_status_separates_workloads_offsets_and_top_requesters() {
    let mut j = Journal::on(
        &[3, 7],
        infos(&[3, 7]),
        &[Start::at_limit(-20), Start::at_limit(-30)],
    );
    j.r7_fact(true, &[3, 7], &[-21, -30], &R7Report::default().top(&[3]));
    j.r7_fact(true, &[3, 7], &[-19, -29], &R7Report::default().top(&[7]));
    let statuses = j.state.r7_status();
    assert!(!statuses.is_empty());
    for status in &statuses {
        let want = status.workload == r7_workload(0) && status.core == core(3);
        assert_eq!(status.self_sufficient, want, "{status:?}");
        assert_eq!(status.passes, u32::from(want), "{status:?}");
        assert_eq!(status.top_requester, status.core == core(3), "{status:?}");
        assert!(status.offset_proxy, "{status:?}");
    }
    j.reset(3);
    for status in j.state.r7_status() {
        assert!(!status.self_sufficient, "{status:?}");
        assert_eq!(status.passes, 0, "{status:?}");
    }
}

#[test]
fn r7_status_ignores_failed_starts_as_top_requester() {
    for carried in [false, true] {
        let mut j = Journal::r7_session();
        let values = j.values();
        let report = R7Report::default().top(&[0]);
        if carried {
            j.r7_fact(
                false,
                &[0, 1],
                &values,
                &report.requests(&[(0, 1.1), (1, 1.08)]),
            );
        } else {
            j.r7_trial(&[0, 1], &values, Outcome::Failure, &report);
        }
        for status in j.state.r7_status() {
            if status.workload == r7_workload(0) && status.core == core(0) {
                assert_eq!(status.passes, 0, "carried {carried}: {status:?}");
                assert!(!status.self_sufficient, "carried {carried}: {status:?}");
            }
        }
    }
}

#[test]
fn r7_status_uses_sorted_profile_and_only_ccd_parts() {
    let cores: Vec<CoreInfo> = [(7, 1), (3, 0), (9, 1), (5, 0)]
        .into_iter()
        .map(|(id, ccd)| CoreInfo {
            core: core(id),
            ccd: CcdId(ccd),
            cpus: Vec::new(),
        })
        .collect();
    let j = Journal::on(
        &[7, 3, 9, 5],
        cores,
        &[
            Start::at_limit(-25),
            Start::at_limit(-20),
            Start::at_limit(-35),
            Start::at_limit(-30),
        ],
    );
    let statuses = j.state.r7_status();
    assert_eq!(statuses.len(), 4 * workloads(Regime::R7).len());
    let mut seen = BTreeSet::new();
    for status in &statuses {
        assert!(
            seen.insert((status.workload, status.core)),
            "duplicate {status:?}"
        );
        assert_eq!(
            status.top_requester,
            status.core == core(3) || status.core == core(7),
            "{status:?}"
        );
    }
}

#[test]
fn r7_status_empty_without_topology() {
    let mut j = Journal::bare(&[3, 7]);
    let seq = j.next_seq();
    let profile: PerCore<_> = [(core(3), off(-20)), (core(7), off(-30))]
        .into_iter()
        .collect();
    let payload: Payload = togi_journal::TrialCarried {
        source: togi_journal::FactSource {
            session: "20230101T000000Z".parse().unwrap(),
            seq,
            build: build(),
            trial: None,
            time: Timestamp::from_second(1_600_000_000).unwrap(),
            boot: BootId::from("old".to_owned()),
            backend: None,
        },
        class: togi_journal::TrialClass {
            regime: Regime::R7,
            workload: Some(r7_workload(0)),
            cores: set(&[3, 7]),
            duration_s: 120,
        },
        condition: Condition::Together,
        activity: None,
        rerun: false,
        profile,
        outcome: Outcome::Pass,
        signal: None,
        duration_s: 120,
        core: None,
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
    .into();
    let _ = j.try_append(payload, &[]);
    assert!(j.state.r7_status().is_empty());
}

#[test]
fn r7_status_cost() {
    fn footprint(j: &Journal) -> usize {
        format!("{:?}{:?}", j.run().r7.status, j.run().r7.measurements).len()
    }
    fn fold_passes(j: &mut Journal, from: usize, to: usize) {
        let loads: [&[u8]; 3] = [&[0, 1], &[2, 3], &[0, 1, 2, 3]];
        let values = j.values();
        for at in from..to {
            let cores = loads[at % loads.len()];
            let top = [cores[0]];
            let requests: Vec<(u8, f64)> = cores.iter().map(|&id| (id, 1.1)).collect();
            j.r7_fact(
                true,
                cores,
                &values,
                &R7Report::default().top(&top).requests(&requests),
            );
        }
    }
    let mut j = Journal::r7_session();
    fold_passes(&mut j, 0, 100);
    let small = footprint(&j);
    let rows = j.state.r7_status().len();
    fold_passes(&mut j, 100, 10_000);
    assert!(
        footprint(&j) <= small + 128,
        "status state grew with passes: {small} -> {}",
        footprint(&j)
    );
    let statuses = j.state.r7_status();
    assert_eq!(statuses.len(), rows);
    assert_eq!(rows, 4 * workloads(Regime::R7).len());
    let core0 = statuses
        .iter()
        .find(|status| status.workload == r7_workload(0) && status.core == core(0))
        .unwrap();
    assert_eq!(core0.passes, 6_667);
}
