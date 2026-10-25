//! A failed next-step qualification: the exact proposal stays blocked for the session and firmware
//! (`docs/spec/tuner.md`, Failed next-step qualification).

use togi_journal::{Activity, CellEvidence, DayStep, Payload, QualificationState, Queue, StepKind};
use togi_machine::{BootId, CcdProfile, ContentId};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{DayQuery, TrialPlan};

use crate::fixture::{Ending, Journal, ccd, content, cores, rebuilt};

/// Whether `plan` qualifies a proposal other than D.
fn qualifies_proposal(plan: &TrialPlan) -> bool {
    plan.activity == Activity::Qualification && plan.task.queue == Queue::Qualification
}

/// Drives passing trials until the first proposal's qualification trial, and fails it on the build
/// `built`.
fn fail_first_proposal(journal: &mut Journal, built: &ContentId) -> CcdProfile {
    for _ in 0..3_500 {
        let plan = journal.until_trial(journal.now());
        if qualifies_proposal(&plan) {
            journal.run_built(&plan, Ending::Fail(None), built);
            return plan.profile;
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("no proposal was qualified");
}

/// The proposals qualified in the next `trials` trials, each run on `built`.
fn qualified_next(journal: &mut Journal, trials: usize, built: &ContentId) -> Vec<CcdProfile> {
    let mut qualified = Vec::new();
    for _ in 0..trials {
        let plan = journal.until_trial(journal.now());
        if qualifies_proposal(&plan) {
            qualified.push(plan.profile.clone());
        }
        journal.run_built(&plan, Ending::Pass, built);
    }
    qualified
}

fn failed_commitments(journal: &Journal, profile: &CcdProfile) -> usize {
    journal
        .events
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::NightQualification(record)
                if &record.profile.ccd == profile
                    && matches!(record.state, QualificationState::Failed { .. }))
        })
        .count()
}

#[test]
fn a_failed_proposal_stays_blocked_across_a_new_night_and_a_rebuilt_backend() {
    let mut journal = Journal::ready();
    let failed = fail_first_proposal(&mut journal, &content());
    assert!(
        qualified_next(&mut journal, 200, &content())
            .iter()
            .all(|q| q != &failed)
    );
    assert_eq!(failed_commitments(&journal, &failed), 1);
    // A cold boot: the failure fabricates no daily event and no retreat.
    journal.boot_start("day1");
    let committed = journal.settle_day(&DayQuery {
        boot: BootId::from("day1".to_owned()),
        invocation: Invocation::ColdBoot {
            pause: Pause::Running,
        },
        now: journal.now(),
    });
    assert!(
        !committed
            .iter()
            .any(|payload| matches!(payload, Payload::DayEvent(_))
                || matches!(
                    payload,
                    Payload::DayStep(DayStep {
                        step: StepKind::Retreat { .. },
                        ..
                    })
                )),
        "{committed:?}"
    );
    // The next night, on a rebuilt backend, still never qualifies it again.
    journal.reboot("tuning1");
    let qualified = qualified_next(&mut journal, 600, &rebuilt());
    assert!(qualified.iter().all(|q| q != &failed), "{qualified:?}");
}

#[test]
fn a_proposal_whose_failed_commitment_never_landed_stays_blocked() {
    let mut journal = Journal::ready();
    // The failing trial ran a rebuilt backend: its record went stale before its failure was committed.
    let failed = fail_first_proposal(&mut journal, &rebuilt());
    let qualified = qualified_next(&mut journal, 600, &rebuilt());
    assert_eq!(failed_commitments(&journal, &failed), 0);
    assert!(qualified.iter().all(|q| q != &failed), "{qualified:?}");
}

#[test]
fn a_fresh_session_may_qualify_a_proposal_an_earlier_session_failed() {
    let mut journal = Journal::ready();
    let failed = fail_first_proposal(&mut journal, &content());
    qualified_next(&mut journal, 1, &content());
    assert_eq!(failed_commitments(&journal, &failed), 1);
    journal.reboot("fresh");
    journal.start_session("20240101T000000Z");
    let qualified = qualified_next(&mut journal, 600, &content());
    assert_eq!(qualified.first(), Some(&failed), "{qualified:?}");
}

#[test]
fn a_failed_certificate_lends_no_witness_to_a_later_qualification() {
    let mut journal = Journal::ready();
    // The first proposal moves CCD 0 only. It passes a cell loading CCD 1 alone, at D's offset there, then
    // fails on a core of CCD 0, which leaves that pass uncontradicted.
    let ccd1_only = |plan: &TrialPlan| {
        !plan.cell.loaded.is_empty()
            && cores()
                .iter()
                .filter(|info| plan.cell.loaded.contains(info.core))
                .all(|info| info.ccd == ccd(1))
    };
    let mut lent = false;
    let failed = loop {
        let plan = journal.until_trial(journal.now());
        if qualifies_proposal(&plan) && lent {
            journal.run(&plan, Ending::Fail(Some(0)));
            break plan.profile;
        }
        lent |= qualifies_proposal(&plan) && ccd1_only(&plan);
        journal.run(&plan, Ending::Pass);
    };
    qualified_next(&mut journal, 1_500, &content());
    let committed = |pick: fn(&QualificationState) -> bool| {
        journal.events.iter().filter_map(move |event| {
            if let Payload::NightQualification(record) = &event.payload
                && pick(&record.state)
            {
                Some((event.seq, record))
            } else {
                None
            }
        })
    };
    let (started, _) = committed(|state| matches!(state, QualificationState::Start { .. }))
        .find(|(_, record)| record.profile.ccd == failed)
        .unwrap();
    let (concluded, _) = committed(|state| matches!(state, QualificationState::Failed { .. }))
        .find(|(_, record)| record.profile.ccd == failed)
        .unwrap();
    let mut completed = 0;
    for (_, record) in committed(|state| matches!(state, QualificationState::Complete { .. })) {
        let QualificationState::Complete { witnesses } = &record.state else {
            unreachable!();
        };
        completed += 1;
        for witness in witnesses {
            if let CellEvidence::Reused { exposure } = witness.evidence {
                assert!(
                    !(started..=concluded).contains(&exposure),
                    "{:?} reused {exposure:?} from the failed {failed:?}",
                    record.profile.ccd
                );
            }
        }
    }
    assert!(completed > 0);
}
