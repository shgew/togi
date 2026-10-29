use super::*;
use crate::day_night::CalendarConfig;
use crate::day_night::successor::{
    AllowanceLedger, FinishReason, Provenance, RunProfile, Split, UnitFinish,
};
use togi_sim::day_night::TrialClass;

fn config(scenario: &truth::Scenario, days: u32, mode: UnitMode) -> RunConfig {
    RunConfig {
        calendar: CalendarConfig {
            days,
            ..CalendarConfig::default()
        },
        provenance: Provenance {
            registration_id: "successor-assembly-test".to_owned(),
            registration_sha256: "a".repeat(64),
            source_revision: "source".to_owned(),
            simulator_revision: "truth".to_owned(),
            runner_revision: "runner".to_owned(),
            report_revision: "report".to_owned(),
            binary_sha256: None,
            input_sha256: None,
            policy_identity: "test-policy".to_owned(),
            policy_source_sha256: "b".repeat(64),
            parameters_sha256: "c".repeat(64),
            slope: 1.5,
            split: Split::Dev,
            seed: 20_001,
            scenario: scenario.id().to_owned(),
            unit_mode: mode,
            topology: Vec::new(),
            membership: Vec::new(),
            run_profile: RunProfile::ReducedDev,
        },
        epochs: Vec::new(),
        daily: Vec::new(),
        tolerance: truth::NUMERICAL_TOLERANCE,
        jobs: 1,
        resource_class: "in-memory-test".to_owned(),
        compilation_seconds: 0.0,
        deadline: None,
    }
}

#[test]
fn ruleset13_promotions_and_night_failures_keep_their_journal_witnesses() {
    use std::collections::BTreeSet;
    use togi_journal::{ApplyCause, Outcome, Payload, StepKind as JournalStep};

    let scenario = truth::Scenario::builtin("successor-edge-deep", 20_001).unwrap();
    let mut policy = crate::day_night::Ruleset13::new(1.5).unwrap().recording();
    let record =
        crate::day_night::successor::run(config(&scenario, 30, UnitMode::CcdUniform), &mut policy)
            .unwrap();
    let journal = policy.events().unwrap();
    let promotions: Vec<_> = journal
        .iter()
        .filter_map(|event| {
            let Payload::DayApplied(applied) = &event.payload else {
                return None;
            };
            let ApplyCause::Step { decision } = applied.cause else {
                return None;
            };
            journal.iter().find_map(|proposal| {
                if let Payload::DayStep(step) = &proposal.payload
                    && step.decision == decision
                    && matches!(step.step, JournalStep::Promote { .. })
                {
                    Some((event, applied))
                } else {
                    None
                }
            })
        })
        .collect();
    assert!(!promotions.is_empty(), "fixture never promoted");
    for (event, applied) in promotions {
        let day = 1.0 + event.mono.unwrap().as_secs_f64() / 86_400.0;
        let witness = record
            .steps
            .iter()
            .find(|step| step.day == day && matches!(step.kind, StepKind::Climb { .. }))
            .expect("applied promotion has a step witness");
        for info in scenario.topology() {
            assert_eq!(
                i64::from(witness.after.get(info.core).unwrap()),
                -applied.target.ccd().get(info.ccd).unwrap().observed().0
            );
        }
    }
    for summary in &record.units {
        let witnesses: BTreeSet<_> = record
            .steps
            .iter()
            .filter_map(|step| {
                if let StepKind::Climb {
                    unit,
                    qualification,
                    ..
                } = &step.kind
                    && *unit == summary.unit
                {
                    Some(qualification)
                } else {
                    None
                }
            })
            .flat_map(|qualification| {
                qualification
                    .fresh_witnesses
                    .iter()
                    .chain(&qualification.reused_witnesses)
                    .chain(&qualification.stock_controls)
            })
            .collect();
        assert!(
            !witnesses.is_empty(),
            "promoted unit has no qualification witnesses"
        );
        assert!(
            summary.qualification_launches > 0,
            "promoted unit has no observed qualification launches"
        );
        for id in witnesses {
            assert!(
                journal.iter().any(|event| {
                    matches!(&event.payload, Payload::TrialStart(start)
                        if start.trial.as_str() == id.as_str())
                }),
                "qualification witness {id} has no journaled launch"
            );
        }
    }
    let failures: Vec<_> = journal
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload
                && end.outcome == Outcome::Failure
            {
                Some(event)
            } else {
                None
            }
        })
        .collect();
    assert!(!failures.is_empty(), "fixture never failed at night");
    for failure in failures {
        let day = 1.0 + failure.mono.unwrap().as_secs_f64() / 86_400.0;
        let reported = record
            .events
            .iter()
            .find(|event| event.period == Period::Night && event.policy_event.day == day)
            .expect("night failure has a machine event");
        assert_eq!(
            reported.policy_event.classification,
            Classification::CpuSuspect
        );
    }
}

