use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::text::{Line, Span};

use crate::frames::colours::Paint;
use crate::frames::fit::{Clause, Form, fit_clauses};
use crate::frames::scene::{Scene, ago, between, hm, wall_minute, wall_second};
use crate::screen::Colours;
use crate::words::noun;

const GAP: &str = "   ";

fn head(colours: Colours, zone: &TimeZone, now: Timestamp) -> Line<'static> {
    Line::from(vec![
        Paint::Grey.span(colours, "togi"),
        Span::raw(GAP),
        Paint::White.span(colours, wall_second(zone, now)),
    ])
}

/// The top line of a folded journal in `width` cells: the clock, the session's length, the counts and the last
/// observed failure; `compact` names an idle trial's pause beside the clock.
///
/// Drop order, first to last: how long ago the last failure was and then its time, `session` before the
/// session's length, the words after the failure and crash counts, the pause notice. The clock, the session's
/// length and the counts are never left out.
pub(crate) fn header(scene: &Scene<'_>, width: usize, compact: bool) -> Line<'static> {
    let colours = scene.colours;
    let paint = |p: Paint, text: String| scene.paint(p, text);
    let now = scene.now;
    let head = head(colours, scene.zone, now);
    let Some(session) = scene.s.session else {
        return status_clauses(head, colours, "no session yet", width);
    };
    let end = scene
        .s
        .dead_end
        .map(|end| end.at)
        .or_else(|| scene.s.stopped.map(|stop| stop.at))
        .unwrap_or(now);
    let planned_end = scene.hold.map(|hold| hold.planned_end);
    let mut paused = planned_end.filter(|&until| now < until);
    let paused_beside = compact && planned_end.is_some();
    let mut clauses = vec![Clause::whole("", head)];
    if let (true, Some(until)) = (paused_beside, planned_end) {
        clauses.push(Clause::new(
            GAP,
            vec![Form::new(
                paint(
                    Paint::Hunt,
                    format!("paused until {}", scene.wall_minute(until)),
                ),
                0,
            )],
        ));
        paused = None;
    }
    let hours = hm(between(session.start, end));
    let text = |value: String| paint(Paint::Text, value);
    let grey = |value: String| paint(Paint::Grey, value);
    let failures = scene.s.failures;
    let crashes = scene.s.crashes;
    let count = |value: u32| text(value.to_string());
    clauses.push(Clause::new(
        GAP,
        vec![
            Form::new(
                Line::from(vec![grey("session ".into()), text(hours.clone())]),
                2,
            ),
            Form::new(Line::from(text(hours)), 0),
        ],
    ));
    clauses.push(Clause::new(
        GAP,
        vec![
            Form::new(
                Line::from(vec![
                    count(failures),
                    grey(format!(" {} · ", noun(failures as usize, "failure"))),
                    count(crashes),
                    grey(format!(" {}", noun(crashes as usize, "crash"))),
                ]),
                1,
            ),
            Form::new(
                Line::from(vec![
                    count(failures),
                    grey(" fail · ".into()),
                    count(crashes),
                    grey(" crash".into()),
                ]),
                0,
            ),
        ],
    ));
    if let (Some(last), false) = (scene.s.last_failure, paused_beside) {
        let at = text(scene.wall_minute(last));
        let label = || grey("last observed failure ".into());
        let mut forms = Vec::new();
        if planned_end.is_none() {
            forms.push(Form::new(
                Line::from(vec![
                    label(),
                    at.clone(),
                    grey(format!(", {}", ago(between(last, now)))),
                ]),
                6,
            ));
        }
        forms.push(Form::new(Line::from(vec![label(), at]), 5));
        forms.push(Form::empty());
        clauses.push(Clause::new(GAP, forms));
    }
    if let Some(until) = paused {
        let minute = wall_minute(scene.zone, until);
        clauses.push(Clause::new(
            GAP,
            vec![
                Form::new(
                    paint(Paint::Hunt, format!("screen paused until {minute}")),
                    4,
                ),
                Form::new(paint(Paint::Hunt, format!("paused until {minute}")), 3),
                Form::empty(),
            ],
        ));
    }
    let line = fit_clauses(width, &clauses);
    #[cfg(test)]
    crate::corpus::line("dash.header", if compact { "compact" } else { "full" }, &line);
    line
}

fn status_clauses(
    head: Line<'static>,
    colours: Colours,
    status: &str,
    width: usize,
) -> Line<'static> {
    fit_clauses(
        width,
        &[
            Clause::whole("", head),
            Clause::whole("", Paint::Grey.span(colours, format!("{GAP}{status}"))),
        ],
    )
}

/// The top line of a journal that is not folded: the clock and `status` (`starting`, `can't read journal`).
pub(crate) fn status_header(
    now: Timestamp,
    zone: &TimeZone,
    colours: Colours,
    status: &str,
    width: usize,
) -> Line<'static> {
    status_clauses(head(colours, zone, now), colours, status, width)
}
