//! D acceptance at the durable journal boundary, including interrupted and snapshot replay.

use test_case::test_case;
use togi_journal::{
    CampaignHistory, ClimbAction, ClimbBlocker, DayAnswer, DayAnswerKind, DayUncleanEnd, Event,
    Payload, Seq, Stamp,
};
use togi_machine::{BootId, CcdProfile, EndEvidence, ResetReason};
use togi_tuner::policy::{Action, Contents, DayQuery, NightQuery, Params, State};

use crate::{
    fixture::{Journal, offsets_of, zero},
    flow::{apply, cold_boot, promoted, steps},
};

pub(crate) fn replay(events: &[Event]) -> State {
    let mut state = State::new(Params::SPEC);
    for event in events {
        let decoded = Event::decode(&event.encode().unwrap()).unwrap();
        state.fold(&decoded).unwrap();
    }
    state
}

pub(crate) fn snapshot(state: &State) -> State {
    serde_json::from_slice(&serde_json::to_vec(state).unwrap()).unwrap()
}

pub(crate) fn drain(state: &mut State, query: &DayQuery, mut sequence: u64) -> Vec<Payload> {
    let mut commitments = Vec::new();
    for _ in 0..100 {
        let owed = state.day(query).unwrap();
        if owed.is_empty() {
            return commitments;
        }
        for decision in owed {
            let event = Event::new(
                Seq::new(sequence).unwrap(),
                Stamp {
                    time: query.now,
                    mono: None,
                },
                query.boot.clone(),
                decision.causes,
                decision.payload.into(),
            );
            commitments.push(event.payload.clone());
            state
                .fold(&Event::decode(&event.encode().unwrap()).unwrap())
                .unwrap();
            sequence += 1;
        }
    }
    panic!("day commitments did not settle");
}

pub(crate) fn adverse() -> Journal {
    let (mut journal, profile) = Journal::promoted_compact();
    journal.boot_start("day2");
    journal.append(
        DayUncleanEnd {
            boot: BootId::from("day1".to_owned()),
            last_checkpoint: None,
            offsets: offsets_of(&profile),
            reset_reason: ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: EndEvidence::unknown(),
        }
        .into(),
        &[],
    );
    journal
}

pub(crate) fn retreated() -> Journal {
    let mut journal = adverse();
    let mut query = cold_boot("day2", 60_000);
    query.now = journal.now();
    let committed = journal.settle_day(&query);
    let retreat = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.decision, ClimbAction::Retreat { .. }))
        .unwrap()
        .clone();
    apply(&mut journal, &retreat, 2, "day2");
    journal.settle_day(&query);
    journal
}

#[test]
fn a_full_fold_equals_every_interrupted_and_replayed_fold() {
    let journal = retreated();
    let expected = serde_json::to_value(replay(&journal.events)).unwrap();
    for cut in 0..=journal.events.len() {
        let mut state = replay(&journal.events[..cut]);
        for event in &journal.events {
            state.fold(event).unwrap();
        }
        assert_eq!(serde_json::to_value(state).unwrap(), expected, "cut {cut}");
    }
}

#[test]
fn a_snapshot_rebuild_reconstructs_permission_allowance_and_stops() {
    let journal = retreated();
    let expected = journal.state.project();
    assert_eq!(expected.climb.campaign.as_ref().unwrap().consumed, 1);
    assert!(expected.climb.units.iter().any(|unit| matches!(
        unit.state,
        togi_tuner::policy::ClimbUnitState::Finished { .. }
    )));
    for cut in 0..=journal.events.len() {
        let mut rebuilt = snapshot(&replay(&journal.events[..cut]));
        for event in &journal.events[cut..] {
            rebuilt.fold(event).unwrap();
        }
        assert_eq!(rebuilt.project(), expected, "snapshot cut {cut}");
    }
}

#[test]
fn a_kill_between_observation_and_derived_commitment_resumes_to_the_same_state() {
    let journal = adverse();
    let mut query = cold_boot("day2", 60_000);
    query.now = journal.now();
    let mut uninterrupted = replay(&journal.events);
    let mut restarted = snapshot(&uninterrupted);
    let seq = journal.events.last().unwrap().seq.get() + 1;
    assert_eq!(
        drain(&mut uninterrupted, &query, seq),
        drain(&mut restarted, &query, seq)
    );
    assert_eq!(uninterrupted.project(), restarted.project());
}

#[test]
fn an_application_failure_before_day_applied_consumes_no_step() {
    let (journal, target) = Journal::promoted_compact();
    let application = journal.events.iter().rposition(|event| matches!(&event.payload, Payload::DayApplied(applied) if applied.target.ccd() == &target)).unwrap();
    let mut state = replay(&journal.events[..application]);
    assert_eq!(
        state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&zero()))
    );
    assert!(
        state
            .project()
            .climb
            .units
            .iter()
            .all(|unit| unit.latest_step.is_none())
    );
    state.fold(&journal.events[application]).unwrap();
    assert_eq!(
        state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&target))
    );
    assert_eq!(
        state
            .project()
            .climb
            .units
            .iter()
            .filter(|unit| unit.latest_step.is_some())
            .count(),
        1
    );
}

#[test]
fn a_lost_qualification_or_retreat_commitment_is_recomputed() {
    let (qualified, _) = promoted();
    let complete = qualified.events.iter().position(|event| matches!(&event.payload, Payload::NightQualification(record) if matches!(record.state, togi_journal::QualificationState::Complete { .. }))).unwrap();
    let state = replay(&qualified.events[..complete]);
    let contents = Contents::new(std::iter::empty());
    let action = state
        .next(&NightQuery {
            now: qualified.events[complete].time,
            contents: &contents,
            unavailable: &std::collections::BTreeMap::new(),
        })
        .unwrap();
    let Action::Decide(decision) = action else {
        panic!("lost qualification was not recomputed: {action:?}");
    };
    assert_eq!(
        Payload::from(decision.payload),
        qualified.events[complete].payload
    );

    let journal = retreated();
    let retreat = journal.events.iter().position(|event| matches!(&event.payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Retreat { .. }))).unwrap();
    let state = replay(&journal.events[..retreat]);
    let mut query = cold_boot("day2", 60_000);
    query.now = journal.events[retreat].time;
    let decision = state.day(&query).unwrap().into_iter().next().unwrap();
    assert_eq!(
        Payload::from(decision.payload),
        journal.events[retreat].payload
    );
}

