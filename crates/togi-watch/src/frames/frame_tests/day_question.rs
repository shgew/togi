//! The pending day question reaches the frame's footer: folded from the journal while a trial runs, shown until the
//! operator's answer is recorded.

use togi_journal::{DayAnswer, DayAnswerKind, DayUncleanEnd};
use togi_machine::{
    EndEvidence, KernelFault, KernelTail, MceCount, PerCore, ResetKind, ResetReason,
};

use super::{Fixture, append, journal, text};

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
            asleep: None,
        },
    }
}

#[test]
fn a_pending_day_question_shows_in_the_footer_while_a_trial_runs_until_it_is_answered() {
    let mut events = journal(Vec::new());
    let boot = events.last().expect("an event").boot.to_string();
    append(&mut events, unclean_end().into(), 1, &boot);

    let asked = Fixture::of(&events);
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
    let events = journal(Vec::new());
    let shown = text(&Fixture::of(&events).main(160, 45).0);
    assert!(!shown.to_lowercase().contains("day question"), "{shown}");
}

#[test]
fn two_pending_questions_are_counted_and_a_narrow_footer_keeps_a_short_form() {
    let mut events = journal(Vec::new());
    let boot = events.last().expect("an event").boot.to_string();
    append(&mut events, unclean_end().into(), 1, &boot);
    let second = DayUncleanEnd {
        boot: "8b2c3d4e-0000-0000-0000-000000000000".to_owned().into(),
        ..unclean_end()
    };
    append(&mut events, second.into(), 2, &boot);
    let asked = Fixture::of(&events);
    let wide = text(&asked.main(160, 45).0);
    assert!(
        wide.lines()
            .last()
            .is_some_and(|row| row.contains("2 day questions pending: togi status")),
        "the footer lacks the count:\n{wide}"
    );
    let short = (40..160).any(|width| {
        text(&asked.main(width, 45).0)
            .lines()
            .last()
            .is_some_and(|row| {
                row.contains("day question: togi status") && !row.contains("pending")
            })
    });
    assert!(short, "no width shortens the question to its short form");
}
