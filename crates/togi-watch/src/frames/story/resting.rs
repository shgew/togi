//! The trial band when no trial runs, and the last trial's ending beside the narration.

use std::fmt::Write as _;

use ratatui::text::Line;
use togi_journal::{Outcome, ShutdownReason};
use togi_machine::{Condition, CoreId, ResetKind, Signal};
use togi_tuner::Uncertainty;

use super::super::colours::Paint;
use super::super::scene::{Scene, clock};
use super::super::terms::{cycle_place, trial_name};
use super::RestingBand;
use super::outcome::{NEXT_TRIAL, next_trial_words};
use crate::model::{RecoveryView, TrialEndView};
use crate::words::{kind_words, lateness, on_cores, signal_words};

const SAVED_ROWS: &str = "Rows show the saved profile, not applied now.";

/// The resting band: no trial in flight, a stop, a dead end, a crashed trial or a recovery.
pub(crate) fn resting_band(scene: &Scene<'_>) -> RestingBand {
    let band = band(scene);
    #[cfg(test)]
    super::corpus::band(&band);
    band
}

fn band(scene: &Scene<'_>) -> RestingBand {
    let s = &scene.s;
    if let Some(dead_end) = s.dead_end {
        let mut lines = vec![format!(
            "{}: {}",
            dead_end.condition.as_str().replace('_', " "),
            dead_end.detail
        )];
        if s.stopped.is_some_and(|stopped| stopped.saved) {
            lines.push(SAVED_ROWS.to_owned());
        }
        return RestingBand {
            title: "DEAD END",
            paint: Paint::Bad,
            lines,
        };
    }
    if s.crashed_trial() {
        return RestingBand {
            title: "CRASHED, NOT YET RECOVERED",
            paint: Paint::Hunt,
            lines: crashed_lines(scene),
        };
    }
    if let Some(stopped) = s.stopped {
        let mut lines = vec![format!(
            "{} · {}",
            scene.wall_second(stopped.at),
            stop_words(stopped.reason)
        )];
        if stopped.saved {
            lines.push(SAVED_ROWS.to_owned());
        }
        return RestingBand {
            title: "STOPPED",
            paint: Paint::Grey,
            lines,
        };
    }
    if let Some(recover) = s.recover {
        return RestingBand {
            title: "RECOVERED FROM A CRASH",
            paint: Paint::Hunt,
            lines: recovered_lines(scene, recover),
        };
    }
    let mut lines = Vec::with_capacity(2);
    if let Some(last) = s.last {
        lines.push(format!("last: {}", last_description(last)));
    }
    lines.push(next_line(scene));
    RestingBand {
        title: "BETWEEN TRIALS",
        paint: Paint::Grey,
        lines,
    }
}

fn stop_words(reason: ShutdownReason) -> &'static str {
    match reason {
        ShutdownReason::Signal => "stopped by a signal",
        ShutdownReason::Cycles => "requested clean cycles completed",
        ShutdownReason::FirstResult => "stopped at the first confirmed result",
        ShutdownReason::Command => "read-only command finished; no applied offsets changed",
        ShutdownReason::DeadEnd => "tuning stopped at a dead end",
    }
}

/// What the tuner runs next between trials.
fn next_line(scene: &Scene<'_>) -> String {
    let between = scene.s.forecast.between.as_ref();
    match between.and_then(|between| between.next.as_ref()) {
        Some(next) => format!(
            "{NEXT_TRIAL}{}",
            next_trial_words(scene, next, None, None)
        ),
        None if between.and_then(|between| between.uncertainty)
            == Some(Uncertainty::NeedsRanking) =>
        {
            format!("{NEXT_TRIAL}read the core ranking before deciding")
        }
        None => format!("{NEXT_TRIAL}deciding"),
    }
}

