//! The Ruleset 13 narration: what a trial is for, what the night does between trials, and the day state when no
//! night is planned. It says what the journal and the policy projection hold and decides nothing: no thresholds,
//! no estimate of when anything finishes.

use std::fmt::Write as _;

use togi_journal::{Activity, HoldReason, Queue, Resolution, StepKind, TaskId, TaskStatus, Tested};
use togi_machine::CcdProfile;
use togi_tuner::policy::{FirmwareValidity, NightProjection, Projection};

use super::super::colours::Paint;
use super::super::scene::Scene;
use super::Story;
use crate::model::{Snapshot, TrialView};
use crate::words::plural;

/// The trial's activity, from its intent, else from the night's current task.
pub(super) fn activity_of(scene: &Scene<'_>, trial: &TrialView) -> Option<Activity> {
    trial.activity.or_else(|| {
        scene
            .s
            .policy
            .night
            .as_ref()?
            .current
            .and_then(|(_, activity)| activity)
    })
}

/// The trial's night task, from its intent, else the night's current task.
pub(super) fn task_of(scene: &Scene<'_>, trial: &TrialView) -> Option<TaskId> {
    trial
        .task
        .or_else(|| scene.s.policy.night.as_ref()?.current.map(|(task, _)| task))
}

/// A queue as the narration names it.
pub(super) const fn queue_word(queue: Queue) -> &'static str {
    match queue {
        Queue::Reproduction => "reproduction",
        Queue::Qualification => "qualification",
        Queue::Mapping => "mapping",
        Queue::Control => "control",
        Queue::Requalification => "requalification",
        Queue::Filler => "filler",
    }
}

/// A task as the narration names it: `mapping task 3`.
pub(super) fn task_words(task: TaskId) -> String {
    format!("{} task {}", queue_word(task.queue), task.ordinal)
}

/// The stage label of a trial: what it is for, in capitals; `None` for a historical per-core activity.
pub(super) fn activity_label(activity: Activity, task: Option<TaskId>) -> Option<&'static str> {
    match activity {
        Activity::Mapping => Some("MAPPING"),
        Activity::Qualification
            if task.is_some_and(|task| task.queue == Queue::Requalification) =>
        {
            Some("REQUALIFYING")
        }
        Activity::Qualification => Some("QUALIFYING"),
        Activity::Reproduction => Some("REPRODUCING"),
        Activity::Control => Some("CONTROLS"),
        Activity::Search | Activity::Checking | Activity::Hunt | Activity::Deepening => None,
    }
}

