//! Rendered-output snapshots for the diagnostic states of the successor report: an absent mean finish
//! depth, learning violations, inherited stop witnesses, blocked, retained and crashed restorations,
//! experiment issues and incompatibility reasons, event classifications, a registered FAIL and per-core
//! mode. Each expected output is a compile-time fixture under `snapshots/`, derived from the layout rules
//! rather than captured from a run. The metrics are built directly, so no record needs to validate.

use togi_simrun::day_night::successor::{
    Classification, Exposure, FinishReason, Period, Phase, RunProfile, Runtime, Split, UnitFinish,
    UnitMode, UnitPhase,
};

use super::*;
use crate::day_night_successor::registration::PolicyRole;
use crate::day_night_successor::score::fixture::uid;
use crate::day_night_successor::score::learning::{
    LearningAudit, PromotionView, RestorationStatus, RestorationView, Violation,
};
use crate::day_night_successor::score::measure::{
    CompleteOutcome, DepthMetrics, EpochView, EventCounts, EventView, LedgerView, Losses,
    PolicyEvidence, PostFinish, SeedRisk, SourceRow, Tagged, UnitMetrics,
};
use crate::day_night_successor::score::rules::Scope;
use crate::day_night_successor::score::stats::Share;
use crate::day_night_successor::score::{
    Diagnostics, Fault, GroupStatus, OutcomeCounts, RuntimeSummary,
};

macro_rules! assert_snapshot {
    ($name:literal, $actual:expr) => {{
        let snapshot = include_str!(concat!("snapshots/render_review_tests__", $name, ".snap"));
        let (_, expected) = snapshot.split_once("\n---\n").expect("snapshot metadata");
        let expected = expected.strip_prefix("---\n").unwrap_or(expected);
        assert_eq!($actual.trim_end(), expected.trim_end());
    }};
}

fn rendered(draw: impl FnOnce(&mut String)) -> String {
    let mut out = String::new();
    draw(&mut out);
    out
}

fn unit(
    index: usize,
    finish: Option<(FinishReason, f64)>,
    finish_depth: Option<u8>,
    final_depth: u8,
) -> UnitMetrics {
    UnitMetrics {
        unit: uid(index),
        finish,
        settled_at: finish.map(|(_, day)| day),
        finish_depth,
        final_depth,
        promotions: 0,
        retreat_steps: 0,
        hold_hours: 0.0,
        qualification_launches: 0,
        qualification_hours: 0.0,
        night_hours: 0.0,
        losses: Losses::default(),
        co_attributions: 0,
        learning_h: 0.0,
        post_finish_h: 0.0,
    }
}

fn depth(
    final_depths: &[u8],
    reference: u32,
    required: u64,
    finish: Option<&[u8]>,
) -> DepthMetrics {
    let total: u32 = final_depths.iter().map(|depth| u32::from(*depth)).sum();
    DepthMetrics {
        final_depths: final_depths.to_vec(),
        final_total: total,
        final_core_mean: Some(f64::from(total) / final_depths.len() as f64),
        finish_depths: finish.map(<[u8]>::to_vec),
        reference,
        physical_max: reference,
        required_total: required,
        unit_floors: vec![],
    }
}

fn seed(number: u64) -> SeedMetrics {
    SeedMetrics {
        seed: number,
        outcome: CompleteOutcome::Converged,
        first_finish_day: Some(10.0),
        settled_at: Some(12.0),
        final_in_a: true,
        post_finish: PostFinish::default(),
        risk: SeedRisk {
            k: 0,
            t: 0.0,
            h: 0.0,
            learning_k: 0,
            learning_t: 0.0,
            learning_h: 0.0,
            rate_ok: true,
            tagged: Tagged::default(),
            sources: vec![],
        },
        depth: depth(&[3, 3], 6, 5, Some(&[3, 3])),
        benign_ok: true,
        retention_ok: true,
        units: vec![],
        events: EventCounts::default(),
        event_views: vec![],
        ledger: None,
        policy_evidence: PolicyEvidence {
            stopped_units: vec![],
            inherited_stops: vec![],
            newest_step: None,
        },
        learning: LearningAudit::default(),
        epochs: vec![],
        daily_co_crashes: 0,
        exposure: Exposure {
            intended_seconds: 0.0,
            observed_seconds: 0.0,
            eligible_seconds: 0.0,
            onset_active_seconds: 0.0,
        },
        runtime: Runtime {
            wall_seconds: 0.0,
            compilation_seconds: 0.0,
            invocation: None,
            jobs: 1,
            resource_class: "review".into(),
            trial_launches: 0,
        },
    }
}

