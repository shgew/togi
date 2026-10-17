//! The words that name the trial the tuner runs next, for the band between trials.

use std::fmt::Write as _;
use std::time::Duration;

use togi_machine::{Condition, CoreId, WorkloadId};
use togi_tuner::{Load, TrialContext, TrialPlan, WorkloadChoice};

use super::super::scene::{Scene, short_duration};
use super::super::terms::part_name;
use super::shape_words;
use crate::model::{HuntShape, TrialView};
use crate::words::{core_ids, kind_words, workload_display};

/// Opens the words that name the trial the tuner runs next, so a core's own next search turn is never taken for
/// it.
pub(super) const NEXT_TRIAL: &str = "next trial: ";

fn duration_of(plan: &TrialPlan) -> Duration {
    Duration::from_secs(u64::from(plan.duration.get()))
}

fn cycle_of(plan: &TrialPlan) -> Option<(u32, u32)> {
    match plan.context {
        TrialContext::Cycle { cycle, step, .. } => Some((cycle.get(), step.get().get())),
        TrialContext::Search { .. }
        | TrialContext::SoloLimit { .. }
        | TrialContext::Rerun { .. }
        | TrialContext::ZeroRerun { .. }
        | TrialContext::Round { .. }
        | TrialContext::Hunt { .. } => None,
    }
}

fn hunt_of(plan: &TrialPlan) -> Option<(u32, u32)> {
    match plan.context {
        TrialContext::Hunt { hunt, group } => Some((hunt.get(), group.get())),
        TrialContext::Search { .. }
        | TrialContext::SoloLimit { .. }
        | TrialContext::Cycle { .. }
        | TrialContext::Rerun { .. }
        | TrialContext::ZeroRerun { .. }
        | TrialContext::Round { .. } => None,
    }
}

fn round_of(plan: &TrialPlan) -> Option<u32> {
    match plan.context {
        TrialContext::Round { round } => Some(round.get()),
        TrialContext::Search { .. }
        | TrialContext::SoloLimit { .. }
        | TrialContext::Cycle { .. }
        | TrialContext::Rerun { .. }
        | TrialContext::ZeroRerun { .. }
        | TrialContext::Hunt { .. } => None,
    }
}

fn is_rerun(plan: &TrialPlan) -> bool {
    matches!(
        plan.context,
        TrialContext::Rerun { .. } | TrialContext::ZeroRerun { .. }
    )
}

fn single_of(plan: &TrialPlan) -> Option<CoreId> {
    match plan.load {
        Load::Single(core) => Some(core),
        Load::Many(_) => None,
    }
}

fn workload_of(plan: &TrialPlan) -> Option<WorkloadId> {
    match plan.workload {
        WorkloadChoice::Frozen(id) => Some(id),
        WorkloadChoice::Rotate => None,
    }
}

/// Describes a trial the tuner runs next the way the trial band describes the trial in flight. A branch that
/// starts a hunt passes that hunt's shape.
pub(super) fn next_trial_words(
    scene: &Scene<'_>,
    next: &TrialPlan,
    after: Option<&TrialView>,
    shape: Option<&HuntShape>,
) -> String {
    let kind = format!("{} {}", next.regime, kind_words(next.regime));
    let what = match single_of(next) {
        Some(core) => {
            let offset = next
                .profile
                .get(core)
                .map_or(0, togi_machine::TargetOffset::get);
            let alone = if next.condition == Condition::Alone {
                " alone"
            } else {
                ""
            };
            format!("{kind}, core {:02}{alone} at {offset}", core.get())
        }
        None => format!("{kind} on {}", core_ids(next.load.cores())),
    };
    let duration = short_duration(duration_of(next));
    if let Some((hunt, group)) = hunt_of(next) {
        let shape = shape.or_else(|| scene.s.shapes.get(&hunt));
        let layout = shape.map_or_else(String::new, |shape| {
            shape_words(scene, next.load.cores(), &next.profile.offsets(), shape)
        });
        if scene
            .s
            .hunt
            .is_some_and(|active| active.plan.number == hunt)
        {
            if let Some(part) = shape.and_then(|shape| plan_part(scene, next, shape)) {
                let shown = if layout.is_empty() { &what } else { &layout };
                return format!("part {part}: {shown}");
            }
        }
        let mut text = format!("hunt {hunt} group {group}: {what}");
        if !layout.is_empty() {
            text.push_str(" · ");
            text.push_str(&layout);
        }
        return text;
    }
    if is_rerun(next) {
        return if next.condition == Condition::Parked {
            format!("rerun {what} with every core at 0 · {duration}")
        } else {
            format!("rerun {what} · {duration}")
        };
    }
    if let Some((cycle, step)) = cycle_of(next) {
        let same_place = after.is_some_and(|trial| {
            trial
                .cycle
                .is_some_and(|place| place.cycle == cycle && place.step == Some(step))
        });
        if same_place {
            if let Some(name) = part_words(scene, next) {
                return name;
            }
        }
        let mut text = format!("step {step}: {kind}");
        if same_place && single_of(next).is_none() && !next.load.cores().is_empty() {
            let _ = write!(text, " on {}", core_ids(next.load.cores()));
        }
        if let Some(workload) = workload_of(next) {
            let _ = write!(text, " with {}", workload_display(workload));
        }
        return text;
    }
    if let Some(round) = round_of(next) {
        return format!("deepening round {round}: {what}");
    }
    what
}

/// Which part of the running hunt's split a trial loads, counting from 1.
fn plan_part(scene: &Scene<'_>, next: &TrialPlan, shape: &HuntShape) -> Option<usize> {
    let hunt = scene.s.hunt?;
    hunt.plan
        .parts
        .iter()
        .position(|part| {
            scene.s.order.iter().all(|&id| {
                let (Some(at), Some(fails), Some(parks)) = (
                    next.profile.get(id),
                    shape.failing.get(id),
                    shape.parked.get(id),
                ) else {
                    return true;
                };
                fails == parks || (at == fails) == part.failing.contains(id)
            })
        })
        .map(|index| index + 1)
}

/// The part of the current checking step a trial loads: `part 2: full CCD 0 on 00-07, 5m`, or
/// `part 4: core 09 at -26, 2m` for a step that loads one core at a time.
fn part_words(scene: &Scene<'_>, next: &TrialPlan) -> Option<String> {
    let (index, name) = matching_part(scene, next)?;
    let duration = short_duration(duration_of(next));
    Some(match single_of(next) {
        Some(core) => {
            let offset = next
                .profile
                .get(core)
                .map_or(0, togi_machine::TargetOffset::get);
            format!(
                "part {index}: core {:02} at {offset}, {duration}",
                core.get()
            )
        }
        None => format!(
            "part {index}: {name} on {}, {duration}",
            core_ids(next.load.cores())
        ),
    })
}

/// The 1-based index and name of the part of `next`'s step that loads exactly its cores.
fn matching_part(scene: &Scene<'_>, next: &TrialPlan) -> Option<(usize, String)> {
    let (_, step) = cycle_of(next)?;
    let cycle = scene.s.cycle?;
    let step = cycle
        .plan
        .steps
        .get(usize::try_from(step).ok()?.checked_sub(1)?)?;
    let want = next.load.cores();
    let index = step.parts.iter().position(|part| part.cores == want)?;
    Some((index + 1, part_name(step.regime, &step.parts[index])))
}
