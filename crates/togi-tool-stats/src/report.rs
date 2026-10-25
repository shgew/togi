use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use togi_journal::{Activity, Attribution, Event, Outcome, Payload};
use togi_machine::{CcdId, Condition, Regime, ResetKind, Signal, WorkloadId};
use togi_render::escape_text;

use crate::depth::{self, DepthRate};
use crate::journal::Journal;
use crate::project::{Projection, Trial, build_name};
use crate::requests::{self, RequestMetrics};
use crate::table::{Table, row};

/// Selects what starts at or after the cutoff; no cutoff selects everything.
pub(crate) fn selected(time: Timestamp, since: Option<Timestamp>) -> bool {
    since.is_none_or(|since| time >= since)
}

pub(crate) fn stamp(time: Timestamp) -> String {
    time.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub(crate) fn hours(seconds: f64) -> String {
    format!("{:.3}", seconds / 3600.0)
}

pub(crate) fn outcome_name(outcome: Option<Outcome>) -> &'static str {
    outcome.map_or("open", Outcome::as_str)
}

pub(crate) fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

/// Every figure of the report, computed before any is rendered.
pub(crate) struct Metrics<'a> {
    pub(crate) since: Option<Timestamp>,
    pub(crate) session: SessionMetrics,
    pub(crate) runs: Vec<(usize, &'a crate::project::Run)>,
    pub(crate) time: TimeMetrics,
    pub(crate) failures: FailureMetrics,
    pub(crate) exposure: BTreeMap<(Regime, WorkloadId), Exposure>,
    pub(crate) recovery: RecoveryGap,
    pub(crate) depth: Vec<DepthRate>,
    pub(crate) requests: RequestMetrics,
    pub(crate) inconclusive: Vec<&'a Trial<'a>>,
    pub(crate) tctl: BTreeMap<(Regime, i32), u32>,
}

pub(crate) fn compute<'a>(p: &'a Projection<'a>, since: Option<Timestamp>) -> Metrics<'a> {
    let (downtime, gaps) = crash_recovery(p, since);
    Metrics {
        since,
        session: session_metrics(p.events),
        runs: p
            .runs
            .iter()
            .enumerate()
            .filter(|(_, run)| selected(run.end, since))
            .map(|(i, run)| (i + 1, run))
            .collect(),
        time: TimeMetrics {
            phases: phase_time(p, since),
            downtime_s: downtime,
        },
        failures: failure_metrics(p, since),
        exposure: exposure_metrics(p, since),
        recovery: summarize_gaps(gaps),
        depth: depth::depth(p, since),
        requests: requests::requests(p, since),
        inconclusive: p
            .trials
            .iter()
            .filter(|t| selected(t.time, since) && t.outcome() == Some(Outcome::Inconclusive))
            .collect(),
        tctl: passing_tctl(p, since),
    }
}

pub(crate) fn render(m: &Metrics<'_>) -> String {
    let mut table = Table::default();
    render_session(&mut table, &m.session, &m.runs, m.since);
    render_time(&mut table, &m.time);
    render_failures(&mut table, &m.failures, &m.exposure, &m.recovery);
    depth::render_depth(&mut table, &m.depth);
    requests::render_requests(&mut table, &m.requests);
    render_outcomes(&mut table, &m.inconclusive, &m.tctl);
    table.finish()
}

pub(crate) struct SessionMetrics {
    ids: Vec<String>,
    rulesets: BTreeSet<u32>,
    builds: BTreeSet<String>,
    events: usize,
    first: Option<Timestamp>,
    last: Option<Timestamp>,
}

fn session_metrics(events: &[Event]) -> SessionMetrics {
    let mut s = SessionMetrics {
        ids: Vec::new(),
        rulesets: BTreeSet::new(),
        builds: BTreeSet::new(),
        events: events.len(),
        first: events.first().map(|event| event.time),
        last: events.last().map(|event| event.time),
    };
    for event in events {
        match &event.payload {
            Payload::SessionStart(start) => {
                s.ids.push(start.session.to_string());
                s.rulesets.insert(start.build.ruleset);
                s.builds.insert(build_name(&start.build));
            }
            Payload::ConfigLoaded(loaded) => {
                s.rulesets.insert(loaded.build.ruleset);
                s.builds.insert(build_name(&loaded.build));
            }
            Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }
    s
}

fn join<T: std::fmt::Display>(items: impl IntoIterator<Item = T>, separator: &str) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(separator)
}

fn render_session(
    table: &mut Table,
    s: &SessionMetrics,
    runs: &[(usize, &crate::project::Run)],
    since: Option<Timestamp>,
) {
    table.section("Session", &["field", "value"]);
    for id in &s.ids {
        row!(table, "id", id);
    }
    row!(table, "rulesets", join(&s.rulesets, ", "));
    row!(table, "builds", join(&s.builds, ", "));
    if s.events > 0 {
        row!(table, "events", s.events);
    }
    if let (Some(first), Some(last)) = (s.first, s.last) {
        row!(table, "first", stamp(first));
        row!(table, "last", stamp(last));
    }
    if let Some(since) = since {
        row!(table, "since", stamp(since));
    }
    table.section(
        "Runs",
        &["run", "start", "end", "build", "trials", "crashes", "ended"],
    );
    for (number, run) in runs {
        row!(
            table,
            number,
            stamp(run.start),
            stamp(run.end),
            join(run.builds.keys(), ","),
            run.trials,
            run.crashes,
            run.ending
                .map_or_else(|| "open".to_owned(), |reason| reason.to_string())
        );
    }
}

