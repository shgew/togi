//! Scorer tests over synthetic records. Most tests use the direct measure and score-row paths with a
//! handful of records; one test scores the full registered Cartesian product.

use togi_simrun::day_night::successor::{
    Admissibility, Classification, FinishReason, InvocationTiming, Phase, RestorationOutcome,
    RiskSource, StepIdentity, StepKind, TruthCause, UnitFinish,
};

use super::fixture::{Ev, Lab, Restore, Spec, resync, retopologize, uid};
use super::score_validated as score;
use super::*;
use crate::day_night_successor::render::{render, render_seeds};

const SLOPE: f64 = 1.5;

fn built(lab: &Lab, spec: &Spec) -> Record {
    let record = spec.build(lab);
    record
        .validate()
        .unwrap_or_else(|error| panic!("fixture record is invalid: {error}"));
    record
}

fn run(lab: &Lab, records: &[Record]) -> Score {
    score(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::Registered),
        records,
    )
    .expect("score")
}

fn metrics(lab: &Lab, record: &Record) -> SeedMetrics {
    let scenario = lab
        .registration
        .scenario(&record.provenance.scenario)
        .expect("scenario");
    measure::measure(record, &Rules::new(&lab.registration), Scope::of(scenario)).expect("measure")
}

fn group<'a>(score: &'a Score, scenario: &str, slope: f64) -> &'a Group {
    score
        .groups
        .iter()
        .find(|group| group.scenario == scenario && (group.slope - slope).abs() < 1e-9)
        .expect("group")
}

fn criterion<'a>(group: &'a Group, name: &str) -> &'a Criterion {
    group
        .criteria
        .iter()
        .find(|item| item.name == name)
        .unwrap_or_else(|| panic!("criterion {name}"))
}

fn labels(score: &Score) -> Vec<&'static str> {
    score
        .faults
        .iter()
        .map(|fault| fault.kind.label())
        .collect()
}

fn rules_of(metrics: &SeedMetrics) -> Vec<&'static str> {
    metrics
        .learning
        .violations
        .iter()
        .map(|violation| violation.rule)
        .collect()
}

fn ledger(metrics: &SeedMetrics) -> &measure::LedgerView {
    metrics
        .ledger
        .as_ref()
        .expect("the policy reported an allowance ledger")
}

fn good(lab: &Lab) -> Vec<Record> {
    lab.records(Split::Dev, RunProfile::Registered, |spec| spec)
}

#[test]
fn a_complete_good_experiment_passes_with_separate_columns() {
    let lab = Lab::new(&["successor-edge-deep", "successor-hidden-daily"], 3);
    let result = run(&lab, &good(&lab));
    assert_eq!((result.expected_cells, result.complete_cells), (12, 12));
    assert!(result.complete(), "{:?} {:?}", result.faults, result.issues);
    assert_eq!(result.verdict, Verdict::Pass);
    assert_eq!(result.groups.len(), 4);
    for item in &result.groups {
        assert_eq!(
            item.status,
            GroupStatus::Met,
            "{} {:?}",
            item.scenario,
            item.criteria
        );
        assert!(item.safety_met && item.usefulness_met);
        assert_eq!(item.seeds.len(), 3);
    }
    let benign = group(&result, "successor-edge-deep", SLOPE);
    assert!(
        benign
            .criteria
            .iter()
            .any(|item| item.name == "benign-depth" && item.column == Column::Usefulness)
    );
    assert!(
        group(&result, "successor-hidden-daily", SLOPE)
            .criteria
            .iter()
            .all(|item| item.name != "benign-depth")
    );
    let depth = criterion(benign, "benign-depth");
    assert_eq!(depth.share, Some(Share::new(3, 3)));
    assert_eq!(benign.seeds[0].settled_at, Some(15.0));
    assert_eq!(benign.diagnostics.outcomes.converged, 3);
}

#[test]
fn stock_forever_is_safe_but_neither_deep_nor_convergent_on_benign_scenarios() {
    let lab = Lab::new(&["successor-edge-deep", "successor-hidden-daily"], 3);
    let records = lab.records(Split::Dev, RunProfile::Registered, Spec::stock);
    let result = run(&lab, &records);
    assert!(result.complete());
    let benign = group(&result, "successor-edge-deep", SLOPE);
    assert!(benign.safety_met, "{:?}", benign.criteria);
    assert!(!benign.usefulness_met);
    assert!(!criterion(benign, "benign-depth").passed);
    assert!(!criterion(benign, "convergence").passed);
    assert!(!criterion(benign, "benign-median-settled-day").passed);
    assert_eq!(
        criterion(benign, "convergence").share,
        Some(Share::new(0, 3))
    );
    assert_eq!(benign.diagnostics.median_settled_day, Some(f64::INFINITY));
    assert_eq!(benign.diagnostics.outcomes.unconverged_crash_free, 3);
    assert_eq!(benign.status, GroupStatus::NotMet);
    // Without a benign criterion the same stock trajectory is not penalized: depth is not demanded there.
    assert_eq!(
        group(&result, "successor-hidden-daily", SLOPE).status,
        GroupStatus::Met
    );
    assert_eq!(result.verdict, Verdict::Fail);
    assert_eq!(
        result.scenario_status("successor-edge-deep"),
        GroupStatus::NotMet
    );
}

#[test]
fn reduced_dev_is_labelled_and_never_gates() {
    let lab = Lab::new(&["successor-edge-deep"], 3);
    let records = lab.records(Split::Dev, RunProfile::ReducedDev, |spec| spec);
    let result = score(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::ReducedDev),
        &records,
    )
    .expect("score");
    assert_eq!(result.verdict, Verdict::ExploratoryReduced);
    assert_eq!(result.label.as_deref(), Some(REDUCED_DEV_LABEL));
    assert_eq!(
        result.verdict.label(),
        "EXPLORATORY REDUCED DEV — NOT A REGISTERED SCORE"
    );
    for text in [render(&result), render_seeds(&result)] {
        assert!(
            text.starts_with(&format!("**{REDUCED_DEV_LABEL}**")),
            "{text}"
        );
        assert!(!text.contains("PASS") && !text.contains("FAIL"));
        assert!(text.contains("criteria met"));
    }
    // A failing reduced read is still exploratory.
    let stock = lab.records(Split::Dev, RunProfile::ReducedDev, Spec::stock);
    let failing = score(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::ReducedDev),
        &stock,
    )
    .expect("score");
    assert_eq!(failing.verdict, Verdict::ExploratoryReduced);
    assert!(render(&failing).contains("criteria not met"));
}

fn incomplete_lab() -> Lab {
    Lab::new(&["successor-edge-deep"], 2)
}

