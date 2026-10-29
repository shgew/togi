//! The stage line: the night or the day, and what the night is doing.

use std::time::Duration;

use ratatui::text::{Line, Span};
use togi_journal::{DayEventId, Queue, ReproductionState, Tested};
use togi_tuner::policy::{NightProjection, Projection};
use togi_tuner::unit::UnitProfile;

use crate::frames::colours::Paint;
use crate::frames::layout::SizeClass;
use crate::frames::scene::{Scene, between, clock, hm};
use crate::frames::story::queue_of;

fn ended(scene: &Scene<'_>) -> bool {
    scene.s.stopped.is_some() || scene.s.dead_end.is_some() || scene.s.recover.is_some()
}

fn mark(scene: &Scene<'_>, running: Paint) -> Span<'static> {
    if ended(scene) || scene.s.crashed_trial() {
        scene.paint(Paint::Track, "○ ")
    } else {
        scene.paint(running, "► ")
    }
}

/// The open night: the latest plan, until it closes.
fn open_night(policy: &Projection) -> Option<&NightProjection> {
    policy
        .night
        .as_ref()
        .filter(|night| night.closed_by.is_none())
}

/// The night's elapsed observed time E: what its queues consumed, together.
fn elapsed(night: &NightProjection) -> Duration {
    Duration::from_secs(night.budgets.iter().map(|budget| budget.consumed_s).sum())
}

fn profile_text(profile: &UnitProfile) -> String {
    profile
        .iter()
        .map(|(_, offset)| offset.get().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn event_name(event: &DayEventId) -> String {
    match event {
        DayEventId::Boot { boot } => {
            let short: String = boot.as_str().chars().take(8).collect();
            format!("boot {short}")
        }
        DayEventId::Record { seq, .. } => format!("record #{seq}"),
        DayEventId::Requalification { night } => format!("requalification {night}"),
    }
}

/// What the night's current trial does, in words: the queue [`queue_of`] names, `None` when nothing runs under a
/// Ruleset 13 activity.
fn activity_words(scene: &Scene<'_>, night: &NightProjection, short: bool) -> Option<String> {
    let trial = scene.s.trial;
    let queue = queue_of(scene, trial)?;
    let words = match queue {
        Queue::Mapping => match trial.and_then(|trial| trial.tested) {
            Some(Tested::Isolated { ccd, offset }) if short => {
                format!("mapping C{} {}", ccd.get(), offset.get())
            }
            Some(Tested::Isolated { ccd, offset }) => {
                format!("mapping CCD {} at {}", ccd.get(), offset.get())
            }
            Some(Tested::Exact {}) | None => "mapping".to_owned(),
        },
        Queue::Qualification | Queue::Requalification => {
            let verb = if queue == Queue::Requalification {
                "requalifying"
            } else {
                "qualifying"
            };
            match &night.qualification {
                Some(progress) => format!("{verb} {}", profile_text(&progress.profile)),
                None => verb.to_owned(),
            }
        }
        Queue::Reproduction => {
            let pending = night
                .reproductions
                .iter()
                .find(|reproduction| reproduction.state == ReproductionState::Pending);
            match pending {
                Some(reproduction) if !short => {
                    format!("reproducing {}", event_name(&reproduction.source))
                }
                Some(_) | None => "reproducing".to_owned(),
            }
        }
        Queue::Control => "controls at CO 0".to_owned(),
        Queue::Filler => match trial.and_then(|trial| trial.tested) {
            Some(Tested::Isolated { ccd, .. }) if short => format!("filler C{}", ccd.get()),
            Some(Tested::Isolated { ccd, .. }) => format!("filler on CCD {}", ccd.get()),
            Some(Tested::Exact {}) | None => "filler".to_owned(),
        },
    };
    Some(words)
}

/// The stage line: the open night with its start, elapsed time and activity, or the day, or why the run stopped. `summary`
/// adds the trial's time for the views that hide the trial band.
pub(crate) fn stage_line(scene: &Scene<'_>, class: SizeClass, summary: bool) -> Line<'static> {
    if scene.s.session.is_none() {
        return Line::default();
    }
    let short = class == SizeClass::Compact;
    let policy = scene.s.policy;
    let mut parts: Vec<Vec<Span<'static>>> = Vec::new();
    if scene.s.stopped.is_some() || policy.stopped {
        parts.push(vec![
            mark(scene, Paint::Running),
            scene.paint(Paint::White, "STOPPED"),
        ]);
    } else if scene.s.dead_end.is_some() {
        parts.push(vec![
            mark(scene, Paint::Running),
            scene.paint(Paint::White, "DEAD END"),
        ]);
    } else if let Some(night) = open_night(policy) {
        let spent = hm(elapsed(night));
        let name = if short {
            format!("NIGHT {spent}")
        } else {
            format!("NIGHT since {} · {spent}", scene.wall_minute(night.started))
        };
        parts.push(vec![
            mark(scene, Paint::Running),
            scene.paint(Paint::White, name),
        ]);
        if let Some(words) = activity_words(scene, night, short) {
            parts.push(vec![scene.paint(Paint::Text, words)]);
        }
    } else {
        parts.push(vec![
            mark(scene, Paint::Running),
            scene.paint(Paint::White, "DAY"),
        ]);
    }
    if summary
        && scene.s.dead_end.is_none()
        && let Some(trial) = scene.s.trial
        && let Some(started) = trial.started
    {
        let now = scene.now;
        let text = if let Some(hold) = scene.hold.filter(|hold| now < hold.planned_end) {
            (
                Paint::Grey,
                format!(
                    "screen paused · planned end {}",
                    scene.wall_minute(hold.planned_end)
                ),
            )
        } else if trial.crashed {
            (Paint::Warn, "trial crashed".to_owned())
        } else if !trial.past_end(now).is_zero() {
            (
                Paint::Warn,
                format!("{} past its end", clock(trial.past_end(now))),
            )
        } else {
            (
                Paint::White,
                format!(
                    "{} left",
                    clock(trial.duration.saturating_sub(between(started, now)))
                ),
            )
        };
        parts.push(vec![scene.paint(text.0, text.1)]);
    }
    let join = if short {
        [" ", "─", " "]
    } else {
        ["  ", "───", "  "]
    };
    let separator = [
        scene.paint(Paint::Text, join[0]),
        scene.paint(Paint::Track, join[1]),
        scene.paint(Paint::Text, join[2]),
    ];
    let mut out = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            out.extend(separator.iter().cloned());
        }
        out.extend(part);
    }
    let line = Line::from(out);
    #[cfg(test)]
    crate::corpus::line(
        "dash.stage",
        &format!("{class:?}{}", if summary { " summary" } else { "" }),
        &line,
    );
    line
}
