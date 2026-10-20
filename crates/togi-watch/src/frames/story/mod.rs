//! The narration: why the screen looks the way it does, what the last trial did, what each outcome of the trial
//! in flight leads to, and the resting band when no trial runs.

#[cfg(test)]
mod corpus;
mod outcome;
mod resting;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::time::Duration;

use ratatui::text::{Line, Span};
use togi_journal::{Activity, DeadEndCondition, ShutdownReason};
use togi_machine::{Condition, CoreSet, PerCore, Regime, Signal, TargetOffset};
use togi_tuner::PhaseNo;

use super::colours::Paint;
use super::fit::{cut_styled, display_width, inert, wrap_styled};
use super::scene::{Scene, clock};
use super::terms::{recut_words, trial_name};
use crate::model::{
    HuntCause, HuntShape, HuntView, Singleton, SingletonStage, TrialEndView, TrialView,
};
use crate::screen::Colours;
use crate::words::{
    core_ids, core_list, kind_words, lateness, plural, signal_words, workload_display,
};

pub(crate) use outcome::outcome_lines;
pub(crate) use resting::{last_lines, resting_band};

/// What the narrator says: why the screen looks the way it does, never what the trial band or the outcome lines
/// already say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Story {
    /// The stage it speaks for: `CYCLE 2`, `HUNT 1`.
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

const PARTIAL_NOTE: &str = "This partial idles the top-requester groups found so far. A backoff re-derives it; an unchanged loaded set keeps its passes. Passes and failures count as ordinary evidence.";

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
    if let Some(dead_end) = s.dead_end {
        return dead_end_story(dead_end.condition);
    }
    if let Some(trial) = s.trial.filter(|trial| trial.crashed) {
        return Story::one(
            "CRASHED",
            format!(
                "Trial {} crashed: a later boot has written events. togi run records the crash.",
                trial.id
            ),
            Paint::Hunt,
        );
    }
    if let Some(stopped) = s.stopped {
        return stopped_story(stopped.reason, stopped.saved);
    }
    if s.recover.is_some() {
        return recover_story(scene);
    }
    let Some(trial) = s.trial else {
        let text = if s.last.is_none() {
            "No trial has run yet; the tuner records what it decides before the first one starts."
        } else {
            "The last trial has ended; the tuner records what it decided before the next one starts."
        };
        return Story::one(stage_label(scene), text.to_owned(), Paint::Running);
    };
    trial_story(scene, trial)
}

fn trial_story(scene: &Scene<'_>, trial: &TrialView) -> Story {
    let s = &scene.s;
    if trial.started.is_none() {
        return Story::one(
            stage_label(scene),
            "The trial's intent is recorded; its offsets are being applied and its workload is starting.".to_owned(),
            Paint::Running,
        );
    }
    let past = trial.past_end(scene.now);
    if !past.is_zero() {
        return Story::one(
            stage_label(scene),
            format!(
                "{} past its planned end, no trial end recorded. Its deadline or watchdog may still be running.",
                clock(past)
            ),
            Paint::Running,
        );
    }
    if trial.zero_rerun() {
        return Story::new(
            "RERUN AT OFFSET 0",
            vec![
                "A failure at offset 0 would stop tuning. Before it does, the failed load runs again with every core at offset 0.".to_owned(),
                "If it fails too, the instability is not caused by Curve Optimizer and tuning stops; if it passes, the failure goes to the cores off offset 0.".to_owned(),
            ],
            "The failed load runs again with every core at offset 0; tuning stops if it fails.",
            Paint::Running,
        );
    }
    if let Some(hunt) = s.hunt {
        return hunt_story(scene, hunt, trial);
    }
    if let (Condition::Alone, None, Some(core), Some(offset)) =
        (trial.condition, trial.round, trial.core, trial.offset)
    {
        let id = core.get();
        let view = scene.core(core);
        let text = match view.and_then(|view| view.confirm.map(|confirm| (confirm, view.fail))) {
            Some((confirm, Some(fail))) => format!(
                "Core {id:02} failed at {}, so {} is its candidate solo limit, and I'm confirming it.",
                fail.get(),
                confirm.offset.get()
            ),
            Some((confirm, None)) => format!(
                "{} is core {id:02}'s candidate solo limit, and I'm confirming it.",
                confirm.offset.get()
            ),
            None => format!(
                "I'm finding core {id:02}'s solo limit, one turn at a time, now at {}.",
                offset.get()
            ),
        };
        return Story::new(
            "SOLO LIMITS",
            vec![
                text.clone(),
                "One core at a time with the rest at 0, so any failure belongs to the loaded core."
                    .to_owned(),
            ],
            text,
            Paint::Running,
        );
    }
    if trial.rerun {
        return rerun_story(scene);
    }
    if trial.round.is_some() || deepening(scene) {
        return round_story(scene, trial);
    }
    cycle_story(scene, trial)
}