#[test_case::test_case(|records| { records.remove(0); }, "missing", 3; "missing cell")]
#[test_case::test_case(|records| records.push(records[0].clone()), "duplicate", 3; "duplicate cell")]
#[test_case::test_case(|records| records.push(Spec::new("successor-edge-deep", 99_999, SLOPE).build(&incomplete_lab())), "unexpected", 4; "unexpected cell")]
#[test_case::test_case(|records| { records[0].units.pop(); }, "invalid", 3; "unit dimensions")]
#[test_case::test_case(|records| records[0].epochs[0].reference.certified_upper_bound += 1, "invalid", 3; "uncertified reference")]
#[test_case::test_case(|records| records[0].outcome = Outcome::CompleteUnconvergedCrashFree { observed_until_day: 731.0 }, "inconsistent", 3; "contradicted outcome")]
fn an_incomplete_cell_blocks_the_verdict(
    edit: fn(&mut Vec<Record>),
    expected: &str,
    complete_cells: u64,
) {
    let lab = incomplete_lab();
    let mut records = good(&lab);
    edit(&mut records);
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(labels(&result), vec![expected], "{:?}", result.faults);
    assert_eq!(
        (result.expected_cells, result.complete_cells),
        (4, complete_cells)
    );
    assert!(!result.complete());
    let text = render(&result);
    assert!(text.contains("INCOMPLETE") && text.contains("## Incomplete cells"));
}

#[test]
fn truncated_and_error_runs_keep_lower_bounds_and_never_count_as_zero() {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let records = lab.records(Split::Dev, RunProfile::Registered, |spec| {
        if spec.seed == 20_001 {
            let mut cut = spec.event(Ev::co("early", 100.0, 0));
            cut.observed_until = Some(400.0);
            cut
        } else if spec.seed == 20_002 && spec.slope < 1.75 {
            let mut broken = spec;
            broken.observed_until = Some(300.0);
            broken.harness_error = true;
            broken
        } else {
            spec
        }
    });
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(result.complete_cells, 1);
    let mut found = labels(&result);
    found.sort_unstable();
    assert_eq!(found, vec!["harness-error", "truncated", "truncated"]);
    let truncated = result
        .faults
        .iter()
        .find(|fault| matches!(fault.kind, FaultKind::Truncated { .. }))
        .expect("truncated");
    let FaultKind::Truncated {
        observed_until_day,
        lower_bound,
        ..
    } = &truncated.kind
    else {
        unreachable!()
    };
    assert_eq!(*observed_until_day, 400.0);
    assert_eq!(
        (
            lower_bound.daily_interruptions,
            lower_bound.daily_co_crashes
        ),
        (1, 1)
    );
    assert!((lower_bound.active_hours - 3192.0).abs() < 1e-6);
    // The one complete cell still describes itself, but the group is incomplete.
    let cell_group = group(&result, "successor-edge-deep", 2.0);
    assert_eq!(cell_group.seeds.len(), 1);
    assert_eq!(cell_group.status, GroupStatus::Incomplete);
    assert!(render(&result).contains("at least 1 daily interruptions"));
}

#[test]
fn all_five_outcome_statuses_are_counted_or_faulted() {
    let lab = Lab::new(&["successor-edge-deep"], 5);
    let records = lab.records(Split::Dev, RunProfile::Registered, |spec| match spec.seed {
        20_002 => spec.stock(),
        20_003 => spec.stock().event(Ev::co("c", 100.0, 0)),
        20_004 => Spec {
            observed_until: Some(400.0),
            ..spec
        },
        20_005 => Spec {
            observed_until: Some(400.0),
            harness_error: true,
            ..spec
        },
        _ => spec,
    });
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    let slope = group(&result, "successor-edge-deep", SLOPE);
    let outcomes = slope.diagnostics.outcomes;
    assert_eq!(
        (
            outcomes.converged,
            outcomes.unconverged_crash_free,
            outcomes.unconverged_with_co_crashes
        ),
        (1, 1, 1)
    );
    assert_eq!((slope.seeds.len(), slope.expected_seeds.len()), (3, 5));
    // Denominators are the complete records, not the expected seeds.
    assert_eq!(
        criterion(slope, "convergence").share,
        Some(Share::new(1, 3))
    );
    assert_eq!(
        criterion(slope, "retained-profile-in-A").share,
        Some(Share::new(3, 3))
    );
    let mut found = labels(&result);
    found.sort_unstable();
    assert_eq!(
        found,
        vec!["harness-error", "harness-error", "truncated", "truncated"]
    );
    // A median over every registered seed is infinite when half the seeds never converged.
    assert_eq!(slope.diagnostics.median_settled_day, Some(f64::INFINITY));
}

#[test_case::test_case(|record| record.provenance.registration_id = "other".into(); "registration id")]
#[test_case::test_case(|record| record.provenance.registration_sha256 = "d".repeat(64); "registration hash")]
#[test_case::test_case(|record| record.provenance.split = Split::Holdout; "split")]
#[test_case::test_case(|record| record.provenance.run_profile = RunProfile::ReducedDev; "run profile")]
#[test_case::test_case(|record| record.provenance.policy_identity = "other".into(); "policy identity")]
#[test_case::test_case(|record| record.provenance.source_revision = "other".into(); "source revision")]
#[test_case::test_case(|record| record.provenance.policy_source_sha256 = "d".repeat(64); "policy source hash")]
#[test_case::test_case(|record| record.provenance.parameters_sha256 = "d".repeat(64); "parameters hash")]
#[test_case::test_case(|record| record.provenance.simulator_revision = "other".into(); "simulator revision")]
#[test_case::test_case(|record| record.provenance.report_revision = "other".into(); "report revision")]
#[test_case::test_case(|record| record.provenance.binary_sha256 = None; "binary hash missing")]
#[test_case::test_case(|record| record.provenance.input_sha256 = Some("d".repeat(64)); "input hash differs")]
#[test_case::test_case(|record| record.runtime.jobs = 3; "jobs")]
#[test_case::test_case(|record| record.runtime.resource_class = "other".into(); "resource class")]
#[test_case::test_case(|record| record.epochs[0].context = "other".into(); "epoch context")]
fn an_identity_or_calendar_mismatch_is_incompatible(edit: fn(&mut Record)) {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let mut records = good(&lab);
    edit(&mut records[0]);
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(labels(&result), vec!["incompatible"], "{:?}", result.faults);
}

#[test]
fn a_topology_the_registration_does_not_describe_is_incompatible() {
    let mut lab = Lab::new(&["successor-edge-deep"], 2);
    let records = good(&lab);
    retopologize(&mut lab, "successor-edge-deep", 4, 4);
    let result = run(&lab, &records);
    assert_eq!(labels(&result), vec!["incompatible"; 4]);
    assert!(result.faults.iter().all(|fault| matches!(&fault.kind, FaultKind::Incompatible(reasons) if reasons.iter().any(|reason| reason.contains("topology")))));
}

