//! Main's NOW rows: the operation sentence and the trial bar, the frozen R6 row, and the status that replaces them.

use togi_journal::{DeadEndCondition, ShutdownReason};
use togi_machine::Regime;

use super::super::stage::tests::{Rig, at, failed_on, trial};
use super::now_lines;
use crate::frames::fit::plain;
use crate::model::{DeadEndView, StopView};

fn rows(rig: &Rig, width: usize) -> [String; 2] {
    now_lines(&rig.scene(), width).map(|line| plain(&line))
}

fn running() -> Rig {
    let mut rig = Rig::new(3, 3);
    let started = rig.now;
    rig.trial = Some(trial(Regime::R3, Some(started), 600));
    rig.now = at("2026-10-12T10:05:00Z");
    rig
}

#[test]
fn a_running_trial_has_one_sentence_and_a_fixed_width_bar_with_elapsed_over_required() {
    let rig = running();
    let [sentence, bar] = rows(&rig, 120);
    assert!(sentence.starts_with("NOW  "), "{sentence}");
    assert!(sentence.contains("R3"), "{sentence}");
    assert!(bar.contains("5:00 / 10:00"), "{bar}");
    let cells = |text: &str| text.chars().filter(|c| matches!(c, '█' | '░')).count();
    assert_eq!(cells(&bar), 24, "{bar}");
    assert_eq!(
        cells(&bar.replace('░', "")),
        12,
        "half elapsed fills half the cells: {bar}"
    );
    let [_, narrow] = rows(&rig, 60);
    assert_eq!(cells(&narrow), 24, "{narrow}");
}

#[test]
fn the_bar_shows_elapsed_time_without_colour() {
    let mut rig = running();
    let at_start = rows(&rig, 120)[1].clone();
    rig.now = at("2026-10-12T10:09:00Z");
    let later = rows(&rig, 120)[1].clone();
    assert_ne!(at_start, later);
    assert!(later.matches('█').count() > at_start.matches('█').count());
}

#[test]
fn a_frozen_r6_row_gives_the_planned_end_and_no_moving_elapsed() {
    let mut rig = Rig::new(3, 3);
    let started = rig.now;
    rig.trial = Some(trial(Regime::R6, Some(started), 7200));
    let first = rows(&rig, 120);
    rig.now = at("2026-10-12T11:30:00Z");
    assert_eq!(rows(&rig, 120), first, "the held frame does not move");
    let bar = &first[1];
    assert!(bar.contains("planned end 12:00"), "{bar}");
    assert!(bar.contains("2:00:00 required"), "{bar}");
    assert!(!bar.contains(" / "), "no moving elapsed: {bar}");
    assert!(!bar.contains('█'), "{bar}");
}

#[test]
fn between_trials_the_status_replaces_the_bar() {
    let rig = Rig::new(3, 3);
    let [sentence, below] = rows(&rig, 120);
    assert!(sentence.starts_with("NOW  BETWEEN TRIALS"), "{sentence}");
    assert!(!below.contains('█') && !below.contains('░'), "{below}");
}

#[test]
fn a_stop_replaces_the_operation_and_the_bar() {
    let mut rig = running();
    rig.stopped = Some(StopView {
        at: rig.now,
        reason: ShutdownReason::Signal,
        saved: true,
    });
    let [sentence, below] = rows(&rig, 120);
    assert!(sentence.starts_with("NOW  STOPPED"), "{sentence}");
    assert!(!sentence.contains("R3"), "{sentence}");
    assert!(!below.contains('█') && !below.contains('░'), "{below}");
}

#[test]
fn a_stop_says_its_reason_once_and_its_last_trial_under_it_never_the_rows_as_unapplied() {
    let mut rig = Rig::new(3, 3);
    rig.last = Some(failed_on(Some(1)));
    rig.stopped = Some(StopView {
        at: rig.now,
        reason: ShutdownReason::Signal,
        saved: true,
    });
    let [sentence, below] = rows(&rig, 160);
    assert!(sentence.contains("stopped by a signal"), "{sentence}");
    assert!(below.starts_with("last: FAILURE"), "{below}");
    assert!(
        !below.contains("not applied") && !below.contains("stopped by"),
        "{below}"
    );
}

#[test]
fn a_stop_with_no_trial_behind_it_has_no_second_row() {
    let mut rig = Rig::new(3, 3);
    rig.stopped = Some(StopView {
        at: rig.now,
        reason: ShutdownReason::Signal,
        saved: false,
    });
    assert!(rows(&rig, 160)[1].is_empty());
}

#[test]
fn a_dead_end_too_long_for_the_row_keeps_its_cause_and_cuts_the_rest() {
    let mut rig = Rig::new(3, 3);
    rig.dead_end = Some(DeadEndView {
        at: rig.now,
        condition: DeadEndCondition::Preflight,
        detail: "failed checks: ryzen_smu, msr, watchdog, pstore, topology, mprime, ycruncher, kernel_log"
            .to_owned(),
    });
    for width in [60, 80] {
        let [sentence, _] = rows(&rig, width);
        assert!(
            sentence.starts_with("NOW  DEAD END · preflight: failed checks: ryzen_smu"),
            "{sentence}"
        );
        assert!(sentence.ends_with("..."), "{sentence}");
        assert!(sentence.chars().count() <= width, "{sentence}");
    }
}

#[test]
fn a_session_less_scene_has_no_now_rows() {
    let model = crate::model::Model::new();
    let zone = jiff::tz::TimeZone::UTC;
    let scene = crate::frames::scene::Scene::new(
        model.snapshot(),
        at("2026-10-12T10:00:00Z"),
        &zone,
        crate::screen::Colours::None,
    );
    assert!(
        now_lines(&scene, 80)
            .iter()
            .all(|line| plain(line).is_empty())
    );
}