fn rerun_story(scene: &Scene<'_>) -> Story {
    let mut lines = vec![
        "The failed load runs again after the offsets changed. This trial checks the new profile."
            .to_owned(),
    ];
    let mut brief = "The failed load runs again on the new profile.".to_owned();
    if let Some(cycle) = scene.s.cycle {
        let resume = cycle.resume_step();
        if resume < cycle.plan.steps.len() {
            let number = cycle.plan.number.map_or(0, std::num::NonZeroU32::get);
            let step = resume + 1;
            lines.push(format!(
                "Cycle {number} waits at step {step} until the rerun passes."
            ));
            brief = format!(
                "The failed load runs again on the new profile; cycle {number} waits at step {step} until the rerun passes."
            );
        }
    }
    Story::new("RERUN", lines, brief, Paint::Running)
}

fn round_story(scene: &Scene<'_>, trial: &TrialView) -> Story {
    let round = trial
        .round
        .or_else(|| scene.s.deepening.map(|view| view.round.get()));
    let label = round.map_or_else(
        || "PHASE 2".to_owned(),
        |round| format!("PHASE 2 · ROUND {round}"),
    );
    if let (Condition::Alone, Some(core), Some(offset)) =
        (trial.condition, trial.core, trial.offset)
    {
        let (id, offset) = (core.get(), offset.get());
        return Story::new(
            label,
            vec![
                format!(
                    "Core {id:02} runs alone at its proposed {offset}, like every moved core's light and heavy checks."
                ),
                "R7 then checks the parts where a moved core is a top requester.".to_owned(),
            ],
            format!("Round check: core {id:02} alone at its proposed {offset}."),
            Paint::Running,
        );
    }
    let round = round.unwrap_or_default();
    Story::new(
        label,
        vec![
            round_moves(scene, round),
            "The checks below distinguish cores that stay moved from members that yield shallower."
                .to_owned(),
        ],
        format!("Checking round {round}'s proposed profile."),
        Paint::Running,
    )
}

/// What the open round moves and the most that is left after it.
fn round_moves(scene: &Scene<'_>, round: u32) -> String {
    let (left, moving) = scene.s.phases.map_or((0, CoreSet::default()), |phases| {
        let moving = phases
            .candidates
            .iter()
            .filter(|candidate| candidate.moving)
            .map(|candidate| candidate.core)
            .collect::<CoreSet>();
        (phases.rounds_left, moving)
    });
    let what = if moving.is_empty() {
        format!("Round {round} checks again the offsets kept from the round that ended unpassed")
    } else {
        format!(
            "Round {round} moves {} one count toward their solo limits",
            core_list(moving)
        )
    };
    format!(
        "{what}; {} left if none fails, then one full cycle.",
        plural(left as usize, "round")
    )
}

