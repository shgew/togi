//! Synthetic Ruleset 14 §5 witnesses, independent of any candidate policy.

use test_case::test_case;
use togi_simrun::day_night::successor::{
    CeilingEvidence, Classification, EdgeObservation, FinishReason, MachineEvent, Period, Phase,
    ProfileStep, ProfileVeto, Qualification, Record, RestorationHold, StepIdentity, StepKind,
    TruthCause, UnitFinish,
};

use super::super::fixture::{Lab, Spec, resync, uid};
use super::{LearningAudit, Rules, audit};

const SCENARIO: &str = "successor-edge-deep";

fn fixture() -> (Lab, Record) {
    let lab = Lab::new(&[SCENARIO], 1);
    let mut record = Spec::new(SCENARIO, 20_001, 1.5).build(&lab);
    let restored = record.steps[1].after;
    let stock = record.steps[0].before;
    let units = vec![uid(0), uid(1)];
    record.steps.push(ProfileStep {
        id: "reverted".into(),
        day: 20.0,
        before: restored,
        after: stock,
        kind: StepKind::Retreat {
            event: Some("source".into()),
            units: units.clone(),
        },
    });
    record.steps.push(ProfileStep {
        id: "restore".into(),
        day: 30.0,
        before: stock,
        after: restored,
        kind: StepKind::ReclassifiedRestoration {
            retreat: "reverted".into(),
            event: "source".into(),
            reclassified_day: 21.0,
            units: units.clone(),
            hold: Some(RestorationHold {
                start_day: 20.0,
                profile: stock,
                clean_hours: 80.0,
                witnesses: vec!["observed-20-30".into()],
            }),
            qualification: Some(Qualification {
                id: "exact-restored".into(),
                day: 29.0,
                epoch: record.epochs[0].id.clone(),
                profile: restored,
                stock_controls: vec!["control".into()],
                fresh_witnesses: vec!["matrix".into()],
                reused_witnesses: vec![],
                safety_checks_satisfied: true,
            }),
            consumed_before: 1,
            consumed_after: 1,
            crash_vetoes: vec![],
        },
    });
    let source = togi_simrun::day_night::successor::LearningEvent {
        id: "source".into(),
        day: 20.0,
        classification: Classification::NonCpu,
        provisional: false,
        reclassified_from: Some(Classification::CpuSuspect),
        charged: true,
        phase: Phase::Learning,
        unit_phase: record.provenance.membership[0].kind,
        newest_step: Some(StepIdentity::Unit {
            unit: uid(1),
            proposal: "climb-1".into(),
        }),
    };
    record.events.push(MachineEvent {
        policy_event: source,
        period: Period::Day,
        truth: TruthCause::NonCpu {},
        phase: Phase::Learning,
        restoration_learning: None,
        profile: restored,
    });
    for summary in &mut record.units {
        summary.finish = Some(UnitFinish {
            unit: summary.unit,
            reason: FinishReason::CpuSuspectStop,
            day: 20.0,
            witnesses: vec!["source".into()],
        });
        summary.retreats.steps = 1;
        summary.ceiling_evidence = vec![CeilingEvidence {
            day: 1.0,
            epoch: record.epochs[0].id.clone(),
            edge: EdgeObservation::Measured { u_depth: 15 },
            ceiling_depth: 10,
            witnesses: vec!["isolated-edge".into()],
        }];
    }
    record.policy.stopped_units = units;
    record.policy.finishes = record
        .units
        .iter()
        .filter_map(|unit| unit.finish.clone())
        .collect();
    retimeline(&mut record);
    (lab, record)
}

fn retimeline(record: &mut Record) {
    let template = record.intervals[0].clone();
    let mut boundaries = vec![1.0];
    boundaries.extend(record.steps.iter().map(|step| step.day));
    boundaries.extend(record.events.iter().map(|event| event.policy_event.day));
    boundaries.push(731.0);
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup();
    record.intervals = boundaries
        .windows(2)
        .map(|pair| {
            let mut interval = template.clone();
            interval.start_day = pair[0];
            interval.end_day = pair[1];
            interval.profile = record
                .steps
                .iter()
                .rfind(|step| step.day <= pair[0])
                .map_or(record.steps[0].before, |step| step.after);
            interval.verified_daily_active_hours = (pair[1] - pair[0]) * 8.0;
            interval.phase = if pair[0] >= 20.0 {
                Phase::PostFinish
            } else {
                Phase::Learning
            };
            for risk in &mut interval.sources {
                risk.expected_failures = 0.0;
            }
            interval
        })
        .collect();
    record.policy.readback = record.steps.last().map(|step| step.after);
    resync(record);
}

