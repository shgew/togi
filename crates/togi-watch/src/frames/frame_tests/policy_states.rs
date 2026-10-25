//! What the whole frame says for tuner states the staged frames do not reach: the day's step, freeze, firmware and
//! questions, a record-sourced event, a re-qualification task, a retried trial and a CCD the tuner has no state for.

use test_case::test_case;
use togi_journal::{
    Classification, DayEventId, HoldReason, Queue, ReproductionState, Resolution, SessionId,
    StepKind, TaskId,
};
use togi_machine::BootId;
use togi_tuner::policy::{EventProjection, FirmwareValidity, ReproductionProjection};

use super::text;
use crate::frames::hand::{Hand, Staged, ccd, step};
use crate::frames::testing::FIXED_NOW;
use crate::tests::kit::seq;

fn record() -> DayEventId {
    DayEventId::Record {
        session: SessionId::new(FIXED_NOW, None).expect("a session id"),
        seq: seq(12),
    }
}

fn shown(hand: &Hand) -> String {
    text(&hand.main(240, 67).0)
}

#[track_caller]
fn assert_shows(shown: &str, want: &str) {
    assert!(shown.contains(want), "the frame lacks {want:?}:\n{shown}");
}

#[test_case(StepKind::Hold { blocked_by: Vec::new() }, "last step   hold, nothing blocks it"; "an unblocked hold")]
#[test_case(
    StepKind::Hold {
        blocked_by: vec![
            HoldReason::Incomplete { credited_s: 3600, required_s: 28_800 },
            HoldReason::QuestionPending { boot: BootId::from("boot-2".to_owned()) },
        ],
    },
    "last step   hold: incomplete, 1h00m of 8h00m, a question is pending";
    "a blocked hold"
)]
#[test_case(
    StepKind::FirmwareZero { changed: vec![ccd(0), ccd(1)] },
    "last step   reset CCD 0, 1 after firmware change";
    "a firmware reset"
)]
fn the_day_names_its_last_step(kind: StepKind, want: &str) {
    let mut hand = Staged::Day.hand();
    hand.policy.day.last_step = Some((seq(5), step(kind)));
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
        "freeze      on: nothing is applied",
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
