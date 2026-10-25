use test_case::test_case;
use togi_journal::{
    DayBootStart, DayCheckpoint, DayOffsetsChanged, DayUncleanEnd, SessionId, SessionStart,
};
use togi_machine::{BiosContext, CpuId, EndEvidence, ResetReason};

use super::*;
use crate::session::Control;
use crate::testing::{Bench, build};

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

fn core(index: i64) -> CoreId {
    CoreId::new(index).unwrap()
}

fn ccd(index: i64) -> CcdId {
    CcdId::new(index).unwrap()
}

fn boot(name: &str) -> BootId {
    name.to_owned().into()
}

fn profile(values: [i64; 4]) -> PerCore<ObservedOffset> {
    (0_i64..)
        .zip(values)
        .map(|(index, value)| (core(index), ObservedOffset(value)))
        .collect()
}

fn event(seq: u64, boot_name: &str, second: i64, payload: impl Into<Payload>) -> Event {
    let payload = payload.into();
    Event {
        seq: Seq::new(seq).unwrap(),
        time: at(second),
        boot: boot(boot_name),
        mono: None,
        cause: Vec::new(),
        msg: payload.message(),
        payload,
    }
}

/// Four cores: 0 and 1 on CCD 0, 2 and 3 on CCD 1.
fn session() -> Event {
    let cores = (0..4)
        .map(|index| CoreInfo {
            core: core(index),
            ccd: ccd(index / 2),
            cpus: vec![CpuId::from(u16::try_from(index).unwrap())],
        })
        .collect();
    event(
        1,
        "tuning",
        0,
        SessionStart {
            build: build(),
            session: SessionId::new(at(0), None).unwrap(),
            cores,
        },
    )
}

/// Boot `crashed` observed at seconds 100 and 700, with an offset change at 400 and a checkpoint at 700, then
/// the unclean end that boot `next` found at 1000.
fn history(offsets: PerCore<ObservedOffset>) -> Vec<Event> {
    vec![
        session(),
        event(
            2,
            "crashed",
            100,
            DayBootStart {
                build: build(),
                bios: BiosContext::default(),
                offsets: profile([0, 0, 0, 0]),
                watchdog: None,
            },
        ),
        event(
            3,
            "crashed",
            400,
            DayOffsetsChanged {
                offsets: profile([-5, -5, -5, -5]),
            },
        ),
        event(
            4,
            "crashed",
            700,
            DayCheckpoint {
                applied_secs: 600,
                load_mix: togi_journal::LoadMix::default(),
                watchdog: None,
            },
        ),
        event(
            5,
            "next",
            1000,
            DayUncleanEnd {
                boot: boot("crashed"),
                last_checkpoint: Seq::new(4),
                offsets,
                reset_reason: ResetReason {
                    kind: None,
                    raw: Vec::new(),
                    supported: false,
                },
                evidence: EndEvidence::unknown(),
            },
        ),
    ]
}

fn draft(crash: Crash) -> Draft {
    Draft {
        crash,
        running: Running::Build,
        note: Some("a Nix build".to_owned()),
    }
}

fn entered(specs: &[OffsetSpec]) -> Entered {
    Entered {
        at: at(500),
        specs: specs.to_vec(),
    }
}

fn observed(entered: Option<Entered>) -> Crash {
    Crash::Observed {
        boot: boot("crashed"),
        entered,
    }
}

fn values(record: &DayRecord) -> Vec<i64> {
    record
        .offsets
        .get()
        .iter()
        .map(|(_, offset)| offset.0)
        .collect()
}

#[test]
fn an_observed_boot_without_entered_offsets_carries_the_readback_and_its_time() {
    let events = history(profile([-5, -5, -10, -10]));
    let record = draft_record(&events, &draft(observed(None))).unwrap();
    assert_eq!(
        record.boot,
        CrashedBoot::UncleanEnd {
            boot: boot("crashed")
        }
    );
    assert_eq!(values(&record), [-5, -5, -10, -10]);
    // The observer last read the offsets at its checkpoint, after the change at 400.
    assert_eq!(record.source, OffsetSource::ReadBack { at: at(700) });
    assert_eq!(record.running, Running::Build);
    assert_eq!(record.note.as_deref(), Some("a Nix build"));
}

