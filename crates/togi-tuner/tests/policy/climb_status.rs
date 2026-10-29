//! The status view of an unfinished unit shows what blocks the next day decision now, not what the last
//! committed hold recorded.

use togi_journal::{ClimbAction, ClimbBlocker, DayEventId, Payload, VetoState};
use togi_machine::BootId;
use togi_tuner::policy::{ClimbUnitState, Projection, VetoProjection, VetoSource};

use crate::{
    climb::veto_successor,
    fixture::{Ending, Journal},
    flow::{cold_boot, steps},
};

/// The blockers every unfinished unit of `projection` shows, one list per unit.
fn held_reasons(projection: &Projection) -> Vec<Vec<ClimbBlocker>> {
    projection
        .climb
        .units
        .iter()
        .map(|unit| match &unit.state {
            ClimbUnitState::Held { reason } => reason.clone(),
            ClimbUnitState::Climbing | ClimbUnitState::Finished { .. } => {
                panic!("expected a held unit, got {:?}", unit.state)
            }
        })
        .collect()
}

#[test]
fn a_night_that_maps_the_edges_after_a_hold_replaces_its_unknown_edge_blockers() {
    let mut journal = Journal::ready();
    journal.boot_start("day1");
    let first = journal.settle_day(&cold_boot("day1", 100));
    let recorded = steps(&first)
        .into_iter()
        .find_map(|step| {
            if let ClimbAction::Hold { blocked_by } = &step.decision {
                Some(blocked_by.clone())
            } else {
                None
            }
        })
        .expect("the first cold boot holds");
    assert!(
        recorded
            .iter()
            .any(|blocker| matches!(blocker, ClimbBlocker::UnknownEdge { .. })),
        "{recorded:?}"
    );
    for reason in held_reasons(&journal.state.project()) {
        assert_eq!(reason, recorded);
    }

    // The night maps both CCDs' edges; no new decision has been committed yet.
    journal.reboot("night1");
    journal.until_filler(|_| Ending::Pass);
    let projection = journal.state.project();
    let shown = held_reasons(&projection);
    assert!(!shown.is_empty(), "status shows no held unit after the night");
    for reason in &shown {
        assert!(
            !reason
                .iter()
                .any(|blocker| matches!(blocker, ClimbBlocker::UnknownEdge { .. })),
            "stale unknown edge: {reason:?}"
        );
    }

    // What status shows is what the next cold boot's decision records.
    journal.boot_start("day2");
    let next = journal.settle_day(&cold_boot("day2", 60_000));
    let recorded = steps(&next)
        .into_iter()
        .find_map(|step| {
            if let ClimbAction::Hold { blocked_by } = &step.decision {
                Some(blocked_by.clone())
            } else {
                None
            }
        })
        .expect("the second cold boot holds");
    for reason in shown {
        assert_eq!(reason, recorded);
    }
}

#[test]
fn qualification_status_advances_before_its_next_progress_commitment() {
    let mut journal = Journal::ready();
    let plan = loop {
        let plan = journal.until_trial(journal.now());
        if plan.task.queue == togi_journal::Queue::Qualification {
            break plan;
        }
        journal.run(&plan, Ending::Pass);
    };
    let qualification = |projection: Projection| {
        projection
            .climb
            .units
            .into_iter()
            .find_map(|unit| unit.next.map(|next| next.qualification))
            .expect("the next day profile has a qualification")
    };
    let before = qualification(journal.state.project());
    journal.run(&plan, Ending::Pass);
    loop {
        let action = journal.next(journal.now()).unwrap();
        let observed = matches!(&action, togi_tuner::policy::Action::Decide(decision)
            if matches!(decision.payload, togi_tuner::policy::PolicyPayload::ExposureObserved(_)));
        journal.commit(action);
        if observed {
            break;
        }
    }
    let after = qualification(journal.state.project());
    assert_eq!(after.outstanding.len() + 1, before.outstanding.len());
    assert_eq!(after.witnesses.len(), before.witnesses.len() + 1);
}

