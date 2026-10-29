//! Regression tests for the per-record measures and the registered fractions: retreat losses by true
//! cause, sources first seen after the finish, the large-count Poisson reference, mixed-seed groups at
//! the registered fractions, the per-unit depth floor beside the aggregate one, never-finished seeds and
//! the exact risk budget. Records are synthetic arithmetic stand-ins built by the shared fixture and
//! edited one rule at a time; every expectation is derived by hand from the registered numbers.

use togi_machine::PerCore;
use togi_simrun::day_night::successor::{
    Admissibility, Outcome, Period, Phase, ProfileStep, Record, RiskSource, RunProfile, SourceRisk,
    Split, StepKind, TruthCause,
};

use super::super::fixture::{Ev, Lab, Spec, resync, retopologize, uid};
use super::super::rules::{Rules, Scope};
use super::super::stats::{CONFIDENCE, poisson_upper_mean, poisson_upper_rate};
use super::super::{Criterion, Group, Score, Share, score_validated as score};
use super::{SeedMetrics, measure};

const SLOPE: f64 = 1.5;
const EDGE_DEEP: &str = "successor-edge-deep";
const HIDDEN_DAILY: &str = "successor-hidden-daily";
const STOCK_BACKGROUND: &str = "successor-stock-background";
const NO_WRITE_LIMIT: &str = "successor-no-write-limit-failure";
const FIRST_SEED: u64 = 20_001;

fn built(lab: &Lab, spec: &Spec) -> Record {
    let record = spec.build(lab);
    validated(&record);
    record
}

fn validated(record: &Record) {
    record
        .validate()
        .unwrap_or_else(|error| panic!("fixture record is invalid: {error}"));
}

fn metrics(lab: &Lab, record: &Record) -> SeedMetrics {
    let scenario = lab
        .registration
        .scenario(&record.provenance.scenario)
        .expect("scenario");
    measure(record, &Rules::new(&lab.registration), Scope::of(scenario)).expect("measure")
}

fn run(lab: &Lab, records: &[Record]) -> Score {
    let result = score(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::Registered),
        records,
    )
    .expect("score");
    assert!(result.faults.is_empty(), "{:?}", result.faults);
    result
}

fn group<'a>(score: &'a Score, scenario: &str) -> &'a Group {
    score
        .groups
        .iter()
        .find(|group| group.scenario == scenario && (group.slope - SLOPE).abs() < 1e-9)
        .expect("group")
}

fn criterion<'a>(group: &'a Group, name: &str) -> &'a Criterion {
    group
        .criteria
        .iter()
        .find(|item| item.name == name)
        .unwrap_or_else(|| panic!("criterion {name}"))
}

/// A profile with every unit at its own depth.
fn deployed(record: &Record, depths: &[u8]) -> PerCore<u8> {
    let mut profile: PerCore<u8> = record
        .provenance
        .topology
        .iter()
        .map(|core| (core.core, 0))
        .collect();
    for (unit, depth) in record.provenance.membership.iter().zip(depths) {
        for core in &unit.cores {
            profile.set(*core, *depth);
        }
    }
    profile
}

fn uniform(record: &Record, depth: u8) -> PerCore<u8> {
    deployed(record, &vec![depth; record.units.len()])
}

fn profile_at(record: &Record, day: f64) -> PerCore<u8> {
    record
        .intervals
        .iter()
        .find(|interval| interval.start_day <= day && day < interval.end_day)
        .expect("interval at day")
        .profile
}

/// Appends a retreat of the named units to the given depths from the profile deployed at `day`.
fn retreat(record: &mut Record, id: &str, day: f64, event: Option<&str>, to: &[(usize, u8)]) {
    let before = profile_at(record, day);
    let mut after = before;
    for (unit, depth) in to {
        for core in &record.provenance.membership[*unit].cores {
            after.set(*core, *depth);
        }
    }
    record.steps.push(ProfileStep {
        id: id.into(),
        day,
        before,
        after,
        kind: StepKind::Retreat {
            event: event.map(str::to_owned),
            units: to.iter().map(|(unit, _)| uid(*unit)).collect(),
        },
    });
    record.steps.sort_by(|a, b| a.day.total_cmp(&b.day));
}

