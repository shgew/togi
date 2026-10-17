//! One dashboard frame: the header and the stage line over a page, and the key hints under it.
//!
//! Every page but Main at the reference size has a sticky title row of its name, its place in the cycle and its
//! neighbours, and a body that scrolls under it. Main is composed in [`main_page`](super::main_page).

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;

use super::dash::{cores_page, header, stage_line, status_header};
use super::draw::{put, rows};
use super::footer::{footer, skipped_clause};
use super::help::help_body;
use super::layout::{Layout, SizeClass};
use super::main_page;
use super::measure::{Main, MainForm};
use super::panels::{history_page, log_arrivals, log_body, page_lines};
use super::scene::Scene;
use super::scroll::scroll_body_with_bar;
use super::story::{Story, narrator_lines, problem_story, starting_story, story};
use super::title::page_title;
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
/// frame is [`Hold::Frozen`] on every page until a later event releases it. Every other frame shows the clock and
/// holds [`Hold::Never`].
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
    let layout = Layout::new(screen);
    let main = Main::new(screen);
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
        &stage_line(
            scene,
            usize::from(layout.stage.width),
            screen.view != View::Main,
        ),
    );
    let main_has_gauges =
        screen.view == View::Main && scene.s.session.is_some() && main.form == MainForm::Gauges;
    if !main_has_gauges {
        title(&layout, screen, buf);
    }
    let mut scroll = 0;
    match screen.view {
        View::Help => scroll = help(&layout, screen, buf),
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
        View::Cores | View::Cycle | View::R7 | View::Hunts | View::Bios | View::History => {
            let width = usize::from(layout.body.width);
            let inner = width.saturating_sub(2).max(1);
            let lines = match screen.view {
                View::Cores => cores_page(scene, inner),
                View::History => history_page(scene, inner),
                view => page_lines(scene, view, inner, layout.class),
            };
            let (body, extent) = scroll_body_with_bar(
                &lines,
                width,
                usize::from(layout.body.height),
                screen.scroll,
                screen.colours,
            );
            rows(buf, layout.body, body, screen.colours);
            scroll = extent;
        }
        View::Main if scene.s.session.is_none() => {
            narrate(&story(scene), &layout, screen, buf);
        }
        View::Main => scroll = main_page::draw(scene, screen, &layout, &main, buf),
    }
    let mut after = if screen.view == View::Log {
        log_arrivals(log, screen.colours)
    } else {
        Vec::new()
    };
    let warned =
        screen.view == View::Main && scene.s.session.is_some() && main_page::has_warning_row(&main);
    if let Some(skipped) = scene.s.skipped
        && !warned
    {
        after.push(skipped_clause(skipped, screen.colours));
    }
    footer(screen, &after, buf);
    #[cfg(test)]
    super::corpus::frame(&layout, &main, scene, buf, screen);
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
    title(&layout, screen, buf);
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
        View::Main
        | View::Cores
        | View::Cycle
        | View::R7
        | View::Hunts
        | View::Bios
        | View::History => narrate(said, &layout, screen, buf),
    }
    footer(screen, &[], buf);
    #[cfg(test)]
    super::corpus::unfolded(&layout, buf, screen);
    Drawn {
        scroll,
        hold: Hold::Never,
    }
}

fn title(layout: &Layout, screen: &Screen, buf: &mut Buffer) {
    put(
        buf,
        layout.title,
        0,
        0,
        &page_title(screen.view, usize::from(layout.title.width), screen.colours),
    );
}

fn help(layout: &Layout, screen: &Screen, buf: &mut Buffer) -> usize {
    let (body, extent) = help_body(
        usize::from(layout.body.width),
        usize::from(layout.body.height),
        screen.scroll,
        screen.colours,
        screen.quit,
    );
    rows(buf, layout.body, body, screen.colours);
    extent
}

fn narrate(said: &Story, layout: &Layout, screen: &Screen, buf: &mut Buffer) {
    rows(
        buf,
        layout.body,
        narrator_lines(
            said,
            usize::from(layout.body.width),
            usize::from(layout.body.height),
            false,
            screen.colours,
        ),
        screen.colours,
    );
}
