//! Runner/report integration boundaries independent of policy simulation.

use super::fixture::{Ev, Lab};
use super::{FaultKind, ScoreError, Verdict, score, score_validated};
use crate::day_night_successor::registration::{PolicyRole, REDUCED_DEV_LABEL, SourceIdentity};
use togi_simrun::day_night::successor::{Classification, RunProfile, Split};

#[test]
fn a_pending_policy_source_is_not_a_scoreable_registration() {
    let mut lab = Lab::full();
    let policy = lab
        .registration
        .sources
        .policies
        .iter_mut()
        .find(|policy| policy.role == PolicyRole::Candidate)
        .unwrap();
    policy.source = SourceIdentity::Pending {};
    lab.registration.validate().unwrap();
    let selection = lab.selection(Split::Dev, RunProfile::Registered);
    assert!(
        matches!(score(&lab.registration, &lab.sha, &selection, &[]), Err(ScoreError::PendingPolicy(name)) if name == "successor")
    );
}

#[test]
fn an_executor_revision_mismatch_is_incompatible() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut records = lab.records(Split::Dev, RunProfile::Registered, |spec| spec);
    records[0].provenance.runner_revision = "f".repeat(40);
    let result = score_validated(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::Registered),
        &records,
    )
    .unwrap();
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert!(result.faults.iter().any(|fault| matches!(&fault.kind, FaultKind::Incompatible(reasons) if reasons.iter().any(|reason| reason == "runner revision differs"))));
}

#[test]
fn a_candidate_cannot_replace_its_daily_classification_with_absence() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut records = lab.records(Split::Dev, RunProfile::Registered, |spec| {
        spec.event(Ev::background("unreported", 20.0))
    });
    for record in &mut records {
        record.events[0].policy_event.classification = Classification::Unreported;
        record.policy.events.clear();
        record.validate().unwrap();
    }
    let result = score_validated(
        &lab.registration,
        &lab.sha,
        &lab.selection(Split::Dev, RunProfile::Registered),
        &records,
    )
    .unwrap();
    assert_eq!(result.verdict, Verdict::Incomplete);
    assert_eq!(result.complete_cells, 0);
    assert!(result.faults.iter().all(|fault| matches!(&fault.kind,
        FaultKind::Incompatible(reasons) if reasons.iter().any(|reason| reason == "a candidate daily event carries no policy classification"))));
}

#[test]
fn a_comparator_can_honestly_omit_allowance_and_policy_classifications() {
    let lab = Lab::new(&["successor-edge-deep"], 1);
    let mut records = lab.records(Split::Dev, RunProfile::Registered, |spec| {
        spec.event(Ev::background("unreported", 20.0))
    });
    let policy = lab.registration.policy("shipped").unwrap();
    let SourceIdentity::Pinned { revision, sha256 } = &policy.source else {
        unreachable!("pinned fixture")
    };
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
        record.events[0].policy_event.classification = Classification::Unreported;
        record.policy.events.clear();
        record.policy.allowance = None;
        record.validate().unwrap();
    }
    let mut selection = lab.selection(Split::Dev, RunProfile::Registered);
    selection.policy = "shipped".into();
    let result = score_validated(&lab.registration, &lab.sha, &selection, &records).unwrap();
    assert_eq!(result.verdict, Verdict::Diagnostic);
    assert_eq!(result.complete_cells, 2);
    assert!(
        result
            .groups
            .iter()
            .flat_map(|group| &group.seeds)
            .all(|seed| seed.ledger.is_none() && !seed.learning.reported)
    );
    let rendered = crate::day_night_successor::render::render_seeds(&result);
    assert!(rendered.contains("unreported"));
}

#[test]
fn a_complete_single_slope_reduced_invocation_is_exploratory() {
    let lab = Lab::full();
    let records: Vec<_> = lab
        .records(Split::Dev, RunProfile::ReducedDev, |spec| spec)
        .into_iter()
        .filter(|record| record.provenance.slope == 1.5)
        .collect();
    let mut selection = lab.selection(Split::Dev, RunProfile::ReducedDev);
    selection.slopes = Some(vec![1.5]);
    let result = score(&lab.registration, &lab.sha, &selection, &records).unwrap();
    assert_eq!(result.expected_cells, 230);
    assert_eq!(result.complete_cells, 230);
    assert_eq!(result.verdict, Verdict::ExploratoryReduced);
    assert_eq!(result.label.as_deref(), Some(REDUCED_DEV_LABEL));
    let rendered = crate::day_night_successor::render::render(&result);
    assert!(rendered.starts_with(REDUCED_DEV_LABEL));
    assert!(!rendered.contains("PASS"));
}
