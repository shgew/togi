use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{ShutdownReason, TrialId};
use togi_machine::{CcdId, Condition, CoreSet, PerCore, Regime, WorkloadId};
use togi_tuner::{CyclePlan, CycleStep, R7CoreStatus};

use super::{RAIL, r7_lines};
use crate::frames::fit::plain;
use crate::frames::layout::SizeClass;
use crate::frames::scene::Scene;
use crate::model::{Count, CycleView, Model, SessionView, StopView, TrialView};
use crate::screen::Colours;
use crate::tests::kit::core;

const WIDTH: usize = 100;

fn workload() -> WorkloadId {
    WorkloadId::new(Regime::R7, 0).expect("a workload")
}

fn status(core_id: u8, top: bool, proxy: bool) -> R7CoreStatus {
    R7CoreStatus {
        core: core(core_id),
        ccd: CcdId::try_from(core_id / 8).unwrap(),
        workload: workload(),
        top_requester: top,
        offset_proxy: proxy,
        self_sufficient: false,
        passes: 0,
    }
}

fn trial(regime: Regime) -> TrialView {
    TrialView {
        id: TrialId::from("t1".to_owned()),
        regime,
        workload: WorkloadId::new(regime, 0).expect("a workload"),
        cores: CoreSet::default(),
        parked: CoreSet::default(),
        idle: CoreSet::default(),
        condition: Condition::Together,
        activity: None,
        profile: PerCore::default(),
        started: Some(Timestamp::UNIX_EPOCH),
        crashed: false,
        duration: Duration::from_secs(60),
        partial: false,
        rerun: false,
        retry: false,
        cycle: None,
        hunt: None,
        core: None,
        offset: None,
        round: None,
        count: Count::default(),
    }
}

/// What the R7 page says while the session's plan has one R7 step current, with `running` the trial in flight
/// and `stopped` the stop, if any.
fn page(
    statuses: &[R7CoreStatus],
    running: Option<&TrialView>,
    stopped: Option<&StopView>,
) -> Vec<String> {
    page_in(WIDTH, statuses, running, stopped)
}

fn page_in(
    width: usize,
    statuses: &[R7CoreStatus],
    running: Option<&TrialView>,
    stopped: Option<&StopView>,
) -> Vec<String> {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let plan = CyclePlan {
        number: None,
        open: true,
        steps: vec![CycleStep {
            regime: Regime::R7,
            workload: workload(),
            parts: Vec::new(),
            chains_complete: false,
            done: false,
        }],
        current: 0,
        paused: false,
    };
    let view = SessionView {
        start: Timestamp::UNIX_EPOCH,
        activity: None,
    };
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.r7 = statuses;
    s.cycle = Some(CycleView {
        plan: &plan,
        step_hunts: &[],
        resume: None,
    });
    s.trial = running;
    s.stopped = stopped;
    let scene = Scene::new(s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    r7_lines(&scene, width, SizeClass::Wide)
        .iter()
        .map(plain)
        .collect()
}

fn workload_row(rows: &[String]) -> &str {
    rows.iter()
        .map(String::as_str)
        .find(|row| row.starts_with("mprime") && row.contains('·'))
        .expect("the workload's row")
}

fn ccd_row(rows: &[String]) -> &str {
    rows.iter()
        .map(String::as_str)
        .find(|row| row.starts_with("CCD 0"))
        .expect("CCD 0's row")
        .trim_end()
}

#[test]
fn the_rail_is_global_and_the_top_groups_are_per_ccd() {
    let rows = page(&[status(3, true, false)], None, None);
    let shown = rows.join(" ");
    let said = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(said.contains(RAIL), "{said}");
    for want in [
        "All loaded cores share one voltage rail",
        "highest voltage request",
        "separate top-request group on each loaded CCD",
        "need not contain the core that sets the rail",
    ] {
        assert!(said.contains(want), "lacks {want:?}: {said}");
    }
    assert!(!said.contains("cores of a CCD share"), "{said}");
}

#[test]
fn the_rail_note_is_one_line_where_the_page_is_wide_enough() {
    let rows = page_in(RAIL.chars().count(), &[status(3, true, false)], None, None);
    assert!(rows.iter().any(|row| row == RAIL), "{rows:#?}");
}

#[test]
fn a_ccd_with_no_top_requester_says_none_is_recorded_yet() {
    let rows = page(&[status(3, false, false)], None, None);
    assert_eq!(ccd_row(&rows), "CCD 0  no top requester recorded yet");
}

#[test]
fn only_a_running_r7_trial_loads_now() {
    let statuses = [status(3, true, false)];
    let running = trial(Regime::R7);
    let loading = page(&statuses, Some(&running), None);
    assert_eq!(workload_row(&loading), "mprime AVX2 36K-248K · loading now");

    let between = page(&statuses, None, None);
    assert_eq!(
        workload_row(&between),
        "mprime AVX2 36K-248K · current step"
    );

    let stop = StopView {
        at: Timestamp::UNIX_EPOCH,
        reason: ShutdownReason::Signal,
        saved: true,
    };
    let stopped = page(&statuses, Some(&running), Some(&stop));
    assert_eq!(
        workload_row(&stopped),
        "mprime AVX2 36K-248K · current step"
    );

    let mut crashed = trial(Regime::R7);
    crashed.crashed = true;
    let after_crash = page(&statuses, Some(&crashed), None);
    assert_eq!(
        workload_row(&after_crash),
        "mprime AVX2 36K-248K · current step"
    );
}

#[test]
fn a_measured_top_requester_says_its_order_is_per_request() {
    let alone = page(
        &[status(3, true, false), status(7, false, false)],
        None,
        None,
    );
    assert_eq!(
        ccd_row(&alone),
        "CCD 0  top requesters 03 · per request order"
    );
}

#[test]
fn measured_top_requesters_within_a_millivolt_are_tied() {
    let tied = page(
        &[status(3, true, false), status(5, true, false)],
        None,
        None,
    );
    assert_eq!(
        ccd_row(&tied),
        "CCD 0  top requesters 03 05 · tied · per request order"
    );
}
