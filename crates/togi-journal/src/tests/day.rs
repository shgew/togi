use test_case::test_case;
use togi_machine::{
    BootId, EndEvidence, GraphicsDriver, KernelFault, KernelTail, MceCount, ObservedOffset,
    PerCore, ResetKind, ResetReason,
};

use super::fixtures::{at, core};
use crate::*;

const A: &str = "aaaaaaaa-0000-0000-0000-000000000000";
const B: &str = "bbbbbbbb-0000-0000-0000-000000000000";

fn id(boot: &str) -> BootId {
    BootId::from(boot.to_owned())
}

fn offsets(zero: i64) -> PerCore<ObservedOffset> {
    [
        (core(0), ObservedOffset(zero)),
        (core(1), ObservedOffset(-5)),
    ]
    .into_iter()
    .collect()
}

fn reset(kind: Option<ResetKind>) -> ResetReason {
    ResetReason {
        kind,
        raw: Vec::new(),
        supported: true,
    }
}

fn read(corrected: u32, uncorrected: u32) -> MceCount {
    MceCount::Read {
        corrected,
        uncorrected,
    }
}

fn faults(faults: &[KernelFault], logging_continued: bool) -> KernelTail {
    KernelTail::Faults {
        faults: faults.to_vec(),
        logging_continued,
    }
}

const RING: KernelFault = KernelFault::GraphicsTimeout {
    driver: GraphicsDriver::Amdgpu,
};

fn end_of(
    boot: &str,
    kind: Option<ResetKind>,
    crashed_mces: MceCount,
    successor_mces: MceCount,
    tail: KernelTail,
) -> DayUncleanEnd {
    DayUncleanEnd {
        boot: id(boot),
        last_checkpoint: None,
        offsets: offsets(-10),
        reset_reason: reset(kind),
        evidence: EndEvidence {
            crashed_mces,
            successor_mces,
            tail,
        },
    }
}

fn class(
    kind: Option<ResetKind>,
    crashed: MceCount,
    successor: MceCount,
    tail: KernelTail,
) -> EndClass {
    end_of(A, kind, crashed, successor, tail).class()
}

