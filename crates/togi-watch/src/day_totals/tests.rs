use jiff::Timestamp;
use test_case::test_case;
use togi_journal::{
    CrashedBoot, DayAnswer, DayAnswerKind, DayBootStart, DayCheckpoint, DayImported, DayInterval,
    DayRecord, DayUncleanEnd, Event, EventKind, ImportClass, IntervalState, ObservedProfile,
    OffsetSource, Payload, Provenance, RecordedOffsets, Running, SessionId, Stamp,
};
use togi_machine::{
    BiosContext, BootId, EndEvidence, ObservedOffset, PerCore, ResetKind, ResetReason,
};

use super::{DayTotals, Tally, unknown_intervals};
use crate::tests::kit::{build, core, seq};

fn offsets(offset: i64) -> PerCore<ObservedOffset> {
    [(core(0), ObservedOffset(offset))].into_iter().collect()
}

fn boot(name: &str) -> BootId {
    name.to_owned().into()
}

fn event(index: u64, boot_name: &str, payload: impl Into<Payload>) -> Event {
    Event::new(
        seq(index),
        Stamp {
            time: Timestamp::UNIX_EPOCH,
            mono: None,
        },
        boot(boot_name),
        Vec::new(),
        payload.into(),
    )
}

fn start(offset: i64) -> DayBootStart {
    DayBootStart {
        build: build(),
        bios: BiosContext::default(),
        offsets: offsets(offset),
        watchdog: None,
    }
}

fn checkpoint(applied_secs: u64) -> DayCheckpoint {
    DayCheckpoint {
        applied_secs,
        load_mix: togi_journal::LoadMix::default(),
        watchdog: None,
    }
}

fn unclean(name: &str, offset: i64) -> DayUncleanEnd {
    DayUncleanEnd {
        boot: boot(name),
        last_checkpoint: None,
        offsets: offsets(offset),
        reset_reason: ResetReason {
            kind: Some(ResetKind::Watchdog),
            raw: Vec::new(),
            supported: true,
        },
        evidence: EndEvidence::unknown(),
    }
}

fn answer(name: &str, offset: i64, kind: DayAnswerKind) -> DayAnswer {
    DayAnswer {
        boot: boot(name),
        offsets: offsets(offset),
        answer: kind,
        uid: 1000,
    }
}

fn interval(state: IntervalState) -> DayInterval {
    DayInterval {
        boot: boot("day-a"),
        start_ns: 0,
        end_ns: 1_000_000_000,
        profile: ObservedProfile::PerCore { cores: offsets(0) },
        application: None,
        state,
        checkpoints: Vec::new(),
        reason: "test".to_owned(),
    }
}