/// Splits the post-finish interval that strictly contains `at`, sharing hours and expected failures in
/// proportion to the days; a boundary that already exists is left alone.
fn split_at(record: &mut Record, at: f64) {
    let Some(index) = record.intervals.iter().position(|interval| {
        interval.phase == Phase::PostFinish && interval.start_day < at && at < interval.end_day
    }) else {
        return;
    };
    let mut tail = record.intervals[index].clone();
    let head_share = (at - tail.start_day) / (tail.end_day - tail.start_day);
    let head = &mut record.intervals[index];
    head.end_day = at;
    head.verified_daily_active_hours *= head_share;
    for source in &mut head.sources {
        source.expected_failures *= head_share;
    }
    tail.start_day = at;
    tail.verified_daily_active_hours *= 1.0 - head_share;
    for source in &mut tail.sources {
        source.expected_failures *= 1.0 - head_share;
    }
    record.intervals.insert(index + 1, tail);
}

/// Moves the post-finish intervals in `[from, to]` to hazard ratio 2 (outside A) with four times the
/// expected failures of the default ratio-one-half trajectory.
fn break_range(record: &mut Record, from: f64, to: f64) {
    split_at(record, from);
    split_at(record, to);
    let units = record.units.len();
    for interval in record.intervals.iter_mut().filter(|interval| {
        interval.phase == Phase::PostFinish && interval.start_day >= from && interval.end_day <= to
    }) {
        interval.admissibility = Admissibility {
            joint_ratio: 2.0,
            unit_ratios: vec![2.0; units],
            member: false,
        };
        for source in &mut interval.sources {
            source.expected_failures *= 4.0;
        }
    }
}

/// Gives every post-finish interval from `from` on the same depth on every unit.
fn thin_from(record: &mut Record, from: f64, depth: u8) {
    split_at(record, from);
    let thin = uniform(record, depth);
    for interval in record
        .intervals
        .iter_mut()
        .filter(|interval| interval.phase == Phase::PostFinish && interval.start_day >= from)
    {
        interval.profile = thin;
    }
}

/// One record per expected cell: `shape` builds the trajectory and `damage` edits the built record,
/// both keyed by the seed's position among the registered seeds.
fn records_with(
    lab: &Lab,
    shape: impl Fn(Spec, u64) -> Spec,
    damage: impl Fn(&mut Record, u64),
) -> Vec<Record> {
    let mut records = lab.records(Split::Dev, RunProfile::Registered, |spec| {
        let index = spec.seed - FIRST_SEED;
        shape(spec, index)
    });
    for record in &mut records {
        let index = record.provenance.seed - FIRST_SEED;
        damage(record, index);
        resync(record);
        validated(record);
    }
    records
}