#[test_case(Some(ResetKind::Watchdog), read(0, 0), read(0, 0), KernelTail::Quiet {} => EndClass::Watchdog; "watchdog")]
#[test_case(Some(ResetKind::Watchdog), MceCount::Unknown {}, MceCount::Unknown {}, KernelTail::Missing {} => EndClass::Watchdog; "watchdog with nothing else readable")]
#[test_case(Some(ResetKind::Unknown), read(0, 0), read(0, 0), KernelTail::Quiet {} => EndClass::Unknown; "an unrecognized reset such as a CF9 write")]
#[test_case(None, read(0, 0), read(0, 0), KernelTail::Quiet {} => EndClass::Unknown; "no reset reason")]
#[test_case(None, MceCount::Unknown {}, MceCount::Unknown {}, KernelTail::Missing {} => EndClass::Unknown; "a missing tail is unknown, never CPU evidence")]
#[test_case(Some(ResetKind::Unknown), MceCount::Unknown {}, MceCount::Unknown {}, KernelTail::Unreadable {} => EndClass::Unknown; "an unreadable tail is unknown, never CPU evidence")]
#[test_case(Some(ResetKind::Unknown), read(0, 0), read(0, 0), faults(&[RING], true) => EndClass::GraphicsSurvived; "a graphics reset followed by more logging")]
#[test_case(Some(ResetKind::Unknown), read(0, 0), read(0, 0), faults(&[RING], false) => EndClass::GraphicsLast; "a graphics reset ending the log")]
#[test_case(Some(ResetKind::Watchdog), read(0, 0), read(0, 0), faults(&[KernelFault::NvidiaXid { code: 79 }], true) => EndClass::GraphicsSurvived; "an Xid followed by logging outranks the watchdog")]
#[test_case(None, read(0, 0), read(0, 0), faults(&[KernelFault::GraphicsReset { driver: GraphicsDriver::Xe }, KernelFault::Oops {}], true) => EndClass::KernelFault; "an oops after graphics is a kernel fault, not graphics")]
#[test_case(None, read(0, 0), read(0, 0), faults(&[KernelFault::HungTask {}, RING], false) => EndClass::GraphicsLast; "graphics after a hung task ends the log")]
#[test_case(None, read(0, 0), read(0, 0), faults(&[RING, KernelFault::HungTask {}], true) => EndClass::KernelFault; "a hung task after graphics is a kernel fault")]
#[test_case(Some(ResetKind::Watchdog), read(0, 0), read(0, 0), faults(&[KernelFault::HungTask {}], false) => EndClass::KernelFault; "a hung task")]
#[test_case(None, read(0, 0), read(0, 0), faults(&[KernelFault::Oops {}], true) => EndClass::KernelFault; "an oops")]
#[test_case(Some(ResetKind::Watchdog), read(0, 0), read(0, 1), KernelTail::Quiet {} => EndClass::CpuFault; "an uncorrected machine check in the successor")]
#[test_case(None, read(1, 2), MceCount::Unknown {}, KernelTail::Missing {} => EndClass::CpuFault; "an uncorrected machine check in the crashed boot")]
#[test_case(Some(ResetKind::Unknown), read(3, 0), read(1, 0), KernelTail::Quiet {} => EndClass::Unknown; "corrected machine checks are not a crash cause")]
#[test_case(Some(ResetKind::Unknown), read(0, 0), read(0, 0), faults(&[KernelFault::MachineCheck {}, RING], true) => EndClass::CpuFault; "a machine check in the tail outranks graphics")]
#[test_case(Some(ResetKind::SyncFlood), read(0, 0), read(0, 0), KernelTail::Quiet {} => EndClass::CpuFault; "a sync flood")]
#[test_case(Some(ResetKind::CpuShutdown), MceCount::Unknown {}, MceCount::Unknown {}, KernelTail::Missing {} => EndClass::CpuFault; "a cpu shutdown")]
#[test_case(Some(ResetKind::ThermalTrip), read(0, 1), read(0, 0), faults(&[RING], true) => EndClass::Thermal; "a thermal trip outranks everything")]
#[test_case(Some(ResetKind::PowerButton), read(0, 0), read(0, 0), KernelTail::Quiet {} => EndClass::PowerButton; "the power button")]
#[test_case(Some(ResetKind::PowerLoss), read(0, 0), read(0, 0), KernelTail::Quiet {} => EndClass::PowerLoss; "power loss")]
fn evidence_classification(
    kind: Option<ResetKind>,
    crashed: MceCount,
    successor: MceCount,
    tail: KernelTail,
) -> EndClass {
    class(kind, crashed, successor, tail)
}

fn unclean(seq: u64, boot: &str) -> Event {
    at(
        seq,
        "cccccccc-0000-0000-0000-000000000000",
        0,
        &[],
        end_of(
            boot,
            None,
            MceCount::Unknown {},
            MceCount::Unknown {},
            KernelTail::Missing {},
        )
        .into(),
    )
}

fn answer(seq: u64, boot: &str, answer: DayAnswerKind) -> Event {
    at(
        seq,
        "cccccccc-0000-0000-0000-000000000000",
        0,
        &[],
        DayAnswer {
            boot: id(boot),
            offsets: offsets(-10),
            answer,
            uid: 1000,
        }
        .into(),
    )
}

fn pending(questions: &DayQuestions) -> Vec<BootId> {
    questions.pending().map(|end| end.boot.clone()).collect()
}

#[test]
fn an_unclean_end_is_pending_until_answered() {
    let questions = DayQuestions::of(&[unclean(2, A), unclean(3, B)]);
    assert_eq!(pending(&questions), [id(A), id(B)]);
    assert_eq!(questions.answer(&id(A)), None);

    let questions = DayQuestions::of(&[
        unclean(2, A),
        unclean(3, B),
        answer(4, A, DayAnswerKind::Frozen),
    ]);
    assert_eq!(pending(&questions), [id(B)]);
    let given = questions.answer(&id(A)).unwrap();
    assert_eq!(given.answer, DayAnswerKind::Frozen);
    assert_eq!(given.uid, 1000);
    assert!(
        questions.get(&id(A)).is_some(),
        "an answered end stays readable"
    );
}

