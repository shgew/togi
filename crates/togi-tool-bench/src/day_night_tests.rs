use std::num::NonZeroUsize;

use crate::day_night::Registration;

fn document() -> serde_json::Value {
    serde_json::json!({
        "id":"test-gate", "version":1, "policy_status":"pending-1217",
        "lambda_per_hour":1.0/3000.0, "policy_slopes":[1.5,2.0],
        "seeds":{"dev":{"first":1,"count":2},"holdout":{"first":101,"count":2}},
        "calendar":{"days":730,"daily_hours":8,"night_hours":8,"night_weekdays":[0,2,4],"year_days":365,"firmware_change_day":180,"sleep":"no exposure"},
        "thresholds":{"covered_crash_free_fraction":0.9,"hidden_year_one_max_crashes_per_ccd":2,"hidden_year_one_fraction":0.9,"overshoot_one_count_fraction":0.95,"overshoot_absolute_max_counts":3,"median_convergence_days":90,"retention_max_lost_counts":1,"retention_fraction":0.9,"fixed_cap_max_steps_per_ccd":7},
        "censored":"failure", "tail_criteria":"worst CCD",
        "scenarios":[{"name":"edge-shallow","daily_class":"covered","cap":"fixed"}],"notes":[]
    })
}

pub(super) fn registration() -> Registration {
    Registration::parse(&serde_json::to_vec(&document()).unwrap()).unwrap()
}

#[test]
fn seed_ranges_are_disjoint_and_exact() {
    let registration = registration();
    assert_eq!(
        registration.seeds.dev.values().unwrap().collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        registration
            .seeds
            .holdout
            .values()
            .unwrap()
            .collect::<Vec<_>>(),
        [101, 102]
    );
}

