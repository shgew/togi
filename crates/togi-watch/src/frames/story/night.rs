//! The D narration: what a trial is for, what the night does between trials, and the day state when no
//! night is planned. It says what the journal and the policy projection hold and decides nothing: no thresholds,
//! no estimate of when anything finishes.

use std::borrow::Cow;
use std::fmt::Write as _;

use togi_journal::{Activity, HoldReason, Queue, Resolution, StepKind, TaskId, TaskStatus, Tested};
use togi_tuner::policy::{FirmwareValidity, NightProjection, Projection};
use togi_tuner::unit::{UnitId, UnitProfile};

use super::super::colours::Paint;
use super::super::scene::Scene;
use super::Story;
use crate::model::{Snapshot, TrialView};
use crate::words::plural;

/// The trial's activity, from its intent, else from the night's current task.
pub(crate) fn activity_of(scene: &Scene<'_>, trial: &TrialView) -> Option<Activity> {
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
pub(crate) fn task_of(scene: &Scene<'_>, trial: &TrialView) -> Option<TaskId> {
    trial
        .task
        .or_else(|| scene.s.policy.night.as_ref()?.current.map(|(task, _)| task))
}

/// The queue a trial serves: its task's queue, else the queue its activity belongs to; `None` for a historical
/// per-core activity. With no trial only the night's committed current task counts: the previous trial's activity
/// is history.
pub(crate) fn queue_of(scene: &Scene<'_>, trial: Option<&TrialView>) -> Option<Queue> {
    let task = match trial {
        Some(trial) => task_of(scene, trial),
        None => scene.s.policy.night.as_ref()?.current.map(|(task, _)| task),
    };
    task.map(|task| task.queue)
        .or_else(|| activity_queue(activity_of(scene, trial?)?))
}

/// The queue an activity belongs to; `None` for a historical per-core activity.
const fn activity_queue(activity: Activity) -> Option<Queue> {
    match activity {
        Activity::Mapping => Some(Queue::Mapping),
        Activity::Qualification => Some(Queue::Qualification),
        Activity::Reproduction => Some(Queue::Reproduction),
        Activity::Control => Some(Queue::Control),
        Activity::Search | Activity::Checking | Activity::Hunt | Activity::Deepening => None,
    }
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
        Activity::Mapping | Activity::Qualification if is_filler(task) => Some("FILLER"),
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
pub(super) fn profile_words(profile: &UnitProfile) -> String {
    profile
        .iter()
        .map(|(unit, offset)| match unit {
            UnitId::Ccd(ccd) => format!("CCD {} at {}", ccd.get(), offset.get()),
            UnitId::Core(core) => format!("core {} at {}", core.get(), offset.get()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A CCD profile as its offsets: `-10/-5`.
pub(super) fn profile_offsets(profile: &UnitProfile) -> String {
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
    let profile = exact_profile(scene, trial, task);
    let (what, brief) = purpose(activity, task, trial.tested, profile.as_deref());
    let mut second = detail(policy, activity, task);
    if trial.retry {
        second.push_str(" This is a retry of a trial that could not run.");
    }
    Story::new(label, vec![what, second], brief, Paint::Running)
}

/// Whether a trial serves the filler task. Filler keeps its underlying mapping or qualification activity, so the
/// task, not the activity, says what it is for.
pub(super) fn is_filler(task: Option<TaskId>) -> bool {
    task.is_some_and(|task| task.queue == Queue::Filler)
}

/// Preserve a nonuniform physical-core vector rather than borrowing one core's value for its CCD.
pub(super) fn trial_profile(scene: &Scene<'_>, trial: &TrialView) -> Option<UnitProfile> {
    let core_phase = scene
        .s
        .policy
        .climb
        .units
        .iter()
        .any(|unit| matches!(unit.unit, UnitId::Core(_)));
    if core_phase {
        return Some(UnitProfile::of_cores(&trial.profile));
    }
    let mut ccds = std::collections::BTreeMap::new();
    for (core, offset) in trial.profile.iter() {
        let view = scene.s.cores.iter().find(|view| view.id == core)?;
        if ccds
            .insert(UnitId::Ccd(view.ccd), offset)
            .is_some_and(|previous| previous != offset)
        {
            return Some(UnitProfile::of_cores(&trial.profile));
        }
    }
    UnitProfile::try_from(ccds).ok()
}

/// The profile an exact trial is named by: a filler repeat by the profile the trial actually runs, any other trial
/// by the night's profile under qualification.
pub(super) fn exact_profile<'s>(
    scene: &'s Scene<'_>,
    trial: &TrialView,
    task: Option<TaskId>,
) -> Option<Cow<'s, UnitProfile>> {
    if is_filler(task) {
        trial_profile(scene, trial).map(Cow::Owned)
    } else {
        scene
            .s
            .policy
            .night
            .as_ref()
            .and_then(|night| night.qualification.as_ref())
            .map(|progress| Cow::Borrowed(&progress.profile))
    }
}

/// Filler repeats exact-current-D cells; it never probes statistical S.
fn filler_purpose(
    activity: Activity,
    task: Option<TaskId>,
    tested: Option<Tested>,
    profile: Option<&UnitProfile>,
    task_part: &str,
) -> Option<(String, String)> {
    if !is_filler(task) {
        return None;
    }
    match (activity, tested) {
        (Activity::Qualification, Some(Tested::Exact {}) | None) => {
            let named = profile.map_or_else(
                || "the current exact D profile".to_owned(),
                |profile| format!("the current exact D profile, {}", profile_words(profile)),
            );
            Some((
                format!(
                    "Filler repeat of {named}: all units run together, one exact qualification cell per trial."
                ),
                format!("Filler repeat of {named}{task_part}."),
            ))
        }
        _ => None,
    }
}

/// What a filler trial's result feeds; only exact-current-D qualification is filler.
fn filler_detail(activity: Activity, task: Option<TaskId>) -> Option<String> {
    if !is_filler(task) {
        return None;
    }
    match activity {
        Activity::Mapping => None,
        Activity::Qualification => Some(
            "Filler repeats exact-current-D cells only when primary work is empty; a failure enters the current-D CPU-suspect event path, not a failed candidate's allowance-free block."
                .to_owned(),
        ),
        Activity::Reproduction
        | Activity::Control
        | Activity::Search
        | Activity::Checking
        | Activity::Hunt
        | Activity::Deepening => None,
    }
}

/// The sentence for a trial's purpose and the brief line; `profile` is the one [`exact_profile`] names.
fn purpose(
    activity: Activity,
    task: Option<TaskId>,
    tested: Option<Tested>,
    profile: Option<&UnitProfile>,
) -> (String, String) {
    let task_part = task.map_or_else(String::new, |task| format!(" ({})", task_words(task)));
    if let Some(said) = filler_purpose(activity, task, tested, profile, &task_part) {
        return said;
    }
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
    let mut text = filler_detail(activity, task).unwrap_or_else(|| match activity {
        Activity::Mapping => {
            "Mapping brackets tagged U, then sweeps uncovered cells from U toward hard ceiling C; S is a non-authorizing diagnostic."
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
                        "{} eligible s witnessed, {} outstanding; only a complete qualification lets the profile be applied by day.",
                        progress.eligible_s,
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
    });
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
pub(crate) fn blocker_words(reason: &togi_journal::ClimbBlocker) -> &'static str {
    use togi_journal::ClimbBlocker;
    match reason {
        ClimbBlocker::Hold { .. } => "exact-profile hold incomplete",
        ClimbBlocker::Certificate {} => "exact night qualification missing",
        ClimbBlocker::CertificateFailed { .. } => "exact candidate qualification blocked",
        ClimbBlocker::QuestionPending { .. } => "question pending",
        ClimbBlocker::EventUnresolved { .. } => "event unresolved",
        ClimbBlocker::Veto { .. } => "whole-profile veto",
        ClimbBlocker::FirmwareInvalid {} => "firmware invalid",
        ClimbBlocker::Frozen {} => "promotion frozen",
        ClimbBlocker::Paused {} => "paused",
        ClimbBlocker::AllowanceExhausted {} => "allowance exhausted",
        ClimbBlocker::UnknownEdge { .. } => "unknown U",
        ClimbBlocker::Finished { .. } => "exploration finished",
        ClimbBlocker::Ceiling { .. } => "hard night ceiling blocks the target",
        ClimbBlocker::CampaignHistory {} => "campaign history unavailable",
        ClimbBlocker::SameBoot {} => "promotion requires an eligible cold boot",
        ClimbBlocker::HoldProfile {} => "hold belongs to a different exact profile",
        ClimbBlocker::SafeApply {} => "safe application prerequisites missing",
        ClimbBlocker::ActivationClosed {} => "nonzero activation closed",
    }
}

pub(crate) fn finish_words(reason: &togi_journal::UnitStopReason) -> &'static str {
    use togi_journal::UnitStopReason;
    match reason {
        UnitStopReason::Ceiling {} => "night ceiling reached",
        UnitStopReason::FirstBackstep { .. } => "first backstep; permanently stopped",
        UnitStopReason::AllowanceExhausted {} => "allowance exhausted",
        UnitStopReason::RestorationBackstep { .. } => "restoration backstep; permanently stopped",
        UnitStopReason::AdverseCeiling {} => "adverse night ceiling; permanently stopped",
        UnitStopReason::Veto {} => "whole-profile veto; permanently stopped",
        UnitStopReason::Predecessor { .. } => "predecessor event; permanently stopped",
    }
}

pub(crate) fn vector_words(profile: &togi_journal::UnitVector) -> String {
    profile
        .cores
        .iter()
        .map(|(core, offset)| format!("{}:{}", core.get(), offset.get()))
        .collect::<Vec<_>>()
        .join(" ")
}

const PERMISSION: &str = "permission: experimental hold + exact night qualification";

fn allowance_words(campaign: Option<&togi_tuner::policy::ClimbCampaignProjection>) -> String {
    campaign.map_or_else(
        || "allowance: history unavailable; new exploration blocked".to_owned(),
        |campaign| format!(
            "allowance: {} consumed / {} limit; {} remaining · {} provisional · {} reclassified{}",
            campaign.consumed, campaign.limit, campaign.remaining, campaign.provisional, campaign.reclassified,
            if campaign.exhausted { " · exhausted; no additional climbing" } else { "" },
        ),
    )
}

fn event_words(event: &togi_journal::DayEventId) -> String {
    match event {
        togi_journal::DayEventId::Boot { boot } => format!("boot {}", boot.as_str()),
        togi_journal::DayEventId::Record { seq, .. } => format!("record #{seq}"),
        togi_journal::DayEventId::Requalification { night } => format!("requalification {night}"),
    }
}

/// One line of the D narration: plain words, or the daily-evidence line whose clauses a renderer may drop
/// when the line does not fit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ClimbLine {
    Text(String),
    Daily(DailyWords),
}

impl ClimbLine {
    /// The line at full length.
    pub(crate) fn words(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Daily(daily) => daily.words(),
        }
    }
}

impl From<String> for ClimbLine {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

/// What the current exact profile's daily evidence bounds, if anything.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DailyBound {
    Unavailable,
    /// The numeric rate bound together with its fixed-rate-model qualification; neither shows without the other.
    Rate(String),
}

/// The daily-evidence line in the clauses it is fitted by: the clean hours always, the bound with its
/// qualification as one clause, the observed counts last.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DailyWords {
    pub(crate) head: String,
    pub(crate) bound: DailyBound,
    pub(crate) observed: String,
}

impl DailyWords {
    pub(crate) const BOUND_SEP: &'static str = "; ";
    pub(crate) const OBSERVED_SEP: &'static str = " · ";

    fn words(&self) -> String {
        let bound = match &self.bound {
            DailyBound::Unavailable => "rate bound unavailable",
            DailyBound::Rate(rate) => rate,
        };
        format!(
            "{}{}{bound}{}{}",
            self.head,
            Self::BOUND_SEP,
            Self::OBSERVED_SEP,
            self.observed
        )
    }
}

/// Campaign and daily evidence are already projected; no renderer invents a permission or rate bound.
pub(crate) fn climb_lines(policy: &Projection) -> Vec<ClimbLine> {
    use togi_journal::{CampaignEventPhase, CampaignHistory, ClimbAction};
    let climb = &policy.climb;
    let mut lines: Vec<ClimbLine> = vec![
        ClimbLine::Text(PERMISSION.to_owned()),
        ClimbLine::Text(allowance_words(climb.campaign.as_ref())),
    ];
    if let Some(campaign) = &climb.campaign {
        if let CampaignHistory::Unreconstructible { reason } = &campaign.history {
            lines.push(ClimbLine::Text(format!(
                "campaign history unavailable: {reason}; new exploration blocked"
            )));
        }
        for event in &campaign.events {
            let phase = match event.phase {
                CampaignEventPhase::Learning => "learning",
                CampaignEventPhase::PostFinish => "post-finish",
            };
            let class = match &event.classification {
                togi_journal::Classification::CpuSuspect { certainty } => {
                    format!("CPU-suspect ({certainty})")
                }
                togi_journal::Classification::NonCpu {} => "non-CPU".to_owned(),
                togi_journal::Classification::Unexplained {} => "unexplained".to_owned(),
            };
            lines.push(ClimbLine::Text(format!(
                "{phase} event {}: {class}{}{}",
                event_words(&event.event),
                if event.provisional {
                    " · provisional"
                } else {
                    ""
                },
                if event.reclassified {
                    " · reclassified; allowance not refunded"
                } else {
                    ""
                }
            )));
        }
    }
    match &climb.daily {
        None => lines.push(ClimbLine::Text(
            "experimental: current exact-profile daily evidence unavailable; no rate bound"
                .to_owned(),
        )),
        Some(daily) => {
            let bound = daily
                .poisson_upper_per_h
                .filter(|rate| rate.is_finite() && *rate > 0.0)
                .map_or(DailyBound::Unavailable, |rate| {
                    DailyBound::Rate(format!(
                        "at most 1 crash per {:.0} h (95%, fixed-rate model)",
                        1.0 / rate
                    ))
                });
            lines.push(ClimbLine::Daily(DailyWords {
                head: format!("experimental: {} clean hours", hours(daily.clean_s)),
                bound,
                observed: format!(
                    "{} crashes / {} observed h",
                    daily.crashes,
                    hours(daily.observed_s)
                ),
            }));
        }
    }
    if let Some(profile) = &climb.profiles.proposed {
        lines.push(ClimbLine::Text(format!(
            "proposed, not applied: {}",
            vector_words(profile)
        )));
    }
    if let Some(profile) = &climb.profiles.paused_saved {
        lines.push(ClimbLine::Text(format!(
            "paused saved D, not readback: {}",
            vector_words(profile)
        )));
    }
    for unit in &climb.units {
        let name = match unit.unit {
            UnitId::Ccd(id) => format!("CCD {}", id.get()),
            UnitId::Core(id) => format!("core {}", id.get()),
        };
        let state = match &unit.state {
            togi_tuner::policy::ClimbUnitState::Climbing => "climbing".to_owned(),
            togi_tuner::policy::ClimbUnitState::Held { reason } => format!(
                "held/unfinished: {}",
                reason
                    .iter()
                    .map(blocker_words)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            togi_tuner::policy::ClimbUnitState::Finished {
                reason, last_clean, ..
            } => format!(
                "finished: {}; last clean {}; remains experimental",
                finish_words(reason),
                last_clean.get()
            ),
        };
        lines.push(ClimbLine::Text(format!(
            "{name}: {state}; hold {} / {} h at exact D",
            hours(unit.hold.credited_s),
            hours(unit.hold.required_s)
        )));
        if let Some(step) = &unit.latest_step {
            let action = match &step.action {
                ClimbAction::Hold { blocked_by } => format!(
                    "hold: {}",
                    blocked_by
                        .iter()
                        .map(blocker_words)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                ClimbAction::Promote { from, to, step, .. } => {
                    format!("step {} → {} ({step} counts)", from.get(), to.get())
                }
                ClimbAction::Retreat { cause, .. } => {
                    use togi_journal::ClimbRetreatCause;
                    match cause {
                        ClimbRetreatCause::AdverseCeiling { .. } => {
                            "retreat: adverse night ceiling".to_owned()
                        }
                        ClimbRetreatCause::Event { event } => {
                            format!("retreat: CPU-suspect {}", event_words(event))
                        }
                        ClimbRetreatCause::Veto { event } => {
                            format!("retreat: whole-profile veto from {}", event_words(event))
                        }
                        ClimbRetreatCause::Completion {} => {
                            "retreat: exploration completion".to_owned()
                        }
                    }
                }
                ClimbAction::Restore { restoration, .. } => match restoration {
                    togi_journal::RestorationKind::Firmware => "firmware restoration".to_owned(),
                    togi_journal::RestorationKind::Reclassified => {
                        "guarded reclassification restoration; climb stays stopped".to_owned()
                    }
                },
                ClimbAction::FirmwareZero {} => "firmware reset to stock".to_owned(),
            };
            lines.push(ClimbLine::Text(format!(
                "{name}: applied {action}; application {:?}; witnesses {:?}",
                step.application, step.witnesses
            )));
        }
    }
    lines
}

pub(super) fn day_lines(snapshot: &Snapshot<'_>) -> Vec<String> {
    let policy = snapshot.policy;
    if !policy.climb.units.is_empty() {
        return vec![
            PERMISSION.to_owned(),
            allowance_words(policy.climb.campaign.as_ref()),
        ];
    }
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
        HoldReason::AtCap {} => "no ordinary step remains; see the unit's finish or held reason",
        HoldReason::NotQualified {} => "no complete qualification covers it",
        HoldReason::Vetoed { .. } => "a veto stands",
        HoldReason::QuestionPending { .. } => "a day question is pending",
        HoldReason::EventUnresolved { .. } => "a day event is unresolved",
        HoldReason::Frozen {} => "promotion is frozen",
        HoldReason::Paused {} => "the cold boot started paused",
        HoldReason::Expired {} => "the proposal expired",
        HoldReason::QualificationFailed { .. } => "it failed qualification",
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
