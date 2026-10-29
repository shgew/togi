use std::path::Path;

use test_case::test_case;
use togi_machine::{Regime, WorkloadId};

use super::forward::three_sessions;
use crate::artifact::Extract;
use crate::compare::{Comparison, compare, interval, metrics, report};
use crate::error::Error;
use crate::forward::{ForwardCheck, forward_check};
use crate::records::{Defect, Record, Sums, Totals, decode, encode, require_complete};

fn extract() -> Extract {
    Extract {
        facts: Vec::new(),
        sha256: "ab".repeat(32),
        rows: 10,
        sessions: 3,
    }
}

fn check() -> ForwardCheck {
    forward_check(&three_sessions(), 0).unwrap()
}

fn records(check: &ForwardCheck, seal: usize) -> Vec<Record> {
    let mut bytes = Vec::new();
    encode(&mut bytes, &extract(), seal, check).unwrap();
    decode(Path::new("records.jsonl"), bytes.as_slice()).unwrap()
}

fn compared(candidate: &[Record], reference: &[Record]) -> Comparison {
    compare(candidate, reference, 200, 7).unwrap()
}

fn text(comparison: &Comparison) -> String {
    let mut out = Vec::new();
    report(&mut out, comparison, 200).unwrap();
    String::from_utf8(out).unwrap()
}

/// Applies `change` to the fit loss and probability of every trial `select` picks.
fn with_trials(
    records: &[Record],
    select: impl Fn(usize) -> bool,
    change: impl Fn(&mut f64, &mut f64),
) -> Vec<Record> {
    let mut index = 0;
    records
        .iter()
        .cloned()
        .map(|mut record| {
            if let Record::Trial {
                fit_log_loss,
                probability,
                ..
            } = &mut record
            {
                if select(index) {
                    change(fit_log_loss, probability);
                }
                index += 1;
            }
            record
        })
        .collect()
}

#[test]
fn the_records_keep_every_trial_and_sum_bit_for_bit_to_the_check() {
    let check = check();
    let records = records(&check, 0);
    assert!(matches!(
        &records[0],
        Record::Input {
            rows: 10,
            seal: 0,
            ..
        }
    ));
    assert_eq!(
        records.last(),
        Some(&Record::Pooled {
            sums: Sums::from(&check.pooled)
        })
    );
    let mut trials = Vec::new();
    for record in &records {
        match record {
            Record::Trial {
                session,
                fit_log_loss,
                probability,
                surprise,
                ..
            } => trials.push((*session, *fit_log_loss, *probability, *surprise)),
            Record::Session { session, sums, .. } => {
                let mine: Vec<_> = trials.iter().filter(|t| t.0 == *session).collect();
                let loss = mine.iter().fold(0.0, |sum, t| sum + t.1);
                let predicted = mine.iter().fold(0.0, |sum, t| sum + t.2);
                assert_eq!(mine.len(), sums.trials);
                assert_eq!(loss.to_bits(), sums.fit_log_loss.to_bits());
                assert_eq!(predicted.to_bits(), sums.predicted.to_bits());
                assert_eq!(mine.iter().filter(|t| t.3).count(), sums.surprises);
            }
            Record::Input { .. } | Record::Pooled { .. } => {}
        }
    }
    assert_eq!(trials.len(), check.pooled.counts.trials);
    let rows: Vec<_> = check.rows.iter().map(|row| row.trials.len()).collect();
    assert_eq!(rows, [3, 3]);
}

#[test]
fn a_run_compared_with_itself_has_ratio_one_and_the_check_s_own_pooled_sums() {
    let check = check();
    let records = records(&check, 0);
    let comparison = compared(&records, &records);
    let all = &comparison.pooled.tallies[0];
    assert_eq!(all.trials, 6);
    assert_eq!(
        all.candidate.fit_log_loss.to_bits(),
        check.pooled.counts.fit_log_loss.to_bits()
    );
    assert_eq!(
        all.constant_log_loss.to_bits(),
        check.pooled.counts.constant_log_loss.to_bits()
    );
    assert_eq!(all.loss_ratio(), 1.0);
    assert_eq!(comparison.pooled.interval, (1.0, 1.0));
    let text = text(&comparison);
    let surprises = all.candidate.surprises;
    assert!(
        text.contains(&format!(
            ": not improved\nF1: pooled failures_p<0.01 candidate={surprises} reference={surprises}: holds\n"
        )),
        "{text}"
    );
    assert!(
        text.ends_with("F3: candidate loss lower in 0 of 2 held-out sessions: fails\n"),
        "{text}"
    );
}