#[test]
fn event_identity_is_unique_across_provisional_manual_and_imported_witnesses() {
    let mut journal = adverse();
    // Replaying the raw provisional event, including after a snapshot, is not a new event.
    let raw = journal.events.last().unwrap().clone();
    journal.state.fold(&raw).unwrap();
    journal.state = snapshot(&journal.state);
    journal.state.fold(&raw).unwrap();
    let projection = journal.state.project();
    assert_eq!(projection.climb.campaign.as_ref().unwrap().consumed, 1);
    assert!(
        projection
            .climb
            .units
            .iter()
            .any(|unit| matches!(unit.state, togi_tuner::ClimbUnitState::Finished { .. }))
    );
    let applied = projection.applied.unwrap().to_ccd().unwrap();
    journal.append(
        DayAnswer {
            boot: BootId::from("day1".to_owned()),
            offsets: offsets_of(&applied),
            answer: DayAnswerKind::PowerOff,
            uid: 1000,
        }
        .into(),
        &[raw.seq],
    );
    let campaign = journal.state.project().climb.campaign.unwrap();
    assert_eq!(campaign.consumed, 1);
    assert_eq!(campaign.reclassified, 1);
    assert!(
        journal
            .state
            .project()
            .climb
            .units
            .iter()
            .any(|unit| matches!(unit.state, togi_tuner::ClimbUnitState::Finished { .. }))
    );
    journal.append(
        togi_journal::DayRecord {
            boot: togi_journal::CrashedBoot::UncleanEnd {
                boot: BootId::from("day1".to_owned()),
            },
            offsets: togi_journal::RecordedOffsets::new(offsets_of(&applied)).unwrap(),
            source: togi_journal::OffsetSource::Entered { at: journal.now() },
            running: togi_journal::Running::Build,
            note: None,
        }
        .into(),
        &[],
    );
    let manual = journal.events.last().unwrap().clone();
    journal.append(imported(&manual).into(), &[]);
    journal.state = snapshot(&journal.state);
    journal.append(imported(&manual).into(), &[]);
    assert_eq!(journal.state.project().climb.campaign.unwrap().consumed, 1);
}

#[test]
fn unreconstructible_campaign_history_blocks_nonzero_exploration() {
    let (journal, _) = Journal::promoted_compact();
    let promotion = journal.events.iter().position(|event| matches!(&event.payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))).unwrap();
    let mut events = journal.events[..promotion].to_vec();
    for event in &mut events {
        if let Payload::CampaignStart(start) = &mut event.payload {
            start.history = CampaignHistory::Unreconstructible {
                reason: "original history is unavailable".into(),
            };
        }
    }
    let mut state = replay(&events);
    let mut query = cold_boot("blocked", 60_000);
    query.now = journal.now();
    let commitments = drain(&mut state, &query, events.last().unwrap().seq.get() + 1);
    assert!(commitments.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(&record.decision, ClimbAction::Hold { blocked_by } if blocked_by.contains(&ClimbBlocker::CampaignHistory {})))));
    assert!(!commitments.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))));
    assert!(matches!(
        state.project().climb.campaign.unwrap().history,
        CampaignHistory::Unreconstructible { .. }
    ));
}

#[test]
fn a_zero_statistical_cap_neither_blocks_nor_retreats_a_qualified_step() {
    let (journal, target) = Journal::promoted_compact();
    let mut events = journal.events.clone();
    for event in &mut events {
        if let Payload::CcdCap(cap) = &mut event.payload {
            cap.new = togi_machine::TargetOffset::ZERO;
        }
    }
    let mut state = replay(&events);
    assert_eq!(
        state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&target))
    );
    let mut query = cold_boot("s-zero", 60_000);
    query.now = journal.now();
    let commitments = drain(&mut state, &query, events.last().unwrap().seq.get() + 1);
    assert!(!commitments.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Retreat { .. }))));
    assert_eq!(
        state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&target))
    );
}

#[test]
fn a_stock_receipt_for_a_nonzero_proposal_never_invents_an_applied_step() {
    let (journal, target) = Journal::promoted_compact();
    let application = journal.events.iter().rposition(|event| matches!(&event.payload, Payload::DayApplied(applied) if applied.target.ccd() == &target)).unwrap();
    let mut state = replay(&journal.events[..application]);
    let mut receipt = journal.events[application].clone();
    let Payload::DayApplied(applied) = &mut receipt.payload else {
        unreachable!();
    };
    applied.target = togi_journal::CcdVector::expand(zero(), &crate::fixture::cores()).unwrap();
    applied.readbacks = offsets_of(&zero());
    state
        .fold(&Event::decode(&receipt.encode().unwrap()).unwrap())
        .unwrap();
    assert_eq!(
        state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&zero()))
    );
    assert!(
        state
            .project()
            .climb
            .units
            .iter()
            .all(|unit| unit.latest_step.is_none())
    );
    assert_eq!(state.project().climb.campaign.unwrap().consumed, 0);
}

/// D, its monthly night and a conclusive failure of D's re-qualification, with the derived
/// `day.event` and `campaign.charge` the night then wrote.
fn requalification_failed() -> Journal {
    let (mut journal, _) = promoted();
    journal.advance(31 * 24 * 3_600);
    journal.reboot("tuning2");
    for _ in 0..3_500 {
        let plan = journal.until_trial(journal.now());
        if plan.task.queue == togi_journal::Queue::Requalification {
            journal.run(&plan, crate::fixture::Ending::Fail(None));
            let _ = journal.until_end(journal.now(), &std::collections::BTreeMap::new());
            return journal;
        }
        journal.run(&plan, crate::fixture::Ending::Pass);
    }
    panic!("D was never re-qualified");
}

#[test]
fn a_requalification_failure_cut_before_its_derived_event_blocks_the_successor() {
    let source = requalification_failed();
    let failed = source.events.iter().position(|event| matches!(&event.payload, Payload::NightQualification(record) if record.purpose == togi_journal::QualificationPurpose::Requalification && matches!(record.state, togi_journal::QualificationState::Failed { .. }))).unwrap();
    let derived = source
        .events
        .iter()
        .position(|event| matches!(&event.payload, Payload::DayEvent(_)))
        .unwrap();
    assert!(failed < derived);
    // The session was reset after the failure and before anything derived from it landed.
    let successor = successor_of(&source.events[..derived]);
    let history = successor.state.project().climb.campaign.unwrap().history;
    assert!(
        matches!(history, CampaignHistory::Unreconstructible { .. }),
        "{history:?}"
    );
}

