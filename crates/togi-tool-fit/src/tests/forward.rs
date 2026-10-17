use std::collections::{BTreeMap, HashSet};

use togi_facts::Decisive;
use togi_machine::{Regime, WorkloadId};
use togi_sim::MachineDefinition;
use togi_tool_trialfacts::{ProfileClassKey, TrialFact};

use super::{blank_machine, crash, fact_at};
use crate::error::Error;
use crate::forward::{
    ForwardCheck, ForwardCounts, ForwardRow, ForwardScore, R7Ccd, R7Rows, forward_check, log_loss,
    reference_check, report_forward, report_reference, score_forward,
};

const S1: &str = "20260101T000000Z";
const S2: &str = "20260102T000000Z";
const S3: &str = "20260103T000000Z";

/// A 16-core trial of the first workload of R1 loading core 0, the shape the fitter accepts.
pub(super) fn trial(session: &str, seq: u64, ruleset: u32, failing: bool) -> TrialFact {
    let outcome = if failing { crash() } else { Decisive::Pass };
    let mut offsets = [0; 16];
    offsets[0] = -20;
    let mut fact = fact_at(session, seq, &offsets, Regime::R1, 0, &[0], 1, outcome);
    fact.build.ruleset = ruleset;
    fact
}

/// Session 1 has four trials and the others three, the first of each failing; the latest session comes first.
pub(super) fn three_sessions() -> Vec<TrialFact> {
    let mut trials = Vec::new();
    for (session, ruleset, n) in [(S3, 3, 3), (S2, 2, 3), (S1, 1, 4)] {
        for i in 0..n {
            trials.push(trial(session, i + 1, ruleset, i == 0));
        }
    }
    trials
}

#[test]
fn a_held_out_session_is_scored_by_a_fit_of_the_sessions_before_it_only() {
    let trials = three_sessions();
    let check = forward_check(&trials, 0).unwrap();
    let rows = &check.rows;
    let sessions: Vec<String> = rows.iter().map(|row| row.session.to_string()).collect();
    assert_eq!(sessions, [S2, S3]);
    assert_eq!(
        [
            rows[0].training_sessions,
            rows[1].training_sessions,
            rows[0].ruleset as usize,
            rows[1].ruleset as usize
        ],
        [1, 2, 2, 3]
    );
    assert_eq!((check.pooled.counts.trials, check.pooled.matches), (6, 6));
    assert!((rows[0].constant - 0.25).abs() < f64::EPSILON);

    let mut later = trials.clone();
    for fact in later.iter_mut().filter(|f| f.session.to_string() == S3) {
        fact.outcome = crash();
    }
    let changed = forward_check(&later, 0).unwrap();
    assert_eq!(
        rows[0], changed.rows[0],
        "later outcomes changed the earlier held-out row"
    );

    for fact in later.iter_mut().filter(|f| f.session.to_string() == S2) {
        fact.outcome = crash();
    }
    let changed = forward_check(&later, 0).unwrap();
    assert_eq!(
        rows[0].score.counts.predicted.to_bits(),
        changed.rows[0].score.counts.predicted.to_bits(),
        "held-out outcomes changed their own prediction"
    );
}

#[test]
fn sessions_with_a_numeric_suffix_are_ordered_by_the_number() {
    let sessions = [
        S1,
        "20260101T000000Z-2",
        "20260101T000000Z-9",
        "20260101T000000Z-10",
    ];
    let mut trials: Vec<TrialFact> = sessions
        .iter()
        .rev()
        .map(|s| trial(s, 1, 1, false))
        .collect();
    let rows = forward_check(&trials, 0).unwrap().rows;
    let got: Vec<String> = rows.iter().map(|row| row.session.to_string()).collect();
    assert_eq!(got, sessions[1..]);
    trials[0].outcome = crash();
    let changed = forward_check(&trials, 0).unwrap().rows;
    assert_eq!(
        rows[..2],
        changed[..2],
        "a later suffixed session changed earlier rows"
    );
}

#[test]
fn a_sealed_session_is_neither_fitted_nor_scored() {
    let mut trials = Vec::new();
    for (i, session) in [S1, S2, S3, "20260104T000000Z"].into_iter().enumerate() {
        let failing = i % 2 == 1;
        trials.push(trial(session, 1, 1, failing));
        trials.push(trial(session, 2, 1, failing));
    }
    let all = forward_check(&trials, 0).unwrap();
    let sealed = forward_check(&trials, 1).unwrap();
    assert_eq!(all.rows[..2], sealed.rows[..]);
    assert_eq!(
        (sealed.pooled.counts.trials, sealed.pooled.counts.failures),
        (4, 2)
    );
    let error = forward_check(&trials, 3).unwrap_err();
    assert!(matches!(
        error,
        Error::Seal {
            seal: 3,
            held_out: 3
        }
    ));
    assert_eq!(
        error.to_string(),
        "--seal 3 leaves no held-out session to score (3 held out)"
    );
}