#[test]
fn offsets_entered_for_an_observed_boot_carry_the_time_they_were_entered() {
    let events = history(profile([-5, -5, -10, -10]));
    let record = draft_record(
        &events,
        &draft(observed(Some(entered(&[OffsetSpec::All(-20)])))),
    )
    .unwrap();
    assert_eq!(
        record.boot,
        CrashedBoot::UncleanEnd {
            boot: boot("crashed")
        }
    );
    assert_eq!(values(&record), [-20, -20, -20, -20]);
    assert_eq!(record.source, OffsetSource::Entered { at: at(500) });
}

#[test]
fn a_crash_without_an_unclean_end_carries_when_it_happened_and_entered_offsets() {
    let events = vec![session()];
    let record = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[OffsetSpec::Ccd(ccd(0), -25), OffsetSpec::Ccd(ccd(1), -30)]),
        }),
    )
    .unwrap();
    assert_eq!(record.boot, CrashedBoot::Unobserved { at: at(300) });
    assert_eq!(values(&record), [-25, -25, -30, -30]);
    assert_eq!(record.source, OffsetSource::Entered { at: at(500) });
}

#[test]
fn a_more_specific_offset_overrides_a_less_specific_one() {
    let events = vec![session()];
    let record = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[
                OffsetSpec::Core(core(3), -12),
                OffsetSpec::Ccd(ccd(1), -30),
                OffsetSpec::All(0),
            ]),
        }),
    )
    .unwrap();
    assert_eq!(values(&record), [0, 0, -30, -12]);
}

#[test]
fn an_unknown_boot_is_refused() {
    let events = history(profile([-5; 4]));
    let error = draft_record(
        &events,
        &draft(Crash::Observed {
            boot: boot("elsewhere"),
            entered: None,
        }),
    )
    .unwrap_err();
    assert!(
        matches!(error, RecordError::UnknownBoot { boot: named } if named == boot("elsewhere"))
    );
}

#[test]
fn a_boot_that_already_has_a_record_is_refused() {
    let mut events = history(profile([-5; 4]));
    let first = draft_record(&events, &draft(observed(None))).unwrap();
    events.push(event(6, "next", 1100, first));
    let error = draft_record(&events, &draft(observed(None))).unwrap_err();
    assert!(matches!(
        error,
        RecordError::AlreadyRecorded { boot: named, seq } if named == boot("crashed") && seq == Seq::new(6).unwrap()
    ));
}

#[test]
fn a_second_record_of_the_same_unobserved_crash_is_refused() {
    let mut events = history(profile([-5; 4]));
    let unobserved = |second: i64| {
        draft(Crash::Unobserved {
            at: at(second),
            entered: entered(&[OffsetSpec::All(-20)]),
        })
    };
    let first = draft_record(&events, &unobserved(300)).unwrap();
    events.push(event(6, "next", 1100, first));
    let error = draft_record(&events, &unobserved(300)).unwrap_err();
    assert!(matches!(
        error,
        RecordError::AlreadyRecordedAt { at: crash, seq } if crash == at(300) && seq == Seq::new(6).unwrap()
    ));
    assert!(draft_record(&events, &unobserved(301)).is_ok());
}

#[test]
fn a_record_of_an_unobserved_crash_does_not_mark_any_boot_recorded() {
    let mut events = history(profile([-5; 4]));
    let unobserved = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[OffsetSpec::All(-20)]),
        }),
    )
    .unwrap();
    events.push(event(6, "next", 1100, unobserved));
    assert!(draft_record(&events, &draft(observed(None))).is_ok());
}

#[test]
fn the_newest_unrecorded_end_is_the_default_boot() {
    let mut events = history(profile([-5; 4]));
    assert_eq!(
        DayCrashes::of(&events)
            .newest_unrecorded()
            .map(|named| named.end.boot.clone()),
        Some(boot("crashed"))
    );
    let record = draft_record(&events, &draft(observed(None))).unwrap();
    events.push(event(6, "next", 1100, record));
    assert_eq!(DayCrashes::of(&events).newest_unrecorded(), None);
}

