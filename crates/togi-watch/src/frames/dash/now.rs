//! Main's `NOW` rows: one operation sentence, and under it the trial bar or the status that replaces it.

use std::time::Duration;

use ratatui::text::{Line, Span};

use crate::frames::colours::Paint;
use crate::frames::fit::{Clause, Form, fit_clauses, line_width, truncate_styled};
use crate::frames::scene::{Scene, between, clock};
use crate::frames::story::{operation, resting_band, trial_where};
use crate::model::TrialView;
use crate::words::{cores_text, load_words, workload_display};

/// The cells of the trial bar, whatever the width and the trial's length.
const BAR_CELLS: usize = 24;

/// The cells of `NOW` and the gap after it.
const LABEL_CELLS: usize = 5;

/// The cells of a `cells`-wide bar `elapsed` of `total` has filled, rounded down.
pub(super) fn filled(cells: usize, elapsed: Duration, total: Duration) -> usize {
    if total.is_zero() {
        return 0;
    }
    let done = cells as u128 * elapsed.as_nanos().min(total.as_nanos()) / total.as_nanos();
    usize::try_from(done).unwrap_or(cells)
}

/// The elapsed fraction as `done` cells and the rest as `rest` cells, so the shape shows elapsed time without
/// colour.
pub(super) fn progress_bar(
    scene: &Scene<'_>,
    cells: usize,
    done_cells: usize,
    done: &str,
    rest: &str,
) -> Line<'static> {
    let n = done_cells.min(cells);
    Line::from(vec![
        scene.paint(Paint::Running, done.repeat(n)),
        scene.paint(Paint::Track, rest.repeat(cells - n)),
    ])
}

fn description(scene: &Scene<'_>, trial: &TrialView) -> Vec<Clause> {
    let leave = |sep: &'static str, line: Line<'static>, rank: u8| {
        Clause::new(sep, vec![Form::new(line, rank), Form::empty()])
    };
    let paint = |p: Paint, text: String| Line::from(scene.paint(p, text));
    if scene.hold.is_some() {
        let cores: Vec<_> = trial.cores.iter().collect();
        return vec![
            Clause::whole("  ", paint(Paint::Chip, " R6 ".into())),
            leave("  ", paint(Paint::White, "idle + bursts".into()), 2),
            Clause::whole(
                "  ",
                paint(
                    Paint::Text,
                    format!("on {}", cores_text(&cores, scene.s.cores.len())),
                ),
            ),
            leave(
                "   ",
                paint(Paint::Grey, "short wake-ups, no workers in between".into()),
                3,
            ),
        ];
    }
    let mut out = vec![
        Clause::whole("  ", paint(Paint::Chip, format!(" {} ", trial.regime))),
        leave(
            "  ",
            paint(Paint::White, load_words(trial.regime).into()),
            2,
        ),
        Clause::whole("  ", paint(Paint::Text, trial_where(scene, trial))),
        leave(
            "   ",
            paint(Paint::Grey, workload_display(trial.workload).into()),
            3,
        ),
    ];
    let workers = trial.cores.len() * usize::from(trial.workload.workload().threads);
    if workers > 0 {
        let suffix = if workers == 1 { "" } else { "s" };
        out.push(leave(
            "",
            paint(Paint::Grey, format!(" · {workers} worker{suffix}")),
            4,
        ));
    }
    out
}

/// The trial the session runs now: none once it stopped, met a dead end, recovered from a crash or lost the
/// trial to one.
fn running_trial<'a>(scene: &Scene<'a>) -> Option<&'a TrialView> {
    let s = &scene.s;
    s.trial.filter(|trial| {
        s.dead_end.is_none() && s.stopped.is_none() && s.recover.is_none() && !trial.crashed
    })
}

fn label(scene: &Scene<'_>, rest: Line<'static>) -> Line<'static> {
    let mut spans = vec![scene.paint(Paint::Grey, "NOW"), Span::raw("  ")];
    spans.extend(rest.spans);
    Line::from(spans)
}