#[test]
fn status_lists_the_predecessor_veto_the_tuner_enforces_under_its_original_identity() {
    let mut journal = veto_successor(-5);
    let before = journal.state.project().vetoes;
    let expected = VetoProjection {
        event: DayEventId::Boot {
            boot: BootId::from("lost".to_owned()),
        },
        state: VetoState::Active,
        source: VetoSource::Predecessor,
    };
    assert_eq!(before, vec![expected.clone()]);
    // The tuner enforces it: the qualified candidate at the vetoed depth is held, not promoted.
    let committed = journal.settle_day(&cold_boot("day1", 60_000));
    assert!(
        !committed.iter().any(|payload| matches!(
            payload,
            Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. })
        )),
        "{committed:?}"
    );
    let held = steps(&committed).into_iter().any(|step| {
        matches!(
            &step.decision,
            ClimbAction::Hold { blocked_by }
                if blocked_by.iter().any(|blocker| matches!(blocker, ClimbBlocker::Veto { event } if *event == expected.event))
        )
    });
    assert!(held, "the hold names the imported veto: {committed:?}");
    assert_eq!(journal.state.project().vetoes, vec![expected]);
}

/// A certified candidate promoted at its cold boot, then a later boot whose record lands `age_s` after the
/// certificate completed, with no decision between them.
fn aged_certificate(age_s: i64) -> Journal {
    let (mut journal, _) = Journal::qualified_compact_after(|journal| {
        let predecessor: togi_journal::SessionId = "20231101T000000Z".parse().unwrap();
        journal.append(
            togi_journal::CampaignStart {
                campaign: togi_journal::CampaignId(togi_journal::Provenance {
                    session: predecessor,
                    seq: togi_journal::Seq::FIRST,
                }),
                allowance: 2,
                origin: togi_journal::CampaignOrigin::Successor {
                    sources: Vec::new(),
                },
                history: togi_journal::CampaignHistory::Complete {},
            }
            .into(),
            &[],
        );
    });
    let completed = journal
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::NightQualification(record)
                if matches!(record.state, togi_journal::QualificationState::Complete { .. }) =>
            {
                Some(event.time)
            }
            _ => None,
        })
        .expect("the candidate was certified");
    let mut first = cold_boot("day1", 60_000);
    first.now = journal.now();
    journal.settle_day(&first);
    let target = completed.as_second() + age_s;
    journal.advance(target - journal.now().as_second());
    journal.boot_start("day2");
    assert_eq!(journal.events.last().unwrap().time.as_second(), target);
    journal
}

#[test_case::test_case(24 * 3_600, false; "a day after")]
#[test_case::test_case(30 * 24 * 3_600 - 1, false; "one second inside the month the certificate stands")]
#[test_case::test_case(30 * 24 * 3_600, true; "at the month the certificate no longer qualifies")]
#[test_case::test_case(45 * 24 * 3_600, true; "long after it")]
fn a_boot_after_the_certificates_month_holds_its_unit_though_no_decision_followed(
    age_s: i64,
    held: bool,
) {
    let mut journal = aged_certificate(age_s);
    let states: Vec<_> = journal
        .state
        .project()
        .climb
        .units
        .iter()
        .map(|unit| (unit.unit.clone(), unit.state.clone()))
        .collect();
    let shown = journal.state.project().climb.units.iter().any(|unit| {
        matches!(unit.state, ClimbUnitState::Held { .. })
            && unit.unit == togi_tuner::UnitId::Ccd(crate::fixture::ccd(0))
    });
    // What the day itself decides at that boot.
    let mut query = cold_boot("day2", 60_000);
    query.now = journal.now();
    let committed = journal.settle_day(&query);
    let promoted = committed.iter().any(|payload| {
        matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))
    });
    assert!(!(held && promoted), "an expired certificate promoted: {committed:?}");
    assert_eq!(shown, held, "{states:?}");
}
