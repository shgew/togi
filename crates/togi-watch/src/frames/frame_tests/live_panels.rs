//! A live trial state never moves a row: Main's rectangles depend on the screen alone, so What happened keeps its
//! heading row and the NOW rows keep their place while a trial runs, ends, waits for the next intent, is prepared
//! and starts again.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use togi_journal::Outcome;

use super::{Fixture, SIZES, main_screen};
use crate::frames::measure::Main;
use crate::frames::testing::rows_of;
use crate::tests::kit::Script;

fn slot(buf: &Buffer, rect: Rect) -> Vec<String> {
    rows_of(buf)
        .iter()
        .skip(usize::from(rect.y))
        .take(usize::from(rect.height))
        .map(|row| row.trim().to_owned())
        .collect()
}

#[test]
fn live_panels_keep_their_rows_from_a_running_trial_to_the_next() {
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
    assert!(
        frames[3]
            .1
            .model
            .snapshot()
            .trial
            .is_some_and(|t| t.started.is_some())
    );
    for (width, height) in SIZES {
        let main = Main::new(&main_screen(width, height));
        for (name, fx) in &frames {
            let snapshot = fx.model.snapshot();
            let (buf, _) = fx.main(width, height);
            let heading = slot(&buf, main.history_head).join("\n");
            assert!(
                heading.starts_with("WHAT HAPPENED"),
                "{width}x{height}: {name} moves What happened off row {}:\n{heading}",
                main.history_head.y
            );
            if snapshot.trial.is_some() {
                continue;
            }
            assert!(snapshot.between_trials(), "{name}");
            let band = slot(&buf, main.now).join("\n");
            assert!(
                band.contains("BETWEEN TRIALS"),
                "{width}x{height}: {name} loses the between-trials sentence:\n{band}"
            );
            assert!(
                !band.contains("left") && !band.contains("past"),
                "{width}x{height}: {name} invents a countdown:\n{band}"
            );
        }
    }
}