#[test]
fn the_log_loss_clamps_the_probability_and_the_constant_predictor_is_scored_alongside() {
    let mut cfg = blank_machine(2);
    cfg.model.past_limit_rate = 0.0;
    cfg.core[0].flat = std::f64::consts::LN_2;
    let mut held_out: Vec<TrialFact> = (0..3).map(|i| trial(S2, i + 1, 1, i < 2)).collect();
    for fact in &mut held_out {
        fact.profile = super::profile(&[-1, 0]);
    }
    held_out[2].profile = super::profile(&[-2, 0]);
    let seen: HashSet<ProfileClassKey> = [ProfileClassKey::from(&held_out[0])].into();
    let (score, breakdown) = score_forward(&cfg, &held_out, &seen, 0.25).unwrap();
    let regimes = breakdown.regimes;
    // Three one-second trials have p = 1 - exp(-ln 2) = 0.5, whatever their measured duration; the constant
    // assigns p = 1/4 to two failures and one pass.
    let counts = ForwardCounts {
        trials: 3,
        failures: 2,
        predicted: 1.5,
        fit_log_loss: 3.0 * std::f64::consts::LN_2,
        constant_log_loss: -2.0 * 0.25_f64.ln() - 0.75_f64.ln(),
    };
    assert_eq!(
        (score.counts.trials, score.counts.failures, score.matches),
        (3, 2, 2)
    );
    assert!((score.counts.predicted - counts.predicted).abs() < 1e-12);
    assert!((score.counts.fit_log_loss - counts.fit_log_loss).abs() < 1e-12);
    assert!((score.counts.constant_log_loss - counts.constant_log_loss).abs() < 1e-12);
    assert_eq!(regimes.len(), 1);
    assert_eq!(regimes[&Regime::R1], score.counts);
    let clamp = -(1e-4_f64).ln();
    let other = -(1.0 - 1e-4_f64).ln();
    for (p, failure, want) in [
        (0.0, true, clamp),
        (1.0, false, clamp),
        (0.0, false, other),
        (1.0, true, other),
    ] {
        assert!(
            (log_loss(p, failure) - want).abs() < 1e-12,
            "p={p} failure={failure}"
        );
    }
}

#[test]
fn the_regime_rows_show_mean_log_loss_in_regime_order() {
    let counts = |trials, failures, predicted, fit, constant| ForwardCounts {
        trials,
        failures,
        predicted,
        fit_log_loss: fit,
        constant_log_loss: constant,
    };
    let row = ForwardRow {
        session: S1.parse().unwrap(),
        ruleset: 1,
        training_sessions: 1,
        constant: 0.5,
        score: ForwardScore {
            counts: counts(3, 0, 0.0, 0.0, 0.0),
            ..ForwardScore::default()
        },
        regimes: BTreeMap::from([
            (Regime::R7, counts(2, 1, 0.5, 1.0, 2.0)),
            (Regime::R1, counts(1, 0, 0.3, 0.3, 0.6)),
        ]),
        r7: R7Rows::default(),
        trials: Vec::new(),
    };
    let check = ForwardCheck {
        rows: vec![row],
        pooled: ForwardScore::default(),
    };
    let mut out = Vec::new();
    report_forward(&mut out, &check, 2).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("Sealed newest sessions: 2 (neither fitted nor scored)\nPooled: "),
        "{text}"
    );
    assert!(
        text.contains(
            "  R1 trials=1 observed=0 predicted=0.3 log_loss/trial fit=0.3000 constant=0.6000\n  R7 trials=2 observed=1 predicted=0.5 log_loss/trial fit=0.5000 constant=1.0000\n"
        ),
        "{text}"
    );
}

#[test]
fn fewer_than_two_sessions_leave_nothing_to_score() {
    let trials = vec![trial(S1, 1, 1, false)];
    let check = forward_check(&trials, 0).unwrap();
    assert!(check.rows.is_empty());
    let mut out = Vec::new();
    report_forward(&mut out, &check, 0).unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("No held-out sessions")
    );
}

fn r7_trial(seq: u64, workload: usize, loaded: &[i64], failing: bool) -> TrialFact {
    let outcome = if failing { crash() } else { Decisive::Pass };
    fact_at(S2, seq, &[0; 16], Regime::R7, workload, loaded, 1, outcome)
}

fn shape(counts: &ForwardCounts) -> (usize, usize) {
    (counts.trials, counts.failures)
}