fn cycle_story(scene: &Scene<'_>, trial: &TrialView) -> Story {
    let s = &scene.s;
    let place = trial.cycle;
    let cycle = place
        .map(|place| place.cycle)
        .or_else(|| {
            s.cycle
                .and_then(|cycle| cycle.plan.number.map(std::num::NonZeroU32::get))
        })
        .unwrap_or_default();
    let name = format!("CYCLE {cycle}");
    let step = place
        .and_then(|place| place.step)
        .map_or_else(|| "This step".to_owned(), |step| format!("Step {step}"));
    if trial.regime == Regime::R6 {
        if scene.hold.is_some() {
            return Story::new(
                name,
                vec![
                    format!(
                        "{step} leaves every core idle for {}, with short wake-ups, to test idle and boost states.",
                        minutes(trial.duration)
                    ),
                    "This screen holds still while the idle trial is in flight, clock included, so drawing it cannot wake the cores.".to_owned(),
                ],
                "Every core idle with short wake-ups; this screen holds still.",
                Paint::Running,
            );
        }
    }
    let kind = format!("{} {}", trial.regime, kind_words(trial.regime));
    let what = format!(
        "{step} is {kind} with {}.",
        workload_display(trial.workload)
    );
    let mut brief = what.clone();
    let mut place_words = match s.phases.map(|phases| phases.phase) {
        Some(PhaseNo::One) => "Every core runs at its profile offset. Phase 1's cycle: its pass confirms the first BIOS profile.".to_owned(),
        Some(PhaseNo::Two) => "Every core runs at its profile offset. Phase 2's confirmation cycle: its pass concludes phase 2.".to_owned(),
        Some(PhaseNo::Concluded) | None => "Every core runs at its profile offset. The cycle keeps testing until stopped.".to_owned(),
    };
    if let Some(place) = place.filter(|place| place.parts > 1) {
        let part = place.part.unwrap_or_default();
        let loads = format!(
            "{part} of {} loads {} for {}.",
            place.parts,
            core_ids(trial.cores),
            minutes(trial.duration)
        );
        place_words = format!("Part {loads}");
        brief = format!("{step}, {kind}: part {loads}");
    }
    if trial.partial {
        PARTIAL_NOTE.clone_into(&mut place_words);
        brief = format!(
            "{step}, R7 partial loads {}; passes and failures count.",
            core_ids(trial.cores)
        );
    } else if s.clean_cycles > 0 {
        let count = if s.clean_cycles == 1 {
            "1 clean cycle counts".to_owned()
        } else {
            format!("{} clean cycles count", s.clean_cycles)
        };
        place_words =
            format!("{count} for this profile. Passing trials cannot prove it will never fail.");
    }
    Story::new(name, vec![what, place_words], brief, Paint::Running)
}

fn deepening(scene: &Scene<'_>) -> bool {
    scene
        .s
        .session
        .is_some_and(|session| session.activity == Some(Activity::Deepening))
}

/// The stage the tuner is in, for a narrator with no trial to speak of.
fn stage_label(scene: &Scene<'_>) -> String {
    let s = &scene.s;
    if let Some(hunt) = s.hunt {
        return format!("HUNT {}", hunt.plan.number);
    }
    if !s.turns.is_empty() {
        return "SOLO LIMITS".to_owned();
    }
    if deepening(scene) {
        return "PHASE 2".to_owned();
    }
    match s.cycle.and_then(|cycle| cycle.plan.number) {
        Some(number) => format!("CYCLE {number}"),
        None => "TOGI".to_owned(),
    }
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

fn stopped_story(reason: ShutdownReason, saved: bool) -> Story {
    let text = match reason {
        ShutdownReason::FirstResult => {
            "The BIOS profile is confirmed, so I stopped at the first result as asked. togi status shows it; togi run continues with phase 2."
        }
        ShutdownReason::Cycles if saved => {
            "The requested clean cycles are complete, so I restored baseline or shallower offsets and stopped. togi run picks up where I left off."
        }
        ShutdownReason::Cycles => {
            "The requested clean cycles are complete, so I've stopped. Everything I learned is in the journal; togi run picks up where I left off."
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
            "A core failed even at offset 0, so the problem isn't Curve Optimizer. Check the rest of the system before tuning again."
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
            "An earlier build may have moved offsets deeper than proven. Answer the reset question at a terminal before tuning continues."
        }
        DeadEndCondition::ThermalTrip => {
            "The CPU shut down from heat. Check the cooling before tuning again."
        }
    };
    Story::one("DEAD END", why.to_owned(), Paint::Bad)
}

fn parked_word(parked_zero: bool) -> &'static str {
    if parked_zero { "parked at 0" } else { "parked" }
}