pub(crate) type PhaseKey = (Option<Activity>, Condition, Regime, Option<Outcome>);

pub(crate) struct TimeMetrics {
    pub(crate) phases: BTreeMap<PhaseKey, u32>,
    pub(crate) downtime_s: f64,
}

fn phase_time(p: &Projection<'_>, since: Option<Timestamp>) -> BTreeMap<PhaseKey, u32> {
    let mut times = BTreeMap::new();
    for t in p.trials.iter().filter(|t| selected(t.time, since)) {
        let key = (
            t.intent.activity,
            t.intent.condition,
            t.intent.regime,
            t.outcome(),
        );
        *times.entry(key).or_default() += t.seconds();
    }
    times
}

/// The seconds from each selected crash's last evidence to the start of the next boot, and the gaps from it
/// to that boot's first event.
fn crash_recovery(p: &Projection<'_>, since: Option<Timestamp>) -> (f64, Vec<f64>) {
    let mut downtime = 0.0;
    let mut gaps = Vec::new();
    for t in p
        .trials
        .iter()
        .filter(|t| selected(t.time, since) && t.crash())
    {
        if let Some(boot) = p.next_boot.get(t.boot).and_then(|next| p.boots.get(next)) {
            gaps.push(
                boot.first_event
                    .duration_since(t.last_evidence)
                    .as_secs_f64(),
            );
            downtime += boot.start.duration_since(t.last_evidence).as_secs_f64();
        }
    }
    (downtime, gaps)
}

fn render_time(table: &mut Table, m: &TimeMetrics) {
    table.section(
        "Time",
        &[
            "activity / condition / regime / outcome",
            "trial seconds",
            "hours",
        ],
    );
    for ((activity, condition, regime, outcome), seconds) in &m.phases {
        row!(
            table,
            format!(
                "{} / {condition} / {regime} / {}",
                optional(activity.as_ref()),
                outcome_name(*outcome)
            ),
            seconds,
            hours(f64::from(*seconds))
        );
    }
    row!(
        table,
        "crash downtime (last evidence to next boot)",
        format!("{:.3}", m.downtime_s),
        hours(m.downtime_s)
    );
}

pub(crate) type FailureKey = (Option<Regime>, Option<WorkloadId>, Signal, Attribution);

/// Crashes counted by the trial seconds before their last evidence: 0, 1-29, 30-59, 60-119 and 120 or more.
pub(crate) type CrashTiming = [u32; 5];

pub(crate) struct FailureMetrics {
    pub(crate) counts: BTreeMap<FailureKey, u32>,
    pub(crate) loaded_ccds: BTreeMap<String, u32>,
    pub(crate) timing: CrashTiming,
    pub(crate) resets: BTreeMap<ResetKind, u32>,
}