fn snapshot(at: Duration, report: PolicyReport) -> Snapshot {
    Snapshot {
        at,
        report,
        applications: PolicyApplications::default(),
    }
}

fn raw(scenario: &truth::Scenario, depth: u8) -> PerCore<u8> {
    scenario
        .topology()
        .iter()
        .map(|core| (core.core, depth))
        .collect()
}

fn change(at: Duration, profile: PerCore<u8>, night: bool) -> machine::Observation {
    machine::Observation {
        before: at,
        after: at,
        profile,
        night,
        kind: ObservationKind::ProfileChange,
    }
}

fn sleep(before: Duration, after: Duration, profile: PerCore<u8>) -> machine::Observation {
    machine::Observation {
        before,
        after,
        profile,
        night: false,
        kind: ObservationKind::Sleep,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the observation fixture keeps each truth input explicit"
)]
fn active(
    scenario: &truth::Scenario,
    state: &mut truth::TruthState,
    class: &TrialClass,
    before: Duration,
    duration: Duration,
    raw: PerCore<u8>,
    night: bool,
    cause: Option<Cause>,
) -> machine::Observation {
    state.interrupt();
    let profile = profile(&raw, scenario).unwrap();
    let seconds = scenario
        .slice_seconds(
            class,
            &profile,
            state,
            duration,
            Duration::from_secs(u64::from(class.regime().warm_up_s())),
        )
        .unwrap();
    let exposure = scenario
        .evaluate(
            class,
            &profile,
            state,
            duration,
            u32::try_from(before.as_secs() / 86_400 + 1).unwrap(),
        )
        .unwrap();
    state
        .advance_profile(Some(class), &profile, duration)
        .unwrap();
    if cause.is_some() {
        state.interrupt();
    }
    machine::Observation {
        before,
        after: before + duration,
        profile: raw,
        night,
        kind: ObservationKind::Active {
            class: class.clone(),
            seconds,
            exposure,
            cause,
            trial_id: night.then(|| format!("trial-{}", before.as_nanos())),
            launch: true,
            intended: duration,
        },
    }
}