#[test]
fn halving_every_loss_halves_each_ratio_exactly_and_improves_every_session() {
    let reference = records(&check(), 0);
    let candidate = with_trials(&reference, |_| true, |loss, _| *loss *= 0.5);
    let comparison = compared(&candidate, &reference);
    for (_, scope) in &comparison.sessions {
        assert_eq!(scope.tallies[0].loss_ratio(), 0.5);
        assert_eq!(scope.interval, (0.5, 0.5));
    }
    assert_eq!(comparison.improved(), 2);
    let text = text(&comparison);
    assert!(text.contains(" ratio=0.5 "), "{text}");
    assert!(
        text.ends_with("F3: candidate loss lower in 2 of 2 held-out sessions: holds\n"),
        "{text}"
    );
    let metrics = metrics(&comparison);
    assert_eq!(metrics["pooled.all.loss_ratio"], 0.5);
    assert_eq!(metrics["pooled.f3_improved"], 2.0);
    assert_eq!(metrics["20260102T000000Z.all.trials"], 3.0);
}

#[test]
fn the_bootstrap_resamples_trials_in_pairs_with_a_fixed_seed() {
    let reference = records(&check(), 0);
    let candidate = with_trials(&reference, |index| index == 1, |loss, _| *loss *= 3.0);
    let first = compared(&candidate, &reference);
    assert_eq!(first, compared(&candidate, &reference));
    let session = &first.sessions[0].1;
    let ratio = session.tallies[0].loss_ratio();
    assert!(
        session.interval.0 <= ratio
            && ratio <= session.interval.1
            && session.interval.0 < session.interval.1,
        "{ratio} outside {:?}",
        session.interval
    );
    assert_eq!(first.sessions[1].1.interval, (1.0, 1.0));
}

#[test]
fn the_interval_is_the_interpolated_2_5_and_97_5_percent_quantiles() {
    let sorted: Vec<f64> = (0..=10).map(f64::from).collect();
    assert_eq!(interval(&sorted), (0.25, 9.75));
    assert_eq!(interval(&[2.0]), (2.0, 2.0));
}

#[test]
fn the_subsets_split_single_core_non_r7_and_r7_trials() {
    let mut records = records(&check(), 0);
    let r7 = WorkloadId::new(Regime::R7, 0).unwrap();
    let mut index = 0;
    for record in &mut records {
        if let Record::Trial {
            workload, loaded, ..
        } = record
        {
            if index < 2 {
                *workload = r7;
                *loaded = 2;
            }
            index += 1;
        }
    }
    let tallies = compared(&records, &records).pooled.tallies;
    let trials = tallies.map(|tally| tally.trials);
    assert_eq!(trials, [6, 4, 4, 2]);
    let text = text(&compared(&records, &records));
    for row in [
        "Pooled all: trials=6 ",
        "Pooled single-core: trials=4 ",
        "Pooled non-R7: trials=4 ",
        "Pooled R7: trials=2 ",
    ] {
        assert!(text.contains(row), "{row}: {text}");
    }
}

#[test]
fn a_subset_without_failures_reports_its_predicted_count_instead_of_calibration() {
    let mut records = records(&check(), 0);
    for record in &mut records {
        if let Record::Trial { failure, .. } = record {
            *failure = false;
        }
    }
    let comparison = compared(&records, &records);
    let all = &comparison.pooled.tallies[0];
    assert_eq!(all.candidate.calibration(all.failures), None);
    let text = text(&comparison);
    assert!(
        text.contains("|ln(predicted/failures)| none: no failures")
            && text.contains("F2: no pooled failures; predicted candidate="),
        "{text}"
    );
    assert!(!metrics(&comparison).contains_key("pooled.all.candidate_calibration"));
}

#[test]
fn records_of_different_inputs_seals_or_trials_do_not_pair() {
    let check = check();
    let reference = records(&check, 0);
    let unpaired = |candidate: &[Record]| match compare(candidate, &reference, 10, 0) {
        Err(Error::Unpaired(why)) => why,
        other => panic!("paired: {other:?}"),
    };
    assert!(unpaired(&records(&check, 1)).contains("with --seal 1, the reference"));

    let mut failed = reference.clone();
    if let Some(Record::Trial { failure, .. }) = failed.get_mut(2) {
        *failure = !*failure;
    }
    assert!(unpaired(&failed).contains("with a different outcome, class or constant"));

    let mut fewer = reference.clone();
    fewer.remove(1);
    assert_eq!(
        unpaired(&fewer),
        "the candidate scored 5 trials, the reference 6"
    );
    assert_eq!(
        unpaired(&reference[1..]),
        "a records file has no input line"
    );
}

#[test]
fn a_complete_file_is_required_complete() {
    assert_eq!(require_complete(&records(&check(), 0)), Ok(()));
}

fn totals(trials: usize, failures: usize) -> Totals {
    Totals { trials, failures }
}

/// The sums of a session or pooled record.
fn sums_of(record: &mut Record) -> &mut Sums {
    match record {
        Record::Session { sums, .. } | Record::Pooled { sums } => sums,
        Record::Input { .. } | Record::Trial { .. } => panic!("{record:?} has no sums"),
    }
}

