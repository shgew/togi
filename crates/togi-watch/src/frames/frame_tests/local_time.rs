//! Every time on screen is local to the reader's zone: the journal stores UTC.

use jiff::tz::{self, TimeZone};
use togi_watchtest::Scenario;

use super::{Fixture, clock, draw_journal, text};
use crate::frames::testing::screen;
use crate::screen::{Colours, View};

fn tokyo() -> TimeZone {
    TimeZone::fixed(tz::offset(9))
}

fn frame(fx: &Fixture, view: View, zone: &TimeZone) -> String {
    let sc = screen(view, 240, 67, Colours::None, false);
    text(&draw_journal(&fx.journal(), &sc, fx.now, zone).0)
}

fn require(name: &str, shown: &str, wants: &[String]) {
    for want in wants {
        assert!(
            shown.contains(want),
            "{name} lacks local {want:?}:\n{shown}"
        );
    }
}

#[test]
fn every_time_on_screen_is_local() {
    let zone = tokyo();
    let at = |time, format: &str| clock(time, &zone, format);

    let stopped = Fixture::scenario(Scenario::Stopped);
    let snapshot = stopped.model.snapshot();
    let stop = snapshot.stopped.expect("a stop");
    let failure = snapshot.last_failure.expect("a failure");
    require(
        "stopped",
        &frame(&stopped, View::Main, &zone),
        &[
            format!("togi   {}", at(stopped.now, "%H:%M:%S")),
            format!("{} · ", at(stop.at, "%H:%M:%S")),
            format!("last observed failure {}", at(failure, "%H:%M")),
            format!("{}  ", at(snapshot.history[0].at, "%H:%M")),
        ],
    );
    require(
        "stopped",
        &frame(&stopped, View::Log, &zone),
        &[format!("{}  ", at(snapshot.log[0].at, "%H:%M:%S"))],
    );
    let utc = frame(&stopped, View::Main, &TimeZone::UTC);
    assert!(
        !utc.contains(&format!("togi   {}", at(stopped.now, "%H:%M:%S"))),
        "the zone moves the header clock"
    );

    let recovering = Fixture::scenario(Scenario::Recovering);
    let crash_at = recovering
        .model
        .snapshot()
        .recover
        .expect("a recovery")
        .crash_at;
    require(
        "recovering",
        &frame(&recovering, View::Main, &zone),
        &[format!("detected {}", at(crash_at, "%H:%M:%S"))],
    );

    let crashed = Fixture::scenario(Scenario::Crashed);
    let started = crashed
        .model
        .snapshot()
        .trial
        .expect("a trial")
        .started
        .expect("started");
    require(
        "crashed",
        &frame(&crashed, View::Main, &zone),
        &[format!(" · started {}", at(started, "%H:%M:%S"))],
    );

    let hunt = Fixture::scenario(Scenario::Hunt);
    let began = hunt
        .model
        .snapshot()
        .hunt
        .expect("a hunt")
        .facts
        .started
        .expect("started");
    require(
        "hunt",
        &frame(&hunt, View::Hunts, &zone),
        &[format!("started {}", at(began, "%H:%M"))],
    );

    let idle = Fixture::scenario(Scenario::Idle);
    let hold = idle.model.snapshot().idle_hold().expect("an idle trial");
    require(
        "idle",
        &frame(&idle, View::Main, &zone),
        &[
            format!("togi   {}", at(hold.started, "%H:%M:%S")),
            format!("planned end {}", at(hold.planned_end, "%H:%M")),
            format!("started {}", at(hold.started, "%H:%M")),
        ],
    );
}
