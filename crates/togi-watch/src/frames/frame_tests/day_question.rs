//! The pending day question reaches the frame's footer: folded from the journal while a trial runs, shown until the
//! operator's answer is recorded.

use togi_journal::{DayAnswer, DayAnswerKind, DayUncleanEnd};
use togi_machine::{
    EndEvidence, KernelFault, KernelTail, MceCount, PerCore, ResetKind, ResetReason,
};
use togi_watchtest::Scenario;

use super::{Fixture, append, text};

const BOOT: &str = "7a1b2c3d-0000-0000-0000-000000000000";
const FOOTER: &str = "1 day question pending: togi status";

fn unclean_end() -> DayUncleanEnd {
    DayUncleanEnd {
        boot: BOOT.to_owned().into(),
        last_checkpoint: None,
        offsets: PerCore::default(),
        reset_reason: ResetReason {
            kind: Some(ResetKind::Watchdog),
            raw: Vec::new(),
            supported: true,
        },
        evidence: EndEvidence {
            crashed_mces: MceCount::Unknown {},
            successor_mces: MceCount::Unknown {},
            tail: KernelTail::Faults {
                faults: vec![KernelFault::Oops {}],
                logging_continued: false,
            },
        },
    }
}

#[test]
fn a_pending_day_question_shows_in_the_footer_while_a_trial_runs_until_it_is_answered() {
    let mut events = Scenario::Search.events().expect("the scenario's journal");
    let boot = events.last().expect("an event").boot.to_string();
    append(&mut events, unclean_end().into(), 1, &boot);

    let asked = Fixture::of(&events);
    assert!(
        asked.model.snapshot().trial.is_some(),
        "the trial is still running"
    );
    let shown = text(&asked.main(160, 45).0);
    assert!(
        shown.lines().last().is_some_and(|row| row.contains(FOOTER)),
        "the footer lacks the pending question:\n{shown}"
    );

    append(
        &mut events,
        DayAnswer {
            boot: BOOT.to_owned().into(),
            offsets: PerCore::default(),
            answer: DayAnswerKind::Frozen,
            uid: 1000,
        }
        .into(),
        1,
        &boot,
    );
    let answered = Fixture::of(&events);
    assert_eq!(answered.model.snapshot().day.pending().count(), 0);
    let shown = text(&answered.main(160, 45).0);
    assert!(
        !shown.to_lowercase().contains("day question"),
        "the answered question still shows:\n{shown}"
    );
}

#[test]
fn a_journal_without_an_unclean_end_has_no_question_in_the_footer() {
    let events = Scenario::Search.events().expect("the scenario's journal");
    let shown = text(&Fixture::of(&events).main(160, 45).0);
    assert!(!shown.to_lowercase().contains("day question"), "{shown}");
}