/// R7 starts of the three catalog workloads (the last is y-cruncher) on one CCD, both CCDs and one core,
/// and an R1 trial that no R7 row may count.
fn r7_starts() -> Vec<TrialFact> {
    vec![
        r7_trial(1, 0, &[0, 1, 2], false),
        r7_trial(2, 0, &[8, 9], true),
        r7_trial(3, 1, &[0, 8], false),
        r7_trial(4, 2, &[3], false),
        r7_trial(5, 2, &[12], true),
        fact_at(S2, 6, &[0; 16], Regime::R1, 0, &[0], 1, crash()),
    ]
}

#[test]
fn r7_starts_are_grouped_by_workload_and_by_loaded_ccd_with_singletons_counted_once_more() {
    let held_out = r7_starts();
    let (score, breakdown) =
        score_forward(&blank_machine(16), &held_out, &HashSet::new(), 0.25).unwrap();
    assert_eq!(shape(&score.counts), (6, 3));
    assert_eq!(shape(&breakdown.regimes[&Regime::R7]), (5, 2));
    let workloads: Vec<(WorkloadId, (usize, usize))> = breakdown
        .r7
        .workloads
        .iter()
        .map(|(id, counts)| (*id, shape(counts)))
        .collect();
    let id = |i| WorkloadId::new(Regime::R7, i).unwrap();
    assert_eq!(
        workloads,
        [(id(0), (2, 1)), (id(1), (1, 0)), (id(2), (2, 1))],
        "y-cruncher is a row of its own"
    );
    let ccds: Vec<(R7Ccd, (usize, usize))> = breakdown
        .r7
        .ccds
        .iter()
        .map(|(ccd, counts)| (*ccd, shape(counts)))
        .collect();
    assert_eq!(
        ccds,
        [
            (R7Ccd::Ccd0, (2, 0)),
            (R7Ccd::Ccd1, (2, 2)),
            (R7Ccd::Both, (1, 0))
        ],
        "a load on both CCDs is its own group, counted in neither CCD row; a singleton is in its CCD's"
    );
    assert_eq!(shape(&breakdown.r7.singleton), (2, 1));
    let rows: usize = breakdown.r7.ccds.values().map(|c| c.trials).sum();
    assert_eq!(rows, breakdown.regimes[&Regime::R7].trials);
}

#[test]
fn the_r7_rows_follow_the_regime_rows_by_workload_then_ccd_then_singleton() {
    let held_out = r7_starts();
    let (score, breakdown) =
        score_forward(&blank_machine(16), &held_out, &HashSet::new(), 0.25).unwrap();
    let check = ForwardCheck {
        rows: vec![ForwardRow {
            session: S2.parse().unwrap(),
            ruleset: 1,
            training_sessions: 1,
            constant: 0.25,
            score,
            regimes: breakdown.regimes,
            r7: breakdown.r7,
            trials: breakdown.trials,
        }],
        pooled: score,
    };
    let mut out = Vec::new();
    report_forward(&mut out, &check, 0).unwrap();
    let text = String::from_utf8(out).unwrap();
    let labels = [
        "  R1 trials=1",
        "  R7 trials=5",
        "  R7 workload mprime-avx2-36k-248k-allcore trials=2 observed=1",
        "  R7 workload mprime-avx512-36k-248k-allcore trials=1 observed=0",
        "  R7 workload ycruncher-fftv4-n63-vt3-allcore trials=2 observed=1",
        "  R7 CCD0 loaded trials=2 observed=0",
        "  R7 CCD1 loaded trials=2 observed=2",
        "  R7 both CCDs loaded trials=1 observed=0",
        "  R7 singleton (also in its CCD row) trials=2 observed=1",
        "Pooled: ",
    ];
    let at: Vec<usize> = labels
        .iter()
        .map(|label| {
            text.find(label)
                .unwrap_or_else(|| panic!("{label}: {text}"))
        })
        .collect();
    assert!(at.is_sorted(), "{text}");
}

