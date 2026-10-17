//! The shared stage line and Main's two phase-progress rows.
//!
//! The pipeline is `SOLO LIMITS → PHASE 1 → PHASE 2 → CLEAN CYCLES`: the cycle is a phase's work, never a stage of
//! its own. A stop, a dead end or a recovery is a lifecycle the line says in words, and under it no stage is
//! marked as running.

use ratatui::text::{Line, Span};
use togi_machine::{Condition, CoreSet};
use togi_tuner::{CycleStep, HuntPlan, PhaseNo, TrialContext};

use crate::frames::colours::Paint;
use crate::frames::fit::{Clause, Form, cut_styled, fit_clauses, line_width};
use crate::frames::panels::hunt_stage;
use crate::frames::scene::{Scene, between, clock};
use crate::frames::terms::{dead_end_words, first_open_part, recut_words};
use crate::model::CycleView;
use crate::screen::View;
use crate::words::{cores_named, kind_words, noun, on_cores, plural};

/// Where the session stands in its life, which decides whether any stage is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    /// Tuning, between trials included.
    Running,
    /// The session stopped; only the next `config.loaded` starts a run.
    Stopped,
    /// The session ended at a dead end.
    DeadEnd,
    /// A crash is recorded or detected and the run has not picked up yet.
    Recovery,
}

impl Lifecycle {
    /// The word the stage line and the progress rows say for a lifecycle that suspends the stages.
    fn word(self) -> Option<(Paint, &'static str)> {
        match self {
            Self::Running => None,
            Self::Stopped => Some((Paint::White, "STOPPED")),
            Self::DeadEnd => Some((Paint::Bad, "DEAD END")),
            Self::Recovery => Some((Paint::Hunt, "RECOVERY")),
        }
    }
}