#[test]
fn retreat_losses_and_event_identities_are_split_by_true_cause() {
    let lab = Lab::new(&[EDGE_DEEP], 1);
    let non_cpu = Ev {
        truth: TruthCause::NonCpu {},
        ..Ev::background("non-cpu", 20.0)
    };
    let unattributed = Ev {
        truth: TruthCause::Unattributed {},
        ..Ev::background("unattributed", 30.0)
    };
    let spec = Spec::new(EDGE_DEEP, FIRST_SEED, SLOPE)
        .event(Ev::background("background", 10.0))
        .event(non_cpu)
        .event(unattributed)
        .event(Ev::co("co", 40.0, 0));
    let mut record = built(&lab, &spec);
    // Unit 0 climbs on day 8 and unit 1 on day 15, so both finish by day 15; each unit sits at depth 5.
    retreat(
        &mut record,
        "r-background",
        10.5,
        Some("background"),
        &[(0, 3)],
    );
    retreat(&mut record, "r-non-cpu", 20.5, Some("non-cpu"), &[(1, 4)]);
    retreat(
        &mut record,
        "r-unattributed",
        30.5,
        Some("unattributed"),
        &[(0, 2), (1, 4)],
    );
    retreat(&mut record, "r-co", 40.5, Some("co"), &[(0, 1)]);
    retreat(&mut record, "r-history", 50.0, None, &[(1, 2)]);
    validated(&record);
    let seed = metrics(&lab, &record);

    let loss = |unit: usize| seed.units[unit].losses;
    // Unit 0 lost 5-3 to the background event, 5-2 to the unattributed one and 5-1 to the CO crash.
    assert_eq!(
        (
            loss(0).co,
            loss(0).background,
            loss(0).non_cpu,
            loss(0).unattributed,
            loss(0).history
        ),
        (4, 2, 0, 3, 0)
    );
    // Unit 1 lost 5-4 to the non-CPU event, 5-4 to the unattributed one and 5-2 with no event at all.
    assert_eq!(
        (
            loss(1).co,
            loss(1).background,
            loss(1).non_cpu,
            loss(1).unattributed,
            loss(1).history
        ),
        (0, 0, 1, 1, 3)
    );
    assert_eq!(
        (seed.units[0].retreat_steps, seed.units[1].retreat_steps),
        (3, 3)
    );

    // The shared retreat names one event for two units; each event is still counted once.
    let counts = seed.events;
    assert_eq!(
        (
            counts.day_learning.total,
            counts.day_learning.background,
            counts.day_learning.co
        ),
        (1, 1, 0)
    );
    assert_eq!(
        (
            counts.day_post_finish.total,
            counts.day_post_finish.co,
            counts.day_post_finish.background,
            counts.day_post_finish.non_cpu,
            counts.day_post_finish.unattributed
        ),
        (3, 1, 0, 1, 1)
    );
    assert_eq!(counts.night.total, 0);
    assert_eq!(counts.day().total, 4);
    assert_eq!(seed.daily_co_crashes, 1);

    let labels: Vec<(&str, &str)> = seed
        .event_views
        .iter()
        .map(|view| (view.id.as_str(), view.truth.as_str()))
        .collect();
    assert_eq!(
        labels,
        vec![
            ("background", "background"),
            ("non-cpu", "non-cpu"),
            ("unattributed", "unattributed"),
            ("co", "co (unit 0)"),
        ]
    );
    let implicated: Vec<usize> = seed
        .event_views
        .iter()
        .map(|view| view.implicated.len())
        .collect();
    assert_eq!(implicated, vec![0, 0, 0, 1]);
}

#[test]
fn a_source_first_seen_after_the_finish_gets_a_row_with_zero_learning() {
    let lab = Lab::new(&[EDGE_DEEP], 1);
    let interaction = RiskSource::Interaction {
        identity: "pair".into(),
    };
    let pair = Ev {
        truth: TruthCause::Co {
            source: interaction.clone(),
            implicated_units: vec![uid(0), uid(1)],
        },
        ..Ev::co("pair-crash", 100.0, 0)
    };
    let mut record = built(&lab, &Spec::new(EDGE_DEEP, FIRST_SEED, SLOPE).event(pair));
    for interval in record
        .intervals
        .iter_mut()
        .filter(|interval| interval.phase == Phase::PostFinish)
    {
        interval.sources.push(SourceRisk {
            source: interaction.clone(),
            expected_failures: 1e-4 * interval.verified_daily_active_hours,
        });
    }
    resync(&mut record);
    // The interaction never appears while learning, so the learning accounts omit it entirely.
    record
        .accounts
        .learning
        .retain(|account| account.source != interaction);
    validated(&record);
    let seed = metrics(&lab, &record);

    let sources: Vec<&str> = seed
        .risk
        .sources
        .iter()
        .map(|row| row.source.as_str())
        .collect();
    assert_eq!(sources, vec!["unit 0", "unit 1", "interaction pair"]);
    let row = &seed.risk.sources[2];
    assert_eq!((row.learning_k, row.learning_h), (0, 0.0));
    assert_eq!(row.post_finish_k, 1);
    // Both units finish by day 15, so 716 days of 8 active hours follow, at 1e-4 failures per hour.
    assert!(
        (row.post_finish_h - 0.5728).abs() < 1e-9,
        "{}",
        row.post_finish_h
    );
    assert_eq!((seed.risk.k, seed.risk.learning_k), (1, 0));
}

