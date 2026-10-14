//! The frame side of the narration corpus (#904): what each region of a drawn frame shows, and the sweep that
//! draws every journal state the simulated sessions pass through.
//!
//! The sweep folds every prefix of every [`Base`] journal and of the cycle a rerun completes, and draws each state at
//! the three main sizes. A state with a trial in flight is also drawn at the trial's planned end and at 90 seconds
//! past it, except an idle (R6) trial, which holds the frame frozen from its start: that one is drawn once and
//! must report [`Hold::Frozen`]. The 18 [`Scenario`] cuts, the journal states and the help and journal views are
//! drawn as whole frames too.

use std::io;

use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use togi_journal::Event;
use togi_watchtest::{Base, Scenario, cycle_completed_by_rerun};

use super::layout::Layout;
use super::measure::Measure;
use super::scene::Scene;
use super::testing::{FIXED_NOW, ready, render_checked, screen, text_snapshot};
use crate::corpus::{case, keep_frame, region};
use crate::model::{Model, Snapshot, project};
use crate::screen::{Colours, Hold, Journal, Screen, View};

/// The three main sizes, wide to compact.
const SIZES: [(u16, u16); 3] = [(240, 67), (160, 45), (120, 33)];

/// The sizes of the help and journal views.
const VIEW_SIZES: [(u16, u16); 3] = [(80, 24), (160, 45), (240, 67)];

/// The widest help view that has one column: the wider ones set three columns side by side, whose rows would
/// join phrases of unrelated sections.
const THREE_COLUMNS: u16 = 195;

/// The scenario cuts whose whole frame the document shows, at the wide size.
const FRAMES: [Scenario; 6] = [
    Scenario::Checking,
    Scenario::MemberProbe,
    Scenario::Idle,
    Scenario::Recovering,
    Scenario::Deadend,
    Scenario::Stopped,
];

/// What the sweep drew.
#[derive(Debug, Default)]
pub(crate) struct Swept {
    /// Each journal folded event by event, with its events and the frames drawn from its prefixes.
    pub(crate) journals: Vec<(String, usize, usize)>,
    /// The scenarios drawn as whole frames at the main sizes.
    pub(crate) scenarios: usize,
    /// Every frame drawn.
    pub(crate) frames: usize,
}

/// Records each region of the frame drawn from `scene` into `buf`, as the reader sees it at its size class.
pub(super) fn frame(m: &Measure, scene: &Scene<'_>, buf: &Buffer, screen: &Screen) {
    if !crate::corpus::collecting() {
        return;
    }
    let layout = &m.layout;
    let form = format!("{:?}", layout.class);
    region("frame.header", &form, buf, layout.header);
    region("frame.footer", &form, buf, layout.footer);
    match screen.view {
        View::Help if layout.body.width < THREE_COLUMNS => {
            region("frame.help", &form, buf, layout.body);
        }
        View::Help => {}
        View::Log => region("frame.log", &form, buf, layout.body),
        View::Main if scene.s.session.is_none() => region("frame.body", &form, buf, layout.body),
        View::Main => {
            region("frame.stage", &form, buf, layout.stage);
            region("frame.say", &form, buf, layout.say);
            region("frame.last", &form, buf, layout.last);
            region("frame.now", &form, buf, m.now);
            region("frame.outcomes", &form, buf, m.outcomes);
            for ccd in &m.ccds {
                region("frame.ccd", &form, buf, ccd.rect);
            }
            region("frame.context", &form, buf, m.context);
            region("frame.history", &form, buf, m.history);
        }
    }
}

/// Records the regions of a frame that has no journal to fold: the header and the body.
pub(super) fn unfolded(layout: &Layout, buf: &Buffer, screen: &Screen) {
    if !crate::corpus::collecting() {
        return;
    }
    let form = format!("{:?}", layout.class);
    region("frame.header", &form, buf, layout.header);
    region("frame.footer", &form, buf, layout.footer);
    match screen.view {
        View::Main => region("frame.body", &form, buf, layout.body),
        View::Log => region("frame.log", &form, buf, layout.body),
        View::Help if layout.body.width < THREE_COLUMNS => {
            region("frame.help", &form, buf, layout.body);
        }
        View::Help => {}
    }
}

fn screen_of(view: View, (width, height): (u16, u16), one_frame: bool) -> Screen {
    Screen {
        one_frame,
        ..screen(view, width, height, Colours::None, !one_frame)
    }
}

/// Draws `journal` as the case `id`, counting the frame; returns the buffer and how long it stays true.
fn draw(
    swept: &mut Swept,
    id: &str,
    journal: &Journal<'_>,
    sc: &Screen,
    now: Timestamp,
) -> (Buffer, Hold) {
    swept.frames += 1;
    let (buf, drawn) = case(id.to_owned(), || render_checked(journal, sc, now));
    (buf, drawn.hold)
}

/// The moments to draw a state at: its last event, 40 seconds after, and when a trial runs that nothing holds, its
/// planned end and 90 seconds past it.
fn clocks(snapshot: &Snapshot<'_>, at: Timestamp) -> Vec<(&'static str, Timestamp)> {
    let mut out = vec![("event", at), ("plus40", at + SignedDuration::from_secs(40))];
    if snapshot.idle_hold().is_none()
        && let Some(trial) = snapshot.trial
        && let Some(started) = trial.started
        && let Ok(length) = SignedDuration::try_from(trial.duration)
        && !length.is_zero()
    {
        let end = started + length;
        out.push(("planned", end));
        out.push(("overdue", end + SignedDuration::from_secs(90)));
    }
    out
}

