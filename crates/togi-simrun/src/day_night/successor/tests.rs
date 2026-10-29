use serde_json::{Value, json};
use test_case::test_case;
use togi_machine::{CcdId, CoreId, CoreInfo, CpuId};

use super::*;

fn fixture() -> Record {
    let topology: Vec<_> = (0_u16..4)
        .map(|index| CoreInfo {
            core: CoreId::new(i64::from(index)).unwrap(),
            ccd: CcdId::new(i64::from(index / 2)).unwrap(),
            cpus: vec![CpuId::from(index)],
        })
        .collect();
    let membership: Vec<_> = topology
        .iter()
        .enumerate()
        .map(|(index, core)| PolicyUnit {
            unit: UnitId::new(u16::try_from(index).unwrap()).unwrap(),
            cores: vec![core.core],
            kind: UnitPhase::Core,
            parent: Some(UnitId::new(u16::from(core.ccd.get())).unwrap()),
        })
        .collect();
    let profile: PerCore<_> = topology.iter().map(|core| (core.core, 40)).collect();
    let finishes: Vec<_> = membership
        .iter()
        .map(|unit| UnitFinish {
            unit: unit.unit,
            reason: FinishReason::Ceiling,
            day: 1.0,
            witnesses: vec![format!("finish-{}", unit.unit.get())],
        })
        .collect();
    let unit_source = |unit| RiskSource::Unit { unit };
    let accounts = |hours| {
        membership
            .iter()
            .map(|unit| SourceAccount {
                source: unit_source(unit.unit),
                k: 0,
                active_hours: hours,
                expected_failures: 0.0,
            })
            .collect()
    };
    Record {
        provenance: Provenance {
            registration_id: "successor-synthetic".into(),
            registration_sha256: "a".repeat(64),
            source_revision: "source".into(),
            simulator_revision: "truth".into(),
            report_revision: "report".into(),
            binary_sha256: Some("b".repeat(64)),
            input_sha256: Some("c".repeat(64)),
            policy_identity: "synthetic".into(),
            policy_source_sha256: "d".repeat(64),
            parameters_sha256: "e".repeat(64),
            slope: 1.5,
            split: Split::Dev,
            seed: 20001,
            scenario: "benign".into(),
            unit_mode: UnitMode::PerCore,
            topology,
            membership: membership.clone(),
            run_profile: RunProfile::Registered,
        },
        epochs: vec![Epoch {
            id: "initial".into(),
            start_day: 1.0,
            end_day: 731.0,
            context: "initial".into(),
            reference: DepthReference {
                operational_max: 180,
                certified_upper_bound: 180,
                physical_max: 200,
                per_unit_max_feasible: vec![45; 4],
                tolerance: 1e-9,
                certificate: "analytic".into(),
            },
        }],
        intervals: vec![ProfileInterval {
            start_day: 1.0,
            end_day: 731.0,
            profile,
            membership: membership.clone(),
            epoch: "initial".into(),
            verified_daily_active_hours: 5840.0,
            phase: Phase::PostFinish,
            restoration_learning: None,
            admissibility: Admissibility {
                joint_ratio: 0.0,
                unit_ratios: vec![0.0; 4],
                member: true,
            },
            sources: membership
                .iter()
                .map(|unit| SourceRisk {
                    source: unit_source(unit.unit),
                    expected_failures: 0.0,
                })
                .collect(),
        }],
        events: vec![],
        units: finishes
            .iter()
            .map(|finish| UnitSummary {
                unit: finish.unit,
                finish: Some(finish.clone()),
                promotions: 0,
                retreats: Retreats {
                    co_counts: 0,
                    background_counts: 0,
                    non_cpu_counts: 0,
                    unattributed_counts: 0,
                    steps: 0,
                },
                hold_hours: 50.0,
                qualification_launches: 1,
                qualification_hours: 1.0,
                night_hours: 8.0,
                ceiling_evidence: vec![],
            })
            .collect(),
        policy: PolicyReport {
            finishes,
            allowance: AllowanceLedger {
                limit: 2,
                consumed: 0,
                provisional: 0,
                reclassified: 0,
                exhausted_day: None,
            },
            events: vec![],
            stopped_units: vec![],
            inherited_stops: vec![],
            newest_step: None,
            readback: Some(profile),
            unit_membership: membership.clone(),
        },
        steps: vec![],
        restorations: vec![],
        accounts: RiskAccounts {
            learning: accounts(0.0),
            post_finish: accounts(5840.0),
        },
        exposure: Exposure {
            intended_seconds: 21_024_000.0,
            observed_seconds: 21_024_000.0,
            eligible_seconds: 21_024_000.0,
            onset_active_seconds: 21_024_000.0,
        },
        outcome: Outcome::CompleteConverged {
            observed_until_day: 731.0,
        },
        runtime: Runtime {
            wall_seconds: 1.0,
            compilation_seconds: 0.0,
            invocation: None,
            jobs: 1,
            resource_class: "synthetic".into(),
            trial_launches: 4,
        },
    }
}