#[test_case::test_case(100, 118.079_272_78, 2_000.0, 0.059_039_636_39; "a hundred events")]
#[test_case::test_case(1_000, 1_053.603_122_13, 50_000.0, 0.021_072_062_44; "a thousand events")]
fn the_poisson_upper_bound_matches_an_independent_direct_sum_at_large_counts(
    k: u64,
    mean: f64,
    hours: f64,
    rate: f64,
) {
    // References come from bisecting the plain sum of exp(-mu + i ln mu - ln i!) for i <= k at 5%.
    let upper = poisson_upper_mean(k, CONFIDENCE);
    assert!((upper - mean).abs() < 1e-6, "k={k}: {upper}");
    let found = poisson_upper_rate(k, hours).expect("exposure");
    assert!((found - rate).abs() < 1e-9, "k={k}: {found}");
}

#[test_case::test_case(19, 1, false; "eighteen of nineteen is just below ninety-five percent")]
#[test_case::test_case(20, 1, true; "nineteen of twenty is exactly ninety-five percent")]
#[test_case::test_case(21, 1, true; "twenty of twenty-one is just above ninety-five percent")]
#[test_case::test_case(20, 2, false; "eighteen of twenty is below ninety-five percent")]
fn the_held_after_finish_fraction_is_ninety_five_percent(seeds: u32, bad: u64, passes: bool) {
    let lab = Lab::new(&[HIDDEN_DAILY], seeds);
    let records = records_with(
        &lab,
        |spec, _| spec,
        |record, index| {
            if index < bad {
                break_range(record, 100.0, 200.0);
            }
        },
    );
    let result = run(&lab, &records);
    let item = group(&result, HIDDEN_DAILY);
    let held = criterion(item, "held-after-finish-in-A");
    assert_eq!(
        held.share,
        Some(Share::new(u64::from(seeds) - bad, u64::from(seeds)))
    );
    assert_eq!(held.passed, passes);
    // The broken seeds spend a tenth of their exposure outside A, within the risk budget, and end in A.
    assert!(criterion(item, "post-finish-risk").passed);
    assert!(criterion(item, "retained-profile-in-A").passed);
    assert_eq!(item.safety_met, passes);
    assert!(item.usefulness_met);
}

#[test_case::test_case(9, 1, false; "eight of nine is just below ninety percent")]
#[test_case::test_case(10, 1, true; "nine of ten is exactly ninety percent")]
#[test_case::test_case(11, 1, true; "ten of eleven is just above ninety percent")]
#[test_case::test_case(10, 2, false; "eight of ten is below ninety percent")]
fn the_benign_depth_fraction_is_ninety_percent(seeds: u32, bad: u64, passes: bool) {
    let lab = Lab::new(&[EDGE_DEEP], seeds);
    // Depth 3 on both units is below the five-count floor and under 80% of the certified 12.
    let records = records_with(
        &lab,
        |spec, index| {
            if index < bad {
                spec.depths(&[3, 3])
            } else {
                spec
            }
        },
        |_, _| {},
    );
    let result = run(&lab, &records);
    let item = group(&result, EDGE_DEEP);
    let good = Share::new(u64::from(seeds) - bad, u64::from(seeds));
    assert_eq!(criterion(item, "benign-depth").share, Some(good));
    assert_eq!(criterion(item, "benign-depth").passed, passes);
    // A shallow seed never settles either, so convergence moves with the depth fraction.
    assert_eq!(criterion(item, "convergence").share, Some(good));
    assert_eq!(criterion(item, "convergence").passed, passes);
    assert_eq!(item.usefulness_met, passes);
    assert!(item.safety_met);
}

