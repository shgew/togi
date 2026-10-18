use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::at;
use crate::frames::colours::Paint;
use crate::frames::draw::{put, put_cells};
use crate::frames::fit::{Clause, Form, fit_clauses, line_width, rule};
use crate::frames::layout::SizeClass;
use crate::frames::measure::Measure;
use crate::frames::scene::{Scene, between, clock};
use crate::frames::story::{operation, resting_band, trial_where};
use crate::model::TrialView;
use crate::words::{cores_text, load_words, workload_display};

const DIGITS: [[&str; 3]; 11] = [
    ["█▀█", "█ █", "▀▀▀"],
    ["▀█ ", " █ ", "▀▀▀"],
    ["▀▀█", "█▀▀", "▀▀▀"],
    ["▀▀█", " ▀█", "▀▀▀"],
    ["█ █", "▀▀█", "  ▀"],
    ["█▀▀", "▀▀█", "▀▀▀"],
    ["█▀▀", "█▀█", "▀▀▀"],
    ["▀▀█", "  █", "  ▀"],
    ["█▀█", "█▀█", "▀▀▀"],
    ["█▀█", "▀▀█", "▀▀▀"],
    [" ", "▀", "▀"],
];

/// The three rows of `text` in large digits; characters other than digits and `:` are skipped.
pub(super) fn big_digits(text: &str) -> [String; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, c) in text.chars().enumerate() {
        let glyph = match c {
            ':' => 10,
            '0'..='9' => usize::from(u8::try_from(c).unwrap_or(b'0') - b'0'),
            _ => continue,
        };
        for (row, part) in rows.iter_mut().zip(DIGITS[glyph]) {
            if i > 0 {
                row.push(' ');
            }
            row.push_str(part);
        }
    }
    rows
}

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

/// `1 passed · trial 2 of 4  ■ ► · ·`, or its short form.
pub(super) fn requirement_line(
    scene: &Scene<'_>,
    trial: &TrialView,
    compact: bool,
) -> Line<'static> {
    let count = trial.count;
    if count.of == 0 {
        return Line::default();
    }
    let label = if compact {
        format!("{}/{} ", count.passed, count.of)
    } else {
        format!(
            "{} passed · trial {} of {}  ",
            count.passed, count.index, count.of
        )
    };
    let mut spans = vec![scene.paint(Paint::Text, label)];
    for i in 0..count.of {
        spans.push(if i < count.passed {
            scene.paint(Paint::Good, "■")
        } else if i + 1 == count.index {
            scene.paint(Paint::Running, "►")
        } else {
            scene.paint(Paint::Grey, "·")
        });
        if i + 1 < count.of {
            spans.push(Span::raw(" "));
        }
    }
    Line::from(spans)
}

fn draw_resting(buf: &mut Buffer, r: Rect, class: SizeClass, scene: &Scene<'_>) {
    let band = resting_band(scene);
    let title = Line::from(scene.paint(band.paint, band.title));
    let text = |line: &String| Line::from(scene.paint(Paint::Text, line.clone()));
    if class == SizeClass::Compact {
        let mut first = title.spans;
        first.push(Span::raw("  "));
        if let Some(line) = band.lines.first() {
            first.extend(text(line).spans);
        }
        put(buf, r, 0, 0, &Line::from(first));
        for (i, line) in band.lines.iter().enumerate().skip(1) {
            put(buf, r, 0, at(i), &text(line));
        }
        return;
    }
    let rule = rule(
        usize::from(r.width),
        &title,
        &Line::default(),
        scene.style(Paint::Track),
    );
    put(buf, r, 0, 0, &rule);
    for (i, line) in band.lines.iter().enumerate() {
        put(buf, r, 0, at(i + 1), &text(line));
    }
}

