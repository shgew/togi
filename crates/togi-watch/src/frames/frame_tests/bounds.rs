//! Every staged frame at every size, view, key setting and colour profile stays inside the frame area and draws only
//! glyphs its profile can draw; help and log scroll to their extent and stay there.

use togi_machine::Regime;

use super::{ALL_SIZES, PROFILES, VIEWS, assert_glyphs_safe};
use crate::frames::hand::{Hand, Staged};
use crate::frames::testing::screen;
use crate::screen::{Colours, Hold, Screen, View};

fn every_screen(fx: &Hand, scenario: Staged) {
    let scenario = scenario.name();
    let started_idle = fx.snapshot().trial.is_some_and(|trial| {
        trial.regime == Regime::R6 && trial.started.is_some() && !trial.crashed
    });
    assert!(started_idle || scenario != "idle", "{scenario}");
    let hold = if started_idle {
        Hold::Frozen
    } else {
        Hold::Never
    };
    for (width, height) in ALL_SIZES {
        for view in VIEWS {
            for colours in PROFILES {
                for keys in [true, false] {
                    let sc = screen(view, width, height, colours, keys);
                    let (buf, drawn) = fx.draw(&sc, fx.now);
                    assert_glyphs_safe(
                        &buf,
                        colours,
                        &format!("{scenario} {width}x{height} {view:?} {colours:?} keys {keys}"),
                    );
                    assert_eq!(
                        drawn.hold, hold,
                        "{scenario} {width}x{height} {view:?} holds as the idle trial does"
                    );
                }
            }
        }
    }
}

fn every_scroll(fx: &Hand, scenario: Staged, sample: fn(usize) -> usize) {
    let scenario = scenario.name();
    for view in [View::Help, View::Log] {
        for (width, height) in ALL_SIZES {
            let at = |scroll: usize| {
                let sc = Screen {
                    scroll,
                    ..screen(view, width, height, Colours::Full, true)
                };
                fx.draw(&sc, fx.now)
            };
            let (top, drawn) = at(0);
            let extent = drawn.scroll;
            let (end, _) = at(usize::MAX);
            let scroll = sample(extent);
            let (buf, drawn) = at(scroll);
            assert_eq!(
                drawn.scroll, extent,
                "{scenario} {view:?} {width}x{height} at {scroll}: the extent does not move"
            );
            assert_eq!(
                scroll >= extent,
                buf == end,
                "{scenario} {view:?} {width}x{height} at {scroll}: only the extent shows the end"
            );
            assert_eq!(
                extent > 0,
                top != end,
                "{scenario} {view:?} {width}x{height}: the end differs from the top only when the view scrolls"
            );
        }
    }
}

macro_rules! scenarios {
    ($($name:ident => $scenario:expr),+ $(,)?) => {$(
        mod $name {
            use super::*;

            #[test]
            fn screens() {
                let scenario = $scenario;
                let fx = scenario.hand();
                every_screen(&fx, scenario);
            }

            #[test_case::test_case(|_| 1; "one")]
            #[test_case::test_case(|_| 3; "three")]
            #[test_case::test_case(|extent| extent / 2; "half")]
            #[test_case::test_case(|extent| extent.saturating_sub(1); "before end")]
            #[test_case::test_case(|extent| extent; "end")]
            #[test_case::test_case(|extent| extent + 1; "one past end")]
            #[test_case::test_case(|extent| extent + 100_000; "far past end")]
            fn scrolling(sample: fn(usize) -> usize) {
                let scenario = $scenario;
                let fx = scenario.hand();
                every_scroll(&fx, scenario, sample);
            }
        }
    )+};
}

scenarios! {
    mapping => Staged::Mapping,
    qualifying => Staged::Qualifying,
    reproducing => Staged::Reproducing,
    day => Staged::Day,
    idle => Staged::Idle,
    crashed => Staged::Crashed,
    recovering => Staged::Recovering,
    between => Staged::Between,
    deadend => Staged::Deadend,
    stopped => Staged::Stopped,
}