#[test_case::test_case(9, 1, false; "eight of nine is just below ninety percent")]
#[test_case::test_case(10, 1, true; "nine of ten is exactly ninety percent")]
#[test_case::test_case(11, 1, true; "ten of eleven is just above ninety percent")]
#[test_case::test_case(10, 2, false; "eight of ten is below ninety percent")]
fn the_convergence_fraction_is_ninety_percent(seeds: u32, bad: u64, passes: bool) {
    let lab = Lab::new(&[EDGE_DEEP], seeds);
    // Outside A until day 730.5, the final run in A starts after the registered settled day 730.
    let records = records_with(
        &lab,
        |spec, _| spec,
        |record, index| {
            if index < bad {
                break_range(record, 15.0, 730.5);
            }
        },
    );
    let result = run(&lab, &records);
    let item = group(&result, EDGE_DEEP);
    let good = Share::new(u64::from(seeds) - bad, u64::from(seeds));
    assert_eq!(criterion(item, "convergence").share, Some(good));
    assert_eq!(criterion(item, "convergence").passed, passes);
    // Every final profile is deep and in A, and the median stays at the early seeds' day 15.
    assert_eq!(
        criterion(item, "benign-depth").share,
        Some(Share::new(u64::from(seeds), u64::from(seeds)))
    );
    assert!(criterion(item, "benign-median-settled-day").passed);
    assert_eq!(item.usefulness_met, passes);
}

#[test_case::test_case(9, 1, false; "eight of nine is just below ninety percent")]
#[test_case::test_case(10, 1, true; "nine of ten is exactly ninety percent")]
#[test_case::test_case(11, 1, true; "ten of eleven is just above ninety percent")]
#[test_case::test_case(10, 2, false; "eight of ten is below ninety percent")]
fn the_retention_fraction_is_ninety_percent(seeds: u32, bad: u64, passes: bool) {
    let lab = Lab::new(&[STOCK_BACKGROUND], seeds);
    // Depth 3 from day 400 is two counts below the depth 5 held at the finish: more than the one allowed.
    let records = records_with(
        &lab,
        |spec, _| spec,
        |record, index| {
            if index < bad {
                thin_from(record, 400.0, 3);
            }
        },
    );
    let result = run(&lab, &records);
    let item = group(&result, STOCK_BACKGROUND);
    let retention = criterion(item, "retention");
    assert_eq!(
        retention.share,
        Some(Share::new(u64::from(seeds) - bad, u64::from(seeds)))
    );
    assert_eq!(retention.passed, passes);
    assert_eq!(item.usefulness_met, passes);
}

#[test_case::test_case(4, true; "one count lost is within the registered loss")]
#[test_case::test_case(3, false; "two counts lost is beyond the registered loss")]
fn retention_allows_exactly_one_lost_count(final_depth: u8, retained: bool) {
    let lab = Lab::new(&[STOCK_BACKGROUND], 1);
    let mut record = built(&lab, &Spec::new(STOCK_BACKGROUND, FIRST_SEED, SLOPE));
    thin_from(&mut record, 400.0, final_depth);
    resync(&mut record);
    validated(&record);
    let seed = metrics(&lab, &record);
    assert_eq!(seed.depth.finish_depths, Some(vec![5, 5]));
    assert_eq!(seed.depth.final_depths, vec![final_depth; 2]);
    assert_eq!(seed.retention_ok, retained);
}

#[test_case::test_case([Some(15), Some(180), Some(180)], Some(180.0), true, true; "the median at exactly day 180 passes")]
#[test_case::test_case([Some(15), Some(181), Some(181)], Some(181.0), false, true; "the median one day past 180 fails")]
#[test_case::test_case([Some(15), Some(15), Some(181)], Some(15.0), true, true; "one late seed leaves the median early")]
#[test_case::test_case([Some(15), Some(180), Some(181)], Some(180.0), true, true; "the middle seed decides an odd median")]
#[test_case::test_case([Some(15), Some(181), None], Some(181.0), false, false; "an unconverged seed sorts last")]
#[test_case::test_case([Some(15), Some(15), None], Some(15.0), true, false; "an early median with a failed convergence fraction")]
fn the_benign_median_settled_day_is_judged_at_day_180(
    settled: [Option<u32>; 3],
    median: Option<f64>,
    median_passes: bool,
    converges: bool,
) {
    let lab = Lab::new(&[EDGE_DEEP], 3);
    let records = records_with(
        &lab,
        |spec, _| spec,
        |record, index| {
            let Some(day) = settled[usize::try_from(index).expect("seed index")] else {
                break_range(record, 15.0, 731.0);
                record.outcome = Outcome::CompleteUnconvergedCrashFree {
                    observed_until_day: 731.0,
                };
                return;
            };
            // Settled at day 15 is the unedited record: both units finish by then.
            if day > 15 {
                break_range(record, 15.0, f64::from(day));
            }
        },
    );
    let result = run(&lab, &records);
    let item = group(&result, EDGE_DEEP);
    assert_eq!(item.diagnostics.median_settled_day, median);
    assert_eq!(
        criterion(item, "benign-median-settled-day").passed,
        median_passes
    );
    assert_eq!(criterion(item, "convergence").passed, converges);
    assert_eq!(item.usefulness_met, median_passes && converges);
}

