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
