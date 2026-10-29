//! D acceptance at the durable journal boundary, including interrupted and snapshot replay.

use togi_journal::{CampaignHistory, ClimbAction, ClimbBlocker, DayAnswer, DayAnswerKind, DayUncleanEnd, Event, Payload, Seq, Stamp};
use togi_machine::{BootId, EndEvidence, ResetReason};
use togi_tuner::policy::{Action, Contents, DayQuery, NightQuery, Params, State};

use crate::{fixture::{Journal, offsets_of, zero}, flow::{apply, cold_boot, promoted, steps}};

fn replay(events: &[Event]) -> State {
    let mut state = State::new(Params::SPEC);
    for event in events {
        let decoded = Event::decode(&event.encode().unwrap()).unwrap();
        state.fold(&decoded).unwrap();
    }
    state
}

fn snapshot(state: &State) -> State {
    serde_json::from_slice(&serde_json::to_vec(state).unwrap()).unwrap()
}

fn drain(state: &mut State, query: &DayQuery, mut sequence: u64) -> Vec<Payload> {
    let mut commitments = Vec::new();
    for _ in 0..100 {
        let owed = state.day(query).unwrap();
        if owed.is_empty() { return commitments; }
        for decision in owed {
            let event = Event::new(Seq::new(sequence).unwrap(), Stamp { time: query.now, mono: None }, query.boot.clone(), decision.causes, decision.payload.into());
            commitments.push(event.payload.clone());
            state.fold(&Event::decode(&event.encode().unwrap()).unwrap()).unwrap();
            sequence += 1;
        }
    }
    panic!("day commitments did not settle");
}

fn adverse() -> Journal {
    let (mut journal, profile) = Journal::promoted_compact();
    journal.boot_start("day2");
    journal.append(DayUncleanEnd {
        boot: BootId::from("day1".to_owned()), last_checkpoint: None, offsets: offsets_of(&profile),
        reset_reason: ResetReason { kind: None, raw: Vec::new(), supported: false }, evidence: EndEvidence::unknown(),
    }.into(), &[]);
    journal
}

fn retreated() -> Journal {
    let mut journal = adverse();
    let mut query = cold_boot("day2", 60_000);
    query.now = journal.now();
    let committed = journal.settle_day(&query);
    let retreat = steps(&committed).into_iter().find(|step| matches!(step.decision, ClimbAction::Retreat { .. })).unwrap().clone();
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
        for event in &journal.events { state.fold(event).unwrap(); }
        assert_eq!(serde_json::to_value(state).unwrap(), expected, "cut {cut}");
    }
}

#[test]
fn a_snapshot_rebuild_reconstructs_permission_allowance_and_stops() {
    let journal = retreated();
    let expected = journal.state.project();
    assert_eq!(expected.climb.campaign.as_ref().unwrap().consumed, 1);
    assert!(expected.climb.units.iter().any(|unit| matches!(unit.state, togi_tuner::policy::ClimbUnitState::Finished { .. })));
    for cut in 0..=journal.events.len() {
        let mut rebuilt = snapshot(&replay(&journal.events[..cut]));
        for event in &journal.events[cut..] { rebuilt.fold(event).unwrap(); }
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
    assert_eq!(drain(&mut uninterrupted, &query, seq), drain(&mut restarted, &query, seq));
    assert_eq!(uninterrupted.project(), restarted.project());
}

#[test]
fn an_application_failure_before_day_applied_consumes_no_step() {
    let (journal, target) = Journal::promoted_compact();
    let application = journal.events.iter().rposition(|event| matches!(&event.payload, Payload::DayApplied(applied) if applied.target.ccd() == &target)).unwrap();
    let mut state = replay(&journal.events[..application]);
    assert_eq!(state.project().applied, Some(togi_tuner::UnitProfile::of_ccd(&zero())));
    assert!(state.project().climb.units.iter().all(|unit| unit.latest_step.is_none()));
    state.fold(&journal.events[application]).unwrap();
    assert_eq!(state.project().applied, Some(togi_tuner::UnitProfile::of_ccd(&target)));
    assert_eq!(state.project().climb.units.iter().filter(|unit| unit.latest_step.is_some()).count(), 1);
}

#[test]
fn a_lost_qualification_or_retreat_commitment_is_recomputed() {
    let (qualified, _) = promoted();
    let complete = qualified.events.iter().position(|event| matches!(&event.payload, Payload::NightQualification(record) if matches!(record.state, togi_journal::QualificationState::Complete { .. }))).unwrap();
    let state = replay(&qualified.events[..complete]);
    let contents = Contents::new(std::iter::empty());
    let action = state.next(&NightQuery { now: qualified.events[complete].time, contents: &contents, unavailable: &std::collections::BTreeMap::new() }).unwrap();
    let Action::Decide(decision) = action else { panic!("lost qualification was not recomputed: {action:?}"); };
    assert_eq!(Payload::from(decision.payload), qualified.events[complete].payload);

    let journal = retreated();
    let retreat = journal.events.iter().position(|event| matches!(&event.payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Retreat { .. }))).unwrap();
    let state = replay(&journal.events[..retreat]);
    let mut query = cold_boot("day2", 60_000);
    query.now = journal.events[retreat].time;
    let decision = state.day(&query).unwrap().into_iter().next().unwrap();
    assert_eq!(Payload::from(decision.payload), journal.events[retreat].payload);
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
    let applied = projection.applied.unwrap().to_ccd().unwrap();
    journal.append(DayAnswer { boot: BootId::from("day1".to_owned()), offsets: offsets_of(&applied), answer: DayAnswerKind::PowerOff, uid: 1000 }.into(), &[raw.seq]);
    let campaign = journal.state.project().climb.campaign.unwrap();
    assert_eq!(campaign.consumed, 1);
    assert_eq!(campaign.reclassified, 1);
    journal.append(togi_journal::DayRecord {
        boot: togi_journal::CrashedBoot::UncleanEnd { boot: BootId::from("day1".to_owned()) },
        offsets: togi_journal::RecordedOffsets::new(offsets_of(&applied)).unwrap(),
        source: togi_journal::OffsetSource::Entered { at: journal.now() },
        running: togi_journal::Running::Build, note: None,
    }.into(), &[]);
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
        if let Payload::CampaignStart(start) = &mut event.payload { start.history = CampaignHistory::Unreconstructible { reason: "original history is unavailable".into() }; }
    }
    let mut state = replay(&events);
    let mut query = cold_boot("blocked", 60_000);
    query.now = journal.now();
    let commitments = drain(&mut state, &query, events.last().unwrap().seq.get() + 1);
    assert!(commitments.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(&record.decision, ClimbAction::Hold { blocked_by } if blocked_by.contains(&ClimbBlocker::CampaignHistory {})))));
    assert!(!commitments.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Promote { .. }))));
    assert!(matches!(state.project().climb.campaign.unwrap().history, CampaignHistory::Unreconstructible { .. }));
}