#[test]
fn a_requalification_failure_cut_before_its_failed_qualification_blocks_the_successor() {
    let source = requalification_failed();
    let exposure = source
        .events
        .iter()
        .position(|event| matches!(&event.payload, Payload::ExposureObserved(observed) if observed.outcome == togi_journal::Outcome::Failure && matches!(observed.tested, togi_journal::Tested::Exact {})))
        .unwrap();
    let failed = source
        .events
        .iter()
        .position(|event| matches!(&event.payload, Payload::NightQualification(record) if matches!(record.state, togi_journal::QualificationState::Failed { .. })))
        .unwrap();
    assert!(exposure < failed);
    // The session was reset after the conclusive failure of D and before its qualification concluded: the
    // imported qualification is still open and nothing derived from the failure exists.
    let cut = &source.events[..=exposure];
    assert!(cut.iter().any(|event| matches!(&event.payload, Payload::NightQualification(record) if record.purpose == togi_journal::QualificationPurpose::Requalification && matches!(record.state, togi_journal::QualificationState::Start { .. }))));
    assert!(!cut.iter().any(|event| matches!(&event.payload, Payload::NightQualification(record) if matches!(record.state, togi_journal::QualificationState::Failed { .. }))));
    let successor = successor_of(cut);
    let history = successor.state.project().climb.campaign.unwrap().history;
    assert!(
        matches!(history, CampaignHistory::Unreconstructible { .. }),
        "{history:?}"
    );
}

/// D after its monthly re-qualification passed, then a conclusive failure of a filler re-run of D's
/// exact cells, with the derived `day.event` the night then wrote.
fn filler_failed() -> Journal {
    let (mut journal, _) = promoted();
    journal.advance(31 * 24 * 3_600);
    journal.reboot("tuning2");
    for _ in 0..3_500 {
        let plan = journal.until_trial(journal.now());
        if plan.task.queue == togi_journal::Queue::Filler
            && matches!(plan.tested, togi_journal::Tested::Exact {})
            && !plan.profile.is_zero()
        {
            journal.run(&plan, crate::fixture::Ending::Fail(None));
            let _ = journal.until_end(journal.now(), &std::collections::BTreeMap::new());
            return journal;
        }
        journal.run(&plan, crate::fixture::Ending::Pass);
    }
    panic!("D's filler was never failed");
}

#[test]
fn a_charged_filler_failure_stays_reconstructible_under_the_importers_exposure_identity() {
    let source = filler_failed();
    assert!(
        source
            .events
            .iter()
            .any(|event| matches!(&event.payload, Payload::DayEvent(_)))
    );
    let successor = successor_of(&source.events);
    let history = successor.state.project().climb.campaign.unwrap().history;
    assert!(
        matches!(history, CampaignHistory::Complete {}),
        "{history:?}"
    );
}

#[test]
fn a_filler_failure_cut_before_its_derived_event_blocks_the_successor() {
    let source = filler_failed();
    let exposure = source
        .events
        .iter()
        .rposition(|event| matches!(&event.payload, Payload::ExposureObserved(observed) if observed.outcome == togi_journal::Outcome::Failure))
        .unwrap();
    let derived = source
        .events
        .iter()
        .position(|event| matches!(&event.payload, Payload::DayEvent(_)))
        .unwrap();
    assert!(exposure < derived);
    let successor = successor_of(&source.events[..derived]);
    let history = successor.state.project().climb.campaign.unwrap().history;
    assert!(
        matches!(history, CampaignHistory::Unreconstructible { .. }),
        "{history:?}"
    );
}

#[test]
fn a_requalification_failure_with_its_derived_event_charges_the_successor() {
    let source = requalification_failed();
    assert!(
        source
            .events
            .iter()
            .any(|event| matches!(&event.payload, Payload::DayEvent(_)))
    );
    let successor = successor_of(&source.events);
    let campaign = successor.state.project().climb.campaign.unwrap();
    assert!(
        matches!(campaign.history, CampaignHistory::Complete {}),
        "{:?}",
        campaign.history
    );
    assert_eq!(campaign.consumed, 1);
}

#[test]
fn an_imported_night_qualification_never_qualifies_a_profile_in_the_successor() {
    let (source, _) = Journal::promoted_compact();
    assert!(source.events.iter().any(|event| matches!(&event.payload, Payload::NightQualification(record) if matches!(record.state, togi_journal::QualificationState::Complete { .. }))));
    let mut successor = successor_of(&source.events);
    successor.hold_stock();
    successor.boot_start("successor-day");
    let mut query = cold_boot("successor-day", 60_000);
    query.now = successor.now();
    let committed = successor.settle_day(&query);
    assert!(!committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))));
    let projection = successor.state.project();
    assert!(projection.qualifications.is_empty());
    assert_eq!(
        projection.applied,
        Some(togi_tuner::UnitProfile::of_ccd(&zero()))
    );
}

#[test]
fn a_predecessor_stop_without_a_unit_stop_record_is_rederived_and_never_repromoted() {
    let source = retreated();
    let events: Vec<_> = source
        .events
        .iter()
        .filter(|event| !matches!(event.payload, Payload::UnitStop(_)))
        .cloned()
        .collect();
    assert!(events.len() < source.events.len());
    let mut successor = successor_of(&events);
    let mut query = cold_boot("successor", 60_000);
    query.now = successor.now();
    successor.settle_day(&query);
    successor.hold_stock();
    successor.boot_start("successor-day");
    let mut query = cold_boot("successor-day", 60_000);
    query.now = successor.now();
    let committed = successor.settle_day(&query);
    assert!(!committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))));
    let projection = successor.state.project();
    let stopped: Vec<_> = projection
        .climb
        .units
        .iter()
        .filter_map(|unit| match &unit.state {
            togi_tuner::ClimbUnitState::Finished {
                reason, last_clean, ..
            } => Some((reason, *last_clean)),
            togi_tuner::ClimbUnitState::Climbing | togi_tuner::ClimbUnitState::Held { .. } => None,
        })
        .collect();
    assert_eq!(stopped.len(), 1, "{stopped:?}");
    assert!(matches!(
        stopped[0].0,
        togi_journal::UnitStopReason::Predecessor { .. }
    ));
    assert_eq!(stopped[0].1, togi_machine::TargetOffset::ZERO);
    assert_eq!(replay(&successor.events).project(), projection);
    for cut in 0..=successor.events.len() {
        let mut rebuilt = snapshot(&replay(&successor.events[..cut]));
        for event in &successor.events[cut..] {
            rebuilt.fold(event).unwrap();
        }
        assert_eq!(rebuilt.project(), projection, "snapshot cut {cut}");
    }
}