fn ledger(limit: u64, consumed: u64, exhausted_day: Option<f64>) -> LedgerView {
    LedgerView {
        limit,
        consumed,
        remaining: limit.saturating_sub(consumed),
        overspend: consumed.saturating_sub(limit),
        exhausted: exhausted_day.is_some(),
        exhausted_day,
        provisional: 0,
        reclassified: 0,
        charged_learning: 0,
        charged_post_finish: 0,
        charged_restoration_tagged: 0,
    }
}

fn epoch(recovered: Option<f64>) -> EpochView {
    EpochView {
        id: "epoch-0".into(),
        start_day: 0.0,
        end_day: 731.0,
        context: "stock".into(),
        operational_max: 6,
        physical_max: 6,
        per_unit_max_feasible: vec![3, 3],
        certificate: "exact".into(),
        member_hours: 700.5,
        hours: 1000.0,
        in_a_at_end: true,
        recovered_to_a_at: recovered,
    }
}

fn group(
    scenario: &str,
    mode: UnitMode,
    slope: f64,
    seeds: Vec<SeedMetrics>,
    expected: &[u64],
) -> Group {
    Group {
        scenario: scenario.into(),
        unit_mode: mode,
        slope,
        scope: Scope::default(),
        expected_seeds: expected.to_vec(),
        seeds,
        criteria: vec![],
        safety_met: true,
        usefulness_met: true,
        status: GroupStatus::Met,
        diagnostics: Diagnostics {
            outcomes: OutcomeCounts::default(),
            k: 0,
            t: 0.0,
            h: 0.0,
            poisson_upper_mean: 0.0,
            poisson_upper_rate: None,
            zero_co_learning_crashes: Share::new(0, 0),
            zero_total_daily_interruptions: Share::new(0, 0),
            zero_daily_co_crashes: Share::new(0, 0),
            mean_unit_total_depth: None,
            mean_core_depth: None,
            median_settled_day: None,
        },
    }
}

fn score(groups: Vec<Group>, split: Split, verdict: Verdict, cells: (u64, u64)) -> Score {
    Score {
        registration_id: "successor-review".into(),
        registration_sha256: "sha-review".into(),
        policy: "successor-candidate".into(),
        policy_identity: "identity-1".into(),
        policy_role: PolicyRole::Candidate,
        split,
        run_profile: RunProfile::Registered,
        verdict,
        label: None,
        registered_slopes: vec![1.5, 2.0],
        selected_slopes: vec![1.5],
        expected_cells: cells.0,
        complete_cells: cells.1,
        issues: vec![],
        faults: vec![],
        groups,
        runtime: RuntimeSummary {
            records: 0,
            complete: 0,
            invocation_wall_seconds: None,
            invocations: vec![],
            summed_run_wall_seconds: 0.0,
            max_run_wall_seconds: 0.0,
            compilation_seconds: 0.0,
            trial_launches: 0,
            jobs: vec![],
            resource_classes: vec![],
            budget_seconds: 0,
            intended_seconds: 0.0,
            observed_seconds: 0.0,
            eligible_seconds: 0.0,
            onset_active_seconds: 0.0,
        },
    }
}

fn two_seed_group(rows: Vec<[UnitMetrics; 2]>) -> Group {
    let mut first = seed(1);
    let mut second = seed(2);
    for [a, b] in rows {
        first.units.push(a);
        second.units.push(b);
    }
    group(
        "successor-units",
        UnitMode::CcdUniform,
        1.5,
        vec![first, second],
        &[1, 2],
    )
}

#[test]
fn the_mean_finish_depth_is_not_available_when_no_seed_recorded_one() {
    let group = two_seed_group(vec![[unit(0, None, None, 2), unit(0, None, None, 4)]]);
    assert_snapshot!(
        "units_without_finish_depth",
        rendered(|out| units(out, &group))
    );
}