fn kind(record: &mut Record) -> &mut StepKind {
    &mut record.steps[3].kind
}

fn judged(lab: &Lab, record: &Record) -> LearningAudit {
    audit(record, &Rules::new(&lab.registration))
}

fn assert_rule(lab: &Lab, record: &Record, rule: &str) {
    assert!(
        judged(lab, record)
            .violations
            .iter()
            .any(|found| found.rule == rule),
        "missing {rule}"
    );
}

#[test]
fn clean_multi_unit_restoration_preserves_depth_stops_and_raw_charge() {
    let (lab, record) = fixture();
    record.validate().expect("coherent restoration record");
    assert_eq!(judged(&lab, &record).violations, vec![]);
    assert_eq!(
        audit(&record, &Rules::new(&lab.registration)).violations,
        vec![]
    );
    assert_eq!(record.policy.allowance.as_ref().unwrap().consumed, 1);
    assert_eq!(record.policy.allowance.as_ref().unwrap().reclassified, 1);
    assert_eq!(record.policy.stopped_units, vec![uid(0), uid(1)]);
}

#[test_case(|record| record.events[0].policy_event.classification = Classification::CpuSuspect, "reclassified-event"; "not non cpu")]
#[test_case(|record| record.events[0].policy_event.reclassified_from = None, "reclassified-event"; "no suspect origin")]
#[test_case(|record| { if let StepKind::ReclassifiedRestoration { reclassified_day, .. } = kind(record) { *reclassified_day = 31.0; } }, "reclassified-event"; "answer after restoration")]
#[test_case(|record| {
    record.events[0].policy_event.day = 19.0;
    if let StepKind::ReclassifiedRestoration { reclassified_day, .. } = kind(record) { *reclassified_day = 19.5; }
}, "reclassified-retreat-units"; "answer strictly before retreat")]
fn classification_must_precede_restoration(edit: fn(&mut Record), rule: &str) {
    let (lab, mut record) = fixture();
    edit(&mut record);
    assert_rule(&lab, &record, rule);
}

#[test]
fn all_reverted_units_are_restored_together() {
    let (lab, mut record) = fixture();
    if let StepKind::ReclassifiedRestoration { units, .. } = kind(&mut record) {
        units.pop();
    }
    assert_rule(&lab, &record, "reclassified-retreat-units");
}

#[test]
fn retreat_cannot_hide_a_physically_reverted_unit() {
    let (lab, mut record) = fixture();
    if let StepKind::Retreat { units, .. } = &mut record.steps[2].kind {
        units.pop();
    }
    for core in &record.provenance.membership[1].cores {
        record.steps[3].after.set(*core, 0);
    }
    let restored = record.steps[3].after;
    if let StepKind::ReclassifiedRestoration { units, qualification, .. } = kind(&mut record) {
        units.pop();
        qualification.as_mut().expect("certificate").profile = restored;
    }
    assert_rule(&lab, &record, "reclassified-retreat-units");
}

#[test]
fn restoration_never_adds_depth() {
    let (lab, mut record) = fixture();
    for core in &record.provenance.membership[0].cores {
        record.steps[3].after.set(*core, 6);
    }
    assert_rule(&lab, &record, "reclassified-depth");
}

#[test_case(|record| { if let StepKind::ReclassifiedRestoration { hold, .. } = kind(record) { *hold = None; } }; "missing hold")]
#[test_case(|record| { let profile = record.steps[3].after; if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind(record) { hold.profile = profile; } }; "wrong profile")]
#[test_case(|record| { if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind(record) { hold.start_day = 19.0; } }; "stale hold")]
#[test_case(|record| record.intervals.iter_mut().filter(|interval| interval.start_day >= 20.0 && interval.end_day <= 30.0).for_each(|interval| interval.verified_daily_active_hours = 0.0); "unverified hold")]
fn restoration_requires_fresh_current_profile_hold(edit: fn(&mut Record)) {
    let (lab, mut record) = fixture();
    edit(&mut record);
    assert_rule(&lab, &record, "reclassified-hold");
}