#[test]
fn scoring_is_unit_generic_beyond_two_units() {
    let mut lab = Lab::new(&["successor-edge-deep"], 2);
    retopologize(&mut lab, "successor-edge-deep", 4, 4);
    let records = good(&lab);
    assert_eq!(records[0].units.len(), 4);
    let result = run(&lab, &records);
    assert_eq!(
        result.verdict,
        Verdict::Pass,
        "{:?}",
        group(&result, "successor-edge-deep", SLOPE).criteria
    );
    let seed = &group(&result, "successor-edge-deep", SLOPE).seeds[0];
    assert_eq!(seed.depth.final_depths, vec![5; 4]);
    assert_eq!(
        (
            seed.depth.final_total,
            seed.depth.required_total,
            seed.depth.reference
        ),
        (20, 20, 24)
    );
    assert_eq!(seed.units.len(), 4);
    assert_eq!(seed.depth.final_core_mean, Some(5.0));
    // One unit short of its floor fails the benign depth: the unit total falls below the required total.
    let shallow = built(
        &lab,
        &Spec::new("successor-edge-deep", 20_001, SLOPE).depths(&[5, 5, 5, 3]),
    );
    assert!(!metrics(&lab, &shallow).benign_ok);
}

#[test]
fn asymmetric_safe_profiles_are_not_compared_by_coordinates() {
    let lab = Lab::new(&["successor-hidden-daily"], 3);
    let records = lab.records(Split::Dev, RunProfile::Registered, |spec| match spec.seed {
        20_001 => spec.depths(&[5, 2]),
        20_002 => spec.depths(&[2, 5]),
        _ => spec.depths(&[4, 4]),
    });
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Pass);
    let seeds = &group(&result, "successor-hidden-daily", SLOPE).seeds;
    assert_eq!(seeds[0].depth.final_depths, vec![5, 2]);
    assert_eq!(seeds[1].depth.final_depths, vec![2, 5]);
    assert_eq!(seeds[2].depth.final_depths, vec![4, 4]);
    assert!(
        seeds
            .iter()
            .all(|seed| seed.final_in_a && seed.post_finish.all_in_a)
    );
}

#[test]
fn a_later_epoch_violation_invalidates_a_settled_interval() {
    let lab = Lab::new(&["successor-moving-cap"], 1);
    let mut record = built(&lab, &Spec::new("successor-moving-cap", 20_001, SLOPE));
    assert_eq!(metrics(&lab, &record).settled_at, Some(15.0));
    for interval in record
        .intervals
        .iter_mut()
        .filter(|interval| interval.start_day >= 400.0)
    {
        interval.admissibility = Admissibility {
            joint_ratio: 3.0,
            unit_ratios: vec![3.0; 2],
            member: false,
        };
        for source in &mut interval.sources {
            source.expected_failures *= 6.0;
        }
    }
    resync(&mut record);
    record.outcome = Outcome::CompleteUnconvergedCrashFree {
        observed_until_day: 731.0,
    };
    record.validate().expect("valid");
    let seed = metrics(&lab, &record);
    assert_eq!(seed.settled_at, None);
    assert!(!seed.final_in_a && !seed.post_finish.all_in_a && !seed.risk.rate_ok);
    assert_eq!(seed.post_finish.first_violation, Some(400.0));
    assert!((seed.post_finish.violating_hours - 331.0 * 8.0).abs() < 1e-6);
    assert!(seed.epochs[0].in_a_at_end && !seed.epochs[1].in_a_at_end);
    assert_eq!(seed.epochs[1].recovered_to_a_at, None);
    // Recording it as converged contradicts the recomputation.
    record.outcome = Outcome::CompleteConverged {
        observed_until_day: 731.0,
    };
    assert!(measure::measure(&record, &Rules::new(&lab.registration), Scope::default()).is_err());
}

#[test]
fn allowance_exhaustion_before_a_promotion_breaks_the_learning_contract() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let spec = Spec::new("successor-edge-deep", 20_001, SLOPE)
        .event(Ev::co("a", 2.0, 0))
        .event(Ev::co("b", 3.0, 1));
    let record = built(&lab, &spec);
    let seed = metrics(&lab, &record);
    assert_eq!(ledger(&seed).consumed, 2);
    assert_eq!(
        (
            ledger(&seed).remaining,
            ledger(&seed).overspend,
            ledger(&seed).exhausted
        ),
        (0, 0, true)
    );
    assert_eq!(ledger(&seed).exhausted_day, Some(3.0));
    // The events on days 2 and 3 are suspect inside the first promotion's hold window, so that
    // promotion also lacks a verified clean hold; the second promotion's window is clean.
    assert_eq!(
        rules_of(&seed),
        vec![
            "promotion-after-exhaustion",
            "promotion-verified-hold",
            "promotion-after-exhaustion"
        ]
    );
    assert_eq!(seed.risk.learning_k, 2);
    assert_eq!(
        seed.learning
            .promotions
            .iter()
            .map(|promotion| promotion.charged_before)
            .collect::<Vec<_>>(),
        vec![2, 2]
    );
    let result = run(
        &lab,
        &[record.clone(), {
            let mut other = record;
            other.provenance.slope = 2.0;
            other
        }],
    );
    let item = group(&result, "successor-edge-deep", SLOPE);
    assert!(!criterion(item, "learning-contract").passed);
    assert!(!item.safety_met && item.usefulness_met);
}

#[test]
fn a_suspect_event_at_a_nonzero_profile_cannot_go_uncharged() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut crash = Ev::co("c", 100.0, 0);
    crash.charged = false;
    let seed = metrics(
        &lab,
        &built(
            &lab,
            &Spec::new("successor-edge-deep", 20_001, SLOPE).event(crash),
        ),
    );
    assert_eq!(rules_of(&seed), vec!["charge-refund"]);
    // A charged event with no suspect basis is also refused.
    let mut basis = Ev::background("b", 100.0);
    basis.charged = true;
    let seed = metrics(
        &lab,
        &built(
            &lab,
            &Spec::new("successor-edge-deep", 20_001, SLOPE).event(basis),
        ),
    );
    assert_eq!(rules_of(&seed), vec!["charge-basis"]);
    // An unexplained event is CPU-suspect too, so leaving it uncharged escapes the ledger.
    let mut unexplained = Ev::background("u", 100.0);
    unexplained.classification = Classification::Unexplained;
    unexplained.charged = false;
    let seed = metrics(
        &lab,
        &built(
            &lab,
            &Spec::new("successor-edge-deep", 20_001, SLOPE).event(unexplained),
        ),
    );
    assert_eq!(rules_of(&seed), vec!["charge-refund"]);
}