#[test]
fn stock_full_record_keeps_policy_absence_and_exact_structure() {
    let scenario = truth::Scenario::builtin("successor-single-ccd", 20_001).unwrap();
    let config = config(&scenario, 1, UnitMode::CcdUniform);
    let initial_provenance = config.provenance.clone();
    let stock = raw(&scenario, 0);
    let observed = Duration::from_hours(24);
    let record = assemble(
        config,
        &scenario,
        vec![sleep(Duration::ZERO, observed, stock)],
        vec![snapshot(observed, PolicyReport::unavailable())],
        observed,
        None,
        0.25,
    )
    .unwrap();
    let membership = vec![PolicyUnit {
        unit: unit_id(0).unwrap(),
        cores: scenario.topology().iter().map(|info| info.core).collect(),
        kind: UnitPhase::Coarse,
        parent: None,
    }];
    let reference = truth::Reference::new(
        &scenario,
        1,
        truth::UnitGrouping::ccd(scenario.topology()).unwrap(),
        &truth::ReferenceCalendar::standard(&scenario).unwrap(),
        truth::LAMBDA,
        truth::NUMERICAL_TOLERANCE,
    )
    .unwrap();
    let optimum = reference.depth_reference().unwrap();
    let mut provenance = initial_provenance;
    provenance.topology = scenario.topology().to_vec();
    provenance.membership = membership.clone();
    let source = RiskSource::Unit {
        unit: unit_id(0).unwrap(),
    };
    let account = SourceAccount {
        source: source.clone(),
        k: 0,
        active_hours: 0.0,
        expected_failures: 0.0,
    };
    let expected = Record {
        provenance,
        epochs: vec![Epoch {
            id: "initial".to_owned(),
            start_day: 1.0,
            end_day: 2.0,
            context: "ctx-initial".to_owned(),
            reference: DepthReference {
                operational_max: optimum.operational.value,
                certified_upper_bound: optimum.operational.upper_bound,
                physical_max: optimum.physical.value,
                per_unit_max_feasible: optimum.unit_maxima,
                tolerance: truth::NUMERICAL_TOLERANCE,
                certificate: format!(
                    "exact-monotone-v1:cap={};states={};components={}",
                    optimum.operational.cap,
                    optimum.operational.states,
                    optimum.operational.components
                ),
            },
        }],
        intervals: vec![ProfileInterval {
            start_day: 1.0,
            end_day: 2.0,
            profile: stock,
            period: Period::Day,
            membership: membership.clone(),
            epoch: "initial".to_owned(),
            verified_daily_active_hours: 0.0,
            phase: Phase::Learning,
            restoration_learning: None,
            admissibility: Admissibility {
                joint_ratio: 0.0,
                unit_ratios: vec![0.0],
                member: true,
            },
            sources: vec![SourceRisk {
                source,
                expected_failures: 0.0,
            }],
        }],
        events: Vec::new(),
        units: vec![UnitSummary {
            unit: unit_id(0).unwrap(),
            finish: None,
            promotions: 0,
            retreats: Retreats {
                co_counts: 0,
                background_counts: 0,
                non_cpu_counts: 0,
                unattributed_counts: 0,
                steps: 0,
            },
            hold_hours: 0.0,
            qualification_launches: 0,
            qualification_hours: 0.0,
            night_hours: 0.0,
            ceiling_evidence: Vec::new(),
        }],
        policy: PolicyReport {
            unit_membership: membership,
            readback: Some(stock),
            ..PolicyReport::unavailable()
        },
        steps: Vec::new(),
        restorations: Vec::new(),
        accounts: RiskAccounts {
            learning: vec![account.clone()],
            post_finish: vec![account],
        },
        exposure: Exposure {
            intended_seconds: 0.0,
            observed_seconds: 0.0,
            eligible_seconds: 0.0,
            onset_active_seconds: 0.0,
        },
        outcome: Outcome::CompleteUnconvergedCrashFree {
            observed_until_day: 2.0,
        },
        runtime: Runtime {
            wall_seconds: 0.25,
            compilation_seconds: 0.0,
            invocation: None,
            jobs: 1,
            resource_class: "in-memory-test".to_owned(),
            trial_launches: 0,
        },
    };
    assert_eq!(record, expected);
    record.validate().unwrap();
    assert_eq!(
        Record::parse(&serde_json::to_vec(&record).unwrap()).unwrap(),
        record
    );
}

