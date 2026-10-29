//! The narration: why the screen looks the way it does, what the last trial did, and the resting band when no
//! trial runs.

#[cfg(test)]
mod corpus;
mod night;
mod resting;
#[cfg(test)]
mod tests;

use std::time::Duration;

use ratatui::text::{Line, Span};
use togi_journal::{Activity, DeadEndCondition, ShutdownReason, Tested};
use togi_machine::Regime;

use super::colours::Paint;
use super::fit::{cut_styled, display_width, inert, wrap_styled};
use super::scene::{Scene, clock};
use crate::model::TrialView;
use crate::screen::Colours;
use crate::words::{core_ids, workload_display};

pub(crate) use night::queue_of;
pub(crate) use night::{blocker_words, climb_lines, finish_words, vector_words};
pub(crate) use resting::{last_lines, resting_band};

/// What the narrator says: why the screen looks the way it does, never what the trial band already says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Story {
    /// The stage it speaks for: `MAPPING`, `QUALIFYING`, `STOPPED`.
    pub(crate) label: String,
    /// At most two on the dashboard. Raw: the narrator escapes them.
    pub(crate) lines: Vec<String>,
    /// The one line a compact screen has room for; empty when the first line serves.
    pub(crate) brief: String,
    /// The paint of the bar and the label.
    pub(crate) paint: Paint,
}

impl Story {
    fn new(
        label: impl Into<String>,
        lines: Vec<String>,
        brief: impl Into<String>,
        paint: Paint,
    ) -> Self {
        Self {
            label: label.into(),
            lines,
            brief: brief.into(),
            paint,
        }
    }

    fn one(label: impl Into<String>, text: String, paint: Paint) -> Self {
        Self::new(label, vec![text], String::new(), paint)
    }
}

/// The trial band when no trial is in flight: its title and the rows under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestingBand {
    /// `BETWEEN TRIALS`, `STOPPED`, `DEAD END`, …
    pub(crate) title: &'static str,
    /// The title's paint.
    pub(crate) paint: Paint,
    /// The rows, raw: the band escapes them.
    pub(crate) lines: Vec<String>,
}

/// The story of a folded journal, with or without a session.
pub(crate) fn story(scene: &Scene<'_>) -> Story {
    let said = spoken(scene);
    #[cfg(test)]
    corpus::said(&said);
    said
}

fn spoken(scene: &Scene<'_>) -> Story {
    let s = &scene.s;
    if s.session.is_none() {
        return Story::one(
            "NO SESSION YET",
            "Nothing has been recorded yet. Start with togi run, or boot the togi entry, and I'll tell you what I'm doing here.".to_owned(),
            Paint::Running,
        );
    }
    if s.trial.is_none() {
        if let Some(end) = s.day.pending().next() {
            return question_story(end);
        }
    }
    if let Some(dead_end) = s.dead_end {
        return dead_end_story(dead_end.condition);
    }
    if s.policy.stopped {
        return stock_story();
    }
    if let Some(trial) = s.trial.filter(|trial| trial.crashed) {
        return Story::one(
            "CRASHED",
            format!(
                "Trial {} crashed: a later boot has written events. togi run records the crash.",
                trial.id
            ),
            Paint::Warn,
        );
    }
    if let Some(stopped) = s.stopped {
        return stopped_story(stopped.reason, stopped.saved);
    }
    if s.recover.is_some() {
        return recover_story(scene);
    }
    let Some(trial) = s.trial else {
        return night::between(scene);
    };
    trial_story(scene, trial)
}

/// The oldest unanswered unclean end of a day-observed boot, as a question. The dashboard has two lines that fit
/// the medium class: the boot, reset reason and reading first, then the four choices with where to answer; the
/// full evidence and the answering commands are in `togi status`, and the compact brief carries the command.
fn question_story(end: &togi_journal::DayUncleanEnd) -> Story {
    use crate::day;
    let command = format!(
        "togi day answer {} {}",
        end.boot.as_str(),
        day::answer_args()
    );
    Story::new(
        "DAY QUESTION",
        vec![
            day::headline(end),
            format!("{}. Answer: togi status", day::choices_line()),
        ],
        format!(
            "Boot {} ended unclean: {command}",
            day::short_boot(&end.boot)
        ),
        Paint::Warn,
    )
}

fn trial_story(scene: &Scene<'_>, trial: &TrialView) -> Story {
    let stage = || night::stage_label(scene.s.policy);
    if trial.started.is_none() {
        return Story::one(
            stage(),
            "The trial's intent is recorded; its offsets are being applied and its workload is starting.".to_owned(),
            Paint::Running,
        );
    }
    let past = trial.past_end(scene.now);
    if !past.is_zero() {
        return Story::one(
            stage(),
            format!(
                "{} past its planned end, no trial end recorded. Its deadline or watchdog may still be running.",
                clock(past)
            ),
            Paint::Running,
        );
    }
    if trial.regime == Regime::R6 && scene.hold.is_some() {
        return Story::new(
            night::activity_of(scene, trial)
                .and_then(|activity| night::activity_label(activity, night::task_of(scene, trial)))
                .map_or_else(|| stage().to_owned(), str::to_owned),
            vec![
                format!(
                    "Every core idles for {}, with short wake-ups, to test idle and boost states.",
                    minutes(trial.duration)
                ),
                "This screen holds still while the idle trial is in flight, clock included, so drawing it cannot wake the cores.".to_owned(),
            ],
            "Every core idle with short wake-ups; this screen holds still.",
            Paint::Running,
        );
    }
    night::running(scene, trial)
}

