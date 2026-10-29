//! Status header activity, shared class wording and the latest night's progress.
//! Daily experimental-permission facts are rendered from the D projection by `climb`.

use std::fmt::Write as _;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{
    Activity, DayEventId, ExposureClass, Position, QualificationPurpose, Queue, ReproductionState,
    Resolution, TaskId, VetoState,
};
use togi_machine::TargetOffset;
use togi_tuner::policy::{FirmwareValidity, NightProjection, Projection, QualificationStage};
use togi_tuner::unit::{UnitId, UnitProfile};

use togi_watch::model::StopView;
use togi_watch::words::age;

use super::esc;
use crate::table::{Row, table, wrap};

/// The journal-derived facts the policy sections are placed by: the zone clock times are shown in, and the
/// time of the `day.boot_start` that closed the latest night, when the journal holds it.
#[derive(Clone, Copy)]
pub(super) struct Where<'a> {
    pub zone: &'a TimeZone,
    pub closed: Option<Timestamp>,
}

impl Where<'_> {
    pub(super) fn clock(&self, at: Timestamp) -> String {
        at.to_zoned(self.zone.clone()).strftime("%H:%M").to_string()
    }
}

/// The night's elapsed observed time E: the sum of what every queue consumed.
fn elapsed(night: &NightProjection) -> String {
    let seconds = night.budgets.iter().map(|budget| budget.consumed_s).sum();
    age(Duration::from_secs(seconds))
}

/// The running task in words: its activity, or `filler` for a filler task before its first trial.
fn task_activity(task: TaskId, activity: Option<Activity>) -> String {
    let doing = activity.map_or(queue_word(task.queue), Activity::as_str);
    format!(
        "{doing} (task {} #{})",
        queue_word(task.queue),
        task.ordinal
    )
}

/// The header's activity: the open night with its current task, the day, a stop.
pub(super) fn activity(policy: &Projection, stopped: Option<&StopView>, at: Where<'_>) -> String {
    if stopped.is_some() {
        return "stopped".to_owned();
    }
    match policy
        .night
        .as_ref()
        .filter(|night| night.closed_by.is_none())
    {
        Some(night) => {
            let task = match night.current {
                Some((task, activity)) => task_activity(task, activity),
                None => "between tasks".to_owned(),
            };
            format!(
                "night since {}, {} elapsed: {task}",
                at.clock(night.started),
                elapsed(night)
            )
        }
        None => "day".to_owned(),
    }
}

/// A trial class in words: regime, workloads, the position it names, duration and placement.
pub(super) fn class_word(class: &ExposureClass) -> String {
    let workloads: Vec<&str> = class
        .workloads
        .iter()
        .map(|content| content.workload.as_str())
        .collect();
    let mut text = format!("{} {}", class.regime.as_str(), workloads.join("+"));
    match class.position {
        Some(Position::Light { core }) => {
            let _ = write!(text, " light core {core}");
        }
        Some(Position::Serial { cpu }) => {
            let _ = write!(text, " serial cpu {cpu}");
        }
        None => {}
    }
    let _ = write!(text, " {}s {}", class.duration_s, class.placement.as_str());
    esc(&text)
}

/// Expanded evidence must distinguish loaded subsets even when the concise status groups them.
pub(super) fn evidence_class_word(class: &ExposureClass) -> String {
    format!("{} cores {}", class_word(class), core_ranges(class.cores))
}

/// Trial classes in words, first-seen order. Classes that differ only in the cores they load read the same,
/// so each reading is shown once with how many classes share it: `R1 mprime-sse-4k-21k 90s exact x8`.
fn classes_word(classes: &[ExposureClass]) -> String {
    counted_words(classes.iter())
}