#[test]
fn budget_expiry_before_first_action_has_valid_empty_prefix() {
    let scenario = truth::Scenario::builtin("successor-single-ccd", 20_001).unwrap();
    let record = assemble(
        config(&scenario, 1, UnitMode::CcdUniform),
        &scenario,
        Vec::new(),
        vec![snapshot(Duration::ZERO, PolicyReport::unavailable())],
        Duration::ZERO,
        Some(Stop::BudgetExpired),
        0.0,
    )
    .unwrap();
    record.validate().unwrap();
    assert_eq!(record.intervals, Vec::<ProfileInterval>::new());
    assert_eq!(record.policy.allowance, None);
    assert_eq!(record.policy.finishes, Vec::<UnitFinish>::new());
    assert_eq!(
        record.outcome,
        Outcome::Truncated {
            observed_until_day: 1.0,
            reason: "invocation execution budget expired".to_owned(),
            lower_bound: PartialCounts {
                daily_interruptions: 0,
                daily_co_crashes: 0,
                active_hours: 0.0,
            }
        }
    );
}

#[test]
fn finish_trigger_keeps_learning_and_later_reports_cannot_move_original_finish() {
    let scenario = truth::Scenario::builtin("successor-single-ccd", 20_001).unwrap();
    let deployed = raw(&scenario, 30);
    let stock = raw(&scenario, 0);
    let elapsed = Duration::from_secs(60);
    let observed = Duration::from_hours(24);
    let mut state = truth::TruthState::cold(scenario.topology()).unwrap();
    let class = &scenario.daily_mix()[0].class;
    let cause = Cause::Truth(truth::FailureCause::Core(scenario.topology()[0].core));
    let observations = vec![
        change(Duration::ZERO, deployed, false),
        active(
            &scenario,
            &mut state,
            class,
            Duration::ZERO,
            elapsed,
            deployed,
            false,
            Some(cause),
        ),
        change(elapsed, stock, false),
        sleep(elapsed, observed, stock),
    ];
    let event = LearningEvent {
        id: "policy-stop".to_owned(),
        day: 1.0,
        classification: Classification::CpuSuspect,
        provisional: false,
        reclassified_from: None,
        charged: true,
        phase: Phase::Learning,
        unit_phase: UnitPhase::Coarse,
        newest_step: None,
    };
    let finish = UnitFinish {
        unit: unit_id(0).unwrap(),
        reason: FinishReason::CpuSuspectStop,
        day: day(elapsed),
        witnesses: vec![event.id.clone()],
    };
    let mut report = PolicyReport {
        finishes: vec![finish.clone()],
        events: vec![event],
        allowance: Some(AllowanceLedger {
            limit: 2,
            consumed: 1,
            provisional: 0,
            reclassified: 0,
            exhausted_day: None,
        }),
        ..PolicyReport::unavailable()
    };
    let first = snapshot(elapsed, report.clone());
    report.finishes[0].day = 1.5;
    let record = assemble(
        config(&scenario, 1, UnitMode::CcdUniform),
        &scenario,
        observations,
        vec![first, snapshot(observed, report)],
        observed,
        None,
        0.0,
    )
    .unwrap();
    record.validate().unwrap();
    assert_eq!(record.units[0].finish, Some(finish));
    assert_eq!(record.events[0].policy_event.id, "policy-stop");
    assert_eq!(record.events[0].policy_event.day, day(elapsed));
    assert_eq!(record.events[0].phase, Phase::Learning);
    assert_eq!(
        record
            .intervals
            .iter()
            .map(|interval| (interval.start_day, interval.end_day, interval.phase))
            .collect::<Vec<_>>(),
        vec![
            (1.0, day(elapsed), Phase::Learning),
            (day(elapsed), 2.0, Phase::PostFinish)
        ]
    );
    assert_eq!(record.accounts.learning[0].k, 1);
    assert_eq!(record.accounts.post_finish[0].k, 0);
    assert_eq!(
        record.outcome,
        Outcome::CompleteConverged {
            observed_until_day: 2.0
        }
    );
}

