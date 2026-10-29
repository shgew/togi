//! A failed next-step qualification: the exact proposal stays blocked for the session and firmware
//! (`docs/spec/tuner.md`, Failed next-step qualification).

use togi_journal::{
    Activity, CellEvidence, ClimbAction, ClimbDecision, Payload, QualificationPurpose,
    QualificationState, Queue,
};
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
            return plan.profile.to_ccd().unwrap();
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("no proposal was qualified");
}

/// Drives passing trials through the next proposal cycle, until a proposal's next-step qualification
/// completes or the night asks for its filler, and returns the proposals qualified on the way.
fn next_proposal_cycle(journal: &mut Journal) -> Vec<CcdProfile> {
    let completed = completed_proposals(journal);
    let mut qualified = Vec::new();
    for _ in 0..3_500 {
        let plan = journal.until_trial(journal.now());
        if completed_proposals(journal) > completed || plan.task.queue == Queue::Filler {
            return qualified;
        }
        if qualifies_proposal(&plan) {
            qualified.push(plan.profile.to_ccd().unwrap());
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("the proposal cycle never ended");
}

/// How many next-step qualifications have completed.
fn completed_proposals(journal: &Journal) -> usize {
    journal
        .events
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::NightQualification(record)
                if record.purpose == QualificationPurpose::NextStep
                    && matches!(record.state, QualificationState::Complete { .. }))
        })
        .count()
}

fn failed_commitments(journal: &Journal, profile: &CcdProfile) -> usize {
    journal
        .events
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::NightQualification(record)
                if record.profile.ccd() == profile
                    && matches!(record.state, QualificationState::Failed { .. }))
        })
        .count()
}

#[test]
fn a_failed_proposal_stays_blocked_across_a_new_night_and_a_rebuilt_backend() {
    let mut journal = Journal::ready();
    let failed = fail_first_proposal(&mut journal, &content());
    assert!(
        next_proposal_cycle(&mut journal)
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
                    Payload::ClimbDecision(ClimbDecision {
                        decision: ClimbAction::Retreat { .. },
                        ..
                    })
                )),
        "{committed:?}"
    );
    // The next night, on a rebuilt backend, still never qualifies it again.
    journal.reboot("tuning1");
    journal.rebuild(rebuilt());
    let qualified = next_proposal_cycle(&mut journal);
    assert!(
        qualified.is_empty(),
        "a blocked deterministic candidate has no alternate: {qualified:?}"
    );
    assert!(qualified.iter().all(|q| q != &failed), "{qualified:?}");
}

#[test]
fn a_proposal_whose_failed_commitment_never_landed_stays_blocked() {
    let mut journal = Journal::ready();
    // The failing trial ran a rebuilt backend: its record went stale before its failure was committed.
    let failed = fail_first_proposal(&mut journal, &rebuilt());
    journal.rebuild(rebuilt());
    let qualified = next_proposal_cycle(&mut journal);
    assert!(
        qualified.is_empty(),
        "a blocked deterministic candidate has no alternate: {qualified:?}"
    );
    assert_eq!(failed_commitments(&journal, &failed), 0);
    assert!(qualified.iter().all(|q| q != &failed), "{qualified:?}");
}

#[test]
fn a_fresh_session_may_qualify_a_proposal_an_earlier_session_failed() {
    let mut journal = Journal::ready();
    let failed = fail_first_proposal(&mut journal, &content());
    // The next query commits the failure before it asks for a trial.
    journal.until_trial(journal.now());
    assert_eq!(failed_commitments(&journal, &failed), 1);
    journal.reboot("fresh");
    journal.start_session("20240101T000000Z");
    let qualified = next_proposal_cycle(&mut journal);
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
    let mut isolated: [Option<TrialPlan>; 2] = [None, None];
    let failed = loop {
        let plan = journal.until_trial(journal.now());
        if let togi_journal::Tested::Isolated { ccd, .. } = plan.tested {
            isolated[usize::from(ccd.get())] = Some(plan.clone());
        }
        if qualifies_proposal(&plan) && ccd1_only(&plan) && isolated[1].is_none() {
            isolated[1] = Some(plan.clone());
        }
        if qualifies_proposal(&plan) && lent {
            journal.run(&plan, Ending::Fail(Some(0)));
            break plan.profile.to_ccd().unwrap();
        }
        lent |= qualifies_proposal(&plan) && ccd1_only(&plan);
        journal.run(&plan, Ending::Pass);
    };
    // Real shallower edge failures change the deterministic candidate. CCD 1 remains
    // at stock in the next vector, so its earlier passing cell would otherwise match.
    for (id, plan) in isolated.into_iter().enumerate() {
        let mut plan = plan.unwrap();
        let ccd = ccd(u8::try_from(id).unwrap());
        let offset = togi_machine::TargetOffset::new(-6).unwrap();
        plan.activity = Activity::Reproduction;
        plan.task.queue = Queue::Reproduction;
        plan.task.ordinal = 50_000 + u32::try_from(id).unwrap();
        plan.tested = togi_journal::Tested::Isolated { ccd, offset };
        plan.profile = togi_tuner::UnitProfile::of_ccd(&crate::fixture::zero().with(ccd, offset));
        let core = plan.cell.loaded.iter().next().unwrap().get();
        journal.run(&plan, Ending::Fail(Some(core)));
    }
    next_proposal_cycle(&mut journal);
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
        .find(|(_, record)| *record.profile.ccd() == failed)
        .unwrap();
    let (concluded, _) = committed(|state| matches!(state, QualificationState::Failed { .. }))
        .find(|(_, record)| *record.profile.ccd() == failed)
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
                    record.profile.ccd()
                );
            }
        }
    }
    assert!(completed > 0);
}