fn recover_story(scene: &Scene<'_>) -> Story {
    let s = &scene.s;
    let Some(recover) = s.recover else {
        return Story::one(stage_label(scene), String::new(), Paint::Running);
    };
    if let Some(hunt) = s.hunt.filter(|hunt| {
        hunt.facts
            .started
            .is_some_and(|started| started >= recover.crash_at)
    }) {
        let parked = parked_word(hunt.facts.parked_at_zero());
        let candidates = core_ids(hunt.plan.candidates);
        return Story::new(
            format!("HUNT {}", hunt.plan.number),
            vec![
                format!(
                    "I look for the cause among {candidates}, the cores with an offset in the failing profile."
                ),
                format!(
                    "I rerun that load with part of them at their failing offsets and the rest {parked}."
                ),
            ],
            format!("To find the cause, I rerun that load with part of {candidates} {parked}."),
            Paint::Hunt,
        );
    }
    let mut lines = vec![
        "I picked up from the journal after the reboot; nothing it recorded is lost.".to_owned(),
    ];
    if let Some(core) = recover.end.as_ref().and_then(|end| end.core) {
        lines.push(format!(
            "Core {:02} was named: its failure point is recorded and it backs off.",
            core.get()
        ));
    }
    let brief = lines[0].clone();
    Story::new("RECOVERED", lines, brief, Paint::Hunt)
}

fn hunt_story(scene: &Scene<'_>, hunt: HuntView<'_>, trial: &TrialView) -> Story {
    let label = format!("HUNT {}", hunt.plan.number);
    if let Some(probe) = trial.hunt.and_then(|place| place.probe) {
        let mut group = CoreSet::default();
        let mut signal = None;
        for g in hunt.plan.groups.iter().filter(|g| g.probe.is_none()) {
            group = g.cores;
            if g.outcome == togi_tuner::GroupState::Failed {
                signal = hunt.group_signal(g.number);
            }
        }
        if !hunt.plan.probes.is_empty() {
            group = hunt.plan.probes.iter().map(|probe| probe.member).collect();
        }
        let verb = if signal == Some(Signal::Crash) {
            "crashed"
        } else {
            "failed"
        };
        let (id, offset) = (probe.core.get(), probe.offset.get());
        if let Some(single) = hunt.singleton() {
            return singleton_probe_story(label, &single, verb, probe);
        }
        let which = if hunt.plan.probes.first().map(|first| first.member) == Some(probe.core) {
            format!("Core {id:02} first.")
        } else {
            format!("Now core {id:02}.")
        };
        let members = core_ids(group);
        return Story::new(
            label,
            vec![
                format!(
                    "{members} {verb} together at their offsets, and every smaller part of them that was tested passed."
                ),
                format!(
                    "So I probe each member: how shallow must it go for the combination to pass? {which}"
                ),
            ],
            format!("Probing members of {members}: core {id:02} at {offset}."),
            Paint::Hunt,
        );
    }
    let (cause, named, brief) = hunt_cause_story(&hunt.facts.cause);
    if hunt.locating() {
        let candidates = core_ids(hunt.plan.candidates);
        return Story::new(
            label,
            vec![
                cause,
                format!(
                    "{named}I rerun that load with {candidates} at 0 and the loaded cores at their offsets: a pass puts the failure on the idle cores."
                ),
            ],
            format!("{brief} Rerunning it with {candidates} at 0."),
            Paint::Hunt,
        );
    }
    Story::new(
        label,
        vec![
            cause,
            format!(
                "{named}I rerun that load with {}.",
                split_words(scene, hunt, trial)
            ),
        ],
        format!(
            "{brief} Rerunning it with {} parked.",
            core_ids(trial.parked)
        ),
        Paint::Hunt,
    )
}

