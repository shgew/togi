//! The Ruleset 13 sections of `togi status`: the header activity, the per-CCD table and the day and night
//! policy parts. Policy facts come from the tuner's projection, runtime stops from the shared journal
//! fold; nothing here computes a cap, a distance or an eligibility.

use std::fmt::Write as _;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{
    Activity, Binding, DayEventId, DayStep, Edge, ExposureClass, HoldReason, Position,
    QualificationPurpose, Queue, ReproductionState, Resolution, RetreatCause, Slope, StepKind,
    TaskId, UncoveredReason, VetoState,
};
use togi_machine::{CcdProfile, CoreId, CoreSet};
use togi_tuner::policy::{
    CcdProjection, DayProjection, FirmwareValidity, NightProjection, Projection, QualificationStage,
};

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
    fn clock(&self, at: Timestamp) -> String {
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

/// A CCD's cores as ranges: `0-7`, `0-3 8`.
fn core_ranges(cores: CoreSet) -> String {
    let mut ids: Vec<u8> = cores.into_iter().map(CoreId::get).collect();
    ids.sort_unstable();
    let mut parts: Vec<String> = Vec::new();
    let mut at = 0;
    while at < ids.len() {
        let mut end = at;
        while end + 1 < ids.len() && ids[end + 1] == ids[end] + 1 {
            end += 1;
        }
        parts.push(if end == at {
            ids[at].to_string()
        } else {
            format!("{}-{}", ids[at], ids[end])
        });
        at = end + 1;
    }
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(" ")
    }
}

fn edge_word(edge: Option<&Edge>) -> String {
    match edge {
        None | Some(Edge::Unknown {}) => "unknown".to_owned(),
        Some(Edge::Observed { offset, .. }) => format!("fails {}", offset.get()),
        Some(Edge::Censored { limit, .. }) => format!("censored {}", limit.get()),
    }
}

const fn slope_word(slope: &Slope) -> &'static str {
    match slope {
        Slope::Fallback { .. } => "fallback",
        Slope::Measured { .. } => "measured",
    }
}