/// A predecessor whose one event was charged under three identities in separate records and
/// bridged later, the second bridge reaching an identity only the first one connected.
#[test]
fn bridged_charge_aliases_consume_one_event_and_keep_their_metadata_across_replay() {
    let (mut source, _) = Journal::promoted_compact();
    let proposed = source
        .events
        .iter()
        .find_map(|event| {
            if let Payload::ClimbDecision(record) = &event.payload
                && matches!(record.decision, ClimbAction::Promote { .. })
            {
                Some(record.proposed.clone())
            } else {
                None
            }
        })
        .unwrap();
    let campaign = source
        .events
        .iter()
        .find_map(|event| {
            if let Payload::CampaignStart(start) = &event.payload {
                Some(start.campaign)
            } else {
                None
            }
        })
        .unwrap();
    let id = |boot: &str| togi_journal::DayEventId::Boot {
        boot: BootId::from(boot.to_owned()),
    };
    let charge = |event: &str, aliases: &[&str], provisional: bool| {
        togi_journal::CampaignCharge {
            campaign,
            event: id(event),
            aliases: aliases.iter().map(|alias| id(alias)).collect(),
            profile: proposed.clone(),
            phase: togi_journal::CampaignEventPhase::Learning,
            unit_phase: togi_journal::UnitPhase::Coarse,
            provisional,
            consumed: 1,
            remaining: 1,
            exhausted: false,
        }
        .into()
    };
    let time = source.now();
    let mut extra = Vec::new();
    for (offset, payload) in [
        charge("first", &[], true),
        charge("second", &[], false),
        charge("third", &[], false),
        // The first bridge joins the second and third; the next joins the first to the second.
        charge("second", &["third"], false),
        charge("first", &["second"], false),
        togi_journal::CampaignReclassified {
            campaign,
            event: id("third"),
            from: togi_journal::Classification::CpuSuspect {
                certainty: togi_journal::Certainty::Provisional,
            },
            to: togi_journal::Classification::NonCpu {},
        }
        .into(),
    ]
    .into_iter()
    .enumerate()
    {
        extra.push(Event::new(
            Seq::new(1_000 + u64::try_from(offset).unwrap()).unwrap(),
            Stamp { time, mono: None },
            BootId::from("boot".to_owned()),
            Vec::new(),
            payload,
        ));
    }
    source.events.extend(extra);
    let mut successor = successor_of(&source.events);
    let mut query = cold_boot("successor", 60_000);
    query.now = successor.now();
    successor.settle_day(&query);
    let projection = successor.state.project();
    let campaign = projection.climb.campaign.as_ref().unwrap();
    assert!(
        matches!(campaign.history, CampaignHistory::Complete {}),
        "{:?}",
        campaign.history
    );
    assert_eq!(campaign.consumed, 1);
    assert_eq!(campaign.provisional, 1);
    assert_eq!(campaign.reclassified, 1);
    assert_eq!(campaign.events.len(), 1);
    let event = &campaign.events[0];
    // The aliases list every identity of the one event, the canonical one included.
    assert_eq!(event.aliases.len(), 3, "{event:?}");
    for boot in ["first", "second", "third"] {
        assert!(event.aliases.contains(&id(boot)), "{boot}: {event:?}");
    }
    assert!(event.aliases.contains(&event.event));
    assert!(event.provisional && event.reclassified);
    // A campaign event's witnesses are the successor's own commitments: the one charge receipt and the
    // one reclassification record, not one per copied charge record.
    let committed = |wanted: fn(&Payload) -> bool| {
        let found: Vec<_> = successor
            .events
            .iter()
            .filter(|record| wanted(&record.payload))
            .map(|record| record.seq)
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        found[0]
    };
    let charged = committed(|payload| matches!(payload, Payload::CampaignCharge(_)));
    let reclassified = committed(|payload| matches!(payload, Payload::CampaignReclassified(_)));
    assert_eq!(event.witnesses, [charged, reclassified]);
    // The one receipt cites every copied charge record that named the event.
    let receipt = successor
        .events
        .iter()
        .find(|record| record.seq == charged)
        .unwrap();
    for original in 1_000..1_005_u64 {
        let copy = successor
            .events
            .iter()
            .find(|record| {
                matches!(&record.payload, Payload::DayImported(copy) if copy.original.seq.get() == original)
            })
            .unwrap_or_else(|| panic!("record {original} was not copied"));
        assert!(
            receipt.cause.contains(&copy.seq),
            "the receipt cites copied charge {original}: {:?}",
            receipt.cause
        );
    }
    assert_eq!(replay(&successor.events).project(), projection);
    assert_eq!(snapshot(&successor.state).project(), projection);
}

/// A successor whose predecessor left only a veto of CCD 0 at `depth`, with no raw daily
/// witness, qualified for the candidate CCD 0 at -5 and waiting at its first cold boot.
pub(crate) fn veto_successor(depth: i64) -> Journal {
    let predecessor: togi_journal::SessionId = "20231101T000000Z".parse().unwrap();
    let (journal, _) = Journal::qualified_compact_after(|journal| {
        let vetoed = zero().with(
            crate::fixture::ccd(0),
            togi_machine::TargetOffset::new(depth).unwrap(),
        );
        let veto = Event::new(
            Seq::new(2).unwrap(),
            Stamp {
                time: journal.now(),
                mono: None,
            },
            BootId::from("lost".to_owned()),
            Vec::new(),
            togi_journal::DayVeto {
                event: togi_journal::DayEventId::Boot {
                    boot: BootId::from("lost".to_owned()),
                },
                profile: togi_journal::ObservedProfile::PerCore {
                    cores: offsets_of(&vetoed),
                },
                state: togi_journal::VetoState::Active,
                evidence: Vec::new(),
                reason: "provisional event vetoes its profile".into(),
            }
            .into(),
        );
        let mut record = imported(&veto);
        record.original.session = predecessor;
        journal.append(record.into(), &[]);
        journal.append(
            togi_journal::CampaignStart {
                campaign: togi_journal::CampaignId(togi_journal::Provenance {
                    session: predecessor,
                    seq: Seq::FIRST,
                }),
                allowance: 2,
                origin: togi_journal::CampaignOrigin::Successor {
                    sources: Vec::new(),
                },
                history: CampaignHistory::Complete {},
            }
            .into(),
            &[],
        );
    });
    journal
}

#[test_case(-5, false; "a veto of the candidate holds it")]
#[test_case(-3, false; "a shallower veto holds the deeper candidate")]
#[test_case(-6, true; "a deeper veto leaves the shallower candidate")]
fn an_imported_veto_without_a_raw_witness_forbids_its_profile_and_deeper(
    depth: i64,
    promotes: bool,
) {
    let mut journal = veto_successor(depth);
    let committed = journal.settle_day(&cold_boot("day1", 60_000));
    assert_eq!(
        committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))),
        promotes
    );
    assert_eq!(replay(&journal.events).project(), journal.state.project());
    assert_eq!(snapshot(&journal.state).project(), journal.state.project());
}

