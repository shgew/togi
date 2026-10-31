//! A manual record of a crash no observer saw, through the journal: it is a CPU-suspect event and an active
//! whole-profile veto under the record's own identity, it steps D back and gets its one reproduction night, and
//! it stops blocking promotion once both are done while its veto stands.

use togi_journal::{
    Classification, ClimbAction, ClimbBlocker, ClimbRetreatCause, CrashedBoot, DayEventId,
    DayImported, DayRecord, EventKind, ImportClass, OffsetSource, Payload, Provenance, Queue,
    RecordedOffsets, ReproductionState, Running, Seq, VetoState,
};
use togi_machine::CcdProfile;

use crate::fixture::{Ending, Journal, SESSION, at, offsets_of};
use crate::flow::{apply, cold_boot, promoted, steps};

/// Records, in the cold boot `boot`, a crash at D that no day observer saw; returns the record's identity.
fn unobserved_crash_at(journal: &mut Journal, boot: &str, d: &CcdProfile) -> DayEventId {
    journal.boot_start(boot);
    let crashed = journal.now();
    let seq = journal.append(
        DayRecord {
            boot: CrashedBoot::Unobserved { at: crashed },
            offsets: RecordedOffsets::new(offsets_of(d)).unwrap(),
            source: OffsetSource::Entered { at: at(50_100) },
            running: Running::Build,
            note: None,
        }
        .into(),
        &[],
    );
    DayEventId::Record {
        session: SESSION.parse().unwrap(),
        seq,
    }
}