#[test]
fn the_mean_finish_depth_averages_only_the_seeds_that_recorded_one() {
    let mut busy = unit(0, Some((FinishReason::Ceiling, 5.0)), Some(4), 4);
    busy.promotions = 2;
    busy.retreat_steps = 1;
    busy.hold_hours = 1.5;
    busy.qualification_launches = 2;
    busy.qualification_hours = 0.25;
    busy.night_hours = 4.0;
    busy.losses = Losses {
        co: 5,
        background: 6,
        non_cpu: 7,
        unattributed: 8,
        history: 9,
    };
    busy.co_attributions = 10;
    let mut other = unit(0, None, None, 2);
    other.promotions = 1;
    let group = two_seed_group(vec![
        [busy, other],
        [
            unit(1, Some((FinishReason::Ceiling, 6.0)), Some(3), 3),
            unit(1, Some((FinishReason::Ceiling, 7.0)), Some(4), 4),
        ],
    ]);
    assert_snapshot!(
        "units_with_partial_finish_depth",
        rendered(|out| units(out, &group))
    );
}

fn view(id: &str, day: f64, period: Period, phase: Phase, class: Classification) -> EventView {
    EventView {
        id: id.into(),
        day,
        period,
        phase,
        unit_phase: UnitPhase::Core,
        restoration_learning: None,
        classification: class,
        provisional: false,
        reclassified_from: None,
        charged: true,
        truth: "co".into(),
        implicated: vec![],
        newest_step: None,
    }
}

#[test]
fn a_per_core_seed_states_every_finish_reason_ledger_and_event_classification() {
    let mut item = seed(7);
    item.depth = depth(&[3, 2, 3, 2], 12, 9, Some(&[3, 3, 3, 2]));
    item.events = EventCounts {
        day_learning: causes(3, 2, 1, 0),
        day_post_finish: causes(3, 2, 0, 1),
        night: causes(1, 1, 0, 0),
    };
    let finishes = [
        (FinishReason::Ceiling, 10.0, 3, 3, 0, 0.5, 0.0),
        (FinishReason::CpuSuspectStop, 9.5, 3, 2, 1, 0.25, 0.125),
        (FinishReason::AllowanceExhausted, 10.0, 3, 3, 0, 0.0, 0.0),
        (
            FinishReason::FirmwareRestorationStop,
            10.0,
            2,
            2,
            0,
            0.0,
            0.0,
        ),
    ];
    for (index, (reason, day, at_finish, last, retreats, learning_h, post_h)) in
        finishes.into_iter().enumerate()
    {
        let mut row = unit(index, Some((reason, day)), Some(at_finish), last);
        row.promotions = u64::from(at_finish);
        row.retreat_steps = retreats;
        row.learning_h = learning_h;
        row.post_finish_h = post_h;
        item.units.push(row);
    }
    let mut ledger = ledger(2, 4, Some(4.25));
    ledger.provisional = 1;
    ledger.reclassified = 1;
    ledger.charged_learning = 2;
    ledger.charged_post_finish = 2;
    ledger.charged_restoration_tagged = 1;
    item.ledger = Some(ledger);
    item.policy_evidence = PolicyEvidence {
        stopped_units: vec![uid(1), uid(3)],
        inherited_stops: vec![],
        newest_step: Some("step-3".into()),
    };
    let mut suspect = view(
        "crash-a",
        3.5,
        Period::Day,
        Phase::Learning,
        Classification::CpuSuspect,
    );
    suspect.implicated = vec![uid(1)];
    suspect.newest_step = Some("step-1".into());
    let mut unanswered = view(
        "crash-b",
        4.25,
        Period::Day,
        Phase::Learning,
        Classification::Unanswered,
    );
    unanswered.provisional = true;
    unanswered.implicated = vec![uid(0), uid(2)];
    let mut background = view(
        "bg-c",
        6.0,
        Period::Day,
        Phase::Learning,
        Classification::NonCpu,
    );
    background.charged = false;
    background.truth = "background".into();
    let mut unexplained = view(
        "crash-d",
        11.0,
        Period::Day,
        Phase::PostFinish,
        Classification::Unexplained,
    );
    unexplained.restoration_learning = Some("fw-1".into());
    unexplained.implicated = vec![uid(3)];
    unexplained.newest_step = Some("step-3".into());
    let mut reclassified = view(
        "flip-e",
        12.5,
        Period::Day,
        Phase::PostFinish,
        Classification::NonCpu,
    );
    reclassified.reclassified_from = Some(Classification::CpuSuspect);
    reclassified.truth = "non-cpu".into();
    let mut night = view(
        "night-f",
        20.0,
        Period::Night,
        Phase::PostFinish,
        Classification::Unanswered,
    );
    night.reclassified_from = Some(Classification::Unexplained);
    night.charged = false;
    night.implicated = vec![uid(1)];
    let mut unreported = view(
        "silent-g",
        21.0,
        Period::Day,
        Phase::PostFinish,
        Classification::Unreported,
    );
    unreported.charged = false;
    unreported.implicated = vec![uid(0)];
    item.event_views = vec![
        suspect,
        unanswered,
        background,
        unexplained,
        reclassified,
        night,
        unreported,
    ];
    item.epochs = vec![epoch(None)];
    let group = group(
        "successor-per-core-events",
        UnitMode::PerCore,
        1.5,
        vec![item.clone()],
        &[7],
    );
    assert_snapshot!(
        "per_core_seed_with_ledger_and_event_classifications",
        rendered(|out| seed_section(out, &group, &item))
    );
}