/// A successor of `source`: its records imported under their original identities, then the
/// campaign start enrolling the successor.
fn successor_of(source: &[Event]) -> Journal {
    successor_importing(source, 1)
}

/// [`successor_of`] where an interrupted import resumed and copied every record `copies` times.
fn successor_importing(source: &[Event], copies: usize) -> Journal {
    let start = source
        .iter()
        .find_map(|event| {
            if let Payload::CampaignStart(start) = &event.payload {
                Some(start.clone())
            } else {
                None
            }
        })
        .unwrap();
    let mut successor = Journal::ready();
    successor.start_session("20231120T221320Z");
    for _ in 0..copies {
        for event in source.iter().filter(|event| crosses_reset(event)) {
            successor.append(imported(event).into(), &[]);
        }
    }
    let mut start = start;
    start.origin = togi_journal::CampaignOrigin::Successor {
        sources: Vec::new(),
    };
    // The successor enrolls after everything it imported, as a real one does.
    if let Some(last) = source.last() {
        let lag = last.time.as_second() - successor.now().as_second() + 1;
        if lag > 0 {
            successor.advance(lag);
        }
    }
    successor.append(start.into(), &[]);
    successor
}

/// Whether the session importer copies `event` across a reset: day observations, campaign history and
/// exposures. Trial records stay behind, which is why an exposure keeps its trial's identity.
fn crosses_reset(event: &Event) -> bool {
    let kind = event.kind().to_string();
    kind.starts_with("day.")
        || kind == "exposure.observed"
        || [
            "session.start",
            "climb.decision",
            "unit.mapping",
            "unit.stop",
            "campaign.start",
            "campaign.charge",
            "campaign.reclassified",
            "night.qualification",
            "successor.imported",
        ]
        .contains(&kind.as_str())
}

#[test]
fn a_reset_after_a_denied_nonzero_proposal_stays_reconstructible_with_no_step() {
    let (journal, target) = Journal::promoted_compact();
    let application = journal.events.iter().rposition(|event| matches!(&event.payload, Payload::DayApplied(applied) if applied.target.ccd() == &target)).unwrap();
    let mut events = journal.events[..application].to_vec();
    let mut receipt = journal.events[application].clone();
    let Payload::DayApplied(applied) = &mut receipt.payload else {
        unreachable!();
    };
    applied.target = togi_journal::CcdVector::expand(zero(), &crate::fixture::cores()).unwrap();
    applied.readbacks = offsets_of(&zero());
    events.push(receipt);
    let mut successor = successor_of(&events);
    let mut query = cold_boot("successor", 60_000);
    query.now = successor.now();
    let committed = successor.settle_day(&query);
    let projection = successor.state.project();
    assert!(
        matches!(
            projection.climb.campaign.as_ref().unwrap().history,
            CampaignHistory::Complete {}
        ),
        "{:?}",
        projection.climb.campaign.as_ref().unwrap().history
    );
    assert!(!committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(&record.decision, ClimbAction::Hold { blocked_by } if blocked_by.contains(&ClimbBlocker::CampaignHistory {})))));
    assert_eq!(
        projection.applied,
        Some(togi_tuner::UnitProfile::of_ccd(&zero()))
    );
    assert!(
        projection
            .climb
            .units
            .iter()
            .all(|unit| unit.latest_step.is_none())
    );
    assert_eq!(projection.climb.campaign.as_ref().unwrap().consumed, 0);
    assert_eq!(replay(&successor.events).project(), projection);
}

/// CPU evidence at stock is platform evidence: the live campaign and the successor that imports its
/// raw records agree it consumes nothing and needs no applied-step ancestry.
#[test]
fn a_stock_cpu_evidence_crash_charges_neither_the_live_campaign_nor_its_successor() {
    let (mut source, _) = Journal::promoted_compact();
    source.boot_start("stock-crash");
    source.boot_start("after-stock-crash");
    source.append(
        DayUncleanEnd {
            boot: BootId::from("stock-crash".to_owned()),
            last_checkpoint: None,
            offsets: offsets_of(&zero()),
            reset_reason: ResetReason {
                kind: Some(togi_machine::ResetKind::SyncFlood),
                raw: Vec::new(),
                supported: true,
            },
            evidence: EndEvidence::unknown(),
        }
        .into(),
        &[],
    );
    source.append(
        DayAnswer {
            boot: BootId::from("stock-crash".to_owned()),
            offsets: offsets_of(&zero()),
            answer: DayAnswerKind::Frozen,
            uid: 1000,
        }
        .into(),
        &[],
    );
    source.append(
        togi_journal::DayRecord {
            boot: togi_journal::CrashedBoot::Unobserved { at: source.now() },
            offsets: togi_journal::RecordedOffsets::new(offsets_of(&zero())).unwrap(),
            source: togi_journal::OffsetSource::Entered { at: source.now() },
            running: togi_journal::Running::Idle,
            note: None,
        }
        .into(),
        &[],
    );
    let mut query = cold_boot("after-stock-crash", 60_000);
    query.now = source.now();
    source.settle_day(&query);
    assert_eq!(source.state.project().climb.campaign.unwrap().consumed, 0);
    let successor = successor_of(&source.events);
    let campaign = successor.state.project().climb.campaign.unwrap();
    assert!(
        matches!(campaign.history, CampaignHistory::Complete {}),
        "{:?}",
        campaign.history
    );
    assert_eq!(campaign.consumed, 0);
}

/// A frozen crash of `crashed` that the machine recorded: `boot` ran at the profile, the next boot
/// found it ended without a clean stop, and the owner called it a freeze.
fn frozen_crash(journal: &mut Journal, boot: &str, crashed: &CcdProfile) {
    journal.boot_start_at(boot, "1", crashed);
    journal.boot_start(&format!("after-{boot}"));
    journal.append(
        DayUncleanEnd {
            boot: BootId::from(boot.to_owned()),
            last_checkpoint: None,
            offsets: offsets_of(crashed),
            reset_reason: ResetReason {
                kind: Some(togi_machine::ResetKind::SyncFlood),
                raw: Vec::new(),
                supported: true,
            },
            evidence: EndEvidence::unknown(),
        }
        .into(),
        &[],
    );
    journal.append(
        DayAnswer {
            boot: BootId::from(boot.to_owned()),
            offsets: offsets_of(crashed),
            answer: DayAnswerKind::Frozen,
            uid: 1000,
        }
        .into(),
        &[],
    );
}

fn on_ccd0(depth: i64) -> CcdProfile {
    zero().with(
        crate::fixture::ccd(0),
        togi_machine::TargetOffset::new(depth).unwrap(),
    )
}