/// What the crash ended, how late, what the journal holds about it, and what runs next.
fn recovered_lines(scene: &Scene<'_>, recover: &RecoveryView) -> Vec<String> {
    let mut detected = format!("detected {}", scene.wall_second(recover.crash_at));
    if let Some(reset) = recover.reset.filter(|&reset| reset != ResetKind::Unknown) {
        let _ = write!(
            detected,
            " after a {} reset",
            reset.as_str().replace('_', " ")
        );
    }
    let Some(trial) = &recover.trial else {
        return vec![
            format!("crash with no trial in flight · {detected}"),
            next_line(scene),
        ];
    };
    let mut first = format!("crash in {}", trial_name(trial));
    let place = cycle_place(trial);
    if !place.is_empty() {
        let _ = write!(first, " · {place}");
    }
    match &recover.end {
        Some(end) if end.core.is_some() => {
            let core = end.core.map_or(0, CoreId::get);
            let _ = write!(first, " · core {core:02} named");
        }
        Some(_) if trial.hunt.is_none() && trial.condition != Condition::Alone => {
            first.push_str(" · no core named");
        }
        _ => {}
    }
    let mut second = Vec::new();
    if let Some((end, sample)) = recover
        .end
        .as_ref()
        .and_then(|end| end.last_sample.map(|sample| (end, sample)))
    {
        second.push(format!("{} the trial", lateness(Some(sample), end.planned)));
        second.push(match end.planned {
            Some(planned) => format!("last sample {} of {}", clock(sample), clock(planned)),
            None => format!("last sample {}", clock(sample)),
        });
        if let Some(core) = end.stalled {
            second.push(format!(
                "core {:02}'s worker had stalled by then",
                core.get()
            ));
        }
    }
    second.push(detected);
    vec![first, second.join(" · "), next_line(scene)]
}

/// Which trial the journal's later boot ended and what records the crash.
fn crashed_lines(scene: &Scene<'_>) -> Vec<String> {
    let Some(trial) = scene.s.trial else {
        return Vec::new();
    };
    let mut first = format!("trial {} · {}", trial.id, trial_name(trial));
    let place = cycle_place(trial);
    if !place.is_empty() {
        let _ = write!(first, " · {place}");
    }
    if let Some(started) = trial.started {
        let _ = write!(first, " · started {}", scene.wall_second(started));
    }
    vec![
        first,
        "A later boot has written events, so the machine restarted before this trial recorded its end.".to_owned(),
        "togi run records the crash and picks up from the journal.".to_owned(),
    ]
}

/// How a trial ended, in one word: a crash is a crash, whatever else the journal says.
fn end_word(end: &TrialEndView) -> (&'static str, Paint) {
    if end.signal == Some(Signal::Crash) {
        ("CRASH", Paint::Bad)
    } else {
        match end.outcome {
            Outcome::Failure => ("FAILURE", Paint::Bad),
            Outcome::Inconclusive => ("INCONCLUSIVE", Paint::Hunt),
            Outcome::Pass => ("PASS", Paint::Good),
        }
    }
}

/// The measurements of a trial that ended: its length, unless a crash cut it short, its cores, its peak Tctl and
/// its median voltage request.
fn end_facts(end: &TrialEndView) -> Vec<String> {
    let cores: Vec<CoreId> = end.cores.iter().collect();
    let crashed = end.signal == Some(Signal::Crash);
    let mut where_ = on_cores(&cores);
    if !crashed {
        where_ = format!("{} {where_}", clock(end.duration));
    }
    let mut facts = vec![where_];
    if let Some(signal) = end.signal.filter(|&signal| signal != Signal::Crash) {
        facts.push(signal_words(signal).to_owned());
    }
    if let Some(core) = end.core {
        facts.push(format!("core {:02} named", core.get()));
    }
    if let Some(tctl) = end.tctl_max_c {
        facts.push(format!("Tctl max {tctl}°C"));
    }
    if let Some(volts) = end.voltage_v {
        facts.push(format!("{volts:.2} V median"));
    }
    facts
}

fn last_description(end: &TrialEndView) -> String {
    let (word, _) = end_word(end);
    let mut text = word.to_owned();
    if end.signal != Some(Signal::Crash) {
        let _ = write!(text, " after {}", clock(end.duration));
    }
    let mut facts = end_facts(end);
    let cores: Vec<CoreId> = end.cores.iter().collect();
    facts[0] = match end.regime {
        Some(regime) => format!("{regime} {} {}", kind_words(regime), on_cores(&cores)),
        None => on_cores(&cores),
    };
    format!("{text} · {}", facts.join(" · "))
}

/// The last trial's ending beside the narration: `last trial      PASS  10:00 on 00-15 · …`.
pub(crate) fn last_lines(scene: &Scene<'_>) -> Vec<Line<'static>> {
    let Some(last) = scene.s.last else {
        return Vec::new();
    };
    let (word, paint) = end_word(last);
    let lines = vec![Line::from(vec![
        scene.paint(Paint::Grey, "last trial      "),
        scene.paint(paint, word),
        scene.paint(Paint::Grey, format!("  {}", end_facts(last).join(" · "))),
    ])];
    #[cfg(test)]
    crate::corpus::lines("story.last", word, &lines);
    lines
}