#[test]
fn an_unobserved_crash_record_commits_a_cpu_suspect_event_an_active_veto_and_a_retreat() {
    let (mut journal, d) = promoted();
    let id = unobserved_crash_at(&mut journal, "day2", &d);
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let event = committed
        .iter()
        .find_map(|payload| {
            if let Payload::DayEvent(event) = payload
                && event.event == id
            {
                Some(event)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no event for {id:?} in {committed:?}"));
    assert!(
        matches!(event.classification, Classification::CpuSuspect { .. }),
        "{event:?}"
    );
    let veto = committed
        .iter()
        .find_map(|payload| {
            if let Payload::DayVeto(veto) = payload
                && veto.event == id
            {
                Some(veto)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no veto for {id:?} in {committed:?}"));
    assert_eq!(veto.state, VetoState::Active);
    let retreat = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.decision, ClimbAction::Retreat { .. }))
        .unwrap_or_else(|| panic!("no retreat in {committed:?}"));
    let ClimbAction::Retreat {
        cause: ClimbRetreatCause::Event { event: cited, .. },
        ..
    } = &retreat.decision
    else {
        panic!("the retreat is not caused by the event: {retreat:?}");
    };
    assert_eq!(*cited, id);
    assert!(crate::flow::profile(&retreat.proposed).is_zero());
}

#[test]
fn an_imported_unobserved_crash_keeps_its_original_record_identity_for_event_and_veto() {
    let (mut journal, d) = promoted();
    journal.boot_start("day2");
    let crashed = journal.now();
    let original = Provenance {
        session: "20231113T221320Z".parse().unwrap(),
        seq: Seq::new(7).unwrap(),
    };
    let record = DayRecord {
        boot: CrashedBoot::Unobserved { at: crashed },
        offsets: RecordedOffsets::new(offsets_of(&d)).unwrap(),
        source: OffsetSource::Entered { at: at(50_100) },
        running: Running::Build,
        note: None,
    };
    let imported_seq = journal.append(
        DayImported {
            original,
            original_kind: EventKind::DayRecord,
            raw: serde_json::to_string(&record).unwrap(),
            time: at(50_100),
            boot: "original-day".to_owned().into(),
            build: None,
            firmware: None,
            profile: None,
            class: ImportClass::Manual,
            missing: vec![],
        }
        .into(),
        &[],
    );
    assert_ne!(imported_seq, original.seq);
    assert_ne!(original.session, SESSION.parse().unwrap());
    let id = DayEventId::Record {
        session: original.session,
        seq: original.seq,
    };
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let events = committed
        .iter()
        .filter_map(|payload| {
            if let Payload::DayEvent(event) = payload {
                Some((
                    &event.event,
                    matches!(event.classification, Classification::CpuSuspect { .. }),
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(events, vec![(&id, true)]);
    let vetoes = committed
        .iter()
        .filter_map(|payload| {
            if let Payload::DayVeto(veto) = payload {
                Some((&veto.event, veto.state))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(vetoes, vec![(&id, VetoState::Active)]);
}

#[test]
fn an_unobserved_crash_record_gets_one_reproduction_night_that_closes_under_its_identity() {
    let (mut journal, d) = promoted();
    let id = unobserved_crash_at(&mut journal, "day2", &d);
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let retreat = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.decision, ClimbAction::Retreat { .. }))
        .cloned()
        .unwrap();
    apply(&mut journal, &retreat, 2, "day2");
    journal.reboot("tuning2");
    let plan = journal.until_trial(at(62_000));
    assert_eq!(plan.task.queue, Queue::Reproduction);
    assert_eq!(plan.profile.to_ccd().unwrap(), d);
    let night = plan.night;
    journal.run(&plan, Ending::Pass);
    journal.boot_start("day3");
    let closure = journal.settle_day(&cold_boot("day3", 63_000));
    let reproduction = closure
        .iter()
        .find_map(|payload| {
            if let Payload::NightReproduction(record) = payload {
                Some(record)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no reproduction closure in {closure:?}"));
    assert_eq!(reproduction.source, id);
    assert_eq!(reproduction.night, night);
    assert_eq!(reproduction.state, ReproductionState::Unreproduced);
    journal.reboot("tuning3");
    for step in 0..30_i64 {
        let plan = journal.until_trial(at(64_000 + step));
        assert_ne!(plan.task.queue, Queue::Reproduction, "{plan:?}");
        journal.run(&plan, Ending::Pass);
    }
}

#[test]
fn an_unobserved_crash_record_blocks_promotion_until_its_retreat_and_reproduction_are_done() {
    let (mut journal, d) = promoted();
    let id = unobserved_crash_at(&mut journal, "day2", &d);
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let retreat = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.decision, ClimbAction::Retreat { .. }))
        .cloned()
        .unwrap();
    apply(&mut journal, &retreat, 2, "day2");
    // The retreat is applied but the reproduction is not closed: the event still blocks promotion.
    journal.boot_start("day2b");
    let waiting = journal.settle_day(&cold_boot("day2b", 61_500));
    let blocked = hold_reasons(&waiting);
    assert!(
        blocked.contains(&ClimbBlocker::EventUnresolved { event: id.clone() }),
        "{blocked:?}"
    );
    // Closing reproduction resolves the work, never the whole-profile veto or the unit's stop.
    journal.reboot("tuning2");
    let plan = journal.until_trial(at(62_000));
    journal.run(&plan, Ending::Pass);
    journal.boot_start("day3");
    let done = journal.settle_day(&cold_boot("day3", 63_000));
    let blocked = hold_reasons(&done);
    assert!(
        !blocked.contains(&ClimbBlocker::EventUnresolved { event: id.clone() }),
        "{blocked:?}"
    );
    assert!(journal.events.iter().any(|event| matches!(&event.payload,
        Payload::DayVeto(veto) if veto.event == id && veto.state == VetoState::Active)));
    assert!(journal.events.iter().any(|event| matches!(&event.payload,
        Payload::UnitStop(stop) if matches!(&stop.reason,
            togi_journal::UnitStopReason::FirstBackstep { event, .. } if event == &id))));
    assert!(
        steps(&done)
            .iter()
            .all(|step| matches!(step.decision, ClimbAction::Hold { .. }))
    );
}

/// What the hold among `payloads` is blocked by.
fn hold_reasons(payloads: &[Payload]) -> Vec<ClimbBlocker> {
    steps(payloads)
        .into_iter()
        .find_map(|step| {
            if let ClimbAction::Hold { blocked_by } = &step.decision {
                Some(blocked_by.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no hold in {payloads:?}"))
}