#[test_case(-5, false; "a crash at the candidate holds it")]
#[test_case(-3, false; "a shallower crash holds the deeper candidate")]
#[test_case(-6, true; "a deeper crash leaves the shallower candidate")]
fn a_crash_before_the_campaign_enrolled_vetoes_its_profile_without_a_charge(
    depth: i64,
    promotes: bool,
) {
    let (mut journal, _) = Journal::qualified_compact_after(|journal| {
        frozen_crash(journal, "before-enrollment", &on_ccd0(depth));
    });
    let committed = journal.settle_day(&cold_boot("day1", 60_000));
    assert_eq!(
        committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))),
        promotes
    );
    assert!(
        !committed
            .iter()
            .any(|payload| matches!(payload, Payload::CampaignCharge(_)))
    );
    let projection = journal.state.project();
    assert_eq!(projection.climb.campaign.as_ref().unwrap().consumed, 0);
    assert_eq!(replay(&journal.events).project(), projection);
    assert_eq!(snapshot(&journal.state).project(), projection);
}

/// Three crashes before enrollment would exhaust a two-event allowance if they were charged; they are
/// vetoes, so the successor keeps the whole allowance, a complete history and the vetoes.
#[test]
fn crashes_before_the_first_enrollment_keep_the_whole_allowance_across_a_successor() {
    let (source, _) = Journal::promoted_compact_after(|journal| {
        for boot in ["early-1", "early-2", "early-3"] {
            frozen_crash(journal, boot, &on_ccd0(-6));
        }
    });
    assert_eq!(source.state.project().climb.campaign.unwrap().consumed, 0);
    let successor = successor_of(&source.events);
    let projection = successor.state.project();
    let campaign = projection.climb.campaign.as_ref().unwrap();
    assert!(
        matches!(campaign.history, CampaignHistory::Complete {}),
        "{:?}",
        campaign.history
    );
    assert_eq!(campaign.consumed, 0);
    assert!(!projection.vetoes.is_empty());
    assert_eq!(replay(&successor.events).project(), projection);
    assert_eq!(snapshot(&successor.state).project(), projection);
}

/// The boundary is the enrollment: a crash after it is still the campaign's learning event.
#[test]
fn a_crash_after_the_campaign_enrolled_is_still_charged() {
    let (mut journal, target) = Journal::promoted_compact();
    frozen_crash(&mut journal, "after-enrollment", &target);
    let mut query = cold_boot("after-after-enrollment", 60_000);
    query.now = journal.now();
    journal.settle_day(&query);
    assert_eq!(journal.state.project().climb.campaign.unwrap().consumed, 1);
}

/// A journal whose campaign enrolled after a crash at `crashed` and then received the manual record
/// of that crash: the record is appended after enrollment, but the crash it names is older. `named`
/// picks a named boot whose observer record is imported from a predecessor session over an unobserved
/// crash. The candidate waits qualified at its first cold boot.
fn late_record_journal(crashed: &CcdProfile, named: bool) -> Journal {
    let predecessor: togi_journal::SessionId = "20231101T000000Z".parse().unwrap();
    let (journal, _) = Journal::qualified_compact_after(|journal| {
        let early = journal.now();
        if named {
            // The observer records arrive with the session that wrote them, so their topology
            // arrives too; an import without it could not be reconstructed at all.
            let mut old = Journal::ready();
            old.boot_start_at("old", "1", crashed);
            for event in old.events.iter().filter(|event| {
                matches!(
                    event.payload,
                    Payload::SessionStart(_) | Payload::DayBootStart(_)
                )
            }) {
                journal.append(imported(event).into(), &[]);
            }
        }
        journal.advance(3_600);
        journal.append(
            togi_journal::CampaignStart {
                campaign: togi_journal::CampaignId(togi_journal::Provenance {
                    session: predecessor,
                    seq: Seq::FIRST,
                }),
                allowance: 2,
                origin: togi_journal::CampaignOrigin::Fresh {},
                history: CampaignHistory::Complete {},
            }
            .into(),
            &[],
        );
        let entered = journal.now();
        journal.append(
            togi_journal::DayRecord {
                boot: if named {
                    togi_journal::CrashedBoot::UncleanEnd {
                        boot: BootId::from("old".to_owned()),
                    }
                } else {
                    togi_journal::CrashedBoot::Unobserved { at: early }
                },
                offsets: togi_journal::RecordedOffsets::new(offsets_of(crashed)).unwrap(),
                source: togi_journal::OffsetSource::Entered { at: entered },
                running: togi_journal::Running::Idle,
                note: None,
            }
            .into(),
            &[],
        );
    });
    journal
}

#[test_case(-5, false, false; "an unobserved crash at the candidate holds it")]
#[test_case(-3, false, false; "a shallower unobserved crash holds the deeper candidate")]
#[test_case(-6, false, true; "a deeper unobserved crash leaves the shallower candidate")]
#[test_case(-5, true, false; "a named old boot at the candidate holds it")]
#[test_case(-3, true, false; "a shallower named old boot holds the deeper candidate")]
#[test_case(-6, true, true; "a deeper named old boot leaves the shallower candidate")]
fn a_record_of_a_crash_older_than_the_enrollment_is_a_veto_without_charge_or_stop(
    depth: i64,
    named: bool,
    promotes: bool,
) {
    let mut journal = late_record_journal(&on_ccd0(depth), named);
    let committed = journal.settle_day(&cold_boot("day1", 60_000));
    assert_eq!(
        committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))),
        promotes
    );
    assert!(
        !committed
            .iter()
            .any(|payload| matches!(payload, Payload::CampaignCharge(_) | Payload::UnitStop(_)))
    );
    let projection = journal.state.project();
    let campaign = projection.climb.campaign.as_ref().unwrap();
    assert_eq!((campaign.consumed, campaign.remaining), (0, 2));
    assert_eq!(replay(&journal.events).project(), projection);
    assert_eq!(snapshot(&journal.state).project(), projection);
}