#[test]
fn an_answer_for_another_profile_leaves_the_question_pending() {
    let mut wrong = answer(3, A, DayAnswerKind::Frozen);
    if let Payload::DayAnswer(given) = &mut wrong.payload {
        given.offsets = offsets(-20);
    }
    let questions = DayQuestions::of(&[unclean(2, A), wrong]);
    assert_eq!(pending(&questions), [id(A)]);
    assert_eq!(questions.answer(&id(A)), None);
}

#[test]
fn a_later_answer_replaces_the_earlier_one_and_keeps_the_question_answered() {
    let questions = DayQuestions::of(&[
        unclean(2, A),
        answer(3, A, DayAnswerKind::Frozen),
        answer(4, A, DayAnswerKind::PowerOff),
    ]);
    assert_eq!(
        questions.answer(&id(A)).map(|given| given.answer),
        Some(DayAnswerKind::PowerOff)
    );
    assert_eq!(pending(&questions), Vec::<BootId>::new());
}

#[test]
fn an_answer_about_a_boot_with_no_unclean_end_is_ignored() {
    let questions = DayQuestions::of(&[answer(2, A, DayAnswerKind::Unsure), unclean(3, A)]);
    assert_eq!(questions.answer(&id(A)), None);
    assert_eq!(pending(&questions), [id(A)]);
    assert_eq!(questions.get(&id(B)), None);
}

#[test]
fn a_repeated_unclean_end_does_not_reopen_an_answered_question() {
    let questions = DayQuestions::of(&[
        unclean(2, A),
        answer(3, A, DayAnswerKind::Graphics),
        unclean(4, A),
    ]);
    assert_eq!(pending(&questions), Vec::<BootId>::new());
    assert!(questions.answer(&id(A)).is_some());
}

#[test]
fn applying_events_one_at_a_time_equals_folding_them_together() {
    let events = [
        unclean(2, A),
        unclean(3, B),
        answer(4, B, DayAnswerKind::Graphics),
        answer(5, B, DayAnswerKind::Unsure),
    ];
    let mut stepped = DayQuestions::default();
    for event in &events {
        stepped.apply(event);
    }
    assert_eq!(stepped, DayQuestions::of(&events));
}

#[test]
fn the_fold_survives_a_round_trip_through_json() {
    let questions = DayQuestions::of(&[
        unclean(2, A),
        unclean(3, B),
        answer(4, A, DayAnswerKind::PowerOff),
    ]);
    let json = serde_json::to_string(&questions).unwrap();
    assert_eq!(
        serde_json::from_str::<DayQuestions>(&json).unwrap(),
        questions
    );
}

#[test_case(DayAnswerKind::Frozen, "frozen"; "frozen")]
#[test_case(DayAnswerKind::Graphics, "graphics"; "graphics")]
#[test_case(DayAnswerKind::PowerOff, "power_off"; "power off")]
#[test_case(DayAnswerKind::Unsure, "unsure"; "unsure")]
fn answers_are_snake_case_on_the_wire(answer: DayAnswerKind, wire: &str) {
    assert_eq!(answer.as_str(), wire);
    assert_eq!(
        serde_json::to_string(&answer).unwrap(),
        format!("\"{wire}\"")
    );
    assert_eq!(
        serde_json::from_str::<DayAnswerKind>(&format!("\"{wire}\"")).unwrap(),
        answer
    );
}

#[test]
fn an_unclean_end_without_evidence_reads_as_unknown() {
    let json = serde_json::json!({
        "boot": A,
        "offsets": {"0": -10, "1": -5},
        "reset_reason": {"kind": "watchdog", "raw": ["watchdog"], "supported": true},
    });
    let end: DayUncleanEnd = serde_json::from_value(json).unwrap();
    assert_eq!(end.evidence, EndEvidence::unknown());
    assert_eq!(end.class(), EndClass::Watchdog);
}

