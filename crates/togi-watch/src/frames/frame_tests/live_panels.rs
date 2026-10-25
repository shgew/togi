//! A live trial state never moves a panel: the rectangles stay put while a trial runs, ends, waits for the next
//! intent, is prepared and starts again.

use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{SIZES, main_screen};
use crate::frames::hand::{Hand, Staged, ended_trial};
use crate::frames::measure::{Measure, measure};
use crate::frames::scene::Scene;
use crate::frames::testing::rows_of;
use crate::screen::Colours;
use togi_machine::Regime;

fn slot(buf: &Buffer, rect: Rect) -> Vec<String> {
    rows_of(buf)
        .iter()
        .skip(usize::from(rect.y))
        .take(usize::from(rect.height))
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn measured(hand: &Hand, width: u16, height: u16) -> Measure {
    let zone = TimeZone::UTC;
    let scene = Scene::new(hand.snapshot(), hand.now, &zone, Colours::None);
    measure(&scene, &main_screen(width, height))
}

/// The states a trial passes through: none yet, intent recorded, started, ended, the next one's intent, started.
fn states() -> Vec<(&'static str, Hand)> {
    let running = Staged::Mapping.hand();
    let trial = running.trial.expect("a running trial");
    let mut out = Vec::new();
    let mut with = |name, trial: Option<crate::model::TrialView>, last| {
        let mut hand = Staged::Mapping.hand();
        hand.trial = trial;
        hand.last = last;
        out.push((name, hand));
    };
    with("no trial yet", None, None);
    let mut preparing = trial.clone();
    preparing.started = None;
    with("preparing", Some(preparing.clone()), None);
    with("running", Some(trial.clone()), None);
    with("ended", None, Some(ended_trial(Regime::R1)));
    with(
        "next preparing",
        Some(preparing),
        Some(ended_trial(Regime::R1)),
    );
    with("next running", Some(trial), Some(ended_trial(Regime::R1)));
    out
}

#[test]
fn live_panels_keep_their_rectangles_from_a_running_trial_to_the_next() {
    let frames = states();
    let running = &frames[2].1;
    assert!(
        running
            .snapshot()
            .trial
            .is_some_and(|t| t.started.is_some())
    );
    for (width, height) in SIZES {
        let want = measured(running, width, height);
        for (name, fx) in &frames {
            let snapshot = fx.snapshot();
            let between = snapshot.trial.is_none();
            assert_eq!(
                measured(fx, width, height),
                want,
                "{width}x{height}: {name} moves a panel"
            );
            let (buf, _) = fx.main(width, height);
            if !between {
                continue;
            }
            assert!(snapshot.between_trials(), "{name}");
            let band = slot(&buf, want.now).join("\n");
            assert!(
                band.contains("BETWEEN TRIALS"),
                "{width}x{height}: {name} loses the between-trials band:\n{band}"
            );
            assert!(
                band.lines().all(|row| !row.trim_end().ends_with(" left"))
                    && !band.contains("past"),
                "{width}x{height}: {name} invents a countdown:\n{band}"
            );
            assert_eq!(
                band.contains("last:"),
                snapshot.last.is_some(),
                "{width}x{height}: {name} misreports the last trial:\n{band}"
            );
        }
    }
}
