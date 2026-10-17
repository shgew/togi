//! A live trial state never moves a panel: the rectangles stay put while a trial runs, ends, waits for the next
//! intent, is prepared and starts again.

use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use togi_journal::Outcome;

use super::{Fixture, SIZES, main_screen};
use crate::frames::measure::{Measure, measure};
use crate::frames::scene::Scene;
use crate::frames::testing::rows_of;
use crate::screen::Colours;
use crate::tests::kit::Script;

fn slot(buf: &Buffer, rect: Rect) -> Vec<String> {
    rows_of(buf)
        .iter()
        .skip(usize::from(rect.y))
        .take(usize::from(rect.height))
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn measured(fx: &Fixture, width: u16, height: u16) -> Measure {
    let zone = TimeZone::UTC;
    let scene = Scene::new(fx.model.snapshot(), fx.now, &zone, Colours::None);
    measure(&scene, &main_screen(width, height))
}

#[test]
fn live_panels_keep_their_rectangles_from_a_running_trial_to_the_next() {
    let mut script = Script::at_limit();
    let mut cuts: Vec<(&str, usize)> = vec![("no trial yet", script.events.len())];
    let plan = script.next_trial();
    cuts.push(("decided", script.events.len()));
    let (intent, trial) = script.intent(&plan);
    cuts.push(("preparing", script.events.len()));
    script.start(&trial);
    cuts.push(("running", script.events.len()));
    script.end(intent, &trial, Outcome::Pass);
    cuts.push(("ended", script.events.len()));
    let plan = script.next_trial();
    cuts.push(("next decided", script.events.len()));
    let (_, trial) = script.intent(&plan);
    cuts.push(("next preparing", script.events.len()));
    script.start(&trial);
    cuts.push(("next running", script.events.len()));
    let frames: Vec<(&str, Fixture)> = cuts
        .iter()
        .map(|&(name, len)| (name, Fixture::of(&script.events[..len])))
        .collect();
    let running = &frames[3].1;
    assert!(
        running
            .model
            .snapshot()
            .trial
            .is_some_and(|t| t.started.is_some())
    );
    for (width, height) in SIZES {
        let want = measured(running, width, height);
        let (running_buf, _) = running.main(width, height);
        let outcomes = slot(&running_buf, want.outcomes);
        assert!(
            outcomes.iter().any(|row| !row.is_empty()),
            "{width}x{height}: a running trial shows outcome lines"
        );
        for (name, fx) in &frames {
            let snapshot = fx.model.snapshot();
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
                !band.contains("left") && !band.contains("past"),
                "{width}x{height}: {name} invents a countdown:\n{band}"
            );
            assert!(
                band.contains("next trial:"),
                "{width}x{height}: {name} hides the next trial:\n{band}"
            );
            assert_eq!(
                band.contains("last:"),
                snapshot.last.is_some(),
                "{width}x{height}: {name} misreports the last trial:\n{band}"
            );
            if snapshot.last.is_some() {
                assert_eq!(
                    slot(&buf, want.outcomes),
                    outcomes,
                    "{width}x{height}: {name} drops the last trial's outcomes"
                );
            }
        }
    }
}