/// A four-unit record at the given depths against an edited depth reference of every epoch.
fn four_units(depths: &[u8], feasible: &[u8], operational: u32, converged: bool) -> (Lab, Record) {
    let mut lab = Lab::new(&[EDGE_DEEP], 1);
    retopologize(&mut lab, EDGE_DEEP, 4, 4);
    let mut record = built(
        &lab,
        &Spec::new(EDGE_DEEP, FIRST_SEED, SLOPE).depths(depths),
    );
    for epoch in &mut record.epochs {
        let reference = &mut epoch.reference;
        reference.per_unit_max_feasible = feasible.to_vec();
        reference.operational_max = operational;
        reference.certified_upper_bound = operational;
        reference.physical_max = reference.physical_max.max(operational);
    }
    record.outcome = if converged {
        Outcome::CompleteConverged {
            observed_until_day: 731.0,
        }
    } else {
        Outcome::CompleteUnconvergedCrashFree {
            observed_until_day: 731.0,
        }
    };
    validated(&record);
    (lab, record)
}

// With M = 24 the required total is ceil(0.8 * 24) = 20, with M = 36 it is ceil(28.8) = 29 and with
// M = 22 it is ceil(17.6) = 18; a unit's floor is min(its maximum feasible depth, 5).
#[test_case::test_case(&[7, 7, 7, 4], &[7, 7, 7, 7], 24, 20, &[5, 5, 5, 5], false; "the total of 25 is met but one unit is below its floor")]
#[test_case::test_case(&[5, 5, 5, 5], &[7, 7, 7, 7], 24, 20, &[5, 5, 5, 5], true; "exactly the total and every floor")]
#[test_case::test_case(&[6, 6, 6, 6], &[10, 10, 10, 10], 36, 29, &[5, 5, 5, 5], false; "every unit is at its floor but the total of 24 is short of 29")]
#[test_case::test_case(&[5, 5, 5, 3], &[7, 7, 7, 3], 22, 18, &[5, 5, 5, 3], true; "a unit that can only reach 3 is held to 3")]
#[test_case::test_case(&[6, 6, 6, 2], &[7, 7, 7, 3], 22, 18, &[5, 5, 5, 3], false; "a unit that can only reach 3 fails at 2 despite the total of 20")]
fn the_per_unit_floor_is_judged_beside_the_aggregate_floor(
    depths: &[u8],
    feasible: &[u8],
    operational: u32,
    required: u64,
    floors: &[u8],
    benign_ok: bool,
) {
    let (lab, record) = four_units(depths, feasible, operational, benign_ok);
    let seed = metrics(&lab, &record);
    assert_eq!(seed.depth.required_total, required);
    assert_eq!(seed.depth.unit_floors, floors);
    assert_eq!(seed.depth.final_depths, depths);
    assert_eq!(seed.benign_ok, benign_ok);
    assert_eq!(seed.settled_at.is_some(), benign_ok);
}