fn description(scene: &Scene<'_>, trial: &TrialView) -> Vec<Clause> {
    let leave = |sep: &'static str, line: Line<'static>, rank: u8| {
        Clause::new(sep, vec![Form::new(line, rank), Form::empty()])
    };
    let paint = |p: Paint, text: String| Line::from(scene.paint(p, text));
    if scene.hold.is_some() {
        return vec![
            Clause::whole("", paint(Paint::Chip, " R6 ".into())),
            leave("  ", paint(Paint::White, "idle + bursts".into()), 2),
            Clause::whole(
                "  ",
                paint(
                    Paint::Text,
                    format!("on {}", cores_text(trial.cores, scene.s.cores.len())),
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
        Clause::whole("", paint(Paint::Chip, format!(" {} ", trial.regime))),
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

/// Draws the trial band into `measure.now`: the trial in flight with its bar, or the resting band.
pub(crate) fn draw_now(buf: &mut Buffer, measure: &Measure, scene: &Scene<'_>) {
    let r = measure.now;
    let class = measure.layout.class;
    let s = &scene.s;
    let trial = match s.trial {
        Some(trial)
            if s.dead_end.is_none()
                && s.stopped.is_none()
                && s.recover.is_none()
                && !trial.crashed =>
        {
            trial
        }
        _ => return draw_resting(buf, r, class, scene),
    };
    let width = usize::from(r.width);
    let paint = |p: Paint, text: String| scene.paint(p, text);
    let op = operation(scene, trial);
    if class != SizeClass::Compact {
        let title = Line::from(paint(Paint::Grey, "NOW".into()));
        let right = Line::from(paint(Paint::Grey, format!("trial {}", trial.id.as_str())));
        put(
            buf,
            r,
            0,
            0,
            &rule(width, &title, &right, scene.style(Paint::Track)),
        );
    }
    let desc_y = u16::from(class != SizeClass::Compact);
    let op_width = line_width(&op);
    put(
        buf,
        r,
        0,
        desc_y,
        &fit_clauses(
            width.saturating_sub(op_width + 3),
            &description(scene, trial),
        ),
    );
    put(buf, r, at(width.saturating_sub(op_width)), desc_y, &op);
    let last = r.height.saturating_sub(1);
    let Some(started) = trial.started else {
        let planned = format!(
            "preparing · workload has not started yet · planned {}",
            clock(trial.duration)
        );
        put(buf, r, 0, last, &Line::from(paint(Paint::Grey, planned)));
        return;
    };
    let quiet = scene.hold;
    let elapsed = between(started, scene.now).min(trial.duration);
    let remaining = trial.duration.saturating_sub(elapsed);
    let past = trial.past_end(scene.now);
    let done = if quiet.is_some() {
        Duration::ZERO
    } else {
        elapsed
    };
    if class == SizeClass::Wide {
        let cells = width.saturating_sub(23);
        let n = filled(cells, done, trial.duration);
        let (mut digits, mut style, mut label) = (clock(remaining), Paint::White, "left");
        if !past.is_zero() {
            (digits, style, label) = (clock(past), Paint::Hunt, "past end");
        }
        if let Some(hold) = quiet {
            (digits, style) = (scene.wall_minute(hold.planned_end), Paint::Grey);
        }
        for y in 3..=5 {
            let (done_glyph, rest) = if y == 5 {
                ("▀", "▀")
            } else {
                ("█", "░")
            };
            put_cells(
                buf,
                r,
                0,
                y,
                &progress_bar(scene, cells, n, done_glyph, rest),
            );
        }
        for (y, row) in big_digits(&digits).into_iter().enumerate() {
            let x = width.saturating_sub(row.chars().count());
            put_cells(buf, r, at(x), 3 + at(y), &Line::from(paint(style, row)));
        }
        if let Some(hold) = quiet {
            let msg = format!(
                "  screen paused · planned end {digits}, so it cannot wake the idle cores  "
            );
            let x = cells.saturating_sub(msg.chars().count()) / 2;
            put(buf, r, at(x), 4, &Line::from(paint(Paint::Text, msg)));
            let begun = Line::from(vec![
                paint(Paint::Grey, "started ".into()),
                paint(Paint::Text, scene.wall_minute(hold.started)),
                paint(Paint::Grey, format!(" · {}", clock(trial.duration))),
            ]);
            put(buf, r, 0, 6, &begun);
            put(
                buf,
                r,
                at(width.saturating_sub(7)),
                6,
                &Line::from(paint(Paint::Grey, "ends at".into())),
            );
        } else {
            let progress = Line::from(vec![
                paint(Paint::Text, clock(elapsed)),
                paint(Paint::Grey, format!(" of {}", clock(trial.duration))),
            ]);
            put(buf, r, 0, 6, &progress);
            let trials = requirement_line(scene, trial, false);
            put(
                buf,
                r,
                at(cells.saturating_sub(line_width(&trials))),
                6,
                &trials,
            );
            put(
                buf,
                r,
                at(width.saturating_sub(label.len())),
                6,
                &Line::from(paint(Paint::Grey, label.into())),
            );
        }
        return;
    }
    let requirement = requirement_line(scene, trial, true);
    let mut label = vec![
        paint(Paint::White, clock(remaining)),
        paint(Paint::Grey, " left".into()),
        Span::raw("  "),
    ];
    label.extend(requirement.spans.iter().cloned());
    if !past.is_zero() {
        label = vec![
            paint(Paint::Hunt, format!("{} past its end", clock(past))),
            Span::raw("  "),
        ];
        label.extend(requirement.spans);
    }
    if let Some(hold) = quiet {
        label = vec![
            paint(Paint::Grey, "screen paused · planned end ".into()),
            paint(Paint::Text, scene.wall_minute(hold.planned_end)),
        ];
    }
    let label = Line::from(label);
    let cells = width.saturating_sub(line_width(&label) + 2);
    let mut bar = progress_bar(scene, cells, filled(cells, done, trial.duration), "█", "░").spans;
    bar.push(Span::raw("  "));
    bar.extend(label.spans);
    put_cells(buf, r, 0, last, &Line::from(bar));
}