/// Folds `events` one at a time and draws every prefix.
fn sweep_journal(swept: &mut Swept, name: &str, events: &[Event]) {
    let before = swept.frames;
    let mut model = Model::new();
    for (i, event) in events.iter().enumerate() {
        model
            .fold(event)
            .unwrap_or_else(|error| panic!("{name}: event {i} does not fold: {error}"));
        let journal = ready(&model);
        let Journal::Ready { snapshot, .. } = &journal else {
            unreachable!("`ready` gives a ready journal")
        };
        let held = snapshot.idle_hold().is_some();
        for (clock, now) in clocks(snapshot, event.time) {
            for (n, &size) in SIZES.iter().enumerate() {
                if n > 0 && clock != "plus40" && clock != "overdue" {
                    continue;
                }
                let id = format!(
                    "sweep/{name}/{i:04}-{}/{}x{}/{clock}",
                    event.kind().as_str(),
                    size.0,
                    size.1
                );
                let sc = screen_of(View::Main, size, false);
                let (_, hold) = draw(swept, &id, &journal, &sc, now);
                if held {
                    assert_eq!(
                        hold,
                        Hold::Frozen,
                        "{id}: a started idle trial holds the frame"
                    );
                }
            }
        }
    }
    swept
        .journals
        .push((name.to_owned(), events.len(), swept.frames - before));
}

/// Draws a finished journal as the case `id` at every main size, keeping the whole frame of the sizes `keep`
/// names.
fn whole(swept: &mut Swept, id: &str, events: &[Event], keep: &[(u16, u16)]) {
    let model = project(events);
    let journal = ready(&model);
    let last = events.last().expect("a journal with events").time;
    let now = last + SignedDuration::from_secs(40);
    for size in SIZES {
        let name = format!("{id}/{}x{}", size.0, size.1);
        let (buf, _) = draw(swept, &name, &journal, &screen_of(View::Main, size, false), now);
        if keep.contains(&size) {
            keep_frame(&name, text_snapshot(&buf));
        }
    }
}

/// The journal states that fold nothing, with and without a one-frame print, and the views that do not depend on
/// the journal.
fn states(swept: &mut Swept) {
    let unreadable = io::Error::other("events.jsonl: unexpected end of file");
    for size in SIZES {
        for (name, journal) in [
            ("missing", Journal::Missing),
            ("starting", Journal::Starting),
            ("problem", Journal::Problem(&unreadable)),
        ] {
            for once in [false, true] {
                let id = format!(
                    "journal/{name}{}/{}x{}",
                    if once { "-once" } else { "" },
                    size.0,
                    size.1
                );
                let (buf, _) = draw(
                    swept,
                    &id,
                    &journal,
                    &screen_of(View::Main, size, once),
                    FIXED_NOW,
                );
                if size == SIZES[0] {
                    keep_frame(&id, text_snapshot(&buf));
                }
            }
        }
    }
    let model = Model::new();
    for size in VIEW_SIZES {
        let id = format!("journal/help/{}x{}", size.0, size.1);
        let (buf, _) = draw(
            swept,
            &id,
            &ready(&model),
            &screen_of(View::Help, size, false),
            FIXED_NOW,
        );
        if size == VIEW_SIZES[0] {
            keep_frame(&id, text_snapshot(&buf));
        }
        let id = format!("journal/empty-log/{}x{}", size.0, size.1);
        draw(
            swept,
            &id,
            &ready(&model),
            &screen_of(View::Log, size, false),
            FIXED_NOW,
        );
    }
}

/// A finished journal's log view at the view sizes, at the top and at the end, and with a skipped-content report
/// in the footer at the three widths its forms take.
fn views(swept: &mut Swept, name: &str, events: &[Event]) {
    let mut model = project(events);
    let last = events.last().expect("a journal with events").time;
    let now = last + SignedDuration::from_secs(40);
    for size in VIEW_SIZES {
        for (end, scroll) in [("top", 0), ("end", usize::MAX)] {
            let id = format!("sweep/{name}/log-{end}/{}x{}", size.0, size.1);
            let sc = Screen {
                scroll,
                ..screen_of(View::Log, size, false)
            };
            draw(swept, &id, &ready(&model), &sc, now);
        }
    }
    model.set_skipped(Some("2 lines skipped (unknown event kind)".to_owned()));
    for size in [(120, 33), (60, 30), (40, 30), (28, 30), (20, 30)] {
        let id = format!("sweep/{name}/skipped/{}x{}", size.0, size.1);
        draw(
            swept,
            &id,
            &ready(&model),
            &screen_of(View::Main, size, false),
            now,
        );
    }
}

/// Draws everything the corpus draws, on the calling thread, which collects.
pub(crate) fn run() -> Swept {
    let mut swept = Swept::default();
    for base in Base::ALL {
        let name = format!("{base:?}").to_lowercase();
        let events = base
            .events()
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        sweep_journal(&mut swept, &name, events);
        views(&mut swept, &name, events);
    }
    let rerun = cycle_completed_by_rerun().unwrap_or_else(|error| panic!("rerun journals: {error}"));
    sweep_journal(&mut swept, "rerun-completes-cycle", &rerun.ended);
    for (n, running) in rerun.running.iter().enumerate() {
        whole(&mut swept, &format!("scenario/rerun-{n}"), running, &[]);
    }
    whole(&mut swept, "scenario/rerun-passed", &rerun.passed, &[]);
    for scenario in Scenario::ALL {
        let events = scenario
            .events()
            .unwrap_or_else(|error| panic!("{}: {error}", scenario.name()));
        let keep: &[(u16, u16)] = if FRAMES.contains(&scenario) {
            &SIZES[..1]
        } else {
            &[]
        };
        whole(
            &mut swept,
            &format!("scenario/{}", scenario.name()),
            &events,
            keep,
        );
        swept.scenarios += 1;
    }
    states(&mut swept);
    swept
}