/// The story of a singleton hunt's culprit while its probe runs: it failed alone, so no combination and no
/// "every smaller part passed" is claimed, and a pass is claimed only once the anchor has one.
fn singleton_probe_story(
    label: String,
    single: &Singleton<'_>,
    verb: &str,
    probe: togi_journal::Member,
) -> Story {
    let (id, offset) = (probe.core.get(), probe.offset.get());
    let failed = format!(
        "Core {id:02} {verb} on its own at {}, so it is the culprit.",
        single.probe.failed_at.first().map_or(offset, |at| at.get())
    );
    let (second, brief) = match single.stage {
        SingletonStage::Anchor => (
            format!(
                "No pass is known at its parked offset yet, and CO 0 is not assumed to pass, so I test core {id:02} at {offset} to find a safe anchor."
            ),
            format!("Testing core {id:02} at {offset}, its parked offset, for a safe anchor."),
        ),
        SingletonStage::Interior => (
            format!(
                "A passing anchor is known, so I probe core {id:02} between its failure and that anchor to narrow its failure point."
            ),
            format!("Probing core {id:02} at {offset} between its failure and its passing anchor."),
        ),
        SingletonStage::Bracketed => (
            format!(
                "A pass sits one count shallower than its failure point, so core {id:02} is bracketed."
            ),
            format!("Core {id:02} is bracketed; its failure point stands."),
        ),
        SingletonStage::Stopped => (
            format!(
                "Probing stopped without a safe bracket, so I keep the shallowest failure I reproduced for core {id:02}."
            ),
            format!("Probing core {id:02} stopped; the shallowest reproduced failure stands."),
        ),
    };
    Story::new(label, vec![failed, second], brief, Paint::Hunt)
}

/// How the hunt splits its candidates in the trial in flight.
fn split_words(scene: &Scene<'_>, hunt: HuntView<'_>, trial: &TrialView) -> String {
    if let Some(part) = hunt.plan.parts.iter().find(|part| part.running) {
        let zero = part.parked.iter().all(|core| {
            scene.s.order.contains(&core) && trial.profile.get(core) == Some(TargetOffset::ZERO)
        });
        return format!(
            "{} at their failing offsets, {} {}",
            core_ids(part.failing),
            core_ids(part.parked),
            parked_word(zero)
        );
    }
    format!(
        "part of {} at its offsets, the rest {}",
        core_ids(hunt.plan.candidates),
        parked_word(hunt.facts.parked_at_zero())
    )
}