#[test]
fn four_units_round_trip_without_two_ccd_dimensions() {
    let record = fixture();
    let bytes = serde_json::to_vec(&record).unwrap();
    assert_eq!(Record::parse(&bytes).unwrap(), record);
    assert_eq!(
        record.unit_depths(&record.intervals[0].profile).unwrap(),
        vec![40; 4]
    );
    assert_eq!(
        Record::physical_core_mean_depth(&record.intervals[0].profile),
        Some(40.0)
    );
}

#[test_case(json!([]); "array not object")]
#[test_case(json!({"unregistered": true}); "unknown field")]
fn strict_record_decoding_rejects_invalid_objects(value: Value) {
    assert!(serde_json::from_value::<Record>(value).is_err());
}

#[test]
fn nested_unknown_field_is_rejected() {
    let mut value = serde_json::to_value(fixture()).unwrap();
    value["intervals"][0]["admissibility"]["ignored"] = json!(true);
    assert!(serde_json::from_value::<Record>(value).is_err());
}

#[test_case(r#"{"0":40,"0":41}"#; "duplicate core")]
#[test_case(r#"{"0":51}"#; "outside write range")]
#[test_case(r"[40,40]"; "positional vector")]
fn strict_profile_decoding_refuses_lossy_vectors(profile: &str) {
    let text = serde_json::to_string(&fixture()).unwrap();
    let text = text.replace(r#"{"0":40,"1":40,"2":40,"3":40}"#, profile);
    assert!(serde_json::from_str::<Record>(&text).is_err());
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    MissingCore,
    DuplicateMembership,
    MissingUnit,
    NonuniformUnit,
    Uncertified,
    MembershipRatio,
    AccountHours,
    AccountRisk,
    MissingInteraction,
    EpochGap,
    ProfileGap,
    PhaseReset,
    Exposure,
    Nonfinite,
    PolicyFinish,
    Ledger,
    Ceiling,
}

#[test_case(Fault::MissingCore; "missing physical core")]
#[test_case(Fault::DuplicateMembership; "overlapping units")]
#[test_case(Fault::MissingUnit; "missing unit summary")]
#[test_case(Fault::NonuniformUnit; "nonuniform uniform-unit registration")]
#[test_case(Fault::Uncertified; "uncertified objective")]
#[test_case(Fault::MembershipRatio; "false safety membership")]
#[test_case(Fault::AccountHours; "invented source exposure")]
#[test_case(Fault::AccountRisk; "omitted trajectory hazard")]
#[test_case(Fault::MissingInteraction; "unaccounted interaction")]
#[test_case(Fault::EpochGap; "epoch gap")]
#[test_case(Fault::ProfileGap; "profile gap")]
#[test_case(Fault::PhaseReset; "post-finish phase reset")]
#[test_case(Fault::Exposure; "eligible exceeds actual")]
#[test_case(Fault::Nonfinite; "nonfinite hazard")]
#[test_case(Fault::PolicyFinish; "inconsistent policy finish")]
#[test_case(Fault::Ledger; "ledger hides charge")]
#[test_case(Fault::Ceiling; "write-edge ceiling")]
fn malformed_records_are_refused(fault: Fault) {
    let mut record = fixture();
    match fault {
        Fault::MissingCore => {
            record.intervals[0].profile = [(CoreId::FIRST, 40)].into_iter().collect();
        }
        Fault::DuplicateMembership => {
            record.provenance.membership[1].cores = record.provenance.membership[0].cores.clone();
        }
        Fault::MissingUnit => {
            record.units.pop();
        }
        Fault::NonuniformUnit => {
            let membership = vec![
                PolicyUnit {
                    unit: UnitId::new(0).unwrap(),
                    cores: vec![CoreId::new(0).unwrap(), CoreId::new(1).unwrap()],
                    kind: UnitPhase::Coarse,
                    parent: None,
                },
                PolicyUnit {
                    unit: UnitId::new(1).unwrap(),
                    cores: vec![CoreId::new(2).unwrap(), CoreId::new(3).unwrap()],
                    kind: UnitPhase::Coarse,
                    parent: None,
                },
            ];
            record.provenance.unit_mode = UnitMode::CcdUniform;
            record.provenance.membership = membership.clone();
            record.policy.unit_membership = membership;
            record.policy.readback = None;
            record.intervals[0].profile.set(CoreId::new(1).unwrap(), 41);
            assert!(record.unit_depths(&record.intervals[0].profile).is_err());
        }
        Fault::Uncertified => record.epochs[0].reference.certified_upper_bound += 1,
        Fault::MembershipRatio => record.intervals[0].admissibility.joint_ratio = 1.01,
        Fault::AccountHours => record.accounts.post_finish[0].active_hours += 1.0,
        Fault::AccountRisk => record.intervals[0].sources[0].expected_failures = 0.1,
        Fault::MissingInteraction => record.intervals[0].sources.push(SourceRisk {
            source: RiskSource::Interaction {
                identity: "package".into(),
            },
            expected_failures: 0.1,
        }),
        Fault::EpochGap => record.epochs[0].start_day = 2.0,
        Fault::ProfileGap => record.intervals[0].start_day = 2.0,
        Fault::PhaseReset => record.intervals[0].phase = Phase::Learning,
        Fault::Exposure => record.exposure.eligible_seconds += 1.0,
        Fault::Nonfinite => record.intervals[0].admissibility.joint_ratio = f64::NAN,
        Fault::PolicyFinish => record.policy.finishes.clear(),
        Fault::Ledger => record.policy.allowance.consumed = 1,
        Fault::Ceiling => record.units[0].ceiling_evidence.push(CeilingEvidence {
            day: 1.0,
            epoch: "initial".into(),
            edge: EdgeObservation::CensoredWriteLimit {},
            ceiling_depth: 50,
            witnesses: vec!["edge".into()],
        }),
    }
    assert!(record.validate().is_err(), "{fault:?}");
}

fn event(
    id: &str,
    day: f64,
    truth: TruthCause,
    provisional: bool,
    reclassified: bool,
) -> MachineEvent {
    let record = fixture();
    MachineEvent {
        policy_event: LearningEvent {
            id: id.into(),
            day,
            classification: if reclassified {
                Classification::NonCpu
            } else {
                Classification::Unanswered
            },
            provisional,
            reclassified_from: reclassified.then_some(Classification::Unanswered),
            charged: true,
            phase: Phase::PostFinish,
            unit_phase: UnitPhase::Core,
            newest_step: None,
        },
        period: Period::Day,
        truth,
        phase: Phase::PostFinish,
        restoration_learning: None,
        profile: record.intervals[0].profile,
    }
}

#[test]
fn provisional_reclassification_keeps_unique_charge_and_uncapped_consumption() {
    let mut record = fixture();
    record.events = vec![
        event("e1", 2.0, TruthCause::Background {}, true, true),
        event("e2", 3.0, TruthCause::NonCpu {}, true, false),
        event("e3", 4.0, TruthCause::Unattributed {}, false, false),
    ];
    record.policy.events = record
        .events
        .iter()
        .map(|event| event.policy_event.clone())
        .collect();
    record.policy.allowance = AllowanceLedger {
        limit: 2,
        consumed: 3,
        provisional: 2,
        reclassified: 1,
        exhausted_day: Some(3.0),
    };
    assert!(record.validate().is_ok());
    assert_eq!(record.policy.allowance.remaining(), 0);
    assert_eq!(record.policy.allowance.overspend(), 1);
    assert!(record.policy.allowance.exhausted());
    record.events.push(record.events[0].clone());
    assert!(record.validate().is_err());
}

#[test]
fn interaction_crash_is_counted_once_not_per_implicated_unit() {
    let mut record = fixture();
    let source = RiskSource::Interaction {
        identity: "package".into(),
    };
    record.events = vec![event(
        "joint",
        2.0,
        TruthCause::Co {
            source: source.clone(),
            implicated_units: vec![UnitId::new(0).unwrap(), UnitId::new(1).unwrap()],
        },
        false,
        false,
    )];
    record.policy.events = record
        .events
        .iter()
        .map(|event| event.policy_event.clone())
        .collect();
    record.policy.allowance.consumed = 1;
    record.accounts.post_finish.push(SourceAccount {
        source: source.clone(),
        k: 1,
        active_hours: 5840.0,
        expected_failures: 0.1,
    });
    record.intervals[0].sources.push(SourceRisk {
        source,
        expected_failures: 0.1,
    });
    assert!(record.validate().is_ok());
    assert_eq!(
        record
            .accounts
            .post_finish
            .iter()
            .map(|account| account.k)
            .sum::<u64>(),
        1
    );
    record.accounts.post_finish.last_mut().unwrap().k = 2;
    assert!(record.validate().is_err());
}

#[test_case("complete-converged"; "converged")]
#[test_case("complete-unconverged-crash-free"; "zero-CO unconverged")]
#[test_case("complete-unconverged-with-co-crashes"; "CO unconverged")]
#[test_case("truncated"; "truncated")]
#[test_case("harness-error"; "harness error")]
fn all_outcomes_round_trip_without_censoring(status: &str) {
    let mut value = json!({"status":status,"observed_until_day":731.0});
    if status == "truncated" {
        value["reason"] = json!("budget");
        value["lower_bound"] =
            json!({"daily_interruptions":1,"daily_co_crashes":0,"active_hours":10.0});
    }
    if status == "harness-error" {
        value["error"] = json!("invalid execution");
    }
    let outcome: Outcome = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(outcome).unwrap(), value);
}