#[test]
fn a_reclassified_post_finish_event_keeps_its_charge() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let event = Ev {
        classification: Classification::NonCpu,
        provisional: true,
        reclassified_from: Some(Classification::Unanswered),
        charged: true,
        ..Ev::background("r", 100.0)
    };
    let seed = metrics(
        &lab,
        &built(
            &lab,
            &Spec::new("successor-edge-deep", 20_001, SLOPE).event(event),
        ),
    );
    assert!(
        seed.learning.violations.is_empty(),
        "{:?}",
        seed.learning.violations
    );
    assert_eq!(
        (
            ledger(&seed).consumed,
            ledger(&seed).provisional,
            ledger(&seed).reclassified,
            ledger(&seed).remaining
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(ledger(&seed).charged_post_finish, 1);
    assert_eq!(
        seed.event_views[0].reclassified_from,
        Some(Classification::Unanswered)
    );
    assert!(seed.event_views[0].charged && seed.event_views[0].provisional);
    assert_eq!(seed.risk.k, 0);
    assert_eq!(seed.events.day_post_finish.background, 1);
}

#[test]
fn a_shared_crash_counts_once_in_k_and_attributes_to_each_unit() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let pair = Ev {
        truth: TruthCause::Co {
            source: RiskSource::Interaction {
                identity: "pair".into(),
            },
            implicated_units: vec![uid(0), uid(1)],
        },
        ..Ev::co("i", 100.0, 0)
    };
    let seed = metrics(
        &lab,
        &built(
            &lab,
            &Spec::new("successor-edge-deep", 20_001, SLOPE).event(pair),
        ),
    );
    assert_eq!(seed.risk.k, 1);
    assert_eq!(
        (seed.units[0].co_attributions, seed.units[1].co_attributions),
        (1, 1)
    );
    assert_eq!(seed.events.day_post_finish.co, 1);
    let source = |label: &str| {
        seed.risk
            .sources
            .iter()
            .find(|row| row.source == label)
            .unwrap_or_else(|| panic!("{label}"))
    };
    assert_eq!(
        (
            source("interaction pair").post_finish_k,
            source("unit 0").post_finish_k
        ),
        (1, 0)
    );
    // T is the one shared exposure, never summed per source.
    assert!((seed.risk.t - 716.0 * 8.0).abs() < 1e-6);
    assert_eq!(seed.daily_co_crashes, 1);
}

#[test]
fn night_failures_stay_out_of_the_daily_accounts() {
    let lab = Lab::new(&["successor-edge-deep"], 3);
    let records = lab.records(Split::Dev, RunProfile::Registered, |spec| {
        if spec.seed == 20_001 {
            spec.event(Ev::night("n", 50.0))
                .event(Ev::background("b", 60.0))
        } else {
            spec
        }
    });
    let result = run(&lab, &records);
    let item = group(&result, "successor-edge-deep", SLOPE);
    let night = &item.seeds[0];
    assert_eq!(
        (
            night.events.night.total,
            night.events.day().total,
            night.risk.k
        ),
        (1, 1, 0)
    );
    assert_eq!(ledger(&night).consumed, 0);
    // Zero total interruptions and zero true CO crashes are separate counts.
    assert_eq!(
        item.diagnostics.zero_total_daily_interruptions,
        Share::new(2, 3)
    );
    assert_eq!(item.diagnostics.zero_daily_co_crashes, Share::new(3, 3));
    assert_eq!(item.diagnostics.zero_co_learning_crashes, Share::new(3, 3));
}

#[test]
fn restoration_learning_overlaps_the_post_finish_exposure() {
    let lab = Lab::new(&["successor-firmware-change"], 1);
    // A true CO crash the policy classified as non-CPU is uncharged, so the restored profile owes no
    // step-back; a charged CPU-suspect event there is refused (see the learning review tests).
    let crash = Ev {
        classification: Classification::NonCpu,
        charged: false,
        tag: Some("fw".into()),
        ..Ev::co("tagged", 200.0, 0)
    };
    let spec = Spec::new("successor-firmware-change", 20_001, SLOPE)
        .restore(Restore::Retained)
        .event(crash);
    let seed = metrics(&lab, &built(&lab, &spec));
    assert!(
        seed.learning.violations.is_empty(),
        "{:?}",
        seed.learning.violations
    );
    assert_eq!(
        seed.learning.restorations[0].status,
        RestorationStatus::Retained
    );
    assert!(
        (seed.learning.restorations[0]
            .stock_hours_recomputed
            .expect("hours")
            - 64.0)
            .abs()
            < 1e-6
    );
    // The tag marks a subset of the post-finish figures; it adds nothing to them.
    assert!((seed.risk.tagged.hours - (731.0 - 189.0) * 8.0).abs() < 1e-6);
    assert_eq!((seed.risk.tagged.k, seed.risk.k), (1, 1));
    assert!(seed.risk.tagged.hours < seed.risk.t);
    assert_eq!(ledger(&seed).charged_restoration_tagged, 0);
    assert_eq!(ledger(&seed).consumed, 0);
    assert_eq!(
        seed.event_views[0].restoration_learning.as_deref(),
        Some("fw")
    );
    // A restoration without enough stock hours breaks the contract.
    let mut record = built(&lab, &spec);
    if let RestorationOutcome::Applied {
        clean_stock_hours, ..
    } = &mut record.restorations[0].outcome
    {
        *clean_stock_hours = 10.0;
    }
    assert_eq!(
        rules_of(&metrics(&lab, &record)),
        vec!["restoration-stock-hours"]
    );
}