/// The distinct classes in order, each repeated one suffixed with its count.
fn counted_words<'a>(classes: impl Iterator<Item = &'a ExposureClass>) -> String {
    let mut counted: Vec<(String, usize)> = Vec::new();
    for word in classes.map(class_word) {
        match counted.iter_mut().find(|(seen, _)| *seen == word) {
            Some((_, count)) => *count += 1,
            None => counted.push((word, 1)),
        }
    }
    counted
        .into_iter()
        .map(|(word, count)| {
            if count == 1 {
                word
            } else {
                format!("{word} x{count}")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn profile_word(profile: &UnitProfile) -> String {
    profile_words(profile.iter())
}


fn profile_words(profile: impl Iterator<Item = (UnitId, TargetOffset)>) -> String {
    let parts: Vec<String> = profile
        .map(|(unit, offset)| match unit {
            UnitId::Ccd(ccd) => format!("CCD {ccd} {}", offset.get()),
            UnitId::Core(core) => format!("core {core} {}", offset.get()),
        })
        .collect();
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Applied hours with at most one decimal, never rounded up.
pub(super) fn hours(seconds: u64) -> String {
    let tenths = seconds * 10 / 3600;
    if tenths.is_multiple_of(10) {
        format!("{}", tenths / 10)
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    }
}

fn event_word(id: &DayEventId) -> String {
    match id {
        DayEventId::Boot { boot } => format!("boot {}", esc(boot.as_str())),
        DayEventId::Record { session, seq } => format!("record #{seq} of session {session}"),
        DayEventId::Requalification { night } => format!("requalification of night #{night}"),
    }
}


const fn reproduction_word(state: ReproductionState) -> &'static str {
    match state {
        ReproductionState::Pending => "reproduction pending",
        ReproductionState::Reproduced => "reproduced",
        ReproductionState::Unreproduced => "not reproduced",
        ReproductionState::Untestable => "untestable",
    }
}


const fn queue_word(queue: Queue) -> &'static str {
    queue.as_str()
}

const fn purpose_word(purpose: QualificationPurpose) -> &'static str {
    match purpose {
        QualificationPurpose::NextStep => "next step",
        QualificationPurpose::Requalification => "requalification",
        QualificationPurpose::Firmware => "firmware",
    }
}

const fn stage_word(stage: QualificationStage) -> &'static str {
    match stage {
        QualificationStage::Start => "started",
        QualificationStage::Progress => "in progress",
        QualificationStage::Complete => "complete",
        QualificationStage::Failed => "failed",
    }
}

/// A queue's part of the elapsed observed time, in whole percent.
fn used_pct(consumed_s: u64, elapsed_s: u64) -> u64 {
    consumed_s
        .checked_mul(100)
        .and_then(|scaled| scaled.checked_div(elapsed_s))
        .unwrap_or(0)
}

fn night_part(out: &mut String, night: &NightProjection, policy: &Projection, at: Where<'_>) {
    let started = at.clock(night.started);
    let title = match (night.closed_by, at.closed) {
        (None, _) => format!("Night since {started}"),
        (Some(_), Some(closed)) => format!("Last night {started}\u{2013}{}", at.clock(closed)),
        (Some(_), None) => format!("Last night {started}"),
    };
    let _ = writeln!(out, "\n{title}");
    let elapsed_s: u64 = night.budgets.iter().map(|budget| budget.consumed_s).sum();
    let _ = writeln!(out, "elapsed: {}", elapsed(night));
    let mut rows = vec![Row::new(
        ["QUEUE", "SHARE", "USED", "TIME"].map(String::from),
    )];
    for budget in night
        .budgets
        .iter()
        .filter(|budget| budget.share_pct != 0 || budget.consumed_s != 0)
    {
        let share = match budget.queue {
            Queue::Filler => "-".to_owned(),
            Queue::Reproduction
            | Queue::Qualification
            | Queue::Mapping
            | Queue::Control
            | Queue::Requalification => format!("{}%", budget.share_pct),
        };
        rows.push(Row::new([
            queue_word(budget.queue).to_owned(),
            share,
            format!("{}%", used_pct(budget.consumed_s, elapsed_s)),
            age(Duration::from_secs(budget.consumed_s)),
        ]));
    }
    table(out, &rows);
    for excluded in &night.unavailable {
        let causes: Vec<String> = excluded
            .causes
            .iter()
            .map(|seq| format!("#{seq}"))
            .collect();
        wrap(
            out,
            "",
            "  ",
            &format!(
                "unavailable: {}, excluded by {}",
                excluded.backend.as_str(),
                causes.join(" ")
            ),
        );
    }
    match night.current {
        Some((task, activity)) => {
            let _ = writeln!(out, "current task: {}", task_activity(task, activity));
        }
        None => out.push_str("current task: none\n"),
    }
    for mapping in &night.mapping {
        let mut text = format!("mapping CCD {}:", mapping.unit);
        match mapping.cursor {
            Some(cursor) => {
                let _ = write!(text, " next offset {}", cursor.get());
            }
            None => text.push_str(" no offset in progress"),
        }
        if let Some((failed, _)) = mapping.bracket {
            let _ = write!(text, ", bracketing the failure at {}", failed.get());
        }
        wrap(out, "", "  ", &text);
    }
    for record in &policy.qualifications {
        let freshness = if record.current { "" } else { ", stale" };
        wrap(
            out,
            "qualification: ",
            "  ",
            &format!(
                "{} ({}) {}{freshness}, {} h eligible witnessed exposure",
                profile_word(&record.profile),
                purpose_word(record.purpose),
                stage_word(record.state),
                hours(record.eligible_s)
            ),
        );
    }
    if let Some(open) = &night.qualification {
        wrap(
            out,
            "",
            "  ",
            &format!(
                "qualifying {}: {} h eligible witnessed exposure, {} outstanding",
                profile_word(&open.profile),
                hours(open.eligible_s),
                open.outstanding.len()
            ),
        );
        if !open.outstanding.is_empty() {
            wrap(
                out,
                "  outstanding ",
                "    ",
                &classes_word(&open.outstanding),
            );
        }
    }
    for reproduction in &night.reproductions {
        let state = reproduction_word(reproduction.state);
        wrap(
            out,
            "reproduction: ",
            "  ",
            &format!(
                "{}: {state}, {} h consumed, {} classes remaining",
                event_word(&reproduction.source),
                hours(reproduction.consumed_s),
                reproduction.remaining.len()
            ),
        );
    }
}

/// Runtime safety remains visible independently of exploration progress or campaign reconstruction.
fn safety_part(out: &mut String, policy: &Projection) {
    out.push_str("\nDaily safety\n");
    let day = &policy.day;
    let _ = writeln!(out, "freeze: {}", if day.freeze { "on" } else { "off" });
    let firmware = match day.firmware {
        FirmwareValidity::Valid => "valid".to_owned(),
        FirmwareValidity::Unknown => "unknown".to_owned(),
        FirmwareValidity::Invalidated { since } => {
            format!("invalidated since [#{since}]; stock until new-context restoration")
        }
    };
    wrap(out, "firmware: ", "  ", &firmware);
    for boot in &day.pending_questions {
        wrap(out, "question pending for boot ", "  ", &esc(boot.as_str()));
    }
    for event in &policy.events {
        let classification = match event.classification {
            togi_journal::Classification::CpuSuspect {
                certainty: togi_journal::Certainty::Definite,
            } => "CPU-suspect",
            togi_journal::Classification::CpuSuspect {
                certainty: togi_journal::Certainty::Provisional,
            } => "provisionally CPU-suspect",
            togi_journal::Classification::NonCpu {} => "not CPU",
            togi_journal::Classification::Unexplained {} => "unexplained",
        };
        let resolution = match event.resolution {
            Resolution::Unresolved {} => "unresolved",
            Resolution::Resolved { .. } => "resolved",
        };
        wrap(
            out,
            "safety event: ",
            "  ",
            &format!(
                "{}: {classification}, {resolution}, {}",
                event_word(&event.event),
                reproduction_word(event.reproduction)
            ),
        );
    }
    for veto in &policy.vetoes {
        let state = match veto.state {
            VetoState::Active => "active",
            VetoState::Cleared => "cleared",
        };
        wrap(
            out,
            "veto: ",
            "  ",
            &format!("{}: {state}", event_word(&veto.event)),
        );
    }
}

/// The unit table and experimental day policy, followed by the latest night.
pub(super) fn sections(out: &mut String, policy: &Projection, at: Where<'_>) {
    super::climb::sections(out, &policy.climb, at);
    safety_part(out, policy);
    if let Some(night) = &policy.night {
        night_part(out, night, policy, at);
    }
}