fn finished_profile(scenario: &truth::Scenario, mut config: RunConfig, depths: &[u8]) -> Record {
    grouping(&mut config, scenario).unwrap();
    assert_eq!(depths.len(), config.provenance.membership.len());
    let deployed = config
        .provenance
        .membership
        .iter()
        .zip(depths)
        .flat_map(|(unit, depth)| unit.cores.iter().map(move |core| (*core, *depth)))
        .collect();
    let report = PolicyReport {
        finishes: config
            .provenance
            .membership
            .iter()
            .map(|unit| UnitFinish {
                unit: unit.unit,
                reason: FinishReason::Ceiling,
                day: 1.0,
                witnesses: vec![format!("synthetic-finish-{}", unit.unit.get())],
            })
            .collect(),
        ..PolicyReport::unavailable()
    };
    let observed = Duration::from_hours(24);
    let record = assemble(
        config,
        scenario,
        vec![
            change(Duration::ZERO, deployed, false),
            sleep(Duration::ZERO, observed, deployed),
        ],
        vec![snapshot(Duration::ZERO, report)],
        observed,
        None,
        0.0,
    )
    .unwrap();
    record.validate().unwrap();
    assert_eq!(record.intervals.last().unwrap().phase, Phase::PostFinish);
    assert!(record.intervals.last().unwrap().admissibility.member);
    record
}

#[test_case::test_case(36, false; "below the required total")]
#[test_case::test_case(37, true; "at the required total")]
fn benign_convergence_requires_ceil_eighty_percent_total(second_depth: u8, expected: bool) {
    let scenario = truth::Scenario::builtin("successor-edge-deep", 20_001).unwrap();
    let record = finished_profile(
        &scenario,
        config(&scenario, 1, UnitMode::CcdUniform),
        &[35, second_depth],
    );
    assert_eq!(record.epochs[0].reference.operational_max, 90);
    assert_eq!((4 * 90_u32).div_ceil(5), 72);
    assert_eq!(
        record.outcome,
        if expected {
            Outcome::CompleteConverged {
                observed_until_day: 2.0,
            }
        } else {
            Outcome::CompleteUnconvergedCrashFree {
                observed_until_day: 2.0,
            }
        }
    );
}

#[test_case::test_case("successor-edge-deep", 4, false; "deep edge below unit floor")]
#[test_case::test_case("successor-edge-deep", 5, true; "deep edge meets unit floor")]
#[test_case::test_case("successor-within-ccd-heterogeneous", 4, false; "heterogeneous below unit floor")]
#[test_case::test_case("successor-within-ccd-heterogeneous", 5, true; "heterogeneous meets unit floor")]
fn benign_convergence_requires_each_units_floor(scenario: &str, first_depth: u8, expected: bool) {
    let scenario = truth::Scenario::builtin(scenario, 20_001).unwrap();
    let mut config = config(&scenario, 1, UnitMode::PerCore);
    let groups = grouping(&mut config, &scenario).unwrap();
    references(&mut config, &scenario, &groups).unwrap();
    let mut depths = config.epochs[0].reference.per_unit_max_feasible.clone();
    depths[0] = first_depth;
    let record = finished_profile(&scenario, config, &depths);
    assert!(
        depths.iter().map(|depth| u32::from(*depth)).sum::<u32>()
            >= (4 * record.epochs[0].reference.operational_max).div_ceil(5)
    );
    assert_eq!(
        matches!(record.outcome, Outcome::CompleteConverged { .. }),
        expected
    );
}

#[test]
fn benign_unit_floor_is_capped_by_maximum_feasible_below_five() {
    let scenario = truth::Scenario::builtin("successor-edge-deep", 20_001).unwrap();
    let reference = truth::Reference::new(
        &scenario,
        1,
        truth::UnitGrouping::ccd(scenario.topology()).unwrap(),
        &truth::ReferenceCalendar::standard(&scenario).unwrap(),
        truth::LAMBDA,
        0.0,
    )
    .unwrap();
    let shallow = reference
        .admissibility(&profile(&raw(&scenario, 3), &scenario).unwrap())
        .unwrap();
    // A stricter synthetic risk budget makes a sub-five floor feasible and necessary.
    let mut config = config(&scenario, 1, UnitMode::CcdUniform);
    config.calendar.lambda_per_hour = shallow
        .per_unit
        .iter()
        .copied()
        .map(|rate| rate * 2.0)
        .fold(shallow.joint, f64::max)
        * 1.01;
    config.tolerance = 0.0;
    let grouping = grouping(&mut config, &scenario).unwrap();
    references(&mut config, &scenario, &grouping).unwrap();
    let depths = config.epochs[0].reference.per_unit_max_feasible.clone();
    assert!(depths.iter().all(|maximum| (1..5).contains(maximum)));
    let record = finished_profile(&scenario, config, &depths);
    assert_eq!(
        record.outcome,
        Outcome::CompleteConverged {
            observed_until_day: 2.0,
        }
    );
}