fn failure_metrics(p: &Projection<'_>, since: Option<Timestamp>) -> FailureMetrics {
    let mut m = FailureMetrics {
        counts: BTreeMap::new(),
        loaded_ccds: BTreeMap::new(),
        timing: [0; 5],
        resets: BTreeMap::new(),
    };
    for event in p.events.iter().filter(|e| selected(e.time, since)) {
        match &event.payload {
            Payload::Failure(f) if f.known_failure.is_none() => {
                let trial = f
                    .trial
                    .as_ref()
                    .and_then(|id| p.by_id.get(id))
                    .map(|&i| &p.trials[i]);
                let regime = f.regime.or_else(|| trial.map(|t| t.intent.regime));
                let workload = trial.map(|t| t.intent.workload);
                *m.counts
                    .entry((regime, workload, f.signal, f.attribution))
                    .or_default() += 1;
                if f.signal == Signal::Crash && f.attribution == Attribution::Unattributed {
                    *m.loaded_ccds.entry(loaded_ccds(p, trial)).or_default() += 1;
                }
            }
            Payload::CrashDetected(crash) => {
                *m.resets
                    .entry(crash.reset_reason.unwrap_or(ResetKind::Unknown))
                    .or_default() += 1;
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }
    for t in p
        .trials
        .iter()
        .filter(|t| selected(t.time, since) && t.crash())
    {
        let bin = match t.seconds() {
            0 => 0,
            1..30 => 1,
            30..60 => 2,
            60..120 => 3,
            _ => 4,
        };
        m.timing[bin] += 1;
    }
    m
}

fn loaded_ccds(p: &Projection<'_>, trial: Option<&Trial<'_>>) -> String {
    let ccds: BTreeSet<u8> = trial
        .into_iter()
        .flat_map(|t| t.class.cores.iter())
        .filter_map(|core| p.ccd_of(core))
        .map(CcdId::get)
        .collect();
    if ccds.is_empty() {
        "idle/unknown".to_owned()
    } else {
        join(ccds, ",")
    }
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct RecoveryGap {
    pub(crate) count: usize,
    pub(crate) shortest: f64,
    pub(crate) median: f64,
    pub(crate) longest: f64,
}

pub(crate) fn summarize_gaps(mut gaps: Vec<f64>) -> RecoveryGap {
    gaps.sort_by(f64::total_cmp);
    let (Some(&shortest), Some(&longest)) = (gaps.first(), gaps.last()) else {
        return RecoveryGap::default();
    };
    let n = gaps.len();
    let median = if n.is_multiple_of(2) {
        f64::midpoint(gaps[n / 2 - 1], gaps[n / 2])
    } else {
        gaps[n / 2]
    };
    RecoveryGap {
        count: n,
        shortest,
        median,
        longest,
    }
}

fn render_failures(
    table: &mut Table,
    m: &FailureMetrics,
    exposure: &BTreeMap<(Regime, WorkloadId), Exposure>,
    gap: &RecoveryGap,
) {
    table.section(
        "Failures",
        &["regime", "workload", "signal", "attribution", "count"],
    );
    for ((regime, workload, signal, attribution), count) in &m.counts {
        row!(
            table,
            optional(regime.as_ref()),
            optional(workload.as_ref()),
            signal,
            attribution,
            count
        );
    }
    render_exposure(table, exposure);
    table.section("Unattributed crashes by loaded CCD", &["CCD(s)", "count"]);
    for (ccds, count) in &m.loaded_ccds {
        row!(table, ccds, count);
    }
    table.section("Crash timing (last evidence)", &["seconds", "count"]);
    for (label, count) in ["0", "1-29", "30-59", "60-119", "120+"]
        .iter()
        .zip(m.timing)
    {
        row!(table, label, count);
    }
    table.section("Reset reasons", &["reason", "count"]);
    for (reason, count) in &m.resets {
        row!(table, reason, count);
    }
    table.section(
        "Recovery gap",
        &["count", "min seconds", "median seconds", "max seconds"],
    );
    if gap.count > 0 {
        row!(
            table,
            gap.count,
            format!("{:.3}", gap.shortest),
            format!("{:.3}", gap.median),
            format!("{:.3}", gap.longest)
        );
    } else {
        row!(table, 0, "-", "-", "-");
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Exposure {
    pub(crate) trials: u32,
    pub(crate) seconds: u32,
    pub(crate) failures: u32,
}

fn exposure_metrics(
    p: &Projection<'_>,
    since: Option<Timestamp>,
) -> BTreeMap<(Regime, WorkloadId), Exposure> {
    let mut exposures: BTreeMap<(Regime, WorkloadId), Exposure> = BTreeMap::new();
    for t in p.trials.iter().filter(|t| selected(t.time, since)) {
        let x = exposures
            .entry((t.intent.regime, t.intent.workload))
            .or_default();
        x.trials += u32::from(t.started);
        x.seconds += t.seconds();
        x.failures += u32::from(t.failed());
    }
    exposures
}

fn render_exposure(table: &mut Table, exposures: &BTreeMap<(Regime, WorkloadId), Exposure>) {
    table.section(
        "Exposure",
        &["regime", "workload", "trials", "hours", "failures"],
    );
    for ((regime, workload), x) in exposures {
        row!(
            table,
            regime,
            workload,
            x.trials,
            hours(f64::from(x.seconds)),
            x.failures
        );
    }
}

fn passing_tctl(p: &Projection<'_>, since: Option<Timestamp>) -> BTreeMap<(Regime, i32), u32> {
    let mut temperatures = BTreeMap::new();
    for t in p.trials.iter().filter(|t| selected(t.time, since)) {
        if let Some(end) = t.end
            && end.end.outcome == Outcome::Pass
            && let Some(max_c) = end.end.tctl_max_c
        {
            *temperatures.entry((t.intent.regime, max_c)).or_default() += 1;
        }
    }
    temperatures
}

fn render_outcomes(
    table: &mut Table,
    inconclusive: &[&Trial<'_>],
    tctl: &BTreeMap<(Regime, i32), u32>,
) {
    table.section("Inconclusive trials", &["trial", "condition", "reason"]);
    for t in inconclusive {
        let reason = t
            .end
            .and_then(|end| end.end.reason.as_deref())
            .map(|reason| escape_text(reason).to_string());
        row!(table, t.intent.trial, t.intent.condition, optional(reason));
    }
    table.section("Tctl", &["regime", "max C", "passing trials"]);
    for ((regime, max_c), count) in tctl {
        row!(table, regime, max_c, count);
    }
}

/// Renders the report of a journal: what ran and for how long, what failed, the failure rate by depth and the
/// R7 request telemetry. A cutoff keeps what starts at or
/// after it; runs that overlap it keep their totals.
pub fn report(journal: &Journal, since: Option<Timestamp>) -> String {
    let projection = Projection::new(journal);
    render(&compute(&projection, since))
}