#[test]
fn firmware_restoration_blocked_and_crashed_paths() {
    let lab = Lab::new(&["successor-firmware-change"], 1);
    let blocked = metrics(
        &lab,
        &built(
            &lab,
            &Spec::new("successor-firmware-change", 20_001, SLOPE).restore(Restore::Blocked),
        ),
    );
    assert!(
        blocked.learning.violations.is_empty(),
        "{:?}",
        blocked.learning.violations
    );
    assert!(
        matches!(&blocked.learning.restorations[0].status, RestorationStatus::Blocked { reason } if reason.contains("not qualified"))
    );
    assert_eq!(blocked.depth.final_depths, vec![0, 0]);
    assert_eq!(blocked.depth.finish_depths, Some(vec![5, 5]));
    assert!(!blocked.retention_ok);

    let crashed = metrics(
        &lab,
        &built(
            &lab,
            &Spec::new("successor-firmware-change", 20_001, SLOPE).restore(Restore::Crashed),
        ),
    );
    assert!(
        crashed.learning.violations.is_empty(),
        "{:?}",
        crashed.learning.violations
    );
    assert_eq!(
        crashed.learning.restorations[0].status,
        RestorationStatus::Crashed {
            event: "crash".into()
        }
    );
    assert_eq!(crashed.learning.restorations[0].stopped_units.len(), 2);
    // The crash keeps the original first-finish boundary: it is post-finish K, tagged as restoration
    // learning, and the units keep their pre-firmware finishes.
    assert_eq!(crashed.first_finish_day, Some(15.0));
    assert_eq!((crashed.risk.learning_k, crashed.risk.k), (0, 1));
    assert_eq!(crashed.risk.tagged.k, 1);
    assert_eq!(crashed.event_views[0].phase, Phase::PostFinish);
    assert!(crashed.units.iter().all(|unit| matches!(
        unit.finish,
        Some((FinishReason::Ceiling, day)) if day <= 15.0
    )));
    assert!(
        matches!(crashed.event_views[0].newest_step.as_deref(), Some(text) if text.starts_with("restoration fw"))
    );
    assert_eq!(
        crashed
            .units
            .iter()
            .map(|unit| unit.retreat_steps)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    assert_eq!(
        crashed
            .units
            .iter()
            .map(|unit| unit.losses.co)
            .collect::<Vec<_>>(),
        vec![5, 5]
    );

    // Dropping the permanent stop breaks the crash contract.
    let crash_record = || {
        built(
            &lab,
            &Spec::new("successor-firmware-change", 20_001, SLOPE).restore(Restore::Crashed),
        )
    };
    let mut record = crash_record();
    record.policy.stopped_units.clear();
    assert!(rules_of(&metrics(&lab, &record)).contains(&"restoration-crash-stop"));
    // A crash that moves a unit's finish to the crash resets the first-finish boundary.
    let mut record = crash_record();
    for unit in &mut record.units {
        if let Some(finish) = &mut unit.finish {
            finish.day = 191.0;
        }
    }
    record.policy.finishes = record
        .units
        .iter()
        .filter_map(|unit| unit.finish.clone())
        .collect();
    assert!(rules_of(&metrics(&lab, &record)).contains(&"restoration-crash-finish"));
    let mut record = built(
        &lab,
        &Spec::new("successor-firmware-change", 20_001, SLOPE).restore(Restore::Crashed),
    );
    record.events[0].policy_event.newest_step = Some(StepIdentity::Restoration {
        restoration: "fw".into(),
        units: vec![uid(0)],
    });
    assert!(rules_of(&metrics(&lab, &record)).contains(&"restoration-crash-identity"));
}

#[test]
fn mixed_unpinned_input_hashes_never_pass() {
    let mut lab = Lab::new(&["successor-edge-deep"], 2);
    lab.registration.sources.input_sha256 = None;
    let mut records = good(&lab);
    for record in &mut records {
        record.provenance.input_sha256 = Some("c".repeat(64));
    }
    let uniform = run(&lab, &records);
    assert_eq!(uniform.verdict, Verdict::Pass, "{:?}", uniform.issues);
    records[0].provenance.input_sha256 = Some("e".repeat(64));
    let mixed = run(&lab, &records);
    assert_eq!(mixed.verdict, Verdict::Incomplete);
    assert!(
        mixed
            .issues
            .iter()
            .any(|issue| issue.contains("different input sets")),
        "{:?}",
        mixed.issues
    );
    assert!(mixed.faults.is_empty());
}

#[test]
fn a_complete_single_slope_reduced_read_is_exploratory_not_incomplete() {
    let lab = Lab::new(&["successor-edge-deep"], 3);
    let records: Vec<Record> = lab
        .records(Split::Dev, RunProfile::ReducedDev, |spec| spec)
        .into_iter()
        .filter(|record| record.provenance.slope.to_bits() == SLOPE.to_bits())
        .collect();
    let mut selection = lab.selection(Split::Dev, RunProfile::ReducedDev);
    selection.slopes = Some(vec![SLOPE]);
    let result = score(&lab.registration, &lab.sha, &selection, &records).expect("score");
    assert_eq!((result.expected_cells, result.complete_cells), (3, 3));
    assert!(result.issues.is_empty(), "{:?}", result.issues);
    assert_eq!(result.verdict, Verdict::ExploratoryReduced);
    assert_eq!(result.selected_slopes, vec![SLOPE]);
    // The same single slope under the registered profile still cannot gate.
    let registered: Vec<Record> = good(&lab)
        .into_iter()
        .filter(|record| record.provenance.slope.to_bits() == SLOPE.to_bits())
        .collect();
    selection.run_profile = RunProfile::Registered;
    let gate = score(&lab.registration, &lab.sha, &selection, &registered).expect("score");
    assert_eq!(gate.verdict, Verdict::Incomplete);
}

#[test]
fn unpinned_binaries_and_selection_errors() {
    let mut lab = Lab::new(&["successor-edge-deep"], 2);
    lab.registration.sources.binary_sha256 = None;
    let mut records = good(&lab);
    for record in &mut records {
        record.provenance.binary_sha256 = Some("b".repeat(64));
    }
    records[0].provenance.binary_sha256 = Some("e".repeat(64));
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert!(
        result
            .issues
            .iter()
            .any(|issue| issue.contains("different binaries")),
        "{:?}",
        result.issues
    );
    assert!(result.faults.is_empty());

    let lab = Lab::new(&["successor-edge-deep"], 2);
    let records = good(&lab);
    let mut selection = lab.selection(Split::Dev, RunProfile::Registered);
    selection.slopes = Some(vec![1.5]);
    let one_slope = score(&lab.registration, &lab.sha, &selection, &records).expect("score");
    assert_eq!(one_slope.expected_cells, 2);
    assert_eq!(one_slope.complete_cells, 2);
    assert_eq!(one_slope.verdict, Verdict::Incomplete);
    assert!(
        one_slope
            .issues
            .iter()
            .any(|issue| issue.contains("1 of 2 registered slopes"))
    );
    assert_eq!(
        one_slope.faults.len(),
        2,
        "records at the unselected slope are unexpected"
    );
    selection.slopes = Some(vec![3.0]);
    assert!(matches!(
        score(&lab.registration, &lab.sha, &selection, &records),
        Err(ScoreError::UnregisteredSlope(_))
    ));
    selection.slopes = None;
    selection.policy = "nobody".into();
    assert!(matches!(
        score(&lab.registration, &lab.sha, &selection, &records),
        Err(ScoreError::UnknownPolicy(_))
    ));
    selection.policy = "successor".into();
    assert!(matches!(
        score(&lab.registration, "not-a-hash", &selection, &records),
        Err(ScoreError::RegistrationHash)
    ));
    // Holdout is never a reduced-dev split.
    let holdout = ScoreSelection {
        split: Split::Holdout,
        run_profile: RunProfile::ReducedDev,
        ..selection
    };
    assert!(matches!(
        score(&lab.registration, &lab.sha, &holdout, &records),
        Err(ScoreError::Registration(_))
    ));
}

#[test]
fn records_round_trip_through_strict_json_lines() {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let records = good(&lab);
    let mut bytes = Vec::new();
    for record in &records {
        bytes.extend(serde_json::to_vec(record).expect("serialize"));
        bytes.extend(b"\n\n");
    }
    assert_eq!(read_records(&bytes).expect("read"), records);
    let mut broken = serde_json::to_vec(&records[0]).expect("serialize");
    broken.extend(b"\n{\"registration\":");
    let error = read_records(&broken).expect_err("garbage line");
    assert_eq!(error.line, 2);
    let mut extra = serde_json::to_value(&records[0]).expect("value");
    extra["unregistered"] = serde_json::json!(true);
    assert!(read_records(extra.to_string().as_bytes()).is_err());
    assert!(read_records(b"").expect("empty").is_empty());
}

#[test]
fn median_and_poisson_diagnostics() {
    assert_eq!(median(&[]), None);
    assert_eq!(median(&[3.0]), Some(3.0));
    assert_eq!(median(&[4.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(&[1.0, 3.0]), Some(2.0));
    assert_eq!(
        median(&[1.0, 3.0, f64::INFINITY, f64::INFINITY]),
        Some(f64::INFINITY)
    );
    assert_eq!(median(&[1.0, 3.0, 5.0, f64::INFINITY]), Some(4.0));
    assert!((poisson_upper_rate(1, 1000.0).expect("rate") - 0.004_743_9).abs() < 1e-6);
    assert_eq!(poisson_upper_rate(1, 0.0), None);
    assert!(Share::new(3, 3).all() && !Share::new(2, 3).all() && !Share::new(0, 0).all());
    assert_eq!(Share::count([true, false, true]), Share::new(2, 3));
}

fn account_score() -> Score {
    let lab = Lab::new(&["successor-edge-deep", "successor-firmware-change"], 1);
    let records = lab.records(Split::Dev, RunProfile::Registered, |spec| {
        if spec.scenario == "successor-firmware-change" {
            spec.restore(Restore::Crashed)
        } else {
            spec.event(Ev::co("c", 100.0, 0))
        }
    });
    let result = run(&lab, &records);
    assert_eq!(
        result.verdict,
        Verdict::Pass,
        "{:?}",
        result
            .groups
            .iter()
            .map(|item| &item.criteria)
            .collect::<Vec<_>>()
    );
    result
}

#[test_case::test_case("Verdict: **PASS**"; "verdict")]
#[test_case::test_case("| Scenario | Mode | Slope | Complete seeds | Safety | Usefulness | Status |"; "scenario table")]
#[test_case::test_case("Model-based diagnostic"; "model based diagnostic")]
#[test_case::test_case("Seeds with zero total daily interruptions"; "zero interruption seeds")]
#[test_case::test_case("Per-unit totals"; "per unit totals")]
#[test_case::test_case("Per-source risk"; "per source risk")]
#[test_case::test_case("Joint settled-day distribution"; "settled day distribution")]
#[test_case::test_case("| Event | Day | Period | Phase |"; "event table")]
#[test_case::test_case("restoration fw"; "restoration identity")]
#[test_case::test_case("crashed (crash)"; "crashed restoration")]
#[test_case::test_case("Allowance: limit 2"; "allowance")]
#[test_case::test_case("## Runtime"; "runtime heading")]
#[test_case::test_case("no completion time is forecast"; "no forecast")]
fn the_seed_render_reports_every_account(needle: &str) {
    let text = render_seeds(&account_score());
    assert!(text.contains(needle), "missing {needle:?}");
}

#[test]
fn the_summary_render_omits_the_seed_sections() {
    let result = account_score();
    assert!(render(&result).len() < render_seeds(&result).len());
}

#[test]
fn the_full_registered_product_is_scored_cell_by_cell() {
    let lab = Lab::full();
    let records = good(&lab);
    assert_eq!(records.len(), 23 * 200 * 2);
    let result = super::score(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::Registered),
        &records,
    )
    .expect("the registered full product scores through the public path");
    assert_eq!(
        (result.expected_cells, result.complete_cells),
        (9_200, 9_200)
    );
    assert!(
        result.complete(),
        "{:?} {:?}",
        result.faults.first(),
        result.issues
    );
    assert_eq!(result.groups.len(), 46);
    assert!(result.groups.iter().all(|item| item.seeds.len() == 200));
    // The synthetic climbs reach five counts, far short of the 44-count floor, so exactly that scenario fails.
    assert_eq!(result.verdict, Verdict::Fail);
    for item in &result.groups {
        let expected = if item.scenario == "successor-no-write-limit-failure" {
            GroupStatus::NotMet
        } else {
            GroupStatus::Met
        };
        assert_eq!(
            item.status, expected,
            "{} {:?}",
            item.scenario, item.criteria
        );
    }
    assert_eq!(
        result.scenario_status("successor-no-write-limit-failure"),
        GroupStatus::NotMet
    );
    assert_eq!(
        result.scenario_status("successor-stock-background"),
        GroupStatus::Met
    );
    assert!(
        group(&result, "successor-stock-background", 2.0)
            .criteria
            .iter()
            .any(|item| item.name == "retention")
    );
}

#[test]
fn the_public_scorer_rejects_a_registration_that_changes_settled_values() {
    let mut lab = Lab::full();
    let selection = lab.selection(Split::Dev, RunProfile::Registered);
    lab.registration.criteria.allowance_limit += 1;
    assert!(matches!(
        super::score(&lab.registration, &lab.sha, &selection, &[]),
        Err(ScoreError::Registration(_))
    ));
    let mut lab = Lab::full();
    lab.registration.scenarios.clear();
    assert!(matches!(
        super::score(&lab.registration, &lab.sha, &selection, &[]),
        Err(ScoreError::Registration(_))
    ));
}

#[test_case::test_case(0, 2.9957; "no events")]
#[test_case::test_case(1, 4.7439; "one event")]
#[test_case::test_case(2, 6.2958; "two events")]
fn the_poisson_upper_mean_matches_the_exact_table(k: u64, expected: f64) {
    let mean = poisson_upper_mean(k, CONFIDENCE);
    assert!((mean - expected).abs() < 1e-3, "k={k}: {mean}");
}

#[test_case::test_case(9, 15.7052; "nine events")]
#[test_case::test_case(10, 16.9622; "ten events")]
#[test_case::test_case(30, 40.6904; "thirty events")]
fn the_poisson_upper_mean_matches_the_chi_square_table_at_larger_counts(k: u64, expected: f64) {
    let mean = poisson_upper_mean(k, CONFIDENCE);
    assert!((mean - expected).abs() < 1e-3, "k={k}: {mean}");
}

#[test_case::test_case(100; "a hundred events")]
#[test_case::test_case(300; "three hundred events")]
fn the_poisson_upper_mean_puts_exactly_five_percent_in_the_lower_tail(k: u64) {
    // Independent reference: ascending terms of the cumulative distribution, no log-space sum.
    let mean = poisson_upper_mean(k, CONFIDENCE);
    let mut term = (-mean).exp();
    assert!(term > 0.0, "the reference sum must not underflow at k={k}");
    let mut cumulative = term;
    for index in 1..=k {
        term *= mean / index as f64;
        cumulative += term;
    }
    assert!((cumulative - 0.05).abs() < 1e-9, "k={k}: {cumulative}");
    assert!(mean > k as f64);
}

#[test_case::test_case(1, 1.0, &[8, 9, 10, 11, 12, 13, 14, 15]; "a stop at the start closes every core of its CCD and no other")]
#[test_case::test_case(0, 30.0, &[4, 5, 6, 7]; "only the later promotions of its cores are closed")]
#[test_case::test_case(1, 200.0, &[]; "a stop after the last promotion closes nothing")]
fn an_inherited_coarse_stop_closes_only_its_own_cores(ccd: usize, stop_day: f64, closed: &[usize]) {
    // Core units and coarse units are separate identity spaces: stopping coarse unit 1 never
    // stops the core numbered 1, which belongs to coarse unit 0.
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut record = built(
        &lab,
        &Spec::new("successor-edge-deep", 20_001, SLOPE).mode(UnitMode::PerCore),
    );
    record.policy.inherited_stops = vec![UnitFinish {
        unit: uid(ccd),
        reason: FinishReason::InheritedStop,
        day: stop_day,
        witnesses: vec![format!("ccd-{ccd}-stock")],
    }];
    record
        .validate()
        .expect("the inherited coarse stop is a valid record");
    let seed = metrics(&lab, &record);
    assert!(
        seed.learning
            .violations
            .iter()
            .all(|violation| violation.rule == "promotion-after-parent-stop"),
        "{:?}",
        seed.learning.violations
    );
    let flagged: Vec<String> = seed
        .learning
        .violations
        .iter()
        .map(|violation| violation.subject.clone())
        .collect();
    let expected: Vec<String> = closed.iter().map(|core| format!("climb-{core}")).collect();
    assert_eq!(flagged, expected);
}

#[test]
fn a_promotion_cannot_claim_a_hold_its_intervals_do_not_verify() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut record = built(&lab, &Spec::new("successor-edge-deep", 20_001, SLOPE));
    assert!(rules_of(&metrics(&lab, &record)).is_empty());
    // The interval before the second promotion verified ten hours, not the claimed fifty.
    record.intervals[1].verified_daily_active_hours = 10.0;
    assert_eq!(
        rules_of(&metrics(&lab, &record)),
        vec!["promotion-verified-hold"]
    );
}

#[test]
fn a_suspect_event_inside_the_hold_restarts_the_hold() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let spec = Spec::new("successor-edge-deep", 20_001, SLOPE).event(Ev::co("inside", 7.0, 0));
    // The stock interruption leaves only eight verified clean hours before the first promotion.
    assert_eq!(
        rules_of(&metrics(&lab, &built(&lab, &spec))),
        vec!["promotion-verified-hold"]
    );
}