#[test]
fn refuses_overlapping_development_and_holdout() {
    let mut document = document();
    document["seeds"]["holdout"]["first"] = 2.into();
    assert!(Registration::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn refuses_empty_seed_range() {
    let mut document = document();
    document["seeds"]["dev"]["count"] = 0.into();
    assert!(Registration::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn refuses_calendar_that_credits_more_than_wall_time() {
    let mut document = document();
    document["calendar"]["daily_hours"] = 20.into();
    assert!(Registration::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn refuses_unrecognized_registration_fields() {
    let mut document = document();
    document["unregistered_override"] = true.into();
    assert!(Registration::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn refuses_positional_registration_document() {
    assert!(Registration::parse(b"[]").is_err());
}

fn successful_metrics() -> togi_simrun::day_night::Metrics {
    togi_simrun::day_night::Metrics {
        convergence_days: vec![Some(90), Some(89)],
        max_overshoot: vec![1, 0],
        promotions: vec![7, 6],
        daily_co_crashes: vec![0, 0],
        year_one_co_crashes: vec![0, 0],
        climb_co_crashes: vec![0, 0],
        cpu_suspect_events: 0,
        background_retreat_counts: vec![0, 0],
        justified_retreat_counts: vec![0, 0],
        non_cpu_retreat_counts: vec![0, 0],
        unattributed_retreat_counts: vec![0, 0],
        converged_depth: vec![Some(25), Some(30)],
        final_depth: vec![24, 29],
        censored: false,
        fixed_cap: true,
        active_hours: 5840.0,
        night_hours: 2504.0,
        sleep_hours: 9176.0,
        true_co_crashes: 0,
        year_one_true_co_crashes: 0,
        non_cpu_events: 0,
        background_events: 0,
    }
}

#[test]
fn gate_uses_worst_ccd_and_accepts_exact_threshold_boundaries() {
    let registration = registration();
    let metrics = successful_metrics();
    let criteria =
        crate::day_night_report::judge(&registration, &registration.scenarios[0], &[&metrics]);
    assert!(criteria.passed());
}

#[test]
fn censored_seed_cannot_pass_by_its_zero_partial_crash_count() {
    let registration = registration();
    let mut metrics = successful_metrics();
    metrics.censored = true;
    let criteria =
        crate::day_night_report::judge(&registration, &registration.scenarios[0], &[&metrics]);
    assert_eq!(criteria.covered_climb, Some(false));
    assert!(!criteria.overshoot_absolute);
    assert!(!criteria.convergence);
    assert_eq!(criteria.fixed_cap_steps, Some(false));
}

#[test]
fn overshoot_absolute_maximum_cannot_hide_in_a_percentile() {
    let registration = registration();
    let mut metrics = successful_metrics();
    metrics.max_overshoot[1] = 4;
    let criteria =
        crate::day_night_report::judge(&registration, &registration.scenarios[0], &[&metrics]);
    assert!(!criteria.overshoot_absolute);
}

#[test]
fn retention_is_checked_per_ccd_not_as_a_depth_sum() {
    let mut registration = registration();
    registration.scenarios[0].retention = true;
    let mut metrics = successful_metrics();
    metrics.final_depth = vec![23, 31];
    let criteria =
        crate::day_night_report::judge(&registration, &registration.scenarios[0], &[&metrics]);
    assert_eq!(criteria.retention, Some(false));
}

#[test]
fn gate_refuses_missing_seeds_instead_of_changing_the_denominator() {
    let registration = registration();
    let records = [crate::day_night_report::Record {
        scenario: "edge-shallow".to_owned(),
        seed: 1,
        comparator: "test-policy".to_owned(),
        wall_s: 0.0,
        metrics: successful_metrics(),
    }];
    assert!(matches!(
        crate::day_night_report::score(
            &registration,
            crate::Selection::Dev,
            "test-policy",
            &records
        ),
        Err(crate::day_night_report::ScoreError::SeedsMismatch(_))
    ));
}

#[test_case::test_case(1, true; "exactly ninety five percent")]
#[test_case::test_case(2, false; "ninety percent is insufficient")]
fn overshoot_tail_uses_exact_seed_fraction(failures: usize, expected: bool) {
    let registration = registration();
    let mut metrics = vec![successful_metrics(); 20];
    for metric in metrics.iter_mut().take(failures) {
        metric.max_overshoot[1] = 2;
    }
    let metrics = metrics.iter().collect::<Vec<_>>();
    let criteria =
        crate::day_night_report::judge(&registration, &registration.scenarios[0], &metrics);
    assert_eq!(criteria.overshoot_fraction, expected);
    assert!(criteria.overshoot_absolute);
}

#[test_case::test_case([3, 0], 1, 0, true; "exactly ninety percent of seeds")]
#[test_case::test_case([3, 0], 2, 0, false; "below ninety percent of seeds")]
#[test_case::test_case([2, 0], 10, 0, true; "two first-year crashes pass")]
#[test_case::test_case([3, 0], 10, 0, false; "three first-year crashes fail")]
#[test_case::test_case([0, 0], 0, 1, true; "one censored seed leaves ninety percent")]
#[test_case::test_case([0, 0], 0, 2, false; "censored seeds count as failures")]
#[test_case::test_case([2, 2], 10, 0, true; "each ccd is judged separately")]
#[test_case::test_case([0, 3], 10, 0, false; "worst ccd decides")]
fn hidden_year_one_gate(first_year: [u32; 2], offenders: usize, censored: usize, expected: bool) {
    let mut registration = registration();
    registration.scenarios[0].daily_class = crate::day_night::DailyCoverage::Hidden;
    let mut metrics = vec![successful_metrics(); 10];
    for metric in metrics.iter_mut().take(offenders) {
        metric.year_one_co_crashes = first_year.to_vec();
    }
    for metric in metrics.iter_mut().skip(offenders).take(censored) {
        metric.censored = true;
    }
    let metrics = metrics.iter().collect::<Vec<_>>();
    let criteria =
        crate::day_night_report::judge(&registration, &registration.scenarios[0], &metrics);
    assert_eq!(criteria.hidden_year_one, Some(expected));
    assert_eq!(criteria.covered_climb, None);
}

type Call = (
    String,
    u64,
    togi_simrun::day_night::PolicyKind,
    u32,
    u32,
    f64,
    f64,
    f64,
    Vec<u8>,
    Option<u32>,
);

type Written = Vec<(String, u64, String)>;

fn orchestration(selection: crate::Selection) -> (Vec<Call>, Written, Vec<String>) {
    let mut document = document();
    document["lambda_per_hour"] = 0.0005.into();
    document["calendar"]["daily_hours"] = 6.into();
    document["calendar"]["night_hours"] = 10.into();
    document["calendar"]["night_weekdays"] = serde_json::json!([1, 3]);
    document["calendar"]["firmware_change_day"] = 200.into();
    document["scenarios"] = serde_json::json!([
        {"name":"edge-shallow","daily_class":"covered","cap":"fixed"},
        {"name":"firmware-change","daily_class":"hidden","cap":"firmware"}
    ]);
    let registration = Registration::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
    let calls = std::sync::Mutex::new(Vec::new());
    let run = |scenario: &str,
               seed: u64,
               comparator: togi_simrun::day_night::PolicyKind,
               calendar: togi_simrun::day_night::CalendarConfig| {
        calls.lock().unwrap().push((
            scenario.to_owned(),
            seed,
            comparator,
            calendar.days,
            calendar.year_days,
            calendar.lambda_per_hour,
            calendar.active_hours,
            calendar.night_hours,
            calendar.night_weekdays.clone(),
            calendar.firmware_change_day,
        ));
        let mut metrics = successful_metrics();
        if comparator == togi_simrun::day_night::PolicyKind::CoZero {
            metrics.true_co_crashes = u32::try_from(seed).unwrap();
            metrics.cpu_suspect_events = 1;
        } else if scenario == "firmware-change" && seed.is_multiple_of(2) {
            metrics.background_retreat_counts = vec![2, 1];
        }
        Ok(metrics)
    };
    let mut jsonl = Vec::new();
    let records = crate::day_night_report::run_policies(
        &registration,
        selection,
        &togi_simrun::day_night::PolicyKind::COMPARATORS,
        NonZeroUsize::MIN,
        &run,
        &mut jsonl,
    )
    .unwrap();
    let mut table = Vec::new();
    crate::day_night_report::report(
        &mut table,
        &registration,
        &togi_simrun::day_night::PolicyKind::COMPARATORS,
        &records,
    )
    .unwrap();
    let written = String::from_utf8(jsonl)
        .unwrap()
        .lines()
        .map(|line| {
            let record: crate::day_night_report::Record = serde_json::from_str(line).unwrap();
            (record.scenario, record.seed, record.comparator)
        })
        .collect();
    let table = String::from_utf8(table)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    (calls.into_inner().unwrap(), written, table)
}

#[test_case::test_case(crate::Selection::Dev, [1, 2]; "development seeds")]
#[test_case::test_case(crate::Selection::Holdout, [101, 102]; "holdout seeds")]
fn orchestration_runs_only_the_selected_seeds_and_reports_them(
    selection: crate::Selection,
    seeds: [u64; 2],
) {
    use togi_simrun::day_night::PolicyKind;
    let (calls, written, table) = orchestration(selection);
    let mut expected_calls = Vec::new();
    let mut expected_written = Vec::new();
    for scenario in ["edge-shallow", "firmware-change"] {
        for seed in seeds {
            for (label, comparator) in [
                ("co-zero", PolicyKind::CoZero),
                ("fixed-margin", PolicyKind::FixedMargin),
            ] {
                expected_calls.push((
                    scenario.to_owned(),
                    seed,
                    comparator,
                    730,
                    365,
                    0.0005,
                    6.0,
                    10.0,
                    vec![1, 3],
                    (scenario == "firmware-change").then_some(200),
                ));
                expected_written.push((scenario.to_owned(), seed, label.to_owned()));
            }
        }
    }
    assert_eq!(calls, expected_calls);
    assert_eq!(written, expected_written);

    let crashes = seeds.iter().sum::<u64>();
    // Exactly one seed per range is even, and it charges 2 + 1 background retreats.
    let bg = 3;
    assert_eq!(
        table,
        [
            "test-gate: policy summary; only ruleset13 can pass the gate; registered slopes [1.5, 2.0]".to_owned(),
            "scenario comparator seeds CO/day suspect year1/CCD climb-free% converge-d overshoot steps final-depth retained% bg-retreat CO-retreat nonCPU-retreat censored".to_owned(),
            format!("edge-shallow co-zero 2 {crashes} 2 0 100.0 90.0 1 7 26.5 100.0 0 0 0 0"),
            "edge-shallow fixed-margin 2 0 0 0 100.0 90.0 1 7 26.5 100.0 0 0 0 0".to_owned(),
            format!("firmware-change co-zero 2 {crashes} 2 0 100.0 90.0 1 7 26.5 100.0 0 0 0 0"),
            format!("firmware-change fixed-margin 2 0 0 0 100.0 90.0 1 7 26.5 100.0 {bg} 0 0 0"),
            "CO/day counts unique CO-caused daily crashes over seeds; retreats sum per-CCD counts; year1/CCD, overshoot and steps are maxima; convergence is median worst-CCD day (inf = not converged); final-depth is median per-run mean CCD depth. Per-CCD JSON crash counts can charge both CCDs for a joint crash. Censored partial counts are lower bounds.".to_owned(),
        ]
    );
}

mod selection {
    use test_case::test_case;
    use togi_simrun::day_night::{PolicyError, PolicyKind};

    use super::registration;
    use crate::day_night_gate::{SelectError, select_policies};

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test_case(&[], &[PolicyKind::CoZero, PolicyKind::FixedMargin]; "no name runs every diagnostic")]
    #[test_case(&["fixed-margin"], &[PolicyKind::FixedMargin]; "one named policy")]
    #[test_case(&["fixed-margin", "co-zero"], &[PolicyKind::FixedMargin, PolicyKind::CoZero]; "repeats keep their order")]
    fn policies_parse_in_the_order_given(given: &[&str], expected: &[PolicyKind]) {
        assert_eq!(
            select_policies(&registration(), &names(given), None).unwrap(),
            expected
        );
    }

    #[test]
    fn an_unknown_policy_names_the_known_ones() {
        let error = select_policies(&registration(), &names(&["ruleset14"]), None).unwrap_err();
        assert!(matches!(
            error,
            SelectError::Policy(PolicyError::Unknown { ref name, ref known })
                if name == "ruleset14" && known == "co-zero, fixed-margin, ruleset13"
        ));
    }

    #[test]
    fn a_policy_cannot_be_selected_twice() {
        assert!(matches!(
            select_policies(&registration(), &names(&["co-zero", "co-zero"]), None),
            Err(SelectError::Duplicate("co-zero"))
        ));
    }

    #[test_case(&[]; "the default comparators")]
    #[test_case(&["co-zero"]; "a named comparator")]
    fn a_registered_slope_is_still_refused_by_a_policy_that_takes_none(given: &[&str]) {
        assert!(matches!(
            select_policies(&registration(), &names(given), Some(1.5)),
            Err(SelectError::Policy(PolicyError::SlopeUnused(_)))
        ));
    }

    #[test_case(&["ruleset13"], 1.5; "the gate at the first slope")]
    #[test_case(&["ruleset13"], 2.0; "the gate at the second slope")]
    fn the_gating_policy_takes_a_registered_slope(given: &[&str], slope: f64) {
        assert_eq!(
            select_policies(&registration(), &names(given), Some(slope)).unwrap(),
            [PolicyKind::Ruleset13 {
                fallback_slope: slope
            }]
        );
    }

    #[test]
    fn the_gating_policy_refuses_to_run_without_a_slope() {
        assert!(matches!(
            select_policies(&registration(), &names(&["ruleset13"]), None),
            Err(SelectError::Policy(PolicyError::SlopeRequired("ruleset13")))
        ));
    }

    #[test_case(1.25; "between nothing registered")]
    #[test_case(3.0; "above every registered slope")]
    fn an_unregistered_slope_is_refused(slope: f64) {
        let error =
            select_policies(&registration(), &names(&["co-zero"]), Some(slope)).unwrap_err();
        assert!(matches!(
            error,
            SelectError::SlopeNotRegistered { registered, .. } if registered == [1.5, 2.0]
        ));
    }
}

mod parallel {
    use std::num::NonZeroUsize;
    use std::sync::{Condvar, Mutex, PoisonError};

    use togi_simrun::day_night::PolicyKind;

    use super::{document, successful_metrics};
    use crate::day_night::Registration;
    use crate::day_night_report::run_policies;

    fn two_scenarios() -> Registration {
        let mut document = document();
        document["scenarios"] = serde_json::json!([
            {"name":"edge-shallow","daily_class":"covered","cap":"fixed"},
            {"name":"firmware-change","daily_class":"hidden","cap":"firmware"}
        ]);
        Registration::parse(&serde_json::to_vec(&document).unwrap()).unwrap()
    }

    /// Holds the callers of [`Gate::wait`] until another thread has called [`Gate::open`].
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        opened: Condvar,
    }

    impl Gate {
        fn open(&self) {
            *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.opened.notify_all();
        }

        fn wait(&self) {
            let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            drop(
                self.opened
                    .wait_while(open, |open| !*open)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }

    type Keys = Vec<(String, u64, String)>;

    fn keys(jobs: usize) -> (Keys, Keys) {
        let registration = two_scenarios();
        // With several workers the first registered run holds until the last registered run has returned, so a
        // pool that wrote in completion order would reverse it. One worker cannot overlap runs.
        let last_returned = Gate::default();
        let run = |scenario: &str, seed: u64, policy: PolicyKind, _| {
            let first = scenario == "edge-shallow" && seed == 1 && policy == PolicyKind::CoZero;
            let last =
                scenario == "firmware-change" && seed == 2 && policy == PolicyKind::FixedMargin;
            if first && jobs > 1 {
                last_returned.wait();
            }
            if last {
                last_returned.open();
            }
            Ok(successful_metrics())
        };
        let mut jsonl = Vec::new();
        let records = run_policies(
            &registration,
            crate::Selection::Dev,
            &PolicyKind::COMPARATORS,
            NonZeroUsize::new(jobs).unwrap(),
            &run,
            &mut jsonl,
        )
        .unwrap();
        let key = |scenario: String, seed, policy| (scenario, seed, policy);
        let returned = records
            .into_iter()
            .map(|record| key(record.scenario, record.seed, record.comparator))
            .collect();
        let written = String::from_utf8(jsonl)
            .unwrap()
            .lines()
            .map(|line| {
                let record: crate::day_night_report::Record = serde_json::from_str(line).unwrap();
                assert!(record.wall_s > 0.0);
                key(record.scenario, record.seed, record.comparator)
            })
            .collect();
        (returned, written)
    }

    #[test]
    fn records_and_jsonl_keep_registered_order_whatever_order_threads_finish_in() {
        let mut expected = Vec::new();
        for scenario in ["edge-shallow", "firmware-change"] {
            for seed in [1, 2] {
                for policy in ["co-zero", "fixed-margin"] {
                    expected.push((scenario.to_owned(), seed, policy.to_owned()));
                }
            }
        }
        for jobs in [1, 3, 16] {
            let (returned, written) = keys(jobs);
            assert_eq!(returned, expected, "{jobs} jobs");
            assert_eq!(written, expected, "{jobs} jobs");
        }
    }

    #[test]
    fn the_first_failing_run_in_order_is_the_error_even_when_others_fail_sooner() {
        let registration = two_scenarios();
        // Every earlier-registered run fails only after a later-registered run has failed.
        let later_failed = Gate::default();
        let run = |scenario: &str, _, _, _| {
            if scenario == "edge-shallow" {
                later_failed.wait();
            } else {
                later_failed.open();
            }
            Err(togi_simrun::day_night::RunError::Calendar(
                if scenario == "edge-shallow" {
                    "first"
                } else {
                    "later"
                },
            ))
        };
        let error = run_policies(
            &registration,
            crate::Selection::Dev,
            &PolicyKind::COMPARATORS,
            NonZeroUsize::new(8).unwrap(),
            &run,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("run day/night"), "{error}");
        assert!(
            matches!(
                error,
                crate::day_night_report::ComparatorError::Run(
                    togi_simrun::day_night::RunError::Calendar("first")
                )
            ),
            "{error:?}"
        );
    }
}

mod verdict {
    use test_case::test_case;
    use togi_simrun::day_night::{PolicyKind, PolicyRole};

    use super::{registration, successful_metrics};
    use crate::day_night_gate::{Verdict, any_fails_gate, render_summary, render_verdict};
    use crate::day_night_report::Record;

    fn records(policy: PolicyKind, overshoot: u8) -> Vec<Record> {
        [1, 2]
            .into_iter()
            .map(|seed| {
                let mut metrics = successful_metrics();
                metrics.max_overshoot[0] = overshoot;
                Record {
                    scenario: "edge-shallow".to_owned(),
                    seed,
                    comparator: policy.label().to_owned(),
                    wall_s: 1.5 * f64::from(u32::try_from(seed).unwrap()),
                    metrics,
                }
            })
            .collect()
    }

    fn verdict(role: PolicyRole, overshoot: u8) -> Verdict {
        let mut verdict = Verdict::compute(
            &registration(),
            crate::Selection::Dev,
            PolicyKind::FixedMargin,
            &records(PolicyKind::FixedMargin, overshoot),
        )
        .unwrap();
        verdict.role = role;
        verdict
    }

    fn render(verdict: &Verdict) -> String {
        let mut out = Vec::new();
        render_verdict(&mut out, &registration(), verdict).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_comparator_table_is_labelled_diagnostic_and_a_pass_is_not_a_gate_pass() {
        assert_eq!(
            render(&verdict(PolicyRole::Diagnostic, 1)),
            "\
### fixed-margin: diagnostic (not a gate pass)

| scenario | climb-free | year-one | overshoot ≤1 | overshoot max | median convergence d | retained | max steps | censored | result |
|---|---|---|---|---|---|---|---|---|---|
| edge-shallow | 1.000 ≥ 0.90 | — | 1.000 ≥ 0.95 | 1 ≤ 3 | 90.0 ≤ 90 | — | 7 ≤ 7 | 0/2 | PASS |

test-gate: PASS (fixed-margin, diagnostic, not a gate pass)
"
        );
    }

    #[test]
    fn a_gating_failure_marks_the_failing_cell_and_the_overall_line() {
        assert_eq!(
            render(&verdict(PolicyRole::Gate, 4)),
            "\
### fixed-margin

| scenario | climb-free | year-one | overshoot ≤1 | overshoot max | median convergence d | retained | max steps | censored | result |
|---|---|---|---|---|---|---|---|---|---|
| edge-shallow | 1.000 ≥ 0.90 | — | 0.000 ≥ 0.95 ✗ | 4 ≤ 3 ✗ | 90.0 ≤ 90 | — | 7 ≤ 7 | 0/2 | FAIL |

test-gate: FAIL
"
        );
    }

    #[test_case(PolicyRole::Gate, 1, false; "a passing gate exits zero")]
    #[test_case(PolicyRole::Gate, 4, true; "a failing gate exits nonzero")]
    #[test_case(PolicyRole::Diagnostic, 1, false; "a passing comparator exits zero")]
    #[test_case(PolicyRole::Diagnostic, 4, false; "a failing comparator still exits zero")]
    fn only_a_failing_gating_policy_fails_the_command(
        role: PolicyRole,
        overshoot: u8,
        fails: bool,
    ) {
        assert_eq!(any_fails_gate(&[verdict(role, overshoot)]), fails);
    }

    #[test]
    fn one_failing_gate_among_passing_verdicts_fails_the_command() {
        assert!(any_fails_gate(&[
            verdict(PolicyRole::Diagnostic, 4),
            verdict(PolicyRole::Gate, 1),
            verdict(PolicyRole::Gate, 4),
        ]));
    }

    #[test]
    fn the_summary_reports_means_per_scenario_and_policy_and_the_total_wall_time() {
        let mut out = Vec::new();
        render_summary(
            &mut out,
            &registration(),
            &[PolicyKind::FixedMargin],
            &records(PolicyKind::FixedMargin, 1),
            std::time::Duration::from_millis(12_340),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
| scenario | policy | runs | mean night hours | mean run wall s |
|---|---|---|---|---|
| edge-shallow | fixed-margin | 2 | 2504.0 | 2.25 |

total wall time: 12.3 s
"
        );
    }
}