fn causes(
    total: u64,
    co: u64,
    background: u64,
    non_cpu: u64,
) -> crate::day_night_successor::score::measure::Causes {
    crate::day_night_successor::score::measure::Causes {
        total,
        co,
        background,
        non_cpu,
        unattributed: 0,
    }
}

#[test]
fn a_seed_without_a_ledger_states_it_and_lists_inherited_stop_witnesses() {
    let mut item = seed(3);
    item.outcome = CompleteOutcome::UnconvergedCrashFree;
    item.first_finish_day = None;
    item.settled_at = None;
    item.depth = depth(&[2, 2], 6, 5, None);
    item.depth.final_core_mean = None;
    item.units = vec![
        unit(0, Some((FinishReason::InheritedStop, 1.0)), Some(2), 2),
        unit(1, None, None, 2),
    ];
    item.policy_evidence = PolicyEvidence {
        stopped_units: vec![uid(0)],
        inherited_stops: vec![UnitFinish {
            unit: uid(0),
            reason: FinishReason::InheritedStop,
            day: 1.0,
            witnesses: vec!["prior-crash-1".into(), "prior-crash-2".into()],
        }],
        newest_step: None,
    };
    item.epochs = vec![epoch(Some(400.25))];
    let group = group(
        "successor-inherited-stop",
        UnitMode::CcdUniform,
        2.0,
        vec![item.clone()],
        &[3],
    );
    assert_snapshot!(
        "seed_without_ledger_and_inherited_stop_witnesses",
        rendered(|out| seed_section(out, &group, &item))
    );
}

#[test]
fn a_seed_lists_blocked_retained_and_crashed_restorations_with_violations() {
    let mut item = seed(4);
    item.first_finish_day = Some(20.0);
    item.settled_at = Some(25.0);
    let mut accounts = ledger(2, 1, None);
    accounts.charged_learning = 1;
    item.ledger = Some(accounts);
    item.policy_evidence.newest_step = Some("step-9".into());
    item.risk.k = 1;
    item.risk.t = 40.0;
    item.risk.h = 0.375;
    item.risk.tagged = Tagged {
        hours: 10.0,
        h: 0.125,
        k: 0,
    };
    item.risk.sources = vec![
        SourceRow {
            source: "co".into(),
            learning_k: 1,
            learning_h: 0.5,
            post_finish_k: 0,
            post_finish_h: 0.125,
        },
        SourceRow {
            source: "background".into(),
            learning_k: 0,
            learning_h: 0.0,
            post_finish_k: 1,
            post_finish_h: 0.25,
        },
    ];
    let restoration = |id: &str,
                       epoch: &str,
                       day: f64,
                       status,
                       applied,
                       clean,
                       recomputed,
                       previous: &[u8],
                       stopped: Vec<_>,
                       qualification: Option<&str>,
                       violations| RestorationView {
        id: id.into(),
        epoch: epoch.into(),
        transition_day: day,
        stock_reset_day: day,
        applied_day: applied,
        clean_stock_hours: clean,
        stock_hours_recomputed: recomputed,
        previous_depths: previous.to_vec(),
        status,
        stopped_units: stopped,
        qualification: qualification.map(Into::into),
        violations,
    };
    let mut blocked = restoration(
        "restore-1",
        "epoch-1",
        100.0,
        RestorationStatus::Blocked {
            reason: "stock | too small".into(),
        },
        None,
        None,
        None,
        &[3, 3],
        vec![],
        None,
        0,
    );
    blocked.stock_reset_day = 100.5;
    item.learning = LearningAudit {
        reported: true,
        violations: vec![
            Violation {
                rule: "stock-reset-profile",
                subject: "restore-3".into(),
                detail: "profile | mismatch".into(),
            },
            Violation {
                rule: "allowance-limit",
                subject: "seed".into(),
                detail: "overspent by 1".into(),
            },
        ],
        promotions: vec![PromotionView {
            id: "promo-1".into(),
            day: 3.5,
            unit: uid(1),
            before: 1,
            after: 2,
            ceiling: 3,
            charged_before: 1,
        }],
        restorations: vec![
            blocked,
            restoration(
                "restore-2",
                "epoch-2",
                200.0,
                RestorationStatus::Retained,
                Some(201.5),
                Some(12.0),
                Some(12.0),
                &[3, 2],
                vec![uid(0)],
                Some("qual-2"),
                0,
            ),
            restoration(
                "restore-3",
                "epoch-3",
                300.0,
                RestorationStatus::Crashed {
                    event: "crash-r3".into(),
                },
                Some(300.25),
                Some(10.0),
                Some(9.5),
                &[2, 2],
                vec![uid(0), uid(1)],
                None,
                1,
            ),
        ],
    };
    let group = group(
        "successor-restoration-states",
        UnitMode::CcdUniform,
        1.5,
        vec![item.clone()],
        &[4],
    );
    assert_snapshot!(
        "seed_restoration_states_violations_and_risk_sources",
        rendered(|out| seed_section(out, &group, &item))
    );
}

