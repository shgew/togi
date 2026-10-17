//! What happened: Main's folded summary and the History page.
//!
//! Routine progress folds into one summary row per burst; everything else stays an entry of its own. The fold is
//! a presentation over the retained entries, which the History page lists unfolded.

use jiff::Timestamp;
use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::{
    Clause, Form, cut_styled, display_width, fit_clauses, line_width, rule, wrap_line, wrap_styled,
};
use super::super::scene::Scene;
use super::table::{cell, wall_date};
use crate::history::{Entry, Failed, What};
use crate::screen::View;
use crate::words::{Tag, Tone, plural};

/// The cells of a row before its sentence: the time, a gap, the tag column and a gap.
const PREFIX: usize = 16;

const NOTHING_YET: &str = "nothing has happened yet";

/// What kind of routine progress an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Routine {
    Pass,
    Chain,
    Step,
    Round,
}

/// Whether `entry` is routine progress that folds, and which kind. A step marker folds; an offset decision
/// with the same tag does not. A round folds only when it passed.
fn routine(entry: &Entry) -> Option<Routine> {
    match entry.tag {
        Tag::Pass => Some(Routine::Pass),
        Tag::Chain => Some(Routine::Chain),
        Tag::Step if matches!(entry.what, What::Step(_)) => Some(Routine::Step),
        Tag::Round if entry.tone == Tone::Good => Some(Routine::Round),
        Tag::Step
        | Tag::Round
        | Tag::Start
        | Tag::Fail
        | Tag::Crash
        | Tag::Unclear
        | Tag::Skip
        | Tag::Reset
        | Tag::Limit
        | Tag::Room
        | Tag::Backoff
        | Tag::Cycle
        | Tag::Hunt
        | Tag::Combo
        | Tag::Stop
        | Tag::Mce
        | Tag::Note
        | Tag::Group
        | Tag::Probe
        | Tag::Confirm
        | Tag::Resume
        | Tag::Hold => None,
    }
}

/// A failure the fold keeps in view whatever else happens after it.
fn is_failure(entry: &Entry) -> bool {
    matches!(entry.tag, Tag::Fail | Tag::Crash) || entry.tone == Tone::Bad
}

/// An entry worth a second row when its sentence does not fit one.
fn is_important(entry: &Entry) -> bool {
    matches!(entry.what, What::Failed(_)) || matches!(entry.tone, Tone::Bad | Tone::Warn)
}

/// Consecutive routine entries, folded.
#[derive(Default)]
struct Burst {
    at: Option<Timestamp>,
    /// The cycle and step the burst belongs to, from its step marker or the step its passes ran in.
    place: Option<(u32, u32)>,
    step: Option<String>,
    credited: u32,
    chains: usize,
    chain: Option<String>,
    round: Option<String>,
}

impl Burst {
    /// Whether a pass in `place` belongs to another step than what the burst holds. A burst of passes outside
    /// any step never takes a step's passes, nor the other way round.
    fn is_elsewhere(&self, place: Option<(u32, u32)>) -> bool {
        self.at.is_some() && self.place != place
    }

    fn add(&mut self, kind: Routine, entry: &Entry) {
        self.at = Some(entry.at);
        match kind {
            Routine::Pass => {
                if let What::Passes(passes) = &entry.what {
                    self.credited += if passes.of > 0 {
                        passes.runs.min(passes.of)
                    } else {
                        passes.runs
                    };
                }
                if let Some(at) = entry.step() {
                    self.place = Some((at.cycle, at.step));
                    self.step.get_or_insert_with(|| at.label());
                }
            }
            Routine::Chain => {
                self.chains += 1;
                self.chain = Some(entry.text());
            }
            Routine::Step => {
                if let Some(at) = entry.step() {
                    self.place = Some((at.cycle, at.step));
                }
                self.step = Some(entry.text());
            }
            Routine::Round => self.round = Some(entry.text()),
        }
    }

    /// The summary row of what was added since the last `take`, or `None` when nothing was.
    fn take(&mut self) -> Option<Entry> {
        let burst = std::mem::take(self);
        let at = burst.at?;
        let credited =
            (burst.credited > 0).then(|| plural(burst.credited as usize, "credited check"));
        let chain = match (burst.chains, burst.chain) {
            (0, _) | (_, None) => None,
            (1, Some(text)) => Some(text),
            (many, Some(_)) => Some(plural(many, "chain note")),
        };
        let (tag, tone) = if burst.round.is_some() {
            (Tag::Round, Tone::Good)
        } else if burst.step.is_some() {
            (Tag::Step, Tone::Plain)
        } else if credited.is_some() {
            (Tag::Pass, Tone::Plain)
        } else {
            (Tag::Chain, Tone::Plain)
        };
        let text = [burst.round, burst.step, credited, chain]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        Some(Entry::line(at, tag, text, tone))
    }
}