#[test]
fn the_reference_ensemble_scores_each_trial_with_the_mean_probability_of_all_members() {
    let member = |p: f64| {
        let mut cfg = blank_machine(2);
        cfg.model.past_limit_rate = 0.0;
        cfg.core[0].flat = -(1.0 - p).ln();
        cfg
    };
    let mut trials = vec![trial(S1, 1, 1, false), trial(S1, 2, 1, false)];
    trials.extend((0..3).map(|i| trial(S2, i + 1, 2, i == 0)));
    for fact in &mut trials {
        fact.profile = super::profile(&[-1, 0]);
    }
    let check = reference_check(&trials, 0, &[member(0.5), member(0.1)]).unwrap();
    assert_eq!(check.rows.len(), 1);
    let row = &check.rows[0];
    assert_eq!(
        (row.session.to_string().as_str(), row.training_sessions),
        (S2, 1)
    );
    // The mean of 0.5 and 0.1 is 0.3 for each of the three one-second trials, one of which failed.
    let c = &row.score.counts;
    assert_eq!(shape(c), (3, 1));
    assert!((c.predicted - 0.9).abs() < 1e-12);
    let want = -(0.3_f64).ln() - 2.0 * (0.7_f64).ln();
    assert!((c.fit_log_loss - want).abs() < 1e-12);
    assert_eq!(check.pooled.counts, *c);
    assert_eq!((row.score.flagged, row.score.eligible), (0, 0));
    assert!(
        matches!(
            reference_check(&trials, 1, &[member(0.5)]),
            Err(Error::Seal { .. })
        ),
        "the reference keeps the forward check's seal"
    );
    assert!(reference_check(&trials, 0, &[]).is_err());

    let mut out = Vec::new();
    report_reference(&mut out, &check, 0, 2).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("(2 members, committed, not fitted, not a gate)")
            && text.contains("the mean over all 2 members")
            && text.contains("flagged_by_any_member/eligible=0/0"),
        "{text}"
    );
}

/// Ten held-out one-second R1 trials loading `core` alone at offset -1, the first `failures` failing,
/// numbered from `first`: one model-check group, just eligible.
fn eligible_group(core: usize, failures: u64, first: u64) -> Vec<TrialFact> {
    let mut offsets = [0; 16];
    offsets[core] = -1;
    let loaded = [i64::try_from(core).unwrap()];
    (0..10)
        .map(|i| {
            let outcome = if i < failures {
                crash()
            } else {
                Decisive::Pass
            };
            fact_at(S2, first + i, &offsets, Regime::R1, 0, &loaded, 1, outcome)
        })
        .collect()
}

#[test]
fn a_reference_group_counts_once_when_any_member_flags_it_in_any_member_order() {
    // A blank machine whose only hazard is a loaded core's flat rate: a one-second trial loading core
    // 0, 1 or 2 alone fails with probability p[0], p[1] or p[2].
    let member = |p: [f64; 3]| {
        let mut cfg = blank_machine(16);
        cfg.model.past_limit_rate = 0.0;
        for (core, p) in p.into_iter().enumerate() {
            cfg.core[core].flat = -(1.0 - p).ln();
        }
        cfg
    };
    // Core 0 failed 10 of 10: Binomial(10, 0.4) puts 1e-4 on that count and flags it, Binomial(10, 0.99)
    // puts 0.90 and does not. Core 1 failed 0 of 10, which both 0.9 and 0.6 flag. Core 2 failed 5 of 10,
    // which neither flags at 0.5. Every member groups the same held-out trials: the same three keys.
    let x = member([0.4, 0.9, 0.5]);
    let y = member([0.99, 0.6, 0.5]);
    let mut trials = vec![trial(S1, 1, 1, false)];
    trials.extend(eligible_group(0, 10, 1));
    trials.extend(eligible_group(1, 0, 11));
    trials.extend(eligible_group(2, 5, 21));
    let groups = |members: &[MachineDefinition]| {
        let check = reference_check(&trials, 0, members).unwrap();
        assert_eq!(check.rows.len(), 1);
        let row = &check.rows[0].score;
        assert_eq!(shape(&row.counts), (30, 15));
        assert_eq!(
            (check.pooled.flagged, check.pooled.eligible),
            (row.flagged, row.eligible)
        );
        (row.flagged, row.eligible)
    };
    assert_eq!(
        groups(std::slice::from_ref(&x)),
        (2, 3),
        "x flags cores 0 and 1"
    );
    assert_eq!(
        groups(std::slice::from_ref(&y)),
        (1, 3),
        "y flags core 1 only"
    );
    assert_eq!(
        groups(&[member([0.695, 0.75, 0.5])]),
        (1, 3),
        "the mean prediction of x and y flags core 1 only"
    );
    for members in [
        vec![x.clone(), y.clone()],
        vec![y.clone(), x.clone()],
        vec![y.clone(), x.clone(), y.clone()],
    ] {
        assert_eq!(
            groups(&members),
            (2, 3),
            "core 0 flagged by x alone and core 1 by both count once each, core 2 is eligible only"
        );
    }

    let check = reference_check(&trials, 0, &[y, x]).unwrap();
    let mut out = Vec::new();
    report_reference(&mut out, &check, 0, 2).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        text.matches("flagged_by_any_member/eligible=2/3").count(),
        2,
        "the row and the pool: {text}"
    );
}