#[test_case::test_case(&[43, 43], false; "both units at 43 are below the no-write-limit floor")]
#[test_case::test_case(&[44, 44], true; "both units at 44 meet the floor")]
#[test_case::test_case(&[45, 45], true; "both units at 45 exceed the floor")]
#[test_case::test_case(&[45, 43], false; "one unit at 43 fails whatever the other holds")]
#[test_case::test_case(&[44, 45], true; "44 and 45 together meet the floor")]
fn the_no_write_limit_scenario_demands_depth_44_on_every_unit(depths: &[u8], benign_ok: bool) {
    let lab = Lab::new(&[NO_WRITE_LIMIT], 1);
    let mut record = built(&lab, &Spec::new(NO_WRITE_LIMIT, FIRST_SEED, SLOPE));
    // The synthetic climbs stop at depth 5; the deployed trajectory is rewritten after the finish.
    let profile = deployed(&record, depths);
    for interval in record
        .intervals
        .iter_mut()
        .filter(|interval| interval.phase == Phase::PostFinish)
    {
        interval.profile = profile;
    }
    record.outcome = if benign_ok {
        Outcome::CompleteConverged {
            observed_until_day: 731.0,
        }
    } else {
        Outcome::CompleteUnconvergedCrashFree {
            observed_until_day: 731.0,
        }
    };
    validated(&record);
    let seed = metrics(&lab, &record);
    assert_eq!(seed.depth.unit_floors, vec![44, 44]);
    assert_eq!(seed.benign_ok, benign_ok);
    assert_eq!(seed.settled_at.is_some(), benign_ok);
}

#[test]
fn a_complete_seed_that_never_finishes_fails_the_post_finish_risk_rule() {
    let lab = Lab::new(&[HIDDEN_DAILY], 2);
    let records = records_with(
        &lab,
        |spec, _| spec,
        |record, _| {
            for unit in &mut record.units {
                unit.finish = None;
            }
            record.policy.finishes.clear();
            for interval in &mut record.intervals {
                interval.phase = Phase::Learning;
            }
            record.outcome = Outcome::CompleteUnconvergedCrashFree {
                observed_until_day: 731.0,
            };
        },
    );
    let seed = metrics(&lab, &records[0]);
    assert_eq!(seed.first_finish_day, None);
    assert_eq!(seed.settled_at, None);
    assert_eq!(
        (seed.post_finish.hours, seed.risk.t, seed.risk.h),
        (0.0, 0.0, 0.0)
    );
    assert!(!seed.risk.rate_ok);
    // Learning-phase exposure is real, but it is not post-finish exposure.
    assert!(seed.risk.learning_t > 0.0);

    let result = run(&lab, &records);
    let item = group(&result, HIDDEN_DAILY);
    let rule = criterion(item, "post-finish-risk");
    assert_eq!(rule.share, Some(Share::new(0, 2)));
    assert!(!rule.passed);
    assert!(!item.safety_met);
}

#[test_case::test_case(-1, true; "one unit in the last place below the budget")]
#[test_case::test_case(0, true; "exactly lambda times the active hours")]
#[test_case::test_case(1, false; "one unit in the last place above the budget")]
fn the_post_finish_risk_budget_is_exactly_lambda_times_the_hours(ulps: i64, within: bool) {
    let lab = Lab::new(&[HIDDEN_DAILY], 1);
    let lambda = lab.registration.lambda();
    let records = records_with(
        &lab,
        |spec, _| spec,
        |record, _| {
            let mut post_finish = record
                .intervals
                .iter_mut()
                .filter(|interval| interval.phase == Phase::PostFinish);
            let interval = post_finish.next().expect("one post-finish interval");
            assert!(
                post_finish.next().is_none(),
                "exactly one post-finish interval"
            );
            let budget = lambda * interval.verified_daily_active_hours;
            let h = f64::from_bits(budget.to_bits().wrapping_add_signed(ulps));
            interval.sources[0].expected_failures = h;
            interval.sources[1].expected_failures = 0.0;
        },
    );
    let seed = metrics(&lab, &records[0]);
    // 716 post-finish days of 8 active hours.
    assert_eq!(seed.risk.t, 5_728.0);
    let budget = lambda * 5_728.0;
    let expected = f64::from_bits(budget.to_bits().wrapping_add_signed(ulps));
    assert_eq!(seed.risk.h.to_bits(), expected.to_bits());
    assert_eq!(seed.risk.rate_ok, within);
    let result = run(&lab, &records);
    let rule = criterion(group(&result, HIDDEN_DAILY), "post-finish-risk");
    assert_eq!(rule.passed, within);
    assert_eq!(
        rule.share,
        Some(Share::new(u64::from(within), 1)),
        "{rule:?}"
    );
}