/// `entries`, newest first, with every run of routine progress folded into one summary per step or round: a step
/// marker, or a pass in another step than the burst's, opens a new summary, and a round's completion closes its
/// own, so each summary names only its own credited checks.
fn fold(entries: &[Entry]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut burst = Burst::default();
    for entry in entries.iter().rev() {
        match routine(entry) {
            Some(Routine::Step) => {
                out.extend(burst.take());
                burst.add(Routine::Step, entry);
            }
            Some(Routine::Round) => {
                burst.add(Routine::Round, entry);
                out.extend(burst.take());
            }
            Some(Routine::Pass) => {
                if burst.is_elsewhere(entry.step().map(|at| (at.cycle, at.step))) {
                    out.extend(burst.take());
                }
                burst.add(Routine::Pass, entry);
            }
            Some(Routine::Chain) => burst.add(Routine::Chain, entry),
            None => {
                out.extend(burst.take());
                out.push(entry.clone());
            }
        }
    }
    out.extend(burst.take());
    out.reverse();
    out
}

fn prefix(scene: &Scene<'_>, entry: &Entry) -> Vec<Span<'static>> {
    vec![
        scene.paint(Paint::Grey, scene.wall_minute(entry.at)),
        Span::raw("  "),
        scene.paint(Paint::from(entry.tone), format!("{:<8}", entry.tag.word())),
        Span::raw(" "),
    ]
}

/// A list `head: a, b, c, d` in `room` cells: as many whole items as fit, then `and N more`. `None` when the
/// entry is no list or not even its first item fits.
fn summarised(entry: &Entry, room: usize) -> Option<String> {
    let What::List { head, items } = &entry.what else {
        return None;
    };
    (1..items.len()).rev().find_map(|kept| {
        let more = items.len() - kept;
        let short = format!("{head}: {} and {more} more", items[..kept].join(", "));
        (display_width(&short) <= room).then_some(short)
    })
}

/// The sentence with its alarm words drawn as an alarm.
fn emphasised(
    scene: &Scene<'_>,
    (before, alarm, after): (String, String, String),
) -> Line<'static> {
    Line::from(vec![
        scene.paint(Paint::Text, before),
        scene.paint(Paint::Bad, alarm),
        scene.paint(Paint::Text, after),
    ])
}