/// The lifecycle of the session in `scene`.
pub(crate) fn lifecycle(scene: &Scene<'_>) -> Lifecycle {
    let s = &scene.s;
    if s.dead_end.is_some() {
        Lifecycle::DeadEnd
    } else if s.stopped.is_some() {
        Lifecycle::Stopped
    } else if s.recover.is_some() || s.crashed_trial() {
        Lifecycle::Recovery
    } else {
        Lifecycle::Running
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Solo,
    One,
    Two,
    Clean,
}

const STAGES: [Stage; 4] = [Stage::Solo, Stage::One, Stage::Two, Stage::Clean];

impl Stage {
    fn name(self, short: bool) -> &'static str {
        match (self, short) {
            (Self::Solo, false) => "SOLO LIMITS",
            (Self::One, false) => "PHASE 1",
            (Self::Two, false) => "PHASE 2",
            (Self::Clean, false) => "CLEAN CYCLES",
            (Self::Solo, true) => "SOLO",
            (Self::One, true) => "P1",
            (Self::Two, true) => "P2",
            (Self::Clean, true) => "CLEAN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Done,
    Current,
    Open,
}

fn found(scene: &Scene<'_>) -> usize {
    scene
        .s
        .cores
        .iter()
        .filter(|core| core.solo.is_some())
        .count()
}

/// Which stages are complete, which one the work is in and which have not begun, whatever the lifecycle.
fn marks(scene: &Scene<'_>) -> [Mark; 4] {
    let total = scene.s.cores.len();
    let phase = scene.s.phases.map(|phases| phases.phase);
    let past_one = matches!(phase, Some(PhaseNo::Two | PhaseNo::Concluded));
    let searched = total > 0 && found(scene) == total;
    if !past_one && !searched && scene.s.cycle.is_none() {
        return [Mark::Current, Mark::Open, Mark::Open, Mark::Open];
    }
    match phase {
        None | Some(PhaseNo::One) => [Mark::Done, Mark::Current, Mark::Open, Mark::Open],
        Some(PhaseNo::Two) => [Mark::Done, Mark::Done, Mark::Current, Mark::Open],
        Some(PhaseNo::Concluded) => [Mark::Done, Mark::Done, Mark::Done, Mark::Current],
    }
}

/// The stage the work is in; the last one in work after a stop.
fn current(scene: &Scene<'_>) -> Stage {
    STAGES
        .into_iter()
        .zip(marks(scene))
        .find_map(|(stage, mark)| (mark == Mark::Current).then_some(stage))
        .unwrap_or(Stage::Solo)
}

/// A cycle held for a repair: a failure or its rerun keeps the checklist where it is.
fn repairing(scene: &Scene<'_>) -> bool {
    scene.s.hunt.is_none()
        && (scene.s.cycle.is_some_and(|cycle| cycle.plan.paused)
            || scene.s.trial.is_some_and(|trial| trial.rerun))
}

fn sub_state(scene: &Scene<'_>, stage: Stage, short: bool) -> Vec<Span<'static>> {
    let s = &scene.s;
    let text = |paint: Paint, words: String| scene.paint(paint, words);
    let mut out = Vec::new();
    match stage {
        Stage::Solo => {}
        Stage::One => {
            if let (false, Some(number)) = (short, s.cycle.and_then(|cycle| cycle.plan.number)) {
                out.push(text(Paint::Text, format!(" · cycle {number}")));
            }
        }
        Stage::Two => {
            if let Some(phases) = s.phases {
                let left = phases.rounds_left;
                match phases.round {
                    Some(round) if short => out.push(text(Paint::Text, format!(" · r{round}"))),
                    Some(round) => out.push(text(
                        Paint::Text,
                        format!(
                            " · round {round} · {} left if none fails",
                            plural(left as usize, "round")
                        ),
                    )),
                    None if phases.confirming => {
                        let words = if short {
                            " · confirm"
                        } else {
                            " · confirmation cycle"
                        };
                        out.push(text(Paint::Text, words.to_owned()));
                    }
                    None if !short => out.push(text(
                        Paint::Text,
                        format!(" · {} left if none fails", plural(left as usize, "round")),
                    )),
                    None => {}
                }
            }
        }
        Stage::Clean => {
            if !short {
                out.push(text(Paint::Track, " · repeats until stopped".to_owned()));
            }
        }
    }
    if let Some(hunt) = s.hunt {
        out.push(text(Paint::Hunt, format!(" · HUNT {}", hunt.plan.number)));
        let place = hunt_stage(scene, short);
        if !place.is_empty() {
            out.push(text(Paint::Hunt, format!(" · {place}")));
        }
    } else if repairing(scene) {
        out.push(text(Paint::Hunt, " · repairing".to_owned()));
    }
    out
}

fn stage_spans(
    scene: &Scene<'_>,
    stage: Stage,
    mark: Mark,
    life: Lifecycle,
    short: bool,
) -> Vec<Span<'static>> {
    let running = mark == Mark::Current && life == Lifecycle::Running;
    let (glyph, name) = match mark {
        Mark::Done => (scene.paint(Paint::Good, "■ "), Paint::Text),
        Mark::Current if running => (scene.paint(Paint::Running, "► "), Paint::White),
        Mark::Current | Mark::Open => (scene.paint(Paint::Track, "○ "), Paint::Grey),
    };
    let mut out = vec![glyph, scene.paint(name, stage.name(short))];
    match stage {
        Stage::Solo if mark != Mark::Done || !short => {
            let total = scene.s.cores.len();
            let paint = if mark == Mark::Done {
                Paint::Grey
            } else {
                Paint::Text
            };
            out.push(scene.paint(paint, format!("  {}/{total}", found(scene))));
        }
        Stage::Solo | Stage::One | Stage::Two | Stage::Clean => {}
    }
    if running {
        out.extend(sub_state(scene, stage, short));
    }
    out
}

/// The trial's time for the views that hide the trial rows.
fn trial_time(scene: &Scene<'_>) -> Option<Span<'static>> {
    if scene.s.dead_end.is_some() {
        return None;
    }
    let trial = scene.s.trial?;
    let started = trial.started?;
    let now = scene.now;
    let (paint, text) = if let Some(hold) = scene.hold.filter(|hold| now < hold.planned_end) {
        (
            Paint::Grey,
            format!(
                "screen paused · planned end {}",
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
    Some(scene.paint(paint, text))
}

/// One stage line in its full or its short names, with or without the trial's time.
fn stage_form(scene: &Scene<'_>, short: bool, summary: bool) -> Line<'static> {
    let life = lifecycle(scene);
    let mut out = Vec::new();
    if let Some((paint, word)) = life.word() {
        out.push(scene.paint(paint, word));
        out.push(Span::raw("  "));
    }
    for (i, (stage, mark)) in STAGES.into_iter().zip(marks(scene)).enumerate() {
        if i > 0 {
            out.push(scene.paint(Paint::Text, " "));
            out.push(scene.paint(Paint::Track, "→"));
            out.push(scene.paint(Paint::Text, " "));
        }
        out.extend(stage_spans(scene, stage, mark, life, short));
    }
    if summary && let Some(time) = trial_time(scene) {
        out.push(scene.paint(Paint::Text, "   "));
        out.push(time);
    }
    Line::from(out)
}

/// The stage line: `SOLO LIMITS → PHASE 1 → PHASE 2 → CLEAN CYCLES` with `■` complete, `►` current and `○` not
/// started. A stop, a dead end or a recovery leads the line and no stage is marked running; a hunt or a repair
/// is a sub-state of the stage it interrupts. `summary` adds the trial's time for the views that hide the trial
/// rows. The full names are used whenever they fit `width`, then the short names, then the short names without
/// the trial's time.
pub(crate) fn stage_line(scene: &Scene<'_>, width: usize, summary: bool) -> Line<'static> {
    if scene.s.session.is_none() {
        return Line::default();
    }
    let mut fitted = None;
    for (short, with_time) in [(false, summary), (true, summary), (true, false)] {
        let line = stage_form(scene, short, with_time);
        if line_width(&line) <= width {
            #[cfg(test)]
            crate::corpus::line(
                "dash.stage",
                &format!(
                    "{}{}",
                    if short { "short" } else { "full" },
                    if with_time { " summary" } else { "" }
                ),
                &line,
            );
            fitted = Some(line);
            break;
        }
    }
    fitted.unwrap_or_else(|| cut_styled(&stage_form(scene, true, false), width))
}

fn sep(scene: &Scene<'_>) -> Line<'static> {
    Line::from(scene.paint(Paint::Grey, " · "))
}

/// A clause that never gives way.
fn whole(scene: &Scene<'_>, paint: Paint, words: impl Into<String>) -> Clause {
    Clause::whole(sep(scene), Line::from(scene.paint(paint, words.into())))
}

/// A clause that shortens through `forms`, each with its rank; an empty form leaves it out.
fn forms(scene: &Scene<'_>, paint: Paint, forms: Vec<(String, u8)>) -> Clause {
    Clause::new(
        sep(scene),
        forms
            .into_iter()
            .map(|(words, rank)| {
                if words.is_empty() {
                    Form::empty()
                } else {
                    Form::new(Line::from(scene.paint(paint, words)), rank)
                }
            })
            .collect(),
    )
}

/// A clause that is left out before anything else gives way.
fn extra(scene: &Scene<'_>, paint: Paint, words: impl Into<String>, rank: u8) -> Clause {
    forms(scene, paint, vec![(words.into(), rank), (String::new(), 0)])
}

fn stage_label(scene: &Scene<'_>, stage: Stage) -> Clause {
    whole(scene, Paint::White, stage.name(false))
}

fn cycle_steps(plan: &togi_tuner::CyclePlan) -> (usize, usize) {
    (
        plan.steps.iter().filter(|step| step.done).count(),
        plan.steps.len(),
    )
}

/// `cycle 1 · 4/18 steps complete`, or that no cycle has begun.
fn cycle_progress(scene: &Scene<'_>) -> Vec<Clause> {
    let Some(cycle) = scene.s.cycle else {
        return vec![whole(scene, Paint::Grey, "waiting for the first cycle")];
    };
    let mut out = Vec::new();
    if let Some(number) = cycle.plan.number {
        out.push(extra(scene, Paint::Text, format!("cycle {number}"), 2));
    }
    let (done, steps) = cycle_steps(cycle.plan);
    if steps > 0 {
        out.push(forms(
            scene,
            Paint::Text,
            vec![
                (format!("{done}/{steps} steps complete"), 1),
                (format!("{done}/{steps} steps"), 0),
            ],
        ));
    }
    out
}

/// The credited and the required checks of the open deepening round.
fn round_checks(scene: &Scene<'_>) -> Option<(u32, u32)> {
    let deepening = scene.s.deepening?;
    let checks = &deepening.checks;
    Some((
        checks
            .iter()
            .map(|check| check.passed.min(check.needed))
            .sum(),
        checks.iter().map(|check| check.needed).sum(),
    ))
}

fn candidates(scene: &Scene<'_>) -> Option<Clause> {
    let phases = scene.s.phases?;
    if phases.candidates.is_empty() {
        return None;
    }
    let all = phases.candidates.len();
    let moving = phases.candidates.iter().filter(|c| c.moving).count();
    Some(forms(
        scene,
        Paint::Text,
        vec![
            (format!("{} · {moving} moving", plural(all, "candidate")), 2),
            (plural(all, "candidate"), 1),
            (String::new(), 0),
        ],
    ))
}

fn hunt_head(scene: &Scene<'_>, plan: &HuntPlan) -> Vec<Clause> {
    let mut out = vec![whole(scene, Paint::Hunt, format!("HUNT {}", plan.number))];
    if let Some(group) = plan.groups.last() {
        out.push(extra(
            scene,
            Paint::Hunt,
            format!("group {}", group.number),
            2,
        ));
    }
    let recut = recut_words(plan.cut, false);
    if !recut.is_empty() {
        out.push(extra(
            scene,
            Paint::Hunt,
            recut.trim_end_matches(" · ").to_owned(),
            3,
        ));
    }
    out
}

fn headline(scene: &Scene<'_>, stage: Stage, life: Lifecycle) -> Vec<Clause> {
    let s = &scene.s;
    let mut out = Vec::new();
    if let Some((paint, word)) = life.word() {
        out.push(whole(scene, paint, word));
    }
    if stage == Stage::Solo {
        let total = s.cores.len();
        out.push(stage_label(scene, stage));
        out.push(forms(
            scene,
            Paint::Text,
            vec![
                (format!("{}/{total} solo limits found", found(scene)), 1),
                (format!("{}/{total} found", found(scene)), 0),
            ],
        ));
        return out;
    }
    out.push(stage_label(scene, stage));
    if let Some(hunt) = s.hunt {
        out.extend(hunt_head(scene, hunt.plan));
        out.extend(enclosing_progress(scene, stage));
        return out;
    }
    if repairing(scene) {
        out.push(forms(
            scene,
            Paint::Hunt,
            vec![
                ("repairing after failure · cycle held".to_owned(), 2),
                ("repairing".to_owned(), 0),
            ],
        ));
        out.extend(enclosing_progress(scene, stage));
        return out;
    }
    match (stage, s.phases) {
        (Stage::Two, Some(phases)) if phases.round.is_some() => {
            out.extend(round_progress(scene, phases.round.unwrap_or(0)));
            out.extend(candidates(scene));
            out.push(extra(
                scene,
                Paint::Grey,
                format!(
                    "{} left if none fails",
                    plural(phases.rounds_left as usize, "round")
                ),
                3,
            ));
        }
        (Stage::Two, Some(phases)) if phases.confirming => {
            out.push(whole(scene, Paint::Text, "confirmation cycle"));
            out.extend(cycle_progress(scene));
        }
        (Stage::Two, Some(phases)) => {
            out.push(whole(scene, Paint::Text, "between rounds"));
            out.extend(candidates(scene));
            out.push(extra(
                scene,
                Paint::Grey,
                format!(
                    "{} left if none fails",
                    plural(phases.rounds_left as usize, "round")
                ),
                3,
            ));
        }
        (Stage::Clean, _) => {
            out.push(clean_recorded(scene));
            out.extend(cycle_progress(scene));
        }
        (Stage::Solo | Stage::One | Stage::Two, _) => out.extend(cycle_progress(scene)),
    }
    out
}

fn clean_recorded(scene: &Scene<'_>) -> Clause {
    let cycles = scene.s.clean_cycles;
    forms(
        scene,
        Paint::Text,
        vec![
            (
                format!("{} recorded", plural(cycles as usize, "clean cycle")),
                1,
            ),
            (format!("{cycles} clean"), 0),
        ],
    )
}

fn round_progress(scene: &Scene<'_>, round: u32) -> Vec<Clause> {
    let mut out = vec![whole(scene, Paint::Text, format!("round {round}"))];
    if let Some((credited, required)) = round_checks(scene) {
        out.push(forms(
            scene,
            Paint::Text,
            vec![
                (format!("{credited}/{required} credited checks"), 1),
                (format!("{credited}/{required} checks"), 0),
            ],
        ));
    }
    out
}

/// What the enclosing stage has recorded, kept beside a hunt or a repair: the clean cycles recorded, the open
/// round and its credited checks, or the cycle's steps.
fn enclosing_progress(scene: &Scene<'_>, stage: Stage) -> Vec<Clause> {
    match (stage, scene.s.phases.and_then(|phases| phases.round)) {
        (Stage::Two, Some(round)) => round_progress(scene, round),
        (Stage::Clean, _) => {
            let mut out = vec![clean_recorded(scene)];
            out.extend(cycle_progress_tail(scene));
            out
        }
        (Stage::Solo | Stage::One | Stage::Two, _) => {
            cycle_progress_tail(scene).into_iter().collect()
        }
    }
}

/// The cycle's steps under a hunt or a repair, the first thing the headline gives up.
fn cycle_progress_tail(scene: &Scene<'_>) -> Option<Clause> {
    let cycle = scene.s.cycle?;
    let (done, steps) = cycle_steps(cycle.plan);
    (steps > 0).then(|| {
        extra(
            scene,
            Paint::Text,
            format!("{done}/{steps} steps complete"),
            1,
        )
    })
}

/// The step checking works in: the running trial's, else where checking resumes.
fn working_step(scene: &Scene<'_>, cycle: &CycleView<'_>) -> usize {
    let steps = cycle.plan.steps.len();
    let placed = scene.s.trial.and_then(|trial| {
        let place = trial.cycle?;
        (Some(place.cycle) == cycle.plan.number.map(std::num::NonZeroU32::get))
            .then_some(place.step? as usize)
            .filter(|&step| step > 0)
            .map(|step| step - 1)
    });
    placed
        .unwrap_or_else(|| cycle.resume_step())
        .min(steps.saturating_sub(1))
}

/// `step 5 · 3/4 credited checks · 1 check still needed`, for the step checking works in.
fn step_requirement(scene: &Scene<'_>, cycle: &CycleView<'_>) -> Vec<Clause> {
    let steps = &cycle.plan.steps;
    let at = working_step(scene, cycle);
    let Some(step) = steps.get(at) else {
        return Vec::new();
    };
    let mut out = vec![whole(scene, Paint::White, format!("step {}", at + 1))];
    out.extend(part_requirement(scene, step));
    out
}

fn part_requirement(scene: &Scene<'_>, step: &CycleStep) -> Vec<Clause> {
    let mut out = Vec::new();
    let known = step.parts.len();
    let open = first_open_part(step);
    if step.regime == togi_machine::Regime::R7 && !step.chains_complete {
        out.push(forms(
            scene,
            Paint::Text,
            vec![
                (
                    format!("{known} known {} · more may follow", noun(known, "part")),
                    2,
                ),
                (format!("{known}+ parts"), 1),
                (String::new(), 0),
            ],
        ));
    } else if let (true, Some(part)) = (known > 1, open) {
        out.push(extra(
            scene,
            Paint::Text,
            format!("part {part} of {known}"),
            1,
        ));
    }
    let Some(part) = open.and_then(|number| step.parts.get(number - 1)) else {
        return out;
    };
    let required = part.short + part.long;
    if required == 0 {
        return out;
    }
    let credited = part.passed.min(required);
    out.push(forms(
        scene,
        Paint::Text,
        vec![
            (format!("{credited}/{required} credited checks"), 2),
            (format!("{credited}/{required} credited"), 1),
            (format!("{credited}/{required}"), 0),
        ],
    ));
    if credited < required {
        let missing = (required - credited) as usize;
        out.push(extra(
            scene,
            Paint::Grey,
            format!("{} still needed", plural(missing, "check")),
            3,
        ));
    }
    out
}

/// The part of the hunt's current split being checked, counted from 1, and how many parts the split has.
fn split_part(plan: &HuntPlan) -> Option<(usize, usize)> {
    let parts = &plan.parts;
    parts
        .iter()
        .position(|part| part.running)
        .or_else(|| {
            parts.iter().position(|part| {
                part.group.is_some() && part.outcome == Some(togi_tuner::GroupState::Running)
            })
        })
        .or_else(|| parts.iter().position(|part| part.group.is_none()))
        .map(|at| (at + 1, parts.len()))
}

fn hunt_requirement(scene: &Scene<'_>, hunt: &crate::model::HuntView<'_>) -> Vec<Clause> {
    let plan = hunt.plan;
    let mut out = Vec::new();
    if !plan.probes.is_empty() {
        let at = plan
            .probes
            .iter()
            .position(|probe| !probe.done)
            .map_or(plan.probes.len(), |index| index + 1);
        out.push(whole(
            scene,
            Paint::Hunt,
            format!("member {at}/{} probes", plan.probes.len()),
        ));
    } else if hunt.locating() {
        out.push(whole(
            scene,
            Paint::Hunt,
            "locating: the failing load reruns at its failing offsets",
        ));
    } else if let Some((at, of)) = split_part(plan) {
        out.push(whole(
            scene,
            Paint::Hunt,
            format!("part {at}/{of} of this split"),
        ));
    } else if plan.groups.is_empty() {
        out.push(whole(scene, Paint::Hunt, "starting"));
    }
    out.push(extra(scene, Paint::Grey, "total hunt work unknown", 1));
    out
}

fn search_requirement(scene: &Scene<'_>) -> Vec<Clause> {
    let alone = scene
        .s
        .trial
        .filter(|trial| trial.condition == Condition::Alone && !trial.crashed);
    let Some(trial) = alone else {
        return vec![whole(
            scene,
            Paint::Grey,
            "waiting for the next search trial",
        )];
    };
    let verb = if trial
        .core
        .and_then(|core| scene.core(core))
        .is_some_and(|core| core.confirm.is_some())
    {
        "confirming"
    } else {
        "searching"
    };
    let mut out = vec![whole(scene, Paint::White, format!("current core {verb}"))];
    if trial.count.of > 0 {
        out.push(whole(
            scene,
            Paint::Text,
            format!(
                "{}/{} credited checks",
                trial.count.passed.min(trial.count.of),
                trial.count.of
            ),
        ));
    }
    out
}

fn round_requirement(scene: &Scene<'_>) -> Vec<Clause> {
    let Some(deepening) = scene.s.deepening else {
        return Vec::new();
    };
    let checks = &deepening.checks;
    let at = checks
        .iter()
        .position(|check| check.running)
        .or_else(|| checks.iter().position(|check| check.passed < check.needed));
    let Some((index, check)) = at.and_then(|index| Some((index, checks.get(index)?))) else {
        return vec![whole(
            scene,
            Paint::Grey,
            "every check of the round is credited",
        )];
    };
    vec![
        whole(
            scene,
            Paint::White,
            format!("check {} of {}", index + 1, checks.len()),
        ),
        extra(
            scene,
            Paint::Text,
            format!("{} {}", check.regime, kind_words(check.regime)),
            2,
        ),
        extra(scene, Paint::Text, on_cores(check.cores), 1),
        whole(
            scene,
            Paint::Text,
            format!(
                "{}/{} credited checks",
                check.passed.min(check.needed),
                check.needed
            ),
        ),
    ]
}

fn queued_resets(scene: &Scene<'_>) -> Option<Clause> {
    let cores: CoreSet = scene
        .s
        .cores
        .iter()
        .filter(|core| core.queued.is_some())
        .map(|core| core.id)
        .collect();
    (!cores.is_empty()).then(|| {
        whole(
            scene,
            Paint::Text,
            format!("reset queued for {}", cores_named(cores)),
        )
    })
}

/// What a lifecycle that suspends the stages says under the last work: why, and what stays queued.
fn lifecycle_reason(scene: &Scene<'_>, life: Lifecycle) -> Vec<Clause> {
    let s = &scene.s;
    let mut out = Vec::new();
    match life {
        Lifecycle::Running => {}
        Lifecycle::Stopped => {
            out.extend(queued_resets(scene));
            out.push(extra(
                scene,
                Paint::Grey,
                format!("BIOS profile: page {}", View::Bios.number()),
                1,
            ));
        }
        Lifecycle::DeadEnd => {
            if let Some(dead_end) = s.dead_end {
                out.push(whole(scene, Paint::Bad, dead_end_words(dead_end.condition)));
            }
            out.extend(queued_resets(scene));
        }
        Lifecycle::Recovery => {
            if let Some(recover) = s.recover {
                out.push(whole(
                    scene,
                    Paint::Text,
                    format!("crash detected {}", scene.wall_minute(recover.crash_at)),
                ));
            }
            if let Some(trial) = s.trial.filter(|trial| trial.crashed) {
                out.push(whole(
                    scene,
                    Paint::Text,
                    format!("trial {} has no recorded end", trial.id),
                ));
            }
        }
    }
    out
}

/// A failed trial whose rerun the tuner runs next: the failure stands until that rerun passes clean.
fn pending_rerun(scene: &Scene<'_>) -> Option<Vec<Clause>> {
    let s = &scene.s;
    if s.trial.is_some() {
        return None;
    }
    let next = s.forecast.between.as_ref()?.next.as_ref()?;
    let rerun = match next.requirement.context {
        TrialContext::Rerun { .. } => "a clean rerun is needed",
        TrialContext::ZeroRerun { .. } => "a clean rerun with every core at 0 is needed",
        TrialContext::Search { .. }
        | TrialContext::SoloLimit { .. }
        | TrialContext::Cycle { .. }
        | TrialContext::Round { .. }
        | TrialContext::Hunt { .. } => return None,
    };
    let failure = match s.last.and_then(|end| end.core) {
        Some(core) => format!("failure on core {:02}", core.get()),
        None => "failure recorded".to_owned(),
    };
    Some(vec![
        whole(scene, Paint::Text, failure),
        whole(scene, Paint::Text, rerun),
    ])
}

fn requirement(scene: &Scene<'_>, stage: Stage, life: Lifecycle) -> Vec<Clause> {
    let s = &scene.s;
    if life != Lifecycle::Running {
        return lifecycle_reason(scene, life);
    }
    if let Some(hunt) = s.hunt {
        return hunt_requirement(scene, &hunt);
    }
    if stage == Stage::Solo {
        return search_requirement(scene);
    }
    if stage == Stage::Two && s.phases.is_some_and(|phases| phases.round.is_some()) {
        return round_requirement(scene);
    }
    if !repairing(scene)
        && let Some(rerun) = pending_rerun(scene)
    {
        return rerun;
    }
    let Some(cycle) = s.cycle else {
        return Vec::new();
    };
    let steps = cycle.plan.steps.len();
    if s.trial.is_none() && steps > 0 && cycle.plan.steps.iter().all(|step| step.done) {
        return vec![whole(scene, Paint::Grey, "every step of the cycle is done")];
    }
    if repairing(scene) {
        let closure = (cycle.plan.current + 1).min(steps);
        if cycle.resume_step() >= steps {
            if steps == 0 {
                return Vec::new();
            }
            return vec![
                whole(
                    scene,
                    Paint::White,
                    format!("chain closure at step {closure} of {steps}"),
                ),
                whole(
                    scene,
                    Paint::Text,
                    "every step is done · the cycle ends once the rerun passes",
                ),
            ];
        }
        let resume = cycle.resume_step() + 1;
        let resumes = whole(
            scene,
            Paint::Text,
            format!("checking resumes at step {resume} of {steps}"),
        );
        if closure == resume {
            return vec![resumes];
        }
        return vec![
            whole(
                scene,
                Paint::White,
                format!("chain closure at step {closure} of {steps}"),
            ),
            resumes,
        ];
    }
    step_requirement(scene, &cycle)
}

/// Main's phase headline and its current requirement, each fitted to `width` cells.
///
/// The headline counts the phase's own work in cores, steps or credited checks, never time; the second row names
/// the requirement checking works on now. A stop, a dead end or a recovery keeps the last work static and gives
/// its reason in place of the requirement.
pub(crate) fn phase_progress(scene: &Scene<'_>, width: usize) -> [Line<'static>; 2] {
    if scene.s.session.is_none() {
        return [Line::default(), Line::default()];
    }
    let life = lifecycle(scene);
    let stage = current(scene);
    let rows = [
        fit_clauses(width, &headline(scene, stage, life)),
        fit_clauses(width, &requirement(scene, stage, life)),
    ];
    #[cfg(test)]
    crate::corpus::lines("dash.progress", "", &rows);
    rows
}

#[cfg(test)]
pub(super) mod tests;