const fn binding_word(binding: &Binding) -> &'static str {
    match binding {
        Binding::Rate { .. } => "rate",
        Binding::EdgeMargin {} => "edge",
        Binding::Baseline {} => "baseline",
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

/// Trial classes in words, first-seen order. Classes that differ only in the cores they load read the same,
/// so each reading is shown once with how many classes share it: `R1 mprime-sse-4k-21k 90s exact x8`.
fn classes_word(classes: &[ExposureClass]) -> String {
    counted_words(classes.iter())
}

/// Why a class is uncovered for the cap.
fn uncovered_reason_word(reason: UncoveredReason) -> &'static str {
    match reason {
        UncoveredReason::NoObservations => "no observations",
        UncoveredReason::NonMappingOnly => "non-mapping exposure only",
        UncoveredReason::MappingInadequate => "mapping exposure inadequate",
    }
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

fn profile_word(profile: &CcdProfile) -> String {
    let parts: Vec<String> = profile
        .iter()
        .map(|(ccd, offset)| format!("CCD {ccd} {}", offset.get()))
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

/// The CCD table's columns, each with the width it is laid out at: the widest value its column can hold (`censored
/// -50` for U, a CCD's core range, an offset). Columns are one space apart, so the ten columns span 76 cells and every
/// value sits under its header whatever the values are; a wider value widens its column for every row.
const CCD_COLUMNS: [(&str, usize); 10] = [
    ("CCD", 3),
    ("CORES", 5),
    ("D", 3),
    ("S", 3),
    ("SLOPE", 8),
    ("BINDING", 8),
    ("U", 12),
    ("REMAINING", 9),
    ("COVERED", 7),
    ("UNCOVERED", 9),
];

/// The widest CORES cell: the other columns at their widths take 71 cells, so this keeps the row within 80. A CCD
/// whose core ranges are wider, such as a noncontiguous `0 2 4 6 8 10 12 14`, counts its cores in the cell and lists
/// them under its row.
const CORES_WIDTH: usize = 9;

pub(super) fn ccd_table(out: &mut String, policy: &Projection) {
    if policy.ccds.is_empty() {
        return;
    }
    let header = CCD_COLUMNS.map(|(name, _)| name.to_owned());
    let rows: Vec<([String; 10], Option<String>, Option<String>)> =
        policy.ccds.iter().map(ccd_row).collect();
    let widths: [usize; 10] = std::array::from_fn(|column| {
        rows.iter()
            .map(|(cells, _, _)| cells[column].chars().count())
            .fold(CCD_COLUMNS[column].1, usize::max)
    });
    ccd_line(out, &header, &widths);
    for (cells, listed, bound) in &rows {
        ccd_line(out, cells, &widths);
        if let Some(cores) = listed {
            wrap(out, "  cores ", "        ", cores);
        }
        if let Some(class) = bound {
            wrap(out, "  bound by ", "           ", class);
        }
    }
    for ccd in &policy.ccds {
        match &ccd.cap {
            Some(cap) if !cap.uncovered.is_empty() => {
                for &reason in UncoveredReason::ALL {
                    let mut classes = cap
                        .uncovered
                        .iter()
                        .filter(|uncovered| uncovered.reason == reason)
                        .map(|uncovered| &uncovered.class)
                        .peekable();
                    if classes.peek().is_none() {
                        continue;
                    }
                    let text = format!(
                        "CCD {} uncovered for the cap, {}: {}",
                        ccd.ccd,
                        uncovered_reason_word(reason),
                        counted_words(classes)
                    );
                    wrap(out, "  ", "    ", &text);
                }
            }
            Some(_) | None => {}
        }
    }
    if policy.ccds.iter().any(|ccd| {
        ccd.cap
            .as_ref()
            .is_some_and(|cap| !cap.uncovered.is_empty())
    }) {
        wrap(
            out,
            "  ",
            "    ",
            "A class is uncovered for the cap when mapping has not adequately exposed it or it lies outside the mapping projection; uncovered is never read as safe.",
        );
    }
    if policy.ccds.iter().any(|ccd| ccd.cap.is_none()) {
        wrap(
            out,
            "  ",
            "    ",
            "A CCD with no cap yet has S at offset 0.",
        );
    }
}

/// One line of the CCD table: each cell padded to its column's width and one space, the last unpadded.
fn ccd_line(out: &mut String, cells: &[String; 10], widths: &[usize; 10]) {
    let last = cells.len() - 1;
    for (column, cell) in cells.iter().enumerate() {
        out.push_str(cell);
        if column < last {
            let pad = widths[column] - cell.chars().count() + 1;
            let _ = write!(out, "{:pad$}", "");
        }
    }
    out.push('\n');
}

/// A CCD's cells, in [`CCD_COLUMNS`] order; its full core list when the CORES cell had to count the cores instead;
/// and the class its cap is bound by when a rate binds it.
fn ccd_row(ccd: &CcdProjection) -> ([String; 10], Option<String>, Option<String>) {
    let (cap, slope, binding, covered, uncovered) = match &ccd.cap {
        Some(cap) => (
            cap.offset.get().to_string(),
            slope_word(&cap.slope).to_owned(),
            binding_word(&cap.binding).to_owned(),
            cap.adequate.len().to_string(),
            cap.uncovered.len().to_string(),
        ),
        None => (
            "-".to_owned(),
            "-".to_owned(),
            "-".to_owned(),
            "-".to_owned(),
            "-".to_owned(),
        ),
    };
    let ranges = core_ranges(ccd.cores);
    let (cores, listed) = if ranges.chars().count() > CORES_WIDTH {
        (format!("{} cores", ccd.cores.len()), Some(ranges))
    } else {
        (ranges, None)
    };
    let cells = [
        ccd.ccd.to_string(),
        cores,
        ccd.day.get().to_string(),
        cap,
        slope,
        binding,
        edge_word(ccd.edge.as_ref()),
        ccd.remaining
            .map_or("-".to_owned(), |left| left.to_string()),
        covered,
        uncovered,
    ];
    let bound = match ccd.cap.as_ref().map(|cap| &cap.binding) {
        Some(Binding::Rate { class }) => Some(class_word(class)),
        Some(Binding::EdgeMargin {} | Binding::Baseline {}) | None => None,
    };
    (cells, listed, bound)
}

const fn reproduction_word(state: ReproductionState) -> &'static str {
    match state {
        ReproductionState::Pending => "reproduction pending",
        ReproductionState::Reproduced => "reproduced",
        ReproductionState::Unreproduced => "not reproduced",
        ReproductionState::Untestable => "untestable",
    }
}

fn hold_reason(reason: &HoldReason) -> String {
    match reason {
        HoldReason::Incomplete {
            credited_s,
            required_s,
        } => format!(
            "hold incomplete, {} of {} h",
            hours(*credited_s),
            hours(*required_s)
        ),
        HoldReason::AtCap {} => "every CCD is at its cap".to_owned(),
        HoldReason::NotQualified {} => "the proposed profile is not qualified".to_owned(),
        HoldReason::Vetoed { veto } => format!("vetoed by {}", event_word(veto)),
        HoldReason::QuestionPending { boot } => {
            format!("question pending for boot {}", esc(boot.as_str()))
        }
        HoldReason::EventUnresolved { event } => {
            format!("{} is unresolved", event_word(event))
        }
        HoldReason::Frozen {} => "promotion is frozen".to_owned(),
        HoldReason::Paused {} => "paused".to_owned(),
        HoldReason::Expired {} => "qualification expired".to_owned(),
        HoldReason::QualificationFailed { profile, .. } => format!(
            "{} failed qualification and stays blocked",
            profile_word(profile.ccd())
        ),
    }
}

fn retreat_cause(cause: &RetreatCause) -> String {
    match cause {
        RetreatCause::ShallowerCap {} => "a cap became shallower".to_owned(),
        RetreatCause::Event { event, .. } => format!("{} was CPU-suspect", event_word(event)),
        RetreatCause::Completion {} => "a held retreat is now qualified".to_owned(),
    }
}

fn changed_word(changed: &[togi_machine::CcdId]) -> String {
    changed
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn step_lines(out: &mut String, seq: togi_journal::Seq, step: &DayStep) {
    let proposed = profile_word(step.proposed.ccd());
    let head = match &step.step {
        StepKind::Hold { blocked_by } => {
            let reasons: Vec<String> = blocked_by.iter().map(hold_reason).collect();
            if reasons.is_empty() {
                "holds".to_owned()
            } else {
                format!("holds, blocked by: {}", reasons.join("; "))
            }
        }
        StepKind::Promote { changed } => format!("promotes CCD {changed} to {proposed}"),
        StepKind::Retreat {
            changed,
            cause,
            wanted,
        } => {
            let mut head = format!(
                "retreats CCD {} to {proposed}: {}",
                changed_word(changed),
                retreat_cause(cause)
            );
            if let Some(wanted) = wanted {
                let _ = write!(
                    head,
                    "; the wanted {} is not qualified, so it falls back to CO 0 until it is",
                    profile_word(wanted.ccd())
                );
            }
            head
        }
        StepKind::FirmwareZero { changed } => {
            format!(
                "sets CCD {} to 0 after a firmware change",
                changed_word(changed)
            )
        }
    };
    wrap(
        out,
        "",
        "  ",
        &format!(
            "latest step [#{seq}] at cold boot {}: {head}",
            esc(step.cold_boot.as_str())
        ),
    );
}

fn day_part(out: &mut String, day: &DayProjection, policy: &Projection) {
    out.push_str("\nDay policy\n");
    let applied = policy
        .applied
        .as_ref()
        .map_or("none yet (CO 0)".to_owned(), profile_word);
    wrap(out, "", "  ", &format!("applied profile: {applied}"));
    match &day.hold {
        Some(hold) => wrap(
            out,
            "",
            "  ",
            &format!(
                "hold: {}/{} h at {} [#{}]",
                hours(hold.eligible_s),
                hours(hold.required_s),
                profile_word(&hold.profile),
                hold.witness
            ),
        ),
        None => out.push_str("hold: none recorded\n"),
    }
    match &day.last_step {
        Some((seq, step)) => step_lines(out, *seq, step),
        None => out.push_str("latest step: none\n"),
    }
    let _ = writeln!(out, "freeze: {}", if day.freeze { "on" } else { "off" });
    let firmware = match &day.firmware {
        FirmwareValidity::Valid => "valid".to_owned(),
        FirmwareValidity::Unknown => "unknown".to_owned(),
        FirmwareValidity::Invalidated { since } => {
            format!("invalidated since [#{since}]; D returns to 0 until requalified")
        }
    };
    let _ = writeln!(out, "firmware: {firmware}");
    for boot in &day.pending_questions {
        wrap(
            out,
            "",
            "  ",
            &format!("question pending for boot {}", esc(boot.as_str())),
        );
    }
    for event in &policy.events {
        let class = match event.classification {
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
        let reproduction = reproduction_word(event.reproduction);
        wrap(
            out,
            "event: ",
            "  ",
            &format!(
                "{}: {class}, {resolution}, {reproduction}",
                event_word(&event.event)
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
        let mut text = format!("mapping CCD {}:", mapping.ccd);
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
                "{} ({}) {}{freshness}",
                profile_word(&record.profile),
                purpose_word(record.purpose),
                stage_word(record.state)
            ),
        );
    }
    if let Some(open) = &night.qualification {
        wrap(
            out,
            "",
            "  ",
            &format!(
                "qualifying {}: {} cells witnessed, {} outstanding",
                profile_word(&open.profile),
                open.witnessed,
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

/// The CCD table and the day and night parts, after the header.
pub(super) fn sections(out: &mut String, policy: &Projection, at: Where<'_>) {
    ccd_table(out, policy);
    day_part(out, &policy.day, policy);
    if let Some(night) = &policy.night {
        night_part(out, night, policy, at);
    }
}
