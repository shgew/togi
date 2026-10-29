//! Record representation regressions for physical night evidence and interruption boundaries.

use test_case::test_case;
use togi_machine::{CoreId, PerCore};

use super::{event, fixture};
use crate::day_night::successor::{
    CeilingEvidence, Classification, EdgeObservation, FinishReason, Outcome, PartialCounts, Period,
    Phase, ProfileStep, ProfileVeto, Qualification, Record, RestorationHold, StepIdentity,
    StepKind, TruthCause, UnitId, ValidationError,
};

fn uid(index: u16) -> UnitId {
    UnitId::new(index).unwrap()
}
fn profile(depth: u8) -> PerCore<u8> {
    (0..4)
        .map(|index| (CoreId::new(index).unwrap(), depth))
        .collect()
}
fn sync_exposure(record: &mut Record) {
    let hours: f64 = record
        .intervals
        .iter()
        .map(|interval| interval.verified_daily_active_hours)
        .sum();
    for account in &mut record.accounts.post_finish {
        account.active_hours = hours;
    }
    record.exposure.observed_seconds = hours * 3600.0;
    record.exposure.eligible_seconds = hours * 3600.0;
    record.exposure.onset_active_seconds = hours * 3600.0;
}

#[test]
fn a_failed_night_probe_retains_its_exact_undeployed_profile_and_truth_risk() {
    let mut record = fixture();
    let base = record.intervals[0].clone();
    let mut before = base.clone();
    before.end_day = 2.5;
    before.verified_daily_active_hours = 12.0;
    let mut night = base.clone();
    night.start_day = 2.5;
    night.end_day = 2.75;
    night.period = Period::Night;
    night.profile = profile(45);
    night.verified_daily_active_hours = 0.0;
    night.admissibility.joint_ratio = 2.0;
    night.admissibility.unit_ratios.fill(2.0);
    night.admissibility.member = false;
    for source in &mut night.sources {
        source.expected_failures = 0.5;
    }
    let mut after = base;
    after.start_day = 2.75;
    after.verified_daily_active_hours = 5826.0;
    let mut failure = event("night-probe", 2.75, TruthCause::Background {}, false, false);
    failure.period = Period::Night;
    failure.profile = night.profile;
    failure.policy_event.classification = Classification::Unreported;
    failure.policy_event.charged = false;
    record.events = vec![failure];
    record.intervals = vec![before, night, after];
    sync_exposure(&mut record);
    record.validate().unwrap();
    let decoded = Record::parse(&serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(decoded, record);
    assert_eq!(record.accounts.post_finish[0].expected_failures, 0.0);
    record.events[0].profile = profile(40);
    assert!(matches!(
        record.validate(),
        Err(ValidationError::Invalid("event pre-interruption profile"))
    ));
}

#[test_case(false; "actual pre-interruption profile")]
#[test_case(true; "stock substitution cannot hide the charge")]
fn a_boundary_event_belongs_to_the_profile_before_its_retreat(spoof: bool) {
    let mut record = fixture();
    let mut before = record.intervals[0].clone();
    before.end_day = 2.0;
    before.verified_daily_active_hours = 8.0;
    let mut after = record.intervals[0].clone();
    after.start_day = 2.0;
    after.profile = profile(0);
    after.verified_daily_active_hours = 5832.0;
    record.intervals = vec![before, after];
    let mut failure = event(
        "retreat-boundary",
        2.0,
        TruthCause::Background {},
        false,
        false,
    );
    failure.profile = profile(if spoof { 0 } else { 40 });
    failure.policy_event.charged = !spoof;
    record.policy.events = vec![failure.policy_event.clone()];
    record.policy.allowance.as_mut().unwrap().consumed = u64::from(!spoof);
    record.events = vec![failure];
    if spoof {
        assert!(matches!(
            record.validate(),
            Err(ValidationError::Invalid("event pre-interruption profile"))
        ));
    } else {
        record.validate().unwrap();
    }
}

#[test]
fn an_event_on_the_initial_boundary_uses_the_initial_profile() {
    let mut record = fixture();
    let mut observed = event("initial", 1.0, TruthCause::Background {}, false, false);
    observed.policy_event.charged = false;
    observed.policy_event.classification = Classification::Unreported;
    record.events = vec![observed];
    record.validate().unwrap();
}

#[test_case(0, false, true; "matching climb unit")]
#[test_case(1, false, false; "mismatched climb unit")]
#[test_case(0, true, false; "non-climb proposal")]
fn a_unit_step_identity_names_that_units_climb(unit: u16, retreat: bool, valid: bool) {
    let mut record = fixture();
    let deployed = record.intervals[0].profile;
    record.steps.push(ProfileStep {
        id: "proposal".into(),
        day: 1.0,
        before: deployed,
        after: deployed,
        kind: if retreat {
            StepKind::Retreat {
                event: None,
                units: vec![uid(0)],
            }
        } else {
            StepKind::Climb {
                unit: uid(0),
                ceiling_depth: 40,
                clean_hold_hours: 50.0,
                qualification: Qualification {
                    id: "qualified".into(),
                    day: 1.0,
                    epoch: "initial".into(),
                    profile: deployed,
                    stock_controls: vec!["stock".into()],
                    fresh_witnesses: vec!["fresh".into()],
                    reused_witnesses: vec![],
                    safety_checks_satisfied: true,
                },
            }
        },
    });
    record.policy.newest_step = Some(StepIdentity::Unit {
        unit: uid(unit),
        proposal: "proposal".into(),
    });
    if valid {
        record.validate().unwrap();
    } else {
        assert!(matches!(
            record.validate(),
            Err(ValidationError::Invalid("newest unit step identity"))
        ));
    }
}

#[test_case(0, 0, true; "measured below five at zero")]
#[test_case(4, 0, true; "measured below five")]
#[test_case(5, 0, true; "measured exactly five")]
#[test_case(6, 1, true; "measured above five")]
#[test_case(50, 45, true; "measured physical write edge")]
#[test_case(51, 45, false; "measured outside write range")]
#[test_case(6, 0, false; "wrong ceiling above five")]
#[test_case(4, 1, false; "wrong ceiling below five")]
fn measured_edge_ceiling_is_five_inside_the_edge(depth: u8, ceiling: u8, valid: bool) {
    let mut record = fixture();
    record.units[0].ceiling_evidence.push(CeilingEvidence {
        day: 1.0,
        epoch: "initial".into(),
        edge: EdgeObservation::Measured { u_depth: depth },
        ceiling_depth: ceiling,
        witnesses: vec!["measured".into()],
    });
    assert_eq!(record.validate().is_ok(), valid);
}

#[test_case(false; "truncated endpoint")]
#[test_case(true; "harness-error endpoint")]
fn a_partial_observation_keeps_the_failure_finish_and_application_at_its_terminal_instant(
    error: bool,
) {
    let mut record = fixture();
    record.intervals[0].end_day = 2.0;
    record.intervals[0].verified_daily_active_hours = 8.0;
    record.intervals[0].phase = Phase::Learning;
    for summary in &mut record.units {
        let finish = summary.finish.as_mut().unwrap();
        finish.day = 2.0;
        finish.reason = FinishReason::CpuSuspectStop;
        finish.witnesses = vec!["terminal".into()];
    }
    record.policy.finishes = record
        .units
        .iter()
        .filter_map(|unit| unit.finish.clone())
        .collect();
    record.policy.stopped_units = (0..4).map(uid).collect();
    let mut observed = event("terminal", 2.0, TruthCause::Background {}, false, false);
    observed.phase = Phase::Learning;
    observed.policy_event.phase = Phase::Learning;
    record.policy.events = vec![observed.policy_event.clone()];
    record.events = vec![observed];
    record.policy.allowance.as_mut().unwrap().consumed = 1;
    record.steps.push(ProfileStep {
        id: "terminal-retreat".into(),
        day: 2.0,
        before: profile(40),
        after: profile(0),
        kind: StepKind::Retreat {
            event: Some("terminal".into()),
            units: (0..4).map(uid).collect(),
        },
    });
    for account in &mut record.accounts.learning {
        account.active_hours = 8.0;
    }
    for account in &mut record.accounts.post_finish {
        account.active_hours = 0.0;
    }
    record.exposure.observed_seconds = 28_800.0;
    record.exposure.eligible_seconds = 28_800.0;
    record.exposure.onset_active_seconds = 28_800.0;
    record.outcome = if error {
        Outcome::HarnessError {
            observed_until_day: 2.0,
            error: "terminal execution failure".into(),
        }
    } else {
        Outcome::Truncated {
            observed_until_day: 2.0,
            reason: "budget expiry".into(),
            lower_bound: PartialCounts {
                daily_interruptions: 1,
                daily_co_crashes: 0,
                active_hours: 8.0,
            },
        }
    };
    record.validate().unwrap();
}

#[test]
fn a_complete_observation_does_not_include_an_event_at_the_unobserved_endpoint() {
    let mut record = fixture();
    let mut observed = event("beyond", 731.0, TruthCause::Background {}, false, false);
    observed.policy_event.charged = false;
    observed.policy_event.classification = Classification::Unreported;
    record.events = vec![observed];
    assert!(matches!(
        record.validate(),
        Err(ValidationError::Invalid("ordered event days"))
    ));
}

#[test_case(101.999_999_910_592_99, 101.999_999_910_593_02, true; "initial rejected profile roundoff")]
#[test_case(26_111.999_977_111_795, 26_111.999_977_111_813, true; "firmware rejected profile roundoff")]
#[test_case(26_111.999_977_111_795, 26_111.999_978_111_795, false; "materially missing allocation")]
#[test_case(1.0, 1.000_000_000_000_000_2, true; "allocation roundoff never grants membership")]
#[test_case(1.0, f64::MAX, false; "overflowing source sums cannot become roundoff")]
fn source_allocation_roundoff_is_relative_without_relaxing_membership(
    joint: f64,
    allocated: f64,
    valid: bool,
) {
    let mut record = fixture();
    for epoch in &mut record.epochs {
        epoch.reference.tolerance = 1e-12;
    }
    for interval in &mut record.intervals {
        interval.profile = profile(30);
        interval.admissibility.joint_ratio = joint;
        interval.admissibility.unit_ratios.fill(allocated);
        interval.admissibility.member = joint <= 1.0 && allocated <= 1.0;
        assert!(!interval.admissibility.member);
    }
    assert_eq!(record.validate().is_ok(), valid);
    if !valid {
        assert!(matches!(
            record.validate(),
            Err(ValidationError::Invalid("joint hazard source allocation"))
        ));
    }
}

fn reclassified(record: &mut Record, full: bool) {
    let deployed = record.intervals[0].profile;
    record.steps.push(ProfileStep {
        id: "reclassified".into(),
        day: 1.0,
        before: deployed,
        after: deployed,
        kind: StepKind::ReclassifiedRestoration {
            retreat: "missing-retreat".into(),
            event: "missing-event".into(),
            reclassified_day: 1.0,
            units: vec![uid(0), uid(1)],
            hold: full.then(|| RestorationHold {
                start_day: 1.0,
                profile: deployed,
                clean_hours: 50.0,
                witnesses: vec!["hold-witness".into()],
            }),
            qualification: full.then(|| Qualification {
                id: "reclassified-qualified".into(),
                day: 1.0,
                epoch: "initial".into(),
                profile: deployed,
                stock_controls: vec!["stock".into()],
                fresh_witnesses: vec!["fresh".into()],
                reused_witnesses: vec![],
                safety_checks_satisfied: true,
            }),
            consumed_before: 3,
            consumed_after: 2,
            crash_vetoes: if full {
                vec![ProfileVeto {
                    event: "veto-event".into(),
                    profile: deployed,
                }]
            } else {
                vec![]
            },
        },
    });
}

#[test_case(true; "with hold, certificate and vetoes")]
#[test_case(false; "absent hold and certificate stay auditable")]
fn a_reclassified_restoration_roundtrips_and_validates_without_semantic_linkage(full: bool) {
    let mut record = fixture();
    reclassified(&mut record, full);
    record.validate().unwrap();
    let bytes = serde_json::to_vec(&record).unwrap();
    assert_eq!(Record::parse(&bytes).unwrap(), record);
}

fn reclassified_mut(record: &mut Record) -> &mut StepKind {
    &mut record.steps.last_mut().unwrap().kind
}

#[test_case(|kind| if let StepKind::ReclassifiedRestoration { retreat, .. } = kind { retreat.clear() }, "reclassified restoration identities"; "empty retreat")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { event, .. } = kind { event.clear() }, "reclassified restoration identities"; "empty event")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { reclassified_day, .. } = kind { *reclassified_day = f64::NAN }, "reclassified restoration identities"; "nonfinite day")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { units, .. } = kind { units.clear() }, "reclassified restoration identities"; "no units")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { units, .. } = kind { units.push(uid(0)) }, "reclassified restoration identities"; "duplicate unit")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { units, .. } = kind { units.push(uid(15)) }, "reclassified restoration identities"; "unit out of range")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind { hold.clean_hours = f64::INFINITY }, "reclassified restoration hold"; "nonfinite hold hours")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind { hold.start_day = 0.0 }, "reclassified restoration hold"; "hold day before origin")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind { hold.witnesses.clear() }, "reclassified restoration hold"; "no hold witnesses")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind { hold.witnesses = vec!["a".into(), "a".into()] }, "reclassified restoration hold"; "duplicate hold witnesses")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind { hold.profile = profile(51) }, "physical-core profile dimensions/write range"; "hold profile out of range")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { qualification: Some(q), .. } = kind { q.id.clear() }, "qualification identity"; "empty qualification")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { crash_vetoes, .. } = kind { crash_vetoes[0].event.clear() }, "reclassified restoration veto event"; "empty veto event")]
#[test_case(|kind| if let StepKind::ReclassifiedRestoration { crash_vetoes, .. } = kind { crash_vetoes[0].profile = profile(51) }, "physical-core profile dimensions/write range"; "veto profile out of range")]
fn a_reclassified_restoration_rejects_unrepresentable_fields(
    corrupt: fn(&mut StepKind),
    reason: &'static str,
) {
    let mut record = fixture();
    reclassified(&mut record, true);
    corrupt(reclassified_mut(&mut record));
    assert!(matches!(
        record.validate(),
        Err(ValidationError::Invalid(found)) if found == reason
    ));
}