/// What started the hunt: a sentence, whether a core was named, and a compact line.
fn hunt_cause_story(cause: &HuntCause) -> (String, String, String) {
    let (named, name_brief) = cause.core.map_or_else(
        || {
            (
                "No core was named: ".to_owned(),
                "; no core named.".to_owned(),
            )
        },
        |core| {
            let id = core.get();
            (
                format!("Core {id:02} was named: "),
                format!("; core {id:02} named."),
            )
        },
    );
    if cause.known {
        let text = format!("{}.", capitalize(&known_words(cause)));
        return (text.clone(), named, text);
    }
    let Some(trial) = &cause.trial else {
        return match cause.signal {
            Some(
                signal @ (Signal::ComputationError
                | Signal::UnexpectedExit
                | Signal::Stall
                | Signal::CorrectedMce
                | Signal::UncorrectedMce),
            ) => (
                format!(
                    "With no trial running and the offsets applied, the machine reported a {}.",
                    signal_words(signal)
                ),
                named,
                format!(
                    "{} while idle{name_brief}",
                    capitalize(signal_words(signal))
                ),
            ),
            Some(Signal::Crash) | None => (
                "The machine crashed while idle, with no trial running, and rebooted.".to_owned(),
                named,
                format!("Crashed while idle{name_brief}"),
            ),
        };
    };
    let mut what = format!(
        "the {} {} trial on {}",
        length_words(trial.duration),
        kind_words(trial.regime),
        core_ids(trial.cores)
    );
    if cause.rerun_of {
        what = format!("the rerun of {what}");
    }
    let name = trial_name(trial);
    match cause.signal {
        Some(Signal::Crash) => {
            let end = cause.end.as_ref();
            let when = lateness(
                end.and_then(|end| end.last_sample),
                end.and_then(|end| end.planned),
            );
            let brief = match when.split_whitespace().next() {
                Some(word) if when != "during" => format!("Crashed {word}"),
                _ => "Crashed".to_owned(),
            };
            (
                format!("The machine crashed {when} {what} and rebooted."),
                named,
                format!("{brief} in {name}{name_brief}"),
            )
        }
        Some(signal) => (
            format!("{} ended with {}.", capitalize(&what), signal_words(signal)),
            named,
            format!("{} in {name}{name_brief}", capitalize(signal_words(signal))),
        ),
        None => (
            format!("{} failed.", capitalize(&what)),
            named,
            format!("Failed in {name}{name_brief}"),
        ),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A trial's planned length as an adjective: `10-minute`.
fn length_words(span: Duration) -> String {
    let secs = span.as_secs();
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}-minute", secs / 60)
    } else {
        format!("{secs}-second")
    }
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

/// Where the trial in flight loads, with how a hunt sets the cores it does not judge: `on 00-07 · …`. Raw.
pub(crate) fn trial_where(scene: &Scene<'_>, trial: &TrialView) -> String {
    if let (Condition::Alone, Some(core), Some(offset)) =
        (trial.condition, trial.core, trial.offset)
    {
        return format!("on core {:02} alone at {}", core.get(), offset.get());
    }
    let place = format!("on {}", core_ids(trial.cores));
    if let Some(shape) = trial.hunt.and_then(|hunt| scene.s.shapes.get(&hunt.hunt)) {
        let shape = shape_words(scene, trial.cores, &trial.profile, shape);
        if !shape.is_empty() {
            return format!("{place} · {shape}");
        }
    }
    let cores = scene.s.cores.len();
    if trial.cycle.is_some_and(|place| place.parts > 0)
        && trial.regime == Regime::R7
        && cores > 0
        && trial.cores.len() == cores
    {
        return format!("{place}, every CCD");
    }
    place
}

/// Which cores a hunt trial runs at their failing offsets and which it parks, and any core it moves elsewhere,
/// such as a probed member.
fn shape_words(
    scene: &Scene<'_>,
    loaded: CoreSet,
    profile: &PerCore<TargetOffset>,
    shape: &HuntShape,
) -> String {
    let mut failing = CoreSet::default();
    let mut parked = CoreSet::default();
    let mut idle = 0;
    let mut moved = Vec::new();
    let mut zero = true;
    for &id in scene.s.order {
        let (Some(at), Some(fails), Some(parks)) =
            (profile.get(id), shape.failing.get(id), shape.parked.get(id))
        else {
            continue;
        };
        if fails == parks {
            continue;
        }
        if at == fails {
            failing.insert(id);
        } else if at == parks {
            parked.insert(id);
            zero &= parks == TargetOffset::ZERO;
            if !loaded.contains(id) {
                idle += 1;
            }
        } else {
            moved.push(format!("core {:02} at {}", id.get(), at.get()));
        }
    }
    if failing.is_empty() && parked.is_empty() && moved.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    if (failing != loaded || idle < parked.len()) && !failing.is_empty() {
        parts.push(format!("{} at failing offsets", core_ids(failing)));
    }
    parts.extend(moved);
    if !parked.is_empty() {
        let mut text = format!("{} parked", core_ids(parked));
        if zero {
            text.push_str(" at 0");
        }
        if idle == parked.len() {
            text.push_str(", idle");
        }
        parts.push(text);
    }
    parts.join(", ")
}

/// What the trial in flight does, in capitals and painted: `CYCLE 2 · STEP 3 · PART 1`, `HUNT 1 · GROUP 2`.
pub(crate) fn operation(scene: &Scene<'_>, trial: &TrialView) -> Line<'static> {
    let alone = match (trial.condition, trial.core, trial.offset) {
        (Condition::Alone, Some(core), Some(offset)) => Some((core, offset)),
        (Condition::Together | Condition::Parked, _, _)
        | (Condition::Alone, None, _)
        | (Condition::Alone, Some(_), None) => None,
    };
    let (mut text, paint) = if let Some(hunt) = trial.hunt {
        let mut text = format!("HUNT {}", hunt.hunt);
        if let Some(probe) = hunt.probe {
            let _ = write!(
                text,
                " · GROUP {} · CORE {:02} AT {}",
                hunt.group,
                probe.core.get(),
                probe.offset.get()
            );
        } else if let Some(part) = hunt.part.filter(|_| hunt.parts > 0) {
            let _ = write!(
                text,
                " · {}PART {part} OF {}",
                recut_words(hunt.cut, true),
                hunt.parts
            );
        } else if hunt.group > 0 {
            let _ = write!(text, " · GROUP {}", hunt.group);
        }
        (text, Paint::Hunt)
    } else if let Some((core, offset)) = alone.filter(|_| trial.round.is_none()) {
        let verb = if scene.core(core).is_some_and(|view| view.confirm.is_some()) {
            "CONFIRM"
        } else {
            "SEARCH"
        };
        (
            format!(
                "{verb} CORE {:02} AT {}{}",
                core.get(),
                offset.get(),
                load_word(trial.regime)
            ),
            Paint::Running,
        )
    } else if trial.zero_rerun() {
        ("RERUN AT OFFSET 0".to_owned(), Paint::Running)
    } else if trial.rerun {
        ("RERUN AFTER BACKOFF".to_owned(), Paint::Running)
    } else if let Some(round) = trial.round {
        let mut text = format!("PHASE 2 · ROUND {round}");
        if let Some((core, offset)) = alone {
            let _ = write!(
                text,
                " · CORE {:02} AT {}{}",
                core.get(),
                offset.get(),
                load_word(trial.regime)
            );
        }
        (text, Paint::Running)
    } else {
        let place = trial.cycle;
        let mut text = format!("CYCLE {}", place.map_or(0, |place| place.cycle));
        if let Some(step) = place.and_then(|place| place.step) {
            let _ = write!(text, " · STEP {step}");
        }
        if let Some(part) = place
            .filter(|place| place.parts > 1)
            .and_then(|place| place.part)
        {
            let _ = write!(text, " · PART {part}");
        }
        (text, Paint::Running)
    };
    if trial.partial {
        text.push_str(" · PARTIAL");
    }
    Line::from(scene.paint(paint, text))
}

/// Marks an alone trial's light or heavy load.
fn load_word(regime: Regime) -> &'static str {
    match regime {
        Regime::R1 => " · LIGHT",
        Regime::R2 => " · HEAVY",
        Regime::R3
        | Regime::R4
        | Regime::R5
        | Regime::R6
        | Regime::R7
        | Regime::R8
        | Regime::R9 => "",
    }
}

/// A hunt cause that is a recorded failure rather than a trial: `R7 all-core already failed at these offsets`.
pub(crate) fn known_words(cause: &HuntCause) -> String {
    let Some(regime) = cause.regime else {
        let mut text = "an idle failure is already recorded at these offsets".to_owned();
        if cause.carried {
            text.push_str(", carried from an earlier session");
        }
        return text;
    };
    let mut text = format!(
        "{regime} {} already failed at these offsets",
        kind_words(regime)
    );
    if cause.carried {
        text.push_str(" in a carried trial");
    }
    text
}

/// What the journal holds about a crash that falls short of naming a core; empty when nothing.
pub(crate) fn crash_evidence(end: Option<&TrialEndView>) -> String {
    let Some((end, sample)) = end
        .filter(|end| end.signal == Some(Signal::Crash))
        .and_then(|end| end.last_sample.map(|sample| (end, sample)))
    else {
        return String::new();
    };
    let at = format!("{} into the trial", clock(sample));
    match end.stalled {
        Some(core) => format!(
            "core {:02}'s worker had stalled by the last sample, {at}",
            core.get()
        ),
        None => format!("the last sample came {at}"),
    }
}