#[test]
fn zero_co_is_not_zero_total_interruptions() {
    let mut record = fixture();
    record.outcome = Outcome::CompleteUnconvergedCrashFree {
        observed_until_day: 731.0,
    };
    record.events = vec![event(
        "background",
        2.0,
        TruthCause::Background {},
        false,
        false,
    )];
    record.policy.events = record
        .events
        .iter()
        .map(|event| event.policy_event.clone())
        .collect();
    record.policy.allowance.consumed = 1;
    assert!(record.validate().is_ok());
    assert_eq!(record.events.len(), 1);
    assert_eq!(
        record
            .accounts
            .post_finish
            .iter()
            .map(|account| account.k)
            .sum::<u64>(),
        0
    );
}

#[test_case(0, true)]
#[test_case(15, true)]
#[test_case(16, false)]
fn checked_unit_indices(index: u16, valid: bool) {
    assert_eq!(UnitId::new(index).is_ok(), valid);
    assert_eq!(
        serde_json::from_value::<UnitId>(json!(index)).is_ok(),
        valid
    );
}

#[test]
fn finishing_interruption_keeps_learning_identity_before_immediate_retreat() {
    let mut record = fixture();
    let mut learning = record.intervals[0].clone();
    learning.end_day = 2.0;
    learning.verified_daily_active_hours = 8.0;
    learning.phase = Phase::Learning;
    learning.sources[0].expected_failures = 0.1;
    record.intervals[0].start_day = 2.0;
    record.intervals[0].verified_daily_active_hours = 5832.0;
    for (_, depth) in record.intervals[0].profile.iter_mut() {
        *depth = 0;
    }
    record.intervals.insert(0, learning);
    let finish = record.units[0].finish.as_mut().unwrap();
    finish.day = 2.0;
    finish.reason = FinishReason::CpuSuspectStop;
    finish.witnesses = vec!["last-learning".into()];
    record.policy.finishes = record
        .units
        .iter()
        .filter_map(|unit| unit.finish.clone())
        .collect();
    let mut crash = event(
        "last-learning",
        2.0,
        TruthCause::Co {
            source: RiskSource::Unit {
                unit: UnitId::new(0).unwrap(),
            },
            implicated_units: vec![UnitId::new(0).unwrap()],
        },
        true,
        false,
    );
    crash.phase = Phase::Learning;
    crash.policy_event.phase = Phase::Learning;
    record.events = vec![crash];
    record.policy.events = record
        .events
        .iter()
        .map(|event| event.policy_event.clone())
        .collect();
    record.policy.allowance.consumed = 1;
    record.policy.allowance.provisional = 1;
    for account in &mut record.accounts.learning {
        account.active_hours = 8.0;
    }
    record.accounts.learning[0].k = 1;
    record.accounts.learning[0].expected_failures = 0.1;
    for account in &mut record.accounts.post_finish {
        account.active_hours = 5832.0;
    }
    assert!(record.validate().is_ok());
    assert_eq!(record.accounts.learning[0].k, 1);
    assert_eq!(record.accounts.post_finish[0].k, 0);
}

