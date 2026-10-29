//! D acceptance at the durable journal boundary, including interrupted and snapshot replay.

use test_case::test_case;
use togi_journal::{
    CampaignHistory, ClimbAction, ClimbBlocker, DayAnswer, DayAnswerKind, DayUncleanEnd, Event,
    Payload, Seq, Stamp,
};
use togi_machine::{BootId, EndEvidence, ResetReason};
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
    let derived = source.events.iter().position(|event| matches!(&event.payload, Payload::DayEvent(_))).unwrap();
    assert!(failed < derived);
    // The session was reset after the failure and before anything derived from it landed.
    let successor = successor_of(&source.events[..derived]);
    let history = successor
        .state
        .project()
        .climb
        .campaign
        .unwrap()
        .history;
    assert!(
        matches!(history, CampaignHistory::Unreconstructible { .. }),
        "{history:?}"
    );
}

#[test]
fn a_requalification_failure_with_its_derived_event_charges_the_successor() {
    let source = requalification_failed();
    assert!(source.events.iter().any(|event| matches!(&event.payload, Payload::DayEvent(_))));
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
            togi_tuner::ClimbUnitState::Climbing
            | togi_tuner::ClimbUnitState::Held { .. } => None,
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
        .find_map(|event| match &event.payload {
            Payload::ClimbDecision(record)
                if matches!(record.decision, ClimbAction::Promote { .. }) =>
            {
                Some(record.proposed.clone())
            }
            _ => None,
        })
        .unwrap();
    let campaign = source
        .events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::CampaignStart(start) => Some(start.campaign),
            _ => None,
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
    // The canonical event is its own alias; the others are the two bridged identities.
    for boot in ["first", "second", "third"] {
        assert!(
            event.event == id(boot) || event.aliases.contains(&id(boot)),
            "{boot}: {event:?}"
        );
    }
    assert!(event.aliases.len() <= 3);
    assert!(event.provisional && event.reclassified);
    assert!(!event.witnesses.is_empty());
    assert_eq!(replay(&successor.events).project(), projection);
    assert_eq!(snapshot(&successor.state).project(), projection);
}

/// A successor whose predecessor left only a veto of CCD 0 at `depth`, with no raw daily
/// witness, qualified for the candidate CCD 0 at -5 and waiting at its first cold boot.
fn veto_successor(depth: i64) -> Journal {
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
    assert_eq!(
        snapshot(&journal.state).project(),
        journal.state.project()
    );
}

/// A successor of `source`: its records imported under their original identities, then the
/// campaign start enrolling the successor.
fn successor_of(source: &[Event]) -> Journal {
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
    for event in source {
        successor.append(imported(event).into(), &[]);
    }
    let mut start = start;
    start.origin = togi_journal::CampaignOrigin::Successor {
        sources: Vec::new(),
    };
    successor.append(start.into(), &[]);
    successor
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

pub(crate) fn imported(event: &Event) -> togi_journal::DayImported {
    let wire: serde_json::Value = serde_json::from_str(&event.encode().unwrap()).unwrap();
    togi_journal::DayImported {
        original: togi_journal::Provenance {
            session: crate::fixture::SESSION.parse().unwrap(),
            seq: event.seq,
        },
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