#[test_case::test_case(43, false; "one unit below depth forty-four")]
#[test_case::test_case(44, true; "all units at depth forty-four")]
fn no_write_limit_failure_convergence_requires_depth_forty_four(first_depth: u8, expected: bool) {
    let scenario = truth::Scenario::builtin("successor-no-write-limit-failure", 20_001).unwrap();
    let record = finished_profile(
        &scenario,
        config(&scenario, 1, UnitMode::CcdUniform),
        &[first_depth, 44],
    );
    assert!(
        u32::from(first_depth) + 44 >= (4 * record.epochs[0].reference.operational_max).div_ceil(5)
    );
    assert_eq!(
        matches!(record.outcome, Outcome::CompleteConverged { .. }),
        expected
    );
}

#[test]
fn interactions_are_counted_once_and_night_hazard_stays_out_of_daily_accounts() {
    let scenario = truth::Scenario::builtin("successor-core-interactions", 20_001).unwrap();
    let deployed = raw(&scenario, 30);
    let pair = truth::InteractionId::new(scenario.topology()[0].core, scenario.topology()[8].core)
        .unwrap();
    let (a, b) = pair.pair();
    let class = &scenario
        .daily_mix()
        .iter()
        .find(|block| block.class.loaded().contains(a) && block.class.loaded().contains(b))
        .unwrap()
        .class;
    let duration = Duration::from_secs(60).min(class.duration());
    let mut state = truth::TruthState::cold(scenario.topology()).unwrap();
    let night = active(
        &scenario,
        &mut state,
        class,
        Duration::ZERO,
        duration,
        deployed,
        true,
        Some(Cause::Truth(truth::FailureCause::Interaction(pair))),
    );
    let daily = active(
        &scenario,
        &mut state,
        class,
        duration,
        duration,
        deployed,
        false,
        Some(Cause::Truth(truth::FailureCause::Interaction(pair))),
    );
    let expected = match &daily.kind {
        ObservationKind::Active { exposure, .. } => {
            exposure
                .interactions
                .iter()
                .find(|entry| entry.source == pair)
                .unwrap()
                .expected_failures
        }
        ObservationKind::Sleep | ObservationKind::ProfileChange => unreachable!(),
    };
    let observed = Duration::from_hours(24);
    let record = assemble(
        config(&scenario, 1, UnitMode::PerCore),
        &scenario,
        vec![
            change(Duration::ZERO, deployed, true),
            night,
            daily,
            sleep(2 * duration, observed, deployed),
        ],
        vec![snapshot(observed, PolicyReport::unavailable())],
        observed,
        None,
        0.0,
    )
    .unwrap();
    record.validate().unwrap();
    let source = interaction(pair);
    let account = record
        .accounts
        .learning
        .iter()
        .find(|entry| entry.source == source)
        .unwrap();
    assert_eq!(account.k, 1);
    assert_eq!(account.expected_failures, expected);
    assert_eq!(account.active_hours, duration.as_secs_f64() / 3600.0);
    assert_eq!(
        record
            .events
            .iter()
            .map(|event| event.period)
            .collect::<Vec<_>>(),
        vec![Period::Night, Period::Day]
    );
    assert_eq!(record.policy.events, Vec::<LearningEvent>::new());
    assert_eq!(
        record.events[0].policy_event.classification,
        Classification::Unreported
    );
    assert_eq!(record.intervals[0].period, Period::Night);
    assert_eq!(record.intervals[0].verified_daily_active_hours, 0.0);
    assert_eq!(
        record.intervals[0]
            .sources
            .iter()
            .find(|entry| entry.source == source)
            .unwrap()
            .expected_failures,
        expected
    );
    assert_eq!(
        record.exposure.observed_seconds,
        (2 * duration).as_secs_f64()
    );
    assert_eq!(
        record.outcome,
        Outcome::CompleteUnconvergedWithCoCrashes {
            observed_until_day: 2.0
        }
    );
}