fn text(scene: &Scene<'_>, paint: Paint, text: impl Into<String>) -> Line<'static> {
    Line::from(scene.paint(paint, text.into()))
}

/// The operation in one sentence: what runs, its load and where.
fn sentence(scene: &Scene<'_>, trial: &TrialView, width: usize) -> Line<'static> {
    let mut clauses = vec![Clause::whole("", operation(scene, trial))];
    clauses.extend(description(scene, trial));
    label(
        scene,
        fit_clauses(width.saturating_sub(LABEL_CELLS), &clauses),
    )
}

/// The bar under the sentence: elapsed over required, a frozen R6's planned end, or why there is no bar yet.
fn trial_row(scene: &Scene<'_>, trial: &TrialView, width: usize) -> Line<'static> {
    let Some(started) = trial.started else {
        let planned = format!(
            "preparing · workload has not started yet · planned {}",
            clock(trial.duration)
        );
        return fit_clauses(
            width,
            &[Clause::whole("", text(scene, Paint::Grey, planned))],
        );
    };
    let past = trial.past_end(scene.now);
    let (done, mut clauses) = if let Some(hold) = scene.hold {
        (
            0,
            vec![
                Clause::whole(
                    "",
                    text(
                        scene,
                        Paint::Text,
                        format!("planned end {}", scene.wall_minute(hold.planned_end)),
                    ),
                ),
                Clause::new(
                    " · ",
                    vec![
                        Form::new(
                            text(
                                scene,
                                Paint::Grey,
                                format!("started {}", scene.wall_minute(started)),
                            ),
                            3,
                        ),
                        Form::empty(),
                    ],
                ),
                Clause::new(
                    " · ",
                    vec![
                        Form::new(
                            text(
                                scene,
                                Paint::Grey,
                                format!("{} required", clock(trial.duration)),
                            ),
                            2,
                        ),
                        Form::empty(),
                    ],
                ),
                Clause::new(
                    " · ",
                    vec![
                        Form::new(text(scene, Paint::Grey, "screen paused"), 1),
                        Form::empty(),
                    ],
                ),
            ],
        )
    } else {
        let elapsed = between(started, scene.now).min(trial.duration);
        let mut clauses = vec![Clause::whole(
            "",
            Line::from(vec![
                scene.paint(Paint::Text, clock(elapsed)),
                scene.paint(Paint::Grey, format!(" / {}", clock(trial.duration))),
            ]),
        )];
        if !past.is_zero() {
            clauses.push(Clause::new(
                " · ",
                vec![
                    Form::new(
                        text(scene, Paint::Hunt, format!("{} past its end", clock(past))),
                        2,
                    ),
                    Form::new(
                        text(scene, Paint::Hunt, format!("{} past end", clock(past))),
                        0,
                    ),
                ],
            ));
            clauses.push(Clause::new(
                " · ",
                vec![
                    Form::new(text(scene, Paint::Grey, "no trial end recorded"), 3),
                    Form::empty(),
                ],
            ));
            clauses.push(Clause::new(
                " · ",
                vec![
                    Form::new(text(scene, Paint::Hunt, "may still be running"), 1),
                    Form::empty(),
                ],
            ));
        }
        (filled(BAR_CELLS, elapsed, trial.duration), clauses)
    };
    if scene.hold.is_none() && trial.count.of > 1 {
        clauses.push(Clause::new(
            " · ",
            vec![
                Form::new(
                    text(
                        scene,
                        Paint::Grey,
                        format!("trial {} of {}", trial.count.index, trial.count.of),
                    ),
                    1,
                ),
                Form::empty(),
            ],
        ));
    }
    let bar = progress_bar(scene, BAR_CELLS, done, "█", "░");
    if width <= BAR_CELLS {
        return truncate_styled(&bar, width);
    }
    let gap = if width > BAR_CELLS + 2 { "  " } else { " " };
    let rest = fit_clauses(width - BAR_CELLS - gap.len(), &clauses);
    let mut spans = bar.spans;
    if line_width(&rest) > 0 {
        spans.push(Span::raw(gap));
        spans.extend(rest.spans);
    }
    Line::from(spans)
}

/// The band that replaces the sentence and the bar between trials, after a stop, at a dead end and around a
/// crash: its title and first line in place of the operation, the rest under it.
fn resting(scene: &Scene<'_>, width: usize) -> [Line<'static>; 2] {
    let band = resting_band(scene);
    let room = width.saturating_sub(LABEL_CELLS);
    let mut lines = band.lines.iter();
    let mut head = vec![Clause::whole("", text(scene, band.paint, band.title))];
    if let Some(first) = lines.next() {
        head.push(Clause::new(
            " · ",
            vec![
                Form::new(text(scene, Paint::Text, first.clone()), 1),
                Form::empty(),
            ],
        ));
    }
    let kept = band.lines.len().saturating_sub(1);
    let rest: Vec<Clause> = lines
        .enumerate()
        .map(|(i, line)| {
            let rank = u8::try_from(kept - i).unwrap_or(u8::MAX);
            Clause::new(
                " · ",
                vec![
                    Form::new(text(scene, Paint::Grey, line.clone()), rank),
                    Form::empty(),
                ],
            )
        })
        .collect();
    [
        label(scene, fit_clauses(room, &head)),
        fit_clauses(width, &rest),
    ]
}

/// Main's NOW sentence and the trial row under it (bar with elapsed and required, R6's planned end, or the
/// status that replaces them), each fitted to `width` cells.
pub(crate) fn now_lines(scene: &Scene<'_>, width: usize) -> [Line<'static>; 2] {
    if scene.s.session.is_none() {
        return [Line::default(), Line::default()];
    }
    match running_trial(scene) {
        Some(trial) => [
            sentence(scene, trial, width),
            trial_row(scene, trial, width),
        ],
        None => resting(scene, width),
    }
}

#[cfg(test)]
mod tests;
