//! Frame time and fold time of the dashboard on simulated sessions.
//!
//! `frame` draws one 240x67 main-view frame of the longest scenario's folded journal, and of the first R7
//! checking trial, which has the most panels, into a buffer; `frame-bytes` also encodes the whole frame as the
//! terminal bytes a first draw writes; `project` folds the checking journal from nothing.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use togi_journal::Event;
use togi_watch::frames::{frame_area, render};
use togi_watch::model::project;
use togi_watch::screen::{Colours, Journal, Quit, Screen, Switch, View};
use togi_watchtest::Scenario;

const SCREEN: Screen = Screen {
    view: View::Main,
    keys: true,
    scroll: 0,
    width: 240,
    height: 67,
    one_frame: false,
    colours: Colours::Full,
    quit: Quit::Keys,
    switch: Switch::Off,
};

fn events(scenario: Scenario) -> Vec<Event> {
    scenario
        .events()
        .unwrap_or_else(|error| panic!("{}: {error}", scenario.name()))
}

fn frame(c: &mut Criterion, name: &str, events: &[Event]) {
    let model = project(events);
    let snapshot = model.snapshot();
    let journal = Journal::Ready {
        snapshot,
        log: snapshot.log(),
    };
    let now = events.last().map_or_else(jiff::Timestamp::now, |event| {
        event.time + SignedDuration::from_secs(40)
    });
    let zone = TimeZone::UTC;
    let mut buf = Buffer::empty(frame_area(&SCREEN));
    c.bench_function(&format!("frame/{name}"), |b| {
        b.iter(|| {
            buf.reset();
            black_box(render(black_box(&journal), &SCREEN, now, &zone, &mut buf))
        });
    });
    let blank = Buffer::empty(frame_area(&SCREEN));
    let mut bytes = Vec::with_capacity(1 << 16);
    c.bench_function(&format!("frame-bytes/{name}"), |b| {
        b.iter(|| {
            buf.reset();
            render(black_box(&journal), &SCREEN, now, &zone, &mut buf);
            bytes.clear();
            let drawn = CrosstermBackend::new(&mut bytes).draw(blank.diff(&buf).into_iter());
            black_box((drawn.is_ok(), bytes.len()))
        });
    });
}

fn benches(c: &mut Criterion) {
    let Some(longest) = Scenario::ALL
        .into_iter()
        .map(|scenario| (scenario, events(scenario)))
        .max_by_key(|(_, events)| events.len())
    else {
        return;
    };
    frame(
        c,
        &format!("{} ({} events)", longest.0.name(), longest.1.len()),
        &longest.1,
    );
    let checking = events(Scenario::Checking);
    frame(
        c,
        &format!("checking ({} events)", checking.len()),
        &checking,
    );
    c.bench_function(
        &format!("project/checking ({} events)", checking.len()),
        |b| {
            b.iter(|| black_box(project(black_box(&checking))));
        },
    );
}

criterion_group!(frames, benches);
criterion_main!(frames);