/// The story of a journal a session is about to append to, before it records anything.
pub(crate) fn starting_story() -> Story {
    let said = Story::one(
        "STARTING",
        "Checking the machine and the journal before the first trial; this screen follows the run as soon as it records something.".to_owned(),
        Paint::Running,
    );
    #[cfg(test)]
    corpus::said(&said);
    said
}

/// The story of a journal that cannot be read; `one_frame` drops the promise to retry, which only live watch
/// keeps.
pub(crate) fn problem_story(error: &dyn std::error::Error, one_frame: bool) -> Story {
    let mut lines = vec![format!(
        "can't read journal: {}",
        togi_render::error_chain(error)
    )];
    if !one_frame {
        lines.push(
            "I'll retry when the journal changes. Tuning itself is not affected by this screen."
                .to_owned(),
        );
    }
    let said = Story::new("JOURNAL UNREADABLE", lines, "", Paint::Bad);
    #[cfg(test)]
    corpus::said(&said);
    said
}

fn stock_story() -> Story {
    Story::new(
        "STOCK FAILURE",
        vec![
            "A conclusive failure with every CCD at CO 0 ended tuning: the platform is unstable at stock, so Curve Optimizer did not cause it.".to_owned(),
            "A new run needs an explicit reset and fresh controls; the journal holds the failed class and its evidence.".to_owned(),
        ],
        "Failure at CO 0: the platform is unstable at stock, so tuning stopped.",
        Paint::Bad,
    )
}

fn stopped_story(reason: ShutdownReason, saved: bool) -> Story {
    let text = match reason {
        // Only an earlier version of togi run stopped at a requested limit; a journal it wrote still says so.
        ShutdownReason::FirstResult | ShutdownReason::Cycles if saved => {
            "I stopped at a run limit an earlier version supported and restored baseline or shallower offsets. togi run picks up where I left off."
        }
        ShutdownReason::FirstResult | ShutdownReason::Cycles => {
            "I stopped at a run limit an earlier version supported. Everything I learned is in the journal; togi run picks up where I left off."
        }
        ShutdownReason::Signal | ShutdownReason::DeadEnd | ShutdownReason::Command if saved => {
            "I restored baseline or shallower offsets before stopping. togi run picks up where I left off."
        }
        ShutdownReason::Signal | ShutdownReason::DeadEnd | ShutdownReason::Command => {
            "I've stopped. Everything I learned is in the journal; togi run picks up where I left off."
        }
    };
    Story::one("STOPPED", text.to_owned(), Paint::Running)
}

fn dead_end_story(condition: DeadEndCondition) -> Story {
    let why = match condition {
        DeadEndCondition::FailureAtZero => {
            "A trial failed conclusively with every CCD at CO 0, so the platform is unstable at stock and Curve Optimizer is not the cause. Check the rest of the system before tuning again."
        }
        DeadEndCondition::Smu => {
            "The CPU didn't take an offset the way I wrote it, so I can't trust what is applied. Nothing else will be written."
        }
        DeadEndCondition::NoEvidence => {
            "I couldn't obtain conclusive trial evidence, so I can't continue testing safely. Check the recorded details before tuning again."
        }
        DeadEndCondition::BootLoop => {
            "The machine crashed three times in a row before I applied any offsets: something else is crashing it. Check the system before tuning again."
        }
        DeadEndCondition::Containment => {
            "I couldn't confirm that the stress programs were contained and cleaned up safely, so tuning can't continue. Check the recorded details."
        }
        DeadEndCondition::Preflight => {
            "A check before tuning failed: this isn't the environment I tune in. Fix the recorded check before tuning again."
        }
        DeadEndCondition::Defect => {
            "An earlier build may have moved offsets deeper than proven. Run togi reset --all to start a fresh session before tuning continues."
        }
        DeadEndCondition::ThermalTrip => {
            "The CPU shut down from heat. Check the cooling before tuning again."
        }
    };
    Story::one("DEAD END", why.to_owned(), Paint::Bad)
}

fn recover_story(scene: &Scene<'_>) -> Story {
    let policy = scene.s.policy;
    let mut lines = vec![
        "I picked up from the journal after the reboot; nothing it recorded is lost.".to_owned(),
    ];
    match night::next_words(policy) {
        Some(next) => lines.push(next),
        None => lines.extend(night::day_lines(&scene.s)),
    }
    let brief = lines[0].clone();
    Story::new("RECOVERED", lines, brief, Paint::Warn)
}

