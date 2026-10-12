//! One dashboard frame: the header and the stage line over the main view, the help or the journal view, and the
//! key hints under them.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;

use super::dash::{draw_ccd, draw_now, header, stage_line, status_header};
use super::draw::{put, rows};
use super::footer::{footer, skipped_clause};
use super::help::help_body;
use super::layout::{Layout, SizeClass};
use super::measure::measure;
use super::panels::{context_lines, history_panel, log_arrivals, log_body};
use super::scene::Scene;
use super::story::{
    Story, last_lines, narrator_lines, outcome_lines, problem_story, starting_story, story,
};
use crate::model::Model;
use crate::screen::{Drawn, Hold, Journal, Log, Screen, View};

const NO_LOG: Log<'static> = Log {
    entries: &[],
    total: 0,
    arrived: 0,
};

/// Draws one frame of `journal` on `screen` at `now` into `buf`, whose area is `(width - 1) x (height - 1)`, and
/// reports how far the view scrolls and how long the frame stays true. Every time on it is local to `zone`.
///
/// A started idle (R6) trial holds the frame at the trial's start: nothing on it moves with the clock, and the
/// frame is [`Hold::Frozen`] until a later event releases it. Every other frame shows the clock and holds
/// [`Hold::Never`].
pub fn render(
    journal: &Journal<'_>,
    screen: &Screen,
    now: Timestamp,
    zone: &TimeZone,
    buf: &mut Buffer,
) -> Drawn {
    match *journal {
        Journal::Ready { snapshot, log } => {
            let scene = Scene::new(snapshot, now, zone, screen.colours);
            folded(&scene, &log, screen, buf)
        }
        Journal::Missing => {
            let model = Model::new();
            let scene = Scene::new(model.snapshot(), now, zone, screen.colours);
            folded(&scene, &NO_LOG, screen, buf)
        }
        Journal::Starting => unfolded(&starting_story(), "starting", screen, now, zone, buf),
        Journal::Problem(error) => unfolded(
            &problem_story(error, screen.one_frame),
            "can't read journal",
            screen,
            now,
            zone,
            buf,
        ),
    }
}

fn folded(scene: &Scene<'_>, log: &Log<'_>, screen: &Screen, buf: &mut Buffer) -> Drawn {
    let m = measure(scene, screen);
    let layout = &m.layout;
    let compact = layout.class == SizeClass::Compact;
    put(
        buf,
        layout.header,
        0,
        0,
        &header(scene, usize::from(layout.header.width), compact),
    );
    put(
        buf,
        layout.stage,
        0,
        0,
        &stage_line(scene, layout.class, screen.view != View::Main),
    );
    let mut scroll = 0;
    match screen.view {
        View::Help => scroll = help(layout, screen, buf),
        View::Log => {
            let (body, extent) = log_body(
                log,
                usize::from(layout.body.width),
                usize::from(layout.body.height),
                screen.scroll,
                scene.zone,
                screen.colours,
            );
            rows(buf, layout.body, body, screen.colours);
            scroll = extent;
        }
        View::Main if scene.s.session.is_none() => {
            let said = story(scene);
            narrate(&said, layout.body, false, screen, buf);
        }
        View::Main => {
            let said = story(scene);
            narrate(&said, layout.say, compact, screen, buf);
            if !compact {
                rows(buf, layout.last, last_lines(scene), screen.colours);
            }
            draw_now(buf, &m, scene);
            rows(
                buf,
                m.outcomes,
                outcome_lines(scene, usize::from(m.outcomes.width), compact),
                screen.colours,
            );
            for ccd in &m.ccds {
                draw_ccd(buf, ccd, scene);
            }
            rows(buf, m.context, context_lines(scene, &m), screen.colours);
            rows(
                buf,
                m.history,
                history_panel(
                    scene,
                    usize::from(m.history.width),
                    usize::from(m.history.height),
                ),
                screen.colours,
            );
        }
    }
    let mut after = if screen.view == View::Log {
        log_arrivals(log, screen.colours)
    } else {
        Vec::new()
    };
    if let Some(skipped) = scene.s.skipped {
        after.push(skipped_clause(skipped, screen.colours));
    }
    footer(screen, &after, buf);
    Drawn {
        scroll,
        hold: if scene.hold.is_some() {
            Hold::Frozen
        } else {
            Hold::Never
        },
    }
}

fn unfolded(
    said: &Story,
    status: &str,
    screen: &Screen,
    now: Timestamp,
    zone: &TimeZone,
    buf: &mut Buffer,
) -> Drawn {
    let layout = Layout::new(screen);
    put(
        buf,
        layout.header,
        0,
        0,
        &status_header(
            now,
            zone,
            screen.colours,
            status,
            usize::from(layout.header.width),
        ),
    );
    let mut scroll = 0;
    match screen.view {
        View::Help => scroll = help(&layout, screen, buf),
        View::Log => {
            let (body, extent) = log_body(
                &NO_LOG,
                usize::from(layout.body.width),
                usize::from(layout.body.height),
                screen.scroll,
                zone,
                screen.colours,
            );
            rows(buf, layout.body, body, screen.colours);
            scroll = extent;
        }
        View::Main => narrate(said, layout.body, false, screen, buf),
    }
    footer(screen, &[], buf);
    Drawn {
        scroll,
        hold: Hold::Never,
    }
}

fn help(layout: &Layout, screen: &Screen, buf: &mut Buffer) -> usize {
    let (body, extent) = help_body(
        usize::from(layout.body.width),
        usize::from(layout.body.height),
        screen.scroll,
        screen.colours,
    );
    rows(buf, layout.body, body, screen.colours);
    extent
}

fn narrate(
    said: &Story,
    rect: ratatui::layout::Rect,
    brief: bool,
    screen: &Screen,
    buf: &mut Buffer,
) {
    rows(
        buf,
        rect,
        narrator_lines(
            said,
            usize::from(rect.width),
            usize::from(rect.height),
            brief,
            screen.colours,
        ),
        screen.colours,
    );
}