#[test]
fn firmware_reset_retains_whole_previous_profile_and_recomputes_without_grace() {
    let scenario = truth::Scenario::builtin("successor-firmware-change", 20_001).unwrap();
    let mut previous = raw(&scenario, 30);
    previous.set(scenario.topology()[0].core, 31);
    let stock = raw(&scenario, 0);
    let transition = Duration::from_hours(180 * 24);
    let observed = Duration::from_hours(181 * 24);
    let record = assemble(
        config(&scenario, 181, UnitMode::PerCore),
        &scenario,
        vec![
            change(Duration::ZERO, previous, false),
            sleep(Duration::ZERO, transition, previous),
            change(transition, stock, false),
            change(transition, previous, false),
            sleep(transition, observed, previous),
        ],
        vec![snapshot(observed, PolicyReport::unavailable())],
        observed,
        None,
        0.0,
    )
    .unwrap();
    record.validate().unwrap();
    assert_eq!(
        record
            .epochs
            .iter()
            .map(|epoch| (&*epoch.id, &*epoch.context, epoch.start_day, epoch.end_day))
            .collect::<Vec<_>>(),
        vec![
            ("initial", "ctx-initial", 1.0, 181.0),
            ("firmware-1", "ctx-firmware-1", 181.0, 182.0)
        ]
    );
    let restoration = &record.restorations[0];
    assert_eq!(restoration.previous_profile, previous);
    assert_eq!(restoration.stock_readback, stock);
    assert_eq!(
        restoration.outcome,
        RestorationOutcome::Blocked {
            reason: "policy supplied no qualified whole-profile restoration".to_owned()
        }
    );
    let grouping = truth::UnitGrouping::cores(scenario.topology()).unwrap();
    let calendar = truth::ReferenceCalendar::standard(&scenario).unwrap();
    let new_reference = truth::Reference::new(
        &scenario,
        181,
        grouping,
        &calendar,
        truth::LAMBDA,
        truth::NUMERICAL_TOLERANCE,
    )
    .unwrap();
    let verdict = new_reference
        .admissibility(&profile(&previous, &scenario).unwrap())
        .unwrap();
    let deployed = record
        .intervals
        .iter()
        .find(|interval| interval.start_day == 181.0)
        .unwrap();
    assert_eq!(deployed.epoch, "firmware-1");
    assert_eq!(deployed.profile, previous);
    assert_eq!(deployed.admissibility.joint_ratio, verdict.joint_ratio);
    assert_eq!(deployed.admissibility.unit_ratios, verdict.per_unit_ratios);
    assert_eq!(record.steps[0].before, previous);
    assert_eq!(record.steps[0].after, stock);
    assert_eq!(record.steps.len(), 1);
}

#[test]
fn supplied_epoch_cannot_hide_a_truth_transition() {
    let scenario = truth::Scenario::builtin("successor-moving-cap", 20_001).unwrap();
    let mut config = config(&scenario, 46, UnitMode::CcdUniform);
    config.epochs.push(Epoch {
        id: "initial".to_owned(),
        start_day: 1.0,
        end_day: 47.0,
        context: "ctx-initial".to_owned(),
        reference: DepthReference {
            operational_max: 0,
            certified_upper_bound: 0,
            physical_max: 0,
            per_unit_max_feasible: Vec::new(),
            tolerance: 0.0,
            certificate: "not-used".to_owned(),
        },
    });
    let result = assemble(
        config,
        &scenario,
        Vec::new(),
        vec![snapshot(Duration::ZERO, PolicyReport::unavailable())],
        Duration::ZERO,
        Some(Stop::BudgetExpired),
        0.0,
    );
    assert!(matches!(
        result,
        Err(RunError::Calendar("truth epoch schedule length"))
    ));
}