/// A trial's planned length as a count: `10 minutes`, `45 seconds`.
fn minutes(span: Duration) -> String {
    let secs = span.as_secs();
    let (count, unit) = if secs >= 60 && secs.is_multiple_of(60) {
        (secs / 60, "minute")
    } else {
        (secs, "second")
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// `story` as `▌  LABEL   text` in `width` cells and at most `height` rows, wrapped lines aligned under the text,
/// never under the label; `brief` is the one line of a compact screen, with no label.
pub(crate) fn narrator_lines(
    story: &Story,
    width: usize,
    height: usize,
    brief: bool,
    colours: Colours,
) -> Vec<Line<'static>> {
    let text_style = Paint::Text.style(colours);
    let bar = || story.paint.span(colours, "▌");
    if brief {
        let text = match (story.brief.as_str(), story.lines.first()) {
            ("", Some(first)) => first.as_str(),
            (brief, _) => brief,
        };
        let mut spans = vec![bar(), Span::raw("  ")];
        spans.extend(
            cut_styled(
                &Line::from(Span::styled(inert(text, colours), text_style)),
                width.saturating_sub(3),
                colours,
            )
            .spans,
        );
        return vec![Line::from(spans)];
    }
    let indent = display_width(&story.label, colours) + 3;
    let room = width.saturating_sub(3 + indent).max(1);
    let wrap = |text: &str| wrap_styled(&inert(text, colours), room, text_style, colours);
    let mut wrapped: Vec<Line<'static>> = story.lines.iter().flat_map(|line| wrap(line)).collect();
    if wrapped.len() > height {
        let brief = wrap(&story.brief);
        wrapped = if !story.brief.is_empty() && brief.len() <= height {
            brief
        } else {
            story
                .lines
                .iter()
                .map(|line| {
                    cut_styled(
                        &Line::from(Span::styled(inert(line, colours), text_style)),
                        room,
                        colours,
                    )
                })
                .collect()
        };
    }
    wrapped
        .into_iter()
        .take(height)
        .enumerate()
        .map(|(i, line)| {
            let label = if i == 0 {
                story.paint.span(colours, format!("{}   ", story.label))
            } else {
                Span::raw(" ".repeat(indent))
            };
            let mut spans = vec![bar(), Span::raw("  "), label];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

/// Where the trial in flight loads: `on 00-07`. Raw.
pub(crate) fn trial_where(_scene: &Scene<'_>, trial: &TrialView) -> String {
    format!("on {}", core_ids(trial.cores))
}

/// What the trial in flight does, in capitals and painted: `MAPPING CCD 1 AT -15`, `QUALIFYING -10/-5`,
/// `FILLER CCD 0 AT -20`, `CONTROLS AT CO 0`, with `· RETRY` when it retries a trial that could not run.
pub(crate) fn operation(scene: &Scene<'_>, trial: &TrialView) -> Line<'static> {
    let task = night::task_of(scene, trial);
    let mut text = match night::activity_of(scene, trial) {
        Some(Activity::Mapping | Activity::Qualification) if night::is_filler(task) => {
            match trial.tested {
                Some(Tested::Isolated { ccd, offset }) => {
                    format!("FILLER CCD {} AT {}", ccd.get(), offset.get())
                }
                Some(Tested::Exact {}) | None => night::exact_profile(scene, trial, task)
                    .map_or_else(
                        || "FILLER".to_owned(),
                        |profile| format!("FILLER {}", night::profile_offsets(&profile)),
                    ),
            }
        }
        Some(Activity::Mapping) => match trial.tested {
            Some(Tested::Isolated { ccd, offset }) => {
                format!("MAPPING CCD {} AT {}", ccd.get(), offset.get())
            }
            Some(Tested::Exact {}) | None => "MAPPING".to_owned(),
        },
        Some(activity @ (Activity::Qualification | Activity::Reproduction)) => {
            let label = night::activity_label(activity, task).unwrap_or_default();
            match trial.tested {
                Some(Tested::Isolated { ccd, offset }) => {
                    format!("{label} CCD {} AT {}", ccd.get(), offset.get())
                }
                Some(Tested::Exact {}) | None if activity == Activity::Qualification => {
                    night::exact_profile(scene, trial, task).map_or_else(
                        || label.to_owned(),
                        |profile| format!("{label} {}", night::profile_offsets(&profile)),
                    )
                }
                Some(Tested::Exact {}) | None => format!("{label} EXACT PROFILE"),
            }
        }
        Some(Activity::Control) => "CONTROLS AT CO 0".to_owned(),
        Some(Activity::Search | Activity::Checking | Activity::Hunt | Activity::Deepening)
        | None => workload_display(trial.workload).to_uppercase(),
    };
    if trial.retry {
        text.push_str(" · RETRY");
    }
    Line::from(scene.paint(Paint::Running, text))
}