/// A CCD profile as words: `CCD 0 at -10, CCD 1 at -5`.
pub(super) fn profile_words(profile: &CcdProfile) -> String {
    profile
        .iter()
        .map(|(ccd, offset)| format!("CCD {} at {}", ccd.get(), offset.get()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A CCD profile as its offsets: `-10/-5`.
pub(super) fn profile_offsets(profile: &CcdProfile) -> String {
    profile
        .iter()
        .map(|(_, offset)| offset.get().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// The stage the policy is in, for a narrator with no trial to speak of: a night until it closes.
pub(super) fn stage_label(policy: &Projection) -> &'static str {
    if open_night(policy).is_some() {
        "NIGHT"
    } else if policy.applied.is_some()
        || policy.day.last_step.is_some()
        || policy.day.hold.is_some()
    {
        "DAY"
    } else {
        "TOGI"
    }
}

/// What a trial in flight is for, with its place in the night's queue.
pub(super) fn running(scene: &Scene<'_>, trial: &TrialView) -> Story {
    let policy = scene.s.policy;
    let activity = activity_of(scene, trial);
    let task = task_of(scene, trial);
    let Some((activity, label)) =
        activity.and_then(|activity| Some((activity, activity_label(activity, task)?)))
    else {
        return Story::one(
            stage_label(policy),
            "The trial's intent names no night task, so all I can say is what its band shows."
                .to_owned(),
            Paint::Running,
        );
    };
    let (what, brief) = purpose(policy, activity, task, trial.tested);
    let mut second = detail(policy, activity, task);
    if trial.retry {
        second.push_str(" This is a retry of a trial that could not run.");
    }
    Story::new(label, vec![what, second], brief, Paint::Running)
}

/// The sentence for a trial's purpose and the brief line.
fn purpose(
    policy: &Projection,
    activity: Activity,
    task: Option<TaskId>,
    tested: Option<Tested>,
) -> (String, String) {
    let task_part = task.map_or_else(String::new, |task| format!(" ({})", task_words(task)));
    match (activity, tested) {
        (Activity::Mapping, Some(Tested::Isolated { ccd, offset })) => (
            format!(
                "Mapping CCD {} at {}: only that CCD runs at {}, every other CCD stays at CO 0, so a failure belongs to it.",
                ccd.get(),
                offset.get(),
                offset.get()
            ),
            format!("Mapping CCD {} at {}{task_part}.", ccd.get(), offset.get()),
        ),
        (Activity::Qualification, Some(Tested::Isolated { ccd, offset })) => (
            format!(
                "Qualifying with CCD {} alone at {}.",
                ccd.get(),
                offset.get()
            ),
            format!("Qualifying CCD {} alone at {}{task_part}.", ccd.get(), offset.get()),
        ),
        (Activity::Reproduction, Some(Tested::Isolated { ccd, offset })) => (
            format!(
                "Reproducing a day failure on CCD {} alone at {}. A clean run clears nothing and earns no qualification credit.",
                ccd.get(),
                offset.get()
            ),
            format!("Reproducing on CCD {} alone at {}{task_part}.", ccd.get(), offset.get()),
        ),
        (Activity::Mapping, Some(Tested::Exact {}) | None) => (
            "Mapping: one CCD at a time runs at the tested offset while the others stay at CO 0.".to_owned(),
            format!("Mapping{task_part}."),
        ),
        (Activity::Qualification, Some(Tested::Exact {}) | None) => {
            let profile = policy
                .night
                .as_ref()
                .and_then(|night| night.qualification.as_ref())
                .map(|progress| &progress.profile);
            let named = profile.map_or_else(
                || "the exact profile".to_owned(),
                |profile| format!("the exact profile, {}", profile_words(profile)),
            );
            (
                format!(
                    "Qualifying {named}: every CCD runs together at its profile offset, one matrix cell per trial."
                ),
                format!("Qualifying {named}{task_part}."),
            )
        }
        (Activity::Reproduction, Some(Tested::Exact {}) | None) => (
            "Reproducing a day failure on the exact failed profile. A clean run clears nothing and earns no qualification credit."
                .to_owned(),
            format!("Reproducing on the exact failed profile{task_part}."),
        ),
        (Activity::Control, _) => (
            "Control trial with every CCD at CO 0. A failure here is a failure at stock, not Curve Optimizer's."
                .to_owned(),
            format!("Control at CO 0{task_part}."),
        ),
        (Activity::Search | Activity::Checking | Activity::Hunt | Activity::Deepening, _) => {
            (String::new(), String::new())
        }
    }
}

/// The second line of a running trial: its place in the plan and what its result feeds.
fn detail(policy: &Projection, activity: Activity, task: Option<TaskId>) -> String {
    let mut text = match activity {
        Activity::Mapping => {
            "Mapping exposure is what a cap S is computed from; a failure here can set the CCD's edge U."
                .to_owned()
        }
        Activity::Qualification => policy
            .night
            .as_ref()
            .and_then(|night| night.qualification.as_ref())
            .map_or_else(
                || "Only a complete qualification lets the profile be applied by day.".to_owned(),
                |progress| {
                    format!(
                        "{} witnessed, {} outstanding; only a complete qualification lets the profile be applied by day.",
                        plural(progress.witnessed, "cell"),
                        progress.outstanding.len()
                    )
                },
            ),
        Activity::Reproduction => {
            "It closes only this night's reproduction task, never the veto.".to_owned()
        }
        Activity::Control => {
            "Controls establish how the machine behaves at stock before any nonzero test.".to_owned()
        }
        Activity::Search | Activity::Checking | Activity::Hunt | Activity::Deepening => {
            String::new()
        }
    };
    if let (Some(task), Some(night)) = (task, open_night(policy)) {
        let waiting = pending(night).filter(|other| other.task != task).count();
        if waiting > 0 {
            let verb = if waiting == 1 { "waits" } else { "wait" };
            let _ = write!(text, " {} {verb} behind it.", plural(waiting, "task"));
        }
    }
    text
}

fn pending(night: &NightProjection) -> impl Iterator<Item = &togi_journal::PlannedTask> {
    night
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Pending)
}

/// The open night: planned, and not yet closed.
pub(super) fn open_night(policy: &Projection) -> Option<&NightProjection> {
    policy
        .night
        .as_ref()
        .filter(|night| night.closed_by.is_none())
}

/// What the open night does next, from its plan, and which backends it cannot use; `None` when no night is open.
pub(super) fn next_words(policy: &Projection) -> Option<String> {
    let night = open_night(policy)?;
    let mut words = match pending(night).next() {
        Some(next) => format!(
            "{} left in the night's plan; the next listed is {}.",
            plural(pending(night).count(), "task"),
            task_words(next.task)
        ),
        None => "Every planned task is done; filler work runs until you end the night.".to_owned(),
    };
    if !night.unavailable.is_empty() {
        let backends = night
            .unavailable
            .iter()
            .map(|unavailable| unavailable.backend.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(words, " Unavailable this night: {backends}.");
    }
    Some(words)
}

/// The next task for the resting band: `mapping task 3`; `None` without an open night's pending task.
pub(super) fn next_task(policy: &Projection) -> Option<TaskId> {
    pending(open_night(policy)?).next().map(|task| task.task)
}

/// Whether the latest night is closed: the operator or a reboot ended it, or a reset, dead end or unavailable backend
/// closed it.
pub(super) fn night_over(policy: &Projection) -> bool {
    policy
        .night
        .as_ref()
        .is_some_and(|night| night.closed_by.is_some())
}

/// Hours as `12.345`, truncated, as the status shows them.
fn hours(seconds: u64) -> String {
    format!("{}.{:03}", seconds / 3600, seconds % 3600 * 1000 / 3600)
}

/// The day state, in up to two lines: the applied profile and what the latest proposal says.
pub(super) fn day_lines(snapshot: &Snapshot<'_>) -> Vec<String> {
    let policy = snapshot.policy;
    let mut first = match &policy.applied {
        Some(profile) => format!("The day profile is {}.", profile_words(profile)),
        None => "No day profile has been applied; every CCD stays at CO 0.".to_owned(),
    };
    if let Some(hold) = &policy.day.hold {
        let _ = write!(first, " {} h credited at it.", hours(hold.eligible_s));
    }
    let mut second = Vec::new();
    if policy.day.freeze {
        second.push(
            "Promotion is frozen: the day profile stays, and retreats and firmware resets still apply."
                .to_owned(),
        );
    }
    match &policy.day.firmware {
        FirmwareValidity::Invalidated { .. } => second.push(
            "A firmware change invalidated the qualifications; the day profile goes back to CO 0 until a night qualifies one again."
                .to_owned(),
        ),
        FirmwareValidity::Valid | FirmwareValidity::Unknown => {}
    }
    if let Some((_, step)) = &policy.day.last_step {
        second.push(step_words(&step.step, snapshot.step_applied(step)));
    }
    let unresolved = policy
        .events
        .iter()
        .filter(|event| matches!(event.resolution, Resolution::Unresolved {}))
        .count();
    if unresolved > 0 {
        second.push(format!(
            "{} still block promotion.",
            plural(unresolved, "day event")
        ));
    }
    if second.is_empty() {
        vec![first]
    } else {
        vec![first, second.join(" ")]
    }
}

fn step_words(step: &StepKind, applied: bool) -> String {
    match step {
        StepKind::Hold { blocked_by } if blocked_by.is_empty() => {
            "The latest proposal keeps the profile.".to_owned()
        }
        StepKind::Hold { blocked_by } => format!(
            "The latest proposal keeps the profile: {}.",
            blocked_by
                .iter()
                .map(hold_reason)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        StepKind::Promote { changed } if applied => {
            format!(
                "The latest step promoted CCD {} and it was applied.",
                changed.get()
            )
        }
        StepKind::Promote { changed } => format!(
            "The latest proposal promotes CCD {}; it counts once it is applied.",
            changed.get()
        ),
        StepKind::Retreat { changed, .. } if applied => format!(
            "The latest step stepped {} back and it was applied.",
            ccd_list(changed)
        ),
        StepKind::Retreat { changed, .. } => format!(
            "The latest proposal steps {} back; it counts once it is applied.",
            ccd_list(changed)
        ),
        StepKind::FirmwareZero { changed } if applied => format!(
            "The latest step set {} to CO 0 after a firmware change and it was applied.",
            ccd_list(changed)
        ),
        StepKind::FirmwareZero { changed } => format!(
            "The latest proposal sets {} to CO 0 after a firmware change; it counts once it is applied.",
            ccd_list(changed)
        ),
    }
}

fn ccd_list(ccds: &[togi_machine::CcdId]) -> String {
    let ids = ccds
        .iter()
        .map(|ccd| ccd.get().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if ccds.len() == 1 {
        format!("CCD {ids}")
    } else {
        format!("CCDs {ids}")
    }
}

fn hold_reason(reason: &HoldReason) -> &'static str {
    match reason {
        HoldReason::Incomplete { .. } => "its hold is not complete",
        HoldReason::AtCap {} => "it is at the cap",
        HoldReason::NotQualified {} => "no complete qualification covers it",
        HoldReason::Vetoed { .. } => "a veto stands",
        HoldReason::QuestionPending { .. } => "a day question is pending",
        HoldReason::EventUnresolved { .. } => "a day event is unresolved",
        HoldReason::Frozen {} => "promotion is frozen",
        HoldReason::Paused {} => "the cold boot started paused",
        HoldReason::Expired {} => "the proposal expired",
    }
}

/// Between trials: the last result, then what the night does next or the day state.
pub(super) fn between(scene: &Scene<'_>) -> Story {
    let policy = scene.s.policy;
    let first = match scene.s.last {
        Some(end) => format!(
            "Trial {} ended: {}.",
            end.id,
            super::resting::end_word(end).0.to_lowercase()
        ),
        None => {
            "No trial has run yet; the tuner records what it decides before the first one starts."
                .to_owned()
        }
    };
    let label = if night_over(policy) {
        "NIGHT OVER"
    } else {
        stage_label(policy)
    };
    let mut lines = vec![first];
    match next_words(policy) {
        Some(next) => lines.push(next),
        None if scene.s.last.is_some() => lines.extend(day_lines(&scene.s)),
        None => {}
    }
    let brief = lines.last().cloned().unwrap_or_default();
    Story::new(label, lines, brief, Paint::Running)
}