#[test_case(49.0 + 59.0 / 60.0, false; "49 hours 59 minutes is short")]
#[test_case(50.0, true; "exactly 50 hours qualifies")]
fn restoration_hold_boundary_uses_verified_current_profile_hours(hours: f64, accepted: bool) {
    let (lab, mut record) = fixture();
    if let StepKind::ReclassifiedRestoration { hold: Some(hold), .. } = kind(&mut record) {
        hold.clean_hours = hours;
    }
    record.intervals.iter_mut()
        .find(|interval| interval.start_day == 20.0 && interval.end_day == 30.0)
        .expect("current-profile hold interval").verified_daily_active_hours = hours;
    resync(&mut record);
    record.validate().expect("coherent witnessed hold");
    let result = judged(&lab, &record);
    assert_eq!(result.violations.iter().any(|violation| violation.rule == "reclassified-hold"), !accepted);
    if accepted { assert_eq!(result.violations, vec![]); }
}

#[test]
fn missing_certificate_is_refused() {
    let (lab, mut record) = fixture();
    if let StepKind::ReclassifiedRestoration { qualification, .. } = kind(&mut record) {
        *qualification = None;
    }
    assert_rule(&lab, &record, "reclassified-qualification");
}

#[test]
fn certificate_is_exact_profile() {
    let (lab, mut record) = fixture();
    let stock = record.steps[3].before;
    if let StepKind::ReclassifiedRestoration {
        qualification: Some(certificate),
        ..
    } = kind(&mut record)
    {
        certificate.profile = stock;
    }
    assert_rule(&lab, &record, "qualification-profile");
}

#[test]
fn certificate_context_cannot_be_unknown() {
    let (lab, mut record) = fixture();
    if let StepKind::ReclassifiedRestoration {
        qualification: Some(certificate),
        ..
    } = kind(&mut record)
    {
        certificate.epoch = "other-firmware".into();
    }
    assert_rule(&lab, &record, "qualification-context");
}

#[test]
fn moving_truth_epoch_in_same_observable_context_keeps_certificate() {
    let (lab, mut record) = fixture();
    let mut later = record.epochs[0].clone();
    later.id = "moving".into();
    later.start_day = 29.5;
    record.epochs[0].end_day = 29.5;
    record.epochs.push(later);
    assert_eq!(judged(&lab, &record).violations, vec![]);
}

#[test]
fn changed_firmware_context_invalidates_certificate() {
    let (lab, mut record) = fixture();
    let mut later = record.epochs[0].clone();
    later.id = "firmware".into();
    later.context = "new".into();
    later.start_day = 29.5;
    record.epochs[0].end_day = 29.5;
    record.epochs.push(later);
    assert_rule(&lab, &record, "qualification-context");
}

#[test]
fn historical_consumption_cannot_decrease() {
    let (lab, mut record) = fixture();
    if let StepKind::ReclassifiedRestoration { consumed_after, .. } = kind(&mut record) {
        *consumed_after = 0;
    }
    assert_rule(&lab, &record, "reclassified-refund");
}

#[test]
fn restored_unit_cannot_promote_again() {
    let (lab, mut record) = fixture();
    let mut later = record.steps[0].clone();
    later.id = "reopened".into();
    later.day = 40.0;
    record.steps.push(later);
    assert_rule(&lab, &record, "reclassified-stop");
}

fn crash(record: &mut Record) {
    let mut event = record.events[0].clone();
    event.policy_event.id = "crash".into();
    event.policy_event.day = 40.0;
    event.policy_event.classification = Classification::CpuSuspect;
    event.policy_event.reclassified_from = None;
    event.phase = Phase::PostFinish;
    event.policy_event.phase = Phase::PostFinish;
    event.policy_event.newest_step = Some(StepIdentity::Restoration {
        restoration: "restore".into(),
        units: vec![uid(0), uid(1)],
    });
    record.events.push(event);
    let restored = record.steps[3].after;
    let stock = record.steps[3].before;
    if let StepKind::ReclassifiedRestoration { crash_vetoes, .. } = kind(record) {
        crash_vetoes.push(ProfileVeto {
            event: "crash".into(),
            profile: restored,
        });
    }
    record.steps.push(ProfileStep {
        id: "escalated".into(),
        day: 40.0,
        before: restored,
        after: stock,
        kind: StepKind::Retreat {
            event: Some("crash".into()),
            units: vec![uid(0), uid(1)],
        },
    });
    for unit in &mut record.units {
        unit.retreats.steps += 1;
    }
    retimeline(record);
}