#[test]
fn a_qualification_dated_before_its_truth_context_is_refused() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut record = built(&lab, &Spec::new("successor-edge-deep", 20_001, SLOPE));
    let StepKind::Climb { qualification, .. } = &mut record.steps[0].kind else {
        unreachable!("the first application is a promotion")
    };
    qualification.day = 0.5;
    assert_eq!(
        rules_of(&metrics(&lab, &record)),
        vec!["qualification-date-context"]
    );
}

#[test_case::test_case(1.0 - 1e-6, true; "just under the budget")]
#[test_case::test_case(1.0 + 1e-6, false; "just over the budget")]
fn the_post_finish_risk_budget_has_no_tolerance(scale: f64, within: bool) {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut record = built(
        &lab,
        &Spec::new("successor-edge-deep", 20_001, SLOPE).ratio(1.0),
    );
    for interval in record
        .intervals
        .iter_mut()
        .filter(|interval| interval.phase == Phase::PostFinish)
    {
        for source in &mut interval.sources {
            source.expected_failures *= scale;
        }
    }
    assert_eq!(metrics(&lab, &record).risk.rate_ok, within);
}

#[test]
fn a_reduced_run_with_a_missing_cell_is_incomplete_and_prints_no_gate_word() {
    let lab = Lab::new(&["successor-edge-deep"], 3);
    let mut records = lab.records(Split::Dev, RunProfile::ReducedDev, |spec| spec);
    records.remove(0);
    let result = score(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::ReducedDev),
        &records,
    )
    .expect("score");
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(labels(&result), vec!["missing"]);
    for text in [render(&result), render_seeds(&result)] {
        assert!(
            text.starts_with(&format!("**{REDUCED_DEV_LABEL}**")),
            "{text}"
        );
        assert!(text.contains("INCOMPLETE"));
        assert!(!text.contains("PASS") && !text.contains("FAIL"));
    }
}