#[test]
fn an_answer_refuses_unknown_fields_and_an_unknown_answer() {
    let known = serde_json::json!({
        "boot": A, "offsets": {"0": -10}, "answer": "frozen", "uid": 1000,
    });
    assert!(serde_json::from_value::<DayAnswer>(known.clone()).is_ok());
    let mut extra = known.clone();
    extra["extra"] = serde_json::json!(1);
    assert!(serde_json::from_value::<DayAnswer>(extra).is_err());
    let mut other = known;
    other["answer"] = serde_json::json!("maybe");
    assert!(serde_json::from_value::<DayAnswer>(other).is_err());
}

#[test]
fn the_summary_is_the_journal_message() {
    let end = end_of(
        A,
        Some(ResetKind::Watchdog),
        read(0, 0),
        MceCount::Unknown {},
        faults(&[RING, KernelFault::NvidiaXid { code: 79 }], false),
    );
    assert_eq!(end.summary(), Payload::from(end.clone()).message());
    assert!(
        end.summary().ends_with(
            "kernel log tail: amdgpu timeout, NVIDIA Xid 79; the log ended at the last error"
        ),
        "{}",
        end.summary()
    );
}

const SUMMARY_HEAD: &str = "day observer of boot aaaaaaaa ended without a clean stop; it wrote no checkpoint; last offsets [00: -10, 01: -5]; machine checks: ";

#[test_case(read(0, 0), read(0, 0), KernelTail::Quiet {}, "none in that boot, none in the next; kernel log tail: no recognized errors"; "quiet tail and no machine checks")]
#[test_case(read(3, 0), read(0, 0), KernelTail::Quiet {}, "0 uncorrected and 3 corrected in that boot, none in the next; kernel log tail: no recognized errors"; "corrected only in the crashed boot")]
#[test_case(read(0, 2), read(5, 1), KernelTail::Quiet {}, "2 uncorrected and 0 corrected in that boot, 1 uncorrected and 5 corrected in the next; kernel log tail: no recognized errors"; "uncorrected listed before corrected in both boots")]
#[test_case(MceCount::Unknown {}, read(1, 0), KernelTail::Unreadable {}, "unknown in that boot, 0 uncorrected and 1 corrected in the next; kernel log tail: unreadable"; "unreadable tail")]
#[test_case(MceCount::Unknown {}, MceCount::Unknown {}, KernelTail::Missing {}, "unknown in that boot, unknown in the next; kernel log tail: missing"; "missing tail")]
#[test_case(read(0, 0), read(0, 0), faults(&[KernelFault::MachineCheck {}], true), "none in that boot, none in the next; kernel log tail: machine check; logging continued after the last error"; "machine check in the tail")]
#[test_case(read(0, 0), read(0, 0), faults(&[KernelFault::GraphicsReset { driver: GraphicsDriver::I915 }], false), "none in that boot, none in the next; kernel log tail: i915 reset; the log ended at the last error"; "graphics reset")]
#[test_case(read(0, 0), read(0, 0), faults(&[KernelFault::GraphicsTimeout { driver: GraphicsDriver::Xe }], true), "none in that boot, none in the next; kernel log tail: xe timeout; logging continued after the last error"; "graphics timeout")]
#[test_case(read(0, 0), read(0, 0), faults(&[KernelFault::Oops {}, KernelFault::HungTask {}], false), "none in that boot, none in the next; kernel log tail: kernel oops, hung task; the log ended at the last error"; "oops then hung task")]
fn the_summary_words_the_evidence(
    crashed: MceCount,
    successor: MceCount,
    tail: KernelTail,
    expected: &str,
) {
    let end = end_of(A, None, crashed, successor, tail);
    assert_eq!(end.summary(), format!("{SUMMARY_HEAD}{expected}"));
}