#[test]
fn the_learning_table_separates_reported_violations_from_an_unreported_ledger() {
    let mut over = seed(1);
    let mut accounts = ledger(2, 3, Some(9.5));
    accounts.charged_learning = 2;
    accounts.charged_post_finish = 1;
    accounts.charged_restoration_tagged = 1;
    over.ledger = Some(accounts);
    over.risk.learning_k = 2;
    let violation = |rule| Violation {
        rule,
        subject: "seed".into(),
        detail: "d".into(),
    };
    over.learning.reported = true;
    over.learning.violations = vec![violation("allowance-limit"), violation("charge-refund")];
    let mut clean = seed(2);
    clean.ledger = Some(ledger(2, 1, None));
    clean.learning.reported = true;
    let mut silent = seed(3);
    silent.risk.learning_k = 1;
    let group = group(
        "successor-learning",
        UnitMode::CcdUniform,
        1.5,
        vec![over, clean, silent],
        &[1, 2, 3],
    );
    assert_snapshot!(
        "learning_table_reported_and_unreported",
        rendered(|out| learning_table(out, &group))
    );
}

fn two_groups() -> Vec<Group> {
    let mut failing = group(
        "successor-ccd-fail",
        UnitMode::CcdUniform,
        1.5,
        vec![seed(1)],
        &[1],
    );
    failing.safety_met = false;
    failing.status = GroupStatus::NotMet;
    let passing = group(
        "successor-per-core-pass",
        UnitMode::PerCore,
        2.0,
        vec![seed(1)],
        &[1],
    );
    vec![failing, passing]
}

#[test]
fn a_registered_candidate_failure_prints_fail_beside_per_core_rows() {
    let result = score(two_groups(), Split::Dev, Verdict::Fail, (2, 2));
    assert_snapshot!(
        "registered_fail_header_and_scenarios",
        rendered(|out| {
            header(out, &result);
            scenarios(out, &result);
        })
    );
}

#[test]
fn an_incomplete_experiment_lists_issues_and_every_fault_reason() {
    let mut pending = group(
        "successor-ccd-incomplete",
        UnitMode::CcdUniform,
        1.5,
        vec![],
        &[1, 2, 3],
    );
    pending.status = GroupStatus::Incomplete;
    let mut result = score(vec![pending], Split::Holdout, Verdict::Incomplete, (3, 1));
    result.issues = vec![
        "slope 2.5 is not registered".into(),
        "cell | mismatch".into(),
    ];
    let fault = |seed, kind| Fault {
        scenario: "successor-ccd-incomplete".into(),
        seed,
        unit_mode: UnitMode::CcdUniform,
        slope: 1.5,
        kind,
    };
    result.faults = vec![
        fault(2, FaultKind::Missing),
        fault(3, FaultKind::Duplicate { copies: 2 }),
        fault(4, FaultKind::Invalid("bad | unit".into())),
        fault(
            5,
            FaultKind::Incompatible(vec![
                "policy identity differs".into(),
                "registration hash differs".into(),
            ]),
        ),
        fault(
            6,
            FaultKind::Inconsistent("outcome contradicts events".into()),
        ),
    ];
    assert_snapshot!(
        "incomplete_experiment_issues_and_faults",
        rendered(|out| {
            header(out, &result);
            scenarios(out, &result);
            faults(out, &result);
        })
    );
}
