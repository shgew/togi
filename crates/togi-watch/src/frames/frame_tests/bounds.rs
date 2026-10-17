//! Every scenario at every size, view, key setting and colour profile stays inside the frame area and draws only
//! console glyphs; help and log scroll to their extent and stay there.

use togi_machine::Regime;
use togi_watchtest::Scenario;

use super::{ALL_SIZES, Fixture, PROFILES, VIEWS};
use crate::frames::testing::{assert_glyphs_drawable, screen};
use crate::screen::{Colours, Hold, Screen};

fn every_screen(fx: &Fixture, scenario: Scenario) {
    let started_idle = fx.model.snapshot().trial.is_some_and(|trial| {
        trial.regime == Regime::R6 && trial.started.is_some() && !trial.crashed
    });
    assert!(started_idle || scenario != Scenario::Idle, "{scenario}");
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
                    assert_glyphs_drawable(
                        &buf,
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

fn every_scroll(fx: &Fixture, scenario: Scenario) {
    for view in VIEWS {
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
            for scroll in [1, 3, extent / 2, extent.saturating_sub(1), extent] {
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
            }
            for scroll in [extent + 1, extent + 100_000] {
                assert_eq!(
                    at(scroll).0,
                    end,
                    "{scenario} {view:?} {width}x{height} at {scroll}: past the extent shows the end"
                );
            }
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
        #[test]
        fn $name() {
            let scenario = $scenario;
            let fx = Fixture::scenario(scenario);
            every_screen(&fx, scenario);
            every_scroll(&fx, scenario);
        }
    )+};
}

scenarios! {
    search => Scenario::Search,
    confirm => Scenario::Confirm,
    checking => Scenario::Checking,
    hunt => Scenario::Hunt,
    member_probe => Scenario::MemberProbe,
    round => Scenario::Round,
    confirmation => Scenario::Confirmation,
    concluded => Scenario::Concluded,
    unconfirmed => Scenario::Unconfirmed,
    hunt_bios => Scenario::HuntBios,
    idle => Scenario::Idle,
    crashed => Scenario::Crashed,
    recovering => Scenario::Recovering,
    combination => Scenario::Combination,
    between => Scenario::Between,
    deadend => Scenario::Deadend,
    first_result => Scenario::FirstResult,
    stopped => Scenario::Stopped,
}

#[test]
fn every_page_stays_inside_the_frame_at_a_sweep_of_sizes() {
    let fx = Fixture::scenario(Scenario::Checking);
    for width in [1, 2, 5, 10, 20, 40, 60, 79, 80, 100, 159, 160, 200, 240] {
        for height in [1, 2, 3, 4, 5, 10, 20, 29, 30, 44, 45, 60, 67] {
            for view in VIEWS {
                for scroll in [0, usize::MAX] {
                    let sc = Screen {
                        scroll,
                        ..screen(view, width, height, Colours::Full, true)
                    };
                    let (buf, _) = fx.draw(&sc, fx.now);
                    assert_glyphs_drawable(&buf, &format!("{width}x{height} {view:?} at {scroll}"));
                }
            }
        }
    }
}