#[test]
fn firmware_restoration_learning_overlaps_original_post_finish_exposure() {
    let mut record = fixture();
    let previous_profile = record.intervals[0].profile;
    let stock: PerCore<u8> = previous_profile.iter().map(|(core, _)| (core, 0)).collect();
    record.epochs[0].end_day = 181.0;
    let mut changed = record.epochs[0].clone();
    changed.id = "changed".into();
    changed.context = "new-context".into();
    changed.start_day = 181.0;
    changed.end_day = 731.0;
    record.epochs.push(changed);
    let initial = &mut record.intervals[0];
    initial.end_day = 181.0;
    initial.verified_daily_active_hours = 1440.0;
    let mut stock_hold = initial.clone();
    stock_hold.start_day = 181.0;
    stock_hold.end_day = 187.25;
    stock_hold.epoch = "changed".into();
    stock_hold.profile = stock;
    stock_hold.verified_daily_active_hours = 50.0;
    let mut restored = stock_hold.clone();
    restored.start_day = 187.25;
    restored.end_day = 731.0;
    restored.profile = previous_profile;
    restored.verified_daily_active_hours = 4350.0;
    restored.restoration_learning = Some("restore".into());
    record.intervals.extend([stock_hold, restored]);
    record.restorations.push(FirmwareRestoration {
        id: "restore".into(),
        epoch: "changed".into(),
        transition_day: 181.0,
        previous_profile,
        membership: record.provenance.membership.clone(),
        stock_reset_day: 181.0,
        stock_readback: stock,
        outcome: RestorationOutcome::Applied {
            day: 187.25,
            clean_stock_hours: 50.0,
            qualification: Qualification {
                id: "new-exact".into(),
                day: 182.0,
                epoch: "changed".into(),
                profile: previous_profile,
                stock_controls: vec!["new-stock".into()],
                fresh_witnesses: vec!["new-positive".into()],
                reused_witnesses: vec![],
                safety_checks_satisfied: true,
            },
            result: RestorationResult::Retained {},
        },
    });
    assert!(record.validate().is_ok());
    assert_eq!(record.first_finish_day(), Some(1.0));
    assert_eq!(record.intervals[2].phase, Phase::PostFinish);
    assert_eq!(
        record.intervals[2].restoration_learning.as_deref(),
        Some("restore")
    );
    assert_eq!(record.accounts.post_finish[0].active_hours, 5840.0);
}