/// Three distinct old crashes recorded after enrollment would exhaust a two-event allowance if they
/// were learning events. They are three original vetoes, so the whole allowance remains, live and in
/// the successor that imports them.
#[test]
fn three_old_crashes_recorded_after_the_enrollment_are_three_vetoes_and_no_charge() {
    let crashed = on_ccd0(-6);
    let predecessor: togi_journal::SessionId = "20231101T000000Z".parse().unwrap();
    let (mut source, _) = Journal::qualified_compact_after(|journal| {
        let mut old = Vec::new();
        for _ in 0..3 {
            old.push(journal.now());
            journal.advance(60);
        }
        journal.advance(3_600);
        journal.append(
            togi_journal::CampaignStart {
                campaign: togi_journal::CampaignId(togi_journal::Provenance {
                    session: predecessor,
                    seq: Seq::FIRST,
                }),
                allowance: 2,
                origin: togi_journal::CampaignOrigin::Fresh {},
                history: CampaignHistory::Complete {},
            }
            .into(),
            &[],
        );
        for at in old {
            let entered = journal.now();
            journal.append(
                togi_journal::DayRecord {
                    boot: togi_journal::CrashedBoot::Unobserved { at },
                    offsets: togi_journal::RecordedOffsets::new(offsets_of(&crashed)).unwrap(),
                    source: togi_journal::OffsetSource::Entered { at: entered },
                    running: togi_journal::Running::Idle,
                    note: None,
                }
                .into(),
                &[],
            );
        }
    });
    source.settle_day(&cold_boot("day1", 60_000));
    assert_eq!(
        source
            .events
            .iter()
            .filter(|event| matches!(event.payload, Payload::DayVeto(_)))
            .count(),
        3
    );
    assert!(!source.events.iter().any(|event| matches!(
        event.payload,
        Payload::CampaignCharge(_) | Payload::UnitStop(_)
    )));
    let live = source.state.project();
    let campaign = live.climb.campaign.as_ref().unwrap();
    assert_eq!((campaign.consumed, campaign.remaining), (0, 2));
    assert_eq!(live.vetoes.len(), 3);
    assert_eq!(replay(&source.events).project(), live);
    assert_eq!(snapshot(&source.state).project(), live);

    let successor = successor_of(&source.events);
    let projection = successor.state.project();
    let campaign = projection.climb.campaign.as_ref().unwrap();
    assert!(
        matches!(campaign.history, CampaignHistory::Complete {}),
        "{:?}",
        campaign.history
    );
    assert_eq!((campaign.consumed, campaign.remaining), (0, 2));
    assert_eq!(projection.vetoes.len(), 3);
    assert_eq!(replay(&successor.events).project(), projection);
    assert_eq!(snapshot(&successor.state).project(), projection);
    for events in [&source.events, &successor.events] {
        let expected = replay(events).project();
        for cut in 0..=events.len() {
            let mut rebuilt = snapshot(&replay(&events[..cut]));
            for event in &events[cut..] {
                rebuilt.fold(event).unwrap();
            }
            assert_eq!(rebuilt.project(), expected, "snapshot cut {cut}");
        }
    }
}

/// A resumed import copies the same original records again; one crash is one veto, never a charge.
#[test]
fn a_repeated_import_of_a_crash_older_than_the_enrollment_keeps_one_veto_and_the_whole_allowance() {
    let mut source = late_record_journal(&on_ccd0(-5), false);
    source.settle_day(&cold_boot("day1", 60_000));
    let once = successor_importing(&source.events, 1).state.project();
    let twice = successor_importing(&source.events, 2);
    let projection = twice.state.project();
    let campaign = projection.climb.campaign.as_ref().unwrap();
    assert!(
        matches!(campaign.history, CampaignHistory::Complete {}),
        "{:?}",
        campaign.history
    );
    assert_eq!((campaign.consumed, campaign.remaining), (0, 2));
    assert_eq!(projection.vetoes.len(), once.vetoes.len());
    assert!(!projection.vetoes.is_empty());
    assert_eq!(replay(&twice.events).project(), projection);
    assert_eq!(snapshot(&twice.state).project(), projection);
}

/// The boundary is the enrollment: a nonzero crash after it with no applied step behind it cannot be
/// charged faithfully, so a successor refuses the history instead of assuming it consumed nothing.
#[test]
fn a_crash_after_the_original_enrollment_without_step_history_is_unreconstructible() {
    let predecessor: togi_journal::SessionId = "20231101T000000Z".parse().unwrap();
    let (source, _) = Journal::qualified_compact_after(|journal| {
        journal.append(
            togi_journal::CampaignStart {
                campaign: togi_journal::CampaignId(togi_journal::Provenance {
                    session: predecessor,
                    seq: Seq::FIRST,
                }),
                allowance: 2,
                origin: togi_journal::CampaignOrigin::Fresh {},
                history: CampaignHistory::Complete {},
            }
            .into(),
            &[],
        );
        frozen_crash(journal, "after-start", &on_ccd0(-5));
    });
    let successor = successor_of(&source.events);
    let campaign = successor.state.project().climb.campaign.unwrap();
    assert!(
        matches!(campaign.history, CampaignHistory::Unreconstructible { .. }),
        "{:?}",
        campaign.history
    );
}

/// An event as the session importer copies it: an exposure keeps the identity of its trial, not its
/// own journal seq.
pub(crate) fn imported(event: &Event) -> togi_journal::DayImported {
    let wire: serde_json::Value = serde_json::from_str(&event.encode().unwrap()).unwrap();
    let original = if let Payload::ExposureObserved(observed) = &event.payload {
        togi_journal::Provenance {
            session: observed.source.session,
            seq: observed.source.seq,
        }
    } else {
        togi_journal::Provenance {
            session: crate::fixture::SESSION.parse().unwrap(),
            seq: event.seq,
        }
    };
    togi_journal::DayImported {
        original,
        original_kind: event.kind(),
        raw: serde_json::to_string(&wire["data"]).unwrap(),
        time: event.time,
        boot: event.boot.clone(),
        build: None,
        firmware: None,
        profile: None,
        class: togi_journal::ImportClass::Manual,
        missing: Vec::new(),
    }
}

#[test]
fn the_successor_import_deduplicates_after_a_crash() {
    let source = retreated();
    let start = source
        .events
        .iter()
        .find_map(|event| {
            if let Payload::CampaignStart(start) = &event.payload {
                Some(start.clone())
            } else {
                None
            }
        })
        .unwrap();
    let mut successor = Journal::ready();
    successor.start_session("20231120T221320Z");
    for event in &source.events {
        successor.append(imported(event).into(), &[]);
    }
    successor.state = snapshot(&successor.state);
    for event in &source.events {
        successor.append(imported(event).into(), &[]);
    }
    let mut query = cold_boot("successor", 60_000);
    query.now = successor.now();
    let waiting = successor.state.day(&query).unwrap();
    assert!(!waiting.iter().any(|decision| matches!(
        decision.payload,
        togi_tuner::PolicyPayload::CampaignStart(_)
    )));
    let mut start = start;
    start.origin = togi_journal::CampaignOrigin::Successor {
        sources: Vec::new(),
    };
    successor.append(start.into(), &[]);
    let committed = successor.settle_day(&query);
    let projection = successor.state.project();
    assert_eq!(
        projection.applied,
        Some(togi_tuner::UnitProfile::of_ccd(&zero()))
    );
    assert_eq!(projection.climb.campaign.as_ref().unwrap().consumed, 1);
    assert!(
        matches!(
            projection.climb.campaign.as_ref().unwrap().history,
            CampaignHistory::Complete {}
        ),
        "{:?}",
        projection.climb.campaign.as_ref().unwrap().history
    );
    assert!(
        projection
            .climb
            .units
            .iter()
            .any(|unit| matches!(unit.state, togi_tuner::ClimbUnitState::Finished { .. }))
    );
    assert!(!committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(decision) if matches!(decision.decision, ClimbAction::Promote { .. }))));
    assert_eq!(replay(&successor.events).project(), projection);
}