enum EndingPolicy {
    StopOnObservation,
    Stopped,
    HarnessError,
    NestedBudgetError,
}

impl crate::day_night::Policy for EndingPolicy {
    fn firmware_changed(
        &mut self,
        _machine: &mut togi_machine::Machine<'_>,
    ) -> Result<(), RunError> {
        Ok(())
    }

    fn start_day(
        &mut self,
        _day: u32,
        _machine: &mut togi_machine::Machine<'_>,
    ) -> Result<(), RunError> {
        match self {
            Self::HarnessError => Err(RunError::Calendar("scripted harness operation")),
            Self::NestedBudgetError => Err(RunError::Machine(togi_machine::Error::Other(
                Box::new(RunError::BudgetExpired),
            ))),
            Self::StopOnObservation | Self::Stopped => Ok(()),
        }
    }

    fn observe_day(
        &mut self,
        _day: u32,
        _evidence: &crate::day_night::DayObservation,
        _machine: &mut togi_machine::Machine<'_>,
    ) -> Result<crate::day_night::DayClassification, RunError> {
        *self = Self::Stopped;
        Ok(crate::day_night::DayClassification::NotCpuSuspect)
    }

    fn night(
        &mut self,
        _night: &crate::day_night::Night,
        _machine: &mut togi_machine::Machine<'_>,
    ) -> Result<(), RunError> {
        Ok(())
    }

    fn is_finished(&self) -> bool {
        matches!(self, Self::Stopped)
    }
}

#[test]
fn runner_stopping_before_horizon_returns_truncated_not_complete() {
    let scenario = truth::Scenario::builtin("successor-single-ccd", 20_001).unwrap();
    let record = crate::day_night::successor::run(
        config(&scenario, 1, UnitMode::CcdUniform),
        &mut EndingPolicy::StopOnObservation,
    )
    .unwrap();
    record.validate().unwrap();
    assert!(matches!(record.outcome, Outcome::Truncated { .. }));
    assert_eq!(record.outcome.observed_until_day(), 1.0 + 2.0 / 24.0);
    assert_eq!(record.exposure.observed_seconds, 2.0 * 3600.0);
}

#[test]
fn runner_harness_failure_retains_error_outcome_and_empty_observation_prefix() {
    let scenario = truth::Scenario::builtin("successor-single-ccd", 20_001).unwrap();
    let record = crate::day_night::successor::run(
        config(&scenario, 1, UnitMode::CcdUniform),
        &mut EndingPolicy::HarnessError,
    )
    .unwrap();
    record.validate().unwrap();
    assert!(
        matches!(&record.outcome, Outcome::HarnessError { error, .. }
        if error.contains("scripted harness operation"))
    );
    assert_eq!(record.outcome.observed_until_day(), 1.0);
    assert_eq!(record.exposure.observed_seconds, 0.0);
}

#[test]
fn runner_nested_machine_budget_error_is_truncation_not_harness_failure() {
    let scenario = truth::Scenario::builtin("successor-single-ccd", 20_001).unwrap();
    let record = crate::day_night::successor::run(
        config(&scenario, 1, UnitMode::CcdUniform),
        &mut EndingPolicy::NestedBudgetError,
    )
    .unwrap();
    record.validate().unwrap();
    assert!(matches!(record.outcome, Outcome::Truncated { .. }));
    assert_eq!(record.outcome.observed_until_day(), 1.0);
    assert_eq!(record.exposure.observed_seconds, 0.0);
}