/// Turns `[from, to]` of the post-finish trajectory into a physical night probe outside A: no verified
/// daily hours, a large raw expected-failure integral and a hazard ratio of 3. The daily hours and
/// expected failures it displaces move to the preceding day interval, so the daily accounts the record
/// already carries stay exact.
fn night_probe(record: &mut Record, from: f64, to: f64) {
    split_at(record, from);
    split_at(record, to);
    let middle = record
        .intervals
        .iter()
        .position(|interval| {
            interval.phase == Phase::PostFinish
                && interval.start_day.total_cmp(&from).is_eq()
                && interval.end_day.total_cmp(&to).is_eq()
        })
        .expect("interval to turn into a probe");
    let units = record.units.len();
    let displaced_hours = record.intervals[middle].verified_daily_active_hours;
    let displaced: Vec<f64> = record.intervals[middle]
        .sources
        .iter()
        .map(|source| source.expected_failures)
        .collect();
    let probe = &mut record.intervals[middle];
    probe.period = Period::Night;
    probe.verified_daily_active_hours = 0.0;
    probe.admissibility = Admissibility {
        joint_ratio: 3.0,
        unit_ratios: vec![3.0; units],
        member: false,
    };
    for source in &mut probe.sources {
        source.expected_failures = 5.0;
    }
    let before = &mut record.intervals[middle - 1];
    before.verified_daily_active_hours += displaced_hours;
    for (source, carried) in before.sources.iter_mut().zip(displaced) {
        source.expected_failures += carried;
    }
}

fn near(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}

#[test]
fn an_unsafe_night_probe_changes_nothing_the_daily_contract_reads() {
    let lab = Lab::new(&[EDGE_DEEP], 1);
    let spec = Spec::new(EDGE_DEEP, FIRST_SEED, SLOPE).event(Ev::night("night-failure", 300.0));
    let control = built(&lab, &spec);
    let mut probed = control.clone();
    night_probe(&mut probed, 299.0, 301.0);
    validated(&probed);
    let (control, probed_seed) = (metrics(&lab, &control), metrics(&lab, &probed));

    // The night interval is outside A with 10 raw expected failures, and it is kept in the record.
    let night: Vec<_> = probed
        .intervals
        .iter()
        .filter(|interval| interval.period == Period::Night)
        .collect();
    assert_eq!(night.len(), 1);
    assert!(!night[0].admissibility.member);
    assert_eq!(night[0].verified_daily_active_hours, 0.0);
    // The night failure stays identifiable as a night event.
    assert_eq!(probed_seed.events.night.total, 1);
    assert_eq!(probed_seed.event_views[0].period, Period::Night);

    // Nothing the daily contract reads moved.
    assert!(probed_seed.final_in_a && probed_seed.post_finish.all_in_a);
    assert_eq!(probed_seed.settled_at, Some(15.0));
    assert_eq!(probed_seed.settled_at, control.settled_at);
    assert_eq!(probed_seed.post_finish.violating_hours, 0.0);
    assert_eq!(probed_seed.post_finish.first_violation, None);
    assert_eq!(
        probed_seed.post_finish.max_joint_ratio,
        control.post_finish.max_joint_ratio
    );
    assert!(near(
        probed_seed.post_finish.hours,
        control.post_finish.hours
    ));
    assert!(near(probed_seed.risk.t, 5_728.0));
    assert!(near(probed_seed.risk.h, control.risk.h));
    assert_eq!(
        (
            probed_seed.risk.k,
            probed_seed.risk.learning_k,
            probed_seed.risk.rate_ok
        ),
        (control.risk.k, control.risk.learning_k, true)
    );
    assert_eq!(probed_seed.depth, control.depth);
    assert_eq!(
        (probed_seed.benign_ok, probed_seed.retention_ok),
        (control.benign_ok, control.retention_ok)
    );
    assert_eq!(probed_seed.daily_co_crashes, control.daily_co_crashes);
    assert_eq!(probed_seed.events, control.events);
    assert_eq!(probed_seed.learning, control.learning);
    for (probe, base) in probed_seed.epochs.iter().zip(&control.epochs) {
        assert!(near(probe.hours, base.hours) && near(probe.member_hours, base.member_hours));
        assert_eq!(
            (probe.in_a_at_end, probe.recovered_to_a_at),
            (base.in_a_at_end, base.recovered_to_a_at)
        );
    }
}