fn pause(journal: &mut Journal, state: togi_journal::PauseState) {
    journal.append(
        togi_journal::DayPause {
            state,
            source: togi_journal::PauseSource::Command { uid: 1000 },
        }
        .into(),
        &[],
    );
}

fn checkpoint(journal: &mut Journal, seconds: u64) {
    journal.append(
        togi_journal::DayCheckpoint {
            applied_secs: seconds,
            load_mix: togi_journal::LoadMix::default(),
            watchdog: None,
        }
        .into(),
        &[],
    );
}

#[test]
fn paused_hours_and_the_resume_boundary_never_earn_a_climb_hold() {
    let mut journal = Journal::ready();
    journal.observe_stock_in("1", 0);
    pause(&mut journal, togi_journal::PauseState::UntilBoot {});
    journal.advance(50 * 3600);
    checkpoint(&mut journal, 50 * 3600);
    pause(&mut journal, togi_journal::PauseState::Running {});
    journal.advance(3600);
    checkpoint(&mut journal, 51 * 3600);
    journal.advance(3600);
    checkpoint(&mut journal, 52 * 3600);
    let boot = journal.events.last().unwrap().boot.clone();
    let mut query = cold_boot("", 0);
    query.boot = boot;
    query.now = journal.now();
    let committed = journal.settle_day(&query);
    assert_eq!(journal.state.project().climb.units[0].hold.credited_s, 3600);
    assert_eq!(
        committed
            .iter()
            .filter_map(|payload| {
                if let Payload::DayInterval(interval) = payload
                    && matches!(
                        interval.state,
                        togi_journal::IntervalState::Applied | togi_journal::IntervalState::Paused
                    )
                {
                    Some(interval.state)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>(),
        vec![
            togi_journal::IntervalState::Paused,
            togi_journal::IntervalState::Paused,
            togi_journal::IntervalState::Applied,
        ]
    );
    assert_eq!(replay(&journal.events).project(), journal.state.project());
}

#[test]
fn a_paused_cold_boot_cannot_promote_even_after_resume_or_snapshot_replay() {
    let (mut journal, _) = Journal::qualified_compact_after(|_| {});
    pause(
        &mut journal,
        togi_journal::PauseState::Until {
            deadline: journal_deadline(),
        },
    );
    journal.boot_start("paused");
    let mut query = cold_boot("paused", 0);
    query.now = journal.now();
    let committed = journal.settle_day(&query);
    assert!(steps(&committed).iter().any(|step| matches!(
        &step.decision,
        ClimbAction::Hold { blocked_by } if blocked_by.contains(&ClimbBlocker::Paused {})
    )));
    assert!(!steps(&committed).iter().any(|step| matches!(
        step.decision,
        ClimbAction::Promote { .. } | ClimbAction::Restore { .. }
    )));
    let projection = journal.state.project();
    assert!(projection.climb.profiles.paused_saved.is_some());
    assert!(projection.climb.units.iter().all(|unit| matches!(
        &unit.state,
        togi_tuner::ClimbUnitState::Held { reason } if reason.contains(&ClimbBlocker::Paused {})
    )));
    pause(&mut journal, togi_journal::PauseState::Running {});
    journal.state = snapshot(&replay(&journal.events));
    query.now = journal.now();
    let resumed = journal.settle_day(&query);
    assert!(!steps(&resumed).iter().any(|step| matches!(
        step.decision,
        ClimbAction::Promote { .. } | ClimbAction::Restore { .. }
    )));
    journal.boot_start("running-next-boot");
    query = cold_boot("running-next-boot", 0);
    query.now = journal.now();
    let next = journal.settle_day(&query);
    assert!(
        steps(&next)
            .iter()
            .any(|step| matches!(step.decision, ClimbAction::Promote { .. }))
    );
}

fn journal_deadline() -> jiff::Timestamp {
    crate::fixture::at(1_000_000)
}

#[test]
fn a_crash_at_paused_stock_neither_charges_the_saved_step_nor_stops_a_unit() {
    let (mut journal, saved) = Journal::promoted_compact();
    pause(&mut journal, togi_journal::PauseState::UntilBoot {});
    let before_zero = journal.state.project().climb.profiles;
    assert_eq!(before_zero.applied, before_zero.paused_saved);
    assert_eq!(before_zero.read_back, Some(offsets_of(&saved)));
    journal.append(
        togi_journal::DayOffsetsChanged {
            offsets: offsets_of(&zero()),
        }
        .into(),
        &[],
    );
    checkpoint(&mut journal, 0);
    let paused = journal.state.project();
    assert_eq!(
        paused.climb.profiles.paused_saved.as_ref().unwrap().cores,
        togi_journal::CcdVector::expand(saved, &crate::fixture::cores())
            .unwrap()
            .cores()
            .clone()
    );
    assert_eq!(paused.climb.profiles.read_back, Some(offsets_of(&zero())));
    assert!(
        paused
            .climb
            .profiles
            .applied
            .as_ref()
            .unwrap()
            .cores
            .iter()
            .all(|(_, offset)| offset.get() == 0)
    );
    journal.boot_start("after-paused-crash");
    journal.append(
        DayUncleanEnd {
            boot: BootId::from("day1".to_owned()),
            last_checkpoint: None,
            offsets: offsets_of(&zero()),
            reset_reason: ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: EndEvidence::unknown(),
        }
        .into(),
        &[],
    );
    let mut query = cold_boot("after-paused-crash", 0);
    query.now = journal.now();
    let committed = journal.settle_day(&query);
    assert!(
        !committed
            .iter()
            .any(|payload| matches!(payload, Payload::CampaignCharge(_) | Payload::UnitStop(_)))
    );
    let projection = journal.state.project();
    assert_eq!(projection.climb.campaign.as_ref().unwrap().consumed, 0);
    assert!(
        projection
            .climb
            .units
            .iter()
            .all(|unit| !matches!(unit.state, togi_tuner::ClimbUnitState::Finished { .. }))
    );
    assert_eq!(replay(&journal.events).project(), projection);
}