#[test]
fn a_journal_without_a_session_cannot_record() {
    let error = draft_record(
        &[],
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[OffsetSpec::All(-20)]),
        }),
    )
    .unwrap_err();
    assert!(matches!(error, RecordError::NoSession));
}

#[test]
fn offsets_that_name_no_core_are_refused() {
    let events = vec![session()];
    let error = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[]),
        }),
    )
    .unwrap_err();
    assert!(matches!(error, RecordError::Offsets(OffsetsError::Empty)));
}

#[test]
fn an_observed_boot_whose_readback_names_no_core_is_refused() {
    let events = history(PerCore::default());
    let error = draft_record(&events, &draft(observed(None))).unwrap_err();
    assert!(matches!(error, RecordError::Offsets(OffsetsError::Empty)));
}

#[test_case(&[OffsetSpec::All(-51)]; "below the range")]
#[test_case(&[OffsetSpec::All(1)]; "above the range")]
#[test_case(&[OffsetSpec::All(0), OffsetSpec::Core(core(0), -60)]; "one core out of range")]
fn an_offset_outside_what_togi_applies_is_refused(specs: &[OffsetSpec]) {
    let events = vec![session()];
    let error = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(specs),
        }),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RecordError::Offsets(OffsetsError::OutOfRange { .. })
    ));
}

#[test]
fn entered_offsets_must_cover_every_core_of_the_session() {
    let events = vec![session()];
    let error = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[OffsetSpec::Ccd(ccd(0), -20)]),
        }),
    )
    .unwrap_err();
    assert!(matches!(error, RecordError::Missing { core: lacking } if lacking == core(2)));
}

#[test_case(&[OffsetSpec::All(-1), OffsetSpec::All(-2)]; "all twice")]
#[test_case(&[OffsetSpec::All(0), OffsetSpec::Ccd(ccd(0), -1), OffsetSpec::Ccd(ccd(0), -2)]; "a ccd twice")]
#[test_case(&[OffsetSpec::All(0), OffsetSpec::Core(core(0), -1), OffsetSpec::Core(core(0), -2)]; "a core twice")]
fn a_core_given_twice_by_the_same_kind_is_refused(specs: &[OffsetSpec]) {
    let events = vec![session()];
    let error = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(specs),
        }),
    )
    .unwrap_err();
    assert!(matches!(error, RecordError::Duplicate { .. }));
}

#[test]
fn a_core_or_ccd_the_session_lacks_is_refused() {
    let events = vec![session()];
    let core_error = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[OffsetSpec::All(0), OffsetSpec::Core(core(9), -1)]),
        }),
    )
    .unwrap_err();
    assert!(matches!(core_error, RecordError::NoSuchCore { core: lacking } if lacking == core(9)));
    let ccd_error = draft_record(
        &events,
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[OffsetSpec::All(0), OffsetSpec::Ccd(ccd(5), -1)]),
        }),
    )
    .unwrap_err();
    assert!(matches!(ccd_error, RecordError::NoSuchCcd { ccd: lacking } if lacking == ccd(5)));
}

#[test]
fn appending_a_record_adds_one_day_record_event() {
    let bench = Bench::new(2);
    let mut running = bench.open();
    assert_eq!(running.start().unwrap(), Control::Continue);
    let mut journal = running.appender.into_journal();
    let record = draft_record(
        &[session()],
        &draft(Crash::Unobserved {
            at: at(300),
            entered: entered(&[OffsetSpec::All(-20)]),
        }),
    )
    .unwrap();
    let before = journal.events().len();
    let seq = append_record(&mut journal, &*bench.sim.seams().clock, record.clone()).unwrap();
    assert_eq!(journal.events().len(), before + 1);
    let event = journal.event(seq).unwrap();
    assert_eq!(event.payload, Payload::DayRecord(record));
    assert!(event.cause.is_empty());
}