#[test]
fn a_zero_statistical_cap_neither_blocks_nor_retreats_a_qualified_step() {
    let (journal, target) = Journal::promoted_compact();
    let mut events = journal.events.clone();
    for event in &mut events {
        if let Payload::CcdCap(cap) = &mut event.payload { cap.new = togi_machine::TargetOffset::ZERO; }
    }
    let mut state = replay(&events);
    assert_eq!(state.project().applied, Some(togi_tuner::UnitProfile::of_ccd(&target)));
    let mut query = cold_boot("s-zero", 60_000);
    query.now = journal.now();
    let commitments = drain(&mut state, &query, events.last().unwrap().seq.get() + 1);
    assert!(!commitments.iter().any(|payload| matches!(payload, Payload::ClimbDecision(record) if matches!(record.decision, ClimbAction::Retreat { .. }))));
    assert_eq!(state.project().applied, Some(togi_tuner::UnitProfile::of_ccd(&target)));
}

#[test]
fn a_stock_receipt_for_a_nonzero_proposal_never_invents_an_applied_step() {
    let (journal, target) = Journal::promoted_compact();
    let application = journal.events.iter().rposition(|event| matches!(&event.payload, Payload::DayApplied(applied) if applied.target.ccd() == &target)).unwrap();
    let mut state = replay(&journal.events[..application]);
    let mut receipt = journal.events[application].clone();
    let Payload::DayApplied(applied) = &mut receipt.payload else { unreachable!(); };
    applied.target = togi_journal::CcdVector::expand(zero(), &crate::fixture::cores()).unwrap();
    applied.readbacks = offsets_of(&zero());
    state.fold(&Event::decode(&receipt.encode().unwrap()).unwrap()).unwrap();
    assert_eq!(state.project().applied, Some(togi_tuner::UnitProfile::of_ccd(&zero())));
    assert!(state.project().climb.units.iter().all(|unit| unit.latest_step.is_none()));
    assert_eq!(state.project().climb.campaign.unwrap().consumed, 0);
}

fn imported(event: &Event) -> togi_journal::DayImported {
    let wire: serde_json::Value = serde_json::from_str(&event.encode().unwrap()).unwrap();
    togi_journal::DayImported {
        original: togi_journal::Provenance { session: crate::fixture::SESSION.parse().unwrap(), seq: event.seq },
        original_kind: event.kind(), raw: serde_json::to_string(&wire["data"]).unwrap(),
        time: event.time, boot: event.boot.clone(), build: None, firmware: None, profile: None,
        class: togi_journal::ImportClass::Manual, missing: Vec::new(),
    }
}

#[test]
fn the_successor_import_deduplicates_after_a_crash() {
    let source = retreated();
    let start = source.events.iter().find_map(|event| if let Payload::CampaignStart(start) = &event.payload { Some(start.clone()) } else { None }).unwrap();
    let mut successor = Journal::ready();
    successor.start_session("20231120T221320Z");
    for event in &source.events { successor.append(imported(event).into(), &[]); }
    successor.state = snapshot(&successor.state);
    for event in &source.events { successor.append(imported(event).into(), &[]); }
    let mut query = cold_boot("successor", 60_000);
    query.now = successor.now();
    let waiting = successor.state.day(&query).unwrap();
    assert!(!waiting.iter().any(|decision| matches!(decision.payload, togi_tuner::PolicyPayload::CampaignStart(_))));
    let mut start = start;
    start.origin = togi_journal::CampaignOrigin::Successor { sources: Vec::new() };
    successor.append(start.into(), &[]);
    let committed = successor.settle_day(&query);
    let projection = successor.state.project();
    assert_eq!(projection.applied, Some(togi_tuner::UnitProfile::of_ccd(&zero())));
    assert_eq!(projection.climb.campaign.as_ref().unwrap().consumed, 1);
    assert!(matches!(projection.climb.campaign.as_ref().unwrap().history, CampaignHistory::Complete {}), "{:?}", projection.climb.campaign.as_ref().unwrap().history);
    assert!(projection.climb.units.iter().any(|unit| matches!(unit.state, togi_tuner::ClimbUnitState::Finished { .. })));
    assert!(!committed.iter().any(|payload| matches!(payload, Payload::ClimbDecision(decision) if matches!(decision.decision, ClimbAction::Promote { .. }))));
    assert_eq!(replay(&successor.events).project(), projection);
}