/// Comparator records for the registered `shipped` policy, optionally without an allowance ledger.
fn comparator_records(lab: &Lab, shape: fn(Spec) -> Spec, ledger: bool) -> Vec<Record> {
    let policy = lab.registration.policy("shipped").expect("comparator");
    let crate::day_night_successor::registration::SourceIdentity::Pinned { revision, sha256 } =
        &policy.source
    else {
        unreachable!("pinned comparator fixture")
    };
    let mut records = lab.records(Split::Dev, RunProfile::Registered, shape);
    for record in &mut records {
        record
            .provenance
            .policy_identity
            .clone_from(&policy.identity);
        record.provenance.source_revision.clone_from(revision);
        record.provenance.policy_source_sha256.clone_from(sha256);
        record
            .provenance
            .parameters_sha256
            .clone_from(&policy.parameters_sha256);
        if !ledger {
            record.policy.allowance = None;
            record
                .validate()
                .expect("a record without charged events may omit its ledger");
        }
    }
    records
}

fn comparator_selection(lab: &Lab) -> ScoreSelection {
    let mut selection = lab.selection(Split::Dev, RunProfile::Registered);
    selection.policy = "shipped".into();
    selection
}

#[test_case::test_case(|spec| spec; "a passing trajectory")]
#[test_case::test_case(Spec::stock; "a failing trajectory")]
fn a_comparator_is_a_diagnostic_and_never_a_gate(shape: fn(Spec) -> Spec) {
    let lab = Lab::new(&["successor-edge-deep"], 3);
    let records = comparator_records(&lab, shape, true);
    let result = score(
        &lab.registration,
        &lab.sha,
        &comparator_selection(&lab),
        &records,
    )
    .expect("score");
    assert!(result.complete(), "{:?} {:?}", result.faults, result.issues);
    assert_eq!(result.verdict, Verdict::Diagnostic);
    for text in [render(&result), render_seeds(&result)] {
        assert!(text.contains("DIAGNOSTIC — NOT A CANDIDATE GATE"), "{text}");
        assert!(!text.contains("PASS") && !text.contains("FAIL"));
    }
}

