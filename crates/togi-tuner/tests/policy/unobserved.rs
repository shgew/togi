//! A manual record of a crash no observer saw, through the journal: it is a CPU-suspect event and an active
//! whole-profile veto under the record's own identity, it steps D back and gets its one reproduction night, and
//! it stops blocking promotion once both are done while its veto stands.

use togi_journal::{
    Classification, CrashedBoot, DayEventId, DayRecord, HoldReason, OffsetSource, Payload, Queue,
    RecordedOffsets, ReproductionState, RetreatCause, Running, StepKind, VetoState,
};
use togi_machine::CcdProfile;

use crate::fixture::{Ending, Journal, SESSION, at, offsets_of};
use crate::flow::{apply, cold_boot, promoted, steps};

/// Records, in the cold boot `boot`, a crash at D that no day observer saw; returns the record's identity.
fn unobserved_crash_at(journal: &mut Journal, boot: &str, d: &CcdProfile) -> DayEventId {
    journal.boot_start(boot);
    let seq = journal.append(
        DayRecord {
            boot: CrashedBoot::Unobserved { at: at(50_000) },
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
        .find(|step| matches!(step.step, StepKind::Retreat { .. }))
        .unwrap_or_else(|| panic!("no retreat in {committed:?}"));
    let StepKind::Retreat {
        cause: RetreatCause::Event { event: cited, .. },
        ..
    } = &retreat.step
    else {
        panic!("the retreat is not caused by the event: {retreat:?}");
    };
    assert_eq!(*cited, id);
    assert!(retreat.proposed.ccd.is_zero());
}

#[test]
fn an_unobserved_crash_record_gets_one_reproduction_night_that_closes_under_its_identity() {
    let (mut journal, d) = promoted();
    let id = unobserved_crash_at(&mut journal, "day2", &d);
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let retreat = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::Retreat { .. }))
        .cloned()
        .unwrap();
    apply(&mut journal, &retreat, 2, "day2");
    journal.reboot("tuning2");
    let plan = journal.until_trial(at(62_000));
    assert_eq!(plan.task.queue, Queue::Reproduction);
    assert_eq!(plan.profile, d);
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
        .find(|step| matches!(step.step, StepKind::Retreat { .. }))
        .cloned()
        .unwrap();
    apply(&mut journal, &retreat, 2, "day2");
    // The retreat is applied but the reproduction is not closed: the event still blocks promotion.
    journal.boot_start("day2b");
    let waiting = journal.settle_day(&cold_boot("day2b", 61_500));
    let blocked = hold_reasons(&waiting);
    assert!(
        blocked.contains(&HoldReason::EventUnresolved { event: id.clone() }),
        "{blocked:?}"
    );
    // The night reproduces it and the next cold boot closes the task: only the veto still blocks.
    journal.reboot("tuning2");
    let plan = journal.until_trial(at(62_000));
    journal.run(&plan, Ending::Pass);
    journal.boot_start("day3");
    let done = journal.settle_day(&cold_boot("day3", 63_000));
    let blocked = hold_reasons(&done);
    assert!(
        !blocked.contains(&HoldReason::EventUnresolved { event: id.clone() }),
        "{blocked:?}"
    );
    assert!(
        blocked.contains(&HoldReason::Vetoed { veto: id }),
        "{blocked:?}"
    );
}

/// What the hold among `payloads` is blocked by.
fn hold_reasons(payloads: &[Payload]) -> Vec<HoldReason> {
    steps(payloads)
        .into_iter()
        .find_map(|step| {
            if let StepKind::Hold { blocked_by } = &step.step {
                Some(blocked_by.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no hold in {payloads:?}"))
}