/// One row: the time, the tag and the sentence cut to its whole words.
fn row(scene: &Scene<'_>, entry: &Entry, width: usize) -> Line<'static> {
    let room = width.saturating_sub(PREFIX);
    let mut spans = prefix(scene, entry);
    let sentence = if let What::Failed(failed) = &entry.what {
        verdict_sentence(scene, failed, room)
    } else {
        let sentence = entry.sentence();
        let short = if sentence.1.is_empty() {
            let text = format!("{}{}", sentence.0, sentence.2);
            if display_width(&text) <= room {
                Some(text)
            } else {
                summarised(entry, room)
            }
        } else {
            None
        };
        match short {
            Some(short) => Line::from(scene.paint(Paint::Text, short)),
            None => cut_styled(&emphasised(scene, sentence), room),
        }
    };
    spans.extend(sentence.spans);
    Line::from(spans)
}

/// The rows of `entry` in `width` cells: one when its sentence fits or `limit` is 1, else its sentence wrapped
/// under its own text column in at most `limit` rows, the last cut to whole words.
fn rows(scene: &Scene<'_>, entry: &Entry, width: usize, limit: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(PREFIX);
    let sentence = emphasised(scene, entry.sentence());
    if limit <= 1 || room == 0 || line_width(&sentence) <= room {
        return vec![row(scene, entry, width)];
    }
    let mut wrapped = wrap_line(&sentence, room);
    if wrapped.len() > limit {
        let summary = summarised(entry, (room * limit).saturating_sub(limit - 1))
            .map(|short| wrap_styled(&short, room, scene.style(Paint::Text)))
            .filter(|lines| lines.len() <= limit);
        wrapped = summary.unwrap_or_else(|| {
            let mut tail = Line::default();
            for (i, line) in wrapped.split_off(limit - 1).into_iter().enumerate() {
                if i > 0 {
                    tail.spans.push(scene.paint(Paint::Text, " ".to_owned()));
                }
                tail.spans.extend(line.spans);
            }
            wrapped.push(cut_styled(&tail, room));
            wrapped
        });
    }
    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let mut spans = if i == 0 {
                prefix(scene, entry)
            } else {
                vec![Span::raw(" ".repeat(PREFIX))]
            };
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

/// The sentence of a failed trial in `room` cells. What the failure was and whom it names are reserved first;
/// the timing gives way before anything else, and the subject is cut to what is left, so a long subject never
/// takes the attribution's cells.
fn verdict_sentence(scene: &Scene<'_>, failed: &Failed, room: usize) -> Line<'static> {
    let verdict = &failed.verdict;
    let sep = || Line::from(scene.paint(Paint::Text, " · ".to_owned()));
    let signal_paint = if verdict.alarm() {
        Paint::Bad
    } else {
        Paint::Text
    };
    let signal = Line::from(scene.paint(signal_paint, verdict.signal_words().to_owned()));
    let named = verdict
        .named
        .map(|named| Line::from(scene.paint(Paint::Text, named.words())));
    let reserved: usize = line_width(&signal)
        + named
            .as_ref()
            .map_or(0, |named| line_width(named) + line_width(&sep()));
    let subject_room = room.saturating_sub(reserved + line_width(&sep()));
    let subject_text = scene.paint(Paint::Text, failed.subject.clone());
    let subject_cut = cut_styled(&Line::from(subject_text.clone()), subject_room);
    let mut subject = vec![Form::new(Line::from(subject_text), 1)];
    subject.push(if line_width(&subject_cut) > 0 {
        Form::new(subject_cut, 0)
    } else {
        Form::empty()
    });
    let mut clauses = vec![
        Clause::new(Line::default(), subject),
        Clause::whole(sep(), signal),
    ];
    if let Some(when) = &verdict.when {
        clauses.push(Clause::new(
            sep(),
            vec![
                Form::new(Line::from(scene.paint(Paint::Text, when.clone())), 2),
                Form::empty(),
            ],
        ));
    }
    if let Some(named) = named {
        clauses.push(Clause::whole(sep(), named));
    }
    fit_clauses(room, &clauses)
}

/// Main's What happened: the newest meaningful entries of the folded history in at most `height` rows of
/// `width` cells, newest first. An important entry may take two rows. The newest failure always stays: when
/// newer entries would push it out, they give way, oldest of them first.
pub(crate) fn main_history(scene: &Scene<'_>, width: usize, height: usize) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let items = fold(scene.s.history);
    if items.is_empty() {
        return vec![cut_styled(&cell(scene, Paint::Grey, NOTHING_YET), width)];
    }
    let render = |entry: &Entry, limit: usize| rows(scene, entry, width, limit);
    let limit_of = |entry: &Entry| if is_important(entry) { 2 } else { 1 };
    let mut chosen: Vec<(usize, Vec<Line<'static>>)> = Vec::new();
    let mut used = 0;
    for (index, entry) in items.iter().enumerate() {
        let wanted = render(entry, limit_of(entry));
        if used + wanted.len() <= height {
            used += wanted.len();
            chosen.push((index, wanted));
            continue;
        }
        if used < height {
            chosen.push((index, render(entry, 1)));
        }
        break;
    }
    if let Some(failure) = items.iter().position(is_failure)
        && !chosen.iter().any(|(index, _)| *index == failure)
    {
        let wanted = render(&items[failure], limit_of(&items[failure]).min(height));
        let cost = |chosen: &[(usize, Vec<Line<'static>>)]| -> usize {
            chosen.iter().map(|(_, rows)| rows.len()).sum()
        };
        while cost(&chosen) + wanted.len() > height && chosen.pop().is_some() {}
        chosen.push((failure, wanted));
    }
    let out: Vec<Line<'static>> = chosen.into_iter().flat_map(|(_, rows)| rows).collect();
    #[cfg(test)]
    crate::corpus::lines("panel.history", "", &out);
    out
}

/// The History page's body in `width` cells, unscrolled: every kept entry in full, newest first, under a
/// separator for each local date.
pub(crate) fn history_page(scene: &Scene<'_>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let entries = scene.s.history;
    if entries.is_empty() {
        return vec![cut_styled(
            &cell(
                scene,
                Paint::Grey,
                format!(
                    "{NOTHING_YET}; page {} lists every event",
                    View::Log.number()
                ),
            ),
            width,
        )];
    }
    let mut out = Vec::new();
    let mut date: Option<String> = None;
    for entry in entries {
        let day = wall_date(scene.zone, entry.at);
        if date.as_ref() != Some(&day) {
            out.push(rule(
                width,
                &cell(scene, Paint::Grey, day.clone()),
                &Line::default(),
                scene.style(Paint::Track),
            ));
            date = Some(day);
        }
        out.extend(rows(scene, entry, width, usize::MAX));
    }
    if scene.s.history_dropped > 0 {
        out.push(cut_styled(
            &cell(
                scene,
                Paint::Grey,
                format!(
                    "{} older entries are not kept here; page {} lists every event",
                    scene.s.history_dropped,
                    View::Log.number()
                ),
            ),
            width,
        ));
    }
    out
}

#[cfg(test)]
mod tests;