#[test]
fn restored_profile_crash_escalates_vetoes_and_charges_without_new_stop() {
    let (lab, mut record) = fixture();
    crash(&mut record);
    record.validate().expect("coherent restored-profile crash");
    assert_eq!(
        audit(&record, &Rules::new(&lab.registration)).violations,
        vec![]
    );
    assert_eq!(judged(&lab, &record).violations, vec![]);
    assert_eq!(record.policy.allowance.as_ref().unwrap().consumed, 2);
    assert_eq!(
        record
            .policy
            .finishes
            .iter()
            .map(|finish| finish.day)
            .collect::<Vec<_>>(),
        vec![20.0, 20.0]
    );
}

#[test_case(|record| { record.steps.pop(); }; "missing backstep")]
#[test_case(|record| { record.steps[4].day = 39.0; }; "backstep before crash")]
#[test_case(|record| { if let StepKind::Retreat { units, .. } = &mut record.steps[4].kind { units.pop(); } }; "partial escalation")]
fn restored_profile_crash_requires_ordinary_escalation(edit: fn(&mut Record)) {
    let (lab, mut record) = fixture();
    crash(&mut record);
    edit(&mut record);
    assert_rule(&lab, &record, "reclassified-crash-retreat");
}

#[test]
fn restored_profile_crash_requires_whole_profile_veto() {
    let (lab, mut record) = fixture();
    crash(&mut record);
    if let StepKind::ReclassifiedRestoration { crash_vetoes, .. } = kind(&mut record) {
        crash_vetoes.clear();
    }
    assert_rule(&lab, &record, "reclassified-crash-veto");
}

#[test]
fn restored_profile_crash_requires_allowance_charge() {
    let (lab, mut record) = fixture();
    crash(&mut record);
    record.events[1].policy_event.charged = false;
    assert_rule(&lab, &record, "reclassified-crash-charge");
}

#[test_case(|record| record.units[0].ceiling_evidence.clear(); "missing ceiling")]
#[test_case(|record| {
    let mut adverse = record.units[0].ceiling_evidence[0].clone();
    adverse.ceiling_depth = 4;
    record.units[0].ceiling_evidence.insert(0, adverse);
}; "earlier adverse evidence is not erased")]
fn restoration_obeys_current_context_ceilings(edit: fn(&mut Record)) {
    let (lab, mut record) = fixture();
    edit(&mut record);
    assert_rule(&lab, &record, "reclassified-ceiling");
}

#[test]
fn restoration_cannot_violate_another_events_whole_profile_veto() {
    let (lab, mut record) = fixture();
    let mut veto = record.events[0].clone();
    veto.policy_event.id = "other".into();
    veto.policy_event.day = 19.0;
    veto.policy_event.classification = Classification::CpuSuspect;
    record.events.insert(0, veto);
    assert_rule(&lab, &record, "reclassified-veto");
}

#[test]
fn pending_question_blocks_restoration() {
    let (lab, mut record) = fixture();
    record.events[0].policy_event.classification = Classification::Unanswered;
    assert_rule(&lab, &record, "reclassified-pending");
}

#[test]
fn remaining_application_blockers_must_be_satisfied() {
    let (lab, mut record) = fixture();
    if let StepKind::ReclassifiedRestoration {
        qualification: Some(certificate),
        ..
    } = kind(&mut record)
    {
        certificate.safety_checks_satisfied = false;
    }
    assert_rule(&lab, &record, "qualification-safety");
}

#[test]
fn restoration_needs_remaining_historical_allowance() {
    let (lab, mut record) = fixture();
    let mut earlier = record.events[0].clone();
    earlier.policy_event.id = "earlier".into();
    earlier.policy_event.day = 19.0;
    record.events.insert(0, earlier);
    if let StepKind::ReclassifiedRestoration {
        consumed_before,
        consumed_after,
        ..
    } = kind(&mut record)
    {
        *consumed_before = 2;
        *consumed_after = 2;
    }
    assert_rule(&lab, &record, "reclassified-after-exhaustion");
}