fn imported(
    index: u64,
    kind: EventKind,
    class: ImportClass,
    raw: String,
    boot_name: &str,
) -> Event {
    event(
        index,
        "day-now",
        DayImported {
            original: Provenance {
                session: SessionId::new(Timestamp::UNIX_EPOCH, None).unwrap(),
                seq: seq(index + 100),
            },
            original_kind: kind,
            raw,
            time: Timestamp::UNIX_EPOCH,
            boot: boot(boot_name),
            build: None,
            firmware: None,
            profile: None,
            class,
            missing: Vec::new(),
        },
    )
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

/// One hour at CO 0 (clean), then half an hour at -10 that ended without a clean stop.
fn mixed() -> Vec<Event> {
    vec![
        event(1, "day-a", start(0)),
        event(2, "day-a", checkpoint(3600)),
        event(3, "day-b", start(-10)),
        event(4, "day-b", checkpoint(1800)),
        event(5, "day-c", unclean("day-b", -10)),
    ]
}

fn described(events: &[Event]) -> (String, String, u32) {
    let totals = DayTotals::of(events);
    (
        totals.all().describe(),
        totals.stock().describe(),
        totals.unknown_intervals(),
    )
}

#[test]
fn no_day_boots_leave_the_totals_empty() {
    let totals = DayTotals::of(&[]);
    assert!(totals.is_empty());
    assert_eq!(totals.all(), Tally::default());
    assert_eq!(totals.all().describe(), "0.0h observed, 0 crashes");
}

#[test]
fn stock_only_hours_are_both_totals() {
    let events = [
        event(1, "day-a", start(0)),
        event(2, "day-a", checkpoint(7200)),
    ];
    let (all, stock, unknown) = described(&events);
    assert_eq!(all, "2.0h observed, 0 crashes");
    assert_eq!(stock, all);
    assert_eq!(unknown, 0);
}

#[test]
fn a_crash_at_a_changed_profile_is_in_the_total_and_not_in_the_stock_total() {
    let (all, stock, _) = described(&mixed());
    assert_eq!(all, "1.5h observed, 1 crash");
    assert_eq!(stock, "1.0h observed, 0 crashes");
}

#[test_case(DayAnswerKind::Frozen, 1; "frozen is a crash")]
#[test_case(DayAnswerKind::Graphics, 1; "graphics is a crash")]
#[test_case(DayAnswerKind::Unsure, 1; "unsure stays a crash")]
#[test_case(DayAnswerKind::PowerOff, 0; "power off is not a crash")]
fn an_answer_decides_whether_the_unclean_end_counts(kind: DayAnswerKind, crashes: u32) {
    let mut events = mixed();
    events.push(event(6, "day-c", answer("day-b", -10, kind)));
    assert_eq!(DayTotals::of(&events).all().crashes, crashes);
}

#[test]
fn the_last_answer_wins() {
    let mut events = mixed();
    events.push(event(
        6,
        "day-c",
        answer("day-b", -10, DayAnswerKind::PowerOff),
    ));
    events.push(event(
        7,
        "day-c",
        answer("day-b", -10, DayAnswerKind::Frozen),
    ));
    assert_eq!(DayTotals::of(&events).all().crashes, 1);
}

#[test]
fn a_crash_at_stock_counts_in_both_totals() {
    let mut events = mixed();
    events.push(event(6, "day-d", start(0)));
    events.push(event(7, "day-e", unclean("day-d", 0)));
    let totals = DayTotals::of(&events);
    assert_eq!((totals.all().crashes, totals.stock().crashes), (2, 1));
}

#[test]
fn a_manual_record_counts_once_and_beats_a_power_off_answer() {
    let record = |crashed: CrashedBoot| DayRecord {
        boot: crashed,
        offsets: RecordedOffsets::new(offsets(-10)).unwrap(),
        source: OffsetSource::Entered {
            at: Timestamp::UNIX_EPOCH,
        },
        running: Running::Idle,
        note: None,
    };
    let mut events = mixed();
    events.push(event(
        6,
        "day-c",
        answer("day-b", -10, DayAnswerKind::PowerOff),
    ));
    assert_eq!(DayTotals::of(&events).all().crashes, 0);
    events.push(event(
        7,
        "day-c",
        record(CrashedBoot::UncleanEnd {
            boot: boot("day-b"),
        }),
    ));
    assert_eq!(DayTotals::of(&events).all().crashes, 1);
    events.push(event(
        8,
        "day-c",
        record(CrashedBoot::Unobserved {
            at: Timestamp::UNIX_EPOCH,
        }),
    ));
    assert_eq!(DayTotals::of(&events).all().crashes, 2);
}

#[test_case(IntervalState::Unobserved, 1; "unobserved")]
#[test_case(IntervalState::Ambiguous, 1; "ambiguous")]
#[test_case(IntervalState::Applied, 0; "applied")]
#[test_case(IntervalState::Paused, 0; "paused")]
#[test_case(IntervalState::Suspended, 0; "suspended")]
fn only_unobserved_and_ambiguous_intervals_are_unknown(state: IntervalState, unknown: u32) {
    let mut events = mixed();
    events.push(event(6, "day-a", interval(state)));
    assert_eq!(DayTotals::of(&events).unknown_intervals(), unknown);
}

#[test]
fn imported_records_are_a_separate_provenance_and_deduplicate_by_original_identity() {
    let mut events = mixed();
    events.push(imported(
        6,
        EventKind::DayBootStart,
        ImportClass::Boot,
        json(&start(-5)),
        "old",
    ));
    events.push(imported(
        7,
        EventKind::DayCheckpoint,
        ImportClass::Checkpoint,
        json(&checkpoint(7200)),
        "old",
    ));
    events.push(imported(
        8,
        EventKind::DayUncleanEnd,
        ImportClass::End,
        json(&unclean("old", -5)),
        "old-next",
    ));
    let again = events[events.len() - 1].clone();
    events.push(again);
    let totals = DayTotals::of(&events);
    assert_eq!(
        totals.all().describe(),
        "3.5h observed (2.0h imported), 2 crashes (1 imported)"
    );
    assert_eq!(totals.stock().describe(), "1.0h observed, 0 crashes");
}

#[test]
fn an_imported_answer_applies_to_a_live_end() {
    let mut events = mixed();
    events.push(imported(
        6,
        EventKind::DayAnswer,
        ImportClass::Answer,
        json(&answer("day-b", -10, DayAnswerKind::PowerOff)),
        "day-b",
    ));
    assert_eq!(DayTotals::of(&events).all().crashes, 0);
}

#[test]
fn an_import_that_does_not_decode_adds_nothing() {
    let events = [imported(
        1,
        EventKind::DayCheckpoint,
        ImportClass::Checkpoint,
        "{".to_owned(),
        "old",
    )];
    assert!(DayTotals::of(&events).is_empty());
}

#[test_case(0, "0 unknown intervals")]
#[test_case(1, "1 unknown interval")]
#[test_case(2, "2 unknown intervals")]
fn unknown_intervals_pluralize(count: u32, text: &str) {
    assert_eq!(unknown_intervals(count), text);
}

#[test]
fn live_and_imported_intervals_share_boot_state_but_keep_their_own_provenance() {
    let events = [
        event(1, "day-a", start(0)),
        event(2, "day-a", checkpoint(3600)),
        imported(
            3,
            EventKind::DayOffsetsChanged,
            ImportClass::ProfileChange,
            json(&togi_journal::DayOffsetsChanged {
                offsets: offsets(-20),
            }),
            "day-a",
        ),
        imported(
            4,
            EventKind::DayCheckpoint,
            ImportClass::Checkpoint,
            json(&checkpoint(7200)),
            "day-a",
        ),
        event(5, "day-a", checkpoint(9000)),
        imported(
            6,
            EventKind::DayCheckpoint,
            ImportClass::Checkpoint,
            json(&checkpoint(10800)),
            "day-a",
        ),
    ];
    let totals = DayTotals::of(&events);
    assert_eq!(
        totals.all(),
        Tally {
            secs: 7200,
            imported_secs: 1800,
            ..Tally::default()
        }
    );
    assert_eq!(
        totals.stock(),
        Tally {
            secs: 3600,
            ..Tally::default()
        }
    );
}

#[test]
fn serialized_day_totals_keep_the_boot_shape_and_resume_pending_intervals() {
    let totals = DayTotals::of(&[
        event(1, "day-a", start(0)),
        event(2, "day-a", checkpoint(3600)),
        event(
            3,
            "day-a",
            togi_journal::DayOffsetsChanged {
                offsets: offsets(-20),
            },
        ),
    ]);
    let encoded = serde_json::to_value(&totals).unwrap();
    assert_eq!(
        encoded["boots"],
        serde_json::json!([{
            "boot": "day-a",
            "offsets": {"0": -20},
            "seconds": 3600,
            "changed": true,
        }])
    );
    let mut restored: DayTotals = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored, totals);
    restored.apply(&event(4, "day-a", checkpoint(7200)));
    restored.apply(&event(5, "day-a", checkpoint(9000)));
    assert_eq!(restored.all().secs, 5400);
    assert_eq!(restored.stock().secs, 3600);
}