// `records(&check(), 0)` is the input (line 1), three trials and the summary of the first held-out session
// (lines 2-5), three trials and the summary of the second (lines 6-9), then the pooled summary (line 10).
#[test_case(|r| r.truncate(9), Defect::NoPooled; "cut after its last session")]
#[test_case(|r| r.truncate(8), Defect::NoPooled; "cut after its last trial")]
#[test_case(|r| r.truncate(1), Defect::NoPooled; "cut after the input")]
#[test_case(Vec::clear, Defect::NoInput; "empty")]
#[test_case(|r| { r.remove(0); }, Defect::NoInput; "without the input")]
#[test_case(|r| r.insert(2, r[0].clone()), Defect::SecondInput { line: 3 }; "second input")]
#[test_case(|r| r.push(r[9].clone()), Defect::SecondPooled { line: 11 }; "duplicate pooled")]
#[test_case(|r| r.push(r[4].clone()), Defect::AfterPooled { line: 11 }; "session after pooled")]
#[test_case(|r| { r.remove(8); }, Defect::UnsummarizedTrials { trials: 3 }; "last session summary missing")]
#[test_case(|r| r[2] = r[6].clone(), Defect::WrongSession { line: 3 }; "trial of another session")]
#[test_case(
    |r| {
        let Record::Session { session: other, .. } = r[8] else {
            panic!("{:?} is no session summary", r[8]);
        };
        if let Record::Session { session, .. } = &mut r[4] {
            *session = other;
        }
    },
    Defect::WrongSession { line: 5 };
    "summary of another session than its trial lines"
)]
#[test_case(
    |r| {
        let mut empty = r[4].clone();
        let sums = sums_of(&mut empty);
        sums.trials = 0;
        sums.failures = 0;
        r.insert(5, empty);
    },
    Defect::RepeatedSession { line: 6 };
    "an empty second summary of a closed session"
)]
#[test_case(|r| r.insert(5, r[1].clone()), Defect::RepeatedSession { line: 6 }; "trial of a closed session")]
#[test_case(
    |r| sums_of(&mut r[4]).trials += 1,
    Defect::SessionTotals { line: 5, recorded: totals(4, 1), counted: totals(3, 1) };
    "session trials disagree"
)]
#[test_case(
    |r| sums_of(&mut r[4]).failures += 1,
    Defect::SessionTotals { line: 5, recorded: totals(3, 2), counted: totals(3, 1) };
    "session failures disagree"
)]
#[test_case(
    |r| { r.remove(5); },
    Defect::SessionTotals { line: 8, recorded: totals(3, 1), counted: totals(2, 0) };
    "a trial line is missing"
)]
#[test_case(
    |r| sums_of(&mut r[9]).trials += 1,
    Defect::PooledTotals { line: 10, recorded: totals(7, 2), counted: totals(6, 2) };
    "pooled trials disagree"
)]
#[test_case(
    |r| sums_of(&mut r[9]).failures -= 1,
    Defect::PooledTotals { line: 10, recorded: totals(6, 1), counted: totals(6, 2) };
    "pooled failures disagree"
)]
fn records_that_are_not_the_whole_output_are_refused(change: fn(&mut Vec<Record>), want: Defect) {
    let mut records = records(&check(), 0);
    change(&mut records);
    assert_eq!(require_complete(&records), Err(want));
}

#[test]
fn a_line_that_is_not_a_record_names_its_file_and_line() {
    let error = decode(
        Path::new("r.jsonl"),
        b"{\"record\":\"pooled\"}\nnot json\n".as_slice(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, Error::Records { line: 1, .. }),
        "{error:?}"
    );
}

fn refused(line: &str) {
    let error = decode(Path::new("r.jsonl"), line.as_bytes()).unwrap_err();
    assert!(
        matches!(&error, Error::Records { line: 1, .. }),
        "{line}: {error:?}"
    );
}

const INPUT: &str = r#"{"record":"input","sha256":"ab","rows":10,"seal":0}"#;
const SUMS: &str = r#""trials":1,"failures":1,"predicted":0.5,"fit_log_loss":0.7,"constant_log_loss":0.7,"surprises":0,"matches":0,"flagged":0,"eligible":0"#;

#[test]
fn an_input_and_pooled_object_decode() {
    let decoded = decode(
        Path::new("r.jsonl"),
        format!("{INPUT}\n{{\"record\":\"pooled\",\"sums\":{{{SUMS}}}}}\n").as_bytes(),
    )
    .unwrap();
    assert_eq!(decoded.len(), 2);
}

#[test]
fn a_correctly_sized_record_array_is_refused() {
    refused(r#"["input","ab",10,0]"#);
}

#[test]
fn a_correctly_sized_sums_array_is_refused() {
    refused(r#"{"record":"pooled","sums":[1,1,0.5,0.7,0.7,0,0,0,0]}"#);
}

#[test]
fn an_unknown_record_member_is_refused() {
    refused(r#"{"record":"input","sha256":"ab","rows":10,"seal":0,"extra":1}"#);
}

#[test]
fn an_unknown_sums_member_is_refused() {
    refused(&format!(
        "{{\"record\":\"pooled\",\"sums\":{{{SUMS},\"extra\":1}}}}"
    ));
}
