//! What the whole frame says for tuner states the staged frames do not reach: the day's step, freeze, firmware and
//! questions, a record-sourced event, a re-qualification task, a retried trial and a CCD the tuner has no state for.

use test_case::test_case;
use togi_journal::{
    CcdVector, Classification, DayEventId, HoldReason, Queue, ReproductionState, Resolution,
    SessionId, StepKind, TaskId,
};
use togi_machine::{BootId, PerCore};
use togi_tuner::policy::{EventProjection, FirmwareValidity, ReproductionProjection};

use super::text;
use crate::frames::hand::{Hand, Staged, ccd, profile_of, step};
use crate::frames::testing::FIXED_NOW;
use crate::tests::kit::seq;

fn record() -> DayEventId {
    DayEventId::Record {
        session: SessionId::new(FIXED_NOW, None).expect("a session id"),
        seq: seq(12),
    }
}

/// The reason a hold names when CCD 0 -15, CCD 1 -5 failed qualification.
fn failed_qualification() -> HoldReason {
    HoldReason::QualificationFailed {
        profile: CcdVector {
            ccd: profile_of(&[(0, -15), (1, -5)]),
            cores: PerCore::default(),
        },
        witnesses: vec![seq(9)],
    }
}

fn shown(hand: &Hand) -> String {
    text(&hand.main(240, 67).0)
}

#[track_caller]
fn assert_shows(shown: &str, want: &str) {
    assert!(shown.contains(want), "the frame lacks {want:?}:\n{shown}");
}

#[test_case(StepKind::Hold { blocked_by: Vec::new() }, false, "last step   hold, nothing blocks it"; "an unblocked hold")]
#[test_case(
    StepKind::Hold {
        blocked_by: vec![
            HoldReason::Incomplete { credited_s: 3600, required_s: 28_800 },
            HoldReason::QuestionPending { boot: BootId::from("boot-2".to_owned()) },
        ],
    },
    false,
    "last step   hold: incomplete, 1h00m of 8h00m, a question is pending";
    "a hold blocked by its hours and a question"
)]
#[test_case(
    StepKind::Hold { blocked_by: vec![HoldReason::AtCap {}, HoldReason::NotQualified {}] },
    false,
    "last step   hold: at the cap, not qualified";
    "a hold at the cap and unqualified"
)]
#[test_case(
    StepKind::Hold { blocked_by: vec![HoldReason::Vetoed { veto: record() }, HoldReason::EventUnresolved { event: record() }] },
    false,
    "last step   hold: vetoed by record #12, record #12 is unresolved";
    "a hold behind a veto and an unresolved event"
)]
#[test_case(
    StepKind::Hold { blocked_by: vec![HoldReason::Frozen {}, HoldReason::Paused {}, HoldReason::Expired {}] },
    false,
    "last step   hold: frozen, paused, expired";
    "a frozen paused expired hold"
)]
#[test_case(
    StepKind::Hold { blocked_by: vec![failed_qualification()] },
    false,
    "last step   hold: -15/-5 failed qualification";
    "a hold on a failed qualification"
)]
#[test_case(StepKind::Promote { changed: ccd(1) }, true, "last step   promoted CCD 1"; "an applied promotion")]
#[test_case(
    StepKind::Promote { changed: ccd(1) },
    false,
    "last step   proposed promoting CCD 1, not applied";
    "a proposed promotion"
)]
#[test_case(
    StepKind::Retreat { changed: vec![ccd(0), ccd(1)], cause: togi_journal::RetreatCause::ShallowerCap {}, wanted: None },
    true,
    "last step   retreated CCD 0, 1";
    "an applied retreat"
)]
#[test_case(
    StepKind::Retreat { changed: vec![ccd(0)], cause: togi_journal::RetreatCause::ShallowerCap {}, wanted: None },
    false,
    "last step   proposed retreating CCD 0, not applied";
    "a proposed retreat"
)]
#[test_case(
    StepKind::FirmwareZero { changed: vec![ccd(0), ccd(1)] },
    true,
    "last step   reset CCD 0, 1 after firmware change";
    "an applied firmware reset"
)]
#[test_case(
    StepKind::FirmwareZero { changed: vec![ccd(0), ccd(1)] },
    false,
    "last step   proposed resetting CCD 0, 1 after firmware change, not applied";
    "a proposed firmware reset"
)]
fn the_day_names_its_last_step(kind: StepKind, applied: bool, want: &str) {
    let mut hand = Staged::Day.hand();
    hand.applied_steps.clear();
    let step = step(kind);
    if applied {
        hand.applied_steps.insert(step.decision);
    }
    hand.policy.day.last_step = Some((seq(5), step));
    assert_shows(&shown(&hand), want);
}

#[test]
fn a_firmware_change_freezes_the_day_and_its_questions_and_events_are_named() {
    let mut hand = Staged::Day.hand();
    hand.policy.applied = None;
    hand.policy.day.freeze = true;
    hand.policy.day.firmware = FirmwareValidity::Invalidated { since: seq(7) };
    hand.policy.day.pending_questions = vec![
        BootId::from("boot-2".to_owned()),
        BootId::from("boot-3".to_owned()),
    ];
    hand.policy.events = vec![EventProjection {
        event: record(),
        classification: Classification::NonCpu {},
        resolution: Resolution::Resolved { retreat: None },
        reproduction: ReproductionState::Untestable,
    }];
    let shown = shown(&hand);
    for want in [
        "freeze      on: D kept, no promotion; retreats and firmware resets still apply",
        "firmware    changed at #7; D awaits a reset",
        "questions   2 pending",
        "events      record #12 · not the cpu · resolved",
    ] {
        assert_shows(&shown, want);
    }
}

#[test]
fn a_reproduction_of_a_recorded_event_names_the_record() {
    let mut hand = Staged::Reproducing.hand();
    if let Some(night) = hand.policy.night.as_mut() {
        night.reproductions = vec![ReproductionProjection {
            source: record(),
            state: ReproductionState::Pending,
            consumed_s: 1500,
            remaining: Vec::new(),
        }];
    }
    assert_shows(&shown(&hand), "reproducing record #12");
}

#[test]
fn a_retried_re_qualification_says_so() {
    let mut hand = Staged::Qualifying.hand();
    if let Some(trial) = hand.trial.as_mut() {
        trial.task = Some(TaskId {
            queue: Queue::Requalification,
            ordinal: 4,
        });
        trial.retry = true;
    }
    let shown = shown(&hand);
    assert_shows(&shown, "requalifying -10/-8");
    assert_shows(&shown, " · RETRY");
}

#[test]
fn a_ccd_the_tuner_has_no_state_for_says_so() {
    let mut hand = Staged::Between.hand();
    hand.policy.ccds.clear();
    assert_shows(&shown(&hand), "no tuner state for this CCD yet");
}