#[test]
fn a_comparator_without_an_allowance_ledger_is_scored_truthfully_as_unreported() {
    let lab = Lab::new(&["successor-edge-deep"], 3);
    let records = comparator_records(&lab, |spec| spec, false);
    // The absent ledger survives the strict JSON round trip as absent, never as a zero ledger.
    let mut bytes = Vec::new();
    for record in &records {
        let value = serde_json::to_value(record).expect("serialize");
        assert!(
            value["policy"]
                .get("allowance")
                .is_none_or(serde_json::Value::is_null),
            "{}",
            value["policy"]
        );
        bytes.extend(serde_json::to_vec(&value).expect("serialize"));
        bytes.push(b'\n');
    }
    let read = read_records(&bytes).expect("round trip");
    assert_eq!(read.len(), records.len());
    assert!(read.iter().all(|record| record.policy.allowance.is_none()));

    let result = score(
        &lab.registration,
        &lab.sha,
        &comparator_selection(&lab),
        &read,
    )
    .expect("score");
    assert!(result.complete(), "{:?} {:?}", result.faults, result.issues);
    assert_eq!(result.verdict, Verdict::Diagnostic);
    for item in &result.groups {
        assert!(item.seeds.iter().all(|seed| {
            seed.ledger.is_none() && !seed.learning.reported && seed.learning.violations.is_empty()
        }));
        // An unreported contract is not a met contract.
        assert!(!criterion(item, "learning-contract").passed);
        assert_eq!(
            criterion(item, "learning-contract").share,
            Some(Share::new(0, 3))
        );
    }
    let text = render_seeds(&result);
    assert!(text.contains("Allowance: unreported"), "{text}");
    assert!(text.contains("| unreported |"), "{text}");
}

#[test]
fn a_candidate_without_an_allowance_ledger_is_refused() {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let mut records = good(&lab);
    for record in &mut records {
        record.policy.allowance = None;
        record
            .validate()
            .expect("a record without charged events may omit its ledger");
    }
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(labels(&result), vec!["incompatible"; 4]);
    assert!(result.faults.iter().all(|fault| matches!(
        &fault.kind,
        FaultKind::Incompatible(reasons)
            if reasons.iter().any(|reason| reason.contains("allowance ledger"))
    )));
}

#[test]
fn a_reported_ledger_is_judged_and_an_unreported_one_is_not_a_pass() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut record = built(&lab, &Spec::new("successor-edge-deep", 20_001, SLOPE));
    let reported = metrics(&lab, &record);
    assert!(reported.learning.reported && reported.learning.violations.is_empty());
    record.policy.allowance = None;
    let unreported = metrics(&lab, &record);
    assert!(unreported.ledger.is_none() && !unreported.learning.reported);
    assert!(unreported.learning.promotions.is_empty());
    // Stops and the newest step are kept outside the missing ledger.
    assert_eq!(unreported.policy_evidence, reported.policy_evidence);
}

#[test]
fn a_nearby_slope_is_not_the_registered_slope() {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let nearby = SLOPE + 1e-12;
    let mut records = good(&lab);
    records[0].provenance.slope = nearby;
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    let mut found = labels(&result);
    found.sort_unstable();
    assert_eq!(found, vec!["missing", "unexpected"]);
    let mut selection = lab.selection(Split::Dev, RunProfile::Registered);
    selection.slopes = Some(vec![nearby]);
    assert!(matches!(
        score(&lab.registration, &lab.sha, &selection, &records),
        Err(ScoreError::UnregisteredSlope(_))
    ));
}

#[test_case::test_case(|_| Some(("shared".into(), 1_000.0)), Some(1_000.0), 1; "one invocation shared by every record counts once")]
#[test_case::test_case(|index| Some((format!("own-{index}"), 1_000.0)), Some(4_000.0), 4; "distinct invocations add")]
#[test_case::test_case(|index| (index != 0).then(|| ("part".into(), 1_000.0)), None, 1; "one missing timing leaves the total unknown")]
#[test_case::test_case(|_| None, None, 0; "no timing at all is unknown, never zero")]
fn the_invocation_wall_is_the_sum_of_distinct_recorded_invocations(
    timing: fn(usize) -> Option<(String, f64)>,
    total: Option<f64>,
    invocations: usize,
) {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let mut records = good(&lab);
    for (index, record) in records.iter_mut().enumerate() {
        record.runtime.invocation = timing(index).map(|(id, elapsed_seconds)| InvocationTiming {
            id,
            elapsed_seconds,
        });
    }
    let result = run(&lab, &records);
    assert_eq!(result.runtime.invocation_wall_seconds, total);
    assert_eq!(result.runtime.invocations.len(), invocations);
    // Run wall seconds are a separate sum and never stand in for the invocation total.
    assert!((result.runtime.summed_run_wall_seconds - 4.0).abs() < 1e-9);
    if total.is_none() {
        assert!(render(&result).contains("not recorded"));
    }
}

#[test]
fn one_invocation_with_two_elapsed_times_is_incomplete() {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let mut records = good(&lab);
    for record in &mut records {
        record.runtime.invocation = Some(InvocationTiming {
            id: "shared".into(),
            elapsed_seconds: 1_000.0,
        });
    }
    records[0]
        .runtime
        .invocation
        .as_mut()
        .expect("timing")
        .elapsed_seconds = 500.0;
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(result.runtime.invocation_wall_seconds, None);
    assert!(
        result
            .issues
            .iter()
            .any(|issue| issue.contains("inconsistent invocation timing: shared")),
        "{:?}",
        result.issues
    );
}

#[test]
fn an_invocation_beyond_the_registered_budget_is_incompatible() {
    let lab = Lab::new(&["successor-edge-deep"], 2);
    let mut records = good(&lab);
    records[0]
        .runtime
        .invocation
        .as_mut()
        .expect("timing")
        .elapsed_seconds = 50_000.0;
    let result = run(&lab, &records);
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(labels(&result), vec!["incompatible"], "{:?}", result.faults);
}
