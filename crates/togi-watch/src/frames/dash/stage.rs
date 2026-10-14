use std::fmt::Write as _;

use ratatui::text::{Line, Span};
use togi_journal::Activity;
use togi_tuner::PhaseNo;

use crate::frames::colours::Paint;
use crate::frames::layout::SizeClass;
use crate::frames::panels::hunt_stage;
use crate::frames::scene::{Scene, between, clock};
use crate::frames::terms::first_open_part;
use crate::words::plural;

fn ended(scene: &Scene<'_>) -> bool {
    scene.s.stopped.is_some() || scene.s.dead_end.is_some()
}

fn mark(scene: &Scene<'_>, running: Paint) -> Span<'static> {
    if ended(scene) || scene.s.crashed_trial() {
        scene.paint(Paint::Track, "○ ")
    } else {
        scene.paint(running, "► ")
    }
}

fn solo_stage(scene: &Scene<'_>, name: &str, found: usize, short: bool) -> Vec<Span<'static>> {
    let total = scene.s.cores.len();
    if found == total && found > 0 {
        let mut out = vec![
            scene.paint(Paint::Good, "■ "),
            scene.paint(Paint::Text, name.to_owned()),
        ];
        if !short {
            out.push(scene.paint(Paint::Grey, format!("  {found}/{total}")));
        }
        return out;
    }
    let mut out = vec![
        mark(scene, Paint::Running),
        scene.paint(Paint::White, name.to_owned()),
        scene.paint(Paint::Text, format!("  {found} of {total} found")),
    ];
    if let Some(trial) = scene.s.trial.filter(|trial| {
        trial.condition == togi_machine::Condition::Alone && !short && !trial.crashed
    }) && let (Some(core), Some(offset)) = (trial.core, trial.offset)
    {
        let verb = if scene.core(core).is_some_and(|core| core.confirm.is_some()) {
            "confirming"
        } else {
            "searching"
        };
        out.push(scene.paint(
            Paint::Text,
            format!(" · core {:02} {verb} {}", core.get(), offset.get()),
        ));
    }
    out
}

fn phase1_stage(scene: &Scene<'_>, name: &str, found: usize) -> Vec<Span<'static>> {
    let Some(phases) = scene.s.phases else {
        return vec![scene.paint(Paint::Grey, format!("○ {name}"))];
    };
    let one = phases.phase == PhaseNo::One;
    if one && (found < scene.s.cores.len() || scene.s.hunt.is_some()) {
        vec![scene.paint(Paint::Grey, format!("○ {name}"))]
    } else if one {
        vec![
            mark(scene, Paint::Running),
            scene.paint(Paint::White, name.to_owned()),
        ]
    } else {
        vec![
            scene.paint(Paint::Good, "■ "),
            scene.paint(Paint::Text, name.to_owned()),
        ]
    }
}

fn phase2_stage(scene: &Scene<'_>, name: &str, short: bool) -> Vec<Span<'static>> {
    let text = |value: String| scene.paint(Paint::Text, value);
    let phases = match scene.s.phases {
        Some(phases) if phases.phase != PhaseNo::One => phases,
        _ => return vec![scene.paint(Paint::Grey, format!("○ {name}"))],
    };
    if phases.phase == PhaseNo::Concluded {
        let mut out = vec![scene.paint(Paint::Good, "■ "), text(name.to_owned())];
        if !short {
            out.push(scene.paint(Paint::Grey, "  concluded"));
        }
        return out;
    }
    let mut out = if scene.s.hunt.is_some() {
        vec![scene.paint(Paint::Hunt, "○ "), text(name.to_owned())]
    } else {
        vec![
            mark(scene, Paint::Running),
            scene.paint(Paint::White, name.to_owned()),
        ]
    };
    let left = phases.rounds_left;
    match phases.round {
        Some(round) if short => out.push(text(format!(" r{round}"))),
        Some(round) => out.push(text(format!("  round {round} · at most {left} left"))),
        None if phases.confirming && !short => out.push(text("  confirmation cycle".into())),
        None if !short => out.push(text(format!(
            "  at most {} left",
            plural(left as usize, "round")
        ))),
        None => {}
    }
    out
}

fn cycle_stage(scene: &Scene<'_>, name: &str, short: bool) -> Vec<Span<'static>> {
    let Some(cycle) = scene.s.cycle else {
        return vec![scene.paint(Paint::Grey, format!("○ {name}"))];
    };
    let plan = cycle.plan;
    let name = plan
        .number
        .map_or_else(|| name.to_owned(), |number| format!("{name} {number}"));
    let steps = plan.steps.len();
    let resume = cycle.resume_step();
    let at = (resume + 1).min(steps);
    if steps > 0 && resume >= steps && (plan.paused || scene.s.hunt.is_some()) {
        return vec![
            scene.paint(Paint::Good, "■ "),
            scene.paint(Paint::Text, name),
        ];
    }
    if plan.paused || scene.s.hunt.is_some() {
        let mut out = vec![
            scene.paint(Paint::Hunt, "○ "),
            scene.paint(Paint::Text, name),
        ];
        if short {
            out.push(scene.paint(Paint::Hunt, format!(" paused at {at}/{steps}")));
            return out;
        }
        let mut words = format!("  paused at step {at} of {steps}");
        if let Some(step) = plan.steps.get(resume).filter(|step| step.parts.len() > 1)
            && let Some(part) = first_open_part(step)
        {
            let _ = write!(words, ", part {part}");
        }
        out.push(scene.paint(Paint::Hunt, words));
        return out;
    }
    if scene.s.stopped.is_some() && plan.current >= steps {
        return vec![
            scene.paint(Paint::Good, "■ "),
            scene.paint(Paint::Text, name),
        ];
    }
    if scene.s.session.and_then(|session| session.activity) != Some(Activity::Checking) {
        return vec![
            scene.paint(Paint::Good, "■ "),
            scene.paint(Paint::Text, name),
        ];
    }
    let mut out = vec![mark(scene, Paint::Running), scene.paint(Paint::White, name)];
    if short {
        return out;
    }
    let here = scene.s.trial.and_then(|trial| {
        let place = trial.cycle?;
        (Some(place.cycle) == plan.number.map(std::num::NonZeroU32::get)).then_some((trial, place))
    });
    let step = here.and_then(|(_, place)| place.step.filter(|&step| step > 0));
    let step = step.map_or(at, |step| step as usize);
    out.push(scene.paint(Paint::Text, format!("  step {step} of {steps}")));
    if let Some((trial, place)) = here {
        if place.parts > 1 {
            out.push(scene.paint(
                Paint::Text,
                format!(" · part {} of {}", place.part.unwrap_or(0), place.parts),
            ));
        }
        if trial.count.of > 1 {
            out.push(scene.paint(
                Paint::Text,
                format!(" · trial {} of {}", trial.count.index, trial.count.of),
            ));
        }
    }
    out
}

/// The stage line: search, phase 1, phase 2, the clean cycles and the cycle, with a hunt as a detour. `summary`
/// adds the trial's time for the views that hide the trial band.
pub(crate) fn stage_line(scene: &Scene<'_>, class: SizeClass, summary: bool) -> Line<'static> {
    if scene.s.session.is_none() {
        return Line::default();
    }
    let found = scene
        .s
        .cores
        .iter()
        .filter(|core| core.solo.is_some())
        .count();
    let short = class == SizeClass::Compact;
    let (names, join) = if short {
        (["SOLO", "P1", "P2", "CLEAN", "CYCLE"], [" ", "─", " "])
    } else {
        (
            ["SOLO LIMITS", "PHASE 1", "PHASE 2", "CLEAN CYCLES", "CYCLE"],
            ["  ", "───", "  "],
        )
    };
    let mut parts = vec![solo_stage(scene, names[0], found, short)];
    if let Some(hunt) = scene.s.hunt {
        let mut stage = vec![
            mark(scene, Paint::Hunt),
            scene.paint(Paint::Lamp, format!(" HUNT {} ", hunt.plan.number)),
        ];
        let where_ = hunt_stage(scene, short);
        if !where_.is_empty() {
            stage.push(scene.paint(Paint::Hunt, format!("  {where_}")));
        }
        parts.push(stage);
    }
    parts.push(phase1_stage(scene, names[1], found));
    parts.push(phase2_stage(scene, names[2], short));
    let mut clean = vec![scene.paint(
        Paint::Grey,
        format!("{} {}", names[3], scene.s.clean_cycles),
    )];
    if !short
        && !ended(scene)
        && scene
            .s
            .phases
            .is_some_and(|phases| phases.phase == PhaseNo::Concluded)
    {
        clean.push(scene.paint(Paint::Track, "  · repeats until stopped"));
    }
    parts.push(clean);
    parts.push(cycle_stage(scene, names[4], short));
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
                    "screen paused until {}",
                    scene.wall_minute(hold.planned_end)
                ),
            )
        } else if trial.crashed {
            (Paint::Hunt, "trial crashed".to_owned())
        } else if !trial.past_end(now).is_zero() {
            (
                Paint::Hunt,
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
