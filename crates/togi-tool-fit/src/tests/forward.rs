use std::collections::{BTreeMap, HashSet};

use togi_facts::Decisive;
use togi_machine::Regime;
use togi_tool_trialfacts::{ProfileClassKey, TrialFact};

use super::{blank_machine, crash, fact_at};
use crate::error::Error;
use crate::forward::{
    ForwardCheck, ForwardCounts, ForwardRow, ForwardScore, forward_check, log_loss, report_forward,
    score_forward,
};

const S1: &str = "20260101T000000Z";
const S2: &str = "20260102T000000Z";
const S3: &str = "20260103T000000Z";

fn trial(session: &str, seq: u64, ruleset: u32, failing: bool) -> TrialFact {
    let outcome = if failing { crash() } else { Decisive::Pass };
    let mut fact = fact_at(session, seq, &[-20, 0], Regime::R1, 0, &[0], 1, outcome);
    fact.build.ruleset = ruleset;
    fact
}

/// Session 1 has four trials and the others three, the first of each failing; the latest session comes first.
fn three_sessions() -> Vec<TrialFact> {
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
    let (score, regimes) = score_forward(&cfg, &held_out, &seen, 0.25).unwrap();
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
    };
    let check = ForwardCheck {
        rows: vec![row],
        pooled: ForwardScore::default(),
    };
    let mut out = Vec::new();
    report_forward(&mut out, &check, 0).unwrap();
    let text = String::from_utf8(out).unwrap();
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
